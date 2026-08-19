//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! 接线策略（T4 起）：
//! - **真实 ASR 优先**：尝试启动 sherpa-onnx sidecar + 麦克风（[crate::asr::SherpaAsr]）；
//!   成功则引擎用真实识别结果（final → 气泡，partial → 实时状态行）。
//! - **失败回退**：sidecar/模型缺失或麦克风不可用时回退到
//!   [engine::MockAsrPort]（合成转写），保证演示模式始终可用。
//! - 模式经 `engine://status` 事件告知前端（`{"mode":"sherpa"|"mock"}`）。
//!
//! T13 新增会话控制：[SessionControl]（`stop_requested` / `session_active` 两个
//! 原子标志）由 [stop_session] / [start_session] 命令与驱动线程共享。
//! 驱动循环每拍检查停止信号，收到后暂停追加转写 + emit `SessionStopped`；
//! 再次开始时恢复追加 + emit `SessionStarted`。托盘（[crate::tray]）与前端
//! 按钮共用同一 `SessionControl` 路径，状态不可能分叉。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use engine::{Engine, EngineEvent, MockAsrPort};
use tauri::{AppHandle, Emitter, State};

use crate::asr::SherpaAsr;

/// engine 事件流的事件名（与前端 `src/engineEvents.ts` 的 `ENGINE_EVENT` 保持一致）。
pub const ENGINE_EVENT: &str = "engine://event";
/// 壳层运行状态事件名（ASR 模式等运营信息，区别于 engine 业务事件流）。
pub const STATUS_EVENT: &str = "engine://status";

/// T13 会话控制信号：前端 command 与驱动线程共享。
///
/// 两个原子标志表达三态机：识别中（`session_active=true`）、
/// 停止中（`stop_requested=true`，驱动收到后走停止路径）、
/// 已停止待开始（`session_active=false`）。
///
/// 前端「停止识别」走 [stop_session]，「开始识别」走 [start_session]；
/// 托盘菜单/热键也经同一命令路径——统一信号源，状态不可能分叉。
#[derive(Clone, Default)]
pub struct SessionControl {
    /// 前端/托盘置位，驱动线程收到后暂停追加 + emit SessionStopped。
    pub stop_requested: Arc<AtomicBool>,
    /// 会话是否进行中（识别中）：驱动线程维护。
    pub session_active: Arc<AtomicBool>,
}

impl SessionControl {
    /// 默认识别中：应用启动即自动开始一个会话。
    pub fn new() -> Self {
        Self {
            stop_requested: Arc::new(AtomicBool::new(false)),
            session_active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// 前端/托盘「停止识别」命令：通知驱动线程停止追加转写。
#[tauri::command]
pub fn stop_session(control: State<'_, SessionControl>) -> Result<(), String> {
    control.stop_requested.store(true, Ordering::Relaxed);
    Ok(())
}

/// 前端/托盘「开始识别」命令：开始（或重新开始）一个会话。
#[tauri::command]
pub fn start_session(control: State<'_, SessionControl>) -> Result<(), String> {
    control.session_active.store(true, Ordering::Relaxed);
    Ok(())
}

/// 启动后台线程驱动 engine 管线，把事件流 emit 给前端。
///
/// 真实 ASR 模式下循环节拍 200ms（partial 刷新及时）；回退演示模式沿用 800ms。
/// T13：启动时 emit `SessionStarted`，循环每拍检查 [SessionControl] 停止信号。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
    let control = app.state::<SessionControl>().inner().clone();
    std::thread::spawn(move || {
        // 首次延迟稍长，给前端 Vite 加载 + 注册 listen 留出时间。
        std::thread::sleep(Duration::from_millis(1200));

        // 尝试真实 ASR；失败回退 mock（演示模式）。
        match SherpaAsr::spawn() {
            Ok(real) => {
                println!("[engine] 真实 ASR 已启动（sherpa-onnx + 麦克风）");
                handle
                    .emit(STATUS_EVENT, serde_json::json!({ "mode": "sherpa" }))
                    .ok();
                // 在 real 移入 Engine 前取出 partial 共享句柄，供主循环轮询。
                let partials = real.partials_handle();
                let (mut engine, rx) = Engine::new(Box::new(real));
                engine.start();
                run_engine(handle, &control, &mut engine, &rx, 200, Some(partials));
            }
            Err(e) => {
                eprintln!("[engine] 真实 ASR 不可用，回退演示模式: {e}");
                handle
                    .emit(STATUS_EVENT, serde_json::json!({ "mode": "mock", "reason": e }))
                    .ok();
                let (mut engine, rx) = Engine::new(Box::new(MockAsrPort::demo()));
                engine.start();
                run_engine(handle, &control, &mut engine, &rx, 800, None);
            }
        }
    });
}

/// 引擎主循环：
/// 1. T13 每拍检查会话控制信号（停止/等待开始）；
/// 2. 拉取并 publish 实时 partial（真实 ASR 模式）；
/// 3. [Engine::step] 拉取 final 转写，产出说话人/片段事件；
/// 4. 把事件流逐条 emit 给前端。
fn run_engine(
    handle: AppHandle,
    control: &SessionControl,
    engine: &mut Engine,
    rx: &mpsc::Receiver<EngineEvent>,
    tick_ms: u64,
    partials: Option<Arc<Mutex<VecDeque<String>>>>,
) {
    // 启动时通知前端「识别中」。
    let _ = handle.emit(ENGINE_EVENT, EngineEvent::SessionStarted);

    let mut was_active = true; // 上一轮会话状态（用于检测上升沿）

    loop {
        // 0. T13 会话控制：停止请求 → 暂停追加 + emit SessionStopped。
        if control.stop_requested.swap(false, Ordering::Relaxed) {
            control.session_active.store(false, Ordering::Relaxed);
            was_active = false;
            let _ = handle.emit(ENGINE_EVENT, EngineEvent::SessionStopped);
            // 等待「开始识别」（不追加、不 tick，但不退出）。
            loop {
                if control.session_active.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            // 重新开始：emit SessionStarted，前端据此重置展示。
            let _ = handle.emit(ENGINE_EVENT, EngineEvent::SessionStarted);
            was_active = true;
        }

        // 1. 实时 partial（真实 ASR 模式）：publish 最新一条给前端状态行。
        if let Some(queue) = &partials {
            let texts: Vec<String> = queue.lock().unwrap().drain(..).collect();
            if let Some(last) = texts.into_iter().last() {
                engine.publish(EngineEvent::PartialResult { text: last });
            }
        }

        // 2. engine.step() 拉取 final 转写，产出说话人/片段事件。
        engine.step();

        // 3. 事件流逐条 emit 给前端。
        while let Ok(evt) = rx.try_recv() {
            println!(
                "[engine] → {ENGINE_EVENT}: {}",
                serde_json::to_string(&evt).unwrap_or_default()
            );
            let _ = handle.emit(ENGINE_EVENT, evt);
        }

        std::thread::sleep(Duration::from_millis(tick_ms));
    }
}
