//! Tauri 壳（薄胶水层）。
//!
//! 职责：把外部能力（音频采集、ASR、LLM、Embedding、托盘/热键）接入
//! `engine` 的三个端口，并经 Tauri Event 把 engine 事件流推给 Web 前端。
//! 本模块不承载业务逻辑。
//!
//! T1 阶段：仅打通「Rust → 前端」事件桥——周期 emit 测试事件
//! `bridge://ping`，前端 listen 并回执。T2 起用 engine 事件流
//! （`engine://event`）作为主事件流，ping 保留为调试心跳。
//! T4 + T9 整合后主事件源为 [crate::pipeline::spawn_engine_emitter]：
//! 真实 ASR（sherpa-onnx + 麦克风，失败回退合成转写）→ 整理管线 →
//! 真实 OpenAI 兼容 LLM（SSE 流式）→ 双轨事件流；另注册 LLM 配置命令
//! （[crate::llm::load_llm_config] / [crate::llm::save_llm_config]）
//! 与模型列表命令（[crate::llm::list_llm_models]）。

mod asr;
mod app_settings;
mod asr_config;
pub mod audio;
mod bridge;
mod cloud_asr;
mod export;
mod first_run;
mod llm;
mod model_paths;
mod pipeline;
mod sessions;

use tauri::Manager;

/// 前端收到 ping 后的回执命令，用于端到端确认事件桥闭环。
#[tauri::command]
fn ping_ack(payload: String) {
    println!("[bridge] frontend acknowledged ping: {payload}");
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(pipeline::SessionControl::default());
            bridge::spawn_ping_emitter(app.handle());
            // 主事件源（T4 + T9 + T10 整合）：真实 ASR（sherpa-onnx + 麦克风）优先，
            // 失败回退合成转写演示模式；final 转写 → 整理管线 → 真实 LLM（SSE）
            // 流式整理；partial 实时 publish；停止后分批汇总会议纪要。事件流统一经
            // `engine://event` 推给前端。
            pipeline::spawn_engine_emitter(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping_ack,
            llm::load_llm_config,
            llm::save_llm_config,
            llm::list_llm_models,
            asr_config::load_asr_config,
            asr_config::save_asr_config,
            app_settings::load_app_settings,
            app_settings::save_app_settings,
            first_run::load_first_run_config,
            first_run::save_first_run_preferences,
            first_run::complete_first_run,
            first_run::reset_first_run,
            first_run::run_first_run_setup,
            pipeline::start_session,
            pipeline::stop_session,
            export::export_session,
            sessions::list_sessions,
            sessions::load_session,
            sessions::export_session_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
