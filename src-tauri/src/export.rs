//! 会话导出（T10/T11）：把整理后的字幕文本与会议纪要写为 Markdown 文件。
//!
//! 文件落在系统文档目录下的 `TalkSee-导出/`，文件名带时间戳，
//! 避免覆盖历史导出。真实命令（[export_session]）只做路径解析，写文件逻辑
//! 在可测的 [export_session_to_dir]（TDD 测试缝）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

/// 把会话（字幕 + 纪要）写入 `dir`，返回生成的文件路径。
///
/// 文件名带 Unix 时间戳；内容为带 `# 字幕记录与会议纪要` 标题的 Markdown：
/// - `## 字幕记录`：逐条发言（时间 + 说话人 + 整理版文本；无整理版时由
///   前端回退原文，对应「关闭 LLM 则导出原文」）；
/// - `## 会议纪要`：结构化纪要（【要点】【行动项】【待办】分节）。
pub fn export_session_to_dir(
    transcript: &str,
    minutes: &str,
    dir: &Path,
) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("会话记录-{ts}.md"));
    let content = format!(
        "# 字幕记录与会议纪要\n\n## 字幕记录\n\n{transcript}\n\n## 会议纪要\n\n{minutes}\n"
    );
    fs::write(&path, content).map_err(|e| format!("写入会话记录文件失败: {e}"))?;
    Ok(path)
}

/// 前端导出命令：写入系统文档目录下的 `TalkSee-导出/`。
#[tauri::command]
pub fn export_session(
    app: AppHandle,
    transcript: String,
    minutes: String,
) -> Result<String, String> {
    let dir = app
        .path()
        .document_dir()
        .map_err(|e| format!("无法定位系统文档目录: {e}"))?
        .join("TalkSee-导出");
    export_session_to_dir(&transcript, &minutes, &dir).map(|path| path.display().to_string())
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
    fn writes_markdown_file_with_transcript_and_minutes() {
        let dir = std::env::temp_dir().join(format!("t10-export-test-{}", timestamp()));
        let path = export_session_to_dir(
            "**[10:00:01] 说话人 1**\n整理版：你好\n",
            "【要点】项目排期确定",
            &dir,
        )
        .unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("md"));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("# 字幕记录"), "md 应含字幕区标题: {raw}");
        assert!(raw.contains("说话人 1"), "md 应含字幕正文: {raw}");
        assert!(raw.contains("## 会议纪要"), "md 应含纪要区标题: {raw}");
        assert!(
            raw.contains("【要点】项目排期确定"),
            "md 应含纪要正文: {raw}"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
