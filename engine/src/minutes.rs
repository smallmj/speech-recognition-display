//! 会议纪要编排：停止识别后把整段内容分批交给 LLM，再汇总为结构化纪要。
//!
//! 对齐规格（docs/spec/0001）用户故事 26/27 与「纪要编排」实现决策：
//!
//! - **用户故事 26**：停止识别后一键生成结构化会议纪要（要点/行动项/待办），会后回顾。
//! - **用户故事 27**：内容太多时系统自动分批交给 LLM 再汇总，长对话也能生成纪要。
//! - **纪要编排**：停止后把片段按时间窗分批（每批约 500 字 + 滚动上文，上限
//!   2000 token）交给 `LlmPort`，再汇总为结构化纪要（要点/行动项/待办）。
//! - **ADR-0003**：单次输入 ≤500 字、滚动窗口 ≤2000 token 防上下文溢出。
//!
//! 本模块是**纯函数、确定性**的分批编排（唯一测试缝）：
//!
//! - [chunk_for_summarize]：把有序片段切成时间窗批次。每批正文 ≤
//!   `max_chars_per_batch`（默认 [BATCH_MAX_CHARS] = 500 字）；第 2+ 批开头
//!   携带上一批末尾至多 [ROLLING_CONTEXT_CHARS]（100）字作**滚动上文**，
//!   防丢失跨批语义（如「这个方案」中的「这个」指代上一批内容）。滚动上文
//!   计入每批的 token 预算（[MAX_TOKENS] = 2000，按中文约 1 字符 ≈ 1 token
//!   估算；500 + 100 = 600 字远低于预算，实际调用由壳层控制）。
//! - [summarize_minutes]：同步模拟路径的完整编排（分批 → 逐批交 `LlmPort`
//!   生成部分纪要 → 把各部分纪要汇总为最终纪要），用 [crate::cleanup::MockLlmPort]
//!   可确定性断言「分批边界与汇总顺序」。真实链路（Tauri 壳）用同一算法，
//!   只是把 `LlmPort` 换成 OpenAI 兼容客户端的异步请求。
//!
//! 取舍说明：
//! - **宁可超预算也不截断**：单条片段超过批上限时单独成批、整条保留，绝不
//!   截断丢字（纪要的完整性优先于 token 预算，规格未要求截断）。
//! - **分批只做文本切分**：不跨请求合并/去重，每批内容独立送 LLM，汇总阶段
//!   由 LLM（或失败回退时的拼接）合并，顺序与时间窗一致。

use crate::types::{LlmPort, Segment};

/// 每批正文上限（字符）。对齐 ADR-0003「单次输入 ≤500 字」。
pub const BATCH_MAX_CHARS: usize = 500;

/// 滚动上文长度（字符）：第 2+ 批携带上一批末尾至多这么多字作上下文，
/// 防丢失跨批语义。约 1-2 条片段的量级。
pub const ROLLING_CONTEXT_CHARS: usize = 100;

/// 每批（正文 + 滚动上文）的 token 预算。按「中文约 1 字符 ≈ 1 token」估算：
/// 500 + 100 = 600 字 ≈ 600 token，远低于 2000；实际调用是否放宽分批上限
/// 由壳层根据本常量决定（当前默认分批参数下不会触顶）。
pub const MAX_TOKENS: usize = 2000;

/// 滚动上文文本前缀标记：让 LLM 知道这一段是「上一批的上下文」，不是本批正文。
pub const ROLLING_CONTEXT_MARKER: &str = "【上文】";

/// 把有序片段切成时间窗批次，供 LLM 逐批生成纪要。
///
/// 规则（对齐规格「纪要编排」与 ADR-0003）：
///
/// - **输入**：按 id/时间有序的全部片段，文本优先取 `cleaned`（整理版），
///   无 `cleaned`（或为空）时回退 `raw`（原文）；空白片段跳过。
/// - **分批**：贪心累计每批正文字符数，超过 `max_chars_per_batch` 开新批；
///   单条超长片段（本身超过批上限）单独成批、**不截断**。
/// - **滚动上文**：第 2+ 批的第一个元素是滚动上文文本（以
///   [ROLLING_CONTEXT_MARKER] 开头），取自上一批**正文**（不含其自身的
///   滚动上文）末尾至多 [ROLLING_CONTEXT_CHARS] 字。
///
/// 返回值：批次列表，每批是一个文本片段列表（`Vec<String>`）；第 2+ 批的
/// `[0]` 为滚动上文。空输入（无片段 / 全部空白）返回空列表。
pub fn chunk_for_summarize(segments: &[Segment], max_chars_per_batch: usize) -> Vec<Vec<String>> {
    // 参数兜底：批上限至少 1 字（0 会让任何片段都「超限」，行为退化）。
    let max_chars = max_chars_per_batch.max(1);

    // 1. 提取有序文本：优先整理版，回退原文；空白片段跳过。
    let texts: Vec<String> = segments
        .iter()
        .filter_map(|s| {
            let text = match s.cleaned.as_deref() {
                Some(cleaned) if !cleaned.trim().is_empty() => cleaned.trim(),
                _ => s.raw.trim(),
            };
            (!text.is_empty()).then(|| text.to_string())
        })
        .collect();
    if texts.is_empty() {
        return Vec::new();
    }

    // 2. 贪心分批：每批正文累计 ≤ max_chars；单条超长片段单独成批不截断。
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut batch_chars: Vec<usize> = Vec::new();
    // 上一批是否因「单条超长」被强制关闭：下一段必须开新批。
    let mut closed_overlong = false;
    for text in texts {
        let chars = text.chars().count();
        let fits_current = !closed_overlong
            && batches
                .last()
                .is_some_and(|batch| !batch.is_empty())
            && batch_chars.last().copied().unwrap_or(0) + chars <= max_chars;
        if !fits_current {
            batches.push(Vec::new());
            batch_chars.push(0);
        }
        batches.last_mut().expect("batch just pushed").push(text);
        let last = batch_chars.last_mut().expect("chars just pushed");
        *last += chars;
        closed_overlong = *last > max_chars;
    }

    // 3. 第 2+ 批开头插入滚动上文（上一批正文末尾至多 ROLLING_CONTEXT_CHARS 字）。
    let mut result: Vec<Vec<String>> = Vec::with_capacity(batches.len());
    for (i, mut batch) in batches.into_iter().enumerate() {
        if i > 0 {
            // 只取上一批的正文（跳过其自身的滚动上文元素），避免上文层层嵌套。
            let prev_content: String = result[i - 1]
                .iter()
                .filter(|piece| !piece.starts_with(ROLLING_CONTEXT_MARKER))
                .map(|piece| piece.as_str())
                .collect();
            let tail: String = prev_content
                .chars()
                .rev()
                .take(ROLLING_CONTEXT_CHARS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            batch.insert(0, format!("{ROLLING_CONTEXT_MARKER} {tail}"));
        }
        result.push(batch);
    }
    result
}

/// 纪要编排（同步模拟路径）：分批 → 逐批交 `LlmPort` 生成部分纪要 → 汇总为最终纪要。
///
/// 算法（与 Tauri 壳的真实异步链路同构，只是把 `LlmPort` 换成真实客户端）：
///
/// 1. [chunk_for_summarize] 分批（每批 ≤500 字 + 滚动上文，防上下文溢出）；
/// 2. **逐批**调用 `llm.summarize(batch)` 得到该时间窗的部分纪要（顺序与
///    时间窗一致——「汇总顺序」即各批部分纪要的拼接顺序）；
/// 3. 把全部部分纪要交给 `llm.summarize(&partials)` 汇总为最终结构化纪要
///    （要点/行动项/待办）。
///
/// 空输入直接返回 `llm.summarize(&[])`（Mock 输出「（无内容）」）。
pub fn summarize_minutes(llm: &dyn LlmPort, segments: &[Segment], max_chars_per_batch: usize) -> String {
    let batches = chunk_for_summarize(segments, max_chars_per_batch);
    if batches.is_empty() {
        return llm.summarize(&[]);
    }
    // 逐批：每批一个部分纪要（批次顺序 = 时间窗顺序）。
    let partials: Vec<String> = batches.iter().map(|batch| llm.summarize(batch)).collect();
    // 汇总：≥2 批时把各批部分纪要交给 LLM 合并为最终结构化纪要；
    // 只有 1 批时该批部分纪要即最终纪要（无需再调一次 LLM）。
    if partials.len() > 1 {
        llm.summarize(&partials)
    } else {
        partials.into_iter().next().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::MockLlmPort;
    use crate::types::{Segment, SegmentStatus};

    /// 构造一条整理完成的片段（cleaned 与 raw 同文）。
    fn seg(id: u64, text: &str) -> Segment {
        seg_raw_cleaned(id, text, Some(text))
    }

    /// 构造一条只有原文（未整理）的片段。
    fn seg_raw(id: u64, text: &str) -> Segment {
        seg_raw_cleaned(id, text, None)
    }

    /// 构造一条原文与整理版可区分的片段（验证「优先用 cleaned」）。
    fn seg_raw_cleaned(id: u64, raw: &str, cleaned: Option<&str>) -> Segment {
        Segment {
            id,
            speaker_id: 1,
            raw: raw.to_string(),
            status: SegmentStatus::Cleaned,
            cleaned: cleaned.map(|c| c.to_string()),
            edit_id: Some(id),
            ts: id * 1000,
            retries: 0,
        }
    }

    /// 批内正文（非滚动上文）的字符数——「每批 ≤ 上限」的统计口径：
    /// 正文计入 500 字上限，滚动上文不计入正文上限（计入 token 预算）。
    fn batch_content_chars(batch: &[String]) -> usize {
        batch
            .iter()
            .filter(|piece| !piece.starts_with(ROLLING_CONTEXT_MARKER))
            .map(|piece| piece.chars().count())
            .sum()
    }

    /// 批内的滚动上文文本（第 2+ 批的 [0] 元素）。
    fn rolling_context(batch: &[String]) -> Option<&str> {
        batch
            .first()
            .filter(|p| p.starts_with(ROLLING_CONTEXT_MARKER))
            .map(|p| p.as_str())
    }

    // ---- 验收：少量文本 → 1 批 ----

    #[test]
    fn short_text_yields_single_batch_without_rolling_context() {
        let segments = vec![seg(0, "我们讨论一下新项目排期。"), seg(1, "好的，周五前出第一版。")];
        let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
        assert_eq!(batches.len(), 1, "少量文本应只有 1 批");
        assert_eq!(batches[0].len(), 2, "两条片段都在同一批");
        assert!(rolling_context(&batches[0]).is_none(), "首批不应有滚动上文");
        assert!(batch_content_chars(&batches[0]) <= BATCH_MAX_CHARS);
    }

    // ---- 验收：超过批上限 → 多批，每批正文 ≤ 上限 ----

    #[test]
    fn over_limit_splits_into_batches_each_within_limit() {
        // 3 段各 60 字，上限 100 → 3 批（60 / 60 / 60）
        let segments: Vec<Segment> = (0..3).map(|i| seg(i, &"甲".repeat(60))).collect();
        let batches = chunk_for_summarize(&segments, 100);
        assert_eq!(batches.len(), 3, "60+60 超 100 → 3 批");
        for batch in &batches {
            assert!(batch_content_chars(batch) <= 100, "每批正文 ≤100 字");
        }
        // 每批正文各 60 字，全部内容不丢失
        let total: usize = batches.iter().map(|b| batch_content_chars(b)).sum();
        assert_eq!(total, 180, "分批不丢字");
    }

    #[test]
    fn default_limit_500_splits_300_char_segments() {
        // 2 段各 300 字，默认上限 500：300 ≤ 500 同批放得下，但 300+300=600>500
        // → 第 2 段开新批。共 2 批，每批正文 ≤500。
        let segments: Vec<Segment> = (0..2).map(|i| seg(i, &"字".repeat(300))).collect();
        let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
        assert_eq!(batches.len(), 2, "300+300 超 500 → 2 批");
        assert!(batch_content_chars(&batches[0]) <= BATCH_MAX_CHARS);
        assert!(batch_content_chars(&batches[1]) <= BATCH_MAX_CHARS);
        // 3 段各 300 字 → 3 批（每段都无法与相邻段合并）
        let segments: Vec<Segment> = (0..3).map(|i| seg(i, &"字".repeat(300))).collect();
        let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
        assert_eq!(batches.len(), 3, "3 段 300 字互不合并 → 3 批");
    }

    // ---- 验收：滚动上文出现在第 2+ 批开头 ----

    #[test]
    fn rolling_context_appears_at_start_of_second_and_later_batches() {
        let segments = vec![seg(0, &"甲".repeat(80)), seg(1, &"乙".repeat(80)), seg(2, &"丙".repeat(80))];
        let batches = chunk_for_summarize(&segments, 100);

        // 3 批；首批无滚动上文
        assert_eq!(batches.len(), 3);
        assert!(rolling_context(&batches[0]).is_none());

        // 第 2 批：开头是上一批（甲×80）末尾 100 字内的滚动上文
        let rc2 = rolling_context(&batches[1]).expect("第 2 批应有滚动上文");
        assert!(rc2.starts_with(ROLLING_CONTEXT_MARKER), "滚动上文带标记: {rc2}");
        assert!(rc2.contains(&"甲".repeat(80)), "第 2 批滚动上文 = 上一批正文末尾: {rc2}");
        assert_eq!(batch_content_chars(&batches[1]), 80, "第 2 批正文仍是乙×80");

        // 第 3 批：滚动上文取自第 2 批正文（乙×80），不嵌套第 2 批自身的滚动上文
        let rc3 = rolling_context(&batches[2]).expect("第 3 批应有滚动上文");
        assert!(rc3.contains(&"乙".repeat(80)), "第 3 批滚动上文 = 第 2 批正文末尾: {rc3}");
        assert!(!rc3.contains("甲"), "滚动上文不层层嵌套上一批的滚动上文");
    }

    #[test]
    fn rolling_context_is_capped_at_constant_chars() {
        // 上一批正文 300 字：前 200 字「头」+ 后 100 字「尾」（可区分首尾），
        // 滚动上文只取末尾 100 字 → 应全是「尾」。
        let segments = vec![seg(0, &format!("{}{}", "头".repeat(200), "尾".repeat(100))), seg(1, "二")];
        let batches = chunk_for_summarize(&segments, 300);
        assert_eq!(batches.len(), 2);
        let rc = rolling_context(&batches[1]).expect("第 2 批应有滚动上文");
        // 去掉标记与分隔空格后，滚动上文恰为上一批末尾 100 字
        let tail = rc.trim_start_matches(ROLLING_CONTEXT_MARKER).trim_start();
        assert_eq!(tail.chars().count(), ROLLING_CONTEXT_CHARS, "滚动上文长度封顶 100 字");
        assert_eq!(tail, &"尾".repeat(100), "取的是上一批末尾（尾×100），不是开头（头）");
    }

    // ---- 验收：空输入 → 空批 ----

    #[test]
    fn empty_input_yields_no_batches() {
        assert!(chunk_for_summarize(&[], BATCH_MAX_CHARS).is_empty(), "无片段 → 空批");
    }

    #[test]
    fn blank_segments_are_skipped() {
        let segments = vec![seg_raw(0, "   "), seg_raw(1, "\t\n"), seg_raw(2, "只有这句有内容")];
        let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
        assert_eq!(batches.len(), 1, "空白片段跳过");
        assert_eq!(batches[0].len(), 1);
        assert_eq!(batches[0][0], "只有这句有内容");
    }

    // ---- 验收：单条超长片段单独成批、不截断 ----

    #[test]
    fn overlong_single_segment_gets_own_batch_not_truncated() {
        let long = "长".repeat(250);
        let segments = vec![seg(0, &long), seg(1, "短句")];
        let batches = chunk_for_summarize(&segments, 100);

        // 超长片段单独成批、整条保留（宁可超预算也不截断丢字）
        assert_eq!(batches.len(), 2, "超长片段单独成批，后续片段开新批");
        assert_eq!(batches[0].len(), 1, "超长批内只有它自己");
        assert_eq!(batches[0][0], long, "超长片段不截断");
        assert!(batch_content_chars(&batches[0]) > 100, "超长批正文超上限（有意为之）");

        // 后续片段在新批（[0] 为该批的滚动上文，正文从 [1] 开始），不受超长批影响
        assert_eq!(batch_content_chars(&batches[1]), 2, "第 2 批正文只有「短句」2 字");
        assert_eq!(batches[1][1], "短句");
    }

    // ---- 验收：文本来源优先整理版，回退原文 ----

    #[test]
    fn cleaned_text_preferred_over_raw() {
        let segments = vec![
            seg_raw_cleaned(0, "原始口语文本", Some("整理后的书面语")),
            seg_raw(1, "这条只有原文"),
            seg_raw_cleaned(2, "空的整理版回退原文", Some("   ")),
        ];
        let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0][0], "整理后的书面语", "优先用 cleaned");
        assert_eq!(batches[0][1], "这条只有原文", "无 cleaned 用 raw");
        assert_eq!(batches[0][2], "空的整理版回退原文", "cleaned 为空回退 raw");
    }

    // ---- 验收：分批边界 + 汇总顺序（summarize_minutes 端到端）----

    #[test]
    fn summarize_minutes_aggregates_partials_in_window_order() {
        // 4 段各 60 字，上限 100 → 每段单独成批，共 4 批。
        // MockLlmPort 每批输出「要点：…（共 N 段）」；汇总阶段再把 4 份部分
        // 纪要按时间窗顺序合并为「要点：…（共 4 段）」。
        let segments: Vec<Segment> = (0..4).map(|i| seg(i, &"甲".repeat(60))).collect();
        let minutes = summarize_minutes(&MockLlmPort, &segments, 100);

        assert!(minutes.starts_with("要点："), "汇总输出为要点开头: {minutes}");
        assert!(minutes.ends_with("（共 4 段）"), "汇总收到 4 段部分纪要（顺序即收集顺序）: {minutes}");
        // 批 1 只有片段 0 一个元素 → 其部分纪要形状为「（共 1 段）」，且在汇总开头
        assert!(minutes.contains(&format!("{}（共 1 段）", "甲".repeat(60))), "批 1 部分纪要按序在前: {minutes}");
        // 批 2+ 含滚动上文元素 → 其部分纪要形状为「（共 2 段）」
        assert!(minutes.contains("（共 2 段）"), "批 2+ 部分纪要含滚动上文元素: {minutes}");
    }

    #[test]
    fn summarize_minutes_empty_session_returns_placeholder() {
        assert_eq!(summarize_minutes(&MockLlmPort, &[], BATCH_MAX_CHARS), "（无内容）");
        let blank = vec![seg_raw(0, "  ")];
        assert_eq!(summarize_minutes(&MockLlmPort, &blank, BATCH_MAX_CHARS), "（无内容）");
    }

    #[test]
    fn summarize_minutes_single_batch_skips_final_aggregation() {
        let segments = vec![seg(0, "一个时间窗的内容")];
        let minutes = summarize_minutes(&MockLlmPort, &segments, BATCH_MAX_CHARS);
        assert_eq!(minutes, "要点：一个时间窗的内容（共 1 段）", "单批：该批部分纪要即最终纪要");
    }
}
