#!/usr/bin/env node
/**
 * 回归检查：双轨展示的用户可感知不变量。
 *
 * 1. 新片段追加后，容器级滚动保持最新字幕可见；用户上翻阅读历史时不强拽
 *    回底部，并提供「回到最新」按钮（v0.4 契约，替代 v0.3 的 scrollIntoView 直滚）。
 * 2. LLM 整理中必须继续显示原文；最终结果到达后才切换，避免文字消失。
 * 3. 同一说话人的未整理片段必须合并成一个渲染组；批次完成后仍合并显示。
 *
 * 退出码：0 = 绿；1 = 红。
 * 用法：node --experimental-strip-types scripts/check-dual-track-regressions.mjs
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { buildRenderGroups } from "../src/components/renderGroups.ts";

const root = fileURLToPath(new URL("..", import.meta.url));
const source = readFileSync(`${root}src/components/DualTrackView.tsx`, "utf8");

// v0.4 滚动契约（重构自 v0.3 的 scrollIntoView 直滚）：
// - 列表容器自身持有 ref（listRef, .dual-list 上），新内容到达时
//   `el.scrollTop = el.scrollHeight`——容器级滚动 + Windows WebView2 兼容
//   （scrollIntoView 会连带滚动祖先导致最新字幕落点错位，见 #4 根因）；
// - 「接近底部才跟随」守卫：用户在底部（距离 <96px）才自动滚，上翻时暂停
//   并出现「回到最新」按钮（.back-to-latest，死代码复活）。
const hasAutoScroll =
  /scrollTop = el\.scrollHeight/.test(source) &&
  /const listRef = useRef<HTMLDivElement \| null>\(null\)/.test(source) &&
  /onScroll=\{onScroll\}/.test(source) &&
  /BOTTOM_THRESHOLD_PX = 96/.test(source) &&
  /className="back-to-latest"/.test(source);
const keepsRawWhileCleaning = /if \(seg\.cleaningPartial != null\) return "pending";/.test(source);

function segment(id, speakerId, raw, status, cleaned = null, editId = null) {
  return {
    id,
    speakerId,
    raw,
    status,
    cleaned,
    editId,
    ts: id,
    retries: 0,
    cleaningPartial: null,
    cleaningEditId: null,
  };
}

const pendingGroups = buildRenderGroups([
  segment(0, 1, "第一句", "frozen"),
  segment(1, 2, "别人插入", "frozen"),
  segment(2, 1, "第三句", "frozen"),
]);
const pendingSpeaker1 = pendingGroups.find((group) => group.speakerId === 1);
const groupsSameSpeakerPending =
  pendingSpeaker1?.segments.map((seg) => seg.id).join(",") === "0,2" &&
  pendingSpeaker1?.raw === "第一句\n第三句";

const cleanedGroups = buildRenderGroups([
  segment(0, 1, "第一句", "cleaned", "第一句，第三句。", 7),
  segment(1, 2, "别人插入", "cleaned", "别人插入。", 8),
  segment(2, 1, "第三句", "cleaned", "第一句，第三句。", 7),
]);
const cleanedSpeaker1 = cleanedGroups.find((group) => group.speakerId === 1);
const groupsSameSpeakerCleaned =
  cleanedSpeaker1?.segments.map((seg) => seg.id).join(",") === "0,2" &&
  cleanedSpeaker1?.raw === "第一句\n第三句" &&
  cleanedSpeaker1?.primary.cleaned === "第一句，第三句。";

const checks = [
  { name: "新片段追加后自动滚动到底部", pass: hasAutoScroll },
  { name: "整理中保留原文，最终结果到达后再切换", pass: keepsRawWhileCleaning },
  { name: "同一说话人未整理片段合并成一个渲染组", pass: groupsSameSpeakerPending },
  { name: "同一批次整理结果合并成一个渲染组", pass: groupsSameSpeakerCleaned },
];

for (const check of checks) {
  console.log(`[dual-track-regressions] ${check.name}: ${check.pass ? "PASS" : "FAIL"}`);
}

if (checks.every((check) => check.pass)) {
  console.log("[dual-track-regressions] PASS：滚动、原文保留与同人批次渲染均满足");
  process.exit(0);
}

console.error("[dual-track-regressions] FAIL：存在用户可感知回归");
process.exit(1);
