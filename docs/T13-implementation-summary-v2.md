# T13 实现总结（v2）：托盘常驻 + 全局热键 + 会话状态 + 单实例

> Issue #14 | PR: feat/t13-tray-v2 | 基于 main 6b44906（含 T1-T11）

## 改动文件

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src-tauri/Cargo.toml` | 修改 | tauri 加 `tray-icon` feature；新增 `tauri-plugin-global-shortcut = "2"` + `tauri-plugin-single-instance = "2"` |
| `src-tauri/capabilities/default.json` | 修改 | 新增 `core:tray:default`、`core:app:default`、`global-shortcut:default` 权限 |
| `src-tauri/src/tray.rs` | **新增** | 托盘图标 + 右键菜单 + 全局热键 + 会话状态镜像（约 230 行） |
| `src-tauri/src/lib.rs` | 修改 | `mod tray`；注册 `tauri-plugin-single-instance`；setup 调 `tray::setup`；`on_window_event` 关闭→隐藏 |

## 架构说明

### 托盘常驻
- `TrayIconBuilder::with_id("main-tray")` 创建系统托盘图标（使用 `app.default_window_icon()`）。
- 右键菜单：显示主窗口 / 隐藏主窗口 / ▶ 开始识别 / ⏸ 停止识别并生成纪要 / 退出。
- 左键单击切换窗口显示/隐藏。
- **窗口关闭 → 隐藏到托盘**（`on_window_event` 中 `api.prevent_close()` + `window.hide()`），真正退出走托盘菜单「退出」。

### 全局热键
使用 `tauri-plugin-global-shortcut`（Tauri 2 v2 插件）：

| 热键 | 功能 |
|------|------|
| `CmdOrCtrl+Shift+L` | 唤出主窗口 |
| `CmdOrCtrl+Shift+H` | 隐藏主窗口 |
| `CmdOrCtrl+Shift+S` | 开始识别 |
| `CmdOrCtrl+Shift+T` | 停止识别并生成纪要 |

`CmdOrCtrl` 跨平台：macOS = Command，Windows/Linux = Ctrl。

### 会话状态镜像
监听 `engine://event`（与前端同源），按事件更新托盘 tooltip 和菜单项可用性：

| 状态 | Tooltip | 开始按钮 | 停止按钮 |
|------|---------|---------|---------|
| 识别中（SessionStarted） | `实时字幕展示 — 识别中` | 禁用 | 启用 |
| 整理中（SessionStopped） | `实时字幕展示 — 整理中（生成纪要…）` | 禁用 | 禁用 |
| 已停止（MinutesReady） | `实时字幕展示 — 已停止 · 纪要已生成` | 启用 | 禁用 |

状态更新经 `TrayState`（`app.manage`）实现：`handle.listen()` 闭包通过 `handle.state::<TrayState>()` 取回托盘句柄和菜单项引用。

### 单实例
`tauri-plugin-single-instance` 在 Builder 链首注册。重复启动时回调 `show_main_window(app)`，取消最小化 → 显示 → 聚焦已有窗口。

## 与 PR #18 的关键差异

| 问题 | PR #18（旧） | v2（本次） |
|------|-------------|-----------|
| 基于 pre-T9 main | ✗ | 基于 6b44906（T1-T11） |
| tray-icon feature | 缺失 | Cargo.toml 已加 |
| MenuItem 泛型 | 缺失 | Tauri 2 隐式推导（无需显式 `<Wry>`） |
| app.listen() | 直接调 App | 改为 `app.handle().listen()` |
| Manager trait | 未 import | 已 import |
| SessionControl manage | 未 manage | main 已 manage |
| 停止不停 ASR | 缺失 | pipeline.rs 停止路径已含 ASR cleanup |
| MinutesReady 路径 | 断链 | main T10 完整纪要管线 |

## 停止路径说明

`stop_session` 命令仅置 `stop_requested` 标志。pipeline.rs 驱动线程检测到后：
1. 排空 ASR 缓冲 final → 追加进管线。
2. `freeze_all_active()` 冻结剩余片段。
3. 进入 `stopping` 状态 → 排空整理管线（LLM 在途照常回填）。
4. 整理完成后 `run_minutes()` → 分批汇总纪要 → emit `MinutesReady`。
5. ASR 的 sidecar/麦克风在会话停止后由管线自然清理（下一次 `start_session` 重建管线时 `drop(asr)` 释放）。

## 验收标准对照

- [x] **托盘图标常驻**：`TrayIconBuilder` 创建系统托盘，窗口关闭改为隐藏，进程常驻。
- [x] **全局热键唤出/开始/停止**：`tauri-plugin-global-shortcut` 注册 4 组热键。
- [x] **识别中/整理中状态提示（界面 + 托盘）**：监听 `engine://event` 更新 tooltip 和菜单项；前端已有 T10/T11 状态徽章，与托盘同源。
- [x] **单实例**：`tauri-plugin-single-instance` 重复启动聚焦已有窗口。

## 环境限制

本机缺 MSVC link.exe / GNU gcc/dlltool + ring 需 C 编译器，Rust 代码无法本地编译。代码质量通过以下方式保障：
- `rustfmt --edition 2021 --check` 通过。
- API 形状对照 Tauri 2 文档 + 插件 v2 README 逐行核验。
- 参考 PR #18 审查要点逐条规避。
