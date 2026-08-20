/**
 * 模型目录面板（T16）：ASR / 说话人 embedding 模型的选择、下载/校验/删除、
 * 进度与取消、镜像与自动回退开关。
 *
 * 与 Rust `src-tauri/src/models.rs` 对齐（camelCase）：
 * - `list_models` → `ModelInfo[]`（含本机下载状态）；
 * - `model-config.json` 经 `load_model_config` / `save_model_config` 读写
 *   （与初始化向导共享同一份配置）；
 * - 下载/取消经 `download_model_async` / `cancel_download`，进度走
 *   `model://progress` 事件（`MODEL_PROGRESS_EVENT`）。
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { subscribe } from "../tauriEvent";
import "./ModelCatalogPanel.css";

/** 与 Rust `MODEL_PROGRESS_EVENT` 保持一致。 */
export const MODEL_PROGRESS_EVENT = "model://progress";

export type ModelKind = "asr" | "embedding";
export type DownloadMirror = "huggingface" | "hf-mirror";

export interface ModelDescription {
  languages: string;
  realtime: string;
  minHardware: string;
  license: string;
  platforms: string;
  notes: string;
}

export interface ModelInfo {
  id: string;
  kind: ModelKind;
  displayName: string;
  dirName: string;
  sizeBytes: number;
  default: boolean;
  downloaded: boolean;
  downloadedFiles: number;
  fileCount: number;
  description: ModelDescription;
}

export interface ModelConfig {
  asrModel: string;
  embeddingModel: string | null;
  mirror: DownloadMirror;
  autoFallbackMirror: boolean;
}

export interface ModelProgress {
  modelId: string;
  status: "running" | "done" | "failed" | "cancelled";
  progress: number;
  file: number | null;
  fileCount: number | null;
  message: string;
}

const DEFAULT_CONFIG: ModelConfig = {
  asrModel: "",
  embeddingModel: "",
  mirror: "hf-mirror",
  autoFallbackMirror: true,
};

/** 把字节数格式化为可读大小。 */
export function formatSize(bytes: number): string {
  if (bytes <= 0) return "未知";
  const mb = bytes / (1024 * 1024);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb.toFixed(0)} MB`;
}

function formatProgress(event: ModelProgress): string {
  return event.message;
}

interface ModelCardProps {
  model: ModelInfo;
  selected: boolean;
  onSelect: () => void;
  progress: ModelProgress | null;
  onDownload: (id: string) => void;
  onCancel: (id: string) => void;
  onDelete: (id: string) => void;
}

function ModelCard({
  model,
  selected,
  onSelect,
  progress,
  onDownload,
  onCancel,
  onDelete,
}: ModelCardProps) {
  const [showDesc, setShowDesc] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const downloading = progress?.status === "running";

  return (
    <div className={`model-card ${selected ? "is-selected" : ""}`}>
      <button
        type="button"
        className="model-card-main"
        onClick={onSelect}
        aria-pressed={selected}
        title={model.dirName}
      >
        <span className="model-card-radio" aria-hidden>
          {selected ? "●" : "○"}
        </span>
        <span className="model-card-title">
          {model.displayName}
          {model.default && <em className="model-card-tag">默认</em>}
        </span>
        <span className="model-card-size">{formatSize(model.sizeBytes)}</span>
        <span className={`model-card-status ${model.downloaded ? "is-ok" : "is-missing"}`}>
          {model.downloaded
            ? "已下载"
            : `未下载${model.fileCount > 0 ? `（${model.downloadedFiles}/${model.fileCount}）` : ""}`}
        </span>
      </button>

      <div className="model-card-body">
        <p className="model-card-hint">
          {model.description.languages} · {model.description.realtime}
        </p>
        {downloading && progress && (
          <div className="model-progress" role="progressbar">
            <div
              className="model-progress-fill"
              style={{ width: `${Math.round(progress.progress * 100)}%` }}
            />
          </div>
        )}
        {progress && (
          <p className="model-card-message">{formatProgress(progress)}</p>
        )}
        {!model.downloaded && !downloading && (
          <button
            type="button"
            className="llm-save"
            onClick={() => onDownload(model.id)}
          >
            下载
          </button>
        )}
        {downloading && (
          <button type="button" className="model-cancel" onClick={() => onCancel(model.id)}>
            取消下载
          </button>
        )}
        {model.downloaded && !downloading && (
          <button
            type="button"
            className="model-delete"
            onClick={() => {
              if (confirmDelete) {
                onDelete(model.id);
                setConfirmDelete(false);
              } else {
                setConfirmDelete(true);
              }
            }}
            title="删除已下载模型以回收磁盘"
          >
            {confirmDelete
              ? selected
                ? "该模型当前被选中，确认删除？"
                : "确认删除？"
              : "删除"}
          </button>
        )}
        <button
          type="button"
          className="model-desc-toggle"
          onClick={() => setShowDesc((v) => !v)}
          aria-expanded={showDesc}
        >
          {showDesc ? "收起说明 ▴" : "模型说明 ▾"}
        </button>
        {showDesc && (
          <dl className="model-desc">
            <dt>语言 / 场景</dt>
            <dd>{model.description.languages}</dd>
            <dt>实时性</dt>
            <dd>{model.description.realtime}</dd>
            <dt>最低硬件</dt>
            <dd>{model.description.minHardware}</dd>
            <dt>许可证</dt>
            <dd>{model.description.license}</dd>
            <dt>适用平台</dt>
            <dd>{model.description.platforms}</dd>
            <dt>备注</dt>
            <dd>{model.description.notes}</dd>
          </dl>
        )}
      </div>
    </div>
  );
}

export default function ModelCatalogPanel() {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [config, setConfig] = useState<ModelConfig>(DEFAULT_CONFIG);
  const [progress, setProgress] = useState<Record<string, ModelProgress>>({});
  const [status, setStatus] = useState("");
  const [saving, setSaving] = useState(false);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(() => {
    invoke<ModelInfo[]>("list_models")
      .then(setModels)
      .catch((e) => setStatus(`读取模型列表失败: ${String(e)}`));
  }, []);

  useEffect(() => {
    invoke<ModelConfig>("load_model_config")
      .then((cfg) => {
        setConfig(cfg);
        setLoaded(true);
      })
      .catch((e) => setStatus(`读取模型配置失败: ${String(e)}`));
    refresh();

    const unsub = subscribe(MODEL_PROGRESS_EVENT, (payload) => {
      const evt = payload as ModelProgress;
      setProgress((prev) => ({ ...prev, [evt.modelId]: evt }));
      if (evt.status === "done" || evt.status === "failed" || evt.status === "cancelled") {
        // 结束后刷新下载状态。
        invoke<ModelInfo[]>("list_models")
          .then(setModels)
          .catch(() => {});
      }
    });
    return () => {
      unsub();
    };
  }, [refresh]);

  const asrModels = useMemo(() => models.filter((m) => m.kind === "asr"), [models]);
  const embeddingModels = useMemo(() => models.filter((m) => m.kind === "embedding"), [models]);

  const download = async (id: string) => {
    setStatus("");
    try {
      await invoke("download_model_async", {
        modelId: id,
        mirror: config.mirror,
        autoFallback: config.autoFallbackMirror,
      });
    } catch (e) {
      setStatus(`启动下载失败: ${String(e)}`);
    }
  };

  const cancel = async (id: string) => {
    await invoke("cancel_download", { modelId: id }).catch((e) =>
      setStatus(`取消失败: ${String(e)}`),
    );
  };

  const remove = async (id: string) => {
    setStatus("");
    try {
      await invoke("delete_model", { modelId: id });
      setStatus("已删除模型（可在需要时重新下载）");
      refresh();
    } catch (e) {
      setStatus(`删除失败: ${String(e)}`);
    }
  };

  const save = async () => {
    setSaving(true);
    setStatus("");
    try {
      await invoke("save_model_config", { config });
      setStatus("已保存 ✓（本地模型改动下次「开始识别」生效）");
    } catch (e) {
      setStatus(`保存失败: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="model-catalog">
      <section className="settings-panel-section">
        <p className="settings-panel-section-title">ASR 识别模型</p>
        <div className="model-list">
          {asrModels.map((m) => (
            <ModelCard
              key={m.id}
              model={m}
              selected={config.asrModel === m.id}
              onSelect={() => setConfig({ ...config, asrModel: m.id })}
              progress={progress[m.id] ?? null}
              onDownload={download}
              onCancel={cancel}
              onDelete={remove}
            />
          ))}
        </div>
        {config.asrModel && !asrModels.find((m) => m.id === config.asrModel)?.downloaded && (
          <p className="model-warn">当前所选 ASR 模型尚未下载，开始识别前请先下载。</p>
        )}
      </section>

      <hr className="settings-panel-divider" />

      <section className="settings-panel-section">
        <p className="settings-panel-section-title">说话人 embedding 模型</p>
        <p className="settings-panel-hint">
          用于按音色区分说话人（SCD）。选择「无」时所有发言归「说话人 1」；模型缺失时自动降级，不影响识别。
        </p>
        <div className="model-list">
          <button
            type="button"
            className={`model-card model-none ${config.embeddingModel === null ? "is-selected" : ""}`}
            onClick={() => setConfig({ ...config, embeddingModel: null })}
            aria-pressed={config.embeddingModel === null}
          >
            <span className="model-card-radio" aria-hidden>
              {config.embeddingModel === null ? "●" : "○"}
            </span>
            <span className="model-card-title">无（不区分说话人）</span>
          </button>
          {embeddingModels.map((m) => (
            <ModelCard
              key={m.id}
              model={m}
              selected={config.embeddingModel === m.id}
              onSelect={() => setConfig({ ...config, embeddingModel: m.id })}
              progress={progress[m.id] ?? null}
              onDownload={download}
              onCancel={cancel}
              onDelete={remove}
            />
          ))}
        </div>
      </section>

      <hr className="settings-panel-divider" />

      <section className="settings-panel-section">
        <p className="settings-panel-section-title">下载设置</p>
        <p className="settings-panel-hint">选择模型下载源；开启自动回退后，主镜像下载失败会自动换另一镜像重试。</p>
        <div className="settings-option-row">
          <button
            type="button"
            className={`settings-option ${config.mirror === "huggingface" ? "active" : ""}`}
            onClick={() => setConfig({ ...config, mirror: "huggingface" })}
          >
            HuggingFace 官方
          </button>
          <button
            type="button"
            className={`settings-option ${config.mirror === "hf-mirror" ? "active" : ""}`}
            onClick={() => setConfig({ ...config, mirror: "hf-mirror" })}
          >
            hf-mirror 国内镜像
          </button>
        </div>
        <div className="settings-toggle-row">
          <span className="settings-toggle-label">自动回退镜像</span>
          <button
            type="button"
            className={`settings-toggle ${config.autoFallbackMirror ? "on" : ""}`}
            onClick={() => setConfig({ ...config, autoFallbackMirror: !config.autoFallbackMirror })}
            aria-label="自动回退镜像"
          >
            <span className="settings-toggle-knob" />
          </button>
        </div>
      </section>

      <div className="llm-actions">
        <button type="button" className="llm-save" onClick={save} disabled={saving || !loaded}>
          {saving ? "保存中…" : "保存模型配置"}
        </button>
        <span className="llm-status" role="status">
          {status}
        </span>
      </div>
    </div>
  );
}
