//! 托盘常驻 + 全局热键 + 会话状态镜像（T13）。
//!
//! 职责（薄胶水层，不承载业务逻辑）：
//! - **托盘图标**：`TrayIconBuilder`（Tauri 2 内置 `tray-icon` feature）创建常驻
//!   托盘，右键菜单含「显示主窗口 / 隐藏主窗口 / ▶ 开始识别 / ⏸ 停止识别并生成纪要
//!   / 退出」，左键单击切换主窗口显示/隐藏；点关闭按钮只是隐藏窗口，进程与托盘常驻。
//! - **会话状态镜像**：监听 `engine://event`（[crate::pipeline::ENGINE_EVENT]），
//!   按 `SessionStarted` / `SessionStopped` / `MinutesReady` 更新托盘 tooltip 与
//!   菜单项文本/可用性——与前端状态徽章同源（同一个事件流），保证界面与托盘一致。
//! - **全局热键**：`tauri-plugin-global-shortcut`（Tauri 2 插件）注册
//!   `CmdOrCtrl+Shift+L`（唤出）、`CmdOrCtrl+Shift+H`（隐藏）、`CmdOrCtrl+Shift+S`
//!   （开始识别）、`CmdOrCtrl+Shift+T`（停止识别并生成纪要）。**注册失败不致命**：
//!   热键被系统占用时记录告警并继续运行（托盘/菜单仍可用），避免拖垮启动。
//! - **单实例**：`tauri-plugin-single-instance`（Tauri 2 插件）在 [crate::run] 的
//!   Builder 链首注册——重复启动时唤起已有主窗口（聚焦/取消最小化），避免麦克风与
//!   ASR sidecar 被多个实例竞争。

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Listener, Manager,
};

use crate::pipeline::{self, SessionControl, ENGINE_EVENT};

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
pub(crate) struct TrayState {
    /// 托盘图标句柄（用于更新 tooltip）。
    tray: TrayIcon,
    /// 「▶ 开始识别」菜单项（随会话状态切换文本/可用性）。
    start_item: MenuItem<tauri::Wry>,
    /// 「⏸ 停止识别并生成纪要」菜单项。
    stop_item: MenuItem<tauri::Wry>,
    /// `engine://event` 监听句柄（持有防 drop 后 unlisten）。
    _listener: tauri::EventId,
}

/// 创建托盘、注册全局热键、挂接会话状态镜像（T13 入口，在 setup 中调用）。
///
/// 返回 `Box<dyn std::error::Error>` 以兼容 tauri setup 闭包的签名；全局热键
/// 注册失败已在内部降级为非致命，不会上抛。
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 托盘图标 + 右键菜单 + 事件（菜单点击 / 左键单击切换窗口）。
    let (tray, start_item, stop_item) = build_tray(app)?;

    // 2. 会话状态镜像：监听 engine 事件流，更新托盘 tooltip 与菜单项。
    //    Tauri 2 的 App 本身无 listen 方法，需用 handle + tauri::Listener trait；
    //    listen 返回 EventId（非 EventHandler）。闭包里用 try_state 取状态，
    //    避免启动竞态（事件先于 manage 到达）时 state::<TrayState> 直接 panic。
    let handle = app.handle().clone();
    let listener = handle.clone().listen(ENGINE_EVENT, move |event| {
        let Ok(evt) = serde_json::from_str::<engine::EngineEvent>(event.payload()) else {
            return;
        };
        let Some(state) = handle.try_state::<TrayState>() else {
            return;
        };
        match evt {
            engine::EngineEvent::SessionStarted => {
                // 识别中：可停止，不可（重新）开始。
                let _ = state
                    .tray
                    .set_tooltip(Some(format!("{TRAY_TOOLTIP_PREFIX} — 识别中")));
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

    // 3. 全局热键（注册失败内部降级为非致命）。
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
///
/// 初始状态为「已停止」（开始可用、停止禁用）；引擎事件流到达后由状态镜像
/// 闭包校正——避免冷启动默认假设错误导致的短暂错误状态。
fn build_tray(
    app: &mut tauri::App,
) -> Result<(TrayIcon, MenuItem<tauri::Wry>, MenuItem<tauri::Wry>), Box<dyn std::error::Error>> {
    let show_i = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let hide_i = MenuItem::with_id(app, MENU_HIDE, "隐藏主窗口", true, None::<&str>)?;
    let start_i = MenuItem::with_id(app, MENU_START, "▶ 开始识别", true, None::<&str>)?;
    let stop_i = MenuItem::with_id(app, MENU_STOP, "⏸ 停止识别并生成纪要", false, None::<&str>)?;
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
        .tooltip(format!("{TRAY_TOOLTIP_PREFIX} — 已停止"))
        .menu(&menu)
        // 左键单击不弹菜单（由 on_tray_icon_event 切换窗口显示/隐藏）。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_HIDE => hide_main_window(app),
            MENU_START => start_recognition(app),
            MENU_STOP => stop_recognition(app),
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
                toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok((tray, start_i, stop_i))
}

/// 注册全局热键（tauri-plugin-global-shortcut，Tauri 2 v2 线）。
///
/// 按键与动作一一对应：`CmdOrCtrl+Shift+L` 唤出、`CmdOrCtrl+Shift+H` 隐藏、
/// `CmdOrCtrl+Shift+S` 开始识别、`CmdOrCtrl+Shift+T` 停止识别并生成纪要。
///
/// **非致命设计**：插件的 setup 里逐个 `manager.register`，任一热键被系统占用会
/// 让整个插件初始化失败并上抛。这里把 `app.plugin` 的错误捕获并降级——热键不可用
/// 时托盘菜单与窗口内按钮仍可完成全部操作。
fn setup_shortcuts(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};

    let plugin = tauri_plugin_global_shortcut::Builder::new()
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
                start_recognition(app);
            } else if shortcut.matches(mods, Code::KeyT) {
                stop_recognition(app);
            }
        })
        .build();

    if let Err(err) = app.handle().plugin(plugin) {
        eprintln!("[tray] 全局热键注册失败（降级为仅托盘/菜单操作）: {err}");
    }
    Ok(())
}

/// 开始识别（托盘菜单与全局热键共用）：复用 [pipeline::start_session]。
fn start_recognition(app: &AppHandle) {
    let control = app.state::<SessionControl>();
    let _ = pipeline::start_session(control);
}

/// 停止识别并生成纪要（托盘菜单与全局热键共用）：复用 [pipeline::stop_session]。
fn stop_recognition(app: &AppHandle) {
    let control = app.state::<SessionControl>();
    let _ = pipeline::stop_session(control);
}

/// 左键单击托盘：主窗口可见则隐藏，隐藏则唤出聚焦（复用 [show_main_window]）。
fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            show_main_window(app);
        }
    }
}

/// 唤出主窗口：取消最小化 → 显示 → 聚焦（供托盘菜单 / 热键 / 单实例回调复用）。
pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 隐藏主窗口（托盘常驻，窗口消失但进程与托盘仍在）。
pub(crate) fn hide_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}
