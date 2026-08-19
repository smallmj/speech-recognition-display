#!/usr/bin/env node
/**
 * 开发模式运行环境：创建 `src-tauri/.venv` 并安装 sherpa-onnx + numpy。
 *
 * 打包应用不依赖此脚本：打包版运行时由 `scripts/package-runtime.mjs` 在构建期
 * 打入 app 资源，首启只做健康检测与模型下载。克隆仓库后本机开发需先执行
 * `pnpm run setup:runtime`。
 */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const venv = path.join(root, "src-tauri", ".venv");
const isWindows = process.platform === "win32";
const pythonInVenv = isWindows
  ? path.join(venv, "Scripts", "python.exe")
  : path.join(venv, "bin", "python3");

function run(cmd, args, label) {
  console.log(`\n>>> ${label}`);
  const result = spawnSync(cmd, args, { stdio: "inherit", shell: isWindows });
  if (result.status !== 0) {
    console.error(`!! ${label} 失败（退出码 ${result.status ?? result.error?.message ?? "?"}）`);
    process.exit(1);
  }
}

if (existsSync(pythonInVenv)) {
  console.log(`检测到已有运行环境：${pythonInVenv}，跳过创建。`);
} else {
  const createCmd = isWindows ? "py" : "python3";
  const createArgs = isWindows ? ["-3", "-m", "venv", venv] : ["-m", "venv", venv];
  run(createCmd, createArgs, "创建 Python venv");
}

run(pythonInVenv, ["-m", "pip", "install", "sherpa-onnx", "numpy"], "安装 sherpa-onnx + numpy");

console.log(`\n运行环境就绪：${pythonInVenv}`);
console.log("模型下载交给应用首次运行向导（可选 hf-mirror 国内镜像）。");
