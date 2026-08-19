//! SCD（说话人切换检测）：声纹向量余弦匹配 + 自动编号 + 音色选性别。
//!
//! 对齐 [ADR-0002](docs/adr/0002-local-streaming-asr-and-self-built-scd.md)：
//! 不做全自动在线 diarization（延迟 + 标签漂移），采用「VAD 切句 + speaker
//! embedding 余弦匹配」自拼方案；系统**只能分组、不能自动认人**（手动命名属 T6）。
//!
//! 职责分工：
//! - **VAD 切句**由 sidecar 的端点检测承担（`maybe_finalize`，见
//!   `src-tauri/sherpa_streaming.py`），每一条 `final` 即一个句/段；本模块只
//!   消费「一条 final + 该段音频的 embedding」。
//! - **[`Scd`]**：说话人模板注册表 —— 输入一条 embedding，与所有已注册模板做
//!   余弦相似度，取最高者：≥ 阈值 → 归入该说话人；< 阈值 → 新建说话人
//!   （id 自动递增：说话人 1/2/3 …）。
//! - **[`Scd::update_template`]**：把新向量并入模板（移动平均），增强鲁棒性。
//!   长会话颜色稳定依赖 **speaker_id 稳定**（[`crate::pipeline::speaker_color`]
//!   按 id 取模映射），而非模板向量精确 —— 只要同一人 id 不变，颜色就不跳变。
//! - **[`Scd::infer_gender`]**：音色选性别。T5 阶段无真实性别分类模型，
//!   MVP 降级为返回 [`Gender::Unknown`]（真实实现需 f0/性别分类模型，
//!   T6 可由用户手动指定性别覆盖）。

use crate::types::Gender;

/// 一条已注册的说话人模板（声纹 + 性别）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTemplate {
    /// 说话人 id（自动编号，1 起）。
    pub id: u32,
    /// 声纹向量（归一化后做余弦匹配）。随 [Scd::update_template] 移动平均更新。
    pub embedding: Vec<f32>,
    /// 说话人性别（音色推断结果，用于前端头像选择）。
    pub gender: Gender,
    /// 已并入的向量数（移动平均权重：avg = (avg·n + new) / (n+1)）。
    pub update_count: u32,
}

/// SCD 配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScdConfig {
    /// 余弦相似度阈值：最高相似度 **≥ 阈值** → 归入现有说话人；< 阈值 → 新建。
    /// 默认 0.75（对齐 ADR「超过阈值归入现有说话人，否则新建」的口径）。
    /// 真实声纹模型（如 3d-speaker eres2net）输出为归一化向量，0.75 是常用经验值；
    /// 阈值越大分组越严（易新建），越小越易误合并（T12 设置系统再做 UI 调节）。
    pub cosine_threshold: f32,
    /// 视为有效发言的最小字符数：短于它的 final（语气词/噪声，如「嗯」「哦」）
    /// 不参与新建说话人判定 —— 声纹质量不可靠，避免噪声制造出假说话人。
    pub min_speech_chars: usize,
}

impl Default for ScdConfig {
    fn default() -> Self {
        Self {
            cosine_threshold: 0.75,
            min_speech_chars: 2,
        }
    }
}

/// 一次说话人归属判定的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakerDecision {
    /// 归属的说话人 id。
    pub speaker_id: u32,
    /// 是否为新建说话人（前端据此决定是否创建新头像条目）。
    pub is_new_speaker: bool,
}

/// 说话人切换检测器：模板注册表 + 余弦匹配状态机。
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
    /// 最近一次归属的说话人（短发言/无声纹时沿用，避免噪声新建）。
    last_speaker_id: Option<u32>,
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
    ///
    /// 短于阈值的 final（语气词/咳嗽声等）声纹不可靠，不做新建判定。
    pub fn is_meaningful_speech(&self, text: &str) -> bool {
        text.chars().count() >= self.config.min_speech_chars
    }

    /// 判定一条 final 归属并（对有效发言）把新向量并入对应模板。
    ///
    /// Tauri 壳的完整入口：一条 final = 文本 + 该段音频的 embedding。
    /// 内部顺序：先判定归属，再并入模板（增强后续匹配）。新建说话人时模板
    /// 已用本条向量初始化（`update_count = 1`），不再重复并入。
    pub fn process_utterance(
        &mut self,
        text: &str,
        embedding: &[f32],
        gender_hint: Option<Gender>,
    ) -> SpeakerDecision {
        let decision = self.assign_for_utterance(text, embedding, gender_hint);
        if !decision.is_new_speaker && self.is_meaningful_speech(text) && !Self::is_empty_signal(embedding)
        {
            self.update_template(decision.speaker_id, embedding);
        }
        decision
    }

    /// 判定一条 final 的说话人归属（不并入模板）。
    ///
    /// - 有效发言：走余弦匹配（[`Self::assign_speaker`]）；
    /// - 过短发言或无声纹（空/全零向量）：沿用最近说话人（首个过短句归
    ///   说话人 1），**绝不新建** —— 噪声保护。
    pub fn assign_for_utterance(
        &mut self,
        text: &str,
        embedding: &[f32],
        gender_hint: Option<Gender>,
    ) -> SpeakerDecision {
        if !self.is_meaningful_speech(text) || Self::is_empty_signal(embedding) {
            let id = self.last_speaker_id.unwrap_or(1);
            return SpeakerDecision {
                speaker_id: id,
                is_new_speaker: false,
            };
        }
        self.assign_speaker(embedding, gender_hint)
    }

    /// 余弦匹配所有已注册模板，取最高相似度：
    /// **≥ 阈值 → 归入该说话人**（`is_new_speaker = false`）；
    /// **< 阈值 → 新建说话人**（id = 当前最大 + 1，`is_new_speaker = true`）。
    ///
    /// 空/全零向量视为「无声纹信号」，沿用最近说话人（由调用方
    /// [`Self::assign_for_utterance`] 拦截；直接调用本方法时也做同样兜底）。
    pub fn assign_speaker(&mut self, embedding: &[f32], gender_hint: Option<Gender>) -> SpeakerDecision {
        if Self::is_empty_signal(embedding) {
            let id = self.last_speaker_id.unwrap_or(1);
            return SpeakerDecision {
                speaker_id: id,
                is_new_speaker: false,
            };
        }

        // 匹配所有已注册模板，取最高相似度（平手取先注册者）。
        let mut best: Option<(f32, u32)> = None;
        for t in &self.templates {
            let sim = cosine_similarity(embedding, &t.embedding);
            if best.is_none_or(|(s, _)| sim > s) {
                best = Some((sim, t.id));
            }
        }

        if let Some((sim, id)) = best {
            if sim >= self.config.cosine_threshold {
                // 归入现有说话人；gender_hint 明确时修正模板性别（Unknown → 明确）。
                if let Some(hint) = gender_hint {
                    if hint != Gender::Unknown {
                        if let Some(t) = self.templates.iter_mut().find(|t| t.id == id) {
                            if t.gender == Gender::Unknown {
                                t.gender = hint;
                            }
                        }
                    }
                }
                self.last_speaker_id = Some(id);
                return SpeakerDecision {
                    speaker_id: id,
                    is_new_speaker: false,
                };
            }
        }

        // 低于阈值（或尚无任何模板）→ 新建说话人，id 自动递增（说话人 1/2/3 …）。
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
        SpeakerDecision {
            speaker_id: id,
            is_new_speaker: true,
        }
    }

    /// 把新向量并入指定说话人模板（移动平均），增强鲁棒性。
    ///
    /// 口径：`avg = (avg·n + new) / (n+1)`，其中 n 为已并入向量数。维度不一致
    /// 或模板不存在时静默忽略（稳健优先）。长会话颜色稳定依赖 speaker_id 稳定
    /// 而非模板精确，因此更新只是让后续匹配更稳，不应导致跳变。
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

    /// 音色选性别：根据声纹向量推断说话人性别。
    ///
    /// **T5 MVP 降级**：无性别分类模型时返回 [`Gender::Unknown`]。真实实现需
    /// 音色性别分类模型（如基于基频 f0 / 专用性别分类 embedding 的判别器），
    /// 接入点已预留 —— 有模型时在此实现，或由 Tauri 壳把 sidecar 的性别结果
    /// 作为 `gender_hint` 传入（优先级高于本函数）。T6 允许用户手动指定性别。
    pub fn infer_gender(&self, _embedding: &[f32]) -> Gender {
        Gender::Unknown
    }

    /// embedding 是否为「无声纹信号」：空向量或全零向量（无方向可比）。
    fn is_empty_signal(embedding: &[f32]) -> bool {
        embedding.is_empty() || embedding.iter().all(|x| *x == 0.0)
    }
}

/// 两个向量的余弦相似度：`a·b / (|a|·|b|)`，值域 [-1, 1]。
///
/// 稳健口径（注释即契约）：
/// - 维度不一致 → 返回 0（无法比较，不 panic）；
/// - 任一侧为零向量（模长为 0）→ 返回 0（无方向可比）；
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
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::speaker_color;

    fn config(threshold: f32) -> ScdConfig {
        ScdConfig {
            cosine_threshold: threshold,
            min_speech_chars: 2,
        }
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

    // ---- 验收：余弦相似度纯函数 ----

    #[test]
    fn cosine_similarity_basics() {
        // 相同向量 = 1
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        // 同向不同模长 = 1（余弦只看方向）
        assert!((cosine_similarity(&[2.0, 0.0], &[3.0, 0.0]) - 1.0).abs() < 1e-6);
        // 正交 = 0
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        // 相反 = -1
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // 零向量 = 0（两侧）
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
        // 维度不一致 = 0（稳健，不 panic）
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        // 空向量 = 0
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    // ---- 验收：同一人后续发言归入同一说话人 ----

    #[test]
    fn same_speaker_subsequent_utterances_join_same_id() {
        let mut scd = Scd::new(config(0.75));
        let d1 = scd.assign_speaker(&emb_a(), None);
        assert_eq!((d1.speaker_id, d1.is_new_speaker), (1, true), "首次出现 → 新建说话人 1");
        // 同一人（相同 embedding）后续发言 → 归入说话人 1，不再新建
        let d2 = scd.assign_speaker(&emb_a(), None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, false));
        let d3 = scd.assign_speaker(&emb_a(), None);
        assert_eq!((d3.speaker_id, d3.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 1, "全程只有一个说话人模板");
    }

    /// 近似 embedding（轻微噪声）仍应归入同一说话人（鲁棒性）。
    #[test]
    fn near_identical_embedding_joins_same_speaker() {
        let mut scd = Scd::new(config(0.75));
        scd.assign_speaker(&emb_a(), None);
        let noisy = vec![0.98, 0.05, -0.03];
        let d = scd.assign_speaker(&noisy, None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false), "噪声向量仍归说话人 1");
    }

    // ---- 验收：换人时新建说话人，id 自动递增 ----

    #[test]
    fn different_speakers_get_incremental_ids() {
        let mut scd = Scd::new(config(0.75));
        let d1 = scd.assign_speaker(&emb_a(), None);
        let d2 = scd.assign_speaker(&emb_b(), None);
        let d3 = scd.assign_speaker(&emb_c(), None);
        assert_eq!((d1.speaker_id, d1.is_new_speaker), (1, true));
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (2, true));
        assert_eq!((d3.speaker_id, d3.is_new_speaker), (3, true));
        assert_eq!(scd.speaker_count(), 3, "说话人 1/2/3 自动编号");
        // 说话人 1 再次出现 → 仍归 1（不新建）
        let d4 = scd.assign_speaker(&emb_a(), None);
        assert_eq!((d4.speaker_id, d4.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 3);
    }

    // ---- 验收：阈值边界（≥ 归入 / < 新建）----

    #[test]
    fn threshold_boundary_join_or_new() {
        let a = emb_a();
        // 与 a 夹角 41°：cos≈0.755 > 0.75 → 应归入；夹角 43°：cos≈0.731 < 0.75 → 应新建
        let near = vec![41.0f32.to_radians().cos(), 41.0f32.to_radians().sin(), 0.0];
        let below = vec![43.0f32.to_radians().cos(), 43.0f32.to_radians().sin(), 0.0];
        // 前置断言：实测相似度确实跨在阈值两侧
        assert!(cosine_similarity(&a, &near) >= 0.75 - 1e-4, "near 应高于阈值");
        assert!(cosine_similarity(&a, &below) < 0.75 - 1e-4, "below 应低于阈值");

        let mut scd = Scd::new(config(0.75));
        scd.assign_speaker(&a, None);
        let d_near = scd.assign_speaker(&near, None);
        assert_eq!((d_near.speaker_id, d_near.is_new_speaker), (1, false), "≥ 阈值 → 归入现有说话人");
        let d_below = scd.assign_speaker(&below, None);
        assert!(d_below.is_new_speaker, "< 阈值 → 新建说话人");
        assert_ne!(d_below.speaker_id, 1);
    }

    /// 阈值边界自洽性：任意角度下，判定结果必须与「sim ≥ 阈值」一致（≥ 归入 / < 新建）。
    #[test]
    fn threshold_decision_is_self_consistent() {
        let a = emb_a();
        for theta_deg in [0.0f32, 30.0, 41.0, 41.4096, 43.0, 60.0, 89.0] {
            let rad = theta_deg.to_radians();
            let b = vec![rad.cos(), rad.sin(), 0.0];
            let sim = cosine_similarity(&a, &b);
            // 每个角度用全新 Scd（避免上一轮新建的模板污染本轮匹配）
            let mut scd = Scd::new(config(0.75));
            scd.assign_speaker(&a, None);
            let d = scd.assign_speaker(&b, None);
            let expected_new = sim < 0.75;
            assert_eq!(
                d.is_new_speaker, expected_new,
                "θ={theta_deg}° sim={sim:.4}：≥ 阈值应归入（new=false），< 阈值应新建（new=true）"
            );
            assert_eq!(d.speaker_id, if expected_new { 2 } else { 1 });
        }
    }

    // ---- 验收：短发言/无声纹不新建（VAD 切句后的噪声保护）----

    #[test]
    fn short_speech_does_not_create_speaker() {
        let mut scd = Scd::new(ScdConfig {
            cosine_threshold: 0.75,
            min_speech_chars: 4, // 少于 4 字视为语气词/噪声
        });
        // 首个 final 是语气词「嗯」：不新建，沿用说话人 1，且不注册模板
        let d = scd.assign_for_utterance("嗯", &emb_a(), None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 0, "过短发言不注册模板");
        // 随后一段完整发言（新音色）→ 真正新建说话人 1
        let d2 = scd.assign_for_utterance("这是一段完整的话。", &emb_a(), None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, true));
        assert_eq!(scd.speaker_count(), 1);
        // 过短发言即使音色明显不同也不新建（沿用最近说话人）
        let d3 = scd.assign_for_utterance("哦", &emb_b(), None);
        assert_eq!((d3.speaker_id, d3.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 1, "噪声不产生假说话人");
    }

    #[test]
    fn empty_or_zero_embedding_reuses_last_speaker() {
        let mut scd = Scd::new(config(0.75));
        scd.assign_speaker(&emb_a(), None);
        // 无声纹信号（空向量 / 全零向量）：不新建，沿用最近说话人
        let d = scd.assign_for_utterance("没有声纹的一段话", &[], None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        let d2 = scd.assign_speaker(&[0.0, 0.0, 0.0], None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, false));
        assert_eq!(scd.speaker_count(), 1);
    }

    // ---- 验收：模板更新后匹配仍稳定（不跳变）----

    #[test]
    fn template_update_keeps_speaker_stable() {
        let mut scd = Scd::new(config(0.75));
        scd.assign_speaker(&emb_a(), None);
        // 后续发言向量轻微漂移，逐次并入模板（移动平均）
        for (i, delta) in [0.05f32, -0.03, 0.02, -0.01].into_iter().enumerate() {
            let drifted = vec![1.0 - delta.abs(), delta, 0.0];
            let d = scd.process_utterance(&format!("第 {i} 句发言"), &drifted, None);
            assert_eq!(d.speaker_id, 1, "模板更新后仍归同一说话人，不跳变");
            assert!(!d.is_new_speaker);
        }
        // 回到原始向量仍匹配说话人 1
        let d = scd.assign_speaker(&emb_a(), None);
        assert_eq!((d.speaker_id, d.is_new_speaker), (1, false));
        // 模板确实累计了并入次数
        let t = scd.templates().iter().find(|t| t.id == 1).unwrap();
        assert_eq!(t.update_count, 5, "初始 1 + 4 次并入");
    }

    // ---- 验收：长会话颜色稳定（speaker_id 恒定 → 颜色不跳变）----

    #[test]
    fn long_session_colors_are_stable() {
        let mut scd = Scd::new(config(0.75));
        // 长会话：说话人 A/B 交替发言 100 次，speaker_id 恒定
        for i in 0..100 {
            let (emb, expect) = if i % 2 == 0 { (emb_a(), 1) } else { (emb_b(), 2) };
            let d = scd.assign_speaker(&emb, None);
            assert_eq!(d.speaker_id, expect, "第 {i} 次发言 speaker_id 恒定");
        }
        // 颜色由 speaker_color(speaker_id) 取模映射：同一 id 恒定、不同 id 不同
        let color1 = speaker_color(1);
        let color2 = speaker_color(2);
        assert_eq!(color1, speaker_color(1));
        assert_eq!(color2, speaker_color(2));
        assert_ne!(color1, color2);
        for _ in 0..50 {
            assert_eq!(speaker_color(1), color1, "长会话中说话人 1 颜色绝不跳变");
        }
        // 8 色取模：1..=8 互异，9 回绕回 1 的颜色（仍稳定可复现）
        let colors: Vec<String> = (1..=8).map(speaker_color).collect();
        let unique: std::collections::HashSet<&String> = colors.iter().collect();
        assert_eq!(unique.len(), 8, "前 8 个说话人颜色互不相同");
        assert_eq!(speaker_color(9), speaker_color(1), "越界 id 取模回绕但仍稳定");
    }

    // ---- 验收：音色选性别（MVP 降级为 Unknown；gender_hint 优先）----

    #[test]
    fn gender_hint_sets_template_gender() {
        let mut scd = Scd::new(config(0.75));
        let d = scd.assign_speaker(&emb_a(), Some(Gender::Female));
        assert!(d.is_new_speaker);
        assert_eq!(scd.template_gender(d.speaker_id), Some(Gender::Female));
        // 无 hint 且无性别模型 → Unknown（MVP 降级）
        let mut scd2 = Scd::new(config(0.75));
        let d2 = scd2.assign_speaker(&emb_b(), None);
        assert_eq!(scd2.template_gender(d2.speaker_id), Some(Gender::Unknown));
    }

    #[test]
    fn infer_gender_degrades_to_unknown_without_model() {
        let scd = Scd::new(config(0.75));
        assert_eq!(
            scd.infer_gender(&emb_a()),
            Gender::Unknown,
            "无性别分类模型 → Unknown（T5 MVP 降级，真实实现需 f0/性别模型）"
        );
    }

    // ---- 完整入口：process_utterance（判定 + 并入模板）----

    #[test]
    fn process_utterance_assigns_and_updates_template() {
        let mut scd = Scd::new(config(0.75));
        let d1 = scd.process_utterance("第一句", &emb_a(), None);
        assert_eq!((d1.speaker_id, d1.is_new_speaker), (1, true));
        let d2 = scd.process_utterance("第二句", &emb_a(), None);
        assert_eq!((d2.speaker_id, d2.is_new_speaker), (1, false));
        // 模板并入两次向量后 update_count = 2
        let t = scd.templates().iter().find(|t| t.id == 1).unwrap();
        assert_eq!(t.update_count, 2);
        // 过短 final 不并入模板
        scd.process_utterance("嗯", &emb_b(), None);
        let t = scd.templates().iter().find(|t| t.id == 1).unwrap();
        assert_eq!(t.update_count, 2, "过短发言不并入模板");
        assert_eq!(scd.speaker_count(), 1, "过短发言不新建");
    }
}
