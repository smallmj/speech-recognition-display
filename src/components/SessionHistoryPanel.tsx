/**
 * 会话历史面板（T11）：列出本地保存的会话，重新打开查看（字幕 + 纪要），
 * 并可导出 Markdown / TXT / SRT。
 *
 * 会话在每次「停止并生成纪要」后由 Rust 端自动写入 app data 目录
 * （`sessions/session-<id>.json`），重启应用后仍在。
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./SessionHistoryPanel.css";

/** 与 Rust `src-tauri/src/sessions.rs` 的 `SessionSegment` 对齐（camelCase）。 */
export interface SessionSegment {
  id: number;
  speakerId: number;
  raw: string;
  cleaned?: string | null;
  ts: number;
}

/** 与 Rust `SessionRecord` 对齐。 */
export interface SessionRecord {
  id: string;
  createdAt: number;
  segments: SessionSegment[];
  minutes: string;
}

/** 与 Rust `SessionSummary` 对齐。 */
export interface SessionSummary {
  id: string;
  createdAt: number;
  segmentCount: number;
  minutes: string;
}

export interface SessionHistoryPanelProps {
  /** 最近一次纪要文本；变化（含新会话清空）时刷新历史列表。 */
  latestMinutes: string | null;
}

function formatTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export default function SessionHistoryPanel({ latestMinutes }: SessionHistoryPanelProps) {
  const [open, setOpen] = useState(false);
  const [summaries, setSummaries] = useState<SessionSummary[]>([]);
  const [selected, setSelected] = useState<SessionRecord | null>(null);
  const [listStatus, setListStatus] = useState("");
  const [exportStatus, setExportStatus] = useState("");

  const refresh = useCallback(async () => {
    try {
      const list = await invoke<SessionSummary[]>("list_sessions");
      setSummaries(list);
      setListStatus(list.length > 0 ? `共 ${list.length} 个历史会话` : "暂无历史会话");
    } catch (e) {
      setListStatus(`读取历史失败: ${String(e)}`);
    }
  }, []);

  // 挂载时 + 每次纪要变化（停止生成后 / 新会话清空）都刷新列表。
  useEffect(() => {
    refresh();
  }, [refresh, latestMinutes]);

  const openSession = async (id: string) => {
    setExportStatus("");
    try {
      const record = await invoke<SessionRecord>("load_session", { id });
      setSelected(record);
    } catch (e) {
      setListStatus(`重新打开失败: ${String(e)}`);
    }
  };

  const exportSession = async (format: "md" | "txt" | "srt") => {
    if (!selected) return;
    setExportStatus(`正在导出 ${format.toUpperCase()}…`);
    try {
      const path = await invoke<string>("export_session_file", {
        id: selected.id,
        format,
      });
      setExportStatus(`已导出 ${format.toUpperCase()}：${path}`);
    } catch (e) {
      setExportStatus(`导出失败: ${String(e)}`);
    }
  };

  const sorted = selected ? [...selected.segments].sort((a, b) => a.id - b.id) : [];

  return (
    <section className="session-history">
      <button
        type="button"
        className="session-history-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        📚 历史会话 {open ? "▾" : "▸"}
      </button>

      {open && (
        <div className="session-history-body">
          <div className="session-history-toolbar">
            <button type="button" className="session-history-refresh" onClick={refresh}>
              刷新
            </button>
            <span className="session-history-status">{listStatus}</span>
          </div>

          <ul className="session-history-list">
            {summaries.map((s) => (
              <li key={s.id} className="session-history-item">
                <span className="session-history-time">
                  {new Date(s.createdAt).toLocaleString()}
                </span>
                <span className="session-history-count">{s.segmentCount} 条</span>
                <button type="button" onClick={() => openSession(s.id)}>
                  重新打开
                </button>
              </li>
            ))}
            {summaries.length === 0 && (
              <li className="session-history-empty">停止并生成纪要后，会话会自动保存到这里。</li>
            )}
          </ul>

          {selected && (
            <div className="session-history-detail">
              <div className="session-history-detail-header">
                <span>会话 {selected.id}</span>
                <span>{new Date(selected.createdAt).toLocaleString()}</span>
              </div>

              <h4 className="session-history-section-title">字幕记录</h4>
              <div className="session-history-transcript">
                {sorted.map((seg) => (
                  <p key={seg.id}>
                    [{formatTime(seg.ts)}] 说话人 {seg.speakerId}：
                    {seg.cleaned && seg.cleaned.trim().length > 0 ? seg.cleaned : seg.raw}
                  </p>
                ))}
              </div>

              <h4 className="session-history-section-title">会议纪要</h4>
              <pre className="session-history-minutes">{selected.minutes}</pre>

              <div className="session-history-exports">
                <button type="button" onClick={() => exportSession("md")}>
                  💾 导出 .md
                </button>
                <button type="button" onClick={() => exportSession("txt")}>
                  💾 导出 .txt
                </button>
                <button type="button" onClick={() => exportSession("srt")}>
                  💾 导出 .srt
                </button>
                {exportStatus && <span className="session-history-export-status">{exportStatus}</span>}
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
