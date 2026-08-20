#!/usr/bin/env node
/**
 * 打包版运行时：把 Python 运行时与 sidecar 脚本复制到 Tauri 资源目录
 * （`src-tauri/resources/runtime/`），供 `tauri.conf.json` 的 resources 打包。
 *
 * 两种模式：
 * - 默认（本地验证）：复制开发 venv（`src-tauri/.venv`）。产物依赖构建机上的
 *   系统 Python（venv 里的 bin/python3 是软链），换机器必挂，仅适合本机验收。
 * - 自包含（正式分发）：设置 `TALKSEE_STANDALONE=1`（或 `TALKSEE_PYTHON_BASE`
 *   指向一个 python-build-standalone 安装目录）时，下载按平台/架构分发的
 *   自包含 CPython，装好 sherpa-onnx + numpy 后作为 `resources/runtime/venv`，
 *   换机器可跑。GitHub Actions 发布流程即用此模式。
 */

import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const devVenv = path.join(root, "src-tauri", ".venv");
const devScript = path.join(root, "src-tauri", "sherpa_streaming.py");
const runtimeDir = path.join(root, "src-tauri", "resources", "runtime");
const venvTarget = path.join(runtimeDir, "venv");

// —— 自包含 Python（python-build-standalone）固定版本 ——
// 该 release 对 aarch64/x86_64-apple-darwin 与 x86_64-pc-windows-msvc 均提供
// install_only.tar.gz；解压后顶层目录是 `python/`。
const PY_VERSION = "3.12.14";
const PY_RELEASE_TAG = "20260814";
const PY_URL_BASE =
  "https://github.com/astral-sh/python-build-standalone/releases/download";
// 与开发环境对齐的固定版本（见 setup-runtime.mjs / 本地 venv）
const SHERPA_ONNX_VERSION = "1.13.6";

const isWindows = process.platform === "win32";

/** 解释器路径：unix 在 bin/，Windows 在根目录（python-build-standalone 布局）。 */
function pythonExecutable(venv) {
  return isWindows
    ? path.join(venv, "python.exe")
    : path.join(venv, "bin", "python3");
}

function run(cmd, args, label) {
  console.log(`\n>>> ${label}`);
  const result = spawnSync(cmd, args, { stdio: "inherit", shell: isWindows });
  if (result.status !== 0) {
    console.error(
      `!! ${label} 失败（退出码 ${result.status ?? result.error?.message ?? "?"}）`
    );
    process.exit(1);
  }
}

function copyScript() {
  if (!existsSync(devScript)) {
    console.error(`未找到 sidecar 脚本：${devScript}`);
    process.exit(1);
  }
  cpSync(devScript, path.join(runtimeDir, "sherpa_streaming.py"));
}

/** 幂等判断：目标 venv 是自包含构建（带标记）且能导入 sherpa_onnx / numpy。 */
function hasRuntime() {
  // 标记文件区分"自包含正式运行时"与"本地开发 venv 的拷贝"——
  // 后者是软链到构建机系统 Python，换机器必挂，绝不能复用。
  if (!existsSync(path.join(venvTarget, ".talksee-standalone"))) return false;
  const py = pythonExecutable(venvTarget);
  if (!existsSync(py)) return false;
  if (!existsSync(path.join(runtimeDir, "sherpa_streaming.py"))) return false;
  const check = spawnSync(py, ["-c", "import sherpa_onnx, numpy"], {
    encoding: "utf8",
  });
  return check.status === 0;
}

/** 目标平台 → python-build-standalone 的 target triple。 */
function detectTriple() {
  const arch = os.arch();
  const platform = process.platform;
  if (platform === "darwin") {
    if (arch === "arm64") return "aarch64-apple-darwin";
    if (arch === "x64") return "x86_64-apple-darwin";
  }
  if (platform === "win32" && arch === "x64") return "x86_64-pc-windows-msvc";
  throw new Error(
    `暂不支持的目标平台：${platform}/${arch}。` +
      "自包含运行时仅支持 macOS(arm64/x64) 与 Windows(x64)。"
  );
}

/** 下载并解压 python-build-standalone（缓存到 target/.python-standalone，幂等）。 */
async function ensureStandalonePythonBase() {
  const triple = detectTriple();
  const cacheDir = path.join(root, "target", ".python-standalone");
  const tarballName = `cpython-${PY_VERSION}+${PY_RELEASE_TAG}-${triple}-install_only.tar.gz`;
  const tarball = path.join(cacheDir, tarballName);
  const extractDir = path.join(cacheDir, `cpython-${PY_VERSION}`);
  const srcDir = path.join(extractDir, "python");
  mkdirSync(cacheDir, { recursive: true });

  if (existsSync(pythonExecutable(srcDir))) {
    console.log(`检测到已就绪的自包含 Python：${pythonExecutable(srcDir)}，跳过下载/解压。`);
    return srcDir;
  }

  const url = `${PY_URL_BASE}/${PY_RELEASE_TAG}/${tarballName}`;
  if (!existsSync(tarball)) {
    console.log(`下载自包含 Python（${triple}）…`);
    const res = await fetch(url);
    if (!res.ok) {
      throw new Error(`下载失败：HTTP ${res.status} ${res.statusText}（${url}）`);
    }
    const buf = Buffer.from(await res.arrayBuffer());
    writeFileSync(tarball, buf);
    console.log(`已下载 ${(buf.length / 1024 / 1024).toFixed(1)} MB → ${tarball}`);
  } else {
    console.log(`复用已下载的压缩包：${tarball}`);
  }

  rmSync(extractDir, { recursive: true, force: true });
  mkdirSync(extractDir, { recursive: true });
  run("tar", ["-xzf", tarball, "-C", extractDir], "解压自包含 Python");

  if (!existsSync(pythonExecutable(srcDir))) {
    throw new Error(`解压后未找到解释器：${pythonExecutable(srcDir)}`);
  }
  return srcDir;
}

/** 自包含模式：从基础 Python 组装 resources/runtime/venv。 */
function buildStandaloneRuntime(srcDir) {
  console.log(`\n>>> 组装自包含运行时 → ${venvTarget}`);
  rmSync(venvTarget, { recursive: true, force: true });
  // verbatimSymlinks: true 让软链按原文保留（bin/python3 -> python3.12 的相对链接），
  // 否则 Node 会把软链解析成绝对路径，产物换机必挂。
  cpSync(srcDir, venvTarget, { recursive: true, verbatimSymlinks: true });
  const py = pythonExecutable(venvTarget);
  run(py, ["-m", "pip", "install", "--upgrade", "pip"], "升级 pip");
  run(
    py,
    ["-m", "pip", "install", `sherpa-onnx==${SHERPA_ONNX_VERSION}`, "numpy"],
    `安装 sherpa-onnx==${SHERPA_ONNX_VERSION} + numpy`
  );
  copyScript();
  writeFileSync(path.join(venvTarget, ".talksee-standalone"), "standalone\n");
  console.log("\n自包含运行时打包完成。");
}

async function main() {
  const standalone =
    process.env.TALKSEE_STANDALONE === "1" || !!process.env.TALKSEE_PYTHON_BASE;
  mkdirSync(runtimeDir, { recursive: true });

  if (standalone) {
    if (hasRuntime() && !process.env.TALKSEE_FORCE_RUNTIME) {
      console.log("检测到已就绪的自包含运行时，跳过重建。");
      return;
    }
    const srcDir = process.env.TALKSEE_PYTHON_BASE
      ? path.resolve(process.env.TALKSEE_PYTHON_BASE)
      : await ensureStandalonePythonBase();
    if (!existsSync(pythonExecutable(srcDir))) {
      throw new Error(
        `TALKSEE_PYTHON_BASE 指向的目录没有解释器：${pythonExecutable(srcDir)}`
      );
    }
    buildStandaloneRuntime(srcDir);
    return;
  }

  // —— 默认模式：本地验证（复制开发 venv）——
  if (!existsSync(path.join(devVenv, isWindows ? "Scripts" : "bin"))) {
    console.log("未找到 src-tauri/.venv，先自动创建运行环境…");
    const setup = spawnSync(process.execPath, [
      path.join(root, "scripts", "setup-runtime.mjs"),
    ], { stdio: "inherit" });
    if (setup.status !== 0) process.exit(setup.status ?? 1);
  }
  if (!existsSync(devScript)) {
    console.error(`未找到 sidecar 脚本：${devScript}`);
    process.exit(1);
  }
  rmSync(venvTarget, { recursive: true, force: true });
  console.log(`复制 venv → ${venvTarget}`);
  cpSync(devVenv, venvTarget, { recursive: true });
  copyScript();
  console.log("打包运行时就绪（本地验证模式）。");
}

main().catch((err) => {
  console.error(`!! ${err.message}`);
  process.exit(1);
});
