import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Rust 端 emit 的测试事件负载。 */
interface PingPayload {
  type: "ping";
  seq: number;
}

/** T1 阶段前端确认收到的事件名（与 src-tauri/src/bridge.rs 保持一致）。 */
const PING_EVENT = "bridge://ping";

export default function App() {
  const [pings, setPings] = useState<number[]>([]);
  const [bridgeReady, setBridgeReady] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;

    (async () => {
      // 注册 listen：收到 Rust 端 ping 事件。
      // 验证点：控制台 `[bridge] ping received: {...}` 即链路已通。
      unlisten = await listen<PingPayload>(PING_EVENT, (event) => {
        console.log("[bridge] ping received:", event.payload);
        setBridgeReady(true);
        setPings((prev) => [...prev, event.payload.seq]);

        // 回执给 Rust 端，形成闭环（Rust 日志可见确认）。
        invoke("ping_ack", { payload: `seq=${event.payload.seq}` }).catch((e) =>
          console.error("[bridge] ping_ack failed:", e),
        );
      });
    })();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  return (
    <div className="app">
      <header className="app-header">
        <h1>实时字幕展示</h1>
        <span className={`badge ${bridgeReady ? "badge-on" : "badge-off"}`}>
          {bridgeReady ? "事件桥已接通" : "等待事件桥…"}
        </span>
      </header>

      <main className="placeholder">
        <p className="placeholder-title">T1 工程脚手架 · 占位界面</p>
        <p className="placeholder-sub">
          Web 前端骨架已加载。后续票将把 engine 事件流渲染为说话人气泡。
        </p>
        <p className="placeholder-meta">
          已收到 ping 事件 <strong>{pings.length}</strong> 次
          {pings.length > 0 && `（seq: ${pings.slice(-5).join(", ")}…）`}
        </p>
      </main>

      <footer className="app-footer">听障实时字幕展示系统 · Tauri 2 + React + engine</footer>
    </div>
  );
}
