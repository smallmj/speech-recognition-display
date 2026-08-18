/**
 * LLM 配置面板（T9）：Base URL / API Key / 模型名 / 整理人设。
 *
 * 经 Tauri invoke 调 `save_llm_config` / `load_llm_config`（Rust 端明文 JSON
 * 存于 app config 目录）。极简实现：不做完整 T12 设置系统，够用即可。
 *
 * 人设字段留空 → 保存为 `null` → Rust 端 [DEFAULT_PERSONA] 内置整理人设生效；
 * 「恢复内置人设」按钮把默认人设文本填入（与 Rust 常量保持一致）。
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./LlmConfigPanel.css";

/** 与 Rust `src-tauri/src/llm.rs` 的 `LlmConfig` 对齐（camelCase）。 */
export interface LlmConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
  persona: string | null;
}

/** 内置整理人设（与 Rust `DEFAULT_PERSONA` 保持一致；「恢复内置人设」按钮用）。 */
export const DEFAULT_PERSONA =
  "你是实时字幕整理助手：把用户提供的口语化转写整理成通顺的书面语，去口语化、纠正错别字、补充标点，不改变原意，不添加原话没有的信息。直接输出整理结果，不要任何解释或前缀。";

export default function LlmConfigPanel() {
  const [open, setOpen] = useState(false);
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [persona, setPersona] = useState("");
  const [status, setStatus] = useState("");
  const [saving, setSaving] = useState(false);

  // 挂载时读取已保存配置（未保存过 → Rust 端返回默认值）。
  useEffect(() => {
    invoke<LlmConfig>("load_llm_config")
      .then((cfg) => {
        setBaseUrl(cfg.baseUrl);
        setApiKey(cfg.apiKey);
        setModel(cfg.model);
        setPersona(cfg.persona ?? "");
      })
      .catch((e) => setStatus(`加载配置失败: ${e}`));
  }, []);

  const save = async () => {
    setSaving(true);
    setStatus("");
    try {
      const config: LlmConfig = {
        baseUrl: baseUrl.trim(),
        apiKey: apiKey.trim(),
        model: model.trim(),
        // 空白人设 → null → Rust 端回退内置默认人设
        persona: persona.trim() ? persona : null,
      };
      await invoke("save_llm_config", { config });
      setStatus("已保存 ✓（驱动线程每次请求前重读配置，无需重启）");
    } catch (e) {
      setStatus(`保存失败: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="llm-config">
      <button
        type="button"
        className="llm-config-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        ⚙️ LLM 配置 {open ? "▾" : "▸"}
      </button>

      {open && (
        <div className="llm-config-form">
          <label className="llm-field">
            <span>Base URL</span>
            <input
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://api.openai.com/v1"
              spellCheck={false}
            />
          </label>
          <label className="llm-field">
            <span>API Key</span>
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="sk-…"
              spellCheck={false}
            />
          </label>
          <label className="llm-field">
            <span>模型名</span>
            <input
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="gpt-4o-mini"
              spellCheck={false}
            />
          </label>
          <label className="llm-field">
            <span>整理人设（留空使用内置预设）</span>
            <textarea
              rows={3}
              value={persona}
              onChange={(e) => setPersona(e.target.value)}
              placeholder={DEFAULT_PERSONA}
            />
          </label>
          <div className="llm-actions">
            <button type="button" className="llm-save" onClick={save} disabled={saving}>
              {saving ? "保存中…" : "保存配置"}
            </button>
            <button type="button" onClick={() => setPersona(DEFAULT_PERSONA)}>
              恢复内置人设
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
