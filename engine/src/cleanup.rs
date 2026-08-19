//! LLM 整理管线：防抖 + 固定节奏冻结、单在途请求、editId 校验、失败回退。
//!
//! 设计要点（对齐 [ADR-0003](docs/adr/0003-dual-track-llm-cleanup.md) 双轨展示）：
//!
//! - **片段不可变**：`Segment.raw` 只写一次，整理结果写入 `cleaned`，绝不改动 `raw`。
//! - **只整理已冻结片段**：`Active`（interim）永不送 LLM；触发时才 `Active → Frozen`。
//! - **防抖 + 固定节奏**：距最后追加 `debounce_duration`（默认 2s）无新内容，或距
//!   上次节奏触发 `rhythm_duration`（默认 5s，可配 10s），即触发一次整理。
//! - **单在途**：同一时刻最多一个整理请求（`in_flight`/`pending`），防 LLM 请求打爆与乱序。
//! - **editId 校验**：`apply_cleanup` 只接受严格更大的 `edit_id`，旧结果/乱序到达被拒绝。
//! - **整理中保留原文**：流式增量只作为状态信号；最终批次结果到达后才切换展示。
//! - **失败回退**：LLM 失败置 `status = Failed`，前端展示原文。
//!
//! 时间模型：**逻辑时钟** —— 一切时刻是从管道创建起算的单调 [`Duration`]。
//! 真实运行由调用方以固定节拍喂 `now`（如每 100ms 一次 `tick`/`step`）；
//! 测试可任意推进时间而无需 `sleep`。`LlmPort` 通过 trait 注入，同步路径
//! 用 [`MockLlmPort`] 验证管线；T9 起真实 LLM 由 Tauri 壳走异步路径驱动：
//! 壳层 `tick(now)` 冻结并派发一个 `pending`，拿 [`CleanupPipeline::pending`]
//! 调真实 OpenAI 兼容接口（SSE 流式），增量以 `SegmentCleaning` 事件 emit，
//! 完成后经 [`CleanupPipeline::apply_cleanup_result`]（editId 校验）回填，
//! 失败经 [`CleanupPipeline::fail_pending`] 置 `Failed` 回退原文。
//!
//! # 状态说明
//!
//! 本模块由引擎单元测试完整验证（见 `mod tests`），并自 T9 起经 `lib.rs` 的
//! `pub use` 导出，供 Tauri 壳层（`src-tauri/src/pipeline.rs` 整理驱动）消费。

use std::time::Duration;

use crate::types::{EngineEvent, LlmPort, Segment, SegmentStatus};

/// 片段存储：不可变原文 + 冻结 + 整理结果 + editId 校验。
///
/// 单写入约束：本结构体由 `CleanupPipeline` 独占，同一时刻只有一个 writer
/// （管线在单线程事件循环内被调用）。并发安全由「单写入 + editId 校验」共同保证。
#[derive(Debug, Default)]
pub struct SegmentStore {
    segments: Vec<Segment>,
}

impl SegmentStore {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// 追加一条新片段。**原文只写一次**：调用方保证 `segment.raw` 之后不再变化，
    /// 本方法也只做追加，不触碰任何已存片段的 `raw`。
    pub fn append(&mut self, segment: Segment) {
        self.segments.push(segment);
    }

    /// 冻结最老的 `Active` 片段，返回其 id；没有可冻结的返回 `None`。
    pub fn freeze_oldest_active(&mut self) -> Option<u64> {
        let idx = self
            .segments
            .iter()
            .position(|s| s.status == SegmentStatus::Active)?;
        self.segments[idx].status = SegmentStatus::Frozen;
        Some(self.segments[idx].id)
    }

    /// 冻结全部 `Active` 片段（触发时刻它们都已满足防抖/节奏），返回冻结数量。
    pub fn freeze_all_active(&mut self) -> usize {
        let mut frozen = 0;
        for seg in &mut self.segments {
            if seg.status == SegmentStatus::Active {
                seg.status = SegmentStatus::Frozen;
                frozen += 1;
            }
        }
        frozen
    }

    /// 所有已冻结但尚未整理的片段（只读）。
    pub fn get_frozen_uncleaned(&self) -> Vec<&Segment> {
        self.segments
            .iter()
            .filter(|s| s.status == SegmentStatus::Frozen)
            .collect()
    }

    /// 最老的已冻结未整理片段（只读）。
    pub fn next_frozen_uncleaned(&self) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| s.status == SegmentStatus::Frozen)
    }

    /// 应用一次整理结果。**editId 校验**：只接受严格大于现有 `edit_id` 的结果，
    /// 旧结果 / 乱序到达返回 `false`（不生效，不丢已接受的更新）。
    pub fn apply_cleanup(&mut self, segment_id: u64, cleaned: String, edit_id: u64) -> bool {
        let Some(seg) = self.segments.iter_mut().find(|s| s.id == segment_id) else {
            return false;
        };
        // editId 单调：只接受更大的
        if seg.edit_id.is_some_and(|existing| edit_id <= existing) {
            return false;
        }
        seg.cleaned = Some(cleaned);
        seg.edit_id = Some(edit_id);
        seg.status = SegmentStatus::Cleaned;
        true
    }

    /// 标记整理失败：`status → Failed`，前端回退展示原文。返回是否生效。
    pub fn mark_failed(&mut self, segment_id: u64) -> bool {
        let Some(seg) = self.segments.iter_mut().find(|s| s.id == segment_id) else {
            return false;
        };
        seg.status = SegmentStatus::Failed;
        seg.retries += 1;
        true
    }

    pub fn get(&self, segment_id: u64) -> Option<&Segment> {
        self.segments.iter().find(|s| s.id == segment_id)
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

/// 整理调度器：防抖 + 固定节奏 + 单在途门控。
#[derive(Debug, Clone)]
pub struct CleanupScheduler {
    /// 防抖窗口：距最后追加这段时间无新内容即触发（默认 2s）。
    debounce_duration: Duration,
    /// 固定节奏：距上次节奏触发这段时间即触发（默认 5s，可配 10s）。
    rhythm_duration: Duration,
    /// 逻辑时钟：最后追加时刻（管道创建起算的单调时长）。
    last_append_time: Duration,
    /// 逻辑时钟：上次节奏触发时刻。
    last_rhythm_trigger: Duration,
    /// 单在途请求标志：为 true 时不得触发新一轮整理。
    in_flight: bool,
}

impl CleanupScheduler {
    pub fn new(debounce_duration: Duration, rhythm_duration: Duration) -> Self {
        Self {
            debounce_duration,
            rhythm_duration,
            last_append_time: Duration::ZERO,
            last_rhythm_trigger: Duration::ZERO,
            in_flight: false,
        }
    }

    /// 是否应触发一次整理。
    ///
    /// 满足其一即触发：
    /// - 防抖：距最后追加已过 `debounce_duration`（说话停顿，finalize 当前片段）；
    /// - 固定节奏：距上次节奏触发已过 `rhythm_duration`（持续说话时兜底 flush）。
    ///
    /// 有在途请求时始终返回 `false`（单在途）。
    pub fn should_trigger(&self, now: Duration) -> bool {
        if self.in_flight {
            return false;
        }
        let since_append = now.saturating_sub(self.last_append_time);
        if since_append >= self.debounce_duration {
            return true;
        }
        let since_rhythm = now.saturating_sub(self.last_rhythm_trigger);
        since_rhythm >= self.rhythm_duration
    }

    /// 记录一次触发：重置节奏时钟（防抖基准保持不变，未追加时持续触发以排空）。
    pub fn mark_triggered(&mut self, now: Duration) {
        self.last_rhythm_trigger = now;
    }

    /// 记录一次追加：重置防抖基准。
    pub fn on_append(&mut self, now: Duration) {
        self.last_append_time = now;
    }

    /// 设定/清除在途标志（单在途门控）。
    pub fn set_in_flight(&mut self, in_flight: bool) {
        self.in_flight = in_flight;
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    pub fn debounce_duration(&self) -> Duration {
        self.debounce_duration
    }

    pub fn rhythm_duration(&self) -> Duration {
        self.rhythm_duration
    }
}

/// 一次在途整理请求（单在途：同一时刻至多一个）。
#[derive(Debug, Clone)]
pub struct PendingCleanup {
    /// 批次主片段：同一说话人未整理片段中的最新一条，用于流式占位事件。
    pub segment_id: u64,
    /// 本批次覆盖的全部片段（同一说话人、已冻结、未整理，按 id 升序）。
    pub segment_ids: Vec<u64>,
    /// 本批次说话人。
    pub speaker_id: u32,
    /// 本次请求预分配的 editId（全局单调）。
    pub edit_id: u64,
    /// 本批次汇整后的待整理原文。
    pub raw: String,
}

/// 整理管线：调度 + 冻结 + 派发 + 结果校验，产出 [`EngineEvent`]。
///
/// 使用方式（同步模拟 LLM）：
/// ```ignore
/// let mut p = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
/// let mut events = vec![p.append(now, 1, "原文".into())];
/// events.extend(p.step(now + 2s));   // 防抖触发 → 冻结 → 调 mock LLM → SegmentCleaned
/// ```
///
/// 异步 LLM（T9）：`tick(now)` 冻结并派发一个 `pending`，调用方拿
/// `pending()` 自行调 LLM 后经 `apply_cleanup_result` / `fail_pending` 回填，
/// 单在途保证同一时刻只有一个请求在飞。
pub struct CleanupPipeline {
    store: SegmentStore,
    scheduler: CleanupScheduler,
    llm: Box<dyn LlmPort>,
    next_segment_id: u64,
    next_edit_id: u64,
    pending: Option<PendingCleanup>,
}

impl CleanupPipeline {
    pub fn new(
        debounce_duration: Duration,
        rhythm_duration: Duration,
        llm: Box<dyn LlmPort>,
    ) -> Self {
        Self {
            store: SegmentStore::new(),
            scheduler: CleanupScheduler::new(debounce_duration, rhythm_duration),
            llm,
            next_segment_id: 0,
            next_edit_id: 0,
            pending: None,
        }
    }

    /// 默认参数：防抖 2s，固定节奏 5s。
    pub fn new_with_defaults(llm: Box<dyn LlmPort>) -> Self {
        Self::new(Duration::from_secs(2), Duration::from_secs(5), llm)
    }

    /// 追加一段不可变原文，生成全局单调 id，产出 `SegmentAppended`。
    pub fn append(&mut self, now: Duration, speaker_id: u32, raw: String) -> EngineEvent {
        let id = self.next_segment_id;
        self.next_segment_id += 1;
        let segment = Segment {
            id,
            speaker_id,
            raw,
            status: SegmentStatus::Active,
            cleaned: None,
            edit_id: None,
            ts: now.as_millis() as u64,
            retries: 0,
        };
        self.store.append(segment.clone());
        self.scheduler.on_append(now);
        EngineEvent::SegmentAppended { segment }
    }

    /// 时钟滴答：判断是否触发 → 冻结全部 active → （单在途）派发一个新整理请求。
    ///
    /// 返回是否触发（发生了冻结）。派发是否发生看 [`Self::has_pending`]。
    pub fn tick(&mut self, now: Duration) -> bool {
        if !self.scheduler.should_trigger(now) {
            return false;
        }
        self.scheduler.mark_triggered(now);
        self.store.freeze_all_active();
        self.dispatch_next();
        true
    }

    /// 派发下一个待整理批次（单在途：已在途则不派发）。返回是否派发。
    ///
    /// 批次规则：取最老已冻结未整理片段的说话人，把**该说话人所有**
    /// 已冻结未整理片段按 id 升序汇整成一个请求。其他说话人的片段保持
    /// Frozen，等待下一个批次，避免不同人的内容被混进同一次整理。
    fn dispatch_next(&mut self) -> bool {
        if self.scheduler.is_in_flight() || self.pending.is_some() {
            return false;
        }
        let Some(primary) = self.store.next_frozen_uncleaned().map(|s| s.clone()) else {
            return false;
        };
        let batch: Vec<Segment> = self
            .store
            .get_frozen_uncleaned()
            .into_iter()
            .filter(|seg| seg.speaker_id == primary.speaker_id)
            .cloned()
            .collect();
        let Some(latest) = batch.last() else {
            return false;
        };
        let raw = batch
            .iter()
            .map(|seg| seg.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let edit_id = self.next_edit_id;
        self.next_edit_id += 1;
        self.scheduler.set_in_flight(true);
        self.pending = Some(PendingCleanup {
            segment_id: latest.id,
            segment_ids: batch.iter().map(|seg| seg.id).collect(),
            speaker_id: primary.speaker_id,
            edit_id,
            raw,
        });
        true
    }

    /// 单步推进（同步模拟路径）：`tick` + 串行排空全部已冻结未整理片段。
    ///
    /// 单在途语义下同一时刻至多一个请求在飞；同步 mock LLM 瞬时完成，
    /// 因此一次调用即可依次完成所有到期片段（真正异步时由调用方逐条
    /// 经 `tick` + `run_pending_with_llm` 驱动）。
    pub fn step(&mut self, now: Duration) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        self.tick(now);
        loop {
            if !self.has_pending() && !self.dispatch_next() {
                break;
            }
            events.extend(self.run_pending_with_llm());
        }
        events
    }

    /// 同步完成在途请求：调注入的 LLM 整理并应用结果。
    pub fn run_pending_with_llm(&mut self) -> Vec<EngineEvent> {
        let Some(p) = self.pending.take() else {
            return Vec::new();
        };
        self.scheduler.set_in_flight(false);
        let cleaned = self.llm.cleanup(&p.raw);
        let segment_ids = p.segment_ids.clone();
        let edit_id = p.edit_id;
        self.apply_cleanup_result(&segment_ids, cleaned, edit_id)
    }

    /// 失败路径：LLM 出错时调用，置 `Failed` 回退原文，产出 `CleanupFailed`。
    pub fn fail_pending(&mut self) -> Vec<EngineEvent> {
        let Some(p) = self.pending.take() else {
            return Vec::new();
        };
        self.scheduler.set_in_flight(false);
        let mut events = Vec::new();
        for segment_id in p.segment_ids {
            if self.store.mark_failed(segment_id) {
                events.push(EngineEvent::CleanupFailed { segment_id });
            }
        }
        events
    }

    /// 应用一次外部 LLM 整理结果（异步场景经此回填）。editId 校验由
    /// [`SegmentStore::apply_cleanup`] 执行：只接受严格更大的，否则丢弃。
    ///
    /// 与 [Self::fail_pending] 对称：无论结果是否生效，都结束本次在途请求
    /// （清 `pending` 与单在途标志）——否则异步成功路径会残留在途状态，
    /// 驱动线程将无限重复处理同一条 pending（T9 遗留缺口，见测试
    /// `async_apply_cleanup_result_releases_in_flight_and_pending`）。
    pub fn apply_cleanup_result(
        &mut self,
        segment_ids: &[u64],
        cleaned: String,
        edit_id: u64,
    ) -> Vec<EngineEvent> {
        // 异步调用方会先 clone pending 再请求 LLM；这里按 editId 清掉匹配的
        // 在途状态。同步路径已在 run_pending_with_llm 中 take，这里自然跳过。
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.edit_id == edit_id)
        {
            self.pending.take();
        }
        self.scheduler.set_in_flight(false);

        let mut applied_ids = Vec::new();
        for &segment_id in segment_ids {
            if self
                .store
                .apply_cleanup(segment_id, cleaned.clone(), edit_id)
            {
                applied_ids.push(segment_id);
            }
        }
        if applied_ids.is_empty() {
            Vec::new()
        } else {
            vec![EngineEvent::SegmentsCleaned {
                segment_ids: applied_ids,
                cleaned,
                edit_id,
            }]
        }
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn pending(&self) -> Option<&PendingCleanup> {
        self.pending.as_ref()
    }

    pub fn store(&self) -> &SegmentStore {
        &self.store
    }

    pub fn scheduler(&self) -> &CleanupScheduler {
        &self.scheduler
    }
}

/// 模拟 LLM 端口：确定性、可断言的整理输出，用于验证管线（T9 前）。
///
/// - `cleanup`：合并连续空白、去首尾空白、句末无标点则补「。」。
/// - `summarize`：拼接要点并标注段落数。
#[derive(Debug, Clone, Copy, Default)]
pub struct MockLlmPort;

impl LlmPort for MockLlmPort {
    fn cleanup(&self, text: &str) -> String {
        // 合并连续空白 + 去首尾空白
        let mut collapsed = String::with_capacity(text.len());
        let mut prev_space = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !prev_space {
                    collapsed.push(' ');
                }
                prev_space = true;
            } else {
                prev_space = false;
                collapsed.push(ch);
            }
        }
        let collapsed = collapsed.trim();
        if collapsed.is_empty() {
            return String::new();
        }
        // 句末无标点则补「。」
        let last = collapsed.chars().last();
        if matches!(last, Some('。' | '！' | '？' | '；' | '，' | '：' | '…')) {
            collapsed.to_string()
        } else {
            format!("{collapsed}。")
        }
    }

    fn summarize(&self, chunks: &[String]) -> String {
        if chunks.is_empty() {
            "（无内容）".to_string()
        } else {
            format!("要点：{}（共 {} 段）", chunks.join("；"), chunks.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SegmentStatus;

    /// 便捷：构造 n 秒时刻。
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn mock_pipeline() -> CleanupPipeline {
        CleanupPipeline::new_with_defaults(Box::new(MockLlmPort))
    }

    // ---- 验收 1：片段不可变、只整理已冻结片段、interim 不送 LLM ----

    #[test]
    fn append_is_immutable_raw_never_changes() {
        let mut p = mock_pipeline();
        let raw = " 这个项目，我们 下周二搞定 ".to_string();
        p.append(secs(0), 1, raw.clone());

        // 触发并完成整理
        let events = p.step(secs(3)); // 防抖 2s 已过
        assert!(events
            .iter()
            .any(|e| matches!(e, EngineEvent::SegmentsCleaned { .. })));

        let seg = p.store().get(0).expect("segment 0 exists");
        // raw 不可变：整理前后一字未改
        assert_eq!(seg.raw, raw);
        // 整理结果写入 cleaned（新字段），不是覆盖 raw
        assert_eq!(seg.cleaned.as_deref(), Some("这个项目，我们 下周二搞定。"));
        assert_ne!(seg.raw, seg.cleaned.as_deref().unwrap());
    }

    #[test]
    fn interim_active_is_never_cleaned() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "还在说话中".to_string());

        // 防抖窗口内多次 tick：不触发、不冻结、不派发、无整理事件
        for t in [
            Duration::from_millis(500),
            secs(1),
            Duration::from_millis(1500),
        ] {
            assert!(!p.tick(t), "t={t:?} 不应触发");
        }
        assert!(!p.has_pending());
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Active);
        assert!(p.store().get(0).unwrap().cleaned.is_none());

        // step 在窗口内也不应产出任何清理事件（interim 不送 LLM）
        let events = p.step(Duration::from_millis(1500));
        assert!(events.is_empty());
    }

    /// 回归：同一次整理必须把同一说话人所有已冻结未整理片段合并成一个请求。
    ///
    /// 用户反馈：现在每条 ASR final 单独整理，导致同一人的连续语义被拆碎。
    /// 正确行为是以“说话人 + 未整理”为批次，而不是以单条 segment 为批次。
    #[test]
    fn pending_cleanup_groups_same_speaker_uncleaned_segments() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "第一句".to_string());
        p.append(secs(1), 2, "别人插入".to_string());
        p.append(secs(2), 1, "第三句".to_string());

        assert!(p.tick(secs(4)), "防抖到期应触发整理");
        let pending = p.pending().expect("应产生整理请求");
        assert_eq!(pending.segment_id, 2, "批次主片段应为该说话人最新片段");
        assert_eq!(
            pending.raw, "第一句\n第三句",
            "同一说话人的未整理原文必须汇整后送 LLM"
        );
        assert_eq!(
            p.store().get(1).map(|s| s.status),
            Some(SegmentStatus::Frozen),
            "其他说话人的片段不应混入本批次"
        );

        let events = p.run_pending_with_llm();
        assert!(matches!(
            events.as_slice(),
            [EngineEvent::SegmentsCleaned { segment_ids, edit_id, .. }]
                if segment_ids == &[0, 2] && *edit_id == 0
        ));
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Cleaned);
        assert_eq!(p.store().get(2).unwrap().status, SegmentStatus::Cleaned);
        assert_eq!(
            p.store().get(0).unwrap().cleaned,
            p.store().get(2).unwrap().cleaned
        );
    }

    /// 回归：LLM 在途整理期间，新识别文字仍可无条件追加并实时可见。
    #[test]
    fn pending_does_not_block_new_utterance_append() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "正在整理的一段".to_string());
        assert!(p.tick(secs(2))); // 触发并进入在途

        // 整理未完成时继续说话：新 final 必须照常进入片段存储（Active）。
        p.append(secs(3), 1, "整理期间新识别的一句话".to_string());
        assert_eq!(p.store().len(), 2, "在途期间追加不得被丢弃或延迟");
        let seg = p.store().get(1).unwrap();
        assert_eq!(seg.raw, "整理期间新识别的一句话");
        assert_eq!(seg.status, SegmentStatus::Active, "新片段保持 Active，不参与在途批次");
    }

    #[test]
    fn only_frozen_segments_are_dispatched() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "第一段".to_string()); // id 0
        p.append(secs(1), 1, "第二段".to_string()); // id 1

        // t=3s 触发 → 全部冻结并派发最老一段
        assert!(p.tick(secs(3)));
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Frozen);
        assert_eq!(p.store().get(1).unwrap().status, SegmentStatus::Frozen);
        assert!(p.has_pending());
        assert_eq!(p.pending().unwrap().segment_id, 1);
        assert_eq!(p.pending().unwrap().segment_ids, vec![0, 1]);
        assert_eq!(p.pending().unwrap().raw, "第一段\n第二段");

        // 此刻新追加一条 active：不得被派发（interim 不送 LLM）
        p.append(secs(3), 1, "还在说".to_string());
        // 完成在途后，下一轮派发只挑 frozen，跳过 active
        let events = p.run_pending_with_llm();
        assert!(events.len() == 1);
        assert!(!p.has_pending());
        // 立即再 tick：新的 active 未到冻结时机也不得派发
        // （防抖窗口内 last_append=3s，t=3.5 不触发）
        assert!(!p.tick(secs(3) + secs(1) / 2));
        assert!(!p.has_pending());
    }

    // ---- 验收 2：防抖（2s 无追加）+ 固定节奏（5s/10s）----

    #[test]
    fn debounce_does_not_trigger_within_2s() {
        let s = mock_pipeline().scheduler().clone();
        // 模拟连续追加：每 0.5s 一次，防抖基准不断刷新
        let mut sched = s;
        sched.on_append(secs(0));
        sched.on_append(Duration::from_millis(500));
        sched.on_append(Duration::from_millis(1000));
        // 距最后追加仅 1s：不触发
        assert!(!sched.should_trigger(Duration::from_millis(2000)));
    }

    #[test]
    fn debounce_triggers_after_2s_idle() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "说完停顿".to_string());
        // 1s：不触发
        assert!(!p.tick(secs(1)));
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Active);
        // 2s：防抖触发 → 冻结
        assert!(p.tick(secs(2)));
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Frozen);
        assert!(p.has_pending());
    }

    #[test]
    fn rhythm_triggers_every_5s_during_continuous_speech() {
        let mut p = mock_pipeline();
        // 持续说话：每 1s 追加一条，t=5s 时距最后追加仅 1s → 防抖（2s）永不满足
        for t in 0..5 {
            p.append(secs(t), 1, format!("第{t}段"));
        }
        // t=5s：防抖未到，但距上次节奏触发已 5s → 固定节奏触发
        assert_eq!(p.scheduler().debounce_duration(), secs(2), "前置：防抖 2s");
        assert!(p.tick(secs(5)), "t=5s 应因固定节奏触发（防抖未到）");
        assert!(p.has_pending());
    }

    #[test]
    fn rhythm_duration_is_configurable_to_10s() {
        let mut p = CleanupPipeline::new(secs(2), secs(10), Box::new(MockLlmPort));
        // 持续说话到 t=9s：每 1s 追加，防抖永不满足
        for t in 0..10 {
            p.append(secs(t), 1, format!("第{t}段"));
        }
        // 5s：节奏 10s 未到、防抖未到 → 不触发
        assert!(!p.tick(secs(5)), "t=5s 配置 10s 节奏不应触发");
        // 10s：距上次节奏触发 10s → 固定节奏触发
        assert!(p.tick(secs(10)), "t=10s 应因 10s 节奏触发（防抖未到）");
    }

    // ---- 验收：单在途 ----

    #[test]
    fn single_in_flight_blocks_new_trigger_and_dispatch() {
        let mut p = mock_pipeline();
        for t in 0..3 {
            p.append(secs(t), 1, format!("段{t}"));
        }
        // t=5s 节奏触发 -> 冻结全部；同一说话人 3 条汇成一个批次
        assert!(p.tick(secs(5)));
        assert_eq!(p.store().get_frozen_uncleaned().len(), 3, "3 条全部冻结");
        assert!(p.has_pending());
        assert_eq!(p.pending().unwrap().segment_id, 2);
        assert_eq!(p.pending().unwrap().segment_ids, vec![0, 1, 2]);

        // 在途未完成时：再过多久都不触发新一轮、不派发新请求
        assert!(!p.tick(secs(20)), "在途时应封锁触发");
        assert!(!p.tick(secs(100)));
        assert_eq!(p.pending().unwrap().segment_id, 2, "在途请求不得被替换");

        // 完成在途批次 -> 3 条全部落库，且不再残留该 speaker 的旧片段
        p.run_pending_with_llm();
        assert!(!p.has_pending());
        assert!(p
            .store()
            .segments()
            .iter()
            .all(|s| s.status == SegmentStatus::Cleaned));
    }

    // ---- 验收：editId 校验 / 乱序 ----

    /// T9 遗留缺口回归：异步成功路径（`apply_cleanup_result`）必须结束本次
    /// 在途请求（清 pending + 解锁单在途），否则驱动线程无限重复处理同一
    /// pending（T10 停止排空循环同样依赖此行为）。
    #[test]
    fn async_apply_cleanup_result_releases_in_flight_and_pending() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "异步整理".to_string());
        assert!(p.tick(secs(3)));
        assert!(p.has_pending());
        let (pid, eid) = (
            p.pending().unwrap().segment_id,
            p.pending().unwrap().edit_id,
        );

        // 异步成功路径：应用结果 -> 产出 SegmentsCleaned，且必须清 pending 并解锁单在途
        let events = p.apply_cleanup_result(&[pid], "整理完成".into(), eid);
        assert!(events.iter().any(|e| matches!(e, EngineEvent::SegmentsCleaned { segment_ids, .. } if segment_ids.contains(&pid))));
        assert!(!p.has_pending(), "成功路径必须清 pending");
        assert!(!p.scheduler().is_in_flight(), "成功路径必须解锁单在途");
        assert_eq!(p.store().get(pid).unwrap().status, SegmentStatus::Cleaned);

        // 解锁后可继续派发新片段
        p.append(secs(3), 1, "第二条".to_string());
        assert!(p.tick(secs(20)));
        assert!(p.has_pending());
        assert_eq!(p.pending().unwrap().segment_id, 1);
    }

    #[test]
    fn edit_id_rejects_stale_results() {
        let mut store = SegmentStore::new();
        store.append(Segment {
            id: 0,
            speaker_id: 1,
            raw: "原文".into(),
            status: SegmentStatus::Frozen,
            cleaned: None,
            edit_id: None,
            ts: 0,
            retries: 0,
        });

        // 首次应用 edit_id=5：生效
        assert!(store.apply_cleanup(0, "整理版A".into(), 5));
        assert_eq!(store.get(0).unwrap().cleaned.as_deref(), Some("整理版A"));

        // 旧 edit_id 被拒绝
        assert!(!store.apply_cleanup(0, "旧结果".into(), 4));
        assert!(!store.apply_cleanup(0, "旧结果2".into(), 5)); // 相等也拒绝
        assert_eq!(store.get(0).unwrap().cleaned.as_deref(), Some("整理版A"));

        // 更大 edit_id 生效
        assert!(store.apply_cleanup(0, "整理版B".into(), 6));
        assert_eq!(store.get(0).unwrap().cleaned.as_deref(), Some("整理版B"));
        assert_eq!(store.get(0).unwrap().edit_id, Some(6));
    }

    #[test]
    fn out_of_order_results_only_max_edit_id_applies() {
        let mut store = SegmentStore::new();
        store.append(Segment {
            id: 0,
            speaker_id: 1,
            raw: "原始完整文本".into(),
            status: SegmentStatus::Frozen,
            cleaned: None,
            edit_id: None,
            ts: 0,
            retries: 0,
        });
        store.append(Segment {
            id: 1,
            speaker_id: 1,
            raw: "第二段原始文本".into(),
            status: SegmentStatus::Frozen,
            cleaned: None,
            edit_id: None,
            ts: 0,
            retries: 0,
        });

        // 模拟并发完成乱序到达：
        // 段0 先到 edit_id=3，后到 edit_id=1（应被拒）
        assert!(store.apply_cleanup(0, "段0-v3".into(), 3));
        assert!(!store.apply_cleanup(0, "段0-v1".into(), 1));
        // 段1 先到 edit_id=2，后到 edit_id=4（应生效）
        assert!(store.apply_cleanup(1, "段1-v2".into(), 2));
        assert!(store.apply_cleanup(1, "段1-v4".into(), 4));

        // 只有每个片段 editId 最大的结果生效：不丢字、不乱序
        let seg0 = store.get(0).unwrap();
        let seg1 = store.get(1).unwrap();
        assert_eq!(seg0.cleaned.as_deref(), Some("段0-v3"));
        assert_eq!(seg0.edit_id, Some(3));
        assert_eq!(seg1.cleaned.as_deref(), Some("段1-v4"));
        assert_eq!(seg1.edit_id, Some(4));
    }

    // ---- 验收：失败回退 ----

    #[test]
    fn cleanup_failure_falls_back_to_raw() {
        let mut p = mock_pipeline();
        p.append(secs(0), 1, "这一段整理失败".to_string());
        assert!(p.tick(secs(3))); // 触发 + 派发
        assert!(p.has_pending());

        // LLM 失败：fail_pending → CleanupFailed，status=Failed，cleaned 保持 None（展示原文）
        let events = p.fail_pending();
        assert!(events
            .iter()
            .any(|e| matches!(e, EngineEvent::CleanupFailed { segment_id: 0 })));
        let seg = p.store().get(0).unwrap();
        assert_eq!(seg.status, SegmentStatus::Failed);
        assert!(seg.cleaned.is_none());
        assert_eq!(seg.retries, 1);
        // 原文完整保留
        assert_eq!(seg.raw, "这一段整理失败");
        // 解锁在途
        assert!(!p.has_pending());
    }

    // ---- 端到端：事件流 + editId 单调 ----

    #[test]
    fn pipeline_emits_full_event_flow_with_monotonic_edit_ids() {
        let mut p = mock_pipeline();
        let mut events = Vec::new();

        events.push(p.append(secs(0), 2, "第一句话".to_string()));
        events.push(p.append(secs(1), 2, "第二句话".to_string()));
        events.extend(p.step(secs(3))); // 防抖触发 → 两条都整理

        let cleaned_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::SegmentsCleaned {
                    segment_ids,
                    cleaned,
                    edit_id,
                } => Some((segment_ids.clone(), cleaned.clone(), *edit_id)),
                _ => None,
            })
            .collect();

        // 同一说话人两条原文作为一个批次整理，且 editId 全局单调
        assert_eq!(cleaned_events.len(), 1);
        assert_eq!(cleaned_events[0].0, vec![0, 1], "批次覆盖两条片段");
        assert_eq!(cleaned_events[0].2, 0, "第一批 editId");

        // 状态与内容落库：两个片段共享同一次整理结果
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Cleaned);
        assert_eq!(p.store().get(1).unwrap().status, SegmentStatus::Cleaned);
        assert_eq!(
            p.store().get(0).unwrap().cleaned.as_deref(),
            Some("第一句话 第二句话。")
        );
        assert_eq!(
            p.store().get(1).unwrap().cleaned,
            p.store().get(0).unwrap().cleaned
        );
    }

    #[test]
    fn mock_llm_cleanup_is_deterministic_and_adds_punctuation() {
        let llm = MockLlmPort;
        assert_eq!(llm.cleanup("  你好 世界 "), "你好 世界。");
        assert_eq!(llm.cleanup("已经说完。"), "已经说完。");
        assert_eq!(llm.cleanup("   "), "");
        assert_eq!(
            llm.summarize(&["a".into(), "b".into()]),
            "要点：a；b（共 2 段）"
        );
    }
}
