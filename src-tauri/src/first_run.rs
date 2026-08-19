//! 首次运行初始化：运行环境检测 + 模型下载 + 模式/镜像持久化。
//!
//! 生命周期由 `first-run.json` 的 `completed` 标记控制：
//! - 未完成：启动直接进入分步打钩向导，确认后才进主界面；
//! - 已完成：启动自检仍会跑（见 [run_first_run_setup]），模型/运行时缺失时
//!   可通过设置里的「重新运行初始化」重跑。
//!
//! 下载实现：原生 HTTP（ureq）流式拉取 + 进度事件 + `.part` 断点续传；
//! 镜像只换 base URL（HuggingFace 官方 / hf-mirror 国内镜像）。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::asr_config::{self, AsrSource};
use crate::model_paths;

/// 首启初始化进度事件名（与前端 `FirstRunWizard` 保持一致）。
pub const FIRST_RUN_EVENT: &str = "first-run://progress";

const CONFIG_FILE: &str = "first-run.json";
const MANIFEST_FILE: &str = ".download-manifest.json";

/// ASR 流式模型（中英标点 int8，2026-06-03 sherpa-onnx 导出）。
const ASR_REPO: &str =
    "csukuangfj2/sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03";
const ASR_DIR: &str = "sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03";
const ASR_FILES: [&str; 5] = [
    "encoder-epoch-99-avg-1.int8.onnx",
    "decoder-epoch-99-avg-1.onnx",
    "joiner-epoch-99-avg-1.int8.onnx",
    "bpe.model",
    "tokens.txt",
];

/// 说话人 speaker embedding 模型（T5 SCD 可选；缺失时降级单说话人）。
const EMBEDDING_REPO: &str = "csukuangfj/speaker-embedding-models";
const EMBEDDING_DIR: &str = "sherpa-onnx-3dspeaker-eres2net-base";
const EMBEDDING_FILES: [&str; 1] =
    ["3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"];

/// 初始化模式：本地 ASR（下载模型）或云端 ASR（仅校验配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitMode {
    Local,
    Cloud,
}

/// 模型下载镜像源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadMirror {
    Huggingface,
    HfMirror,
}

impl DownloadMirror {
    fn base_url(self) -> &'static str {
        match self {
            Self::Huggingface => "https://huggingface.co",
            Self::HfMirror => "https://hf-mirror.com",
        }
    }
}

/// 首启完成状态与上次选择（持久化到 app config 目录）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FirstRunConfig {
    pub completed: bool,
    pub mode: InitMode,
    pub mirror: DownloadMirror,
}

impl Default for FirstRunConfig {
    fn default() -> Self {
        Self {
            completed: false,
            mode: InitMode::Local,
            mirror: DownloadMirror::Huggingface,
        }
    }
}

/// 向导步骤。
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
    let Ok(raw) = fs::read_to_string(&path) else {
        return FirstRunConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_config(app: &AppHandle, config: &FirstRunConfig) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(config).map_err(|e| format!("序列化配置失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

#[tauri::command]
pub fn load_first_run_config(app: AppHandle) -> Result<FirstRunConfig, String> {
    Ok(read_config(&app))
}

/// 保存模式/镜像偏好（未完成也会记住，供下次启动回填向导）。
#[tauri::command]
pub fn save_first_run_preferences(
    app: AppHandle,
    mode: InitMode,
    mirror: DownloadMirror,
) -> Result<(), String> {
    let mut config = read_config(&app);
    config.mode = mode;
    config.mirror = mirror;
    write_config(&app, &config)
}

/// 确认初始化完成。云端模式要求现有 `asr-config.json` 已通过云端字段校验；
/// 本地模式除 UI 勾选外，后端也会复检运行时与 ASR 模型是否真实就绪。
#[tauri::command]
pub fn complete_first_run(
    app: AppHandle,
    mode: InitMode,
    mirror: DownloadMirror,
) -> Result<(), String> {
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
        mirror,
    };
    write_config(&app, &config)
}

/// 设置里的「重新运行初始化」：仅清除完成标记，保留上次模式/镜像选择。
#[tauri::command]
pub fn reset_first_run(app: AppHandle) -> Result<(), String> {
    let mut config = read_config(&app);
    config.completed = false;
    write_config(&app, &config)
}

/// 启动后台初始化任务（立即返回；进度经 [FIRST_RUN_EVENT] 事件推送）。
#[tauri::command]
pub fn run_first_run_setup(
    app: AppHandle,
    mirror: DownloadMirror,
    skip_embedding: bool,
) -> Result<(), String> {
    std::thread::spawn(move || run_setup(&app, mirror, skip_embedding));
    Ok(())
}

struct ModelGroup {
    step: ProgressStep,
    dir_name: &'static str,
    repo: &'static str,
    label: &'static str,
    files: &'static [&'static str],
}

const ASR_GROUP: ModelGroup = ModelGroup {
    step: ProgressStep::Asr,
    dir_name: ASR_DIR,
    repo: ASR_REPO,
    label: "ASR 识别模型",
    files: &ASR_FILES,
};

const EMBEDDING_GROUP: ModelGroup = ModelGroup {
    step: ProgressStep::Embedding,
    dir_name: EMBEDDING_DIR,
    repo: EMBEDDING_REPO,
    label: "说话人 embedding 模型",
    files: &EMBEDDING_FILES,
};

fn run_setup(app: &AppHandle, mirror: DownloadMirror, skip_embedding: bool) {
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

    if let Err(message) = download_group(app, mirror, &ASR_GROUP) {
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

    if skip_embedding {
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
    } else if let Err(message) = download_group(app, mirror, &EMBEDDING_GROUP) {
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

/// 本地分支完成门槛：运行时就绪 + 模型根目录下存在完整 ASR 模型目录。
fn local_ready(app: &AppHandle) -> Result<(), String> {
    runtime_ready(app)?;
    let root = model_paths::model_root(app)?;
    let ready = std::fs::read_dir(&root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir() && p.join("tokens.txt").is_file() && p.join("bpe.model").is_file()
                })
                .any(|dir| {
                    ["encoder", "decoder", "joiner"]
                        .iter()
                        .all(|prefix| has_onnx_with_prefix(&dir, prefix))
                })
        })
        .unwrap_or(false);
    if !ready {
        return Err("本地 ASR 模型未就绪（请先在向导中下载 ASR 模型）".to_string());
    }
    Ok(())
}

fn has_onnx_with_prefix(dir: &Path, prefix: &str) -> bool {
    dir.read_dir().map_or(false, |mut it| {
        it.any(|entry| {
            entry.map_or(false, |e| {
                let file_name = e.file_name();
                let name = file_name.to_string_lossy();
                name.starts_with(prefix)
                    && e.path().extension().is_some_and(|ext| ext.to_string_lossy() == "onnx")
            })
        })
    })
}

/// 模型目录下载清单：记录每个文件首次成功下载的大小，重跑时按大小幂等校验。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct DownloadManifest {
    files: HashMap<String, u64>,
}

fn read_manifest(dir: &Path) -> DownloadManifest {
    let Ok(raw) = fs::read_to_string(dir.join(MANIFEST_FILE)) else {
        return DownloadManifest::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_manifest(dir: &Path, manifest: &DownloadManifest) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(manifest).map_err(|e| format!("序列化下载清单失败: {e}"))?;
    fs::write(dir.join(MANIFEST_FILE), json).map_err(|e| format!("写入下载清单失败: {e}"))
}

fn download_group(
    app: &AppHandle,
    mirror: DownloadMirror,
    group: &ModelGroup,
) -> Result<(), String> {
    let root = model_paths::model_root(app)?;
    let dir = root.join(group.dir_name);
    fs::create_dir_all(&dir).map_err(|e| format!("创建模型目录失败 {dir:?}: {e}"))?;
    let count = group.files.len();
    emit_progress(
        app,
        progress(
            group.step,
            ProgressStatus::Running,
            0.0,
            Some(0),
            Some(count),
            format!("准备下载 {}…", group.label),
        ),
    );

    let mut manifest = read_manifest(&dir);
    // 首次运行但模型目录已手工就绪：用当前大小初始化清单，避免重复下载。
    if manifest.files.is_empty()
        && group.files.iter().all(|file| {
            dir.join(file).is_file()
                && dir.join(file).metadata().map(|m| m.len() > 0).unwrap_or(false)
        })
    {
        for file in group.files {
            if let Ok(len) = file_len(&dir.join(file)) {
                manifest.files.insert(file.to_string(), len);
            }
        }
        write_manifest(&dir, &manifest)?;
    }

    for (index, file) in group.files.iter().enumerate() {
        let target = dir.join(file);
        let len = file_len(&target)?;
        if target.is_file() && len > 0 && manifest.files.get(*file) == Some(&len) {
            emit_progress(
                app,
                progress(
                    group.step,
                    ProgressStatus::Running,
                    (index + 1) as f64 / count as f64,
                    Some(index + 1),
                    Some(count),
                    format!("{} 已存在，跳过", file),
                ),
            );
            continue;
        }
        let url = format!(
            "{}/{}/resolve/main/{}",
            mirror.base_url(),
            group.repo,
            file
        );
        download_file(app, group.step, &url, &target, index + 1, count, file)?;
        manifest.files.insert(file.to_string(), file_len(&target)?);
        write_manifest(&dir, &manifest)?;
    }

    emit_progress(
        app,
        progress(
            group.step,
            ProgressStatus::Done,
            1.0,
            Some(count),
            Some(count),
            format!("{} 下载完成", group.label),
        ),
    );
    Ok(())
}

fn download_file(
    app: &AppHandle,
    step: ProgressStep,
    url: &str,
    target: &Path,
    index: usize,
    count: usize,
    file_name: &str,
) -> Result<(), String> {
    let part = part_path(target);
    let existing = file_len(&part)?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180))
        .build();
    let mut request = agent.get(url);
    if existing > 0 {
        request = request.set("Range", &format!("bytes={existing}-"));
    }
    let response = request
        .call()
        .map_err(|e| format!("下载 {file_name} 请求失败: {e}"))?;
    let status = response.status();
    let (offset, total) = match status {
        206 => {
            let remaining = content_length(&response);
            (existing, existing + remaining)
        }
        200 => (0, content_length(&response)),
        _ => return Err(format!("下载 {file_name} 失败：HTTP {status}")),
    };

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(&part)
        .map_err(|e| format!("创建下载文件失败 {part:?}: {e}"))?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("定位下载文件失败: {e}"))?;
    }

    let mut reader = response.into_reader();
    let mut buffer = [0u8; 64 * 1024];
    let mut done = offset;
    let mut last_percent = -1i32;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("读取 {file_name} 失败: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])
            .map_err(|e| format!("写入 {file_name} 失败: {e}"))?;
        done += n as u64;

        if total > 0 {
            let percent = ((done * 100) / total) as i32;
            if percent != last_percent {
                last_percent = percent;
                let step_progress =
                    ((index as f64 - 1.0) + percent as f64 / 100.0) / count as f64;
                emit_progress(
                    app,
                    progress(
                        step,
                        ProgressStatus::Running,
                        step_progress,
                        Some(index),
                        Some(count),
                        format!("{file_name} {percent}%"),
                    ),
                );
            }
        } else if done % (2 * 1024 * 1024) < buffer.len() as u64 {
            emit_progress(
                app,
                progress(
                    step,
                    ProgressStatus::Running,
                    index as f64 / count as f64,
                    Some(index),
                    Some(count),
                    format!("{file_name} {} MB", done / 1024 / 1024),
                ),
            );
        }
    }

    file.sync_all()
        .map_err(|e| format!("同步 {file_name} 失败: {e}"))?;
    fs::rename(&part, target).map_err(|e| format!("完成 {file_name} 失败: {e}"))?;
    emit_progress(
        app,
        progress(
            step,
            ProgressStatus::Running,
            index as f64 / count as f64,
            Some(index),
            Some(count),
            format!("{file_name} 完成"),
        ),
    );
    Ok(())
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn file_len(path: &Path) -> Result<u64, String> {
    match path.metadata() {
        Ok(m) if m.is_file() => Ok(m.len()),
        _ => Ok(0),
    }
}

fn content_length(response: &ureq::Response) -> u64 {
    response
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
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
        assert_eq!(config.mirror, DownloadMirror::Huggingface);
    }

    #[test]
    fn config_round_trips_camel_and_kebab() {
        let config = FirstRunConfig {
            completed: true,
            mode: InitMode::Cloud,
            mirror: DownloadMirror::HfMirror,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""mode":"cloud""#));
        assert!(json.contains(r#""mirror":"hf-mirror""#));
        assert_eq!(serde_json::from_str::<FirstRunConfig>(&json).unwrap(), config);
    }

    #[test]
    fn mirror_base_urls() {
        assert_eq!(DownloadMirror::Huggingface.base_url(), "https://huggingface.co");
        assert_eq!(DownloadMirror::HfMirror.base_url(), "https://hf-mirror.com");
    }

    #[test]
    fn part_path_appends_suffix() {
        let path = part_path(Path::new("/tmp/encoder.onnx"));
        assert_eq!(path, PathBuf::from("/tmp/encoder.onnx.part"));
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
