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
  /**
   * 客户端侧的流式整理增量（非 Rust 序列化字段）：收到 `segmentCleaning`
   * 时累积，`segmentCleaned` / `cleanupFailed` 到达后清空。
   */
  cleaningPartial?: string | null;
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
  | { type: "partialResult"; text: string }
  | { type: "segmentCleaning"; segmentId: number; partial: string }
  | { type: "segmentCleaned"; segmentId: number; cleaned: string; editId: number }
  | { type: "cleanupFailed"; segmentId: number }
  | { type: "minutesReady"; minutes: string };

/** Rust 端 emit 的 engine 事件流事件名（与 `src-tauri/src/pipeline.rs` 保持一致）。 */
export const ENGINE_EVENT = "engine://event";

/** 壳层运行状态事件名（ASR 模式等，与 `src-tauri/src/pipeline.rs` 保持一致）。 */
export const STATUS_EVENT = "engine://status";

/** 壳层状态负载：mode = sherpa（真实本地 ASR）| mock（合成转写演示）。 */
export interface StatusPayload {
  mode: "sherpa" | "mock";
  reason?: string;
}
