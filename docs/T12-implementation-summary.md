# T12 实现总结：设置系统（标签页 + 操作提示 + 持久化）

> Issue: [\#13](https://github.com/smallmj/speech-recognition-display/issues/13)
> 依赖：T3（显示）、T7（ASR）、T9（LLM）、T11（历史）均已完成。

## 实现内容

- **标签页分组设置对话框**（`src/components/SettingsDialog.tsx`）：
  - 常规 / ASR / LLM 整理 / 显示 / 快捷键 / 历史 / 关于 七个标签页（规格 #31）；
  - 每项设置都带操作提示（规格 #32）：说明该项的作用与配置方式，紧贴控件显示；
  - 头部按钮改为「⚙ 设置」打开对话框；主流程不再散落独立的 ASR / LLM / 历史
    折叠面板，全部收进设置界面。
- **常规配置持久化 + 即时生效**（`src-tauri/src/app_settings.rs`）：
  - 整理间隔（5s / 10s）写入 app config 目录 `app-settings.json`（规格 #21、#42）；
  - `load_app_settings` / `save_app_settings` Tauri 命令；前端切换后立即保存；
  - `pipeline.rs` 驱动线程每秒轮询常规配置，档位变化时调用
    `CleanupPipeline::set_rhythm_duration`（新增，`engine/src/cleanup.rs`），
    无需重建整理管线，保存后即时生效；新会话重建管线时同样恢复已保存档位。
- **ASR / LLM 配置嵌入**：`AsrConfigPanel` / `LlmConfigPanel` /
  `SessionHistoryPanel` 支持 `embedded` 模式（不显示折叠按钮、始终展开），
  原保存与热切换机制不变（ASR 每秒轮询热切换、LLM 每次请求前重读）。
- **显示设置**：主题 / 字号 / 字体 / 文字颜色 / 置顶大字迁移到「显示」标签页，
  localStorage 持久化 + 即时应用照旧（T3 行为不变）。
- **快捷键页**：列出当前窗口内可用的操作与按键（打开/关闭设置、退出置顶大字、
  原文/整理版切换、整理间隔、开始/停止识别）；全局热键 / 托盘常驻提示由 T13 提供。
- **历史页**：嵌入会话历史面板（列表 + 重新打开 + 导出 Markdown / TXT / SRT）。
- **关于页**：版本 / 技术栈 / 数据与配置位置 / 规格与实现索引。

## 验收标准逐条对照

- [x] **标签页分组设置界面完整呈现**：七个标签页，左侧分组导航 + 右侧内容区。
- [x] **每项设置带操作提示**：每个分组、每项控件旁均有简短提示文字。
- [x] **配置持久化，重启保留**：显示设置在 localStorage；ASR / LLM /
  常规设置在系统应用配置目录；会话历史在应用数据目录。
- [x] **配置即时生效**：ASR 热切换、LLM 请求前重读、整理间隔每秒轮询热更新
  （无需重建管线）、显示设置即时应用根元素样式。

## 验证

- `cargo test`：engine 60 个测试 + 壳层 39 个测试全部通过
  （新增 app_settings 4 个测试 + 整理间隔热更新 1 个测试）。
- `pnpm build`：TypeScript 检查 + Vite 构建通过。
- `pnpm check:focus-exit`：通过（置顶大字模式仍有 Esc 与悬浮按钮两条退出途径）。
- `pnpm check:dual-track` / `pnpm check:llm-nonblocking`：通过。

## 已知限制

- 快捷键页为说明性内容：全局热键 / 托盘常驻依赖 T13（PR #18 返工中），
  当前版本支持窗口内按钮与 Esc 操作。
- ASR / LLM 表单在切换标签页时会重新加载已保存配置（未保存的改动会丢弃），
  需先点「保存」再切换。
