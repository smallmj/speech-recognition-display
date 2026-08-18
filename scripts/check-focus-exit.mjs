#!/usr/bin/env node
/**
 * 不变量检查：置顶大字模式必须存在可达的退出途径。
 *
 * 断言：focus 模式开启时，以下至少一条成立：
 *   A. 存在 ESC 键处理会把 focusMode 关掉（setFocusMode(false)）；
 *   B. 存在一个不在被 focus CSS 隐藏容器内的退出按钮（onClick 关掉 focusMode）。
 *
 * 退出码：0 = 绿（有退出途径）；1 = 红（没有退出途径，用户在置顶大字模式下被困）。
 * 用法：node scripts/check-focus-exit.mjs
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));

const css = readFileSync(`${root}src/styles.css`, "utf8");
const app = readFileSync(`${root}src/App.tsx`, "utf8");

// ---- 1. 找出 focus 模式下被隐藏的关键容器 ----
const focusBlock = css.match(/置顶大字模式[\s\S]*?设置面板/)?.[0] ?? "";
const hiddenSelectors = [
  ...focusBlock.matchAll(/\[data-focus-mode="true"\]\s*([.\w-]+)/g),
].map((m) => m[1]);

// ---- 2. ESC 途径：App.tsx 是否有针对 focusMode 的 ESC 退出 ----
function hasEscFocusExit(src) {
  // 提取每个 keydown/KeyboardEvent 处理块，检查其中是否同时出现 Escape 与 setFocusMode(false)
  const blocks = src.split(/useEffect|function |addEventListener/).filter((b) => b.includes("Escape"));
  return blocks.some((b) => /Escape/.test(b) && /setFocusMode\s*\(\s*false\s*\)/.test(b));
}

// ---- 3. 按钮途径：是否存在关 focusMode 的 onClick，且其祖先容器不在隐藏列表 ----
function hasExitButton(src) {
  // 找到含 setFocusMode(false) 的 onClick 按钮及其所在标签，粗粒度判断它是否在 header 内：
  // 在 JSX 中，header 块以 <header className="app-header"> 开头，其内出现 focusMode 开关按钮。
  // 若所有 setFocusMode(false) 调用都位于 app-header 块内 → 视为被隐藏 → 无按钮途径。
  const headerStart = src.indexOf('<header className="app-header"');
  const headerEnd = src.indexOf("</header>", headerStart);
  const headerBlock = headerStart >= 0 ? src.slice(headerStart, headerEnd) : "";
  const outside = src.replace(headerBlock, ""); // 移除 header 块后剩下的代码
  return /setFocusMode\s*\(\s*false\s*\)/.test(outside);
}

const escExit = hasEscFocusExit(app);
const btnExit = hasExitButton(app);
const hidden = hiddenSelectors.join(", ");

console.log(`[check-focus-exit] focus 模式隐藏容器: ${hidden || "(未解析到)"}`);
console.log(`[check-focus-exit] ESC 退出途径: ${escExit ? "有" : "无"}`);
console.log(`[check-focus-exit] header 外退出按钮: ${btnExit ? "有" : "无"}`);

if (escExit || btnExit) {
  console.log("[check-focus-exit] PASS：置顶大字模式存在退出途径");
  process.exit(0);
}
console.error(
  "[check-focus-exit] FAIL：置顶大字模式下没有任何可达退出途径（ESC 不会关闭 focus 模式，且唯一开关按钮在被隐藏的 app-header 内）"
);
process.exit(1);
