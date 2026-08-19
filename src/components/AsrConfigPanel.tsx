/**
 * ASR 来源配置面板（T7）：本地 sherpa-onnx / 云端 Deepgram 兼容流式 ASR。
 *
 * 配置保存到 Rust 端 `asr-config.json`。驱动线程每秒轮询配置并在来源变化时
 * 热替换 AsrPort；整理管线、说话人状态与已显示气泡不重置。
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./LlmConfigPanel.css";

/** 与 Rust `src-tauri/src/asr_config.rs` 的 `AsrConfig` 对齐（camelCase）。 */
export interface AsrConfig {
  source: "local" | "cloud";
  cloudEndpoint: string;
  cloudApiKey: string;
  cloudModel: string;
  cloudLanguage: string;
}

const DEFAULT_CONFIG: AsrConfig = {
  source: "local",
  cloudEndpoint: "wss://api.deepgram.com/v1/listen",
  cloudApiKey: "",
  cloudModel: "nova-3",
  cloudLanguage: "multi",
};

export interface AsrConfigPanelProps {
  /** 嵌入设置对话框：不显示折叠按钮，始终展开表单（T12）。 */
  embedded?: boolean;
}

export default function AsrConfigPanel({ embedded = false }: AsrConfigPanelProps) {
  const [open, setOpen] = useState(embedded);
  const [source, setSource] = useState<AsrConfig["source"]>("local");
  const [cloudEndpoint, setCloudEndpoint] = useState(DEFAULT_CONFIG.cloudEndpoint);
  const [cloudApiKey, setCloudApiKey] = useState("");
  const [cloudModel, setCloudModel] = useState(DEFAULT_CONFIG.cloudModel);
  const [cloudLanguage, setCloudLanguage] = useState(DEFAULT_CONFIG.cloudLanguage);
  const [status, setStatus] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    invoke<AsrConfig>("load_asr_config")
      .then((config) => {
        setSource(config.source);
        setCloudEndpoint(config.cloudEndpoint);
        setCloudApiKey(config.cloudApiKey);
        setCloudModel(config.cloudModel);
        setCloudLanguage(config.cloudLanguage);
      })
      .catch((e) => setStatus(`加载 ASR 配置失败: ${e}`));
  }, []);

  const save = async () => {
    setSaving(true);
    setStatus("");
    try {
      const config: AsrConfig = {
        source,
        cloudEndpoint: cloudEndpoint.trim(),
        cloudApiKey: cloudApiKey.trim(),
        cloudModel: cloudModel.trim(),
        cloudLanguage: cloudLanguage.trim(),
      };
      if (config.source === "cloud") {
        if (!config.cloudEndpoint) throw new Error("请先填写云端 ASR 端点");
        if (!config.cloudApiKey) throw new Error("请先填写云端 ASR API Key");
        if (!config.cloudModel) throw new Error("请先填写云端 ASR 模型");
        if (!config.cloudLanguage) throw new Error("请先填写云端 ASR 语言");
      }
      await invoke("save_asr_config", { config });
      setStatus("已保存 ✓（后台自动热切换，无需重启）");
    } catch (e) {
      setStatus(`保存失败: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="asr-config">
      {!embedded && (
        <button
          type="button"
          className="llm-config-toggle"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          🎙️ ASR 配置 {open ? "▾" : "▸"}
        </button>
      )}

      {(embedded || open) && (
        <div className="llm-config-form">
          <div className="llm-field">
            <span>识别来源</span>
            <div className="asr-source-row" role="radiogroup" aria-label="ASR 来源">
              <button
                type="button"
                className={`settings-option ${source === "local" ? "active" : ""}`}
                role="radio"
                aria-checked={source === "local"}
                onClick={() => setSource("local")}
              >
                本地 ASR
              </button>
              <button
                type="button"
                className={`settings-option ${source === "cloud" ? "active" : ""}`}
                role="radio"
                aria-checked={source === "cloud"}
                onClick={() => setSource("cloud")}
              >
                云端 ASR
              </button>
            </div>
            <span className="asr-field-hint">
              本地默认离线运行；云端使用 Deepgram 兼容流式 WebSocket，适合本地识别质量不足时增强。
            </span>
          </div>

          <label className="llm-field">
            <span>云端端点</span>
            <input
              value={cloudEndpoint}
              onChange={(e) => setCloudEndpoint(e.target.value)}
              placeholder="wss://api.deepgram.com/v1/listen"
              spellCheck={false}
              disabled={source === "local"}
            />
          </label>
          <label className="llm-field">
            <span>云端 API Key</span>
            <input
              type="password"
              value={cloudApiKey}
              onChange={(e) => setCloudApiKey(e.target.value)}
              placeholder="Deepgram API Key"
              spellCheck={false}
              disabled={source === "local"}
            />
          </label>
          <div className="llm-field">
            <span>云端模型</span>
            <input
              value={cloudModel}
              onChange={(e) => setCloudModel(e.target.value)}
              placeholder="nova-3"
              spellCheck={false}
              disabled={source === "local"}
            />
          </div>
          <label className="llm-field">
            <span>云端语言</span>
            <input
              value={cloudLanguage}
              onChange={(e) => setCloudLanguage(e.target.value)}
              placeholder="multi（中英混合）/ zh / en"
              spellCheck={false}
              disabled={source === "local"}
            />
            <span className="asr-field-hint">
              Deepgram 的 `multi` 支持中英混合识别；也可填 `zh`、`en` 等服务商支持的语言代码。
            </span>
          </label>

          <div className="llm-actions">
            <button type="button" className="llm-save" onClick={save} disabled={saving}>
              {saving ? "保存中…" : "保存 ASR 配置"}
            </button>
            <span className="llm-status" role="status">
              {status}
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
