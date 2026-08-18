/**
 * 双轨展示演示组件：在浏览器内合成 engine 事件，模拟整理管线的
 * 「防抖（2s 无追加）+ 固定节奏（5s/10s）+ 失败回退」语义，
 * 让 `DualTrackView` 无需真实 engine 即可完整演示。
 *
 * 真实链路（T2 事件桥接通后）用 `useEngineEvents()` + `DualTrackView`
 * 即可，本演示只是 T8 阶段的一键验证入口：
 *
 * ```tsx
 * import DualTrackDemo from "./components/DualTrackDemo";
 * <DualTrackDemo />   // 演示模式
 * ```
 *
 * 内部的模拟逻辑刻意对齐 engine/src/cleanup.rs：
 * - 每次「说话」= 追加一条 active 片段（interim，不送 LLM）；
 * - 距最后追加 ≥ 2s（防抖）或距上次节奏触发 ≥ interval（固定节奏）→
 *   把 pending 的 active 片段全部整理（模拟 LLM：补标点）；
 * - 随机小概率模拟整理失败（回退原文）。
 */

import { useEffect, useMemo, useRef, useState } from "react";
import type { EngineEvent, Gender, Segment, SegmentStatus } from "../engineEvents";
import DualTrackView, { type CleanupInterval } from "./DualTrackView";

/** 演示用：模拟 LLM 整理 —— 合并空白、句末补标点（对齐 MockLlmPort）。 */
function mockCleanup(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  if (!collapsed) return "";
  const last = collapsed[collapsed.length - 1];
  if ("。！？；，：…".includes(last)) return collapsed;
  return `${collapsed}。`;
}

/** 演示语料：说话人轮流发言（无标点口语，便于展示补标点高亮）。 */
const SAMPLE_UTTERANCES: Array<{ speakerId: number; raw: string }> = [
  { speakerId: 1, raw: "好的没问题那就这样吧" },
  { speakerId: 2, raw: "我觉得可以不过预算方面还需要再确认一下" },
  { speakerId: 1, raw: "那这个周五之前能出第一版吗" },
  { speakerId: 2, raw: "应该可以我这边协调一下人力" },
  { speakerId: 3, raw: "接口文档我今晚补完明天早上给你" },
  { speakerId: 1, raw: "嗯嗯然后有问题随时群里同步" },
];

/** 说话人颜色（对齐 engine SPEAKER_PALETTE 前几色）。 */
const DEMO_SPEAKER_COLORS: Record<number, string> = {
  1: "#4f8cff",
  2: "#34c759",
  3: "#ff9500",
  4: "#ff3b30",
};

let demoId = 0;
function nextId(): number {
  demoId += 1;
  return demoId;
}

function makeSegment(speakerId: number, raw: string, status: SegmentStatus, ts: number): Segment {
  return {
    id: nextId(),
    speakerId,
    raw,
    status,
    cleaned: null,
    editId: null,
    ts,
    retries: 0,
  };
}

function makeAppended(seg: Segment): EngineEvent {
  return { type: "segmentAppended", segment: seg };
}

/** 演示用：登记说话人（颜色/性别），对齐 engine 先发 SpeakerAssigned。 */
function makeSpeakerAssigned(segmentId: number, speakerId: number, gender: Gender = "unknown"): EngineEvent {
  return {
    type: "speakerAssigned",
    segmentId,
    speakerId,
    isNewSpeaker: true,
    color: DEMO_SPEAKER_COLORS[speakerId] ?? "#8e8e93",
    gender,
  };
}

/**
 * 演示组件：合成事件 + 展示。
 */
export default function DualTrackDemo() {
  const [events, setEvents] = useState<EngineEvent[]>([]);
  const [intervalSeconds, setIntervalSeconds] = useState<CleanupInterval>(5);
  const [autoPlay, setAutoPlay] = useState(false);

  // 演示状态（用 ref 避免闭包过期）：
  // pendingActive = 已追加、等待整理的片段（对应 engine 的 active 片段）
  const pendingActiveRef = useRef<Segment[]>([]);
  const lastAppendRef = useRef<number>(0);
  const lastRhythmRef = useRef<number>(0);

  const push = (make: (ts: number) => EngineEvent) => {
    const ts = Date.now();
    setEvents((prev) => [...prev, make(ts)]);
  };

  /** 模拟「说话」：追加一条 active 片段（interim，不送 LLM）+ 登记说话人。 */
  const speak = () => {
    const sample = SAMPLE_UTTERANCES[Math.floor(Math.random() * SAMPLE_UTTERANCES.length)];
    const seg = makeSegment(sample.speakerId, sample.raw, "active", Date.now());
    pendingActiveRef.current = [...pendingActiveRef.current, seg];
    lastAppendRef.current = Date.now();
    push(() => makeSpeakerAssigned(seg.id, seg.speakerId));
    push(() => makeAppended(seg));
  };

  /** 模拟一次整理触发：pending 的 active 全部冻结并整理（随机失败）。 */
  const cleanupPending = () => {
    const now = Date.now();
    lastRhythmRef.current = now;
    const batch = pendingActiveRef.current;
    if (batch.length === 0) return;
    pendingActiveRef.current = [];

    for (const seg of batch) {
      const fail = Math.random() < 0.12; // 12% 概率模拟整理失败
      if (fail) {
        push(() => ({ type: "cleanupFailed", segmentId: seg.id }));
      } else {
        const cleaned = mockCleanup(seg.raw);
        push(() => ({ type: "segmentCleaned", segmentId: seg.id, cleaned, editId: seg.id }));
      }
    }
  };

  /** 自动演示主循环：模拟 engine 的防抖 + 固定节奏。 */
  useEffect(() => {
    if (!autoPlay) return;
    const timer = window.setInterval(() => {
      const now = Date.now();
      const sinceAppend = now - lastAppendRef.current;
      const sinceRhythm = now - lastRhythmRef.current;
      const debounceMs = 2000;
      const rhythmMs = intervalSeconds * 1000;
      const idle = sinceAppend >= debounceMs;
      const rhythmDue = sinceRhythm >= rhythmMs;
      if (idle || rhythmDue) {
        cleanupPending();
        return; // 本轮不追加，让整理先落库
      }
      speak();
    }, 400);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoPlay, intervalSeconds]);

  // 首次进入：预置几段不同状态的片段，让三种形态（整理版/原文占位/失败）一眼可见。
  useEffect(() => {
    if (events.length > 0) return;
    const base = Date.now();
    const s1 = makeSegment(1, "好的没问题那就这样吧", "cleaned", base - 4000);
    const s2 = makeSegment(2, "我觉得可以不过预算方面还需要再确认一下", "cleaned", base - 3000);
    const s3 = makeSegment(1, "那这个周五之前能出第一版吗", "active", base - 500);
    const s4 = makeSegment(3, "这个部分后续再确认", "failed", base - 1000);
    const cleaned1 = mockCleanup(s1.raw);
    const cleaned2 = mockCleanup(s2.raw);
    setEvents((prev) => [
      ...prev,
      makeSpeakerAssigned(s1.id, s1.speakerId, "female"),
      makeAppended(s1),
      makeSpeakerAssigned(s2.id, s2.speakerId, "male"),
      makeAppended(s2),
      makeSpeakerAssigned(s3.id, s3.speakerId),
      makeAppended(s3),
      makeSpeakerAssigned(s4.id, s4.speakerId, "male"),
      makeAppended(s4),
      { type: "segmentCleaned", segmentId: s1.id, cleaned: cleaned1, editId: 1 },
      { type: "segmentCleaned", segmentId: s2.id, cleaned: cleaned2, editId: 2 },
    ]);
    pendingActiveRef.current = [s3];
    lastAppendRef.current = base - 500;
    lastRhythmRef.current = base;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 控制台也可直接看合成的事件流。
  useEffect(() => {
    if (events.length > 0 && events.length % 3 === 0) {
      console.log("[dualtrack-demo] events:", events.length);
    }
  }, [events]);

  const segmentCount = useMemo(
    () =>
      events.reduce<Set<number>>((acc, e) => {
        if (e.type === "segmentAppended") acc.add(e.segment.id);
        return acc;
      }, new Set<number>()).size,
    [events],
  );

  return (
    <div className="dualtrack-demo">
      <div className="dualtrack-demo-controls">
        <button type="button" onClick={speak}>
          模拟说话
        </button>
        <button type="button" onClick={cleanupPending}>
          立即整理
        </button>
        <button
          type="button"
          className={autoPlay ? "is-on" : ""}
          onClick={() => setAutoPlay((v) => !v)}
        >
          {autoPlay ? "停止自动演示" : "自动演示"}
        </button>
        <span className="dualtrack-demo-hint">
          共 {segmentCount} 条片段 · 自动演示模拟 2s 防抖 + {intervalSeconds}s 节奏
        </span>
      </div>

      <DualTrackView
        events={events}
        intervalSeconds={intervalSeconds}
        onIntervalChange={setIntervalSeconds}
      />
    </div>
  );
}
