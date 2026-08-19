/**
 * 会议纪要展示面板（T10）：停止识别后由 LLM 分批汇总生成的结构化会议纪要
 * （要点/行动项/待办），供会后回顾。
 *
 * 对齐规格用户故事 26/27/36：
 * - 停止识别后一键生成结构化纪要并在此查看；
 * - 内容多时自动分批交给 LLM 再汇总（本面板只负责展示最终纪要）；
 * - 生成期间显示状态提示（「正在生成纪要…」）。
 *
 * 渲染规则：轻量解析【分节】标题（行首 `【…】`）为小节标题，其余内容按
 * 段落渲染；不做复杂结构化解析，保证任何 LLM 输出形状都能可靠展示。
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ReactNode } from "react";
import "./MinutesPanel.css";

export interface MinutesPanelProps {
  /** 最终纪要文本；null 表示尚未生成。 */
  minutes: string | null;
  /** 本会话的 Markdown 字幕记录（整理版优先，无整理版回退原文）。 */
  transcript: string;
  /** 是否正在生成纪要（停止后、minutesReady 到达前）。 */
  generating: boolean;
}

/** 把纪要文本解析为渲染节点：`【标题】` 行 → 分节标题，其余 → 段落。 */
export function renderMinutes(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const paragraphs: string[] = [];
  let key = 0;

  const flushParagraphs = () => {
    if (paragraphs.length > 0) {
      nodes.push(
        <p key={key++} className="minutes-para">
          {paragraphs.join("\n")}
        </p>,
      );
      paragraphs.length = 0;
    }
  };

  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const m = trimmed.match(/^【([^】]+)】\s*(.*)$/);
    if (m) {
      flushParagraphs();
      nodes.push(
        <h3 key={key++} className="minutes-section">
          【{m[1]}】
        </h3>,
      );
      if (m[2]) paragraphs.push(m[2]);
    } else {
      paragraphs.push(trimmed);
    }
  }
  flushParagraphs();
  return nodes;
}

export default function MinutesPanel({ minutes, transcript, generating }: MinutesPanelProps) {
  const [exportStatus, setExportStatus] = useState("");

  // 新会话开始（minutes 清空）时，清除上一会话的导出状态。
  useEffect(() => {
    if (!minutes) setExportStatus("");
  }, [minutes]);

  const doExport = async () => {
    if (!minutes) return;
    setExportStatus("正在导出…");
    try {
      const path = await invoke<string>("export_session", {
        transcript,
        minutes,
      });
      setExportStatus(`已导出：${path}`);
    } catch (e) {
      setExportStatus(`导出失败: ${String(e)}`);
    }
  };

  return (
    <section className="minutes-panel" aria-label="会议纪要">
      <div className="minutes-header">
        <span className="minutes-title">📋 会议纪要</span>
        {generating && <span className="minutes-badge is-generating">正在生成纪要…</span>}
        {!generating && minutes && <span className="minutes-badge is-ready">已生成</span>}
        {!generating && minutes && (
          <button
            type="button"
            className="minutes-export"
            onClick={doExport}
            disabled={exportStatus === "正在导出…"}
          >
            💾 导出 .md
          </button>
        )}
        {exportStatus && <span className="minutes-export-status">{exportStatus}</span>}
      </div>
      <div className="minutes-body">
        {generating && !minutes && (
          <p className="minutes-empty">已停止识别，正在分批汇总（要点/行动项/待办），请稍候…</p>
        )}
        {!generating && !minutes && (
          <p className="minutes-empty">点击「停止并生成纪要」后，这里显示结构化会议纪要。</p>
        )}
        {minutes && renderMinutes(minutes)}
      </div>
    </section>
  );
}
