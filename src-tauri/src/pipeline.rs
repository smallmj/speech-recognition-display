//! 把 engine 事件流桥接到 Tauri 前端（`engine://event`）。
//!
//! T2 冒烟管线在壳层的接线：注入 [MockAsrPort]（合成转写），后台线程
//! 周期驱动 [Engine::step]，把产出的 [EngineEvent] 逐条 emit 给前端。
//! 前端 `listen("engine://event", ...)` 即可收到彩色气泡所需的全部数据
//! （说话人颜色/性别 + 片段正文）。

use std::time::Duration;

use engine::{Engine, MockAsrPort};
use tauri::{AppHandle, Emitter};

/// engine 事件流的事件名（与前端 `src/engineEvents.ts` 的 `ENGINE_EVENT` 保持一致）。
pub const ENGINE_EVENT: &str = "engine://event";

/// 启动后台线程驱动 engine 冒烟管线，把事件流 emit 给前端。
///
/// 事件节奏：每次循环驱动 [Engine::step] 产出 1~2 条事件，间隔 ~800ms，
/// 形成「气泡持续追加」的演示效果。
pub fn spawn_engine_emitter(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        // 注入合成 ASR 端口（4 位说话人轮流发言，无限循环）。
        let (mut engine, rx) = Engine::new(Box::new(MockAsrPort::demo()));
        engine.start();

        // 首次延迟稍长，给前端 Vite 加载 + 注册 listen 留出时间。
        std::thread::sleep(Duration::from_millis(1200));

        loop {
            engine.step();
            while let Ok(evt) = rx.try_recv() {
                println!(
                    "[engine] → {ENGINE_EVENT}: {}",
                    serde_json::to_string(&evt).unwrap_or_default()
                );
                let _ = handle.emit(ENGINE_EVENT, evt);
            }
            std::thread::sleep(Duration::from_millis(800));
        }
    });
}
