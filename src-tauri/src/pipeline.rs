//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! 接线策略（T4 + T7 + T9 整合）：
//! - **ASR 可配置（T7）**：按 `asr-config.json` 启动本地 sherpa-onnx 或云端
//!   Deepgram 兼容 WebSocket；驱动线程每秒重读配置，来源变化时只替换 ASR
//!   端口，不重建整理管线/说话人状态，切换对字幕链路保持无缝。本地失败回退
//!   mock；云端失败回退本地，再失败回退 mock。模式经 `engine://status`
//!   事件告知前端（`{"mode":"sherpa"|"cloud"|"mock"}`）。
//! - **final 统一进整理管线（T9）**：无论真实还是合成，final 转写都直接喂入
//!   [engine::CleanupPipeline]（防抖 + 固定节奏 + 单在途），驱动线程周期
//!   `tick(now)` 冻结并派发 `pending`，**LLM 请求交给独立 worker 线程**
//!   执行（[crate::llm::OpenAiLlmClient]，SSE 流式），每个 delta 以
//!   `SegmentCleaning` 状态信号 emit（前端整理中保留原文），完成后经
//!   `apply_cleanup_result` 回填并 emit `SegmentsCleaned`（同一说话人批次），失败经
//!   `fail_pending` emit `CleanupFailed`（前端回退展示原文）。
//! - **说话人**：T4 阶段无 SCD，真实 ASR 的 final 全部归说话人 1、性别
//!   Unknown（T5 接入 SCD 后替换）；SpeakerAssigned（颜色/性别）在片段
//!   前 emit，与 engine 事件顺序对齐。
//!
//! 时基：engine 用逻辑时钟（[std::time::Duration]，自管道创建起算），驱动
//! 线程每 200ms 一个节拍推进 `now`（统一节拍：partial 刷新及时，整理
//! 节奏不变——真实 ASR 的防抖/固定节奏由 CleanupPipeline 的调度器决定）。
//!
//! T5（SCD）接线：说话人切换检测在 [crate::asr::SherpaAsr] 内部完成 —— stdout 读线程
//! 在解析每条 final 时（embedding 的唯一来源点）经 [engine::Scd] 决定
//! speaker_id/gender/is_new_speaker，speaker_id 随 [engine::Utterance] 进入
//! [engine::CleanupPipeline]（[append_utterance]），`is_new_speaker` 判定用于
//! `SpeakerAssigned` 事件（有判定用判定，降级路径 None 退回「首次出现即新建」）。
//! 因此本模块无需持有 SCD 状态；配置了 speaker embedding 模型时
//! [crate::asr::SherpaAsr::scd_embedding_active] 为真（真实余弦匹配），否则降级
//! 为单说话人（见 asr.rs 注释）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use engine::{
    chunk_for_summarize, AsrPort, CleanupPipeline, EngineEvent, LlmPort, MockAsrPort, MockLlmPort,
    BATCH_MAX_CHARS,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::asr::SherpaAsr;
use crate::app_settings;
use crate::asr_config::{self, AsrConfig, AsrSource};
use crate::cloud_asr::CloudAsr;
use crate::llm::{self, OpenAiLlmClient};

/// engine 事件流的事件名（与前端 `src/engineEvents.ts` 的 `ENGINE_EVENT` 保持一致）。
pub const ENGINE_EVENT: &str = "engine://event";
/// 壳层运行状态事件名（ASR 模式等运营信息，区别于 engine 业务事件流）。
pub const STATUS_EVENT: &str = "engine://status";

/// 驱动循环节拍：逻辑时钟步进间隔（ms）。统一 200ms：partial 刷新及时，
/// 整理管线的防抖（2s）/固定节奏（5s）不受节拍影响。
const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// ASR 配置轮询间隔（5 个 tick = 1s）。保存配置后无需重启应用。
const ASR_CONFIG_POLL_TICKS: u32 = 5;

/// 常规应用设置轮询间隔（5 个 tick = 1s）。整理间隔保存后即时生效。
const APP_SETTINGS_POLL_TICKS: u32 = 5;

/// ASR 数据源模式（决定追加节奏与 partial 是否可用）。
enum AsrMode {
    /// 本地 ASR（sherpa-onnx + 麦克风）：partial 实时 publish，final 全部
    /// 无条件追加（不丢字）。
    Local {
        /// 实时 partial 缓冲（sidecar stdout 线程写入，本循环轮询）。
        partials: Arc<Mutex<VecDeque<String>>>,
    },
    /// 云端 ASR（Deepgram 兼容 WebSocket）：同样 partial 实时 publish，
    /// final 全部无条件追加（不丢字）。
    Cloud {
        /// 实时 partial 缓冲（WebSocket 任务写入，本循环轮询）。
        partials: Arc<Mutex<VecDeque<String>>>,
    },
    /// 回退演示模式（合成转写）：无 partial，1 进 1 出节奏（便于观察
    /// 「追加 -> 防抖 -> 流式整理 -> 完成」的完整过程）。
    Mock,
}

impl AsrMode {
    fn status_name(&self) -> &'static str {
        match self {
            Self::Local { .. } => "sherpa",
            Self::Cloud { .. } => "cloud",
            Self::Mock => "mock",
        }
    }
}

fn emit_asr_status(handle: &AppHandle, mode: &str, reason: Option<String>) {
    let payload = match reason {
        Some(reason) => serde_json::json!({ "mode": mode, "reason": reason }),
        None => serde_json::json!({ "mode": mode }),
    };
    let _ = handle.emit(STATUS_EVENT, payload);
}

/// 透出本地 ASR 的说话人分组（SCD）状态：`active` = sidecar 确认加载了 speaker
/// embedding 模型（按音色分人）；`disabled` = 未配置或加载失败（全部归说话人 1）。
/// 由 [crate::asr] 的 stdout 读线程在 `started` 事件到达时调用，保证贴近真实。
pub(crate) fn emit_scd_status(handle: &AppHandle, active: bool) {
    let _ = handle.emit(
        STATUS_EVENT,
        serde_json::json!({ "mode": "sherpa", "scd": if active { "active" } else { "disabled" } }),
    );
}

/// 按配置启动 ASR 端口。只负责启动，不承担降级策略。
fn start_asr(handle: &AppHandle, config: &AsrConfig) -> Result<(Box<dyn AsrPort>, AsrMode), String> {
    match config.effective_source() {
        AsrSource::Local => {
            let real = SherpaAsr::spawn(handle)?;
            println!("[engine] 本地 ASR 已启动（sherpa-onnx + 麦克风）");
            if real.scd_configured() {
                println!(
                    "[engine] SCD: speaker embedding 模型已配置{}",
                    if real.scd_embedding_active() {
                        "，sidecar 已确认加载（embedding 余弦匹配生效）"
                    } else {
                        "（等待 sidecar 确认加载…）"
                    }
                );
            } else {
                println!(
                    "[engine] SCD: 未配置 speaker embedding 模型，降级为单说话人（全部归说话人 1）"
                );
            }
            let partials = real.partials_handle();
            Ok((Box::new(real), AsrMode::Local { partials }))
        }
        AsrSource::Cloud => {
            let real = CloudAsr::spawn(config.clone())?;
            println!("[engine] 云端 ASR 已启动（Deepgram 兼容流式接口）");
            let partials = real.partials_handle();
            Ok((Box::new(real), AsrMode::Cloud { partials }))
        }
    }
}

/// 是否需要热切换 ASR：
/// - 来源变化必须切换；
/// - 云端已生效且任意云端配置变化时重连；
/// - 云端曾尝试失败并回退本地后，用户修正云端配置也允许重试。
fn should_switch_asr(
    next_source: AsrSource,
    active_source: AsrSource,
    last_attempted_source: AsrSource,
) -> bool {
    next_source != last_attempted_source
        || (next_source == AsrSource::Cloud
            && (active_source == AsrSource::Cloud || last_attempted_source == AsrSource::Cloud))
}

/// 启动目标 ASR；失败时按「云端 -> 本地 -> mock」「本地 -> mock」降级。
fn start_asr_with_fallback(
    handle: &AppHandle,
    config: &AsrConfig,
) -> (Box<dyn AsrPort>, AsrMode, AsrSource) {
    let desired = config.effective_source();
    match start_asr(handle, config) {
        Ok((asr, mode)) => {
            emit_asr_status(handle, mode.status_name(), None);
            (asr, mode, desired)
        }
        Err(primary_error) => {
            if desired == AsrSource::Cloud {
                let local_config = AsrConfig {
                    source: AsrSource::Local,
                    ..config.clone()
                };
                if let Ok((asr, mode)) = start_asr(handle, &local_config) {
                    let reason = format!("云端 ASR 启动失败，已回退本地 ASR：{primary_error}");
                    eprintln!("[engine] {reason}");
                    emit_asr_status(handle, mode.status_name(), Some(reason));
                    return (asr, mode, AsrSource::Local);
                }
            }
            let reason = format!("ASR 启动失败，已回退演示模式：{primary_error}");
            eprintln!("[engine] {reason}");
            emit_asr_status(handle, "mock", Some(reason));
            (Box::new(MockAsrPort::demo()), AsrMode::Mock, desired)
        }
    }
}

/// 会话控制信号：前端 command（[stop_session] / [start_session]）与驱动线程共享。
///
/// 两个原子标志即表达会话状态机：
/// - 识别中：`running=true`（点「开始识别」后进入；应用启动时为 false）；
/// - 停止中：前端置 `stop_requested`，驱动线程收到后进入停止分支
///   （不再追加 → 冻结 → 排空整理 → 分批汇总纪要）；
/// - 已停止待开始：`running=false`，前端「开始识别」置 `start_requested`，
///   驱动线程重新拉起 ASR、重建管线并 emit `SessionStarted`。
#[derive(Clone)]
pub struct SessionControl {
    stop_requested: Arc<AtomicBool>,
    start_requested: Arc<AtomicBool>,
}

impl Default for SessionControl {
    fn default() -> Self {
        Self {
            stop_requested: Arc::new(AtomicBool::new(false)),
            start_requested: Arc::new(AtomicBool::new(false)),
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
/// 驱动线程检测到 `start_requested`，重建整理管线（清空上一会话片段与状态）
/// 并 emit `SessionStarted`，前端据此重置展示与纪要区。
#[tauri::command]
pub fn start_session(control: State<SessionControl>) -> Result<(), String> {
    control.start_requested.store(true, Ordering::Relaxed);
    Ok(())
}

/// LLM 异步结果：worker 线程完成后经 mpsc 通道交回主循环回填。
///
/// 主循环绝不能在整理请求期间阻塞——识别（final/partial）优先，LLM 整理
/// 交给独立线程，这样整理中仍能实时显示新识别文字。
struct LlmOutcome {
    segment_ids: Vec<u64>,
    edit_id: u64,
    result: Result<String, String>,
}

/// 启动后台线程驱动「真实/合成 ASR → 整理管线 → 真实 LLM → 事件流」垂直链路。
///
/// 节奏：每节拍推进逻辑时钟 → 拉取并 publish 最新 partial、排空 final 队列
/// 逐条追加（真实与演示模式都不因整理而停顿）→ `tick(now)` → 有 pending
/// 则交独立线程调真实 LLM（SSE 增量 emit）→ 结果经通道回主循环回填/失败。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
    let control = app.state::<SessionControl>().inner().clone();
    std::thread::spawn(move || {
        // 首次延迟稍长，给前端 Vite 加载 + 注册 listen 留出时间。
        std::thread::sleep(Duration::from_millis(1200));

        // 启动时只读配置，不拉起麦克风/sidecar；首次「开始识别」才启动。
        // 失败降级策略见 start_asr_with_fallback。
        let mut config = asr_config::read_config(&handle);
        let mut last_attempted_source = config.effective_source();
        let mut asr: Option<Box<dyn AsrPort>> = None;
        let mut mode: Option<AsrMode> = None;
        let mut active_source = config.effective_source();

        // 整理管线：初始 MockLlmPort 仅作占位；每个 pending 在 worker 线程里
        // 按最新配置选择 Mock / 真实 LLM，结果经通道回主循环回填。
        let mut pipeline = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
        // 常规设置：启动时应用已保存的整理间隔；之后每秒轮询，档位变化即时
        // 更新节奏（无需重建管线）。
        let mut app_config = app_settings::read_config(&handle);
        pipeline.set_rhythm_duration(app_config.cleanup_interval());
        let mut now = Duration::ZERO; // 逻辑时钟（自管道创建起算）
        let mut known_speakers: Vec<u32> = Vec::new(); // 已登记说话人（颜色/性别）
        let mut config_poll_ticks: u32 = 0;
        let mut app_settings_poll_ticks: u32 = 0;
        // LLM 整理在独立线程执行，结果经通道交回；dispatched_edit_id 防止
        // 同一条 pending 被重复送线程（pending 清空后 edit_id 才递增）。
        let (llm_result_tx, llm_result_rx) = mpsc::channel::<LlmOutcome>();
        let mut dispatched_edit_id: Option<u64> = None;

        // 会话状态机：启动落在「未开始」，点「开始识别」后再进入识别。
        let mut running = false;
        let mut stopping = false;

        loop {
            now += TICK_INTERVAL;
            config_poll_ticks += 1;
            app_settings_poll_ticks += 1;

            // 0. 常规设置轮询：整理间隔变化时热更新固定节奏（T12）。
            if app_settings_poll_ticks >= APP_SETTINGS_POLL_TICKS {
                app_settings_poll_ticks = 0;
                let next_app_config = app_settings::read_config(&handle);
                if next_app_config.cleanup_interval_seconds
                    != app_config.cleanup_interval_seconds
                {
                    println!(
                        "[settings] 整理间隔 {}s → {}s（即时生效）",
                        app_config.cleanup_interval_seconds,
                        next_app_config.cleanup_interval_seconds
                    );
                    pipeline.set_rhythm_duration(next_app_config.cleanup_interval());
                    app_config = next_app_config;
                }
            }

            // 1. ASR 配置轮询：识别中保存后热切换来源。切换前排空旧 final，
            //    pipeline / known_speakers / pending 状态保持不变。
            if config_poll_ticks >= ASR_CONFIG_POLL_TICKS && running {
                config_poll_ticks = 0;
                let next_config = asr_config::read_config(&handle);
                if next_config != config {
                    let next_source = next_config.effective_source();
                    let should_switch =
                        should_switch_asr(next_source, active_source, last_attempted_source);
                    if should_switch {
                        let asr_ref = asr.as_mut().expect("识别中 ASR 必须已启动");
                        drain_asr_inputs(asr_ref, &handle, &mut pipeline, &mut known_speakers, now);
                        asr_ref.stop();
                        // 先释放旧麦克风/连接，再启动新来源，避免短暂双路采集。
                        let (next_asr, next_mode, next_active) =
                            start_asr_with_fallback(&handle, &next_config);
                        asr = Some(next_asr);
                        mode = Some(next_mode);
                        active_source = next_active;
                        last_attempted_source = next_source;
                    }
                    config = next_config;
                }
            }

            // 2. 会话控制：收到「开始识别」且已停止完成 → 启动/复用 ASR 并重建
            //    管线开新会话；收到「停止并生成纪要」→ 停止追加并进入纪要停止流程。
            if !running && !stopping && control.start_requested.swap(false, Ordering::Relaxed) {
                // 停止期间 ASR 可能仍积累 final：丢弃，避免旧会话内容混入新会话。
                if let (Some(asr_ref), Some(mode_ref)) = (asr.as_mut(), mode.as_ref()) {
                    discard_asr_inputs(asr_ref, mode_ref);
                }
                let next_config = asr_config::read_config(&handle);
                // 每次「开始识别」都重新拉起 ASR：让模型补齐/云端配置变更立即生效，
                // 也避免先以 mock 回退启动后，后续会话仍复用旧 mock。
                if let Some(mut old) = asr.take() {
                    old.stop();
                }
                let (next_asr, next_mode, next_active) =
                    start_asr_with_fallback(&handle, &next_config);
                asr = Some(next_asr);
                mode = Some(next_mode);
                active_source = next_active;
                last_attempted_source = next_config.effective_source();
                config = next_config;
                pipeline = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
                pipeline.set_rhythm_duration(app_config.cleanup_interval());
                now = Duration::ZERO;
                known_speakers.clear();
                dispatched_edit_id = None;
                running = true;
                emit(&handle, EngineEvent::SessionStarted);
            }

            if running && control.stop_requested.swap(false, Ordering::Relaxed) {
                let asr_ref = asr.as_mut().expect("识别中 ASR 必须已启动");
                // 把停止瞬间已在缓冲的 final 全部收进管线，再冻结剩余 active。
                drain_asr_inputs(asr_ref, &handle, &mut pipeline, &mut known_speakers, now);
                let frozen = pipeline.freeze_all_active();
                println!("[session] 停止识别：冻结 {frozen} 条剩余片段，进入纪要流程");
                running = false;
                stopping = true;
            }

            // 3. 转写输入：
            //    - 识别中：partial 实时 publish、final 逐条追加（真实/演示一致）；
            //    - 未识别/已停止：丢弃继续到达的 partial/final（停止后不再产生新内容）。
            if running {
                let partial_mode = match mode.as_ref() {
                    Some(AsrMode::Local { partials }) | Some(AsrMode::Cloud { partials }) => {
                        Some(partials)
                    }
                    _ => None,
                };
                if let Some(partials) = partial_mode {
                    let texts: Vec<String> = partials.lock().unwrap().drain(..).collect();
                    if let Some(last) = texts.into_iter().last() {
                        emit(&handle, EngineEvent::PartialResult { text: last });
                    }
                }
                if let Some(asr_ref) = asr.as_mut() {
                    drain_asr_inputs(asr_ref, &handle, &mut pipeline, &mut known_speakers, now);
                }
            } else if let (Some(asr_ref), Some(mode_ref)) = (asr.as_mut(), mode.as_ref()) {
                discard_asr_inputs(asr_ref, mode_ref);
            }

            // 4. 先收 LLM 结果（若已就绪）：回填/失败与下一次派发同拍完成。
            //    识别中与停止排空阶段都要处理（停止时在途整理照常回填）。
            while let Ok(outcome) = llm_result_rx.try_recv() {
                let is_current = pipeline
                    .pending()
                    .is_some_and(|p| p.edit_id == outcome.edit_id);
                if !is_current {
                    continue; // 迟到/重复结果，忽略，不干扰当前在途请求
                }
                match outcome.result {
                    Ok(cleaned) => {
                        println!(
                            "[llm] segments {:?} 整理完成: {cleaned:?}",
                            outcome.segment_ids
                        );
                        for evt in pipeline.apply_cleanup_result(
                            &outcome.segment_ids,
                            cleaned,
                            outcome.edit_id,
                        ) {
                            emit(&handle, evt);
                        }
                    }
                    Err(err) => {
                        println!("[llm] segment 整理失败（重试 3 次后放弃）: {err}");
                        for evt in pipeline.fail_pending() {
                            emit(&handle, evt);
                        }
                    }
                }
            }

            // 5. 时钟滴答 + 派发 pending：识别中与停止排空阶段都允许整理在途。
            if running || stopping {
                pipeline.tick(now);
                if let Some(p) = pipeline.pending().cloned() {
                    if dispatched_edit_id == Some(p.edit_id) {
                        // 已派发过：等结果回来（尾部 sleep 统一执行）。
                    } else {
                        dispatched_edit_id = Some(p.edit_id);
                        let tx = llm_result_tx.clone();
                        let worker_handle = handle.clone();
                        std::thread::spawn(move || {
                            // 每次请求前重读配置：配置了有效 API Key 用真实客户端，
                            // 否则用 MockLlmPort（未配置时整理降级为占位，ASR 不受影响）。
                            let cfg = llm::read_config(&worker_handle);
                            let llm_port: Box<dyn engine::LlmPort> =
                                if cfg.api_key.trim().is_empty() {
                                    println!("[llm] 未配置 API Key，整理降级为 mock 占位");
                                    Box::new(MockLlmPort)
                                } else {
                                    Box::new(OpenAiLlmClient::new(cfg))
                                };
                            println!(
                                "[llm] speaker {} segments {:?} 送 LLM 整理（{} 字）",
                                p.speaker_id,
                                p.segment_ids,
                                p.raw.chars().count()
                            );
                            let result = llm_port.cleanup_streaming(&p.raw, &mut |partial| {
                                emit(
                                    &worker_handle,
                                    EngineEvent::SegmentCleaning {
                                        segment_id: p.segment_id,
                                        edit_id: p.edit_id,
                                        partial: partial.to_string(),
                                    },
                                );
                            });
                            let _ = tx.send(LlmOutcome {
                                segment_ids: p.segment_ids,
                                edit_id: p.edit_id,
                                result,
                            });
                        });
                    }
                }
            }

            // 6. 停止排空完成（无 pending / 在途 / 未整理片段）→ 分批汇总纪要。
            if stopping && pipeline_idle(&pipeline) {
                stopping = false;
                run_minutes(&handle, &pipeline);
            }

            std::thread::sleep(TICK_INTERVAL);
        }
    });
}

/// 丢弃停止后 ASR 继续缓冲的 partial / final（新会话开始前或停止排空期间）。
fn discard_asr_inputs(asr: &mut Box<dyn AsrPort>, mode: &AsrMode) {
    while asr.next_utterance().is_some() {}
    match mode {
        AsrMode::Local { partials } | AsrMode::Cloud { partials } => {
            partials.lock().unwrap().clear();
        }
        AsrMode::Mock => {}
    }
}

/// 排空 ASR 已定稿的 final，逐条追加进整理管线（热切换/停止/识别共用）。
fn drain_asr_inputs(
    asr: &mut Box<dyn AsrPort>,
    handle: &AppHandle,
    pipeline: &mut CleanupPipeline,
    known_speakers: &mut Vec<u32>,
    now: Duration,
) {
    while let Some(utt) = asr.next_utterance() {
        append_utterance(handle, pipeline, known_speakers, now, utt);
    }
}

/// 整理管线是否已全部消化：无在途、无 pending、无未整理/未冻结片段。
fn pipeline_idle(pipeline: &CleanupPipeline) -> bool {
    !pipeline.has_pending()
        && !pipeline.scheduler().is_in_flight()
        && pipeline.store().get_frozen_uncleaned().is_empty()
        && pipeline
            .store()
            .segments()
            .iter()
            .all(|s| s.status != engine::SegmentStatus::Active)
}

/// 纪要阶段使用的 LLM 端口（Result 形态）：真实客户端失败可回退该批原文，
/// Mock 始终成功（确定性输出）。
enum MinutesLlm {
    Mock(MockLlmPort),
    OpenAi(OpenAiLlmClient),
}

impl MinutesLlm {
    fn summarize(&self, chunks: &[String]) -> Result<String, String> {
        match self {
            Self::Mock(mock) => Ok(mock.summarize(chunks)),
            Self::OpenAi(client) => client.summarize_result(chunks),
        }
    }
}

/// 构造当前生效的纪要 LLM：配置了有效 API Key → 真实 OpenAI 兼容客户端；
/// 未配置 → 降级 Mock（纪要仍能输出结构化占位，ASR 不受影响）。
fn current_minutes_llm(handle: &AppHandle) -> MinutesLlm {
    let cfg = llm::read_config(handle);
    if cfg.api_key.trim().is_empty() {
        println!("[llm] 未配置 API Key，纪要降级为 mock 占位（ASR 不受影响）");
        MinutesLlm::Mock(MockLlmPort)
    } else {
        MinutesLlm::OpenAi(OpenAiLlmClient::new(cfg))
    }
}

/// 停止流程（T10）：emit `SessionStopped` → engine 分批（每批 ≤500 字 + 滚动
/// 上文）→ 逐批真实 LLM 生成部分纪要 → ≥2 批时再汇总为最终结构化纪要
/// （要点/行动项/待办）→ emit `MinutesReady`。
///
/// 分批算法与 engine 的 [engine::minutes] 纯函数同构；纪要最终结果整段返回
/// （不流式展示）。单批失败时回退该批原文、汇总失败时拼接各批部分纪要，
/// 尽力不丢内容。
fn run_minutes(handle: &AppHandle, pipeline: &CleanupPipeline) {
    emit(handle, EngineEvent::SessionStopped);
    let segments: Vec<engine::Segment> = pipeline.store().segments().to_vec();
    let batches = chunk_for_summarize(&segments, BATCH_MAX_CHARS);
    if batches.is_empty() {
        emit(
            handle,
            EngineEvent::MinutesReady {
                minutes: "（本次会话无内容，未生成纪要）".to_string(),
            },
        );
        return;
    }

    let llm_port = current_minutes_llm(handle);
    let mut partials: Vec<String> = Vec::new();
    for (i, batch) in batches.iter().enumerate() {
        let batch_text = batch.join("\n");
        println!(
            "[minutes] 第 {} 批送 LLM 生成纪要（{} 字）",
            i + 1,
            batch_text.chars().count()
        );
        match llm_port.summarize(&[batch_text.clone()]) {
            Ok(partial) => partials.push(partial),
            Err(err) => {
                // 单批失败（重试 3 次后）：回退该批原文，尽力不丢内容。
                println!("[minutes] 第 {} 批纪要失败（回退该批原文）: {err}", i + 1);
                partials.push(batch_text);
            }
        }
    }

    let minutes = if partials.len() > 1 {
        match llm_port.summarize(&partials) {
            Ok(merged) => merged,
            Err(err) => {
                // 汇总失败：拼接各批部分纪要兜底（仍保留结构化分节）。
                println!("[minutes] 汇总失败（拼接各批部分纪要）: {err}");
                partials.join("\n\n")
            }
        }
    } else {
        partials
            .into_iter()
            .next()
            .unwrap_or_else(|| "（无内容）".to_string())
    };

    // T11 自动保存：会话（原文/整理版/说话人/时间）与纪要写入本地历史，重启后仍在。
    let session_segments: Vec<crate::sessions::SessionSegment> = segments
        .iter()
        .map(|s| crate::sessions::SessionSegment {
            id: s.id,
            speaker_id: s.speaker_id,
            raw: s.raw.clone(),
            cleaned: s.cleaned.clone(),
            ts: s.ts,
        })
        .collect();
    if let Err(err) = crate::sessions::save_session(handle, session_segments, minutes.clone()) {
        eprintln!("[sessions] 自动保存会话失败: {err}");
    }

    emit(handle, EngineEvent::MinutesReady { minutes });
}

/// 把一条转写喂入整理管线：先发说话人归属（颜色/性别，首次出现），
/// 再发 `SegmentAppended`（对齐 engine 事件顺序）。
///
/// T5（SCD）接线：`utt.is_new_speaker` 为 SCD 在 read_stdout 的判定结果
/// （真实 embedding 路径）——有判定用判定（首个 final 是短发言时 SCD 归入
/// 说话人 1 且 is_new=false）；无判定（降级单说话人路径，None）退回
/// 「首次出现即新建」的 T9 既有行为。
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
                    is_new_speaker: utt.is_new_speaker.unwrap_or(true),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_when_requested_source_changes() {
        assert!(should_switch_asr(
            AsrSource::Cloud,
            AsrSource::Local,
            AsrSource::Local
        ));
        assert!(should_switch_asr(
            AsrSource::Local,
            AsrSource::Cloud,
            AsrSource::Cloud
        ));
    }

    #[test]
    fn reconnects_cloud_when_cloud_config_changes() {
        assert!(should_switch_asr(
            AsrSource::Cloud,
            AsrSource::Cloud,
            AsrSource::Cloud
        ));
    }

    #[test]
    fn retries_cloud_after_failed_attempt_and_config_fix() {
        assert!(should_switch_asr(
            AsrSource::Cloud,
            AsrSource::Local,
            AsrSource::Cloud
        ));
    }

    #[test]
    fn pipeline_idle_tracks_pending_and_frozen_work() {
        let mut p = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
        assert!(pipeline_idle(&p), "空管线即 idle");

        p.append(Duration::ZERO, 1, "一句话".to_string());
        assert!(!pipeline_idle(&p), "有 active 片段未冻结 → 不 idle");

        p.freeze_all_active();
        assert!(!pipeline_idle(&p), "已冻结但未整理 → 不 idle");

        p.step(Duration::from_secs(3));
        assert!(pipeline_idle(&p), "整理完成后应回到 idle");
    }

    #[test]
    fn does_not_restart_local_when_unused_cloud_fields_change() {
        assert!(!should_switch_asr(
            AsrSource::Local,
            AsrSource::Local,
            AsrSource::Local
        ));
    }
}
