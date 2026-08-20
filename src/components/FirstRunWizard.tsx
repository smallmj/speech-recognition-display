/**
 * 首次运行初始化向导（T14 + T16）：分步检测/下载，每完成一项打钩，确认后进主界面。
 *
 * - 本地模式：选择 ASR / 说话人 embedding 模型（embedding 可选「无」）与镜像 →
 *   运行环境检测 → 下载所选 ASR → 下载所选 embedding → 确认进入主界面；
 * - 云端模式：填写 Deepgram 兼容云端配置并通过后端校验 → 确认进入主界面。
 * - 模型选择与镜像统一存 `model-config.json`（与设置「模型」页共享），改动即记忆。
 * - 下载进度：运行环境步骤走 `first-run://progress`，模型下载进度走
 *   `model://progress`（与设置页共用同一事件）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { subscribe } from "../tauriEvent";
import type { AsrConfig } from "./AsrConfigPanel";
import {
  MODEL_PROGRESS_EVENT,
  type DownloadMirror,
  type ModelConfig,
  type ModelInfo,
  type ModelProgress,
} from "./ModelCatalogPanel";
import "./FirstRunWizard.css";

/** 与 Rust `src-tauri/src/first_run.rs` 的 FIRST_RUN_EVENT 保持一致。 */
export const FIRST_RUN_EVENT = "first-run://progress";

export type FirstRunMode = "local" | "cloud";

export interface FirstRunConfig {
  completed: boolean;
  mode: FirstRunMode;
}

type StepId = "runtime" | "asr" | "embedding";
type StepStatus = "idle" | "running" | "done" | "failed";

interface StepState {
  status: StepStatus;
  message: string;
  progress: number;
  file: number | null;
  fileCount: number | null;
}

interface FirstRunProgress {
  step: StepId | "setup";
  status: "running" | "done" | "failed";
  progress?: number;
  file?: number | null;
  fileCount?: number | null;
  message?: string;
}

interface FirstRunWizardProps {
  onComplete: () => void;
  onLater: () => void;
}

const EMPTY_STEP: StepState = {
  status: "idle",
  message: "",
  progress: 0,
  file: null,
  fileCount: null,
};

const STEP_META: { id: StepId; title: string; desc: string }[] = [
  { id: "runtime", title: "运行环境检测", desc: "Python + sherpa-onnx 运行时" },
  { id: "asr", title: "下载 ASR 模型", desc: "按所选模型下载" },
  { id: "embedding", title: "下载说话人模型", desc: "可选，按音色区分说话人" },
];

function asStepState(evt: FirstRunProgress): StepState | null {
  if (evt.step !== "runtime" && evt.step !== "asr" && evt.step !== "embedding") return null;
  return {
    status: evt.status,
    message: evt.message ?? "",
    progress: evt.progress ?? 0,
    file: evt.file ?? null,
    fileCount: evt.fileCount ?? null,
  };
}

function modelStepStatus(status: ModelProgress["status"]): StepStatus {
  switch (status) {
    case "running":
      return "running";
    case "done":
      return "done";
    case "cancelled":
      return "failed";
    default:
      return "failed";
  }
}

export default function FirstRunWizard({ onComplete, onLater }: FirstRunWizardProps) {
  const [mode, setMode] = useState<FirstRunMode>("local");
  const [mirror, setMirror] = useState<DownloadMirror>("hf-mirror");
  const [autoFallback, setAutoFallback] = useState(true);
  const [asrModelId, setAsrModelId] = useState("");
  const [embeddingModelId, setEmbeddingModelId] = useState<string | null>("");
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [cloud, setCloud] = useState<AsrConfig>({
    source: "local",
    cloudEndpoint: "wss://api.deepgram.com/v1/listen",
    cloudApiKey: "",
    cloudModel: "nova-3",
    cloudLanguage: "multi",
  });
  const [steps, setSteps] = useState<Record<StepId, StepState>>({
    runtime: EMPTY_STEP,
    asr: EMPTY_STEP,
    embedding: EMPTY_STEP,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [loaded, setLoaded] = useState(false);

  // 订阅回调需要最新选择；用 ref 避免闭包过期。
  const asrModelIdRef = useRef(asrModelId);
  const embeddingModelIdRef = useRef(embeddingModelId);
  useEffect(() => {
    asrModelIdRef.current = asrModelId;
  }, [asrModelId]);
  useEffect(() => {
    embeddingModelIdRef.current = embeddingModelId;
  }, [embeddingModelId]);

  const asrModels = useMemo(() => models.filter((m) => m.kind === "asr"), [models]);
  const embeddingModels = useMemo(() => models.filter((m) => m.kind === "embedding"), [models]);

  useEffect(() => {
    Promise.all([
      invoke<FirstRunConfig>("load_first_run_config"),
      invoke<ModelConfig>("load_model_config"),
      invoke<AsrConfig>("load_asr_config"),
      invoke<ModelInfo[]>("list_models"),
    ])
      .then(([first, modelCfg, asrCfg, modelList]) => {
        setMode(first.mode);
        setMirror(modelCfg.mirror);
        setAutoFallback(modelCfg.autoFallbackMirror);
        setAsrModelId(modelCfg.asrModel || "");
        setEmbeddingModelId(modelCfg.embeddingModel);
        setCloud(asrCfg);
        setModels(modelList);
        setLoaded(true);
      })
      .catch((e) => setError(`读取初始化状态失败: ${String(e)}`));

    return subscribe(FIRST_RUN_EVENT, (payload) => {
      const evt = payload as FirstRunProgress;
      if (evt.status === "failed") {
        setError(evt.message ?? "初始化失败");
        setBusy(false);
      }
      const next = asStepState(evt);
      if (next) {
        setSteps((prev) => ({ ...prev, [evt.step]: next }));
      }
    });
  }, []);

  // 模型下载进度（与设置「模型」页共用 `model://progress`）。
  useEffect(() => {
    return subscribe(MODEL_PROGRESS_EVENT, (payload) => {
      const evt = payload as ModelProgress;
      const stepId =
        evt.modelId === asrModelIdRef.current
          ? "asr"
          : evt.modelId === embeddingModelIdRef.current
            ? "embedding"
            : null;
      if (!stepId) return;
      setSteps((prev) => ({
        ...prev,
        [stepId]: {
          status: modelStepStatus(evt.status),
          message: evt.message,
          progress: evt.progress ?? 0,
          file: evt.file ?? null,
          fileCount: evt.fileCount ?? null,
        },
      }));
      if (evt.status === "done" || evt.status === "failed" || evt.status === "cancelled") {
        if (evt.status === "failed" || evt.status === "cancelled") {
          setError(evt.message ?? "模型下载失败");
          setBusy(false);
        }
        invoke<ModelInfo[]>("list_models")
          .then(setModels)
          .catch(() => {});
      }
    });
  }, []);

  // 模式/模型/镜像选择即时记忆：即使未完成或「稍后再配」，下次启动也回填本次选择。
  useEffect(() => {
    if (!loaded) return;
    invoke("save_first_run_preferences", { mode }).catch((e) =>
      console.warn("[first-run] 保存初始化偏好失败:", e),
    );
  }, [loaded, mode]);

  useEffect(() => {
    if (!loaded) return;
    if (asrModelId === "") return;
    invoke("save_model_config", {
      config: {
        asrModel: asrModelId,
        embeddingModel: embeddingModelId,
        mirror,
        autoFallbackMirror: autoFallback,
      },
    }).catch((e) => console.warn("[first-run] 保存模型配置失败:", e));
  }, [loaded, asrModelId, embeddingModelId, mirror, autoFallback]);

  const embeddingSkipped = embeddingModelId == null;
  const localReady = useMemo(
    () =>
      steps.runtime.status === "done" &&
      steps.asr.status === "done" &&
      (embeddingSkipped || steps.embedding.status === "done"),
    [steps, embeddingSkipped],
  );

  useEffect(() => {
    if (localReady) setBusy(false);
  }, [localReady]);

  const startSetup = useCallback(async () => {
    setError("");
    setBusy(true);
    setSteps({ runtime: EMPTY_STEP, asr: EMPTY_STEP, embedding: EMPTY_STEP });
    try {
      await invoke("run_first_run_setup", {
        asrModelId,
        embeddingModelId,
      });
    } catch (e) {
      setError(`启动初始化失败: ${String(e)}`);
      setBusy(false);
    }
  }, [asrModelId, embeddingModelId]);

  const confirm = useCallback(async () => {
    setError("");
    setConfirming(true);
    try {
      if (mode === "cloud") {
        const config: AsrConfig = {
          ...cloud,
          source: "cloud",
          cloudEndpoint: cloud.cloudEndpoint.trim(),
          cloudApiKey: cloud.cloudApiKey.trim(),
          cloudModel: cloud.cloudModel.trim(),
          cloudLanguage: cloud.cloudLanguage.trim(),
        };
        await invoke("save_asr_config", { config });
      }
      await invoke("complete_first_run", { mode });
      onComplete();
    } catch (e) {
      setError(String(e));
      setConfirming(false);
    }
  }, [cloud, mode, onComplete]);

  const cloudInvalid =
    !cloud.cloudEndpoint.trim() ||
    !/^wss?:\/\//.test(cloud.cloudEndpoint.trim()) ||
    !cloud.cloudApiKey.trim() ||
    !cloud.cloudModel.trim() ||
    !cloud.cloudLanguage.trim();

  const modelSize = (id: string) => {
    const m = models.find((x) => x.id === id);
    if (!m) return "";
    return `${((m.sizeBytes ?? 0) / (1024 * 1024)).toFixed(0)}MB${m.downloaded ? " · 已下载" : ""}`;
  };

  return (
    <div className="first-run-screen">
      <div className="first-run-panel">
        <header className="first-run-header">
          <h1 className="first-run-title">首次运行初始化</h1>
          <p className="first-run-subtitle">
            选择识别模型并检测运行环境；每完成一项自动打钩，确认后进入主界面。
          </p>
        </header>

        <div className="first-run-mode" role="radiogroup" aria-label="识别模式">
          <button
            type="button"
            className={`first-run-mode-btn ${mode === "local" ? "is-active" : ""}`}
            onClick={() => setMode("local")}
          >
            本地 ASR
          </button>
          <button
            type="button"
            className={`first-run-mode-btn ${mode === "cloud" ? "is-active" : ""}`}
            onClick={() => setMode("cloud")}
          >
            云端 ASR
          </button>
        </div>

        {mode === "local" ? (
          <>
            <div className="first-run-pick">
              <span className="first-run-label">ASR 识别模型</span>
              <select
                value={asrModelId}
                onChange={(e) => setAsrModelId(e.target.value)}
                disabled={!loaded}
                aria-label="选择 ASR 模型"
              >
                {asrModels.length === 0 && <option value="">加载模型目录…</option>}
                {asrModels.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.displayName} · {modelSize(m.id)}
                  </option>
                ))}
              </select>
              {asrModelId && !asrModels.find((m) => m.id === asrModelId)?.downloaded && (
                <p className="first-run-hint-warn">该模型尚未下载，开始配置时将自动下载。</p>
              )}
            </div>

            <div className="first-run-pick">
              <span className="first-run-label">说话人 embedding 模型</span>
              <select
                value={embeddingModelId ?? ""}
                onChange={(e) => setEmbeddingModelId(e.target.value || null)}
                disabled={!loaded}
                aria-label="选择说话人 embedding 模型"
              >
                <option value="">无（不区分说话人）</option>
                {embeddingModels.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.displayName} · {modelSize(m.id)}
                  </option>
                ))}
              </select>
              <p className="first-run-hint">
                用于按音色区分说话人；选「无」时所有发言归为「说话人 1」。
              </p>
            </div>

            <div className="first-run-mirror">
              <span className="first-run-label">模型下载源</span>
              <div className="first-run-mode" role="radiogroup" aria-label="下载镜像">
                <button
                  type="button"
                  className={`first-run-mode-btn ${mirror === "huggingface" ? "is-active" : ""}`}
                  onClick={() => setMirror("huggingface")}
                >
                  HuggingFace 官方
                </button>
                <button
                  type="button"
                  className={`first-run-mode-btn ${mirror === "hf-mirror" ? "is-active" : ""}`}
                  onClick={() => setMirror("hf-mirror")}
                >
                  hf-mirror 国内镜像
                </button>
              </div>
              <label className="first-run-auto-fallback">
                <input
                  type="checkbox"
                  checked={autoFallback}
                  onChange={(e) => setAutoFallback(e.target.checked)}
                />
                <span>下载失败时自动切换到另一镜像重试</span>
              </label>
            </div>

            <ol className="first-run-steps">
              {STEP_META.map((meta) => {
                const state = steps[meta.id];
                return (
                  <li key={meta.id} className={`first-run-step is-${state.status}`}>
                    <span className="first-run-step-icon" aria-hidden>
                      {state.status === "done" ? "✓" : state.status === "running" ? "…" : "○"}
                    </span>
                    <div className="first-run-step-body">
                      <div className="first-run-step-head">
                        <span className="first-run-step-title">{meta.title}</span>
                        {state.file != null && state.fileCount != null && (
                          <span className="first-run-step-count">
                            文件 {Math.min(state.file, state.fileCount)}/{state.fileCount}
                          </span>
                        )}
                      </div>
                      <p className="first-run-step-desc">{meta.desc}</p>
                      {(state.status === "running" || state.status === "done") &&
                        state.progress > 0 && (
                          <div className="first-run-progress" role="progressbar">
                            <div
                              className="first-run-progress-fill"
                              style={{ width: `${Math.round(state.progress * 100)}%` }}
                            />
                          </div>
                        )}
                      {state.message && <p className="first-run-step-message">{state.message}</p>}
                    </div>
                  </li>
                );
              })}
            </ol>
          </>
        ) : (
          <div className="first-run-cloud">
            <label className="first-run-field">
              <span>云端端点</span>
              <input
                value={cloud.cloudEndpoint}
                onChange={(e) => setCloud({ ...cloud, cloudEndpoint: e.target.value })}
                placeholder="wss://api.deepgram.com/v1/listen"
                spellCheck={false}
              />
            </label>
            <label className="first-run-field">
              <span>云端 API Key</span>
              <input
                type="password"
                value={cloud.cloudApiKey}
                onChange={(e) => setCloud({ ...cloud, cloudApiKey: e.target.value })}
                placeholder="Deepgram API Key"
                spellCheck={false}
              />
            </label>
            <label className="first-run-field">
              <span>云端模型</span>
              <input
                value={cloud.cloudModel}
                onChange={(e) => setCloud({ ...cloud, cloudModel: e.target.value })}
                placeholder="nova-3"
                spellCheck={false}
              />
            </label>
            <label className="first-run-field">
              <span>云端语言</span>
              <input
                value={cloud.cloudLanguage}
                onChange={(e) => setCloud({ ...cloud, cloudLanguage: e.target.value })}
                placeholder="multi（中英混合）/ zh / en"
                spellCheck={false}
              />
            </label>
          </div>
        )}

        {error && (
          <p className="first-run-error" role="alert">
            {error}
          </p>
        )}

        <footer className="first-run-actions">
          {mode === "local" ? (
            <button
              type="button"
              className="first-run-primary"
              onClick={localReady ? confirm : startSetup}
              disabled={busy || confirming || (loaded && asrModelId === "")}
            >
              {busy ? "配置中…" : localReady ? "确认并进入主界面" : "开始配置"}
            </button>
          ) : (
            <button
              type="button"
              className="first-run-primary"
              onClick={confirm}
              disabled={cloudInvalid || confirming}
            >
              {confirming ? "保存中…" : "保存并进入主界面"}
            </button>
          )}
          <button type="button" className="first-run-secondary" onClick={onLater}>
            稍后再配
          </button>
        </footer>
      </div>
    </div>
  );
}
