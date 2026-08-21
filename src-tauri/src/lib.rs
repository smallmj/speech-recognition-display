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
mod models;
mod pipeline;
mod sessions;
mod tray;

use tauri::Manager;

/// 前端收到 ping 后的回执命令，用于端到端确认事件桥闭环。
#[tauri::command]
fn ping_ack(payload: String) {
    println!("[bridge] frontend acknowledged ping: {payload}");
}

/// 检查 GitHub 最新 Release（仅 macOS 使用：当前构建未签名/未公证，不走
/// updater 插件内置下载，而是让前端弹确认框后打开 Releases 页手动下载）。
///
/// 返回 JSON 字符串 `{"url":"...","version":"N.N.N"}`（有新版时），
/// 无新版本 / 无法查询时返回空字符串（前端按「已是最新」处理）。
#[tauri::command]
fn check_latest_release(app: tauri::AppHandle) -> Result<String, String> {
    const API: &str = "https://api.github.com/repos/smallmj/talksee/releases/latest";
    let resp = ureq::get(API)
        .set("User-Agent", "talksee-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("查询最新版本失败: {e}"))?;
    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("解析 GitHub 响应失败: {e}"))?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches(['v', 'V'])
        .to_string();
    let url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com/smallmj/talksee/releases/latest")
        .to_string();
    let current = app.package_info().version.to_string();
    if tag.is_empty() || !is_newer_version(&tag, &current) {
        return Ok(String::new());
    }
    Ok(format!(r#"{{"url":"{url}","version":"{tag}"}}"#))
}

/// 逐段数字比较两个 `N.N.N` 版本号：a > b 返回 true（忽略前导 v）。
fn is_newer_version(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    let len = pa.len().max(pb.len());
    for i in 0..len {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

pub fn run() {
    tauri::Builder::default()
        // T13 单实例：重复启动时唤起已有主窗口，避免麦克风/ASR 被多实例竞争。
        // 链首注册，确保在任何窗口创建前生效。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        // T19 应用内自动更新：dialog（确认弹窗）/ process（更新后重启）/
        // opener（macOS 打开 GitHub Releases 页手动下载）。updater 仅在
        // Windows 走完整更新流程（macOS 未签名/未公证，禁用插件内置下载）。
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(pipeline::SessionControl::default());
            app.manage(asr::CorrectionState::default());
            app.manage(models::ModelDownloadRegistry::default());
            bridge::spawn_ping_emitter(app.handle());
            // 主事件源（T4 + T9 + T10 整合）：真实 ASR（sherpa-onnx + 麦克风）优先，
            // 失败回退合成转写演示模式；final 转写 → 整理管线 → 真实 LLM（SSE）
            // 流式整理；partial 实时 publish；停止后分批汇总会议纪要。事件流统一经
            // `engine://event` 推给前端。
            pipeline::spawn_engine_emitter(app.handle());
            // T13：托盘常驻 + 全局热键 + 会话状态镜像（在 SessionControl manage 之后）。
            tray::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // T13 关闭按钮 → 隐藏到托盘（不退出进程），真正退出走托盘菜单「退出」。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                crate::tray::log_tray(window.app_handle(), "CloseRequested -> prevent_close + hide");
                if let Err(err) = window.hide() {
                    crate::tray::log_tray(window.app_handle(), &format!("隐藏窗口失败: {err}"));
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ping_ack,
            check_latest_release,
            llm::load_llm_config,
            llm::save_llm_config,
            llm::list_llm_models,
            asr_config::load_asr_config,
            asr_config::save_asr_config,
            models::load_model_config,
            models::save_model_config,
            models::list_models,
            models::download_model_async,
            models::cancel_download,
            models::delete_model,
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
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| match event {
            // macOS：点 Dock 图标触发 `applicationShouldHandleReopen`（RunEvent::Reopen）。
            // 主窗口被隐藏（关闭→托盘）时 macOS 不会自动恢复隐藏窗口，只会激活应用，
            // 必须在这里主动唤回——否则会表现为「点 Dock 图标窗口打不开」。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows,
                ..
            } => {
                if !has_visible_windows {
                    tray::show_main_window(app_handle);
                }
            }
            _ => {}
        });
}
