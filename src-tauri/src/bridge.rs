//! 前后端事件桥。
//!
//! T1 阶段只做链路打通：后台线程周期 emit 测试事件 `bridge://ping`，
//! 前端 listen 后可见（console.log + 界面计数）并回执 `ping_ack`。
//! 事件名以 `bridge://` 前缀区分于后续 engine 事件（`engine://...`）。

use std::time::Duration;

use tauri::{AppHandle, Emitter};

/// 测试事件名（T1 专用，后续票替换为 engine 事件流）。
pub const PING_EVENT: &str = "bridge://ping";

/// 启动一个后台线程，周期性地 emit `bridge://ping`。
///
/// 首次延迟稍长，给前端 Vite 加载 + 注册 listen 留出时间。
pub fn spawn_ping_emitter(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let mut seq: u64 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(if seq == 0 { 1500 } else { 3000 }));
            seq += 1;
            let payload = serde_json::json!({ "type": "ping", "seq": seq });
            let _ = handle.emit(PING_EVENT, payload);
            println!("[bridge] emitted {PING_EVENT} # {seq}");
        }
    });
}
