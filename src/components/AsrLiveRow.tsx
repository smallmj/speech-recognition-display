/**
 * 实时识别状态行（T4）：真实 ASR 的 partial 边说边出。
 *
 * 监听 `engine://event` 的 `partialResult`（真实 ASR 实时中间结果，final
 * 定稿后由 `segmentAppended` 携带完整片段）；2s 无新 partial 回到「聆听中」
 * （隐藏状态行）。演示模式（合成转写）不产生 partial，本组件不渲染。
 *
 * 原 BubbleFlow 内的状态行抽出为独立组件，与双轨展示（DualTrackView）共存。
 */

import { useEffect, useRef, useState } from "react";
import { ENGINE_EVENT, type EngineEvent } from "../engineEvents";
import { subscribe } from "../tauriEvent";

export default function AsrLiveRow() {
  const [partial, setPartial] = useState<string>("");
  const partialAtRef = useRef<number>(0);
  const [, setTick] = useState(0);

  // 每 1s 重渲染一次，用于「识别中」状态的过期判定（2s 无 partial 即聆听中）。
  useEffect(() => {
    const timer = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(timer);
  }, []);

  // 注册 engine 事件流监听（模块级单例，StrictMode 安全）。
  useEffect(() => {
    return subscribe(ENGINE_EVENT, (payload) => {
      const evt = payload as EngineEvent;
      if (evt.type === "partialResult") {
        setPartial(evt.text);
        partialAtRef.current = Date.now();
      }
    });
  }, []);

  // 2s 无更新 → 隐藏（回到「聆听中」）。
  if (!partial || Date.now() - partialAtRef.current >= 2000) {
    return null;
  }

  return (
    <div className="asr-live" aria-live="polite">
      <span className="asr-live-dot" aria-hidden />
      <span className="asr-live-label">识别中</span>
      <span className="asr-live-text">{partial}</span>
    </div>
  );
}
