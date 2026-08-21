# Changelog / 更新日志

> 本文件记录 TalkSee（语见）从首个可运行版本以来的**全部**更新。每条同时给出中文与英文。
> This file records every release of TalkSee since the first runnable build. Each entry is given in both Chinese and English.
>
> **维护约定**：每次发布新版本，都必须在此文件顶部（`Unreleased`/最新版本之前）新增一条中英双语的更新日志，格式见 [docs/release-notes-template.md](docs/release-notes-template.md)。
> **Convention**: On every release, add a bilingual entry above the latest version, following [docs/release-notes-template.md](docs/release-notes-template.md).
>
> 更新日志采用 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 风格的语义分类：`Added / Changed / Fixed / Security`。
> The changelog follows a Keep-a-Changelog-style semantic structure: `Added / Changed / Fixed / Security`.

---

## [0.4.0] — 2026-08-21

### Added / 新增
- **ASR 模型升级**：默认模型换成 streaming paraformer bilingual zh-en（真流式、中英双语、英文与抗噪更强）；新增 **SenseVoice 高精度模式**（可选，ITN + 标点，最接近微信语音识别效果）。
  - **ASR model upgrade**: default model switched to streaming paraformer bilingual zh-en (real-time, bilingual, better English & noise robustness); new **SenseVoice high-accuracy mode** (optional, ITN + punctuation, closest to WeChat voice input).
- **说话人边界重构（VAD 切片先行）**：Silero VAD 切割语言段作为「转写 + speaker embedding + 气泡」的统一边界，修复下一说话人开头被并进上一说话人最终结果的问题。
  - **VAD-first speaker segmentation**: Silero VAD segments speech into the unified transcript/embedding/bubble boundary, fixing the next speaker's onset being spliced onto the previous speaker's final result.
- **说话人多窗口判定**：头/尾/整段 embedding 投票 + 句内拆句自愈；后台 FastClustering 回补订正误归属。
  - **Multi-window speaker attribution**: head/tail/whole-segment embedding voting + in-sentence split-and-heal; background FastClustering backfill corrects misattributions.
- **应用内自动更新**（Windows 全自动、macOS 引导手动下载），基于 tauri-plugin-updater + GitHub Releases 托管 `latest.json`。
  - **In-app auto-update** (Windows fully automatic, macOS guided manual download) via tauri-plugin-updater + GitHub Releases `latest.json`.

### Changed / 变更
- **主界面与大字模式滚动修复**：字幕列表改为容器级滚动，头部按钮不再随内容滚走；新内容「近底部自动跟随、上翻暂停、回到最新按钮」；最新字幕底部留白；大字模式字号 1.8× → 2.2×。
  - **Main window & focus-mode scrolling fixed**: the caption list scrolls in-container (toolbar stays put); follow-latest when near bottom, pause on scroll-up, "Back to latest" button; breathing room below the latest caption; focus-mode font bumped 1.8× → 2.2×.
- **发布管线升级**：改用 `tauri-action` 生成含签名与 `latest.json` 的更新产物；`bundle.targets` 增加 `app` 以产出 macOS `.app.tar.gz` 更新包；固定 `@tauri-apps/plugin-updater` 与 Rust crate 版本对齐。
  - **Release pipeline upgraded**: switched to `tauri-action` producing signed update artifacts + `latest.json`; added `app` target for macOS `.app.tar.gz`; pinned the JS updater plugin to match the Rust crate.

### Fixed / 修复
- 修复「第二说话人的开头被挂到前一个说话人的末尾」的边界泄漏。
  - Fixed the boundary leak where a second speaker's onset got appended to the previous speaker's output.
- 修复 Windows 大字模式下最新字幕不能正确显示在屏幕内的问题（容器级滚动替代 `scrollIntoView`，避免 WebView2 连动祖先）。
  - Fixed Windows focus-mode where the latest caption was not shown correctly (container scrolling replaces `scrollIntoView`, avoiding WebView2 ancestor scroll).
- 修复无 VAD 降级路径丢失 speaker embedding 的回归（SCD 降级时仍能按音色分人）。
  - Fixed a regression where the no-VAD degraded path dropped speaker embeddings (SCD still groups by voice when VAD is unavailable).

### Security / 安全
- 更新签名密钥体系：Ed25519 密钥用于更新产物签名；私钥只存 CI Secrets，公钥写入 `tauri.conf.json`。
  - Update signing key system: Ed25519 keys sign update artifacts; private key only in CI Secrets, public key in `tauri.conf.json`.

---

## [0.3.1] — 2026-08-21

### Fixed / 修复
- **ASR 模型目录修复**：移除与 sherpa-onnx 1.13.6 不兼容（加载即崩溃 / 无可下载公开来源）的两个 X-ASR 模型，避免「点了开始识别却一个字都没有」的静默失败；sidecar 启动握手（超时/出错时明确报错而非静默）。
  - **ASR model catalog fix**: removed two X-ASR models incompatible with sherpa-onnx 1.13.6 (crashed on load / no downloadable public source), eliminating the silent "started but no text" failure; added sidecar startup handshake (explicit error on timeout/crash).

---

## [0.3.0] — 2026-08-19

### Added / 新增
- **发布管线**：自包含 Python 运行时（python-build-standalone）+ GitHub Release 草稿自动构建 macOS（arm64/x64）DMG 与 Windows NSIS 安装包；换机可跑、免系统 Python。
  - **Release pipeline**: self-contained Python runtime (python-build-standalone) + automated GitHub Release draft building macOS (arm64/x64) DMG and Windows NSIS installers; portable, no system Python needed.
- **品牌落地**：TalkSee/语见 新 logo 与应用图标。
  - **Branding**: TalkSee/语见 new logo and app icons.

### Changed / 变更
- **运行时打包修正**：`tauri.conf.json` resources 由 map+glob 改为目录映射，保留 `venv/` 层级；Windows 解释器路径增加 python-build-standalone 布局回退。
  - **Runtime packaging fix**: resources changed from map+glob to directory mapping, preserving the `venv/` layout; Windows interpreter path falls back to the python-build-standalone layout.
- 发布工作流（`release.yml`）：矩阵 macOS arm64/x64 + Windows x64，打 tag 或手动触发。
  - Release workflow (`release.yml`): matrix of macOS arm64/x64 + Windows x64, triggered by tag or manually.

---

## [0.2.0] — 2026-08-17

### Added / 新增
- **首个完整版 README**：功能特性 / 架构 / 快速开始 / 下载安装 / 打包 / 测试 / 目录结构。
  - **First full README**: features / architecture / quick start / download & install / build / test / project layout.
- **品牌命名**：项目更名「语见 TalkSee」，仓库与品牌文案统一。
  - **Branding**: project renamed to "语见 TalkSee"; repo and brand copy unified.

> 说明：0.1.x 阶段为功能积攒期（未打 tag），其全部功能在 0.2.0 形成第一个可公开描述的完整版本，见下方 **v0.1.0 功能积攒**。
> Note: The 0.1.x phase was the feature-accumulation period (no tags). Its features formed the first publicly described complete version at 0.2.0 — see **v0.1.0 Feature Accumulation** below.

---

## [0.1.0] — 功能积攒期（2026-08，无公开 tag） / Feature accumulation (Aug 2026, no public tag)

> 这是内部开发阶段，版本停留于 0.1.0，功能逐步落地到 0.2.0 前的完整形态。这里按功能列出全部落地内容（对应 ticket 索引 `docs/tickets.md`）。
> This was the internal development phase, staying at version 0.1.0 while features accumulated into the complete form seen before 0.2.0. All landed capabilities are listed here by feature (matching `docs/tickets.md`).

### Added / 新增
- **工程脚手架 (T1)**：Tauri 2.x + `engine` Rust crate + React 前端 + Tauri IPC 事件桥（`bridge://` 心跳与 `engine://` 事件流）。
  - **Scaffolding (T1)**: Tauri 2.x + `engine` Rust crate + React frontend + Tauri IPC event bridge (`bridge://` heartbeat and `engine://` event stream).
- **冒烟管线 (T2)**：合成转写 → 带说话人/颜色的气泡事件流（垂直切片验证）。
  - **Smoke pipeline (T2)**: synthetic transcripts → speaker/color-labeled bubble events (vertical-slice verification).
- **LLM 整理管线 (T8)**：固定节奏（5s/10s）+ 防抖 + 单在途 + editId 单调校验 + 双轨展示；整理失败保留原文。
  - **LLM cleanup pipeline (T8)**: fixed rhythm (5s/10s) + debounce + single in-flight + editId monotonic guard + dual-track display; raw kept on failure.
- **真实麦克风 + 本地流式 ASR (T4)**：cpal 采集/重采样到 16kHz + sherpa-onnx sidecar + 实时 partial。
  - **Real mic + local streaming ASR (T4)**: cpal capture/resample to 16kHz + sherpa-onnx sidecar + live partials.
- **显示定制 (T3)**：主题三态（auto/light/dark）+ 置顶大字模式 + 字号/字体/文字颜色 + localStorage 持久化。
  - **Display customization (T3)**: theme tri-state (auto/light/dark) + always-on-top focus mode + font size/family/text color + localStorage persistence.
- **真实 LLM 接入 (T9)**：OpenAI 兼容 SSE 流式 + 退避重试 ×3 + 流式增量填充 + 500 字/批窗口 + 前端长度单调守卫。
  - **Real LLM integration (T9)**: OpenAI-compatible SSE streaming + retry ×3 + streaming delta fill + 500-char/batch window + frontend length monotonic guard.
- **说话人切换检测 (T5, T15)**：speaker embedding（ERes2NetV2）余弦匹配 + 自动编号 + 颜色稳定 + 短句/噪声保护（T15 修复每条 final 都新建说话人的幻影问题）。
  - **Speaker change detection (T5, T15)**: speaker embedding (ERes2NetV2) cosine match + auto-numbering + stable colors + short-utterance/noise guards (T15 fixed phantom-speaker creation on every final).
- **手动命名与头像 (T6)**：说话人档案本地存储 + 点击改名 + 头像选择器/换一批。
  - **Manual naming & avatars (T6)**: speaker profiles in local storage + click-to-rename + avatar picker/shuffle.
- **云端 ASR 可切换 (T7)**：Deepgram 兼容流式 WebSocket，设置一键切换。
  - **Switchable cloud ASR (T7)**: Deepgram-compatible streaming WebSocket, one-click toggle in settings.
- **会议纪要 (T10)**：停止后把片段分批（~500 字/批 + 滚动上文）交给 LLM，汇总为【要点】【行动项】【待办】。
  - **Meeting minutes (T10)**: on stop, batch segments (~500 chars/batch + rolling context) to the LLM and summarize into key points / action items / to-dos.
- **会话历史与导出 (T11)**：会话自动保存、重启仍在、历史可重开；导出 Markdown/TXT/SRT（逐条带时间码）。
  - **Session history & export (T11)**: sessions auto-save, survive restarts, reopenable; export Markdown/TXT/SRT (timestamped per line).
- **设置系统 (T12)**：标签页分组 + 操作提示 + 持久化 + 即时生效。
  - **Settings system (T12)**: tabbed groups + hints + persistence + instant effect.
- **托盘常驻 + 全局热键 + 单实例 (T13)**：托盘 + ⌘/Ctrl+Shift+L/H/S/T + 重复启动唤回主窗口。
  - **Tray + global hotkeys + single instance (T13)**: tray + ⌘/Ctrl+Shift+L/H/S/T + relaunch summons the main window.
- **首启向导 (T14)**：运行环境检测 + ASR/说话人模型下载（可选国内镜像）+ 手动开始识别。
  - **First-run wizard (T14)**: environment check + ASR/speaker-model download (optional China mirror) + manual start recognition.
- **模型选择与配置 (T16)**：ASR / Embedding 模型目录 manifest + 模型选择 + 下载镜像（HF ↔ hf-mirror 自动回退）+ 断点续传 + LLM 整理开关。
  - **Model selection & config (T16)**: ASR/Embedding model manifest + selection + download mirror (HF ↔ hf-mirror auto-fallback) + resumable downloads + LLM cleanup toggle.

---

## 更多 / More

- 各版本详情、实现总结与架构决策见 `docs/`（`T*-implementation-summary.md`、`docs/adr/`、调研报告）。
- Per-version details, implementation notes, and architecture decisions live in `docs/` (`T*-implementation-summary.md`, `docs/adr/`, research reports).

[0.4.0]: https://github.com/smallmj/talksee/releases/tag/v0.4.0
[0.3.1]: https://github.com/smallmj/talksee/releases/tag/v0.3.1
[0.3.0]: https://github.com/smallmj/talksee/releases/tag/v0.3.0
[0.2.0]: https://github.com/smallmj/talksee/releases
[0.1.0]: https://github.com/smallmj/talksee/releases
