import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import DualTrackView, { useEngineEvents } from "./components/DualTrackView";
import AsrConfigPanel from "./components/AsrConfigPanel";
import LlmConfigPanel from "./components/LlmConfigPanel";
import AsrLiveRow from "./components/AsrLiveRow";
import { ENGINE_EVENT, STATUS_EVENT, type StatusPayload } from "./engineEvents";
import { subscribe } from "./tauriEvent";
import {
  DisplayContext,
  useDisplaySettings,
  useDisplaySettingsState,
  type FontFamily,
  type FontSize,
  type TextColor,
  type Theme,
  DISPLAY_LABELS,
} from "./displaySettings";

/** Rust 端 emit 的 ping 事件负载（调试心跳，T1 遗留）。 */
interface PingPayload {
  type: "ping";
  seq: number;
}

/** T1 阶段前端确认收到的事件名（与 src-tauri/src/bridge.rs 保持一致）。 */
const PING_EVENT = "bridge://ping";

// ---------------------------------------------------------------------------
// 设置面板（在 Provider 内部渲染，可直接使用 useDisplaySettings）
// ---------------------------------------------------------------------------

function SettingsPanel({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { settings, setTheme, setFocusMode, setFontSize, setFontFamily, setTextColor } =
    useDisplaySettings();

  // ESC 关闭
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const themeOptions: { value: Theme; label: string }[] = [
    { value: "auto", label: "跟随系统" },
    { value: "light", label: "浅色" },
    { value: "dark", label: "深色" },
  ];

  const sizeOptions: { value: FontSize; label: string }[] = [
    { value: "small", label: DISPLAY_LABELS.fontSize.small },
    { value: "medium", label: DISPLAY_LABELS.fontSize.medium },
    { value: "large", label: DISPLAY_LABELS.fontSize.large },
    { value: "xlarge", label: DISPLAY_LABELS.fontSize.xlarge },
  ];

  const familyOptions: { value: FontFamily; label: string }[] = [
    { value: "default", label: DISPLAY_LABELS.fontFamily.default },
    { value: "pingfang", label: DISPLAY_LABELS.fontFamily.pingfang },
    { value: "songti", label: DISPLAY_LABELS.fontFamily.songti },
    { value: "heiti", label: DISPLAY_LABELS.fontFamily.heiti },
    { value: "kaiti", label: DISPLAY_LABELS.fontFamily.kaiti },
  ];

  const colorOptions: { value: TextColor; label: string }[] = [
    { value: "default", label: DISPLAY_LABELS.textColor.default },
    { value: "black", label: DISPLAY_LABELS.textColor.black },
    { value: "darkgray", label: DISPLAY_LABELS.textColor.darkgray },
    { value: "white", label: DISPLAY_LABELS.textColor.white },
    { value: "darkblue", label: DISPLAY_LABELS.textColor.darkblue },
  ];

  return (
    <>
      {/* 覆盖层：点击关闭 */}
      <div className="settings-panel-overlay" onClick={onClose} />
      <div className="settings-panel">
        {/* 主题 */}
        <div className="settings-panel-section">
          <p className="settings-panel-section-title">主题</p>
          <p className="settings-panel-hint">跟随系统或手动固定</p>
          <div className="settings-option-row">
            {themeOptions.map((t) => (
              <button
                key={t.value}
                className={`settings-option ${settings.theme === t.value ? "active" : ""}`}
                onClick={() => setTheme(t.value)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>

        <hr className="settings-panel-divider" />

        {/* 字号 */}
        <div className="settings-panel-section">
          <p className="settings-panel-section-title">字号</p>
          <p className="settings-panel-hint">气泡文字大小</p>
          <div className="settings-option-row">
            {sizeOptions.map((s) => (
              <button
                key={s.value}
                className={`settings-option ${settings.fontSize === s.value ? "active" : ""}`}
                onClick={() => setFontSize(s.value)}
              >
                {s.label}
              </button>
            ))}
          </div>
        </div>

        <hr className="settings-panel-divider" />

        {/* 字体 */}
        <div className="settings-panel-section">
          <p className="settings-panel-section-title">字体</p>
          <p className="settings-panel-hint">气泡文字字体</p>
          <div className="settings-option-row">
            {familyOptions.map((f) => (
              <button
                key={f.value}
                className={`settings-option ${settings.fontFamily === f.value ? "active" : ""}`}
                onClick={() => setFontFamily(f.value)}
              >
                {f.label}
              </button>
            ))}
          </div>
        </div>

        <hr className="settings-panel-divider" />

        {/* 文字颜色 */}
        <div className="settings-panel-section">
          <p className="settings-panel-section-title">文字颜色</p>
          <p className="settings-panel-hint">气泡文字颜色</p>
          <div className="settings-option-row">
            {colorOptions.map((c) => (
              <button
                key={c.value}
                className={`settings-option ${settings.textColor === c.value ? "active" : ""}`}
                onClick={() => setTextColor(c.value)}
              >
                {c.label}
              </button>
            ))}
          </div>
        </div>

        <hr className="settings-panel-divider" />

        {/* 置顶大字 */}
        <div className="settings-panel-section">
          <div className="settings-toggle-row">
            <span className="settings-toggle-label">置顶大字模式</span>
            <button
              className={`settings-toggle ${settings.focusMode ? "on" : ""}`}
              onClick={() => setFocusMode(!settings.focusMode)}
              aria-label="切换置顶大字模式"
            >
              <span className="settings-toggle-knob" />
            </button>
          </div>
          <p className="settings-panel-hint">窗口始终置顶 + 超大字体</p>
        </div>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

export default function App() {
  const display = useDisplaySettingsState();

  const [bridgeReady, setBridgeReady] = useState(false);
  const [engineLive, setEngineLive] = useState(false);
  const [asrMode, setAsrMode] = useState<StatusPayload["mode"] | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);

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

  // 置顶大字模式：同步 Tauri 窗口置顶
  const focusMode = display.settings.focusMode;
  useEffect(() => {
    getCurrentWindow()
      .setAlwaysOnTop(focusMode)
      .catch((e) => console.warn("[display] setAlwaysOnTop 失败:", e));
  }, [focusMode]);

  // 全局 ESC：设置面板打开时先关面板；否则若在置顶大字模式则退出大字模式。
  // （置顶大字模式下头部被隐藏，ESC 是必须的退出途径之一。）
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (settingsOpen) {
        setSettingsOpen(false);
      } else if (focusMode) {
        display.setFocusMode(false);
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [settingsOpen, focusMode, display]);

  const closeSettings = useCallback(() => setSettingsOpen(false), []);

  // T9 主事件源：整理管线驱动（真实/合成转写 → 真实 LLM → 双轨事件流）。
  const events = useEngineEvents();

  return (
    <DisplayContext.Provider value={display}>
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
              asrMode === "sherpa" || asrMode === "cloud"
                ? "badge-on"
                : asrMode === "mock"
                  ? "badge-off"
                  : "badge-wait"
            }`}
          >
            {asrMode === "sherpa"
              ? "本地 ASR（sherpa-onnx）"
              : asrMode === "cloud"
                ? "云端 ASR（流式）"
                : asrMode === "mock"
                  ? "演示模式（合成转写）"
                  : "ASR 初始化中…"}
          </span>

          <div className="app-header-right">
            {/* 置顶大字一键开关 */}
            <button
              className={`settings-option ${display.settings.focusMode ? "active" : ""}`}
              onClick={() => display.setFocusMode(!display.settings.focusMode)}
              title="置顶大字模式"
            >
              📌 置顶大字
            </button>

            {/* 显示设置按钮 */}
            <button
              className="settings-btn"
              onClick={() => setSettingsOpen((v) => !v)}
              title="显示设置"
            >
              ⚙ 显示
            </button>
          </div>
        </header>

        <main className="app-main">
          <AsrConfigPanel />
          <LlmConfigPanel />
          <DualTrackView events={events} />
          <AsrLiveRow />
        </main>

        <footer className="app-footer">
          听障实时字幕展示系统 · Tauri 2 + React + engine（T4 真实本地 ASR → T9 LLM 流式整理 → 双轨展示）
        </footer>

        {/* 置顶大字模式浮动退出按钮：头部隐藏后仍可一键退出（也可按 Esc） */}
        {focusMode && (
          <button
            type="button"
            className="focus-exit"
            onClick={() => display.setFocusMode(false)}
            title="退出置顶大字模式（Esc）"
          >
            ✕ 退出大字
          </button>
        )}

        <SettingsPanel open={settingsOpen} onClose={closeSettings} />
      </div>
    </DisplayContext.Provider>
  );
}
