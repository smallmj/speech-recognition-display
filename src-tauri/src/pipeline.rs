//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! 接线策略（T4 + T9 + T10 整合）：
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
//! - **会话控制（T10）**：[SessionControl]（`stop_requested` /
//!   `session_active` 两个原子标志）由 [stop_session] / [start_session] 两个
//!   `#[tauri::command]` 与驱动线程共享。驱动线程每拍检查停止信号，收到后
//!   进入**停止分支**（[run_stop_flow]）：停止追加转写 → 冻结剩余 active →
//!   排空整理队列（在途 LLM 结果照常回填，纪要尽可能基于整理版文本）→
//!   emit `SessionStopped` → engine 分批（[engine::minutes::chunk_for_summarize]，
//!   每批 ≤500 字 + 滚动上文）→ 逐批调 `OpenAiLlmClient::summarize` 生成
//!   部分纪要 → 汇总为最终结构化纪要（要点/行动项/待办）→ emit
//!   `MinutesReady`。停止后驱动线程等待「开始识别」；再次开始时重建整理
//!   管线（清空上一会话片段，真实 ASR 则丢弃停止期间堆积的 final/partial）
//!   并 emit `SessionStarted`，前端据此重置展示与纪要区。
//!
//! 时基：engine 用逻辑时钟（[std::time::Duration]，自管道创建起算），驱动
//! 线程每 200ms 一个节拍推进 `now`（统一节拍：partial 刷新及时，整理
//! 节奏不变——真实 ASR 的防抖/固定节奏由 CleanupPipeline 的调度器决定）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::minutes::{chunk_for_summarize, BATCH_MAX_CHARS};
use engine::{AsrPort, CleanupPipeline, EngineEvent, MockAsrPort, MockLlmPort, Segment, SegmentStatus};
use tauri::{AppHandle, Emitter, Manager, State};

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

/// 会话控制信号：前端 command（[stop_session] / [start_session]）与驱动线程共享。
///
/// 两个原子标志即可表达三态会话机：识别中（`session_active=true`）、
/// 停止中（`stop_requested=true`，驱动线程收到后走停止分支）、
/// 已停止待开始（`session_active=false`）。
#[derive(Clone, Default)]
pub struct SessionControl {
    /// 「停止并生成纪要」请求：前端置位，驱动线程收到后走停止分支。
    pub stop_requested: Arc<AtomicBool>,
    /// 会话是否进行中（识别中）：驱动线程维护，前端据此切换按钮/状态。
    pub session_active: Arc<AtomicBool>,
}

impl SessionControl {
    /// 默认识别中：应用启动即自动开始一个会话（对齐 T9 演示行为）。
    pub fn new() -> Self {
        Self {
            stop_requested: Arc::new(AtomicBool::new(false)),
            session_active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// 前端「停止并生成纪要」命令：通知驱动线程停止追加转写并触发分批汇总。
#[tauri::command]
pub fn stop_session(control: State<SessionControl>) -> Result<(), String> {
    control.stop_requested.store(true, Ordering::Relaxed);
    Ok(())
}

/// 前端「开始识别」命令：开始（或重新开始）一个会话。停止后再次开始时，
/// 驱动线程检测到 `session_active` 上升沿，重建整理管线（清空上一会话
/// 片段）并 emit `SessionStarted`。
#[tauri::command]
pub fn start_session(control: State<SessionControl>) -> Result<(), String> {
    control.session_active.store(true, Ordering::Relaxed);
    Ok(())
}

/// 启动后台线程驱动「真实/合成 ASR → 整理管线 → 真实 LLM → 事件流」垂直链路。
///
/// 节奏：每节拍推进逻辑时钟 → 先检查停止信号（收到则走 [run_stop_flow]）→
/// 会话不在进行中时等待「开始识别」（真实 ASR 排空堆积缓冲）→ （真实模式）
/// 拉取并 publish 最新 partial、排空 final 队列逐条追加；（演示模式）无在途
/// 请求且上一段已落库时追加下一条 → `tick(now)` → 有 pending 则调真实 LLM
/// （SSE 增量 emit）→ 回填/失败。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
    let control = app.state::<SessionControl>().inner().clone();
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
        // 上一拍会话是否进行中（检测「停止 → 重新开始」的上升沿，重建管线）。
        let mut was_active = control.session_active.load(Ordering::Relaxed);

        // 启动即自动开始一个会话（对齐 T9 演示行为；前端据此初始化「识别中」态）。
        emit(&handle, EngineEvent::SessionStarted);

        loop {
            now += TICK_INTERVAL;

            // 0. 停止请求：走「停止 → 排空 → 分批汇总纪要」分支。
            if control.stop_requested.swap(false, Ordering::Relaxed) {
                run_stop_flow(&handle, &control, &mut pipeline, &mut now);
                continue;
            }

            // 1. 会话状态机：上升沿重建管线；非激活时等待「开始识别」。
            let active = control.session_active.load(Ordering::Relaxed);
            if active && !was_active {
                // 重新开始：重建管线（清空上一会话片段与状态），emit SessionStarted，
                // 前端据此重置双轨展示与纪要区。
                pipeline = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
                now = Duration::ZERO;
                known_speakers.clear();
                segment_resolved = true;
                match &mode {
                    // 真实 ASR：丢弃停止期间堆积的 final 与 partial（不属于新会话），
                    // 麦克风与 sidecar 保持运行（重启开销大）。
                    AsrMode::Sherpa { partials } => {
                        while asr.next_utterance().is_some() {}
                        partials.lock().unwrap().clear();
                    }
                    // 演示模式：重置合成脚本从头播放。
                    AsrMode::Mock => {
                        asr = Box::new(MockAsrPort::demo());
                    }
                }
                emit(&handle, EngineEvent::SessionStarted);
            }
            was_active = active;
            if !active {
                // 停止期间：排空真实 ASR 堆积的 final/partial，防缓冲无界增长。
                if let AsrMode::Sherpa { partials } = &mode {
                    while asr.next_utterance().is_some() {}
                    partials.lock().unwrap().clear();
                }
                std::thread::sleep(TICK_INTERVAL);
                continue;
            }

            // 2. 实时 partial（真实 ASR 模式）：publish 最新一条给前端状态行。
            if let AsrMode::Sherpa { partials } = &mode {
                let texts: Vec<String> = partials.lock().unwrap().drain(..).collect();
                if let Some(last) = texts.into_iter().last() {
                    emit(&handle, EngineEvent::PartialResult { text: last });
                }
            }

            // 3. 追加转写：
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

            // 4. 时钟滴答：防抖/节奏触发 → 冻结 active → 派发一个 pending（单在途）。
            pipeline.tick(now);

            // 5. 有 pending → 经 LlmPort trait 调真实/mock LLM（SSE 流式）：
            //    增量 emit，完成/失败回填。
            if let Some(p) = pipeline.pending().cloned() {
                let llm_port = current_llm(&handle);
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

/// 构造当前生效的 LLM 端口（每次请求前调用，前端保存配置后无需重启即生效）：
///
/// - 配置了有效 API Key → 真实 OpenAI 兼容客户端（[OpenAiLlmClient]）；
/// - 未配置 → 降级为 [MockLlmPort]（整理照常产出确定性结果，**绝不降级
///   ASR**——真实/合成识别路径不受影响，对齐审查意见「LLM 未配置时整理
///   管线可降级，但不能把 ASR 降级」）。
fn current_llm(handle: &AppHandle) -> Box<dyn engine::LlmPort> {
    let cfg = llm::read_config(handle);
    if cfg.api_key.trim().is_empty() {
        println!("[llm] 未配置 API Key，整理降级为 mock 占位（ASR 不受影响）");
        Box::new(MockLlmPort)
    } else {
        Box::new(OpenAiLlmClient::new(cfg))
    }
}

/// 停止流程（T10）：不再追加转写 → 冻结剩余 active → 排空整理队列（在途
/// LLM 结果照常回填）→ emit `SessionStopped` → engine 分批 → 逐批调真实
/// LLM 生成部分纪要 → 汇总为最终结构化纪要 → emit `MinutesReady`。
///
/// 分批汇总算法与 engine 的 `summarize_minutes`（同步模拟路径）同构，只是把
/// `LlmPort` 换成 [engine::LlmPort] 的真实实现（[OpenAiLlmClient]）：
/// 每批一个请求（≤500 字 + 滚动上文，防上下文溢出对齐 ADR-0003），≥2 批时
/// 再调一次 LLM 把各批部分纪要合并为最终纪要（要点/行动项/待办）；单批时
/// 该批部分纪要即最终纪要。纪要最终结果整段返回（不流式展示）。
fn run_stop_flow(
    handle: &AppHandle,
    control: &SessionControl,
    pipeline: &mut CleanupPipeline,
    now: &mut Duration,
) {
    // 1. 冻结剩余 active：本次会话全部片段进入「可整理」状态。
    pipeline.store_mut().freeze_all_active();

    // 2. 排空整理队列：持续推进 tick + 处理 pending，直到无在途、无待整理、
    //    无 active（在途 LLM 结果照常经 apply_cleanup_result/fail_pending 回填，
    //    纪要尽可能基于整理版文本）。排空可能需等一次防抖（≤2s），与正常演示
    //    节奏一致。
    loop {
        let still_working = pipeline.has_pending()
            || !pipeline.store().get_frozen_uncleaned().is_empty()
            || pipeline
                .store()
                .segments()
                .iter()
                .any(|s| s.status == SegmentStatus::Active);
        if !still_working {
            break;
        }
        *now += TICK_INTERVAL;
        pipeline.tick(*now);
        if let Some(p) = pipeline.pending().cloned() {
            let llm_port = current_llm(handle);
            let result = llm_port.cleanup_streaming(&p.raw, &mut |partial| {
                emit(
                    handle,
                    EngineEvent::SegmentCleaning {
                        segment_id: p.segment_id,
                        edit_id: p.edit_id,
                        partial: partial.to_string(),
                    },
                );
            });
            match result {
                Ok(cleaned) => {
                    for evt in pipeline.apply_cleanup_result(p.segment_id, cleaned, p.edit_id) {
                        emit(handle, evt);
                    }
                }
                Err(err) => {
                    println!("[llm] segment {} 整理失败（重试 3 次后放弃）: {err}", p.segment_id);
                    for evt in pipeline.fail_pending() {
                        emit(handle, evt);
                    }
                }
            }
        }
        std::thread::sleep(TICK_INTERVAL);
    }

    // 3. 会话已停止：前端据此显示「正在生成纪要…」。
    emit(handle, EngineEvent::SessionStopped);

    // 4. 分批汇总纪要：engine 分批（每批 ≤500 字 + 滚动上文）→ 逐批真实 LLM。
    let segments: Vec<Segment> = pipeline.store().segments().to_vec();
    let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
    if batches.is_empty() {
        emit(
            handle,
            EngineEvent::MinutesReady {
                minutes: "（本次会话无内容，未生成纪要）".to_string(),
            },
        );
    } else {
        let llm_port = current_llm(handle);
        let mut partials: Vec<String> = Vec::new();
        for (i, batch) in batches.iter().enumerate() {
            // 批内文本拼接（含滚动上文元素；engine 已把滚动上文放在批首）。
            let batch_text = batch.join("\n");
            println!("[minutes] 第 {} 批送 LLM 生成纪要（{} 字）", i + 1, batch_text.chars().count());
            // 纪要最终结果整段返回，不流式展示（on_partial 为 no-op）。
            match llm_port.summarize_streaming(&[batch_text], &mut |_| {}) {
                Ok(partial) => partials.push(partial),
                Err(err) => {
                    // 单批失败（重试 3 次后）：回退该批原文，尽力不丢内容。
                    println!("[minutes] 第 {} 批纪要失败（重试 3 次后放弃，回退该批原文）: {err}", i + 1);
                    partials.push(batch_text);
                }
            }
        }
        // 汇总：≥2 批时再调一次 LLM 合并为最终纪要；单批时该批部分纪要即最终纪要。
        let minutes = if partials.len() > 1 {
            match llm_port.summarize_streaming(&partials, &mut |_| {}) {
                Ok(m) => m,
                Err(err) => {
                    // 汇总失败（重试 3 次后）：拼接各批部分纪要兜底（仍是结构化分节）。
                    println!("[minutes] 汇总失败（重试 3 次后放弃，拼接各批部分纪要）: {err}");
                    partials.join("\n\n")
                }
            }
        } else {
            partials.into_iter().next().unwrap_or_else(|| "（无内容）".to_string())
        };
        emit(handle, EngineEvent::MinutesReady { minutes });
    }

    // 5. 会话结束：驱动线程回到「等待开始识别」状态。
    control.session_active.store(false, Ordering::Relaxed);
}

/// emit 一条 engine 事件（打印 + 推送给前端）。
fn emit(handle: &AppHandle, evt: EngineEvent) {
    println!(
        "[engine] → {ENGINE_EVENT}: {}",
        serde_json::to_string(&evt).unwrap_or_default()
    );
    let _ = handle.emit(ENGINE_EVENT, evt);
}
