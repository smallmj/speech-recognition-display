//! engine 数据契约：事件类型、片段模型与三个可注入端口。
//!
//! 本模块不依赖 Tauri —— 是整个应用唯一的测试缝。
//!
//! 序列化契约（与规格文档数据契约对齐）：
//! - 事件用 `type` 标签区分（如 `{"type":"segmentAppended", ...}`）
//! - 字段一律 camelCase（`speakerId` / `isNewSpeaker` / `editId` …）
//! - `SegmentStatus` / `Gender` 用小写字符串（`"active"` / `"male"` …）

use serde::{Deserialize, Serialize};

/// 片段生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentStatus {
    /// 正在追加中，尚未送 LLM 整理（interim）。
    Active,
    /// 已冻结，允许送 LLM 整理。
    Frozen,
    /// 已整理完成。
    Cleaned,
    /// 整理失败，回退展示原文。
    Failed,
}

/// 说话人性别（用于前端头像选择，engine 端根据音色特征标记）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gender {
    Male,
    Female,
    Unknown,
}

/// 一条发言的不可变原文片段。
///
/// `raw` 只写一次；整理结果写入 `cleaned`，通过单调递增的 `edit_id`
/// 让渲染层只接受更大的值，避免乱序/旧结果覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// 全局单调递增 id，排序与去重键。
    pub id: u64,
    /// 所属说话人 id。
    pub speaker_id: u32,
    /// 不可变原文，只写一次。
    pub raw: String,
    pub status: SegmentStatus,
    /// 整理结果（LLM 整理版）。
    pub cleaned: Option<String>,
    /// 写入时的单调 id，渲染层只接受更大的。
    pub edit_id: Option<u64>,
    /// 最后追加时间（防抖基准，毫秒）。
    pub ts: u64,
    /// LLM 失败重试次数。
    pub retries: u32,
}

/// ASR 端产出的一条转写单元（一条完整发言）。
///
/// 真实实现（T4+）由 ASR/SCD 上游产出；T2 冒烟管线由 [crate::pipeline::MockAsrPort]
/// 按预设脚本逐条给出。`speaker_id` 在冒烟阶段由脚本指定，真实阶段由 SCD 决定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Utterance {
    /// 说话人 id（SCD 归属结果）。
    pub speaker_id: u32,
    /// 说话人性别（用于头像选择）。
    pub gender: Gender,
    /// 转写文本。
    pub text: String,
    /// 时间戳（毫秒）。
    pub ts: u64,
}

/// engine 对外暴露的统一事件流。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    SessionStarted,
    SessionStopped,
    #[serde(rename_all = "camelCase")]
    SegmentAppended {
        segment: Segment,
    },
    /// 流式识别的实时中间结果（边说边出）。final 定稿后由
    /// [EngineEvent::SegmentAppended] 携带完整片段。
    #[serde(rename_all = "camelCase")]
    PartialResult {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    SpeakerAssigned {
        segment_id: u64,
        speaker_id: u32,
        is_new_speaker: bool,
        /// 该说话人分配到的颜色（hex，如 `#4f8cff`），同一说话人恒定。
        color: String,
        gender: Gender,
    },
    /// LLM 整理结果的流式增量（SSE 每个 delta），前端据此逐字填充整理版。
    /// 由壳层在 SSE 过程中 emit（engine 只定义类型与序列化契约，不产生该事件）。
    #[serde(rename_all = "camelCase")]
    SegmentCleaning {
        segment_id: u64,
        /// 截至当前 delta 的累积整理文本（部分结果）。
        partial: String,
    },
    #[serde(rename_all = "camelCase")]
    SegmentCleaned {
        segment_id: u64,
        cleaned: String,
        edit_id: u64,
    },
    #[serde(rename_all = "camelCase")]
    CleanupFailed {
        segment_id: u64,
    },
    #[serde(rename_all = "camelCase")]
    MinutesReady {
        minutes: String,
    },
}

/// ASR 端口：外部语音识别能力（本地 sherpa-onnx / 云端）接入点。
///
/// 采用"拉取"模型：宿主（engine / 驱动线程）周期调用 [AsrPort::next_utterance]
/// 取走一条转写结果。T4 真实 ASR 可在内部缓冲回调流，对外仍实现本 trait。
pub trait AsrPort: Send {
    fn start(&mut self);
    fn stop(&mut self);
    /// 拉取下一条转写结果；当前无新结果时返回 `None`。
    fn next_utterance(&mut self) -> Option<Utterance>;
}

/// Embedding 端口：说话人声纹向量计算（用于 SCD 余弦匹配）。
pub trait EmbeddingPort: Send {
    fn compute_embedding(&self, audio: &[f32]) -> Vec<f32>;
}

/// LLM 端口：整理（去口语化/纠错/补标点）与纪要摘要。
pub trait LlmPort: Send {
    fn cleanup(&self, text: &str) -> String;
    fn summarize(&self, chunks: &[String]) -> String;
}
