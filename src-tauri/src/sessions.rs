//! 会话持久化与导出（T11）：会话自动保存到本地、历史列表、重新打开、
//! 导出 Markdown / TXT / SRT，重启后历史仍在。
//!
//! 会话记录以 JSON 存在 app data 目录的 `sessions/`；导出文件写入系统文档
//! 目录的 `TalkSee-导出/`。纯格式化函数（markdown/txt/srt）与
//! 保存/列表逻辑都可测（TDD 测试缝）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSegment {
    pub id: u64,
    pub speaker_id: u32,
    pub raw: String,
    pub cleaned: Option<String>,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub created_at: u64,
    pub segments: Vec<SessionSegment>,
    pub minutes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub created_at: u64,
    pub segment_count: usize,
    pub minutes: String,
}

/// 一段发言的导出文本：优先整理版，无整理版（LLM 未开启/未整理/失败）回退原文。
fn segment_text(seg: &SessionSegment) -> &str {
    match seg.cleaned.as_deref() {
        Some(cleaned) if !cleaned.trim().is_empty() => cleaned,
        _ => seg.raw.as_str(),
    }
}

/// 毫秒时间戳 → `HH:MM:SS,mmm`（SRT 用）。
fn srt_time(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

/// 毫秒时间戳 → `HH:MM:SS`（Markdown/TXT 用）。
fn clock_time(ms: u64) -> String {
    srt_time(ms).split(',').next().unwrap_or("").to_string()
}

/// 会话记录 → Markdown（字幕记录 + 会议纪要）。
pub fn format_session_markdown(record: &SessionRecord) -> String {
    let mut transcript = Vec::new();
    let mut segments: Vec<&SessionSegment> = record.segments.iter().collect();
    segments.sort_by_key(|s| s.id);
    for seg in segments {
        transcript.push(format!(
            "### [{}] 说话人 {}\n{}",
            clock_time(seg.ts),
            seg.speaker_id,
            segment_text(seg)
        ));
    }
    format!(
        "# 字幕记录与会议纪要\n\n## 字幕记录\n\n{}\n\n## 会议纪要\n\n{}\n",
        transcript.join("\n\n"),
        record.minutes
    )
}

/// 会话记录 → 纯文本（TXT）。
pub fn format_session_txt(record: &SessionRecord) -> String {
    let mut transcript = Vec::new();
    let mut segments: Vec<&SessionSegment> = record.segments.iter().collect();
    segments.sort_by_key(|s| s.id);
    for seg in segments {
        transcript.push(format!(
            "[{}] 说话人 {}\n{}",
            clock_time(seg.ts),
            seg.speaker_id,
            segment_text(seg)
        ));
    }
    format!(
        "字幕记录\n\n{}\n\n会议纪要\n\n{}\n",
        transcript.join("\n\n"),
        record.minutes
    )
}

/// 会话记录 → SRT 字幕。
///
/// 只有每条发言的开始时间点，因此结束时间取下一条发言的开始时间；最后一条
/// 取开始时间 + 5 秒（无重叠、无负时长）。
pub fn format_session_srt(record: &SessionRecord) -> String {
    let mut segments: Vec<&SessionSegment> = record.segments.iter().collect();
    segments.sort_by_key(|s| s.id);
    let mut entries = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        let start = seg.ts;
        let end = segments
            .get(i + 1)
            .map(|next| next.ts)
            .unwrap_or(start + 5_000);
        entries.push(format!(
            "{}\n{} --> {}\n{}\n",
            i + 1,
            srt_time(start),
            srt_time(end),
            segment_text(seg)
        ));
    }
    entries.join("\n")
}

/// 把会话记录写入 `dir/session-<id>.json`。
pub fn save_session_to_dir(record: &SessionRecord, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建会话目录失败: {e}"))?;
    let path = dir.join(format!("session-{}.json", record.id));
    let json = serde_json::to_string_pretty(record).map_err(|e| format!("序列化会话失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入会话文件失败: {e}"))?;
    Ok(path)
}

/// 从 `dir` 列出全部会话摘要（按创建时间倒序，即最新在前）。
pub fn list_sessions_from_dir(dir: &Path) -> Result<Vec<SessionSummary>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut summaries = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| format!("读取会话目录失败: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<SessionRecord>(&raw) else {
            continue;
        };
        summaries.push(SessionSummary {
            segment_count: record.segments.len(),
            id: record.id,
            created_at: record.created_at,
            minutes: record.minutes,
        });
    }
    summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(summaries)
}

/// 从 `dir` 读取指定 id 的会话记录。
pub fn load_session_from_dir(dir: &Path, id: &str) -> Result<SessionRecord, String> {
    let path = dir.join(format!("session-{id}.json"));
    let raw = fs::read_to_string(&path).map_err(|e| format!("读取会话失败: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("解析会话失败: {e}"))
}

fn sessions_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("sessions"))
        .map_err(|e| format!("无法获取 app data 目录: {e}"))
}

/// 驱动线程在 `MinutesReady` 前自动保存会话（T11）。
pub(crate) fn save_session(
    app: &AppHandle,
    segments: Vec<SessionSegment>,
    minutes: String,
) -> Result<SessionRecord, String> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let record = SessionRecord {
        id: format!("{now_ms}"),
        created_at: now_ms,
        segments,
        minutes,
    };
    save_session_to_dir(&record, &sessions_dir(app)?)?;
    Ok(record)
}

/// 前端历史列表命令。
#[tauri::command]
pub fn list_sessions(app: AppHandle) -> Result<Vec<SessionSummary>, String> {
    list_sessions_from_dir(&sessions_dir(&app)?)
}

/// 前端「重新打开」命令：返回完整会话记录。
#[tauri::command]
pub fn load_session(app: AppHandle, id: String) -> Result<SessionRecord, String> {
    load_session_from_dir(&sessions_dir(&app)?, &id)
}

/// 前端导出历史会话命令：`format` = md | txt | srt，写入导出目录并返回路径。
#[tauri::command]
pub fn export_session_file(app: AppHandle, id: String, format: String) -> Result<String, String> {
    let record = load_session_from_dir(&sessions_dir(&app)?, &id)?;
    let (ext, content) = match format.as_str() {
        "md" => ("md", format_session_markdown(&record)),
        "txt" => ("txt", format_session_txt(&record)),
        "srt" => ("srt", format_session_srt(&record)),
        other => return Err(format!("不支持的导出格式: {other}（支持 md/txt/srt）")),
    };
    let dir = app
        .path()
        .document_dir()
        .map_err(|e| format!("无法定位系统文档目录: {e}"))?
        .join("TalkSee-导出");
    fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let path = dir.join(format!("会话记录-{}.{ext}", record.id));
    fs::write(&path, content).map_err(|e| format!("写入导出文件失败: {e}"))?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: u64, speaker: u32, raw: &str, cleaned: Option<&str>, ts: u64) -> SessionSegment {
        SessionSegment {
            id,
            speaker_id: speaker,
            raw: raw.to_string(),
            cleaned: cleaned.map(|c| c.to_string()),
            ts,
        }
    }

    fn record() -> SessionRecord {
        SessionRecord {
            id: "123".to_string(),
            created_at: 1_700_000_000_000,
            segments: vec![
                seg(1, 1, "原始口语", Some("整理后的书面语"), 1_700_000_000_000),
                seg(2, 2, "第二句原文", None, 1_700_000_005_000),
            ],
            minutes: "【要点】项目排期确定".to_string(),
        }
    }

    #[test]
    fn formats_markdown_with_transcript_and_minutes() {
        let md = format_session_markdown(&record());
        assert!(md.contains("## 字幕记录"), "md: {md}");
        assert!(md.contains("整理后的书面语"), "应优先整理版: {md}");
        assert!(md.contains("第二句原文"), "无整理版回退原文: {md}");
        assert!(md.contains("## 会议纪要"), "md: {md}");
    }

    #[test]
    fn formats_txt_with_plain_lines() {
        let txt = format_session_txt(&record());
        assert!(txt.contains("字幕记录"));
        assert!(txt.contains("整理后的书面语"));
        assert!(txt.contains("第二句原文"));
        assert!(txt.contains("会议纪要"));
    }

    #[test]
    fn formats_srt_with_timecodes() {
        let rec = record();
        let srt = format_session_srt(&rec);
        let first_start = srt_time(rec.segments[0].ts);
        let first_end = srt_time(rec.segments[1].ts);
        let second_end = srt_time(rec.segments[1].ts + 5_000);
        assert!(
            srt.contains(&format!("1\n{first_start} --> {first_end}")),
            "srt: {srt}"
        );
        assert!(
            srt.contains(&format!("2\n{first_end} --> {second_end}")),
            "srt: {srt}"
        );
        assert!(srt.contains("整理后的书面语"));
        assert!(srt.contains("第二句原文"));
    }

    #[test]
    fn missing_sessions_dir_lists_empty() {
        let dir = std::env::temp_dir().join(format!(
            "t11-no-sessions-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        ));
        assert!(list_sessions_from_dir(&dir).unwrap().is_empty());
    }

    #[test]
    fn save_and_list_roundtrip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!(
            "t11-sessions-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        ));
        save_session_to_dir(&record(), &dir).unwrap();
        let list = list_sessions_from_dir(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "123");
        assert_eq!(list[0].segment_count, 2);

        let loaded = load_session_from_dir(&dir, "123").unwrap();
        assert_eq!(loaded, record());
        fs::remove_dir_all(&dir).ok();
    }
}
