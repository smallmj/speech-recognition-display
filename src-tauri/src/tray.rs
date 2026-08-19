//! 托盘常驻 + 全局热键 + 会话状态镜像（T13）。
//!
//! 职责（薄胶水层，不承载业务逻辑）：
//! - **托盘图标**：`TrayIconBuilder`（Tauri 2 内置，无需外部插件）创建常驻托盘，
//!   右键菜单含「显示主窗口 / 隐藏主窗口 / ▶ 开始识别 / ⏸ 停止识别并生成纪要 / 退出」，
//!   左键单击切换主窗口显示/隐藏。
//! - **会话状态镜像**：监听 `engine://event`（[crate::pipeline::ENGINE_EVENT]），
//!   按 `SessionStarted` / `SessionStopped` / `MinutesReady` 更新托盘 tooltip 与
//!   菜单项文本/可用性——与前端状态徽章同源（同一个事件流），保证界面与托盘一致。
//! - **全局热键**：`tauri-plugin-global-shortcut`（Tauri 2 插件，v2 线）注册
//!   `CmdOrCtrl+Shift+L`（唤出）、`CmdOrCtrl+Shift+H`（隐藏）、`CmdOrCtrl+Shift+S`
//!   （开始识别）、`CmdOrCtrl+Shift+T`（停止识别并生成纪要）。
//! - **单实例**：`tauri-plugin-single-instance`（Tauri 2 插件，v2 线）在
//!   [crate::run] 的 Builder 链首注册——重复启动时唤起已有主窗口（聚焦/取消最小化），
//!   避免麦克风与 ASR sidecar 被多个实例竞争。

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

use crate::pipeline::{self, ENGINE_EVENT, SessionControl};

/// 托盘菜单项 id（`on_menu_event` 按此分发）。
const MENU_SHOW: &str = "tray-show";
const MENU_HIDE: &str = "tray-hide";
const MENU_START: &str = "tray-start";
const MENU_STOP: &str = "tray-stop";
const MENU_QUIT: &str = "tray-quit";

/// 全局热键：`CmdOrCtrl` 跨平台（macOS=Command，Windows/Linux=Ctrl）。
const HOTKEY_SHOW: &str = "CmdOrCtrl+Shift+L";
const HOTKEY_HIDE: &str = "CmdOrCtrl+Shift+H";
const HOTKEY_START: &str = "CmdOrCtrl+Shift+S";
const HOTKEY_STOP: &str = "CmdOrCtrl+Shift+T";

/// 托盘 tooltip 前缀（后接会话状态）。
const TRAY_TOOLTIP_PREFIX: &str = "实时字幕展示";

/// 托盘运行时状态：托盘句柄 + 需要动态更新的菜单项 + 事件监听句柄。
///
/// 由 [setup] 创建并 `app.manage`；会话状态镜像闭包经 `AppHandle.state` 取回更新。
pub struct TrayState {
    /// 托盘图标句柄（用于更新 tooltip）。
    pub tray: TrayIcon,
    /// 「▶ 开始识别」菜单项（随会话状态切换文本/可用性）。
    pub start_item: MenuItem,
    /// 「⏸ 停止识别并生成纪要」菜单项。
    pub stop_item: MenuItem,
    /// `engine://event` 监听句柄（持有防 drop 后 unlisten）。
    _listener: tauri::EventHandler,
}

/// 创建托盘、注册全局热键、挂接会话状态镜像（T13 入口，在 setup 中调用）。
///
/// 返回 `Box<dyn std::error::Error>` 以兼容 tauri setup 闭包与插件自定义错误
/// （如 `tauri-plugin-global-shortcut` 的热键字符串解析错误）。
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 托盘图标 + 右键菜单 + 事件（菜单点击 / 左键单击切换窗口）。
    let (tray, start_item, stop_item) = build_tray(app)?;

    // 2. 会话状态镜像：监听 engine 事件流，更新托盘 tooltip 与菜单项。
    let handle = app.handle().clone();
    let listener = app.listen(ENGINE_EVENT, move |event| {
        let Ok(evt) = serde_json::from_value::<engine::EngineEvent>(event.payload().clone()) else {
            return;
        };
        let state = handle.state::<TrayState>();
        match evt {
            engine::EngineEvent::SessionStarted => {
                // 识别中：可停止，不可（重新）开始。
                let _ = state.tray.set_tooltip(Some(format!("{TRAY_TOOLTIP_PREFIX} — 识别中")));
                let _ = state.start_item.set_text("▶ 开始识别");
                let _ = state.start_item.set_enabled(false);
                let _ = state.stop_item.set_enabled(true);
            }
            engine::EngineEvent::SessionStopped => {
                // 整理中（停止后生成纪要）：开始/停止均暂不可用。
                let _ = state
                    .tray
                    .set_tooltip(Some(format!("{TRAY_TOOLTIP_PREFIX} — 整理中（生成纪要…）")));
                let _ = state.start_item.set_enabled(false);
                let _ = state.stop_item.set_enabled(false);
            }
            engine::EngineEvent::MinutesReady { .. } => {
                // 已停止 / 纪要就绪：可重新开始识别。
                let _ = state
                    .tray
                    .set_tooltip(Some(format!("{TRAY_TOOLTIP_PREFIX} — 已停止 · 纪要已生成")));
                let _ = state.start_item.set_text("▶ 重新开始识别");
                let _ = state.start_item.set_enabled(true);
                let _ = state.stop_item.set_enabled(false);
            }
            _ => {}
        }
    });

    // 3. 全局热键（tauri-plugin-global-shortcut，Tauri 2 v2 线）。
    setup_shortcuts(app)?;

    // 4. 托管托盘状态（闭包在运行时经 AppHandle.state 访问）。
    app.manage(TrayState {
        tray,
        start_item,
        stop_item,
        _listener: listener,
    });
    Ok(())
}

/// 构建托盘图标 + 右键菜单，返回托盘句柄与需要动态更新的菜单项。
fn build_tray(app: &mut tauri::App) -> Result<(TrayIcon, MenuItem, MenuItem), Box<dyn std::error::Error>> {
    let show_i = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let hide_i = MenuItem::with_id(app, MENU_HIDE, "隐藏主窗口", true, None::<&str>)?;
    // 默认「识别中」（对齐 SessionControl::new 的 session_active=true），
    // 事件流到达后由状态镜像闭包校正文本/可用性。
    let start_i = MenuItem::with_id(app, MENU_START, "▶ 开始识别", false, None::<&str>)?;
    let stop_i = MenuItem::with_id(
        app,
        MENU_STOP,
        "⏸ 停止识别并生成纪要",
        true,
        None::<&str>,
    )?;
    let quit_i = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    let menu = Menu::new(app)?;
    menu.append(&show_i)?;
    menu.append(&hide_i)?;
    menu.append(&sep)?;
    menu.append(&start_i)?;
    menu.append(&stop_i)?;
    menu.append(&sep)?;
    menu.append(&quit_i)?;

    let tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("缺少默认窗口图标").clone())
        .tooltip(format!("{TRAY_TOOLTIP_PREFIX} — 识别中"))
        .menu(&menu)
        // 左键单击不弹菜单（由 on_tray_icon_event 切换窗口显示/隐藏）。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_HIDE => hide_main_window(app),
            MENU_START => {
                let control = app.state::<SessionControl>();
                let _ = pipeline::start_session(control);
            }
            MENU_STOP => {
                let control = app.state::<SessionControl>();
                let _ = pipeline::stop_session(control);
            }
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击（抬起）：主窗口可见则隐藏，隐藏则唤出聚焦。
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok((tray, start_i, stop_i))
}

/// 注册全局热键（tauri-plugin-global-shortcut，Tauri 2 v2 线）。
///
/// 按键与 [setup_shortcuts] 的 handler 一一对应：
/// `CmdOrCtrl+Shift+L` 唤出、`CmdOrCtrl+Shift+H` 隐藏、`CmdOrCtrl+Shift+S` 开始识别、
/// `CmdOrCtrl+Shift+T` 停止识别并生成纪要。
fn setup_shortcuts(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};

    app.handle().plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_shortcuts([HOTKEY_SHOW, HOTKEY_HIDE, HOTKEY_START, HOTKEY_STOP])?
            .with_handler(|app, shortcut, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                // CmdOrCtrl：macOS 为 Command(SUPER)，Windows/Linux 为 Ctrl。
                #[cfg(target_os = "macos")]
                let mods = Modifiers::SUPER | Modifiers::SHIFT;
                #[cfg(not(target_os = "macos"))]
                let mods = Modifiers::CONTROL | Modifiers::SHIFT;

                if shortcut.matches(mods, Code::KeyL) {
                    show_main_window(app);
                } else if shortcut.matches(mods, Code::KeyH) {
                    hide_main_window(app);
                } else if shortcut.matches(mods, Code::KeyS) {
                    let control = app.state::<SessionControl>();
                    let _ = pipeline::start_session(control);
                } else if shortcut.matches(mods, Code::KeyT) {
                    let control = app.state::<SessionControl>();
                    let _ = pipeline::stop_session(control);
                }
            })
            .build(),
    )?;
    Ok(())
}

/// 唤出主窗口：取消最小化 → 显示 → 聚焦（供托盘菜单 / 热键 / 单实例回调复用）。
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 隐藏主窗口（托盘常驻，窗口消失但进程与托盘仍在）。
fn hide_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}
