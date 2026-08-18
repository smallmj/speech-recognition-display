/**
 * 双轨展示（Dual-track Display）：默认只显示 LLM 整理版，一键切换显示原文。
 *
 * 对齐 ADR-0003：原文不可变保留在后台作为事实来源，界面默认展示整理版
 * （阅读体验最优），整理版对改动词做差异高亮（CARTGPT 做法），并提供
 * 「显示原文」一键切换核对。
 *
 * 本组件是**纯渲染组件**：只接收 engine 事件流（`EngineEvent[]`）并渲染，
 * 不做任何业务逻辑。事件由上层提供：
 *
 * - 真实链路：上层用 [useEngineEvents] 监听 `engine://event` 累积事件后传入；
 * - 演示链路：`DualTrackDemo` 在浏览器内合成事件（模拟防抖/节奏/失败）。
 *
 * 渲染规则：
 * - 默认（整理版模式）：有 `cleaned` 显示整理版并高亮改动词；
 *   状态 `failed` 回退显示原文；尚未整理的 `active`/`frozen` 显示原文作占位。
 * - 原文模式：一律显示原文 `raw`。
 */

import { useMemo, useState } from "react";
import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ENGINE_EVENT, type EngineEvent, type Segment } from "../engineEvents";
import { diffHighlight } from "./diff";
import "./DualTrack.css";

/** 未登记到说话人时的兜底颜色（SpeakerAssigned 先到，正常不会用到）。 */
export const FALLBACK_COLOR = "#8e8e93";

/** 可选的整理间隔（秒）：5s / 10s 两档。 */
export type CleanupInterval = 5 | 10;

export interface DualTrackViewProps {
  /** 累积的 engine 事件流（顺序即到达顺序）。 */
  events: EngineEvent[];
  /** 当前整理的固定节奏间隔（秒）。默认 5s。 */
  intervalSeconds?: CleanupInterval;
  /** 切换整理间隔的回调（由上层驱动 engine 配置）。 */
  onIntervalChange?: (seconds: CleanupInterval) => void;
}

/** 展示模式：显示整理版 / 显示原文。 */
type DisplayMode = "cleaned" | "raw";

/**
 * 把事件流规约到片段映射：`SegmentAppended` 增，`SegmentCleaned` 更新
 * （editId 校验：只接受更大值，防御乱序），`CleanupFailed` 置 Failed。
 */
export function reconcileSegments(prev: Map<number, Segment>, evt: EngineEvent): Map<number, Segment> {
  switch (evt.type) {
    case "segmentAppended": {
      const next = new Map(prev);
      next.set(evt.segment.id, evt.segment);
      return next;
    }
    case "segmentCleaned": {
      const seg = prev.get(evt.segmentId);
      if (!seg) return prev;
      // editId 校验：与 engine 一致，只接受严格更大的结果
      if (seg.editId != null && evt.editId <= seg.editId) return prev;
      const next = new Map(prev);
      next.set(seg.id, {
        ...seg,
        cleaned: evt.cleaned,
        editId: evt.editId,
        status: "cleaned",
      });
      return next;
    }
    case "cleanupFailed": {
      const seg = prev.get(evt.segmentId);
      if (!seg) return prev;
      const next = new Map(prev);
      next.set(seg.id, { ...seg, status: "failed" });
      return next;
    }
    default:
      return prev; // sessionStarted / speakerAssigned / ... 不改变片段
  }
}

/** 从事件流规约说话人颜色映射（SpeakerAssigned 先于 SegmentAppended 到达）。 */
export function reconcileSpeakerColors(prev: Map<number, string>, evt: EngineEvent): Map<number, string> {
  if (evt.type === "speakerAssigned") {
    const next = new Map(prev);
    if (!next.has(evt.speakerId)) {
      next.set(evt.speakerId, evt.color);
    }
    return next;
  }
  return prev;
}

/** 把事件数组规约成片段映射（纯函数，供渲染/测试）。 */
export function reduceEvents(events: EngineEvent[]): Map<number, Segment> {
  let map = new Map<number, Segment>();
  for (const evt of events) {
    map = reconcileSegments(map, evt);
  }
  return map;
}

/** 把事件数组规约成说话人颜色映射。 */
export function reduceSpeakerColors(events: EngineEvent[]): Map<number, string> {
  let map = new Map<number, string>();
  for (const evt of events) {
    map = reconcileSpeakerColors(map, evt);
  }
  return map;
}

/**
 * 监听 `engine://event` 并累积事件流。返回按到达顺序排列的事件数组；
 * 组件卸载时自动取消监听。
 *
 * 真实链路集成（一行）：
 * ```tsx
 * const events = useEngineEvents();
 * <DualTrackView events={events} />
 * ```
 */
export function useEngineEvents(): EngineEvent[] {
  const [events, setEvents] = useState<EngineEvent[]>([]);
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<EngineEvent>(ENGINE_EVENT, (event) => {
        setEvents((prev) => [...prev, event.payload]);
      });
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);
  return events;
}

/** 判断一条片段在给定模式下展示哪种文本。 */
export function resolveMode(seg: Segment, mode: DisplayMode): "cleaned" | "raw" | "pending" {
  if (mode === "raw") return "raw";
  if (seg.status === "failed") return "raw"; // 整理失败回退原文
  if (seg.status === "cleaned" && seg.cleaned != null) return "cleaned";
  return "pending"; // active/frozen：整理版未就绪，临时显示原文
}

/**
 * 双轨展示组件：默认显示整理版，一键切换显示原文，整理版改动词差异高亮。
 */
export default function DualTrackView({
  events,
  intervalSeconds = 5,
  onIntervalChange,
}: DualTrackViewProps) {
  const [mode, setMode] = useState<DisplayMode>("cleaned");

  const segments = useMemo(() => reduceEvents(events), [events]);
  const speakerColors = useMemo(() => reduceSpeakerColors(events), [events]);
  const sorted = useMemo(
    () => [...segments.values()].sort((a, b) => a.id - b.id),
    [segments],
  );

  const cleanedCount = [...segments.values()].filter((s) => s.status === "cleaned").length;
  const failedCount = [...segments.values()].filter((s) => s.status === "failed").length;

  return (
    <div className="dual-track">
      <div className="dual-toolbar">
        <div className="dual-title">整理版 · 双轨展示</div>
        <div className="dual-controls">
          {onIntervalChange && (
            <div className="dual-interval" role="group" aria-label="整理间隔">
              <span className="dual-label">整理间隔</span>
              {([5, 10] as const).map((s) => (
                <button
                  key={s}
                  type="button"
                  className={`dual-chip ${intervalSeconds === s ? "is-on" : ""}`}
                  aria-pressed={intervalSeconds === s}
                  onClick={() => onIntervalChange(s)}
                >
                  {s}s
                </button>
              ))}
            </div>
          )}
          <button
            type="button"
            className="dual-toggle"
            aria-pressed={mode === "raw"}
            onClick={() => setMode((m) => (m === "cleaned" ? "raw" : "cleaned"))}
          >
            {mode === "cleaned" ? "显示原文" : "显示整理版"}
          </button>
        </div>
      </div>

      <div className="dual-list">
        {sorted.length === 0 && (
          <div className="dual-empty">
            <p className="dual-empty-title">等待片段…</p>
            <p className="dual-empty-sub">整理管线把原文交给 LLM 后，这里默认展示整理版。</p>
          </div>
        )}

        {sorted.map((seg) => {
          const color = speakerColors.get(seg.speakerId) ?? FALLBACK_COLOR;
          const segMode = resolveMode(seg, mode);
          return (
            <div className="dual-row" key={seg.id}>
              <div className="dual-meta">
                <span className="dual-speaker" style={{ color }}>
                  说话人 {seg.speakerId}
                </span>
                {seg.status === "active" && <span className="dual-badge is-pending">整理中…</span>}
                {seg.status === "failed" && <span className="dual-badge is-failed">整理失败 · 原文</span>}
              </div>
              <div
                className={`dual-text ${segMode === "raw" || segMode === "pending" ? "is-raw" : ""}`}
                style={{
                  borderLeftColor: color,
                  background: `${color}1f`,
                }}
              >
                {segMode === "cleaned" && seg.cleaned != null
                  ? diffHighlight(seg.raw, seg.cleaned).map((run, i) =>
                      run.added ? (
                        <mark key={i} className="dual-mark">
                          {run.text}
                        </mark>
                      ) : (
                        <span key={i}>{run.text}</span>
                      ),
                    )
                  : seg.raw}
              </div>
            </div>
          );
        })}
      </div>

      <footer className="dual-status">
        共 <strong>{sorted.length}</strong> 条 · 整理完成 <strong>{cleanedCount}</strong> 条 · 失败{" "}
        <strong>{failedCount}</strong> 条 · 当前{mode === "cleaned" ? "整理版" : "原文"}
      </footer>
    </div>
  );
}
