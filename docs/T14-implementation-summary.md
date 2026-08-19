# T14 实现总结：首次运行初始化向导 + 手动开始识别

> 目标：clone 后首次运行自动检测运行环境、下载 ASR / embedding 模型（可选国内
> 镜像）、分步打钩确认后进入主界面；每次打开软件不再自动开始识别。

## 实现内容

- **首启向导**（`src/components/FirstRunWizard.tsx`）：
  - 未初始化时启动直接进入向导，确认后才进主界面；未完成前每次启动都回向导；
  - 本地分支步骤：运行环境检测 → 下载 ASR 模型 → 下载说话人模型（可跳过，
    SCD 降级为单说话人）→ 确认进入主界面；
  - 云端分支：填写 Deepgram 兼容云端配置并通过现有 `asr-config` 校验 →
    确认进入主界面；
  - 下载失败可重试，提供「稍后再配」延后出口；设置里提供「重新运行初始化」。
  - 模式/镜像选择即时记忆：未完成或延后配置也会保存，下次启动回填向导。
- **下载与镜像**（`src-tauri/src/first_run.rs`）：
  - 原生 HTTP（ureq）流式下载 + 进度事件 + `.part` 断点续传；
  - 每个模型目录维护 `.download-manifest.json` 记录已下载文件大小，重跑时按大小
    幂等校验，损坏/尺寸不符的文件会重新下载；
  - HuggingFace 官方 / hf-mirror 国内镜像二选一，选择持久化；
  - ASR 模型：`csukuangfj2/sherpa-onnx-x-asr-...-punct-int8-2026-06-03`；
    embedding 模型：`csukuangfj/speaker-embedding-models`（3d-speaker eres2net）。
- **路径抽象**（`src-tauri/src/model_paths.rs`）：
  - 开发模式沿用仓库 `src-tauri/.venv` + `src-tauri/asr-models`；
  - 打包模式运行时从 app 资源 `runtime/` 读取，模型下载到 app 数据目录；
  - `asr.rs` 的 sidecar / 模型解析统一走该模块。
- **打包运行时**（`scripts/setup-runtime.mjs` / `scripts/package-runtime.mjs`）：
  - 克隆后开发环境先 `pnpm run setup:runtime` 创建 venv 并安装 sherpa-onnx/numpy；
  - `pnpm run package:runtime` 把运行时复制进 `src-tauri/resources/runtime/`，
    `tauri.conf.json` 的 `beforeBuildCommand` 已串入该脚本，构建期自动打包；
    正式分发建议用自包含 Python。
  - 本地完成确认由后端复检：运行时就绪 + ASR 模型目录包含 encoder/decoder/joiner
    ONNX 与 tokens/bpe，避免仅靠前端勾选放行。
- **手动开始识别**（`src-tauri/src/pipeline.rs` + `src/App.tsx`）：
  - 启动不再自动开始会话、不拉起麦克风/sidecar，前端状态为「未开始」；
  - 每次点「开始识别」重新拉起 ASR（模型补齐 / 云端配置变更立即生效），
    点「停止并生成纪要」走原有冻结与纪要流程。

## 验证

- `cargo test --workspace`：engine 60 + 壳层 45 个测试全部通过
  （新增 first_run / model_paths 测试）。
- `pnpm build`：TypeScript 检查 + Vite 构建通过。
- `pnpm check:dual-track` / `check:llm-nonblocking` / `check:focus-exit`：通过。

## 已知限制

- ModelScope 上未找到本次固定模型仓库的镜像，故镜像选项只提供 HuggingFace 官方
  与 hf-mirror；后续如上传 ModelScope 镜像可扩展 `DownloadMirror`。
- `package-runtime.mjs` 直接复制本机 venv 适合本地打包/验收；正式分发建议改用
  自包含 Python 运行时，避免依赖构建机系统 Python（见 `resources/runtime/README.md`）。
