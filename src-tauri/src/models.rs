//! 模型目录 manifest + `model-config.json` + 共享下载引擎（T16）。
//!
//! 单一来源：模型目录（哪个 ASR / 哪个说话人 embedding）与下载镜像选择统一
//! 存放在 `model-config.json`，初始化向导与设置「模型」页读写同一份；运行时
//! 不再按目录名自动探测，改为「按配置的模型 id 解析 + 校验文件存在」。
//!
//! 下载引擎从 `first_run.rs` 泛化而来：原生 HTTP（ureq）+ `.part` 断点续传 +
//! 每模型 `.download-manifest.json` 幂等校验 + 进度事件（`model://progress`）
//! + 可取消（取消保留 `.part`，下次续传）+ 镜像自动回退（HF ↔ hf-mirror）。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::model_paths;

/// 模型下载进度事件名（前端「模型」页与初始化向导共用）。
pub const MODEL_PROGRESS_EVENT: &str = "model://progress";

const CONFIG_FILE: &str = "model-config.json";
const LEGACY_FIRST_RUN_FILE: &str = "first-run.json";
const MANIFEST_FILE: &str = ".download-manifest.json";

/// 取消下载的哨兵错误前缀：`.part` 保留，下次续传。
const CANCELLED_PREFIX: &str = "[cancelled]";

/// 下载镜像源（国内 / 国际）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadMirror {
    Huggingface,
    HfMirror,
}

impl DownloadMirror {
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Huggingface => "https://huggingface.co",
            Self::HfMirror => "https://hf-mirror.com",
        }
    }

    /// 自动回退时切换到的另一镜像。
    pub fn alternate(self) -> Self {
        match self {
            Self::Huggingface => Self::HfMirror,
            Self::HfMirror => Self::Huggingface,
        }
    }

    /// 解析字符串表示（旧配置/迁移用）；未知值按官方镜像处理。
    pub fn parse(s: &str) -> Self {
        match s {
            "hf-mirror" => Self::HfMirror,
            _ => Self::Huggingface,
        }
    }
}

/// 模型类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Asr,
    Embedding,
}

/// 模型的面向用户说明（写死进 manifest，避免依赖网络）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescription {
    /// 适配语言与场景。
    pub languages: String,
    /// 实时性说明（延迟档位 / RTF）。
    pub realtime: String,
    /// 最低硬件与存储说明（CPU / 内存 / 线程）。
    pub min_hardware: String,
    /// 许可证。
    pub license: String,
    /// 适用平台。
    pub platforms: String,
    /// 补充备注。
    pub notes: String,
}

/// 目录条目（manifest 项）。serde camelCase 对齐前端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub kind: ModelKind,
    pub display_name: String,
    /// HuggingFace 仓库（`owner/repo`）。
    pub repo: String,
    /// 模型根目录 `asr-models/` 下的子目录名。
    pub dir_name: String,
    pub files: Vec<String>,
    /// 各文件字节数之和（显示下载大小）。
    pub size_bytes: u64,
    /// 同类默认项标记。
    pub default: bool,
    pub description: ModelDescription,
}

/// 目录条目 + 本机下载状态（list_models 返回给前端）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub kind: ModelKind,
    pub display_name: String,
    pub dir_name: String,
    pub size_bytes: u64,
    pub default: bool,
    /// 所有预期文件已存在且非空。
    pub downloaded: bool,
    /// 已下载文件数（未全部下载时用于展示进度）。
    pub downloaded_files: usize,
    pub file_count: usize,
    pub description: ModelDescription,
}

// ---------------------------------------------------------------------------
// 模型目录 manifest（硬编码）
// ---------------------------------------------------------------------------

fn desc(
    languages: &str,
    realtime: &str,
    min_hardware: &str,
    license: &str,
    platforms: &str,
    notes: &str,
) -> ModelDescription {
    ModelDescription {
        languages: languages.to_string(),
        realtime: realtime.to_string(),
        min_hardware: min_hardware.to_string(),
        license: license.to_string(),
        platforms: platforms.to_string(),
        notes: notes.to_string(),
    }
}

fn entry(
    id: &str,
    kind: ModelKind,
    display_name: &str,
    repo: &str,
    dir_name: &str,
    files: &[&str],
    size_bytes: u64,
    default: bool,
    description: ModelDescription,
) -> ModelEntry {
    ModelEntry {
        id: id.to_string(),
        kind,
        display_name: display_name.to_string(),
        repo: repo.to_string(),
        dir_name: dir_name.to_string(),
        files: files.iter().map(|s| s.to_string()).collect(),
        size_bytes,
        default,
        description,
    }
}

/// 内置模型目录。数据源：2026-08 实测（见 issue #27）。
///
/// 说明：规格（issue #27）原列 3 个 ASR；`sherpa-onnx-x-asr-960ms-...-2026-06-05`
/// 的 sherpa-onnx int8 导出文件经核实**没有公开可下载来源**（HuggingFace 各 org、
/// GitHub 均无此精确产物；本地 README 指向的 `Gilgamesh-J/X-ASR` GitHub 仓库只有
/// 不同格式的 `encoder-960ms.onnx` 等原生 ONNX，缺 `bpe.model`，sidecar 无法可靠
/// 加载），故从可下载目录中移除，避免「能下载但识别不可用」。本地已有该目录的开发
/// /CI 场景仍可用 `SHERPA_MODEL_DIR` 环境变量覆盖（见 `asr.rs`）。
pub fn all_models() -> &'static [ModelEntry] {
    static MODELS: OnceLock<Vec<ModelEntry>> = OnceLock::new();
    MODELS.get_or_init(|| vec![
        // ---- ASR（sherpa-onnx 流式转写）----
        entry(
            "sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03",
            ModelKind::Asr,
            "zipformer 中英标点（2026-06-03）",
            "csukuangfj2/sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03",
            "sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03",
            &[
                "encoder-epoch-99-avg-1.int8.onnx",
                "decoder-epoch-99-avg-1.onnx",
                "joiner-epoch-99-avg-1.int8.onnx",
                "bpe.model",
                "tokens.txt",
            ],
            175_813_027,
            true,
            desc(
                "中文为主，中英混合，含标点",
                "标准流式（边说边出字），低延迟",
                "双核 CPU + 4GB 内存即可；2 线程推理",
                "Apache-2.0（以官方模型卡为准）",
                "macOS / Windows（纯 CPU，无需 GPU）",
                "2026-06 新版，识别质量与标点完善，默认推荐",
            ),
        ),
        entry(
            "sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20",
            ModelKind::Asr,
            "zipformer 中英双语（2023-02-20）",
            "csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20",
            "sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20",
            &[
                "encoder-epoch-99-avg-1.int8.onnx",
                "decoder-epoch-99-avg-1.onnx",
                "joiner-epoch-99-avg-1.int8.onnx",
                "bpe.model",
                "tokens.txt",
            ],
            199_301_041,
            false,
            desc(
                "中英双语",
                "流式（边说边出字）",
                "双核 CPU + 4GB 内存即可；2 线程推理",
                "Apache-2.0",
                "macOS / Windows（纯 CPU，无需 GPU）",
                "较早期模型（2023），文件略大，作为备选",
            ),
        ),
        // ---- 说话人 speaker embedding（T5 SCD，可选）----
        entry(
            "sherpa-onnx-3dspeaker-eres2netv2-base",
            ModelKind::Embedding,
            "3d-speaker eres2netv2（推荐）",
            "csukuangfj/speaker-embedding-models",
            "sherpa-onnx-3dspeaker-eres2netv2-base",
            &["3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx"],
            71_441_526,
            true,
            desc(
                "中文说话人 embedding",
                "每句一次向量提取",
                "单核 CPU 即可；内存占用低",
                "Apache-2.0（以官方模型卡为准）",
                "macOS / Windows（纯 CPU）",
                "短句区分度更好（实测），默认推荐",
            ),
        ),
        entry(
            "sherpa-onnx-3dspeaker-eres2net-base",
            ModelKind::Embedding,
            "3d-speaker eres2net-base",
            "csukuangfj/speaker-embedding-models",
            "sherpa-onnx-3dspeaker-eres2net-base",
            &["3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"],
            39_593_761,
            false,
            desc(
                "中文说话人 embedding",
                "每句一次向量提取",
                "单核 CPU 即可；内存占用低",
                "Apache-2.0（以官方模型卡为准）",
                "macOS / Windows（纯 CPU）",
                "尺寸更小（约 38MB），短句区分度略逊于 eres2netv2",
            ),
        ),
        entry(
            "sherpa-onnx-wespeaker-zh-cnceleb-resnet34",
            ModelKind::Embedding,
            "Wespeaker resnet34（中文）",
            "csukuangfj/speaker-embedding-models",
            "sherpa-onnx-wespeaker-zh-cnceleb-resnet34",
            &["wespeaker_zh_cnceleb_resnet34.onnx"],
            26_534_363,
            false,
            desc(
                "中文说话人 embedding（CN-Celeb 训练）",
                "每句一次向量提取",
                "单核 CPU 即可；内存占用低",
                "Apache-2.0（以官方模型卡为准）",
                "macOS / Windows（纯 CPU）",
                "输出 256 维（与 eres2net 的 192 维不同，跨会话切换安全）；已由协作方验证",
            ),
        ),
    ])
}

pub fn find_model(id: &str) -> Option<&'static ModelEntry> {
    all_models().iter().find(|e| e.id == id)
}

pub fn models_by_kind(kind: ModelKind) -> impl Iterator<Item = &'static ModelEntry> {
    all_models().iter().filter(move |e| e.kind == kind)
}

pub fn default_model(kind: ModelKind) -> &'static ModelEntry {
    all_models()
        .iter()
        .find(|e| e.kind == kind && e.default)
        .unwrap_or_else(|| all_models().iter().find(|e| e.kind == kind).expect("每类至少一个模型"))
}

// ---------------------------------------------------------------------------
// 下载状态判定
// ---------------------------------------------------------------------------

/// 一个模型是否已全部下载（所有预期文件存在且非空）。
fn model_ready(entry: &ModelEntry, root: &Path) -> bool {
    entry
        .files
        .iter()
        .all(|f| file_len(&root.join(&entry.dir_name).join(f)).map(|l| l > 0).unwrap_or(false))
}

fn downloaded_file_count(entry: &ModelEntry, root: &Path) -> usize {
    entry
        .files
        .iter()
        .filter(|f| file_len(&root.join(&entry.dir_name).join(f)).map(|l| l > 0).unwrap_or(false))
        .count()
}

pub fn model_info(entry: &ModelEntry, root: &Path) -> ModelInfo {
    ModelInfo {
        id: entry.id.clone(),
        kind: entry.kind,
        display_name: entry.display_name.clone(),
        dir_name: entry.dir_name.clone(),
        size_bytes: entry.size_bytes,
        default: entry.default,
        downloaded: model_ready(entry, root),
        downloaded_files: downloaded_file_count(entry, root),
        file_count: entry.files.len(),
        description: entry.description.clone(),
    }
}

/// 所选 ASR 模型的目录（若就绪）。缺失返回错误（调用方据此阻塞开始并引导下载）。
pub fn selected_asr_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let config = read_config(app);
    let entry = find_model(&config.asr_model)
        .ok_or_else(|| format!("未知的 ASR 模型：{}（请到设置重新选择）", config.asr_model))?;
    let root = model_paths::model_root(app)?;
    let dir = root.join(&entry.dir_name);
    if !model_ready(entry, &root) {
        return Err(format!(
            "所选 ASR 模型未下载：{}（请到设置「模型」页下载）",
            entry.display_name
        ));
    }
    Ok(dir)
}

/// 所选说话人 embedding 模型目录（可选；None = 未启用/未配置，SCD 降级单说话人）。
pub fn selected_embedding_dir(app: &AppHandle) -> Option<PathBuf> {
    let config = read_config(app);
    let entry = find_model(&config.embedding_model?)?;
    let root = model_paths::model_root(app).ok()?;
    let dir = root.join(&entry.dir_name);
    if !model_ready(entry, &root) {
        return None;
    }
    Some(dir)
}

// ---------------------------------------------------------------------------
// `model-config.json`：模型选择 + 镜像（单一来源）
// ---------------------------------------------------------------------------

/// 模型配置（初始化向导与设置页共享）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelConfig {
    pub asr_model: String,
    /// None = 不启用说话人区分（SCD 降级单说话人）。
    pub embedding_model: Option<String>,
    pub mirror: DownloadMirror,
    pub auto_fallback_mirror: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            asr_model: default_model(ModelKind::Asr).id.clone(),
            embedding_model: Some(default_model(ModelKind::Embedding).id.clone()),
            mirror: DownloadMirror::HfMirror,
            auto_fallback_mirror: true,
        }
    }
}

impl ModelConfig {
    fn validate(&self) -> Result<(), String> {
        if find_model(&self.asr_model).is_none() {
            return Err(format!("未知的 ASR 模型：{}", self.asr_model));
        }
        if let Some(id) = &self.embedding_model {
            if find_model(id).is_none() {
                return Err(format!("未知的说话人模型：{id}"));
            }
        }
        Ok(())
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| format!("无法获取 app config 目录: {e}"))
}

fn read_legacy_mirror(app: &AppHandle) -> Option<DownloadMirror> {
    let dir = app.path().app_config_dir().ok()?;
    let raw = fs::read_to_string(dir.join(LEGACY_FIRST_RUN_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("mirror")
        .and_then(|v| v.as_str())
        .map(DownloadMirror::parse)
}

/// 读取配置；文件缺失时按存量迁移规则生成（不写盘，确定性计算）。
pub fn read_config(app: &AppHandle) -> ModelConfig {
    let Ok(path) = config_path(app) else {
        return migrate_config(app);
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return migrate_config(app);
    };
    let parsed: ModelConfig = serde_json::from_str(&raw).unwrap_or_else(|_| migrate_config(app));
    // 防御：旧/损坏配置里的未知模型 id（asr 或 embedding）回退迁移结果。
    if parsed.validate().is_err() {
        return migrate_config(app);
    }
    parsed
}

/// 存量迁移：镜像沿用旧 first-run.json；模型选择优先磁盘上已下载的同类
/// manifest 模型，否则落到默认值（升级后不被「缺模型」卡住）。
fn migrate_config(app: &AppHandle) -> ModelConfig {
    let mut cfg = ModelConfig::default();
    if let Some(mirror) = read_legacy_mirror(app) {
        cfg.mirror = mirror;
    }
    if let Ok(root) = model_paths::model_root(app) {
        cfg.asr_model = prefer_downloaded(ModelKind::Asr, &cfg.asr_model, &root);
        cfg.embedding_model = prefer_downloaded_embedding(&cfg.embedding_model, &root);
    }
    cfg
}

fn prefer_downloaded(kind: ModelKind, fallback: &str, root: &Path) -> String {
    let downloaded: Vec<_> = models_by_kind(kind)
        .filter(|e| model_ready(e, root))
        .map(|e| e.id.clone())
        .collect();
    if downloaded.contains(&fallback.to_string()) {
        return fallback.to_string();
    }
    downloaded.into_iter().next().unwrap_or_else(|| fallback.to_string())
}

fn prefer_downloaded_embedding(fallback: &Option<String>, root: &Path) -> Option<String> {
    match fallback {
        Some(id) => Some(prefer_downloaded(ModelKind::Embedding, id, root)),
        None => {
            // 旧版未启用 embedding 时保持不启用（尊重旧选择）。
            None
        }
    }
}

#[tauri::command]
pub fn load_model_config(app: AppHandle) -> Result<ModelConfig, String> {
    Ok(read_config(&app))
}

#[tauri::command]
pub fn save_model_config(app: AppHandle, config: ModelConfig) -> Result<(), String> {
    config.validate()?;
    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| format!("序列化配置失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

#[tauri::command]
pub fn list_models(app: AppHandle) -> Result<Vec<ModelInfo>, String> {
    let root = model_paths::model_root(&app)?;
    Ok(all_models().iter().map(|e| model_info(e, &root)).collect())
}

// ---------------------------------------------------------------------------
// 下载引擎（共享、可取消、镜像自动回退）
// ---------------------------------------------------------------------------

/// 下载进度状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelProgressStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// 进度事件负载（前端「模型」页与初始化向导共用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProgress {
    pub model_id: String,
    pub status: ModelProgressStatus,
    /// 整体进度 0..1（跨文件累计）。
    pub progress: f64,
    pub file: Option<usize>,
    pub file_count: Option<usize>,
    pub message: String,
}

/// 取消注册表（tauri 托管状态）：model_id -> 取消标志。
#[derive(Default)]
pub struct ModelDownloadRegistry {
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl ModelDownloadRegistry {
    fn register(&self, model_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .unwrap()
            .insert(model_id.to_string(), Arc::clone(&flag));
        flag
    }

    fn unregister(&self, model_id: &str) {
        self.cancels.lock().unwrap().remove(model_id);
    }

    fn request_cancel(&self, model_id: &str) -> bool {
        self.cancels
            .lock()
            .unwrap()
            .get(model_id)
            .map(|f| f.store(true, Ordering::Relaxed))
            .is_some()
    }
}

fn emit_progress(app: &AppHandle, event: &ModelProgress) {
    let _ = app.emit(MODEL_PROGRESS_EVENT, event.clone());
}

fn progress(
    model_id: &str,
    status: ModelProgressStatus,
    progress: f64,
    file: Option<usize>,
    file_count: Option<usize>,
    message: impl Into<String>,
) -> ModelProgress {
    ModelProgress {
        model_id: model_id.to_string(),
        status,
        progress,
        file,
        file_count,
        message: message.into(),
    }
}

/// 取消标志是否已置位。
fn is_cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

/// 按镜像与是否自动回退生成尝试顺序（纯函数，可单测）。
fn mirror_candidates(mirror: DownloadMirror, auto_fallback: bool) -> Vec<DownloadMirror> {
    if auto_fallback {
        vec![mirror, mirror.alternate()]
    } else {
        vec![mirror]
    }
}

fn download_url(mirror: DownloadMirror, repo: &str, file: &str) -> String {
    format!("{}/{}/resolve/main/{}", mirror.base_url(), repo, file)
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

/// 下载一个模型（阻塞）。进度经 `model://progress` 事件推送；取消经注册表。
pub fn download_model(
    app: &AppHandle,
    model_id: &str,
    mirror: DownloadMirror,
    auto_fallback: bool,
) -> Result<(), String> {
    let entry = find_model(model_id).ok_or_else(|| format!("未知模型：{model_id}"))?;
    let root = model_paths::model_root(app)?;
    let dir = root.join(&entry.dir_name);
    fs::create_dir_all(&dir).map_err(|e| format!("创建模型目录失败 {dir:?}: {e}"))?;
    let count = entry.files.len();

    emit_progress(
        app,
        &progress(
            model_id,
            ModelProgressStatus::Running,
            0.0,
            Some(0),
            Some(count),
            format!("准备下载 {}…", entry.display_name),
        ),
    );

    let mut manifest = read_manifest(&dir);
    // 首次运行但模型目录已手工就绪：用当前大小初始化清单，避免重复下载。
    if manifest.files.is_empty()
        && entry.files.iter().all(|file| {
            dir.join(file).is_file()
                && dir.join(file).metadata().map(|m| m.len() > 0).unwrap_or(false)
        })
    {
        for file in &entry.files {
            if let Ok(len) = file_len(&dir.join(file)) {
                manifest.files.insert(file.clone(), len);
            }
        }
        write_manifest(&dir, &manifest)?;
    }

    let registry = app.state::<ModelDownloadRegistry>().inner();
    let cancel = registry.register(model_id);
    let result = (|| {
        for (index, file) in entry.files.iter().enumerate() {
            if is_cancelled(&cancel) {
                emit_progress(
                    app,
                    &progress(
                        model_id,
                        ModelProgressStatus::Cancelled,
                        index as f64 / count as f64,
                        Some(index),
                        Some(count),
                        "下载已取消（.part 保留，下次续传）",
                    ),
                );
                return Err(format!("{CANCELLED_PREFIX}下载已取消"));
            }
            let target = dir.join(file);
            let len = file_len(&target)?;
            if target.is_file() && len > 0 && manifest.files.get(file) == Some(&len) {
                emit_progress(
                    app,
                    &progress(
                        model_id,
                        ModelProgressStatus::Running,
                        (index + 1) as f64 / count as f64,
                        Some(index + 1),
                        Some(count),
                        format!("{file} 已存在，跳过"),
                    ),
                );
                continue;
            }

            // 镜像自动回退：主镜像失败时用另一镜像重试一次；两个都失败则合并报错。
            let mut errors: Vec<String> = Vec::new();
            for attempt_mirror in mirror_candidates(mirror, auto_fallback) {
                let url = download_url(attempt_mirror, &entry.repo, file);
                match download_file(app, model_id, &url, &target, index, count, file, &cancel) {
                    Ok(()) => {
                        errors.clear();
                        break;
                    }
                    Err(e) if e.starts_with(CANCELLED_PREFIX) => {
                        emit_progress(
                            app,
                            &progress(
                                model_id,
                                ModelProgressStatus::Cancelled,
                                index as f64 / count as f64,
                                Some(index),
                                Some(count),
                                "下载已取消（.part 保留，下次续传）",
                            ),
                        );
                        return Err(e);
                    }
                    Err(e) => {
                        errors.push(e);
                    }
                }
            }
            if let Some(e) = errors.last() {
                let combined = errors.join("；");
                emit_progress(
                    app,
                    &progress(
                        model_id,
                        ModelProgressStatus::Failed,
                        index as f64 / count as f64,
                        Some(index),
                        Some(count),
                        format!("下载 {file} 失败：{combined}"),
                    ),
                );
                return Err(combined);
            }
            manifest.files.insert(file.clone(), file_len(&target)?);
            write_manifest(&dir, &manifest)?;
        }
        Ok(())
    })();
    registry.unregister(model_id);

    match &result {
        Ok(()) => emit_progress(
            app,
            &progress(
                model_id,
                ModelProgressStatus::Done,
                1.0,
                Some(count),
                Some(count),
                format!("{} 下载完成", entry.display_name),
            ),
        ),
        Err(e) if e.starts_with(CANCELLED_PREFIX) => {
            emit_progress(
                app,
                &progress(
                    model_id,
                    ModelProgressStatus::Cancelled,
                    1.0,
                    Some(count),
                    Some(count),
                    "下载已取消",
                ),
            );
        }
        Err(_) => {}
    }
    result
}

fn download_file(
    app: &AppHandle,
    model_id: &str,
    url: &str,
    target: &Path,
    index: usize,
    count: usize,
    file_name: &str,
    cancel: &AtomicBool,
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
        if is_cancelled(cancel) {
            return Err(format!("{CANCELLED_PREFIX}下载已取消"));
        }
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
                let step_progress = (index as f64 + percent as f64 / 100.0) / count as f64;
                emit_progress(
                    app,
                    &progress(
                        model_id,
                        ModelProgressStatus::Running,
                        step_progress,
                        Some(index + 1),
                        Some(count),
                        format!("{file_name} {percent}%"),
                    ),
                );
            }
        } else if done % (2 * 1024 * 1024) < buffer.len() as u64 {
            emit_progress(
                app,
                &progress(
                    model_id,
                    ModelProgressStatus::Running,
                    (index + 1) as f64 / count as f64,
                    Some(index + 1),
                    Some(count),
                    format!("{file_name} {} MB", done / 1024 / 1024),
                ),
            );
        }
    }

    file.sync_all()
        .map_err(|e| format!("同步 {file_name} 失败: {e}"))?;
    fs::rename(&part, target).map_err(|e| format!("完成 {file_name} 失败: {e}"))?;
    Ok(())
}

/// 下载一个模型（异步后台执行，立即返回；进度经事件推送）。
#[tauri::command]
pub fn download_model_async(
    app: AppHandle,
    model_id: String,
    mirror: DownloadMirror,
    auto_fallback: bool,
) -> Result<(), String> {
    std::thread::spawn(move || {
        let _ = download_model(&app, &model_id, mirror, auto_fallback);
    });
    Ok(())
}

/// 请求取消指定模型的下载（若正在下载）。已取消/未在下载时静默。
#[tauri::command]
pub fn cancel_download(app: AppHandle, model_id: String) -> Result<(), String> {
    app.state::<ModelDownloadRegistry>().request_cancel(&model_id);
    Ok(())
}

/// 删除已下载的模型目录（回收磁盘）。当前选中模型的删除保护由前端确认。
#[tauri::command]
pub fn delete_model(app: AppHandle, model_id: String) -> Result<(), String> {
    let entry = find_model(&model_id).ok_or_else(|| format!("未知模型：{model_id}"))?;
    let root = model_paths::model_root(&app)?;
    let dir = root.join(&entry.dir_name);
    if !dir.is_dir() {
        return Err(format!("模型未下载：{}", entry.display_name));
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("删除 {} 失败: {e}", entry.display_name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_two_asr_and_three_embedding() {
        // ASR 目录原列 3 个，960ms 因无可下载公开来源被移除（见 all_models 注释）。
        let asr: Vec<_> = models_by_kind(ModelKind::Asr).collect();
        let emb: Vec<_> = models_by_kind(ModelKind::Embedding).collect();
        assert_eq!(asr.len(), 2);
        assert_eq!(emb.len(), 3);
    }

    #[test]
    fn manifest_ids_are_unique() {
        let mut ids: Vec<&str> = all_models().iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        let original = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), original);
    }

    #[test]
    fn wespeaker_is_embedding_not_asr() {
        let wespeaker = find_model("sherpa-onnx-wespeaker-zh-cnceleb-resnet34").unwrap();
        assert_eq!(wespeaker.kind, ModelKind::Embedding);
    }

    #[test]
    fn defaults_point_to_manifest_entries() {
        let asr = default_model(ModelKind::Asr);
        let emb = default_model(ModelKind::Embedding);
        assert_eq!(asr.id, "sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03");
        assert_eq!(emb.id, "sherpa-onnx-3dspeaker-eres2netv2-base");
    }

    #[test]
    fn model_config_defaults() {
        let cfg = ModelConfig::default();
        assert_eq!(cfg.asr_model, default_model(ModelKind::Asr).id);
        assert_eq!(cfg.embedding_model.as_deref(), Some(default_model(ModelKind::Embedding).id.as_str()));
        assert_eq!(cfg.mirror, DownloadMirror::HfMirror);
        assert!(cfg.auto_fallback_mirror);
        cfg.validate().unwrap();
    }

    #[test]
    fn model_config_round_trips_camel_case() {
        let cfg = ModelConfig {
            asr_model: default_model(ModelKind::Asr).id.clone(),
            embedding_model: None,
            mirror: DownloadMirror::Huggingface,
            auto_fallback_mirror: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""asrModel""#));
        assert!(json.contains(r#""embeddingModel":null"#));
        assert!(json.contains(r#""mirror":"huggingface""#));
        assert!(json.contains(r#""autoFallbackMirror":false"#));
        let parsed: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn model_config_rejects_unknown_model_id() {
        let cfg = ModelConfig {
            asr_model: "nope".to_string(),
            ..ModelConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn mirror_base_urls_and_alternates() {
        assert_eq!(DownloadMirror::Huggingface.base_url(), "https://huggingface.co");
        assert_eq!(DownloadMirror::HfMirror.base_url(), "https://hf-mirror.com");
        assert_eq!(DownloadMirror::Huggingface.alternate(), DownloadMirror::HfMirror);
        assert_eq!(DownloadMirror::HfMirror.alternate(), DownloadMirror::Huggingface);
        assert_eq!(DownloadMirror::parse("hf-mirror"), DownloadMirror::HfMirror);
        assert_eq!(DownloadMirror::parse("huggingface"), DownloadMirror::Huggingface);
        assert_eq!(DownloadMirror::parse("??? "), DownloadMirror::Huggingface);
    }

    #[test]
    fn mirror_candidates_with_and_without_fallback() {
        assert_eq!(
            mirror_candidates(DownloadMirror::HfMirror, true),
            vec![DownloadMirror::HfMirror, DownloadMirror::Huggingface]
        );
        assert_eq!(
            mirror_candidates(DownloadMirror::Huggingface, false),
            vec![DownloadMirror::Huggingface]
        );
    }

    #[test]
    fn download_url_uses_repo_and_file() {
        assert_eq!(
            download_url(DownloadMirror::HfMirror, "csukuangfj2/x", "tokens.txt"),
            "https://hf-mirror.com/csukuangfj2/x/resolve/main/tokens.txt"
        );
    }

    #[test]
    fn part_path_appends_suffix() {
        let path = part_path(Path::new("/tmp/encoder.onnx"));
        assert_eq!(path, PathBuf::from("/tmp/encoder.onnx.part"));
    }

    #[test]
    fn status_detection_requires_all_files_present() {
        let root = std::env::temp_dir().join(format!("models-status-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let entry = default_model(ModelKind::Asr);
        let dir = root.join(&entry.dir_name);
        fs::create_dir_all(&dir).unwrap();
        assert!(!model_ready(entry, &root));
        for f in &entry.files {
            fs::write(dir.join(f), vec![1u8; 8]).unwrap();
        }
        assert!(model_ready(entry, &root));
        assert_eq!(downloaded_file_count(entry, &root), entry.files.len());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prefer_downloaded_picks_present_model_over_default() {
        let root = std::env::temp_dir().join(format!("models-prefer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let default_asr = default_model(ModelKind::Asr);
        let alt_asr = models_by_kind(ModelKind::Asr)
            .find(|e| !e.default)
            .unwrap();
        fs::create_dir_all(root.join(&alt_asr.dir_name)).unwrap();
        for f in &alt_asr.files {
            fs::write(root.join(&alt_asr.dir_name).join(f), vec![1u8; 8]).unwrap();
        }
        // 默认未下载、备选已下载 → 优先备选。
        let chosen = prefer_downloaded(ModelKind::Asr, &default_asr.id, &root);
        assert_eq!(chosen, alt_asr.id);
        // 默认已下载 → 保持默认。
        fs::create_dir_all(root.join(&default_asr.dir_name)).unwrap();
        for f in &default_asr.files {
            fs::write(root.join(&default_asr.dir_name).join(f), vec![1u8; 8]).unwrap();
        }
        let chosen = prefer_downloaded(ModelKind::Asr, &default_asr.id, &root);
        assert_eq!(chosen, default_asr.id);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn download_manifest_round_trips_sizes() {
        let dir = std::env::temp_dir().join(format!("models-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut manifest = DownloadManifest::default();
        manifest.files.insert("encoder.int8.onnx".to_string(), 1234);
        manifest.files.insert("tokens.txt".to_string(), 99);
        write_manifest(&dir, &manifest).unwrap();
        let parsed = read_manifest(&dir);
        assert_eq!(parsed.files, manifest.files);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_manifest_missing_file_is_default_empty() {
        let dir = std::env::temp_dir().join(format!("models-manifest-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(read_manifest(&dir).files.is_empty());
    }

    #[test]
    fn cancel_registry_requests_and_detects_cancel() {
        let registry = ModelDownloadRegistry::default();
        let flag = registry.register("m1");
        assert!(!is_cancelled(&flag));
        assert!(registry.request_cancel("m1"));
        assert!(is_cancelled(&flag));
        // 未注册的模型请求取消：静默返回 false。
        assert!(!registry.request_cancel("nope"));
        registry.unregister("m1");
        assert!(!registry.request_cancel("m1"));
    }

    #[test]
    fn progress_event_serializes_camel_case() {
        let event = progress(
            "m1",
            ModelProgressStatus::Running,
            0.5,
            Some(2),
            Some(5),
            "encoder 50%",
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""modelId":"m1""#));
        assert!(json.contains(r#""fileCount":5"#));
        assert!(json.contains(r#""status":"running""#));
    }
}
