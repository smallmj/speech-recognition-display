#!/usr/bin/env node
/**
 * 回归检查：双轨展示的用户可感知不变量。
 *
 * 1. 新片段追加后，列表容器必须自动滚动到底部；否则最新字幕在屏幕外。
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

const hasAutoScroll =
  /useRef<HTMLDivElement \| null>\(null\)/.test(source) &&
  /scrollIntoView\(\{ block: "end", inline: "nearest" \}\)/.test(source) &&
  /\}, \[sorted\.length\]\);/.test(source) &&
  /<div ref=\{bottomRef\} aria-hidden \/>/.test(source);
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
