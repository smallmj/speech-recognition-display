# T13 实现总结：托盘常驻 + 全局热键 + 会话状态

> 目标：软件托盘常驻、全局热键唤出 / 开始 / 停止识别；界面与托盘在
> 识别中 / 整理中状态有清晰提示；重复启动单实例聚焦已有窗口。
> 验收（Issue #14）：托盘图标常驻 · 全局热键唤出/开始/停止 · 识别中/整理中状态
> 提示（界面 + 托盘）· 单实例。

## 实现内容

- **托盘常驻**（`src-tauri/src/tray.rs` + `lib.rs`）：
  - Tauri 2 内置 `TrayIconBuilder`（`tray-icon` feature）创建常驻托盘；
    右键菜单「显示主窗口 / 隐藏主窗口 / ▶ 开始识别 / ⏸ 停止识别并生成纪要 / 退出」，
    左键单击切换窗口显示/隐藏（`toggle_main_window`）；
  - 窗口关闭按钮改为隐藏到托盘（`on_window_event` 里 `api.prevent_close()` +
    `window.hide()`），进程与托盘常驻；真正退出走托盘菜单「退出」（`app.exit(0)`）；
  - 初始菜单状态为「已停止」（开始可用、停止禁用），由事件流校正——避免冷启动
    默认假设错误的短暂状态错乱。
- **全局热键**（`src-tauri/src/tray.rs::setup_shortcuts`）：
  - `tauri-plugin-global-shortcut`（v2）注册 `CmdOrCtrl+Shift+L`（唤出）、
    `CmdOrCtrl+Shift+H`（隐藏）、`CmdOrCtrl+Shift+S`（开始识别）、
    `CmdOrCtrl+Shift+T`（停止识别并生成纪要）；`CmdOrCtrl` 在 macOS=Command、
    Windows/Linux=Ctrl（`Modifiers` 按 `target_os` 选择）；
  - **非致命设计**：`app.handle().plugin(...)` 失败时记录告警并降级——热键被系统
    占用时应用仍正常启动，托盘菜单与窗口内按钮可完成全部操作；
  - 菜单与热键共用同一动作函数（`start_recognition` / `stop_recognition` /
    `show_main_window` / `hide_main_window`），避免同一动作两处表达。
- **会话状态镜像**（界面 + 托盘同源）：
  - 监听 `engine://event`（与前端状态徽章同一事件流），按
    `SessionStarted`（识别中）/ `SessionStopped`（整理中）/ `MinutesReady`（已停止）
    更新托盘 tooltip 与菜单项文本/可用性；闭包用 `try_state::<TrayState>()` 读取，
    规避启动竞态 panic。
- **单实例**（`src-tauri/src/lib.rs`）：
  - `tauri-plugin-single-instance`（v2）在 Builder 链首注册，重复启动时唤起已有
    主窗口（`show_main_window`），避免麦克风 / ASR sidecar 被多实例竞争。
- **停止识别真正释放 ASR/麦克风**（`src-tauri/src/pipeline.rs`）：
  - 修复此前「停止」只置 `running=false`、ASR 对象一直存活导致麦克风持续打开的
    问题：停止时先排空已缓冲 final、冻结剩余 active，然后 `asr.take().stop()` 释放
    麦克风 / sidecar，`mode = None` 保持一致；整理与纪要阶段不再空转采集。
- **T12 快捷键页同步**（`src/components/SettingsDialog.tsx`）：
  - 「快捷键」标签页由「T13 提供」占位改为列出实际全局热键（Cmd/Ctrl+Shift+L/H/S/T）
    与托盘操作（点击图标唤出 / 关闭窗口隐藏 / 托盘菜单退出）。
- **能力声明**（`src-tauri/capabilities/default.json`）：
  - 补充 `core:tray:default` / `core:app:default` / `global-shortcut:default`，
    与托盘 / 热键插件对齐（Rust 侧使用，预留给前端 IPC）。

## 验证

- `cargo check` / `cargo build`：新增依赖与托盘/热键代码编译通过（Tauri 2.11.5
  需 `MenuItem<tauri::Wry>` 显式泛型、`handle.clone().listen()` + `tauri::Listener`、
  `listen` 返回 `EventId`、`payload()` 为 `&str` 用 `serde_json::from_str`）。
- `cargo test`：engine + 壳层共 62 个测试全部通过。
- `pnpm build`：TypeScript 检查 + Vite 构建通过。
- 运行时冒烟：开发二进制启动后进程存活（无托盘/热键初始化 panic，无注册失败
  告警），bridge ping 正常输出后手动结束进程。

## 已知限制

- 全局热键被系统占用时自动降级为仅托盘/按钮操作（已捕获，不拖垮启动），但当前
  没有在 UI 里显式提示该降级；后续可在设置页展示热键注册状态。
- 托盘图标复用应用默认窗口图标；如需独立托盘图样可在 `TrayIconBuilder` 更换。
- 左键单击切换显隐为通用 UX 约定；部分 Windows 托盘区域对左键语义可能不同
  （Windows 托盘默认左键弹出菜单、右键菜单在 `show_menu_on_left_click(false)` 下
  保持右键菜单），跨平台表现以实际运行为准。
