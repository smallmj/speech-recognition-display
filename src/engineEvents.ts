/**
 * engine 事件流的 TypeScript 契约。
 *
 * 与 `engine/src/types.rs` 的 serde 序列化对齐：
 * - 事件用 `type` 标签区分（Rust `#[serde(tag = "type", rename_all = "camelCase")]`）；
 * - 字段一律 camelCase；
 * - `SegmentStatus` / `Gender` 为小写字符串。
 */

export type SegmentStatus = "active" | "frozen" | "cleaned" | "failed";

export type Gender = "male" | "female" | "unknown";

export interface Segment {
  id: number;
  speakerId: number;
  raw: string;
  status: SegmentStatus;
  cleaned?: string | null;
  editId?: number | null;
  ts: number;
  retries: number;
}

export type EngineEvent =
  | { type: "sessionStarted" }
  | { type: "sessionStopped" }
  | { type: "segmentAppended"; segment: Segment }
  | {
      type: "speakerAssigned";
      segmentId: number;
      speakerId: number;
      isNewSpeaker: boolean;
      color: string;
      gender: Gender;
    }
  | { type: "segmentCleaned"; segmentId: number; cleaned: string; editId: number }
  | { type: "cleanupFailed"; segmentId: number }
  | { type: "minutesReady"; minutes: string };

/** Rust 端 emit 的 engine 事件流事件名（与 `src-tauri/src/pipeline.rs` 保持一致）。 */
export const ENGINE_EVENT = "engine://event";
