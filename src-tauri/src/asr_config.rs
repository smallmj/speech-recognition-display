//! ASR 来源配置（T7：本地 / 云端可切换）。
//!
//! 配置以明文 JSON 保存在 app config 目录（`asr-config.json`）。云端 API Key
//! 明文存储与 T9 LLM 配置保持同一 MVP 取舍；云端字段当前对应 Deepgram 兼容
//! 流式 WebSocket 协议，端点可在设置中覆盖。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const CONFIG_FILE: &str = "asr-config.json";

/// ASR 来源。Rust 命名保持领域术语，serde 使用 camelCase 与前端契约对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AsrSource {
    /// 本地离线 ASR（sherpa-onnx）。
    Local,
    /// 云端流式 ASR（Deepgram 兼容 WebSocket）。
    Cloud,
}

/// ASR 配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AsrConfig {
    /// 当前选择：local / cloud。
    pub source: AsrSource,
    /// 云端 WebSocket 端点（不含本客户端附加的查询参数）。
    pub cloud_endpoint: String,
    /// 云端 API Key。
    pub cloud_api_key: String,
    /// 云端模型名。
    pub cloud_model: String,
    /// 云端语言代码；默认 `multi` 覆盖中英混合识别。
    pub cloud_language: String,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            source: AsrSource::Local,
            cloud_endpoint: "wss://api.deepgram.com/v1/listen".to_string(),
            cloud_api_key: String::new(),
            cloud_model: "nova-3".to_string(),
            cloud_language: "multi".to_string(),
        }
    }
}

impl AsrConfig {
    /// 实际可启动的来源：云端字段不完整时降级为本地，避免驱动线程反复启动失败。
    pub fn effective_source(&self) -> AsrSource {
        match self.source {
            AsrSource::Cloud if self.cloud_invalid_reason().is_none() => AsrSource::Cloud,
            _ => AsrSource::Local,
        }
    }

    /// 云端配置不可用时的原因；配置完整返回 `None`。
    pub fn cloud_invalid_reason(&self) -> Option<String> {
        if self.cloud_endpoint.trim().is_empty() {
            return Some("云端 ASR 端点为空".to_string());
        }
        if !self.cloud_endpoint.trim_start().starts_with("ws://")
            && !self.cloud_endpoint.trim_start().starts_with("wss://")
        {
            return Some("云端 ASR 端点必须是 ws:// 或 wss://".to_string());
        }
        if self.cloud_api_key.trim().is_empty() {
            return Some("云端 ASR API Key 为空".to_string());
        }
        if self.cloud_model.trim().is_empty() {
            return Some("云端 ASR 模型为空".to_string());
        }
        if self.cloud_language.trim().is_empty() {
            return Some("云端 ASR 语言为空".to_string());
        }
        None
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| format!("无法获取 app config 目录: {e}"))
}

/// 读取配置；文件不存在或损坏时返回默认配置，保证应用可启动。
pub(crate) fn read_config(app: &AppHandle) -> AsrConfig {
    let Ok(path) = config_path(app) else {
        return AsrConfig::default();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return AsrConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

#[tauri::command]
pub fn load_asr_config(app: AppHandle) -> Result<AsrConfig, String> {
    Ok(read_config(&app))
}

#[tauri::command]
pub fn save_asr_config(app: AppHandle, config: AsrConfig) -> Result<(), String> {
    if let Some(reason) = config.cloud_invalid_reason() {
        if config.source == AsrSource::Cloud {
            return Err(reason);
        }
    }

    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| format!("序列化配置失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_local_and_deepgram_cloud_defaults() {
        let cfg = AsrConfig::default();
        assert_eq!(cfg.source, AsrSource::Local);
        assert_eq!(cfg.cloud_endpoint, "wss://api.deepgram.com/v1/listen");
        assert_eq!(cfg.cloud_model, "nova-3");
        assert_eq!(cfg.cloud_language, "multi");
        assert_eq!(cfg.effective_source(), AsrSource::Local);
    }

    #[test]
    fn cloud_config_round_trips_and_takes_effect() {
        let cfg = AsrConfig {
            source: AsrSource::Cloud,
            cloud_api_key: "secret".to_string(),
            ..AsrConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: AsrConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
        assert_eq!(parsed.effective_source(), AsrSource::Cloud);
    }

    #[test]
    fn incomplete_cloud_config_falls_back_to_local() {
        let cfg = AsrConfig {
            source: AsrSource::Cloud,
            ..AsrConfig::default()
        };
        assert_eq!(cfg.effective_source(), AsrSource::Local);
        assert_eq!(
            cfg.cloud_invalid_reason().as_deref(),
            Some("云端 ASR API Key 为空")
        );
    }

    #[test]
    fn legacy_json_missing_fields_uses_defaults() {
        let parsed: AsrConfig = serde_json::from_str(r#"{"source":"cloud"}"#).unwrap();
        assert_eq!(parsed.source, AsrSource::Cloud);
        assert_eq!(parsed.cloud_model, "nova-3");
    }
}
