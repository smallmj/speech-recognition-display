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
//! - **失败回退**：LLM 失败置 `status = Failed`，前端展示原文。
//!
//! 时间模型：**逻辑时钟** —— 一切时刻是从管道创建起算的单调 [`Duration`]。
//! 真实运行由调用方以固定节拍喂 `now`（如每 100ms 一次 `tick`/`step`）；
//! 测试可任意推进时间而无需 `sleep`。`LlmPort` 通过 trait 注入，本票用
//! [`MockLlmPort`] 验证管线（T9 再接真实 LLM）。
//!
//! # 状态说明
//!
//! 本模块当前由引擎单元测试完整验证（见 `mod tests`）；真实消费方（Tauri 壳
//! 接入、T9 真实 LLM 接入、与 `pipeline::Engine` 的串接）在后续票落地，接入前
//! 允许「构造未使用」的 `dead_code` 告警，以保持本票只改 `lib.rs` 一行、不动
//! 并行票（T2 `pipeline.rs`）文件的约束。
#![allow(dead_code)]

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
        Self { segments: Vec::new() }
    }

    /// 追加一条新片段。**原文只写一次**：调用方保证 `segment.raw` 之后不再变化，
    /// 本方法也只做追加，不触碰任何已存片段的 `raw`。
    pub fn append(&mut self, segment: Segment) {
        self.segments.push(segment);
    }

    /// 冻结最老的 `Active` 片段，返回其 id；没有可冻结的返回 `None`。
    pub fn freeze_oldest_active(&mut self) -> Option<u64> {
        let idx = self.segments.iter().position(|s| s.status == SegmentStatus::Active)?;
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
        self.segments.iter().filter(|s| s.status == SegmentStatus::Frozen).collect()
    }

    /// 最老的已冻结未整理片段（只读）。
    pub fn next_frozen_uncleaned(&self) -> Option<&Segment> {
        self.segments.iter().find(|s| s.status == SegmentStatus::Frozen)
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
    pub segment_id: u64,
    /// 本次请求预分配的 editId（全局单调）。
    pub edit_id: u64,
    /// 待整理的不可变原文。
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
    pub fn new(debounce_duration: Duration, rhythm_duration: Duration, llm: Box<dyn LlmPort>) -> Self {
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

    /// 派发下一个待整理片段（单在途：已在途则不派发）。返回是否派发。
    fn dispatch_next(&mut self) -> bool {
        if self.scheduler.is_in_flight() || self.pending.is_some() {
            return false;
        }
        let Some(seg) = self.store.next_frozen_uncleaned().map(|s| s.clone()) else {
            return false;
        };
        let edit_id = self.next_edit_id;
        self.next_edit_id += 1;
        self.scheduler.set_in_flight(true);
        self.pending = Some(PendingCleanup {
            segment_id: seg.id,
            edit_id,
            raw: seg.raw.clone(),
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
        self.apply_cleanup_result(p.segment_id, cleaned, p.edit_id)
    }

    /// 失败路径：LLM 出错时调用，置 `Failed` 回退原文，产出 `CleanupFailed`。
    pub fn fail_pending(&mut self) -> Vec<EngineEvent> {
        let Some(p) = self.pending.take() else {
            return Vec::new();
        };
        self.scheduler.set_in_flight(false);
        if self.store.mark_failed(p.segment_id) {
            vec![EngineEvent::CleanupFailed { segment_id: p.segment_id }]
        } else {
            Vec::new()
        }
    }

    /// 应用一次外部 LLM 整理结果（异步场景经此回填）。editId 校验由
    /// [`SegmentStore::apply_cleanup`] 执行：只接受严格更大的，否则丢弃。
    pub fn apply_cleanup_result(&mut self, segment_id: u64, cleaned: String, edit_id: u64) -> Vec<EngineEvent> {
        if self.store.apply_cleanup(segment_id, cleaned.clone(), edit_id) {
            vec![EngineEvent::SegmentCleaned { segment_id, cleaned, edit_id }]
        } else {
            Vec::new()
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
        assert!(events.iter().any(|e| matches!(e, EngineEvent::SegmentCleaned { .. })));

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
        for t in [Duration::from_millis(500), secs(1), Duration::from_millis(1500)] {
            assert!(!p.tick(t), "t={t:?} 不应触发");
        }
        assert!(!p.has_pending());
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Active);
        assert!(p.store().get(0).unwrap().cleaned.is_none());

        // step 在窗口内也不应产出任何清理事件（interim 不送 LLM）
        let events = p.step(Duration::from_millis(1500));
        assert!(events.is_empty());
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
        assert_eq!(p.pending().unwrap().segment_id, 0);
        assert_eq!(p.pending().unwrap().raw, "第一段");

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
        // t=5s 节奏触发 → 冻结全部，派发 1 条（在途片段在 store 中仍为 Frozen，
        // 直到整理结果落库才变 Cleaned）
        assert!(p.tick(secs(5)));
        assert_eq!(p.store().get_frozen_uncleaned().len(), 3, "3 条全部冻结");
        assert!(p.has_pending());
        assert_eq!(p.pending().unwrap().segment_id, 0);

        // 在途未完成时：再过多久都不触发新一轮、不派发新请求
        assert!(!p.tick(secs(20)), "在途时应封锁触发");
        assert!(!p.tick(secs(100)));
        assert_eq!(p.pending().unwrap().segment_id, 0, "在途请求不得被替换");

        // 完成在途 → 解锁，下一轮才派发第二条
        p.run_pending_with_llm();
        assert!(p.tick(secs(20)));
        assert_eq!(p.pending().unwrap().segment_id, 1);
    }

    // ---- 验收：editId 校验 / 乱序 ----

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
        assert!(events.iter().any(|e| matches!(e, EngineEvent::CleanupFailed { segment_id: 0 })));
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
                EngineEvent::SegmentCleaned { segment_id, cleaned, edit_id } => {
                    Some((*segment_id, cleaned.clone(), *edit_id))
                }
                _ => None,
            })
            .collect();

        // 两条都被整理，editId 单调递增
        assert_eq!(cleaned_events.len(), 2);
        let ids: Vec<u64> = cleaned_events.iter().map(|(_, _, e)| *e).collect();
        assert_eq!(ids, vec![0, 1], "editId 单调递增");
        let seg_ids: Vec<u64> = cleaned_events.iter().map(|(s, _, _)| *s).collect();
        assert_eq!(seg_ids, vec![0, 1], "按追加顺序整理，不乱序");

        // 状态与内容落库
        assert_eq!(p.store().get(0).unwrap().status, SegmentStatus::Cleaned);
        assert_eq!(p.store().get(0).unwrap().cleaned.as_deref(), Some("第一句话。"));
        assert_eq!(p.store().get(1).unwrap().cleaned.as_deref(), Some("第二句话。"));
    }

    #[test]
    fn mock_llm_cleanup_is_deterministic_and_adds_punctuation() {
        let llm = MockLlmPort;
        assert_eq!(llm.cleanup("  你好 世界 "), "你好 世界。");
        assert_eq!(llm.cleanup("已经说完。"), "已经说完。");
        assert_eq!(llm.cleanup("   "), "");
        assert_eq!(llm.summarize(&["a".into(), "b".into()]), "要点：a；b（共 2 段）");
    }
}
