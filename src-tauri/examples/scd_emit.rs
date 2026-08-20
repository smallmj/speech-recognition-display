//! SCD 回归验证：读 sidecar 的 NDJSON final（含 embedding + speech_duration），
//! 输出真实 SCD 归属（含追溯修正）。
//!
//! 用法（stdin 协议与 `sherpa_streaming.py` 对齐）：
//! `cargo run --manifest-path src-tauri/Cargo.toml --example scd_emit`
//! 每行一条 final：`{"type":"final","text":"...","embedding":[...],"speech_duration":1.2}`。
//! 输出每条的 `index / speaker_id / is_new_speaker / corrections / text`，
//! 供脚本断言。

use std::io::{self, BufRead};

use engine::Scd;

fn main() {
    let mut scd = Scd::default();
    let stdin = io::stdin();
    let mut index = 0u64;
    for raw in stdin.lock().lines() {
        let Ok(raw) = raw else { break };
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if obj.get("type").and_then(|v| v.as_str()) != Some("final") {
            continue;
        }
        let Some(text) = obj.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        let embedding = obj
            .get("embedding")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .map(|x| x.as_f64().map(|v| v as f32))
                    .collect::<Option<Vec<f32>>>()
            })
            .unwrap_or_default();
        let speech_seconds = obj
            .get("speech_duration")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(0.0);
        let decision = scd.process_utterance(index, text, &embedding, speech_seconds, None);
        let corrections: Vec<serde_json::Value> = decision
            .corrections
            .iter()
            .map(|c| {
                serde_json::json!({
                    "target": c.utt_token,
                    "new_speaker_id": c.new_speaker_id,
                })
            })
            .collect();
        let row = serde_json::json!({
            "index": index,
            "speaker_id": decision.speaker_id,
            "is_new_speaker": decision.is_new_speaker,
            "corrections": corrections,
            "text": text,
        });
        println!("{row}");
        index += 1;
    }
}
