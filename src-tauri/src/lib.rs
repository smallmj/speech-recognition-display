//! Tauri 壳（薄胶水层）。
//!
//! 职责：把外部能力（音频采集、ASR、LLM、Embedding、托盘/热键）接入
//! `engine` 的三个端口，并经 Tauri Event 把 engine 事件流推给 Web 前端。
//! 本模块不承载业务逻辑。
//!
//! T1 阶段：尚未接入真实外部能力，仅打通「Rust → 前端」事件桥——
//! 周期性地 emit 一条测试事件 `bridge://ping`，前端 listen 并回执，
//! 以此验证链路。T2 起用 engine 事件流（`engine://event`）作为主事件流，
//! ping 保留为调试心跳。

mod bridge;
mod pipeline;

/// 前端收到 ping 后的回执命令，用于端到端确认事件桥闭环。
#[tauri::command]
fn ping_ack(payload: String) {
    println!("[bridge] frontend acknowledged ping: {payload}");
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            bridge::spawn_ping_emitter(app.handle());
            // T2 冒烟管线：engine（MockAsrPort）事件流 → 前端 `engine://event`。
            pipeline::spawn_engine_emitter(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ping_ack])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
