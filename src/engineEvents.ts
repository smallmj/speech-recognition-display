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
   * 客户端侧的流式整理增量（非 Rust 序列化字段）：仅用于标记「整理中」；
   * 展示层保留原文，`segmentCleaned` / `segmentsCleaned` / `cleanupFailed`
   * 到达后清空。
   */
  cleaningPartial?: string | null;
  /**
   * 客户端侧的流式整理 editId（非 Rust 序列化字段）：`segmentCleaning` 的
   * editId，用于拒绝乱序残余增量（只接受 `>= 当前`）。
   */
  cleaningEditId?: number | null;
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
  | {
      type: "segmentCleaning";
      segmentId: number;
      /** 本次流式请求的 editId（同一请求内不变）；渲染层只接受 `>= 当前` 的状态信号。 */
      editId: number;
      partial: string;
    }
  | { type: "segmentCleaned"; segmentId: number; cleaned: string; editId: number }
  | {
      /** 同一次 LLM 请求覆盖的片段批次（同一说话人的全部未整理片段）。 */
      type: "segmentsCleaned";
      segmentIds: number[];
      cleaned: string;
      editId: number;
    }
  | { type: "cleanupFailed"; segmentId: number }
  | { type: "minutesReady"; minutes: string };

/** Rust 端 emit 的 engine 事件流事件名（与 `src-tauri/src/pipeline.rs` 保持一致）。 */
export const ENGINE_EVENT = "engine://event";

/** 壳层运行状态事件名（ASR 模式等，与 `src-tauri/src/pipeline.rs` 保持一致）。 */
export const STATUS_EVENT = "engine://status";

/** 壳层状态负载：mode = sherpa（本地 ASR）| cloud（云端 ASR）| mock（合成转写演示）。 */
export interface StatusPayload {
  mode: "sherpa" | "cloud" | "mock";
  reason?: string;
}
