//! Tauri 壳（薄胶水层）。
//!
//! 职责：把外部能力（音频采集、ASR、LLM、Embedding、托盘/热键）接入
//! `engine` 的三个端口，并经 Tauri Event 把 engine 事件流推给 Web 前端。
//! 本模块不承载业务逻辑。
//!
//! T1 阶段：仅打通「Rust → 前端」事件桥——周期 emit 测试事件
//! `bridge://ping`，前端 listen 并回执。T2 起用 engine 事件流
//! （`engine://event`）作为主事件流，ping 保留为调试心跳。
//! T4 起主事件源为 [crate::pipeline::spawn_engine_emitter]：
//! 真实 ASR（sherpa-onnx + 麦克风，失败回退合成转写）→ 事件流；
//! T13 托盘常驻（[crate::tray::setup]）、全局热键、单实例、
//! 关闭窗口改为隐藏到托盘。

pub mod audio;
mod asr;
mod bridge;
mod pipeline;
mod tray;

/// 前端收到 ping 后的回执命令，用于端到端确认事件桥闭环。
#[tauri::command]
fn ping_ack(payload: String) {
    println!("[bridge] frontend acknowledged ping: {payload}");
}

pub fn run() {
    tauri::Builder::default()
        // T13 单实例：重复启动时唤回已有主窗口（聚焦/取消最小化），
        // 避免麦克风与 ASR sidecar 被多个实例竞争。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .setup(|app| {
            // T13 托盘常驻 + 全局热键 + 会话状态镜像。
            tray::setup(app)?;
            bridge::spawn_ping_emitter(app.handle());
            // T4 起：真实 ASR（sherpa-onnx + 麦克风）优先，失败自动回退
            // 合成转写演示模式；事件流统一经 `engine://event` 推给前端。
            pipeline::spawn_engine_emitter(app.handle());
            Ok(())
        })
        // T13：窗口关闭时隐藏到托盘（常驻），真正退出走托盘菜单「退出」。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 仅对主窗口生效
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![ping_ack])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
