//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! 接线策略（T4 + T9 整合）：
//! - **真实 ASR 优先（T4）**：尝试启动 sherpa-onnx sidecar + 麦克风
//!   （[crate::asr::SherpaAsr]）；成功则真实识别结果进入整理链路，
//!   partial 实时 publish 给前端状态行；失败回退
//!   [engine::MockAsrPort]（合成转写），保证演示模式始终可用。
//!   模式经 `engine://status` 事件告知前端（`{"mode":"sherpa"|"mock"}`）。
//! - **final 统一进整理管线（T9）**：无论真实还是合成，final 转写都直接喂入
//!   [engine::CleanupPipeline]（防抖 + 固定节奏 + 单在途），驱动线程周期
//!   `tick(now)` 冻结并派发 `pending`，拿到 pending 后调真实 OpenAI 兼容
//!   LLM（[crate::llm::OpenAiLlmClient]，SSE 流式），每个 delta 以
//!   `SegmentCleaning` 增量 emit（前端逐字填充整理版），完成后经
//!   `apply_cleanup_result` 回填并 emit `SegmentCleaned`，失败经
//!   `fail_pending` emit `CleanupFailed`（前端回退展示原文）。
//! - **说话人**：T4 阶段无 SCD，真实 ASR 的 final 全部归说话人 1、性别
//!   Unknown（T5 接入 SCD 后替换）；SpeakerAssigned（颜色/性别）在片段
//!   前 emit，与 engine 事件顺序对齐。
//!
//! 时基：engine 用逻辑时钟（[std::time::Duration]，自管道创建起算），驱动
//! 线程每 200ms 一个节拍推进 `now`（统一节拍：partial 刷新及时，整理
//! 节奏不变——真实 ASR 的防抖/固定节奏由 CleanupPipeline 的调度器决定）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::{AsrPort, CleanupPipeline, EngineEvent, MockAsrPort, MockLlmPort};
use tauri::{AppHandle, Emitter};

use crate::asr::SherpaAsr;
use crate::llm::{self, OpenAiLlmClient};

/// engine 事件流的事件名（与前端 `src/engineEvents.ts` 的 `ENGINE_EVENT` 保持一致）。
pub const ENGINE_EVENT: &str = "engine://event";
/// 壳层运行状态事件名（ASR 模式等运营信息，区别于 engine 业务事件流）。
pub const STATUS_EVENT: &str = "engine://status";

/// 驱动循环节拍：逻辑时钟步进间隔（ms）。统一 200ms：partial 刷新及时，
/// 整理管线的防抖（2s）/固定节奏（5s）不受节拍影响。
const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// ASR 数据源模式（决定追加节奏与 partial 是否可用）。
enum AsrMode {
    /// 真实 ASR（sherpa-onnx + 麦克风）：partial 实时 publish，final 全部
    /// 无条件追加（不丢字）。
    Sherpa {
        /// 实时 partial 缓冲（sidecar stdout 线程写入，本循环轮询）。
        partials: Arc<Mutex<VecDeque<String>>>,
    },
    /// 回退演示模式（合成转写）：无 partial，1 进 1 出节奏（便于观察
    /// 「追加 → 防抖 → 流式整理 → 完成」的完整过程）。
    Mock,
}

/// 启动后台线程驱动「真实/合成 ASR → 整理管线 → 真实 LLM → 事件流」垂直链路。
///
/// 节奏：每节拍推进逻辑时钟 → （真实模式）拉取并 publish 最新 partial、
/// 排空 final 队列逐条追加；（演示模式）无在途请求且上一段已落库时追加
/// 下一条 → `tick(now)` → 有 pending 则调真实 LLM（SSE 增量 emit）→
/// 回填/失败。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        // 首次延迟稍长，给前端 Vite 加载 + 注册 listen 留出时间。
        std::thread::sleep(Duration::from_millis(1200));

        // 尝试真实 ASR；失败回退 mock（演示模式）。partial 句柄在 real 移入
        // Box<dyn AsrPort> 前取出，供主循环轮询。
        let (mut asr, mode): (Box<dyn AsrPort>, AsrMode) = match SherpaAsr::spawn() {
            Ok(real) => {
                println!("[engine] 真实 ASR 已启动（sherpa-onnx + 麦克风）");
                let _ = handle.emit(STATUS_EVENT, serde_json::json!({ "mode": "sherpa" }));
                let partials = real.partials_handle();
                (Box::new(real), AsrMode::Sherpa { partials })
            }
            Err(e) => {
                eprintln!("[engine] 真实 ASR 不可用，回退演示模式: {e}");
                let _ = handle
                    .emit(STATUS_EVENT, serde_json::json!({ "mode": "mock", "reason": e }));
                (Box::new(MockAsrPort::demo()), AsrMode::Mock)
            }
        };

        // 整理管线：同步路径兜底用 MockLlmPort（行为确定）；真实 LLM 走
        // tick + pending + apply_cleanup_result/fail_pending 的异步路径。
        let mut pipeline = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
        let mut now = Duration::ZERO; // 逻辑时钟（自管道创建起算）
        let mut known_speakers: Vec<u32> = Vec::new(); // 已登记说话人（颜色/性别）
        let mut segment_resolved = true; // 演示模式：上一段已整理落库，才追加下一段

        loop {
            now += TICK_INTERVAL;

            // 1. 实时 partial（真实 ASR 模式）：publish 最新一条给前端状态行。
            if let AsrMode::Sherpa { partials } = &mode {
                let texts: Vec<String> = partials.lock().unwrap().drain(..).collect();
                if let Some(last) = texts.into_iter().last() {
                    emit(&handle, EngineEvent::PartialResult { text: last });
                }
            }

            // 2. 追加转写：
            //    - 真实 ASR：排空 final 队列逐条追加（不丢字，final 全部入管线）；
            //    - 演示模式：无在途请求且上一段已落库时追加下一条（1 进 1 出）。
            match &mode {
                AsrMode::Sherpa { .. } => {
                    while let Some(utt) = asr.next_utterance() {
                        append_utterance(&handle, &mut pipeline, &mut known_speakers, now, utt);
                    }
                }
                AsrMode::Mock => {
                    if !pipeline.has_pending()
                        && !pipeline.scheduler().is_in_flight()
                        && segment_resolved
                    {
                        if let Some(utt) = asr.next_utterance() {
                            append_utterance(&handle, &mut pipeline, &mut known_speakers, now, utt);
                            segment_resolved = false;
                        }
                    }
                }
            }

            // 3. 时钟滴答：防抖/节奏触发 → 冻结 active → 派发一个 pending（单在途）。
            pipeline.tick(now);

            // 4. 有 pending → 经 LlmPort trait 调真实/mock LLM（SSE 流式）：
            //    增量 emit，完成/失败回填。
            if let Some(p) = pipeline.pending().cloned() {
                // 每次请求前重读配置：配置了有效 API Key 用真实客户端，
                // 否则用 MockLlmPort（未配置时整理降级为占位，ASR 不受影响）。
                let cfg = llm::read_config(&handle);
                let llm_port: Box<dyn engine::LlmPort> = if cfg.api_key.trim().is_empty() {
                    println!("[llm] 未配置 API Key，整理降级为 mock 占位");
                    Box::new(MockLlmPort)
                } else {
                    Box::new(OpenAiLlmClient::new(cfg))
                };
                println!("[llm] segment {} 送 LLM 整理（{} 字）", p.segment_id, p.raw.chars().count());
                let result = llm_port.cleanup_streaming(&p.raw, &mut |partial| {
                    emit(
                        &handle,
                        EngineEvent::SegmentCleaning {
                            segment_id: p.segment_id,
                            edit_id: p.edit_id,
                            partial: partial.to_string(),
                        },
                    );
                });
                match result {
                    Ok(cleaned) => {
                        println!("[llm] segment {} 整理完成: {cleaned:?}", p.segment_id);
                        for evt in pipeline.apply_cleanup_result(p.segment_id, cleaned, p.edit_id) {
                            emit(&handle, evt);
                        }
                    }
                    Err(err) => {
                        println!("[llm] segment {} 整理失败（重试 3 次后放弃）: {err}", p.segment_id);
                        for evt in pipeline.fail_pending() {
                            emit(&handle, evt);
                        }
                    }
                }
                segment_resolved = true;
            }

            std::thread::sleep(TICK_INTERVAL);
        }
    });
}

/// 把一条转写喂入整理管线：先发说话人归属（颜色/性别，首次出现），
/// 再发 `SegmentAppended`（对齐 engine 事件顺序）。
fn append_utterance(
    handle: &AppHandle,
    pipeline: &mut CleanupPipeline,
    known_speakers: &mut Vec<u32>,
    now: Duration,
    utt: engine::Utterance,
) {
    let evt = pipeline.append(now, utt.speaker_id, utt.text.clone());
    if let EngineEvent::SegmentAppended { segment } = &evt {
        if !known_speakers.contains(&segment.speaker_id) {
            known_speakers.push(segment.speaker_id);
            emit(
                handle,
                EngineEvent::SpeakerAssigned {
                    segment_id: segment.id,
                    speaker_id: segment.speaker_id,
                    is_new_speaker: true,
                    color: engine::speaker_color(segment.speaker_id),
                    gender: utt.gender,
                },
            );
        }
        emit(handle, evt);
    }
}

/// emit 一条 engine 事件（打印 + 推送给前端）。
fn emit(handle: &AppHandle, evt: EngineEvent) {
    println!(
        "[engine] → {ENGINE_EVENT}: {}",
        serde_json::to_string(&evt).unwrap_or_default()
    );
    let _ = handle.emit(ENGINE_EVENT, evt);
}
