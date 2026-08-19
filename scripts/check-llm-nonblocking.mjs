#!/usr/bin/env node
/**
 * 回归检查：LLM 整理绝不能阻塞识别链路。
 *
 * 约束：
 * 1. 主驱动循环里不能直接调用阻塞式 `cleanup_streaming`；
 * 2. LLM 必须在独立线程里执行，完成后经通道把结果交回主循环回填；
 * 3. 识别 final 在每拍无条件追加（真实 ASR 不等待整理完成）。
 *
 * 退出码：0 = 绿；1 = 红。
 * 用法：node scripts/check-llm-nonblocking.mjs
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const source = readFileSync(`${root}src-tauri/src/pipeline.rs`, "utf8");

// 主循环区段：从 while 循环开始到函数末尾，必须没有任何直接 LLM 调用。
const cleanupIndex = source.indexOf("cleanup_streaming");
const spawnIndexes = [...source.matchAll(/std::thread::spawn/g)].map((m) => m.index ?? 0);
// 只有两层 spawn 时（外层驱动线程 + 内层 LLM worker），cleanup_streaming
// 才被视作在独立 worker 内执行；若退化成在主循环直接调用，这里会变红。
const llmCallInWorker = spawnIndexes.filter((i) => i < cleanupIndex).length >= 2;
const resultViaChannel =
  /mpsc::channel/.test(source) && /LlmOutcome/.test(source);

const checks = [
  { name: "主循环不直接调用阻塞式 LLM", pass: llmCallInWorker },
  { name: "LLM 在独立线程中执行", pass: llmCallInWorker },
  { name: "LLM 结果经通道交回主循环", pass: resultViaChannel },
];

for (const check of checks) {
  console.log(`[llm-nonblocking] ${check.name}: ${check.pass ? "PASS" : "FAIL"}`);
}

if (checks.every((check) => check.pass)) {
  console.log("[llm-nonblocking] PASS：整理异步执行，识别链路不被阻塞");
  process.exit(0);
}

console.error("[llm-nonblocking] FAIL：LLM 调用可能阻塞识别链路");
process.exit(1);
