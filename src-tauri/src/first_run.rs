//! 首次运行初始化：运行环境检测 + 所选模型下载 + 模式持久化（T14 + T16）。
//!
//! 生命周期由 `first-run.json` 的 `completed` 标记控制：
//! - 未完成：启动直接进入分步打钩向导，确认后才进主界面；
//! - 已完成：启动自检仍会跑（见 [run_first_run_setup]），模型/运行时缺失时
//!   可通过设置里的「重新运行初始化」重跑。
//!
//! T16 起模型选择与镜像统一归 `models::ModelConfig`（`model-config.json`）：
//! 本模块只负责运行时检测与把「所选模型」交给共享下载引擎（进度走
//! `model://progress`，见 `models::download_model`）。

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::asr_config::{self, AsrSource};
use crate::model_paths;
use crate::models;

/// 首启初始化进度事件名（与前端 `FirstRunWizard` 保持一致；现仅承载运行环境步骤）。
pub const FIRST_RUN_EVENT: &str = "first-run://progress";

const CONFIG_FILE: &str = "first-run.json";

/// 初始化模式：本地 ASR（下载模型）或云端 ASR（仅校验配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitMode {
    Local,
    Cloud,
}

/// 首启完成状态与上次模式选择（持久化到 app config 目录）。
///
/// 注意：镜像与模型选择已移入 `model-config.json`（T16），此处不再保存 mirror；
/// 旧文件里的 `mirror` 字段由 `models::read_legacy_mirror` 做一次性迁移读取。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FirstRunConfig {
    pub completed: bool,
    pub mode: InitMode,
}

impl Default for FirstRunConfig {
    fn default() -> Self {
        Self {
            completed: false,
            mode: InitMode::Local,
        }
    }
}

/// 向导步骤（现仅运行环境由本模块驱动；ASR / embedding 步骤进度走 `model://progress`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStep {
    Runtime,
    Asr,
    Embedding,
}

/// 步骤状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStatus {
    Running,
    Done,
    Failed,
}

/// 进度事件负载（Rust 侧类型化，前端 `FirstRunProgress` 与之对齐）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstRunProgress {
    pub step: ProgressStep,
    pub status: ProgressStatus,
    pub progress: f64,
    pub file: Option<usize>,
    pub file_count: Option<usize>,
    pub message: String,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| format!("无法获取 app config 目录: {e}"))
}

fn read_config(app: &AppHandle) -> FirstRunConfig {
    let Ok(path) = config_path(app) else {
        return FirstRunConfig::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return FirstRunConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_config(app: &AppHandle, config: &FirstRunConfig) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(config).map_err(|e| format!("序列化配置失败: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

#[tauri::command]
pub fn load_first_run_config(app: AppHandle) -> Result<FirstRunConfig, String> {
    Ok(read_config(&app))
}

/// 保存模式偏好（未完成也会记住，供下次启动回填向导）。镜像与模型选择由
/// `models::save_model_config` 持久化。
#[tauri::command]
pub fn save_first_run_preferences(app: AppHandle, mode: InitMode) -> Result<(), String> {
    let mut config = read_config(&app);
    config.mode = mode;
    write_config(&app, &config)
}

/// 确认初始化完成。云端模式要求现有 `asr-config.json` 已通过云端字段校验；
/// 本地模式除 UI 勾选外，后端也会复检运行时与所选 ASR 模型是否真实就绪。
#[tauri::command]
pub fn complete_first_run(app: AppHandle, mode: InitMode) -> Result<(), String> {
    let mut asr_config = asr_config::read_config(&app);
    match mode {
        InitMode::Local => {
            local_ready(&app)?;
            asr_config.source = AsrSource::Local;
        }
        InitMode::Cloud => {
            if let Some(reason) = asr_config.cloud_invalid_reason() {
                return Err(format!("云端 ASR 配置不完整：{reason}"));
            }
            asr_config.source = AsrSource::Cloud;
        }
    }
    asr_config::save_asr_config(app.clone(), asr_config)?;

    let config = FirstRunConfig {
        completed: true,
        mode,
    };
    write_config(&app, &config)
}

/// 设置里的「重新运行初始化」：仅清除完成标记，保留上次模式/模型/镜像选择。
#[tauri::command]
pub fn reset_first_run(app: AppHandle) -> Result<(), String> {
    let mut config = read_config(&app);
    config.completed = false;
    write_config(&app, &config)
}

/// 启动后台初始化任务（立即返回；运行环境步骤经 [FIRST_RUN_EVENT] 推送，
/// 模型下载进度经 `model://progress` 推送）。模型/镜像从 `model-config.json`
/// 读取（单一来源）。
#[tauri::command]
pub fn run_first_run_setup(
    app: AppHandle,
    asr_model_id: String,
    embedding_model_id: Option<String>,
) -> Result<(), String> {
    std::thread::spawn(move || run_setup(&app, asr_model_id, embedding_model_id));
    Ok(())
}

fn run_setup(app: &AppHandle, asr_model_id: String, embedding_model_id: Option<String>) {
    let model_cfg = models::read_config(app);
    let mirror = model_cfg.mirror;
    let auto_fallback = model_cfg.auto_fallback_mirror;

    if let Err(message) = check_runtime(app) {
        emit_progress(
            app,
            progress(
                ProgressStep::Runtime,
                ProgressStatus::Failed,
                0.0,
                None,
                None,
                message,
            ),
        );
        return;
    }
    emit_progress(
        app,
        progress(
            ProgressStep::Runtime,
            ProgressStatus::Done,
            1.0,
            None,
            None,
            "运行环境就绪",
        ),
    );

    if let Err(message) = models::download_model(app, &asr_model_id, mirror, auto_fallback) {
        emit_progress(
            app,
            progress(
                ProgressStep::Asr,
                ProgressStatus::Failed,
                0.0,
                None,
                None,
                message,
            ),
        );
        return;
    }

    if let Some(embedding_id) = embedding_model_id {
        if let Err(message) = models::download_model(app, &embedding_id, mirror, auto_fallback) {
            emit_progress(
                app,
                progress(
                    ProgressStep::Embedding,
                    ProgressStatus::Failed,
                    0.0,
                    None,
                    None,
                    message,
                ),
            );
        }
    } else {
        emit_progress(
            app,
            progress(
                ProgressStep::Embedding,
                ProgressStatus::Done,
                1.0,
                None,
                None,
                "已跳过说话人模型（SCD 降级为单说话人）",
            ),
        );
    }
}

/// 运行时健康检查（无进度事件；供向导与本地完成校验复用）。
fn runtime_ready(app: &AppHandle) -> Result<(), String> {
    let runtime = model_paths::runtime_paths(app)?;
    if !runtime.python.is_file() {
        return Err(format!(
            "找不到 sidecar Python：{}（开发模式请先运行 `pnpm run setup:runtime`）",
            runtime.python.display()
        ));
    }
    if !runtime.script.is_file() {
        return Err(format!("找不到 sidecar 脚本：{}", runtime.script.display()));
    }
    match Command::new(&runtime.python)
        .arg("-c")
        .arg("import sherpa_onnx, numpy")
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err("Python 依赖未就绪（sherpa-onnx / numpy 未安装）".to_string()),
        Err(e) => Err(format!("无法执行 Python：{e}")),
    }
}

fn check_runtime(app: &AppHandle) -> Result<(), String> {
    emit_progress(
        app,
        progress(
            ProgressStep::Runtime,
            ProgressStatus::Running,
            0.0,
            None,
            None,
            "正在检测 Python 运行环境…",
        ),
    );
    runtime_ready(app)
}

/// 本地分支完成门槛：运行时就绪 + 所选 ASR 模型就绪（embedding 可选，缺失降级）。
fn local_ready(app: &AppHandle) -> Result<(), String> {
    runtime_ready(app)?;
    models::selected_asr_dir(app)?;
    Ok(())
}

fn progress(
    step: ProgressStep,
    status: ProgressStatus,
    progress: f64,
    file: Option<usize>,
    file_count: Option<usize>,
    message: impl Into<String>,
) -> FirstRunProgress {
    FirstRunProgress {
        step,
        status,
        progress,
        file,
        file_count,
        message: message.into(),
    }
}

fn emit_progress(app: &AppHandle, event: FirstRunProgress) {
    let _ = app.emit(FIRST_RUN_EVENT, event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_not_completed_local() {
        let config = FirstRunConfig::default();
        assert!(!config.completed);
        assert_eq!(config.mode, InitMode::Local);
    }

    #[test]
    fn config_round_trips_camel_and_lowercase() {
        let config = FirstRunConfig {
            completed: true,
            mode: InitMode::Cloud,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""mode":"cloud""#));
        assert_eq!(serde_json::from_str::<FirstRunConfig>(&json).unwrap(), config);
    }

    #[test]
    fn legacy_first_run_with_mirror_still_parses() {
        // 旧文件含 mirror 字段；新结构忽略之。
        let parsed: FirstRunConfig =
            serde_json::from_str(r#"{"completed":true,"mode":"local","mirror":"hf-mirror"}"#)
                .unwrap();
        assert!(parsed.completed);
        assert_eq!(parsed.mode, InitMode::Local);
    }

    #[test]
    fn progress_event_serializes_camel_case() {
        let event = progress(
            ProgressStep::Asr,
            ProgressStatus::Running,
            0.5,
            Some(2),
            Some(5),
            "encoder 50%",
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""step":"asr""#));
        assert!(json.contains(r#""fileCount":5"#));
    }

}
