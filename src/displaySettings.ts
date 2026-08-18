/**
 * 显示设置 store：localStorage 持久化 + React Context + 即时应用到根元素。
 *
 * 设置项：
 * - theme: "auto" | "light" | "dark"   —— 主题跟随系统或手动固定
 * - focusMode: boolean                  —— 置顶大字模式
 * - fontSize: "small" | "medium" | "large" | "xlarge"
 * - fontFamily: 系统默认 / 苹方 / 宋体 / 黑体 / 楷体
 * - textColor: 预设文字颜色
 */

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

export type Theme = "auto" | "light" | "dark";
export type FontSize = "small" | "medium" | "large" | "xlarge";
export type FontFamily = "default" | "pingfang" | "songti" | "heiti" | "kaiti";
export type TextColor = "default" | "black" | "darkgray" | "white" | "darkblue";

export interface DisplaySettings {
  theme: Theme;
  focusMode: boolean;
  fontSize: FontSize;
  fontFamily: FontFamily;
  textColor: TextColor;
}

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

const STORAGE_KEY = "scd.display.settings";

const FONT_SIZE_MAP: Record<FontSize, string> = {
  small: "13px",
  medium: "15px",
  large: "18px",
  xlarge: "22px",
};

const FONT_FAMILY_MAP: Record<FontFamily, string> = {
  default: "inherit",
  pingfang: '"PingFang SC", "苹方", sans-serif',
  songti: '"Songti SC", "宋体", SimSun, serif',
  heiti: '"Heiti SC", "黑体", SimHei, sans-serif',
  kaiti: '"Kaiti SC", "楷体", KaiTi, serif',
};

const TEXT_COLOR_MAP: Record<TextColor, string> = {
  default: "inherit",
  black: "#000000",
  darkgray: "#333333",
  white: "#ffffff",
  darkblue: "#1a3a5c",
};

const FONT_SIZE_LABELS: Record<FontSize, string> = {
  small: "小",
  medium: "中",
  large: "大",
  xlarge: "特大",
};

const FONT_FAMILY_LABELS: Record<FontFamily, string> = {
  default: "系统默认",
  pingfang: "苹方",
  songti: "宋体",
  heiti: "黑体",
  kaiti: "楷体",
};

const TEXT_COLOR_LABELS: Record<TextColor, string> = {
  default: "默认",
  black: "纯黑",
  darkgray: "深灰",
  white: "纯白",
  darkblue: "深蓝",
};

export const DISPLAY_LABELS = {
  fontSize: FONT_SIZE_LABELS,
  fontFamily: FONT_FAMILY_LABELS,
  textColor: TEXT_COLOR_LABELS,
} as const;

// ---------------------------------------------------------------------------
// 默认值
// ---------------------------------------------------------------------------

export const DEFAULT_SETTINGS: DisplaySettings = {
  theme: "auto",
  focusMode: false,
  fontSize: "medium",
  fontFamily: "default",
  textColor: "default",
};

// ---------------------------------------------------------------------------
// localStorage 读写
// ---------------------------------------------------------------------------

function loadSettings(): DisplaySettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<DisplaySettings>;
      return { ...DEFAULT_SETTINGS, ...parsed };
    }
  } catch {
    console.warn("[displaySettings] localStorage 读取失败，使用默认值");
  }
  return { ...DEFAULT_SETTINGS };
}

function saveSettings(s: DisplaySettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
  } catch {
    console.warn("[displaySettings] localStorage 写入失败");
  }
}

// ---------------------------------------------------------------------------
// 应用到根元素
// ---------------------------------------------------------------------------

function applyToRoot(s: DisplaySettings): void {
  const root = document.documentElement;

  // 主题
  root.setAttribute("data-theme", s.theme);

  // 置顶大字模式
  if (s.focusMode) {
    root.setAttribute("data-focus-mode", "true");
  } else {
    root.removeAttribute("data-focus-mode");
  }

  // 字号
  root.style.setProperty("--bubble-font-size", FONT_SIZE_MAP[s.fontSize]);

  // 字体
  root.style.setProperty("--bubble-font-family", FONT_FAMILY_MAP[s.fontFamily]);

  // 文字颜色
  root.style.setProperty("--bubble-text-color", TEXT_COLOR_MAP[s.textColor]);
}

// ---------------------------------------------------------------------------
// React Context
// ---------------------------------------------------------------------------

export interface DisplayContextValue {
  settings: DisplaySettings;
  setTheme: (t: Theme) => void;
  setFocusMode: (v: boolean) => void;
  setFontSize: (s: FontSize) => void;
  setFontFamily: (f: FontFamily) => void;
  setTextColor: (c: TextColor) => void;
}

export const DisplayContext = createContext<DisplayContextValue | null>(null);

/**
 * 顶层 hook：读取持久化设置、提供 setter、写入 localStorage 并应用到根元素。
 * 仅在 App 组件中调用一次。
 */
export function useDisplaySettingsState(): DisplayContextValue {
  const [settings, setSettings] = useState<DisplaySettings>(DEFAULT_SETTINGS);

  // 初始化：从 localStorage 读取并应用
  useEffect(() => {
    const loaded = loadSettings();
    setSettings(loaded);
    applyToRoot(loaded);
  }, []);

  const update = useCallback((patch: Partial<DisplaySettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...patch };
      saveSettings(next);
      applyToRoot(next);
      return next;
    });
  }, []);

  const setTheme = useCallback((t: Theme) => update({ theme: t }), [update]);
  const setFocusMode = useCallback((v: boolean) => update({ focusMode: v }), [update]);
  const setFontSize = useCallback((s: FontSize) => update({ fontSize: s }), [update]);
  const setFontFamily = useCallback((f: FontFamily) => update({ fontFamily: f }), [update]);
  const setTextColor = useCallback((c: TextColor) => update({ textColor: c }), [update]);

  return useMemo(
    () => ({ settings, setTheme, setFocusMode, setFontSize, setFontFamily, setTextColor }),
    [settings, setTheme, setFocusMode, setFontSize, setFontFamily, setTextColor],
  );
}

/** 组件内消费显示设置。 */
export function useDisplaySettings(): DisplayContextValue {
  const ctx = useContext(DisplayContext);
  if (!ctx) {
    throw new Error("useDisplaySettings 必须在 DisplayContext.Provider 内使用");
  }
  return ctx;
}