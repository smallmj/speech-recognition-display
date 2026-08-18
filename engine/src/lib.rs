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

mod types;

pub use types::{
    AsrPort, EmbeddingPort, EngineEvent, LlmPort, Segment, SegmentStatus,
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

        // 事件也可序列化
        let evt = EngineEvent::SegmentCleaned {
            segment_id: 7,
            cleaned: "你好，我想确认一下时间。".into(),
            edit_id: 3,
        };
        let evt_json = serde_json::to_string(&evt).expect("serialize event");
        assert!(evt_json.contains("SegmentCleaned"));
    }

    /// T1 骨架测试：三个端口 trait 可被 mock 实现（测试缝成立）。
    #[test]
    fn ports_are_implementable() {
        struct MockAsr;
        impl AsrPort for MockAsr {
            fn start(&mut self) {}
            fn stop(&mut self) {}
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

        let emb = MockEmbedding.compute_embedding(&[1.0, 2.0, 3.0]);
        assert_eq!(emb, vec![6.0]);

        let llm = MockLlm;
        assert_eq!(llm.cleanup(" 原文 "), " 原文 ");
        assert_eq!(llm.summarize(&["a".into()]), "纪要");
    }
}
