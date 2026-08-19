//! 会议纪要导出（T10 增强）：把已生成的纪要写为 Markdown 文件。
//!
//! 文件落在系统文档目录下的 `语音识别展示系统-导出/`，文件名带时间戳，
//! 避免覆盖历史纪要。真实命令（[export_minutes]）只做路径解析，写文件逻辑
//! 在可测的 [export_minutes_to_dir]（TDD 测试缝）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

/// 把纪要写入 `dir`，返回生成的文件路径。
///
/// 文件名带 Unix 时间戳，避免覆盖历史纪要；内容为带 `# 会议纪要` 标题的
/// Markdown（正文保留纪要的【要点】【行动项】【待办】分节，用户可用任意
/// Markdown 阅读器打开）。
pub fn export_minutes_to_dir(minutes: &str, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("会议纪要-{ts}.md"));
    let content = format!("# 会议纪要\n\n{minutes}\n");
    fs::write(&path, content).map_err(|e| format!("写入纪要文件失败: {e}"))?;
    Ok(path)
}

/// 前端导出命令：写入系统文档目录下的 `语音识别展示系统-导出/`。
#[tauri::command]
pub fn export_minutes(app: AppHandle, minutes: String) -> Result<String, String> {
    let dir = app
        .path()
        .document_dir()
        .map_err(|e| format!("无法定位系统文档目录: {e}"))?
        .join("语音识别展示系统-导出");
    export_minutes_to_dir(&minutes, &dir).map(|path| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }

    #[test]
    fn writes_markdown_file_with_title_and_content() {
        let dir = std::env::temp_dir().join(format!("t10-export-test-{}", timestamp()));
        let path = export_minutes_to_dir("【要点】项目排期确定", &dir).unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("md"));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("# 会议纪要"), "md 应带标题: {raw}");
        assert!(
            raw.contains("【要点】项目排期确定"),
            "md 应含纪要正文: {raw}"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
