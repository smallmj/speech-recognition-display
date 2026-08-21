<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="brand/logo-horizontal-dark.png">
    <img alt="语见 TalkSee · 让对话，看得见" src="brand/logo-horizontal-light.png" width="440">
  </picture>
</p>

# 语见（TalkSee）— 听障实时字幕展示系统

> **让对话，看得见。** TalkSee（语见）——为听障人士在面对面多人对话中提供**实时字幕**的跨平台桌面应用（macOS / Windows）。
> 实时把周围人的话转成文字，按说话人显示为彩色气泡 + 随机头像；经 LLM 自动整理成通顺书面语，
> 会话结束后一键生成结构化会议纪要。**MVP 已验证可行，所有功能以 Tauri 2 + Rust engine + Python sherpa-onnx 落地。**

- 版本：v0.4.0
- 构建：[![CI](https://github.com/smallmj/talksee/actions/workflows/ci.yml/badge.svg)](https://github.com/smallmj/talksee/actions/workflows/ci.yml) · [![Release](https://github.com/smallmj/talksee/actions/workflows/release.yml/badge.svg)](https://github.com/smallmj/talksee/actions/workflows/release.yml)
- 许可：[MIT](LICENSE) · 规格：[Issue #1](https://github.com/smallmj/talksee/issues/1)（已关闭）
- 实现总结：`docs/T*-implementation-summary.md` · 架构决策见 `docs/adr/`

---

## 项目初衷

### 为什么做这个

全球有数亿人存在不同程度的听力损失（据 WHO 估算，约 4.3 亿人存在致残性听力损失）。对听障人士来说，
**面对面 3–5 人的日常对话是最难的场景之一**：听不清别人在说什么、跟不上对话节奏、轮到自己发言时又
担心"没听全就接话"。读唇、手语、请人转述，都难以覆盖真实、快速、多人同时发生的对话——一个眼神、
一次抢话、一句低声的插话，都可能让听障者被排除在对话之外。

技术的进步让这件事第一次有了真正可落地的解法：**本地流式语音识别**（sherpa-onnx）已经足够快、
足够准、足够便宜；**大语言模型**可以把口语化的转写整理成通顺的书面语；**按说话人区分**的展示让
对话的结构一目了然。这个项目想做的，就是把这些能力组合成一个"替听障人群听、并让对话看得见"的工具。

### 我们坚持的三个原则

1. **本地优先，隐私第一**：对话是私密的。默认全部在本地完成（离线 ASR、本地模型），音频与文字
   不出本机；云端 ASR / LLM 只有在用户主动启用并配置后才会联网。
2. **实时且可读**：字幕"边说边出"，按说话人分成不同颜色的气泡，默认展示 LLM 整理后的通顺文本、
   可一键核对原文——既要跟上对话节奏，也要读得舒服。
3. **开放、可定制、成本可承担**：开源意味着可以被任何有需要的人使用、审查、改进；本地模型免费
   运行，无需为每场对话付费，也让隐私与安全经得起检查。

### 愿景

让每一场对话，对听障人群都**看得见、跟得上、参与得了**。如果你或你身边的人需要这样的工具，
欢迎使用、反馈、贡献——也欢迎把它带给更需要的人。

---

## 功能特性

### 实时识别（本地优先）
- **流式识别**：本地 sherpa-onnx 流式 ASR，边说边出字（中文为主 + 中英混合）
- **本地优先**：默认本地离线识别，音频与文字不出本机
- **云端可切换**：Deepgram 兼容流式 WebSocket，设置中一键切换并配置 API

### 说话人区分（SCD）
- 基于 **speaker embedding（ERes2NetV2）** 余弦匹配，实时区分说话人并自动编号
- 每位说话人**颜色稳定**、配随机头像；可手动重命名（"说话人 2"→"张三"）、换头像/换一批
- 短句/噪声保护：不产生幻影说话人，长会话颜色不跳变

### 双轨整理（LLM）
- OpenAI 兼容接口（Base URL + API Key + 模型名），**SSE 流式**输出，失败退避重试 3 次
- 按可配置间隔（5s / 10s）自动去口语化、纠错、补标点
- **默认只显示整理版**，一键切换原文；改动词**差异高亮**；整理失败保留原文不丢内容

### 会议纪要
- 停止识别后把片段自动分批（约 500 字/批 + 滚动上文）交给 LLM，再汇总为
  **【要点】【行动项】【待办】** 结构化纪要

### 会话历史与导出
- 会话自动保存到本机，重启后仍在；历史列表可重新打开查看
- 导出 **Markdown / TXT / SRT**（逐条带时间码）

### 显示与交互
- 置顶大字模式（始终置顶 + 超大字体，Esc / ✕ 退出）
- 深浅主题跟随系统并可手动覆盖；自定义字号 / 字体 / 文字颜色
- 气泡流自动滚动到最新内容

### 桌面集成与设置
- **托盘常驻** + **全局热键**（⌘/Ctrl+Shift+L/H/S/T）+ 单实例（重复启动唤回主窗口）
- 首启向导：运行环境检测 + ASR / 说话人模型下载（可选国内镜像），未完成前每次启动回到向导
- 设置中心：常规 / 模型 / LLM 整理 / 显示 / 快捷键 / 历史 / 关于，标签页分组 + 操作提示 + 持久化 + 即时生效
- **应用内自动更新**：Windows 在设置「关于」可一键检查并自动下载安装（用户确认后重启生效）；
  macOS 安装包未签名/未公证，检测到新版本会打开 GitHub Releases 页手动下载

---

## 架构

```
┌──────────────────────────────────────────────────────────┐
│ src/   React 18 前端（薄渲染）                            │
│   双轨气泡流 · 说话人徽章 · 纪要面板 · 历史 · 设置 · 首启向导 │
└───────────────▲──────────────────────────────────────────┘
                │  Tauri IPC（invoke + engine://event 事件流）
┌───────────────┴──────────────────────────────────────────┐
│ src-tauri/   Tauri 2 壳（胶水层）                         │
│   音频采集(cpal) · ASR/云端ASR驱动 · LLM客户端(ureq+SSE)   │
│   托盘/全局热键/单实例 · 会话持久化 · 导出 · 首启与模型下载  │
│   ─────────────────────────────────────────────────────  │
│   sherpa_streaming.py  Python sidecar（stdin/stdout NDJSON│
│   协议，16kHz PCM 流式转写 + speaker embedding 提取）      │
└───────────────▲──────────────────────────────────────────┘
                │  AsrPort / EmbeddingPort / LlmPort（端口注入，不依赖 Tauri）
┌───────────────┴──────────────────────────────────────────┐
│ engine/   Rust 核心库（唯一测试缝）                       │
│   片段管理 · SCD(余弦匹配) · LLM 整理管线(防抖+节奏+editId)│
│   会议纪要分批汇总 · 统一 EngineEvent 事件契约             │
└──────────────────────────────────────────────────────────┘
```

- **engine**（`engine/`）：全部业务逻辑的独立 Rust 库，不依赖 Tauri，通过
  `AsrPort` / `EmbeddingPort` / `LlmPort` 三个端口注入外部能力，对外暴露统一
  事件流。**这是整个应用唯一的测试缝**（合成输入 → 断言事件流，无真实音频/网络/WebView）。
- **Tauri 壳**（`src-tauri/`）：薄胶水层，负责音频采集（cpal）、本地/云端 ASR 驱动、
  LLM 客户端（ureq 阻塞 + SSE 解析）、托盘/热键/单实例、会话持久化与导出、首启与模型下载。
- **Python sidecar**（`src-tauri/sherpa_streaming.py`）：sherpa-onnx 流式 ASR，
  通过 stdin/stdout NDJSON 与 Rust 通信，可独立运行验证 ASR 链路。
- **React 前端**（`src/`）：刻意薄渲染，只消费事件流。

领域词汇见 [CONTEXT.md](CONTEXT.md)；关键决策见 `docs/adr/`（0001 Tauri 跨平台、
0002 本地流式 ASR + 自研 SCD、0003 双轨 LLM 整理、0004 可切换云端 ASR）。

---

## 快速开始（开发模式）

前置要求：Node.js ≥ 18（pnpm）、Rust 工具链（Tauri 2 依赖）、Python 3。

```bash
pnpm install            # 前端依赖
pnpm run setup:runtime  # 创建 src-tauri/.venv 并安装 sherpa-onnx + numpy（幂等，已存在则跳过）
pnpm tauri dev          # 启动开发模式
```

首次启动进入**初始化向导**：检测运行环境 → 下载 ASR 模型（可选 hf-mirror 国内镜像，
断点续传）→ 下载说话人模型（可跳过，跳过则 SCD 降级为单说话人）→ 进入主界面。

> 缺模型 / 想换识别模型：设置 →「模型」页下载或切换；云端 ASR、LLM 整理分别在
> 设置对应页配置（OpenAI 兼容：Base URL + API Key + 模型名）。

## 下载与安装（正式发布）

从 [GitHub Releases](https://github.com/smallmj/talksee/releases) 下载对应平台的安装包：

| 平台 | 安装包 | 说明 |
|------|--------|------|
| macOS（Apple Silicon） | `TalkSee_*_aarch64.dmg` | 打开 DMG，把 TalkSee 拖入「应用程序」 |
| macOS（Intel） | `TalkSee_*_x64.dmg` | 同上 |
| Windows | `TalkSee_*_x64-setup.exe` | NSIS 一键安装 |

> **免签名版**：代码签名/公证尚未启用，首次安装会收到系统安全提示，按下面放行即可。

**macOS**：首次打开若提示"无法验证开发者"，请**右键点 TalkSee → 打开 → 确认**；
或拖入「应用程序」后执行：

```bash
xattr -dr com.apple.quarantine "/Applications/TalkSee.app"
```

**Windows**：若 SmartScreen 提示"Windows 已保护你的电脑"，点 **更多信息 → 仍要运行**。

首次启动进入初始化向导：检测运行环境并下载识别模型（可选国内镜像），之后本地离线识别，音频不出本机。

## 打包

```bash
pnpm tauri build                          # 本地验证：构建期自动执行 pnpm build + package:runtime
TALKSEE_STANDALONE=1 pnpm tauri build     # 正式分发：打入自包含 Python（python-build-standalone），换机可跑
```

产物在 `target/release/bundle/`（macOS DMG：`bundle/dmg/`；Windows NSIS：`bundle/nsis/`）。
默认模式直接复制本机 `src-tauri/.venv`，仅适合本机验收；**正式发布务必用 `TALKSEE_STANDALONE=1`**
（GitHub Actions 发布流程已默认开启）。打包版运行时已内置，首启只做健康检测与模型下载。

## 发布新版本

1. 同步版本号：`src-tauri/tauri.conf.json` / `Cargo.toml` / `package.json` 三处一致。
2. 打 tag 并推送，CI 自动构建 macOS/Windows 安装包并创建**草稿** Release：

   ```bash
   git tag v0.4.0 && git push origin v0.4.0
   ```

3. 到 GitHub Releases 检查草稿、确认无误后点发布。

工作流 [`.github/workflows/release.yml`](.github/workflows/release.yml)：macOS（arm64/x64）出 DMG、
Windows（x64）出 NSIS；tag 触发，也支持仓库页手动触发（Actions → Release → Run workflow）。

## 测试

```bash
cargo test --workspace          # engine 单元测试 + 壳层测试（唯一测试缝）
pnpm build                      # tsc 类型检查 + Vite 构建
pnpm check:dual-track           # 双轨展示回归
pnpm check:llm-nonblocking      # LLM 非阻塞整理回归
pnpm check:focus-exit           # 置顶大字退出回归
pnpm check:scd-embedding        # SCD embedding 回归
```

## 目录结构

```
src/                  React 前端（组件、双轨展示、设置、会话历史、首启向导）
engine/               Rust 核心库（业务逻辑 + 测试缝）
src-tauri/            Tauri 2 壳 + Python sidecar（sherpa_streaming.py）
scripts/              setup-runtime / package-runtime / 回归检查脚本
docs/                 ADR、Ticket 索引、各票实现总结、调研报告
```

## 已知边界（MVP）

- 安装包未做代码签名/公证（macOS Gatekeeper 与 Windows SmartScreen 会提示，放行方式见「下载与安装」）；Windows 支持应用内自动更新，macOS 未签名故升级走 Releases 页手动下载
- 仅面对面麦克风采集；不采集系统/在线会议音频；无气泡回放
- SCD 只做切换检测 + 手动命名，不做全自动 diarization / 跨天身份持久化（v0.4 起按 VAD 段切句 + 头尾多窗口投票 + 后台回补聚类订正，短句/抢话场景仍有模型下限）
- 头像性别不按音色自动选择；无识别中暂停/继续（详见 Issue #1 Out of Scope）
- 云端 ASR 为 Deepgram 兼容协议，其他厂商需增加壳层协议适配
