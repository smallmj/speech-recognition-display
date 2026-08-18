//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! 接线策略（T4 起）：
//! - **真实 ASR 优先**：尝试启动 sherpa-onnx sidecar + 麦克风（[crate::asr::SherpaAsr]）；
//!   成功则引擎用真实识别结果（final → 气泡，partial → 实时状态行）。
//! - **失败回退**：sidecar/模型缺失或麦克风不可用时回退到
//!   [engine::MockAsrPort]（合成转写），保证演示模式始终可用。
//! - 模式经 `engine://status` 事件告知前端（`{"mode":"sherpa"|"mock"}`）。

use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use engine::{Engine, EngineEvent, MockAsrPort};
use tauri::{AppHandle, Emitter};

use crate::asr::SherpaAsr;

/// engine 事件流的事件名（与前端 `src/engineEvents.ts` 的 `ENGINE_EVENT` 保持一致）。
pub const ENGINE_EVENT: &str = "engine://event";
/// 壳层运行状态事件名（ASR 模式等运营信息，区别于 engine 业务事件流）。
pub const STATUS_EVENT: &str = "engine://status";

/// 启动后台线程驱动 engine 管线，把事件流 emit 给前端。
///
/// 真实 ASR 模式下循环节拍 200ms（partial 刷新及时）；回退演示模式沿用 800ms。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
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
                run_engine(handle, &mut engine, &rx, 200, Some(partials));
            }
            Err(e) => {
                eprintln!("[engine] 真实 ASR 不可用，回退演示模式: {e}");
                handle
                    .emit(STATUS_EVENT, serde_json::json!({ "mode": "mock", "reason": e }))
                    .ok();
                let (mut engine, rx) = Engine::new(Box::new(MockAsrPort::demo()));
                engine.start();
                run_engine(handle, &mut engine, &rx, 800, None);
            }
        }
    });
}

/// 引擎主循环：
/// 1. 拉取并 publish 实时 partial（真实 ASR 模式）；
/// 2. [Engine::step] 拉取 final 转写，产出说话人/片段事件；
/// 3. 把事件流逐条 emit 给前端。
fn run_engine(
    handle: AppHandle,
    engine: &mut Engine,
    rx: &mpsc::Receiver<EngineEvent>,
    tick_ms: u64,
    partials: Option<Arc<Mutex<VecDeque<String>>>>,
) {
    loop {
        if let Some(queue) = &partials {
            let texts: Vec<String> = queue.lock().unwrap().drain(..).collect();
            if let Some(last) = texts.into_iter().last() {
                engine.publish(EngineEvent::PartialResult { text: last });
            }
        }

        engine.step();

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
