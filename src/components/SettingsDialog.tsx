/**
 * 设置对话框（T12）：所有配置集中到标签页分组界面。
 *
 * 标签页：常规 / ASR / LLM 整理 / 显示 / 快捷键 / 历史 / 关于（规格 #31）。
 * - 常规：整理间隔 5s/10s，保存到 Rust 端 `app-settings.json`，后台每秒轮询
 *   即时生效（规格 #21、#42）。
 * - ASR / LLM 整理：嵌入既有面板（embedded 模式，无折叠按钮、始终展开表单）。
 * - 显示：主题 / 字号 / 字体 / 文字颜色 / 置顶大字，localStorage 持久化 + 即时应用。
 * - 快捷键：当前窗口内可用操作与按键说明 + T13 全局热键 / 托盘操作。
 * - 历史：嵌入会话历史面板（列表 + 重新打开 + 导出）。
 * - 关于：版本 / 项目仓库 / 技术栈与运行时 / 隐私说明。
 */

import { useEffect, useState } from "react";
import {
  DISPLAY_LABELS,
  useDisplaySettings,
  type FontFamily,
  type FontSize,
  type TextColor,
  type Theme,
} from "../displaySettings";
import type { CleanupInterval } from "./DualTrackView";
import AsrConfigPanel from "./AsrConfigPanel";
import ModelCatalogPanel from "./ModelCatalogPanel";
import LlmConfigPanel from "./LlmConfigPanel";
import SessionHistoryPanel from "./SessionHistoryPanel";
import { checkForUpdates } from "../updater";
import { getVersion } from "@tauri-apps/api/app";
import logoMark from "../../brand/logo-mark-256.png";

export interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
  /** 当前整理间隔（秒），与 Rust `app-settings.json` 保持一致。 */
  cleanupInterval: CleanupInterval;
  /** 切换整理间隔（App 侧负责保存到 Rust 配置并即时生效）。 */
  onCleanupIntervalChange: (seconds: CleanupInterval) => void;
  /** 是否启用 LLM 整理（T16，App 侧保存并即时生效）。 */
  llmCleanupEnabled: boolean;
  /** 切换 LLM 整理开关。 */
  onLlmCleanupEnabledChange: (enabled: boolean) => void;
  /** 打开时若提供，则直接落到该标签页（用于「缺模型 → 引导去下载」）。 */
  initialTab?: TabId | null;
  /** 消费掉 initialTab 后回调（App 侧清空，避免下次打开重复跳转）。 */
  onInitialTabConsumed?: () => void;
  /** 最近一次纪要文本（历史页刷新用）。 */
  latestMinutes: string | null;
  /** 重新运行首次初始化（清除完成标记并回到向导）。 */
  onReinitialize: () => void;
}

export type TabId = "general" | "model" | "llm" | "display" | "shortcut" | "history" | "about";

const TABS: { id: TabId; label: string }[] = [
  { id: "general", label: "常规" },
  { id: "model", label: "模型" },
  { id: "llm", label: "LLM 整理" },
  { id: "display", label: "显示" },
  { id: "shortcut", label: "快捷键" },
  { id: "history", label: "历史" },
  { id: "about", label: "关于" },
];

/** 窗口内操作 + 全局热键（T13）/托盘操作。CmdOrCtrl：macOS=⌘ Command，Windows/Linux=Ctrl。 */
const SHORTCUT_ROWS: { operation: string; how: string }[] = [
  { operation: "唤出主窗口（全局）", how: "按 Cmd/Ctrl + Shift + L；或点击托盘图标。" },
  { operation: "隐藏到托盘（全局）", how: "按 Cmd/Ctrl + Shift + H；或点击窗口关闭按钮。" },
  { operation: "开始识别（全局）", how: "按 Cmd/Ctrl + Shift + S；或点击头部「▶ 开始识别」、托盘菜单同项。" },
  { operation: "停止并生成纪要（全局）", how: "按 Cmd/Ctrl + Shift + T；或点击头部「⏹ 停止并生成纪要」、托盘菜单同项。" },
  { operation: "打开 / 关闭设置", how: "点击头部「⚙ 设置」按钮；按 Esc 关闭。" },
  { operation: "退出置顶大字模式", how: "按 Esc，或点击悬浮的「✕ 退出大字」按钮。" },
  { operation: "显示原文 / 整理版切换", how: "点击字幕工具栏「显示原文 / 显示整理版」按钮。" },
  { operation: "切换整理间隔", how: "点击字幕工具栏的 5s / 10s 按钮，或到「设置 → 常规」调整。" },
  { operation: "退出应用", how: "托盘菜单「退出」；关闭窗口仅隐藏到托盘，进程常驻。" },
];

export default function SettingsDialog({
  open,
  onClose,
  cleanupInterval,
  onCleanupIntervalChange,
  llmCleanupEnabled,
  onLlmCleanupEnabledChange,
  latestMinutes,
  onReinitialize,
  initialTab,
  onInitialTabConsumed,
}: SettingsDialogProps) {
  const [activeTab, setActiveTab] = useState<TabId>("general");

  // 打开设置时若指定了目标标签页（如「缺模型 → 模型页下载」），跳转过去。
  useEffect(() => {
    if (open && initialTab) {
      setActiveTab(initialTab);
      onInitialTabConsumed?.();
    }
  }, [open, initialTab, onInitialTabConsumed]);
  const { settings, setTheme, setFocusMode, setFontSize, setFontFamily, setTextColor } =
    useDisplaySettings();

  // 关于页：动态读取应用版本（与 Cargo.toml / tauri.conf.json 保持一致，避免硬编码漂移）。
  const [appVersion, setAppVersion] = useState("0.2.0");
  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion("0.2.0"));
  }, []);

  // 关于页「检查更新」（T19）：展示 checkForUpdates 返回的提示文案。
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const handleCheckUpdate = async () => {
    if (checkingUpdate) return;
    setCheckingUpdate(true);
    setUpdateMsg(null);
    try {
      const msg = await checkForUpdates({ manual: true });
      setUpdateMsg(msg ?? "检查更新失败，请稍后重试。");
    } catch (err) {
      console.error("[updater] 手动检查失败:", err);
      setUpdateMsg("检查更新失败，请稍后重试。");
    } finally {
      setCheckingUpdate(false);
    }
  };

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
    <div className="settings-dialog-overlay" onClick={onClose}>
      <div
        className="settings-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="设置"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="settings-dialog-header">
          <span className="settings-dialog-title">⚙ 设置</span>
          <button
            type="button"
            className="settings-dialog-close"
            onClick={onClose}
            aria-label="关闭设置"
            title="关闭设置（Esc）"
          >
            ✕
          </button>
        </header>

        <div className="settings-dialog-body">
          <nav className="settings-tabs" aria-label="设置分组">
            {TABS.map((tab) => (
              <button
                key={tab.id}
                type="button"
                className={`settings-tab ${activeTab === tab.id ? "is-active" : ""}`}
                aria-selected={activeTab === tab.id}
                onClick={() => setActiveTab(tab.id)}
              >
                {tab.label}
              </button>
            ))}
          </nav>

          <div className="settings-tab-content">
            {activeTab === "general" && (
              <section className="settings-tab-section">
                <p className="settings-panel-section-title">整理间隔</p>
                <p className="settings-panel-hint">
                  字幕从原文切换为整理版的固定节奏：间隔短刷新更快，间隔长整理更稳定。
                </p>
                <div className="settings-option-row" role="group" aria-label="整理间隔">
                  {([5, 10] as const).map((s) => (
                    <button
                      key={s}
                      type="button"
                      className={`settings-option ${cleanupInterval === s ? "active" : ""}`}
                      aria-pressed={cleanupInterval === s}
                      onClick={() => onCleanupIntervalChange(s)}
                    >
                      {s} 秒
                    </button>
                  ))}
                </div>
                <p className="settings-panel-hint">
                  选择后立即保存到本机并即时生效（后台每秒同步），无需重启应用。
                </p>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">初始化</p>
                  <p className="settings-panel-hint">
                    重新检测运行环境、切换本地 / 云端模式或更换下载镜像；已完成的步骤会跳过。
                  </p>
                  <button
                    type="button"
                    className="settings-option"
                    onClick={onReinitialize}
                  >
                    重新运行初始化
                  </button>
                </div>
              </section>
            )}

            {activeTab === "model" && (
              <section className="settings-tab-section">
                <p className="settings-panel-hint">
                  选择识别来源（本地 sherpa-onnx / Deepgram 兼容云端流式）与要使用的本地模型；
                  本地模型改动下次「开始识别」生效，云端 ASR 保存后自动热切换。
                </p>
                <AsrConfigPanel embedded />
                <hr className="settings-panel-divider" />
                <ModelCatalogPanel />
              </section>
            )}

            {activeTab === "llm" && (
              <section className="settings-tab-section">
                <div className="settings-toggle-row">
                  <span className="settings-toggle-label">启用 LLM 整理</span>
                  <button
                    type="button"
                    className={`settings-toggle ${llmCleanupEnabled ? "on" : ""}`}
                    onClick={() => onLlmCleanupEnabledChange(!llmCleanupEnabled)}
                    aria-label="切换启用 LLM 整理"
                  >
                    <span className="settings-toggle-knob" />
                  </button>
                </div>
                <p className="settings-panel-hint">
                  关闭后实时字幕整理与会议纪要都不再调用 LLM，字幕一律原文（双轨「整理版」等同原文）；
                  保存后即时生效，重新开启即恢复。
                </p>

                <hr className="settings-panel-divider" />

                <p className="settings-panel-hint">
                  配置 OpenAI 兼容服务（Base URL + API Key + 模型名），可对接 DeepSeek、豆包、
                  OpenAI、本地 Ollama 等。每次整理请求前自动读取最新配置，保存后即时生效。
                </p>
                <LlmConfigPanel embedded />
              </section>
            )}

            {activeTab === "display" && (
              <section className="settings-tab-section">
                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">主题</p>
                  <p className="settings-panel-hint">跟随系统或手动固定浅色 / 深色。</p>
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

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">字号</p>
                  <p className="settings-panel-hint">气泡文字大小，越大越易读。</p>
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

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">字体</p>
                  <p className="settings-panel-hint">气泡文字字体，随系统字体渲染。</p>
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

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">文字颜色</p>
                  <p className="settings-panel-hint">气泡文字颜色，深色背景建议浅色文字。</p>
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
                  <p className="settings-panel-hint">
                    窗口始终置顶 + 超大字体；按 Esc 或悬浮按钮退出。所有显示设置自动保存。
                  </p>
                </div>
              </section>
            )}

            {activeTab === "shortcut" && (
              <section className="settings-tab-section">
                <p className="settings-panel-hint">
                  支持窗口内按钮 + 全局热键（T13）+ 托盘菜单：窗口关闭后驻留系统托盘，可用热键或托盘随时唤出 / 开始 / 停止识别。
                </p>
                <ul className="settings-shortcut-list">
                  {SHORTCUT_ROWS.map((row) => (
                    <li key={row.operation} className="settings-shortcut-row">
                      <span className="settings-shortcut-operation">{row.operation}</span>
                      <span className="settings-shortcut-how">{row.how}</span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {activeTab === "history" && (
              <section className="settings-tab-section">
                <p className="settings-panel-hint">
                  每次「停止并生成纪要」后自动保存到本机，重启后仍在；可重新打开查看并导出
                  Markdown / TXT / SRT。
                </p>
                <SessionHistoryPanel embedded latestMinutes={latestMinutes} />
              </section>
            )}

            {activeTab === "about" && (
              <section className="settings-tab-section">
                <div className="settings-panel-section settings-about-hero">
                  <img src={logoMark} alt="" className="settings-about-logo" />
                  <p className="settings-about-line settings-about-name">
                    语见 TalkSee · 让对话，看得见
                  </p>
                </div>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">版本</p>
                  <p className="settings-about-line">
                    语见 TalkSee v{appVersion}（听障实时字幕展示 MVP）。
                  </p>
                </div>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">更新</p>
                  <p className="settings-panel-hint">
                    Windows 支持应用内自动更新；macOS 安装包未签名/未公证，检查到新版本后会打开 GitHub
                    Releases 页手动下载。
                  </p>
                  <button
                    type="button"
                    className="settings-option"
                    onClick={handleCheckUpdate}
                    disabled={checkingUpdate}
                  >
                    {checkingUpdate ? "正在检查…" : "检查更新"}
                  </button>
                  {updateMsg && <p className="settings-about-line">{updateMsg}</p>}
                </div>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">项目仓库</p>
                  <p className="settings-about-line">
                    github.com/smallmj/talksee
                  </p>
                </div>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">技术栈与运行时</p>
                  <p className="settings-about-line">
                    Tauri 2 + React 18 + Rust engine；Python sherpa-onnx 本地流式 ASR、
                    Deepgram 兼容云端 ASR、OpenAI 兼容 LLM（SSE 流式整理 + 会议纪要）。
                    说话人区分基于 speaker embedding（ERes2NetV2）。
                  </p>
                </div>

                <hr className="settings-panel-divider" />

                <div className="settings-panel-section">
                  <p className="settings-panel-section-title">隐私说明</p>
                  <p className="settings-about-line">
                    默认使用本地识别，音频与文字不出本机；云端 ASR / LLM 仅在设置中启用并配置后才会联网。
                  </p>
                </div>
              </section>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
