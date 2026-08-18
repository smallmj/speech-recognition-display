import { useEffect, useRef, useState } from "react";
import { ENGINE_EVENT, type EngineEvent, type Gender } from "../engineEvents";
import { subscribe } from "../tauriEvent";

/** 一条待渲染的气泡。 */
interface Bubble {
  key: number; // segmentId，作 React key
  speakerId: number;
  text: string;
  ts: number;
}

/** 说话人元数据（颜色/性别/头像），来自 SpeakerAssigned 事件。 */
interface SpeakerInfo {
  color: string;
  gender: Gender;
  avatar: string;
}

/** 未登记到说话人时的兜底颜色（正常不会用到：SpeakerAssigned 先于 SegmentAppended）。 */
const FALLBACK_COLOR = "#8e8e93";

/** 头像 emoji 集：按性别随机挑一个，同说话人恒定（记住谁是谁）。 */
const AVATAR_SETS: Record<Gender, string[]> = {
  female: ["👩", "👩🦰", "👩🦱", "👩🦳", "👩🦲", "👧", "👩🏼"],
  male: ["👨", "👨🦰", "👨🦱", "👨🦲", "👨🦳", "👦", "👨🏼"],
  unknown: ["🧑", "🙂", "😀", "😊"],
};

function randomFrom<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

/** 把毫秒时间戳格式化为 HH:MM:SS。 */
function formatTime(ts: number): string {
  if (!ts) return "";
  return new Date(ts).toLocaleTimeString("zh-CN", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/**
 * 气泡流：监听 `engine://event`，把每个 `SegmentAppended` 渲染成按说话人
 * 着色的彩色气泡 + 随机头像；气泡流自动滚动到底，上翻暂停、回到底部恢复
 * （规格用户故事 12/13）。
 */
export default function BubbleFlow() {
  const [bubbles, setBubbles] = useState<Bubble[]>([]);
  const [speakers, setSpeakers] = useState<Map<number, SpeakerInfo>>(new Map());
  const [stuckToBottom, setStuckToBottom] = useState(true);
  const [scrolledUp, setScrolledUp] = useState(false);
  const [count, setCount] = useState(0);
  // 实时识别中间结果（边说边出）；最后更新时间用于「识别中/聆听中」判定。
  const [partial, setPartial] = useState<string>("");
  const partialAtRef = useRef<number>(0);
  const [, setPartialTick] = useState(0);

  const scrollRef = useRef<HTMLDivElement>(null);
  const speakersRef = useRef(speakers);

  // 让事件回调始终读到最新的 speakers 映射（避免闭包过期）。
  useEffect(() => {
    speakersRef.current = speakers;
  }, [speakers]);

  // 每 1s 重渲染一次，用于「识别中」状态的过期判定（2s 无 partial 即聆听中）。
  useEffect(() => {
    const timer = setInterval(() => setPartialTick((t) => t + 1), 1000);
    return () => clearInterval(timer);
  }, []);

  // 注册 engine 事件流监听（模块级单例，StrictMode 安全：每个事件名只有一个 Tauri 监听）。
  useEffect(() => {
    return subscribe(ENGINE_EVENT, (payload) => {
      const evt = payload as EngineEvent;

      if (evt.type === "speakerAssigned") {
          setSpeakers((prev) => {
            const next = new Map(prev);
            const existing = next.get(evt.speakerId);
            if (!existing) {
              next.set(evt.speakerId, {
                color: evt.color,
                gender: evt.gender,
                avatar: randomFrom(AVATAR_SETS[evt.gender] ?? AVATAR_SETS.unknown),
              });
            }
            return next;
          });
        } else if (evt.type === "segmentAppended") {
          const seg = evt.segment;
          const info = speakersRef.current.get(seg.speakerId);
          setBubbles((prev) => [
            ...prev,
            {
              key: seg.id,
              speakerId: seg.speakerId,
              text: seg.raw,
              ts: seg.ts,
            },
          ]);
          setCount((c) => c + 1);
          // 归属信息先到；万一缺失用兜底颜色，后续 speakerAssigned 再来也会登记。
          if (!info) {
            console.warn("[bubble] segment 先于 speakerAssigned 到达:", seg.speakerId);
          }
        } else if (evt.type === "partialResult") {
          setPartial(evt.text);
          partialAtRef.current = Date.now();
        }
      });
  }, []);

  // 新气泡到达时，若处于「跟随底部」状态则自动滚到底。
  useEffect(() => {
    if (!stuckToBottom || !scrollRef.current) return;
    const el = scrollRef.current;
    el.scrollTop = el.scrollHeight;
  }, [bubbles, stuckToBottom]);

  // 滚动事件：在底部则恢复跟随，离开底部则暂停并显示「回到最新」。
  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    setStuckToBottom(atBottom);
    setScrolledUp(!atBottom);
  }

  function jumpToLatest() {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    setStuckToBottom(true);
    setScrolledUp(false);
  }

  return (
    <div className="bubble-flow-wrap">
      <div className="bubble-flow" ref={scrollRef} onScroll={handleScroll}>
        {bubbles.length === 0 && (
          <div className="bubble-empty">
            <p className="bubble-empty-title">等待字幕…</p>
            <p className="bubble-empty-sub">engine 冒烟管线（合成转写）即将把对话渲染为气泡。</p>
          </div>
        )}

        {bubbles.map((b) => {
          const info = speakers.get(b.speakerId);
          const color = info?.color ?? FALLBACK_COLOR;
          const avatar = info?.avatar ?? "🧑";
          return (
            <div className="bubble-row" key={b.key}>
              <div
                className="bubble-avatar"
                style={{ background: `linear-gradient(135deg, ${color}, ${color}cc)` }}
                aria-hidden
              >
                {avatar}
              </div>
              <div className="bubble-column">
                <div className="bubble-meta">
                  <span className="bubble-speaker" style={{ color }}>
                    说话人 {b.speakerId}
                  </span>
                  <span className="bubble-time">{formatTime(b.ts)}</span>
                </div>
                <div
                  className="bubble-text"
                  style={{
                    borderLeftColor: color,
                    background: `${color}1f`,
                  }}
                >
                  {b.text}
                </div>
              </div>
            </div>
          );
        })}
      </div>

      {scrolledUp && (
        <button type="button" className="back-to-latest" onClick={jumpToLatest}>
          ↓ 回到最新
        </button>
      )}

      {/* 实时识别状态行：partial 边说边出；2s 无更新回到「聆听中」 */}
      {partial && Date.now() - partialAtRef.current < 2000 ? (
        <div className="asr-live" aria-live="polite">
          <span className="asr-live-dot" aria-hidden />
          <span className="asr-live-label">识别中</span>
          <span className="asr-live-text">{partial}</span>
        </div>
      ) : null}

      <footer className="bubble-status">
        已渲染 <strong>{count}</strong> 条气泡 · {speakers.size} 位说话人
      </footer>
    </div>
  );
}
