//! engine 数据契约：事件类型、片段模型与三个可注入端口。
//!
//! 本模块不依赖 Tauri —— 是整个应用唯一的测试缝。

use serde::{Deserialize, Serialize};

/// 片段生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// 一条发言的不可变原文片段。
///
/// `raw` 只写一次；整理结果写入 `cleaned`，通过单调递增的 `edit_id`
/// 让渲染层只接受更大的值，避免乱序/旧结果覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// engine 对外暴露的统一事件流。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EngineEvent {
    SessionStarted,
    SessionStopped,
    SegmentAppended { segment: Segment },
    SpeakerAssigned { segment_id: u64, speaker_id: u32, is_new_speaker: bool },
    SegmentCleaned { segment_id: u64, cleaned: String, edit_id: u64 },
    CleanupFailed { segment_id: u64 },
    MinutesReady { minutes: String },
}

/// ASR 端口：外部语音识别能力（本地 sherpa-onnx / 云端）接入点。
pub trait AsrPort: Send {
    fn start(&mut self);
    fn stop(&mut self);
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
