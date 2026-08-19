//! 常规应用设置（T12 设置系统：整理间隔等，持久化到 app config 目录）。
//!
//! 与 ASR/LLM/显示设置不同，这里放「不归任何单一模块」的常规设置在
//! `app-settings.json`：目前只有整理间隔（5s/10s 两档，规格 #21）。
//! 驱动线程每秒轮询本配置，档位变化时调用
//! [engine::CleanupPipeline::set_rhythm_duration]，无需重建整理管线，
//! 保存后即时生效。

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const CONFIG_FILE: &str = "app-settings.json";

/// 支持的整理间隔档位。
pub const CLEANUP_INTERVALS: [u8; 2] = [5, 10];

/// 常规应用设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// LLM 整理固定节奏（秒）：规格限定 5s / 10s 两档。
    pub cleanup_interval_seconds: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            cleanup_interval_seconds: CLEANUP_INTERVALS[0],
        }
    }
}

impl AppSettings {
    /// 档位是否合法（5 或 10）。
    pub fn is_valid_interval(seconds: u8) -> bool {
        CLEANUP_INTERVALS.contains(&seconds)
    }

    /// 当前档位对应的节奏时长；未知档位回退默认 5s（防御旧文件/手改损坏）。
    pub fn cleanup_interval(&self) -> Duration {
        Duration::from_secs(if Self::is_valid_interval(self.cleanup_interval_seconds) {
            self.cleanup_interval_seconds
        } else {
            CLEANUP_INTERVALS[0]
        } as u64)
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| format!("无法获取 app config 目录: {e}"))
}

/// 读取配置；文件不存在或损坏时返回默认配置，保证应用可启动。
pub(crate) fn read_config(app: &AppHandle) -> AppSettings {
    let Ok(path) = config_path(app) else {
        return AppSettings::default();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return AppSettings::default();
    };
    serde_json::from_str::<AppSettings>(&raw).unwrap_or_default()
}

#[tauri::command]
pub fn load_app_settings(app: AppHandle) -> Result<AppSettings, String> {
    Ok(read_config(&app))
}

#[tauri::command]
pub fn save_app_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    if !AppSettings::is_valid_interval(settings.cleanup_interval_seconds) {
        return Err(format!(
            "整理间隔必须是 {} 秒（当前为 {}）",
            CLEANUP_INTERVALS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" 或 "),
            settings.cleanup_interval_seconds
        ));
    }

    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| format!("序列化配置失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_5_seconds() {
        let cfg = AppSettings::default();
        assert_eq!(cfg.cleanup_interval_seconds, 5);
        assert_eq!(cfg.cleanup_interval(), Duration::from_secs(5));
    }

    #[test]
    fn round_trips_via_json_camel_case() {
        let cfg = AppSettings {
            cleanup_interval_seconds: 10,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, r#"{"cleanupIntervalSeconds":10}"#);
        let parsed: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
        assert_eq!(parsed.cleanup_interval(), Duration::from_secs(10));
    }

    #[test]
    fn legacy_json_missing_fields_uses_defaults() {
        let parsed: AppSettings = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(parsed.cleanup_interval_seconds, 5);
    }

    #[test]
    fn invalid_interval_falls_back_to_default_rhythm() {
        let cfg = AppSettings {
            cleanup_interval_seconds: 7,
        };
        assert!(!AppSettings::is_valid_interval(7));
        assert_eq!(cfg.cleanup_interval(), Duration::from_secs(5));
    }
}
