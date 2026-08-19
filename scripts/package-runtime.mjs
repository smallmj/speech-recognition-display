#!/usr/bin/env node
/**
 * 打包版运行时：把开发 venv 与 sidecar 脚本复制到 Tauri 资源目录
 * （`src-tauri/resources/runtime/`），供 `tauri.conf.json` 的 resources 打包。
 *
 * 说明：直接复制 venv 适合本机打包/验收；面向最终用户的正式分发建议改用
 * 自包含 Python（如 python-build-standalone）放入 `resources/runtime/venv`，
 * 避免依赖构建机上的系统 Python。
 */

import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const devVenv = path.join(root, "src-tauri", ".venv");
const devScript = path.join(root, "src-tauri", "sherpa_streaming.py");
const runtimeDir = path.join(root, "src-tauri", "resources", "runtime");
const venvTarget = path.join(runtimeDir, "venv");

if (!existsSync(path.join(devVenv, process.platform === "win32" ? "Scripts" : "bin"))) {
  console.log("未找到 src-tauri/.venv，先自动创建运行环境…");
  const setup = spawnSync(process.execPath, [path.join(root, "scripts", "setup-runtime.mjs")], {
    stdio: "inherit",
  });
  if (setup.status !== 0) process.exit(setup.status ?? 1);
}
if (!existsSync(devScript)) {
  console.error(`未找到 sidecar 脚本：${devScript}`);
  process.exit(1);
}

mkdirSync(runtimeDir, { recursive: true });
rmSync(venvTarget, { recursive: true, force: true });
console.log(`复制 venv → ${venvTarget}`);
cpSync(devVenv, venvTarget, { recursive: true });
cpSync(devScript, path.join(runtimeDir, "sherpa_streaming.py"));
console.log("打包运行时就绪。");
