import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import BubbleFlow from "./components/BubbleFlow";
import { ENGINE_EVENT, STATUS_EVENT, type StatusPayload } from "./engineEvents";
import { subscribe } from "./tauriEvent";

/** Rust 端 emit 的 ping 事件负载（调试心跳，T1 遗留）。 */
interface PingPayload {
  type: "ping";
  seq: number;
}

/** T1 阶段前端确认收到的事件名（与 src-tauri/src/bridge.rs 保持一致）。 */
const PING_EVENT = "bridge://ping";

export default function App() {
  const [bridgeReady, setBridgeReady] = useState(false);
  const [engineLive, setEngineLive] = useState(false);
  const [asrMode, setAsrMode] = useState<StatusPayload["mode"] | null>(null);

  // T1 调试心跳：bridge://ping → 回执，确认 Rust→前端事件桥闭环。
  useEffect(() => {
    return subscribe(PING_EVENT, (payload) => {
      const event = payload as PingPayload;
      setBridgeReady(true);
      invoke("ping_ack", { payload: `seq=${event.seq}` }).catch((e) =>
        console.error("[bridge] ping_ack failed:", e),
      );
    });
  }, []);

  // engine 事件流存活探测：收到任意 engine://event 即认为管线已通。
  useEffect(() => {
    return subscribe(ENGINE_EVENT, () => setEngineLive(true));
  }, []);

  // 壳层运行状态：ASR 模式（真实本地 ASR / 合成转写演示）。
  useEffect(() => {
    return subscribe(STATUS_EVENT, (payload) => {
      const st = payload as StatusPayload;
      setAsrMode(st.mode);
      if (st.reason) {
        console.warn("[status] ASR 回退原因:", st.reason);
      }
    });
  }, []);

  return (
    <div className="app">
      <header className="app-header">
        <h1>实时字幕展示</h1>
        <span className={`badge ${bridgeReady ? "badge-on" : "badge-off"}`}>
          {bridgeReady ? "事件桥已接通" : "等待事件桥…"}
        </span>
        <span className={`badge ${engineLive ? "badge-on" : "badge-off"}`}>
          {engineLive ? "engine 管线运行中" : "等待 engine 事件…"}
        </span>
        <span
          className={`badge ${
            asrMode === "sherpa" ? "badge-on" : asrMode === "mock" ? "badge-off" : "badge-wait"
          }`}
        >
          {asrMode === "sherpa"
            ? "本地 ASR（sherpa-onnx）"
            : asrMode === "mock"
              ? "演示模式（合成转写）"
              : "ASR 初始化中…"}
        </span>
      </header>

      <main className="app-main">
        <BubbleFlow />
      </main>

      <footer className="app-footer">
        听障实时字幕展示系统 · Tauri 2 + React + engine（T4 真实本地 ASR）
      </footer>
    </div>
  );
}
