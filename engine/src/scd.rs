//! SCD（说话人切换检测）：speaker embedding 余弦匹配 + 自动编号 + 音色选性别。
//!
//! 对齐 [ADR-0002](docs/adr/0002-local-streaming-asr-and-self-built-scd.md)：
//! 不做全自动在线 diarization（延迟 + 标签漂移），采用「VAD 切句 + speaker
//! embedding 余弦匹配」自拼方案；系统**只能分组、不能自动认人**（手动命名属 T6）。
//! 术语对齐 ADR-0002（及 CONTEXT.md 的 Avoid「声纹识别」）：说话人向量一律称
//! speaker embedding，不叫「声纹」。
//!
//! 职责分工：
//! - **VAD 切句**由 sidecar 的端点检测承担（`maybe_finalize`，见
//!   `src-tauri/sherpa_streaming.py`），每一条 `final` 即一个句/段；本模块只
//!   消费「一条 final + 该段音频的 embedding + 有效语音时长」。
//! - **[`Scd`]**：说话人模板注册表 + 三段式判定状态机（见 [`Scd::process_utterance`]）。
//!
//! ## 判定模型（2026-08 实测校准，见 PR #25）
//!
//! eres2net / eres2netv2 在「句子级短段」上的同一人余弦远低于 0.75（实测
//! eres2net-base 干净 2s 段同一人均值 ≈0.51、3s ≈0.64；加房间噪声更低），
//! 固定 0.75 阈值会把每条 final 都判成新说话人（幻影说话人）。因此采用
//! **三个协同机制**：
//!
//! 1. **时长自适应阈值 + 相对最近邻**：匹配阈值按有效语音时长分档
//!    （[`ScdConfig::match_threshold_long`] / [`ScdConfig::match_threshold_short`]），
//!    多模板时要求 top1 比 top2 高出 [`ScdConfig::match_margin`] 才归入
//!    （相对信号在短段上仍有效，实测 1s 以上最近邻正确率接近 100%）。
//! 2. **时长门槛**：有效语音 < [`ScdConfig::min_speech_seconds`] 的片段不做
//!    embedding 判定，沿用最近说话人（绝不新建）——短段 embedding 近乎随机，
//!    是幻影说话人的主要来源。
//! 3. **新说话人「证据确认」**：新说话人只允许两种方式产生——
//!    (a) 单段有效语音 ≥ [`ScdConfig::new_speaker_min_seconds`] 且远离所有模板；
//!    (b) 两个**互相印证**的短段：彼此余弦 ≥ [`ScdConfig::confirm_threshold`]
//!    且都远离所有模板 → 创建新说话人，并**追溯修正**（[`SpeakerCorrection`]）
//!    前一个短段的归属。候选在 [`ScdConfig::pending_max_span`] 条发言内未被
//!    印证则作废（防止长时间后误认幻影）。这样两人间隔很近也能在 1–2 句内
//!    分开，且不产生幻影。
//!
//! 验证（真实 eres2netv2 embedding + SNR15–20dB 噪声，6 seeds）：单人短句
//! → 1 说话人 100%；两人 1.5–2.5s 轮换 → 2 说话人 ~95%；两人 0.9–1.6s
//! 极短轮换 → 2 说话人 ~62–70%（首/末短句无法自证，暂挂前一说话人——模型下限）。

use crate::types::Gender;

/// 一条已注册的说话人模板（speaker embedding + 性别）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTemplate {
    /// 说话人 id（自动编号，1 起）。
    pub id: u32,
    /// speaker embedding（说话人向量，归一化后做余弦匹配）。随 [Scd::update_template] 移动平均更新。
    pub embedding: Vec<f32>,
    /// 说话人性别（音色推断结果，用于前端头像选择）。
    pub gender: Gender,
    /// 已并入的向量数（移动平均权重：avg = (avg·n + new) / (n+1)）。
    pub update_count: u32,
}

/// SCD 配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScdConfig {
    /// 文本长度门槛：短于它的 final（语气词/噪声，如「嗯」「哦」）不参与新建判定。
    pub min_speech_chars: usize,
    /// **有效语音时长门槛（秒）**：短于此的片段不做 embedding 判定，
    /// 沿用最近说话人（绝不新建）——短段 embedding 不可靠，是幻影说话人主因。
    pub min_speech_seconds: f32,
    /// 匹配阈值分档时长（秒）：时长 ≥ 此值用 [`Self::match_threshold_long`]，
    /// 否则用 [`Self::match_threshold_short`]。
    pub long_seconds: f32,
    /// 长段匹配阈值（时长 ≥ [`Self::long_seconds`]）。实测 eres2netv2 干净 3s
    /// 同一人余弦 ≈0.78，加噪 ≈0.71；取 0.60 留有余量。
    pub match_threshold_long: f32,
    /// 短段匹配阈值（[`Self::min_speech_seconds`] ≤ 时长 < [`Self::long_seconds`]）。
    /// 实测 eres2netv2 加噪 1.5–2s 同一人余弦均值 ≈0.54–0.60，取 0.48。
    pub match_threshold_short: f32,
    /// 相对最近邻 margin：模板 ≥2 时，top1 需比 top2 高出至少此值才可归入
    /// （绝对值压缩时相对信号仍有效）。
    pub match_margin: f32,
    /// 新建候选阈值：top1 < 此值才视为「可能的新说话人」候选（而非沿用）。
    pub new_speaker_threshold: f32,
    /// 单段即可新建说话人的有效语音时长（秒）。
    pub new_speaker_min_seconds: f32,
    /// 印证阈值：两个候选短段互相的余弦 ≥ 此值才确认新建（证据确认机制）。
    pub confirm_threshold: f32,
    /// pending 候选最多存活多少条后续发言；超过即作废（防长期悬挂误认幻影）。
    pub pending_max_span: u32,
}

impl Default for ScdConfig {
    fn default() -> Self {
        Self {
            min_speech_chars: 2,
            min_speech_seconds: 1.0,
            long_seconds: 2.5,
            match_threshold_long: 0.60,
            match_threshold_short: 0.48,
            match_margin: 0.08,
            new_speaker_threshold: 0.38,
            new_speaker_min_seconds: 2.0,
            confirm_threshold: 0.38,
            pending_max_span: 10,
        }
    }
}

/// 一次说话人归属判定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerDecision {
    /// 归属的说话人 id。
    pub speaker_id: u32,
    /// 是否为新建说话人（前端据此决定是否创建新头像条目）。
    pub is_new_speaker: bool,
    /// 追溯修正：确认新说话人时，把此前 pending 的短段（调用方给它的
    /// `utt_token`）改归到新说话人。调用方需把 `utt_token` 与最终片段 id
    /// 对应起来并发出修正事件。
    pub corrections: Vec<SpeakerCorrection>,
}

/// 一条追溯修正：目标 utterance（由调用方持有的 `utt_token` 标识）应改归
/// 到 `new_speaker_id`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakerCorrection {
    pub utt_token: u64,
    pub new_speaker_id: u32,
}

/// 说话人切换检测器：模板注册表 + 三段式判定状态机。
///
/// 线程模型：由 Tauri 壳的 stdout 读线程独占持有（[`Scd`] 是 `Send`，
/// 经 `Arc<Mutex<Scd>>` 共享，但同一时刻只有一个 writer —— final 解析点）。
/// 全部业务逻辑不依赖 Tauri，可被单元测试直接驱动。
#[derive(Debug)]
pub struct Scd {
    config: ScdConfig,
    templates: Vec<SpeakerTemplate>,
    /// 下一个可分配的新说话人 id（自动编号：1/2/3 …）。
    next_speaker_id: u32,
    /// 最近一次归属的说话人（短发言/无 speaker embedding 时沿用，避免噪声新建）。
    last_speaker_id: Option<u32>,
    /// 待确认的新说话人候选：(embedding, 首个候选段的 utt_token, 已存活发言数)。
    /// 第二个互相印证的候选段到达时确认新建（证据确认机制）。
    pending_candidate: Option<(Vec<f32>, u64, u32)>,
}

impl Default for Scd {
    fn default() -> Self {
        Self::new(ScdConfig::default())
    }
}

impl Scd {
    pub fn new(config: ScdConfig) -> Self {
        Self {
            config,
            templates: Vec::new(),
            next_speaker_id: 1,
            last_speaker_id: None,
            pending_candidate: None,
        }
    }

    pub fn config(&self) -> ScdConfig {
        self.config
    }

    pub fn templates(&self) -> &[SpeakerTemplate] {
        &self.templates
    }

    pub fn speaker_count(&self) -> usize {
        self.templates.len()
    }

    /// 最近一次归属的说话人 id（无任何发言时为 `None`）。
    pub fn last_speaker_id(&self) -> Option<u32> {
        self.last_speaker_id
    }

    /// 查询某说话人模板的性别（未注册返回 `None`）。
    pub fn template_gender(&self, speaker_id: u32) -> Option<Gender> {
        self.templates
            .iter()
            .find(|t| t.id == speaker_id)
            .map(|t| t.gender)
    }

    /// 文本是否算「有效发言」（长度 ≥ `min_speech_chars`）。
    pub fn is_meaningful_speech(&self, text: &str) -> bool {
        text.chars().count() >= self.config.min_speech_chars
    }

    /// 有效语音时长是否达到 embedding 判定门槛（≥ `min_speech_seconds`）。
    pub fn is_meaningful_duration(&self, speech_seconds: f32) -> bool {
        speech_seconds >= self.config.min_speech_seconds
    }

    /// 判定一条 final 归属的完整入口。
    ///
    /// 参数：
    /// - `utt_token`：调用方为这条 final 分配的唯一序号；当这条 final 作为
    ///   pending 候选被后续片段确认新建时，会通过 [`SpeakerCorrection`] 引用它。
    /// - `text`：转写文本。
    /// - `embedding`：该段音频的 speaker embedding。
    /// - `speech_seconds`：该段音频**有效语音**时长（秒，裁静音后），由 sidecar 上报。
    /// - `gender_hint`：sidecar 上报或用户手动指定的性别。
    ///
    /// 三段式判定：
    /// 1. 无信号（过短文本 / 空·全零·NaN embedding / 有效语音 < 时长门槛）
    ///    → 沿用最近说话人（首个归说话人 1），**绝不新建**；
    /// 2. 有信号 → 与所有模板余弦匹配：top1 ≥ 该时长档阈值且满足相对 margin
    ///    → 归入该说话人；top1 < 新建阈值 → 新说话人候选（长段直接新建 /
    ///    短段进入 pending 待印证）；
    /// 3. 其余（阈值与新建阈值之间的模糊带）→ 沿用最近说话人。
    pub fn process_utterance(
        &mut self,
        utt_token: u64,
        text: &str,
        embedding: &[f32],
        speech_seconds: f32,
        gender_hint: Option<Gender>,
    ) -> SpeakerDecision {
        // 1. 无信号兜底：沿用最近说话人，绝不新建（噪声保护）。
        if !self.is_meaningful_speech(text)
            || !self.is_meaningful_duration(speech_seconds)
            || Self::is_empty_signal(embedding)
        {
            return self.resolve_no_embedding();
        }

        // 每条有信号的发言都让 pending 候选存活计数 +1（超限后作废）。
        self.age_pending();

        let sims: Vec<(f32, u32)> = self
            .templates
            .iter()
            .map(|t| (cosine_similarity(embedding, &t.embedding), t.id))
            .collect();

        // 首个有效发言：种子说话人 1（需要第一个锚点）。
        if sims.is_empty() {
            let id = self.next_speaker_id;
            self.next_speaker_id += 1;
            let gender = match gender_hint {
                Some(g) if g != Gender::Unknown => g,
                _ => self.infer_gender(embedding),
            };
            self.templates.push(SpeakerTemplate {
                id,
                embedding: embedding.to_vec(),
                gender,
                update_count: 1,
            });
            self.last_speaker_id = Some(id);
            self.pending_candidate = None;
            return SpeakerDecision {
                speaker_id: id,
                is_new_speaker: true,
                corrections: Vec::new(),
            };
        }

        let threshold = if speech_seconds >= self.config.long_seconds {
            self.config.match_threshold_long
        } else {
            self.config.match_threshold_short
        };
        let (mut top1, mut top1_id) = sims[0];
        let mut top2 = f32::MIN;
        for &(sim, id) in &sims[1..] {
            if sim > top1 {
                top2 = top1;
                top1 = sim;
                top1_id = id;
            } else if sim > top2 {
                top2 = sim;
            }
        }

        // 2a. 归入现有说话人：top1 ≥ 阈值 且（唯一模板 或 top1-top2 ≥ margin）。
        if top1 >= threshold && (self.templates.len() == 1 || top1 - top2 >= self.config.match_margin)
        {
            if let Some(hint) = gender_hint {
                if hint != Gender::Unknown {
                    if let Some(t) = self.templates.iter_mut().find(|t| t.id == top1_id) {
                        if t.gender == Gender::Unknown {
                            t.gender = hint;
                        }
                    }
                }
            }
            self.update_template(top1_id, embedding);
            self.last_speaker_id = Some(top1_id);
            // 注意：不主动作废 pending 候选 —— 新说话人的「第一句短段」常常夹在
            // 现有说话人的发言之间，证据需要跨过它们保留到第二条印证句到达。
            return SpeakerDecision {
                speaker_id: top1_id,
                is_new_speaker: false,
                corrections: Vec::new(),
            };
        }

        // 2b. 新说话人候选：远离所有模板。
        if top1 < self.config.new_speaker_threshold {
            // 长段（有效语音 ≥ new_speaker_min_seconds）：单段证据足够，直接新建。
            if speech_seconds >= self.config.new_speaker_min_seconds {
                let id = self.next_speaker_id;
                self.next_speaker_id += 1;
                let gender = match gender_hint {
                    Some(g) if g != Gender::Unknown => g,
                    _ => self.infer_gender(embedding),
                };
                self.templates.push(SpeakerTemplate {
                    id,
                    embedding: embedding.to_vec(),
                    gender,
                    update_count: 1,
                });
                self.last_speaker_id = Some(id);
                self.pending_candidate = None;
                return SpeakerDecision {
                    speaker_id: id,
                    is_new_speaker: true,
                    corrections: Vec::new(),
                };
            }

            // 短段：现有 pending 候选未超限则尝试印证。
            if let Some((pend_emb, pend_token, pend_span)) = self.pending_candidate.clone() {
                if pend_span >= self.config.pending_max_span {
                    self.pending_candidate = None; // 存活超限，作废
                } else if cosine_similarity(embedding, &pend_emb) >= self.config.confirm_threshold {
                    let id = self.next_speaker_id;
                    self.next_speaker_id += 1;
                    let gender = match gender_hint {
                        Some(g) if g != Gender::Unknown => g,
                        _ => self.infer_gender(embedding),
                    };
                    self.templates.push(SpeakerTemplate {
                        id,
                        embedding: pend_emb,
                        gender,
                        update_count: 1,
                    });
                    self.update_template(id, embedding);
                    self.last_speaker_id = Some(id);
                    self.pending_candidate = None;
                    return SpeakerDecision {
                        speaker_id: id,
                        is_new_speaker: true,
                        corrections: vec![SpeakerCorrection {
                            utt_token: pend_token,
                            new_speaker_id: id,
                        }],
                    };
                }
            }
            self.pending_candidate = Some((embedding.to_vec(), utt_token, 0));
            let id = self.last_speaker_id.unwrap_or(1);
            self.last_speaker_id = Some(id);
            return SpeakerDecision {
                speaker_id: id,
                is_new_speaker: false,
                corrections: Vec::new(),
            };
        }

        // 3. 模糊带（new_speaker_threshold ≤ top1 < threshold）：沿用最近说话人。
        let id = self.last_speaker_id.unwrap_or(1);
        self.last_speaker_id = Some(id);
        SpeakerDecision {
            speaker_id: id,
            is_new_speaker: false,
            corrections: Vec::new(),
        }
    }

    /// 把新向量并入指定说话人模板（移动平均），增强鲁棒性。
    ///
    /// 口径：`avg = (avg·n + new) / (n+1)`，其中 n 为已并入向量数。维度不一致
    /// 或模板不存在时静默忽略（稳健优先）。
    pub fn update_template(&mut self, speaker_id: u32, embedding: &[f32]) {
        let Some(t) = self.templates.iter_mut().find(|t| t.id == speaker_id) else {
            return;
        };
        if t.embedding.len() != embedding.len() {
            return;
        }
        let n = t.update_count as f64;
        let new_count = t.update_count + 1;
        for (old, new) in t.embedding.iter_mut().zip(embedding.iter()) {
            *old = ((*old as f64 * n + *new as f64) / new_count as f64) as f32;
        }
        t.update_count = new_count;
    }

    /// 音色选性别：根据 speaker embedding 推断说话人性别。
    ///
    /// **T5 明确决策：降级返回 [`Gender::Unknown`]**——MVP 阶段无真实性别分类
    /// 模型，基于 embedding 维度/基频的简单启发精度不可靠。`gender_hint`
    /// 优先于本函数。
    pub fn infer_gender(&self, _embedding: &[f32]) -> Gender {
        Gender::Unknown
    }

    /// 让 pending 候选存活计数 +1；超限后作废（防长期悬挂误认幻影）。
    fn age_pending(&mut self) {
        if let Some((e, t, span)) = self.pending_candidate.take() {
            self.pending_candidate = Some((e, t, span + 1));
        }
    }

    /// 「无 speaker embedding 信号」的兜底：沿用最近说话人（首个归说话人 1），
    /// **绝不新建** —— 噪声保护。
    fn resolve_no_embedding(&self) -> SpeakerDecision {
        let id = self.last_speaker_id.unwrap_or(1);
        SpeakerDecision {
            speaker_id: id,
            is_new_speaker: false,
            corrections: Vec::new(),
        }
    }

    /// embedding 是否为「无 speaker embedding 信号」：空向量、全零向量，或
    /// **任一分量为 NaN**（sidecar 解析异常时的防御）。
    fn is_empty_signal(embedding: &[f32]) -> bool {
        embedding.is_empty()
            || embedding.iter().any(|x| x.is_nan())
            || embedding.iter().all(|x| *x == 0.0)
    }
}

/// 两个向量的余弦相似度：`a·b / (|a|·|b|)`，值域 [-1, 1]。
///
/// 稳健口径（注释即契约）：
/// - 维度不一致 → 返回 0（无法比较，不 panic）；
/// - 任一侧为零向量（模长为 0）→ 返回 0（无方向可比）；
/// - 任一侧含 NaN/Inf → 返回 0（无方向可比，防御解析异常的 embedding，
///   避免 NaN 相似度被当作 0 而误新建说话人）；
/// - 内部用 f64 累加，减少 float32 累积误差后截断回 f32。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    // 模长需为正有限值、点积需有限：NaN/±Inf/零模长都无方向可比 → 0
    if !(na > 0.0) || !(nb > 0.0) || !na.is_finite() || !nb.is_finite() || !dot.is_finite() {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::speaker_color;

    fn config() -> ScdConfig {
        ScdConfig::default()
    }

    // 可预测的合成 embedding：同一人恒用同一向量（相似度 1），不同人互相正交（相似度 0）。
    fn emb_a() -> Vec<f32> {
        vec![1.0, 0.0, 0.0]
    }
    fn emb_b() -> Vec<f32> {
        vec![0.0, 1.0, 0.0]
    }
    fn emb_c() -> Vec<f32> {
        vec![0.0, 0.0, 1.0]
    }

    const DUR_LONG: f32 = 3.0; // ≥ long_seconds：长段阈值 / 可单段新建
    const DUR_SHORT: f32 = 1.5; // ≥ min_speech_seconds，< long_seconds
    const DUR_TINY: f32 = 0.6; // < min_speech_seconds：无信号兜底

    // ---- 验收：余弦相似度纯函数 ----

    #[test]
    fn cosine_similarity_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[2.0, 0.0], &[3.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    // ---- 验收：同一人后续发言归入同一说话人（时长自适应阈值）----

    #[test]
    fn same_speaker_repeats_match() {
        let mut scd = Scd::new(config());
        let d1 = scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        assert_eq!((d1.speaker_id, d1.is_new_speaker), (1, true));
        let d2 = scd.process_utterance(2, "第二句", &emb_a(), DUR_LONG, None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, false));
        // 短段（≥ 时长门槛、< 长段分界）也归同一人
        let d3 = scd.process_utterance(3, "短句", &emb_a(), DUR_SHORT, None);
        assert_eq!((d3.speaker_id, d3.is_new_speaker), (1, false));
    }

    // ---- 验收：短时长（< min_speech_seconds）绝不新建、不产生模板 ----

    #[test]
    fn tiny_duration_never_creates_speaker() {
        let mut scd = Scd::new(config());
        let d = scd.process_utterance(1, "嗯嗯", &emb_a(), DUR_TINY, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false), "首条极短段归 1 但不新建");
        assert_eq!(scd.speaker_count(), 0, "极短段不建模板");
        let d2 = scd.process_utterance(2, "正常的一句话", &emb_a(), DUR_LONG, None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, true), "后续长段种子说话人 1");
        assert_eq!(scd.speaker_count(), 1);
    }

    // ---- 验收：过短文本 / 无 embedding / NaN 均沿用，绝不新建 ----

    #[test]
    fn no_signal_falls_back_to_last_speaker() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        let d = scd.process_utterance(2, "嗯", &emb_b(), DUR_LONG, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        let d2 = scd.process_utterance(3, "没有向量的一段话", &[], DUR_LONG, None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, false));
        let d3 = scd.process_utterance(
            4,
            "含 NaN 的一段话",
            &[f32::NAN, f32::NAN, f32::NAN],
            DUR_LONG,
            None,
        );
        assert_eq!((d3.speaker_id, d3.is_new_speaker), (1, false));
        let d4 = scd.process_utterance(5, "含 NaN 的第二段", &[1.0, f32::NAN, 0.0], DUR_LONG, None);
        assert_eq!((d4.speaker_id, d4.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 1, "无信号不产生模板");
    }

    // ---- 验收：新说话人单长段直接新建 ----

    #[test]
    fn long_segment_creates_new_speaker() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        let d = scd.process_utterance(2, "不同的人", &emb_b(), DUR_LONG, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (2, true));
        assert_eq!(scd.speaker_count(), 2);
    }

    // ---- 验收：短段新说话人需要「证据确认」+ 追溯修正 ----

    #[test]
    fn short_new_speaker_requires_confirmation_and_corrects() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        // B 短段（远离 A）：不新建，沿用 A，进入 pending（token=2）
        let d1 = scd.process_utterance(2, "B 的短句一", &emb_b(), DUR_SHORT, None);
        assert_eq!((d1.speaker_id, d1.is_new_speaker), (1, false), "单短段不新建");
        assert_eq!(scd.speaker_count(), 1);
        // B 第二条短段印证 → 新建说话人 2，并追溯修正 token=2
        let d2 = scd.process_utterance(3, "B 的短句二", &emb_b(), DUR_SHORT, None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (2, true));
        assert_eq!(scd.speaker_count(), 2);
        assert_eq!(
            d2.corrections,
            vec![SpeakerCorrection {
                utt_token: 2,
                new_speaker_id: 2
            }]
        );
    }

    #[test]
    fn unconfirmed_short_candidate_stays_pending() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        scd.process_utterance(2, "候选一", &emb_b(), DUR_SHORT, None);
        let d = scd.process_utterance(3, "又是 A", &emb_a(), DUR_SHORT, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 1);
    }

    // ---- 验收：模板更新后匹配仍稳定（不跳变）----

    #[test]
    fn template_update_keeps_speaker_stable() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, None);
        for i in 0..4u64 {
            let drifted = vec![1.0 - 0.05 * (i as f32 + 1.0), 0.05 * (i as f32 + 1.0), 0.0];
            let d = scd.process_utterance(2 + i, "后续句", &drifted, DUR_LONG, None);
            assert_eq!((d.speaker_id, d.is_new_speaker), (1, false), "模板更新后仍归同一说话人");
        }
        let d = scd.process_utterance(9, "回原点", &emb_a(), DUR_LONG, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        let t = scd.templates().iter().find(|t| t.id == 1).unwrap();
        assert_eq!(t.update_count, 6, "初始 1 + 4 次漂移并入 + 回原点并入");
    }

    // ---- 验收：长会话颜色稳定（speaker_id 恒定 → 颜色不跳变）----

    #[test]
    fn long_session_colors_are_stable() {
        let mut scd = Scd::new(config());
        for i in 0..100u64 {
            let (emb, expect) = if i % 2 == 0 { (emb_a(), 1) } else { (emb_b(), 2) };
            let d = scd.process_utterance(i + 1, "发言", &emb, DUR_LONG, None);
            assert_eq!(d.speaker_id, expect, "第 {i} 次发言 speaker_id 恒定");
        }
        let color1 = speaker_color(1);
        let color2 = speaker_color(2);
        assert_eq!(color1, speaker_color(1));
        assert_eq!(color2, speaker_color(2));
        assert_ne!(color1, color2);
        for _ in 0..50 {
            assert_eq!(speaker_color(1), color1, "长会话中说话人 1 颜色绝不跳变");
        }
        let colors: Vec<String> = (1..=8).map(speaker_color).collect();
        let unique: std::collections::HashSet<&String> = colors.iter().collect();
        assert_eq!(unique.len(), 8, "前 8 个说话人颜色互不相同");
        assert_eq!(speaker_color(9), speaker_color(1), "越界 id 取模回绕但仍稳定");
    }

    // ---- 验收：相对最近邻（margin）在模板 ≥2 时生效 ----

    #[test]
    fn relative_margin_prefers_top1() {
        let mut scd = Scd::new(config());
        scd.process_utterance(1, "A", &emb_a(), DUR_LONG, None);
        scd.process_utterance(2, "B", &emb_b(), DUR_LONG, None);
        let near = vec![0.8, 0.2, 0.0];
        let d = scd.process_utterance(3, "偏 A 的发言", &near, DUR_LONG, None);
        assert_eq!(d.speaker_id, 1);
        let ambiguous = vec![0.6, 0.6, 0.0];
        let d2 = scd.process_utterance(4, "模糊的发言", &ambiguous, DUR_LONG, None);
        assert_eq!(d2.speaker_id, 1);
        assert_eq!(d2.is_new_speaker, false);
    }

    // ---- 验收：单人短句长会话不产生幻影说话人（用户 1 人 → 4 说话人 bug）----

    #[test]
    fn monologue_short_sentences_stays_one_speaker() {
        let mut scd = Scd::new(config());
        let seq = ["好好", "现在这个怎么样", "哦", "HELLO HELLO HELLO", "嗯嗯", "再测试一句"];
        for (i, text) in seq.iter().enumerate() {
            let dur = if text.chars().count() <= 2 { DUR_TINY } else { 2.0 };
            let d = scd.process_utterance(i as u64 + 1, text, &emb_a(), dur, None);
            assert_eq!(d.speaker_id, 1, "{text} 应归说话人 1");
        }
        assert_eq!(scd.speaker_count(), 1, "单人会话绝不产生幻影说话人");
    }

    // ---- 验收：两人快速交替短句能分成两个说话人（用户 2 人 → 12 说话人 bug）----

    #[test]
    fn fast_alternation_splits_into_two_speakers() {
        let mut scd = Scd::new(config());
        // A1(长) B1(短) A2(短) B2(短 印证 B1 → 说话人 2 + 追溯) A3(短 归1) B3(短 归2)
        let seq: Vec<(Vec<f32>, f32)> = vec![
            (emb_a(), DUR_LONG),
            (emb_b(), DUR_SHORT),
            (emb_a(), DUR_SHORT),
            (emb_b(), DUR_SHORT),
            (emb_a(), DUR_SHORT),
            (emb_b(), DUR_SHORT),
        ];
        let mut decisions = Vec::new();
        for (i, (emb, dur)) in seq.iter().enumerate() {
            let d = scd.process_utterance(i as u64 + 1, "发言", emb, *dur, None);
            decisions.push((d.speaker_id, d.is_new_speaker, d.corrections.clone()));
        }
        assert_eq!(decisions[0].0, 1, "A1 → 1");
        assert_eq!(decisions[1].0, 1, "B1 未证实 → 沿用 1");
        assert_eq!(decisions[2].0, 1, "A2 → 1");
        assert_eq!(decisions[3].0, 2, "B2 印证 B1 → 2");
        assert_eq!(decisions[4].0, 1, "A3 → 1");
        assert_eq!(decisions[5].0, 2, "B3 → 2");
        assert_eq!(scd.speaker_count(), 2, "两个说话人");
        // B1（token=2）被追溯修正到说话人 2
        assert_eq!(
            decisions[3].2,
            vec![SpeakerCorrection {
                utt_token: 2,
                new_speaker_id: 2
            }],
            "B1（token=2）被追溯修正到说话人 2"
        );
    }

    // ---- 验收：音色选性别（T5 决策：降级为 Unknown；gender_hint 优先）----

    #[test]
    fn gender_hint_sets_template_gender() {
        let mut scd = Scd::new(config());
        let d = scd.process_utterance(1, "第一句", &emb_a(), DUR_LONG, Some(Gender::Female));
        assert!(d.is_new_speaker);
        assert_eq!(scd.template_gender(d.speaker_id), Some(Gender::Female));
        let mut scd2 = Scd::new(config());
        let d2 = scd2.process_utterance(1, "第一句", &emb_b(), DUR_LONG, None);
        assert_eq!(scd2.template_gender(d2.speaker_id), Some(Gender::Unknown));
    }

    #[test]
    fn infer_gender_degrades_to_unknown_without_model() {
        let scd = Scd::new(config());
        assert_eq!(scd.infer_gender(&emb_a()), Gender::Unknown);
    }
}
