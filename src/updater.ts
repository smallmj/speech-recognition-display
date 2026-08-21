/**
 * 应用内更新（T19）。
 *
 * Windows：`tauri-plugin-updater` 全自动更新。安装包经 tauri-action 签名，
 * `latest.json` 每次发布自动生成；NSIS `installMode=passive`，下载后静默安装，
 * 用户确认后 `relaunch` 重启生效。
 * macOS：构建未签名/未公证，updater 插件内置下载无法通过 Gatekeeper，改为经
 * Rust 命令 `check_latest_release` 查询 GitHub 最新 Release，用户确认后用
 * opener 打开 Releases 页手动下载安装。
 *
 * 两条路径升级前都必须用户确认，绝不静默升级。
 */

import { getVersion } from "@tauri-apps/api/app";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent } from "@tauri-apps/plugin-updater";

/** Rust `check_latest_release` 返回的 JSON 负载。 */
interface LatestRelease {
  url: string;
  version: string;
}

/**
 * Windows/macOS 统一入口：macOS（未签名）走「打开 Releases 页手动下载」，
 * Windows 走全自动更新。
 *
 * @returns 提示文案（已是最新 / 正在下载更新 / 请手动下载新版等），让调用方
 *          直接展示；发生异常或无事发生时返回 null（调用方自行忽略/记录）。
 */
export async function checkForUpdates({ manual = false } = {}): Promise<string | null> {
  // 非 Tauri 环境（浏览器里跑 vite dev / 静态预览）以及 dev 构建一律跳过。
  if (!isTauri() || import.meta.env.DEV) return null;

  try {
    const platform = navigator.platform.toLowerCase();
    if (platform.includes("mac")) {
      return await checkForUpdatesOnMacOS(manual);
    }
    // Windows（及未来已签名平台）走插件全自动更新；Linux 未打安装包，跳过。
    if (!platform.includes("win")) return null;
    return await checkForUpdatesOnWindows(manual);
  } catch (err) {
    console.error("[updater] 检查更新失败:", err);
    return null;
  }
}

/**
 * macOS：查询 GitHub 最新 Release（Rust 端比较版本），确认后打开 Releases 页
 * 手动下载（当前构建未签名/未公证，无法自动下载安装）。
 */
async function checkForUpdatesOnMacOS(manual: boolean): Promise<string | null> {
  const raw = await invoke<string>("check_latest_release");
  if (!raw) {
    return manual ? "当前已是最新版本。" : null;
  }
  let release: LatestRelease;
  try {
    release = JSON.parse(raw) as LatestRelease;
  } catch {
    console.warn("[updater] 解析 check_latest_release 结果失败:", raw);
    return null;
  }
  if (!release.url) return null;

  const current = await getVersion();
  const confirmed = await ask(
    `发现新版本 v${release.version}（当前 v${current}）。\n\n` +
      "macOS 安装包未签名/未公证，无法自动下载安装，将打开 GitHub Releases 页手动下载。",
    { title: "发现新版本", kind: "info", okLabel: "前往下载", cancelLabel: "稍后再说" },
  );
  if (!confirmed) return null;

  await openUrl(release.url);
  return "已打开下载页面，请按页面说明安装新版本。";
}

/**
 * Windows：`tauri-plugin-updater` 全自动更新 —— 确认 → 下载并安装 →
 * 重启应用生效。
 */
async function checkForUpdatesOnWindows(manual: boolean): Promise<string | null> {
  const update = await check();
  if (!update) {
    return manual ? "当前已是最新版本。" : null;
  }

  const confirmed = await ask(
    `发现新版本 v${update.version}（当前 v${update.currentVersion}），是否立即下载并安装？`,
    { title: "发现新版本", kind: "info", okLabel: "立即更新", cancelLabel: "稍后再说" },
  );
  if (!confirmed) return null;

  // 下载 + 安装（NSIS passive：进度在后台，不打断使用）。更新包较小，进度仅记日志。
  await update.downloadAndInstall((event: DownloadEvent) => {
    if (event.event === "Progress" && event.data.chunkLength) {
      console.debug("[updater] Windows 更新下载进度（累计字节）:", event.data.chunkLength);
    }
  });

  // 安装完成后重启应用生效（用户已确认更新）。
  await relaunch();
  return "更新已安装，正在重启…";
}