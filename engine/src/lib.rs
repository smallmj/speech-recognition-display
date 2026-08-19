//! `engine` — 听障实时字幕展示系统的独立 Rust 核心库。
//!
//! 承载全部业务逻辑（片段管理、说话人检测、整理管线、纪要编排），
//! 不依赖 Tauri。外部能力（音频采集、ASR、LLM、Embedding）通过
//! [AsrPort]、[EmbeddingPort]、[LlmPort] 三个端口注入，业务逻辑对外
//! 暴露统一的事件流 [EngineEvent]。
//!
//! 这是整个应用**唯一的测试缝**：测试中喂入合成输入（文本片段、
//! 预计算 embedding、模拟 LLM 返回），断言输出的事件流，不触及
//! 真实音频/网络/WebView。
//!
//! T2 新增 [crate::pipeline]：冒烟管线（[crate::pipeline::MockAsrPort] +
//! [crate::pipeline::Engine]），把合成转写转化为带说话人/颜色的片段事件流。
//!
//! T8 新增 [crate::cleanup]：LLM 整理管线（防抖 + 固定节奏 + 单在途 + editId
//! 校验 + 失败回退），并把 [crate::cleanup::CleanupPipeline] 等导出，供 Tauri
//! 壳层经「`tick` 派发 pending → 调真实 LLM → `apply_cleanup_result` /
//! `fail_pending` 回填」的异步路径驱动（T9 真实 LLM 接入）。
//! T5 新增 [crate::scd]：说话人切换检测 —— speaker embedding 余弦匹配 + 自动编号 +
//! 音色选性别（T5 决策：降级 Unknown，见 [T5 实现总结](docs/T5-implementation-summary.md)）。
//! 颜色稳定复用 [crate::pipeline::speaker_color]。

mod cleanup;
mod pipeline;
mod scd;
mod types;

pub use cleanup::{CleanupPipeline, CleanupScheduler, MockLlmPort, PendingCleanup, SegmentStore};
pub use pipeline::{speaker_color, Engine, MockAsrPort, SPEAKER_PALETTE};
pub use scd::{cosine_similarity, Scd, ScdConfig, SpeakerDecision, SpeakerTemplate};
pub use types::{
    AsrPort, EmbeddingPort, EngineEvent, Gender, LlmPort, Segment, SegmentStatus, Utterance,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// T1 骨架测试：确认核心类型可构造、可序列化（事件桥的 JSON 契约）。
    #[test]
    fn segment_roundtrips_through_json() {
        let seg = Segment {
            id: 7,
            speaker_id: 2,
            raw: "你好，我想确认一下时间。".into(),
            status: SegmentStatus::Cleaned,
            cleaned: Some("你好，我想确认一下时间。".into()),
            edit_id: Some(3),
            ts: 1_700_000_000_000,
            retries: 1,
        };

        // 构造 + 字段可读
        assert_eq!(seg.id, 7);
        assert_eq!(seg.speaker_id, 2);
        assert_eq!(seg.cleaned.as_deref(), Some("你好，我想确认一下时间。"));

        // 序列化 → 反序列化保持相等（前端桥接的 JSON 契约）
        let json = serde_json::to_string(&seg).expect("serialize segment");
        let back: Segment = serde_json::from_str(&json).expect("deserialize segment");
        assert_eq!(seg, back);

        // 事件也可序列化；契约是 type 标签 + camelCase 字段
        let evt = EngineEvent::SegmentCleaned {
            segment_id: 7,
            cleaned: "你好，我想确认一下时间。".into(),
            edit_id: 3,
        };
        let evt_json = serde_json::to_string(&evt).expect("serialize event");
        assert!(
            evt_json.contains("\"type\":\"segmentCleaned\""),
            "got: {evt_json}"
        );
        assert!(evt_json.contains("\"segmentId\""), "got: {evt_json}");
    }

    /// T9 新增事件：LLM 流式增量的序列化契约（前端据此逐字填充整理版）。
    /// 带 `editId`：同一流式请求内不变，渲染层据此拒绝乱序残余增量。
    #[test]
    fn segment_cleaning_event_serializes_with_type_tag_and_camel_case() {
        let evt = EngineEvent::SegmentCleaning {
            segment_id: 3,
            edit_id: 5,
            partial: "你好，我想".into(),
        };
        let json = serde_json::to_string(&evt).expect("serialize event");
        assert!(json.contains("\"type\":\"segmentCleaning\""), "got: {json}");
        assert!(json.contains("\"segmentId\":3"), "got: {json}");
        assert!(json.contains("\"editId\":5"), "got: {json}");
        assert!(json.contains("\"partial\":\"你好，我想\""), "got: {json}");

        // 反序列化往返保持相等（前端桥接的 JSON 契约）
        let back: EngineEvent = serde_json::from_str(&json).expect("deserialize event");
        assert_eq!(evt, back);
    }

    /// 同一说话人批次整理结果的序列化契约：前端需要原子更新多个片段。
    #[test]
    fn segments_cleaned_event_serializes_with_type_tag_and_camel_case() {
        let evt = EngineEvent::SegmentsCleaned {
            segment_ids: vec![1, 3],
            cleaned: "两句汇整后的整理版。".into(),
            edit_id: 7,
        };
        let json = serde_json::to_string(&evt).expect("serialize event");
        assert!(json.contains("\"type\":\"segmentsCleaned\""), "got: {json}");
        assert!(json.contains("\"segmentIds\":[1,3]"), "got: {json}");
        assert!(json.contains("\"editId\":7"), "got: {json}");

        let back: EngineEvent = serde_json::from_str(&json).expect("deserialize event");
        assert_eq!(evt, back);
    }

    /// T9 审查修复：`LlmPort::cleanup_streaming` 默认实现走同步 `cleanup`
    /// 并回调一次全量（mock / 同步端口无需覆盖）；trait 对象安全（可
    /// `Box<dyn LlmPort>` 调用），engine 测试缝可覆盖流式路径。
    #[test]
    fn cleanup_streaming_default_invokes_callback_once_with_full_result() {
        struct MockLlm;
        impl LlmPort for MockLlm {
            fn cleanup(&self, text: &str) -> String {
                format!("【{text}】")
            }
            fn summarize(&self, _chunks: &[String]) -> String {
                "纪要".to_string()
            }
        }

        let mut received: Vec<String> = Vec::new();
        let result =
            MockLlm.cleanup_streaming("口语原文", &mut |p| received.push(p.to_string()));
        assert_eq!(result.as_deref(), Ok("【口语原文】"));
        assert_eq!(received, vec!["【口语原文】"], "默认实现应回调一次全量结果");
    }

    /// T1 骨架测试：三个端口 trait 可被 mock 实现（测试缝成立）。
    #[test]
    fn ports_are_implementable() {
        struct MockAsr;
        impl AsrPort for MockAsr {
            fn start(&mut self) {}
            fn stop(&mut self) {}
            fn next_utterance(&mut self) -> Option<Utterance> {
                None
            }
        }

        struct MockEmbedding;
        impl EmbeddingPort for MockEmbedding {
            fn compute_embedding(&self, audio: &[f32]) -> Vec<f32> {
                vec![audio.iter().copied().sum()]
            }
        }

        struct MockLlm;
        impl LlmPort for MockLlm {
            fn cleanup(&self, text: &str) -> String {
                text.to_string()
            }
            fn summarize(&self, _chunks: &[String]) -> String {
                "纪要".to_string()
            }
        }

        let mut asr = MockAsr;
        asr.start();
        asr.stop();
        assert!(asr.next_utterance().is_none());

        let emb = MockEmbedding.compute_embedding(&[1.0, 2.0, 3.0]);
        assert_eq!(emb, vec![6.0]);

        let llm = MockLlm;
        assert_eq!(llm.cleanup(" 原文 "), " 原文 ");
        assert_eq!(llm.summarize(&["a".into()]), "纪要");
    }
}
