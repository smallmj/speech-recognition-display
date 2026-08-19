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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::{AsrPort, CleanupPipeline, EngineEvent, MockAsrPort, MockLlmPort};
use tauri::{AppHandle, Emitter};

use crate::asr::SherpaAsr;
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

/// 按配置启动 ASR 端口。只负责启动，不承担降级策略。
fn start_asr(config: &AsrConfig) -> Result<(Box<dyn AsrPort>, AsrMode), String> {
    match config.effective_source() {
        AsrSource::Local => {
            let real = SherpaAsr::spawn()?;
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
    match start_asr(config) {
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
                if let Ok((asr, mode)) = start_asr(&local_config) {
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

        // 首次按持久化配置启动。失败降级策略见 start_asr_with_fallback。
        let mut config = asr_config::read_config(&handle);
        let mut last_attempted_source = config.effective_source();
        let (mut asr, mut mode, mut active_source) = start_asr_with_fallback(&handle, &config);

        // 整理管线：同步路径兜底用 MockLlmPort（行为确定）；真实 LLM 走
        // tick + pending + apply_cleanup_result/fail_pending 的异步路径。
        let mut pipeline = CleanupPipeline::new_with_defaults(Box::new(MockLlmPort));
        let mut now = Duration::ZERO; // 逻辑时钟（自管道创建起算）
        let mut known_speakers: Vec<u32> = Vec::new(); // 已登记说话人（颜色/性别）
        let mut segment_resolved = true; // 演示模式：上一段已整理落库，才追加下一段
        let mut config_poll_ticks: u32 = 0;

        loop {
            now += TICK_INTERVAL;
            config_poll_ticks += 1;

            // 1. ASR 配置轮询：保存后热切换来源。切换前排空旧 final，
            //    pipeline / known_speakers / pending 状态保持不变。
            if config_poll_ticks >= ASR_CONFIG_POLL_TICKS {
                config_poll_ticks = 0;
                let next_config = asr_config::read_config(&handle);
                if next_config != config {
                    let next_source = next_config.effective_source();
                    let should_switch =
                        should_switch_asr(next_source, active_source, last_attempted_source);
                    if should_switch {
                        while let Some(utt) = asr.next_utterance() {
                            append_utterance(&handle, &mut pipeline, &mut known_speakers, now, utt);
                        }
                        asr.stop();
                        // 先释放旧麦克风/连接，再启动新来源，避免短暂双路采集。
                        drop(asr);
                        let (next_asr, next_mode, next_active) =
                            start_asr_with_fallback(&handle, &next_config);
                        asr = next_asr;
                        mode = next_mode;
                        active_source = next_active;
                        last_attempted_source = next_source;
                    }
                    config = next_config;
                }
            }

            // 1b. 实时 partial（本地/云端 ASR 模式）：publish 最新一条给前端状态行。
            let partial_mode = match &mode {
                AsrMode::Local { partials } | AsrMode::Cloud { partials } => Some(partials),
                AsrMode::Mock => None,
            };
            if let Some(partials) = partial_mode {
                let texts: Vec<String> = partials.lock().unwrap().drain(..).collect();
                if let Some(last) = texts.into_iter().last() {
                    emit(&handle, EngineEvent::PartialResult { text: last });
                }
            }

            // 2. 追加转写：
            //    - 本地/云端 ASR：排空 final 队列逐条追加（不丢字，final 全部入管线）；
            //    - 演示模式：无在途请求且上一段已落库时追加下一条（1 进 1 出）。
            let is_real_asr = !matches!(mode, AsrMode::Mock);
            if is_real_asr {
                while let Some(utt) = asr.next_utterance() {
                    append_utterance(&handle, &mut pipeline, &mut known_speakers, now, utt);
                }
            } else {
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
                println!(
                    "[llm] segment {} 送 LLM 整理（{} 字）",
                    p.segment_id,
                    p.raw.chars().count()
                );
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
                        println!(
                            "[llm] segment {} 整理失败（重试 3 次后放弃）: {err}",
                            p.segment_id
                        );
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
    fn does_not_restart_local_when_unused_cloud_fields_change() {
        assert!(!should_switch_asr(
            AsrSource::Local,
            AsrSource::Local,
            AsrSource::Local
        ));
    }
}
