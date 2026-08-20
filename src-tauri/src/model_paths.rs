//! ASR 运行时与模型目录的统一解析（开发 / 打包双路径）。
//!
//! - 开发模式（`tauri::is_dev()`）：沿用仓库内 `src-tauri/.venv` 与
//!   `src-tauri/asr-models`，克隆后本机调试零额外配置。
//! - 打包模式：Python + `sherpa-onnx` 运行时在构建期打入 app 资源
//!   （`resources/runtime/`，只读）；模型下载到 app 数据目录
//!   （macOS `~/Library/Application Support/<id>/asr-models`，
//!   Windows `%APPDATA%/<id>\asr-models`），保证用户目录可写。

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// 打包版运行时在 resource 目录下的子目录（与 `tauri.conf.json` resources 对应）。
const PACKAGED_RUNTIME_DIR: &str = "runtime";

/// 运行时路径：Python 可执行文件 + sidecar 脚本。
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub python: PathBuf,
    pub script: PathBuf,
}

/// 解析 Python 运行时内的解释器路径。
///
/// 支持两种布局：
/// - 标准 venv：Windows 用 `Scripts/python.exe`，其余用 `bin/python3`；
/// - python-build-standalone 直接解压（正式分发）：解释器在 Windows 位于
///   venv 根目录 `python.exe`（unix 仍是 `bin/python3`），见
///   `scripts/package-runtime.mjs`。
fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        let scripts = venv.join("Scripts").join("python.exe");
        if scripts.is_file() {
            scripts
        } else {
            venv.join("python.exe")
        }
    } else {
        let python3 = venv.join("bin").join("python3");
        if python3.is_file() {
            python3
        } else {
            venv.join("bin").join("python")
        }
    }
}

/// 模型根目录：打包版用 app 数据目录，开发版用仓库 `src-tauri/asr-models`。
pub fn model_root(app: &AppHandle) -> Result<PathBuf, String> {
    if tauri::is_dev() {
        Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("asr-models"))
    } else {
        app.path()
            .app_data_dir()
            .map(|dir| dir.join("asr-models"))
            .map_err(|e| format!("无法获取 app 数据目录: {e}"))
    }
}

/// 解析 sidecar Python 与脚本路径。
pub fn runtime_paths(app: &AppHandle) -> Result<RuntimePaths, String> {
    if tauri::is_dev() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        Ok(RuntimePaths {
            python: venv_python(&manifest.join(".venv")),
            script: manifest.join("sherpa_streaming.py"),
        })
    } else {
        let resource_dir = app
            .path()
            .resource_dir()
            .map_err(|e| format!("无法获取 app 资源目录: {e}"))?;
        let runtime = resource_dir.join(PACKAGED_RUNTIME_DIR);
        Ok(RuntimePaths {
            python: venv_python(&runtime.join("venv")),
            script: runtime.join("sherpa_streaming.py"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venv_python_prefers_existing_layout() {
        let tmp = std::env::temp_dir().join(format!("talksee-venv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        if cfg!(windows) {
            // 标准 venv 布局（Scripts/python.exe）优先
            std::fs::create_dir_all(tmp.join("Scripts")).unwrap();
            std::fs::write(tmp.join("Scripts").join("python.exe"), b"").unwrap();
            assert_eq!(venv_python(&tmp), tmp.join("Scripts").join("python.exe"));
            // 只有根目录 python.exe（python-build-standalone 布局）时回退
            let root_only = tmp.join("root-only");
            std::fs::create_dir_all(&root_only).unwrap();
            std::fs::write(root_only.join("python.exe"), b"").unwrap();
            assert_eq!(venv_python(&root_only), root_only.join("python.exe"));
        } else {
            // bin/python3 优先
            std::fs::create_dir_all(tmp.join("bin")).unwrap();
            std::fs::write(tmp.join("bin").join("python3"), b"").unwrap();
            assert_eq!(venv_python(&tmp), tmp.join("bin").join("python3"));
            // 只有 bin/python 时回退
            let fallback = tmp.join("fallback");
            std::fs::create_dir_all(fallback.join("bin")).unwrap();
            std::fs::write(fallback.join("bin").join("python"), b"").unwrap();
            assert_eq!(venv_python(&fallback), fallback.join("bin").join("python"));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
