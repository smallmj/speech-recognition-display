import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import logoMark from "../brand/logo-mark-256.png";
import DualTrackView, {
  reduceEvents,
  useEngineEvents,
  type CleanupInterval,
} from "./components/DualTrackView";
import SettingsDialog, { type TabId } from "./components/SettingsDialog";
import { type ModelConfig, type ModelInfo } from "./components/ModelCatalogPanel";
import { type AsrConfig } from "./components/AsrConfigPanel";
import AsrLiveRow from "./components/AsrLiveRow";
import MinutesPanel from "./components/MinutesPanel";
import FirstRunWizard, { type FirstRunConfig } from "./components/FirstRunWizard";
import {
  ENGINE_EVENT,
  STATUS_EVENT,
  type EngineEvent,
  type Segment,
  type StatusPayload,
} from "./engineEvents";
import { profileOf, useSpeakerProfiles, type SpeakerProfiles } from "./speakerProfiles";
import { subscribe } from "./tauriEvent";
import {
  DisplayContext,
  useDisplaySettingsState,
} from "./displaySettings";

/** Rust 端 emit 的 ping 事件负载（调试心跳，T1 遗留）。 */
interface PingPayload {
  type: "ping";
  seq: number;
}

/** T1 阶段前端确认收到的事件名（与 src-tauri/src/bridge.rs 保持一致）。 */
const PING_EVENT = "bridge://ping";

/** 导出用时间格式：毫秒时间戳 → HH:MM:SS（本地时区）。 */
function formatExportTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 把会话片段规约为导出用的 Markdown 字幕记录。
 *
 * 每条发言 = 时间 + 说话人 + 文本；文本优先用整理版 `cleaned`（LLM 开启时），
 * 无整理版（未配置 LLM / 未整理 / 整理失败）时回退原文 `raw`。
 */
function buildTranscriptMarkdown(
  segments: Segment[],
  profiles: SpeakerProfiles,
): string {
  return segments
    .map((seg) => {
      const profile = profileOf(profiles, seg.speakerId);
      const name = profile.name ?? `说话人 ${seg.speakerId}`;
      const text =
        seg.cleaned && seg.cleaned.trim().length > 0 ? seg.cleaned.trim() : seg.raw.trim();
      return `### [${formatExportTime(seg.ts)}] ${name}\n${text}`;
    })
    .join("\n\n");
}

/** 与 Rust `src-tauri/src/app_settings.rs` 的 `AppSettings` 对齐（camelCase）。 */
interface AppSettings {
  cleanupIntervalSeconds: 5 | 10;
  llmCleanupEnabled: boolean;
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

export default function App() {
  const display = useDisplaySettingsState();
  const { profiles } = useSpeakerProfiles();

  const [bridgeReady, setBridgeReady] = useState(false);
  const [engineLive, setEngineLive] = useState(false);
  const [asrMode, setAsrMode] = useState<StatusPayload["mode"] | null>(null);
  const [asrError, setAsrError] = useState<string | null>(null);
  const [scdStatus, setScdStatus] = useState<StatusPayload["scd"] | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // T14 首次运行：未完成时启动直接进向导；「稍后再配」本次跳过，下次启动仍回向导。
  const [firstRun, setFirstRun] = useState<FirstRunConfig | null>(null);
  const [skipFirstRun, setSkipFirstRun] = useState(false);
  // T12/T16 常规设置：整理间隔 + 启用 LLM 整理，App 侧为唯一写入方（避免
  // 多写入方互相覆盖）；切换后保存到 Rust `app-settings.json` 并即时生效。
  const [appSettings, setAppSettings] = useState<AppSettings>({
    cleanupIntervalSeconds: 5,
    llmCleanupEnabled: true,
  });
  const appSettingsRef = useRef(appSettings);
  useEffect(() => {
    appSettingsRef.current = appSettings;
  }, [appSettings]);
  const cleanupInterval = appSettings.cleanupIntervalSeconds;
  // 打开设置时希望落到的标签页（如「缺模型 → 模型页下载」）。
  const [settingsTab, setSettingsTab] = useState<TabId | null>(null);
  // T10 会话状态机：识别中 → 停止/生成纪要 → 纪要已生成 →（开始识别）识别中。
  const [sessionStatus, setSessionStatus] = useState<
    "idle" | "recognizing" | "stopping" | "generating" | "ready"
  >("idle");
  const [minutes, setMinutes] = useState<string | null>(null);

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
      setScdStatus(st.mode === "sherpa" ? (st.scd ?? null) : null);
      if (st.mode === "error") {
        // T16：所选本地 ASR 缺失等配置错误 → 阻止识别并回到「未开始」。
        setAsrError(st.reason ?? "ASR 配置错误");
        setSessionStatus("idle");
      } else {
        setAsrError(null);
        if (st.reason) {
          console.warn("[status] ASR 回退原因:", st.reason);
        }
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

  // 加载已保存的常规设置（整理间隔 + LLM 开关），重启后保留。
  useEffect(() => {
    invoke<AppSettings>("load_app_settings")
      .then(setAppSettings)
      .catch((e) => console.warn("[settings] 加载常规配置失败:", e));
  }, []);

  const saveAppSettings = useCallback((next: AppSettings) => {
    invoke("save_app_settings", { settings: next }).catch((e) =>
      console.error("[settings] 保存常规配置失败:", e),
    );
  }, []);

  // 切换整理间隔：立即更新本地展示状态，并保存到 Rust 端 app-settings.json；
  // 后台驱动线程每秒轮询该配置并热更新整理节奏，无需重启。
  const handleCleanupIntervalChange = useCallback(
    (seconds: CleanupInterval) => {
      const next = { ...appSettingsRef.current, cleanupIntervalSeconds: seconds };
      setAppSettings(next);
      saveAppSettings(next);
    },
    [saveAppSettings],
  );

  // 切换「启用 LLM 整理」：保存后后台驱动线程即时生效（关闭整理/纪要，开启恢复）。
  const handleLlmCleanupEnabledChange = useCallback(
    (enabled: boolean) => {
      const next = { ...appSettingsRef.current, llmCleanupEnabled: enabled };
      setAppSettings(next);
      saveAppSettings(next);
    },
    [saveAppSettings],
  );

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

  // T14：启动读取首次运行状态；已完成直接进主界面，未完成进向导。
  useEffect(() => {
    invoke<FirstRunConfig>("load_first_run_config")
      .then(setFirstRun)
      .catch((e) => {
        console.warn("[first-run] 读取初始化状态失败:", e);
        setFirstRun({ completed: false, mode: "local" });
      });
  }, []);

  const handleReinitialize = useCallback(() => {
    setSettingsOpen(false);
    setSkipFirstRun(false);
    setFirstRun({ completed: false, mode: "local" });
    invoke("reset_first_run").catch((e) =>
      console.warn("[first-run] 重置初始化状态失败:", e),
    );
  }, []);

  // T9 主事件源：整理管线驱动（真实/合成转写 → 真实 LLM → 双轨事件流）。
  const events = useEngineEvents();

  // T10 会话作用域事件：只保留最近一次 sessionStarted 之后的事件。重新开始
  // 会话时 engine 重建管线（片段 id 从 0 复用），据此切掉上一会话的旧事件，
  // 避免新旧片段混排。
  const sessionEvents = useMemo(() => {
    let idx = events.length;
    for (let i = events.length - 1; i >= 0; i--) {
      if (events[i].type === "sessionStarted") {
        idx = i;
        break;
      }
    }
    return events.slice(idx);
  }, [events]);

  // T11 导出：把本会话片段规约为 Markdown 字幕记录（整理版优先，无整理版回退原文）。
  const sessionSegments = useMemo(() => {
    const map = reduceEvents(sessionEvents);
    return [...map.values()].sort((a, b) => a.id - b.id);
  }, [sessionEvents]);
  const transcript = useMemo(
    () => buildTranscriptMarkdown(sessionSegments, profiles),
    [sessionSegments, profiles],
  );

  // 以最近一条会话事件驱动状态机：sessionStarted 重置，sessionStopped 进入
  // 「正在生成纪要」，minutesReady 进入「纪要已生成」。
  const lastSessionEvent: EngineEvent | undefined = sessionEvents[sessionEvents.length - 1];
  useEffect(() => {
    if (!lastSessionEvent) return;
    switch (lastSessionEvent.type) {
      case "sessionStarted":
        setSessionStatus("recognizing");
        setMinutes(null);
        break;
      case "sessionStopped":
        setSessionStatus("generating");
        break;
      case "minutesReady":
        setSessionStatus("ready");
        setMinutes(lastSessionEvent.minutes);
        break;
      default:
        break;
    }
  }, [lastSessionEvent]);

  // 按钮点击：即时切换本地状态，Rust 驱动线程随后经事件流确认。
  const stopSession = () => {
    setSessionStatus("stopping");
    invoke("stop_session").catch((e) => console.error("[session] stop_session failed:", e));
  };

  const startSession = async () => {
    setSessionStatus("recognizing");
    setMinutes(null);
    // T16：来源=本地且所选 ASR 未下载 → 不开始，打开设置「模型」页引导下载
    // （后端也会兜底阻止并提示）。
    try {
      const asrCfg = await invoke<AsrConfig>("load_asr_config");
      if (asrCfg.source === "local") {
        const cfg = await invoke<ModelConfig>("load_model_config");
        const models = await invoke<ModelInfo[]>("list_models");
        const selected = models.find((m) => m.id === cfg.asrModel);
        if (selected && !selected.downloaded) {
          setSessionStatus("idle");
          setSettingsTab("model");
          setSettingsOpen(true);
          return;
        }
      }
    } catch (e) {
      console.warn("[session] 模型预检失败（交由后端兜底）:", e);
    }
    invoke("start_session").catch((e) => console.error("[session] start_session failed:", e));
  };

  const sessionBadge = {
    idle: { cls: "badge-wait", text: "未开始" },
    recognizing: { cls: "badge-on", text: "识别中" },
    stopping: { cls: "badge-off", text: "正在停止…" },
    generating: { cls: "badge-off", text: "正在生成纪要…" },
    ready: { cls: "badge-on", text: "纪要已生成" },
  }[sessionStatus];

  if (firstRun === null) {
    return <div className="app-loading">正在启动…</div>;
  }

  if (!skipFirstRun && !firstRun.completed) {
    return (
      <FirstRunWizard
        onComplete={() => setSkipFirstRun(true)}
        onLater={() => setSkipFirstRun(true)}
      />
    );
  }

  return (
    <DisplayContext.Provider value={display}>
      <div className="app">
        <header className="app-header">
          <img src={logoMark} alt="" className="app-logo" />
          <h1>语见 · 实时字幕</h1>
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
                : asrMode === "mock" || asrMode === "error"
                  ? "badge-off"
                  : "badge-wait"
            }`}
            title={asrError ?? undefined}
          >
            {asrMode === "sherpa"
              ? "本地 ASR（sherpa-onnx）"
              : asrMode === "cloud"
                ? "云端 ASR（流式）"
                : asrMode === "mock"
                  ? "演示模式（合成转写）"
                  : asrMode === "error"
                    ? "ASR 配置错误"
                    : "ASR 初始化中…"}
          </span>
          {asrMode === "sherpa" && scdStatus && (
            <span className={`badge ${scdStatus === "active" ? "badge-on" : "badge-off"}`}>
              {scdStatus === "active" ? "按音色分人" : "单说话人降级"}
            </span>
          )}
          {!appSettings.llmCleanupEnabled && (
            <span className="badge badge-off" title="LLM 整理已关闭，字幕保持原文；可到设置「LLM 整理」重新启用">
              LLM 整理已关闭
            </span>
          )}
          <span className={`badge ${sessionBadge.cls}`}>{sessionBadge.text}</span>

          <div className="app-header-right">
            {/* T10 会话控制：停止并生成纪要 / 开始识别 */}
            <div className="session-controls">
              <button
                type="button"
                className="session-btn is-start"
                onClick={startSession}
                disabled={sessionStatus !== "ready" && sessionStatus !== "idle"}
              >
                ▶ 开始识别
              </button>
              <button
                type="button"
                className="session-btn is-stop"
                onClick={stopSession}
                disabled={sessionStatus !== "recognizing"}
              >
                ⏹ 停止并生成纪要
              </button>
            </div>

            {/* 置顶大字一键开关 */}
            <button
              className={`settings-option ${display.settings.focusMode ? "active" : ""}`}
              onClick={() => display.setFocusMode(!display.settings.focusMode)}
              title="置顶大字模式"
            >
              📌 置顶大字
            </button>

            {/* 设置按钮：打开标签页分组设置对话框（T12） */}
            <button
              className="settings-btn"
              onClick={() => setSettingsOpen((v) => !v)}
              title="设置（标签页分组）"
            >
              ⚙ 设置
            </button>
          </div>
        </header>

        <main className="app-main">
          <DualTrackView
            events={sessionEvents}
            intervalSeconds={cleanupInterval}
            onIntervalChange={handleCleanupIntervalChange}
          />
          <AsrLiveRow />
          <MinutesPanel
            minutes={minutes}
            transcript={transcript}
            generating={sessionStatus === "stopping" || sessionStatus === "generating"}
          />
        </main>

        <footer className="app-footer">
          语见 TalkSee · 听障实时字幕展示系统 · Tauri 2 + React + Rust engine
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

        <SettingsDialog
          open={settingsOpen}
          onClose={closeSettings}
          cleanupInterval={cleanupInterval}
          onCleanupIntervalChange={handleCleanupIntervalChange}
          llmCleanupEnabled={appSettings.llmCleanupEnabled}
          onLlmCleanupEnabledChange={handleLlmCleanupEnabledChange}
          latestMinutes={minutes}
          onReinitialize={handleReinitialize}
          initialTab={settingsTab}
          onInitialTabConsumed={() => setSettingsTab(null)}
        />
      </div>
    </DisplayContext.Provider>
  );
}
