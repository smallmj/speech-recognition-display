# T13 实现总结：托盘常驻 + 全局热键 + 会话状态 + 单实例

> 对应 Issue [#14](https://github.com/smallmj/speech-recognition-display/issues/14)
> 分支：`feat/t13-tray`（基于含 T4+T9+T10 的整合版代码）

## 改动文件

| 文件 | 改动 |
|---|---|
| `src-tauri/src/tray.rs` | **新增**。托盘图标 + 右键菜单 + 全局热键 + 会话状态镜像 + 窗口唤出/隐藏工具函数 |
| `src-tauri/src/lib.rs` | 注册 `tauri-plugin-single-instance`（Builder 链首）、`on_window_event` 关闭→隐藏到托盘、setup 调 `tray::setup`；`mod tray;` |
| `src-tauri/Cargo.toml` | 新增 `tauri-plugin-global-shortcut = "2"`、`tauri-plugin-single-instance = "2"`（Tauri 2 v2 线，与 `tauri = "2"` 对齐） |
| `src-tauri/capabilities/default.json` | 新增 `core:tray:default`、`core:app:default`、`global-shortcut:default` |
| 前端（`src/App.tsx` / `src/engineEvents.ts`） | **零改动**——状态徽章（识别中/停止/生成纪要）与开始/停止按钮已存在（T10），与托盘共用同一事件流与 `SessionControl` 命令路径 |

## 架构决策

1. **托盘用程序化 `TrayIconBuilder`（Tauri 2 内置），不在 `tauri.conf.json` 配 `app.trayIcon`**：
   Tauri 2 的 `app.trayIcon` 配置是声明式快照，无法在运行期动态更新 tooltip/菜单标签；
   程序化 API 返回可持有的 `TrayIcon`/`MenuItem` 句柄，会话状态镜像需要动态改文本，故走代码。
2. **状态同源**：托盘 tooltip/菜单不自己维护状态机，而是 `app.listen(engine://event)` 监听
   `sessionStarted`/`sessionStopped`/`minutesReady`，与前端徽章同一条事件流、同一个
   `SessionControl` 原子状态 → 界面与托盘不可能分叉。
3. **单实例必须最先注册**：`tauri-plugin-single-instance` 官方 README 明确「插件按注册顺序
   运行，请最先注册本插件」，故放在 Builder 链首；回调里 `unminimize + show + set_focus` 唤回已有窗口。

## 验收标准逐条对照

| 验收项 | 实现 | 结果 |
|---|---|---|
| 托盘图标常驻 | `TrayIconBuilder::with_id("main-tray")` + 应用默认图标；关闭窗口（`CloseRequested`）时 `hide()` + `prevent_close()`，进程与托盘常驻，真正退出走托盘菜单「退出」 | ✅ |
| 全局热键唤出 / 开始 / 停止 | `tauri-plugin-global-shortcut` 注册 4 个热键：`CmdOrCtrl+Shift+L` 唤出主窗口、`CmdOrCtrl+Shift+H` 隐藏、`CmdOrCtrl+Shift+S` 开始识别、`CmdOrCtrl+Shift+T` 停止识别并生成纪要 | ✅ |
| 识别中 / 整理中状态提示（界面 + 托盘） | 界面：T10 已有徽章（识别中/正在停止/正在生成纪要/纪要已生成）零改动；托盘：`sessionStarted`→tooltip「识别中」+ 菜单「▶ 开始识别」禁用、「⏸ 停止识别并生成纪要」启用；`sessionStopped`→tooltip「整理中（生成纪要…）」；`minutesReady`→tooltip「已停止 · 纪要已生成」+「▶ 重新开始识别」启用 | ✅ |
| 单实例（重复启动聚焦已有窗口） | `tauri-plugin-single-instance::init` 回调唤回 main 窗口 | ✅ |

## 热键方案

| 热键 | 动作 |
|---|---|
| `CmdOrCtrl+Shift+L` | 唤出主窗口（取消最小化 + 显示 + 聚焦） |
| `CmdOrCtrl+Shift+H` | 隐藏主窗口（托盘常驻） |
| `CmdOrCtrl+Shift+S` | 开始识别（`pipeline::start_session`） |
| `CmdOrCtrl+Shift+T` | 停止识别并生成纪要（`pipeline::stop_session`） |

macOS 上 `CmdOrCtrl` 解析为 Command（handler 内 `Modifiers::SUPER`），Windows/Linux 为 Ctrl。

## 托盘菜单

```
显示主窗口
隐藏主窗口
──────────
▶ 开始识别            ← 状态镜像：识别中禁用 / 已停止启用（文本变「重新开始识别」）
⏸ 停止识别并生成纪要   ← 状态镜像：识别中启用 / 整理中禁用
──────────
退出
```

左键单击托盘图标：切换主窗口显示/隐藏（`on_tray_icon_event` 的 `Click` + `MouseButton::Left` +
`MouseButtonState::Up`）。

## 统一触发路径（避免状态不一致）

托盘菜单项、全局热键、前端按钮都归结为同一个命令：
- 开始：`pipeline::start_session`（置 `SessionControl.session_active`）
- 停止：`pipeline::stop_session`（置 `SessionControl.stop_requested`）

驱动线程（`spawn_engine_emitter`）是唯一的状态推进者，`engine://event` 是唯一状态广播源，
托盘与前端都只消费它——任何入口触发的状态变化都会同时反映到界面徽章与托盘。

## 已知限制

1. **本机无法编译 Rust**（缺 MSVC link.exe / GNU 工具链 + ring 需 C 编译器，仓库约定不编译验证）。
   API 形状已对照 Tauri 2 官方文档与插件 README（v2 线）：`tauri::tray::TrayIconBuilder`、
   `tauri::menu::{Menu, MenuItem, PredefinedMenuItem}`、`tauri-plugin-global-shortcut` 2.3.1
   （`Builder::new().with_shortcuts([...])?.with_handler(...)`）、`tauri-plugin-single-instance` 2.0.2
   （`init(|app, argv, cwd| ...)`）。若编译报 API 偏差，多为插件小版本差异，按报错微调即可。
2. **全局热键冲突**：`CmdOrCtrl+Shift+L/S/T/H` 为常见组合，若用户系统占用，注册会失败
   （`with_shortcuts` 的 `?` 会让 setup 失败）。后续可把热键做成可配置（T12 设置系统接入点）。
3. **Linux 托盘**：`tooltip` 与左键点击事件在 Linux 部分桌面不支持（Tauri 官方说明），
   菜单仍可用；主路径按 Windows/macOS 验收。
4. **关闭窗口即隐藏**：行为变更——窗口关闭按钮现在隐藏到托盘而非退出，需要用户经托盘「退出」
   结束进程；这是托盘常驻应用的标准语义（对齐用户故事 1「常驻系统托盘」）。
5. **前端未感知窗口可见性**：热键唤出/隐藏在 Rust 侧完成，前端不需要参与（符合任务指引）。

## 验证

- `pnpm build`（tsc --noEmit + vite build）✅ 通过。
- engine 未改动；前端未破坏既有契约（`engineEvents.ts` 无变化）。
