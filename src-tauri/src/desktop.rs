//! 桌面集成：悬浮窗、右键菜单、系统托盘、关闭按钮行为。
//!
//! 设计要点（2026-09-18 与 Tony 定的方案）：
//! 1. 悬浮窗是**独立的无边框透明窗口**，与主界面解耦。
//! 2. 悬浮窗要读压缩设置（后缀/保存方式/元数据/目标尺寸），走 Rust 侧的**设置镜像**：
//!    主窗每次保存顺手推一份过来。不去赌两个 webview 共享 localStorage —— 跨窗口
//!    存储是不是同一份不是保证行为。
//! 3. 关闭按钮的拦截放在 Rust 侧（`prevent_close`），前端只负责首次那个提示弹窗，
//!    这样即使主窗没跑起来（或被隐藏到托盘）行为也一致。
//!
//! ⚠️⚠️ 悬浮窗和菜单窗**必须在 `tauri.conf.json` 里声明**，不要改回 `WebviewWindowBuilder`：
//! 系统文件拖放（OLE / IDropTarget）只有在**配置声明的窗口**上才可靠注册，而且要求窗口在
//! 拖拽发生**之前**就已经被 DWM 合成过。用 Rust builder 建 + `visible(false)` 这条路径时，
//! 往悬浮窗拖文件会「什么反应都没有」—— Rust 侧根本收不到 drop 事件，没有任何报错。
//! （2026-09-19 定位：Tauri #13761 + WebView2 的 drop target 注册时机。同理见
//! `references/Hermes_架构与排障.md` 里 Tauri 打包那几条。）
//! 所以悬浮窗声明成 `visible: true` 但初始停在屏幕外（`PARK_POS`），启动即完成合成与
//! drop target 注册；开关只做「挪到用户位置 / 挪回屏幕外」，**不调 `hide()`**
//! （避免 drop target 注册被撤销，那样「关掉再打开」就又不灵了）。

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, Window,
    WindowEvent,
};

pub const MAIN_LABEL: &str = "main";
pub const FLOAT_LABEL: &str = "float";
pub const MENU_LABEL: &str = "menu";

/// 悬浮窗尺寸 = 胶囊（168×40）+ 四周 14px 透明边（留给 CSS 阴影）。
/// 透明边不能大：透明区域照样会吃掉鼠标事件。
const FLOAT_SHADOW_PAD: i32 = 14;
const FLOAT_WIDTH: f64 = (168 + FLOAT_SHADOW_PAD * 2) as f64;
const FLOAT_HEIGHT: f64 = (40 + FLOAT_SHADOW_PAD * 2) as f64;

/// 展开态卡片尺寸。拖拽投放、压缩进行中、结果展示共用同一张卡，所以只有这一套尺寸。
const FLOAT_DRAG_CARD_WIDTH: i32 = 280;
const FLOAT_DRAG_CARD_HEIGHT: i32 = 160;
const FLOAT_DRAG_WIDTH: f64 = (FLOAT_DRAG_CARD_WIDTH + FLOAT_SHADOW_PAD * 2) as f64;
const FLOAT_DRAG_HEIGHT: f64 = (FLOAT_DRAG_CARD_HEIGHT + FLOAT_SHADOW_PAD * 2) as f64;

/// 右键菜单窗：菜单本体 184×140 + 12px 透明边（留阴影）。**尺寸必须与 tauri.conf.json 一致**，
/// 改这里就要同步改那边。
const MENU_SHADOW_PAD: i32 = 12;
const MENU_WIDTH: f64 = (184 + MENU_SHADOW_PAD * 2) as f64;
const MENU_HEIGHT: f64 = (140 + MENU_SHADOW_PAD * 2) as f64;

/// 首次出现的位置：屏幕右侧，垂直 38% 处，距屏幕边缘 40px。
const FLOAT_MARGIN: f64 = 40.0;
const FLOAT_VERTICAL_RATIO: f64 = 0.38;

/// 「停车场」：所有显示器之外。悬浮窗保持 visible 停在这里，好让 WebView2 尽早合成。
const PARK_POS: i32 = -10000;

/// 拖动窗口时位置写盘的节流间隔。
const POS_WRITE_INTERVAL_MS: u64 = 400;

const CLOSE_EVENT: &str = "close-requested";
const SHOWN_EVENT: &str = "float-shown";
const HIDDEN_EVENT: &str = "float-hidden";
const MENU_SHOWN_EVENT: &str = "menu-shown";

static BUSY: AtomicBool = AtomicBool::new(false);
/// 悬浮窗的「期望状态」。
///
/// ⚠️ ready / wanted / visible 必须放在**同一把锁**里一起读写，别退回成几个独立的
/// `AtomicBool`：显示判定是「读两个标志 → 再决定要不要显示」的 check-then-act，
/// 拆成多个原子量中间就有间隙 —— 主窗的 `set_float_visible` 与浮窗自己的 `reveal_float`
/// 可能都判定为「不用我显示」，结果谁都没显示，悬浮窗就一直停在停车场。
/// （旧版正是这样：用户视角是「设置里明明勾了显示悬浮窗，启动后却有时不出现」。）
/// 见 `ensure_float_visible_locked`。
struct FloatWant {
    /// 页面是否已经跑起来（跑起来才允许挪进视野，避免白屏闪一下）。
    ready: bool,
    /// 用户开着「显示悬浮窗」开关。
    wanted: bool,
    /// 当前是否已经挪到用户位置（幂等：重复请求不会反复定位、不会重复播进场动效）。
    visible: bool,
}
static FLOAT_WANT: Mutex<FloatWant> =
    Mutex::new(FloatWant { ready: false, wanted: false, visible: false });
/// 卡片展开中：期间的位置/尺寸变化不写进位置记忆。
static FLOAT_EXPANDED: AtomicBool = AtomicBool::new(false);
/// 展开前的胶囊窗口矩形 (x, y, w, h)，收拢时还原。
static FLOAT_IDLE_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
static SETTINGS_MIRROR: Mutex<Option<Value>> = Mutex::new(None);
static FLOAT_POS: Mutex<Option<(i32, i32)>> = Mutex::new(None);
static LAST_POS_WRITE: AtomicU64 = AtomicU64::new(0);

/// 压缩任务的全局闸门。
///
/// `engine` 的取消信号是全局 `AtomicBool`，主窗和悬浮窗同时跑会互相踩
/// （一边取消会把另一边也打断）。用 RAII 保证任何返回路径都会释放。
pub struct BusyGuard;

impl BusyGuard {
    pub fn acquire() -> Result<Self, String> {
        BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| BusyGuard)
            .map_err(|_| "已有压缩任务进行中，请稍后再试".to_string())
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

// ===== 设置镜像 =====

#[tauri::command]
pub fn sync_settings(settings: Value) {
    if let Ok(mut slot) = SETTINGS_MIRROR.lock() {
        *slot = Some(settings);
    }
}

#[tauri::command]
pub fn get_settings() -> Value {
    SETTINGS_MIRROR
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or(Value::Null)
}

fn mirrored_string(key: &str) -> Option<String> {
    SETTINGS_MIRROR
        .lock()
        .ok()?
        .as_ref()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

fn remember_close_action(action: &str) {
    let Ok(mut slot) = SETTINGS_MIRROR.lock() else {
        return;
    };
    let mut value = slot.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("closeAction".to_string(), Value::String(action.to_string()));
    }
    *slot = Some(value);
}

// ===== 悬浮窗 =====

/// 用户把开关关掉 / 在右键菜单里选了「隐藏悬浮窗」/ Alt+F4：期望状态与窗口位置一起收起来。
/// **不调 `hide()`**：注册好的 drop target 会跟着没了，下次打开开关就又开始「拖进去没反应」。
fn park_float(app: &AppHandle) {
    if let Ok(mut want) = FLOAT_WANT.lock() {
        want.wanted = false;
        want.visible = false;
    }
    if let Some(window) = app.get_webview_window(FLOAT_LABEL) {
        let _ = window.set_position(PhysicalPosition::new(PARK_POS, PARK_POS));
    }
}

fn is_parked(x: i32, y: i32) -> bool {
    x < -5000 || y < -5000
}

/// 页面就绪、且用户确实开着开关时，把停车场里的悬浮窗挪到用户位置并显示。
/// 失败会把错误往上抛（不吞），这样调用方能看见，也能靠前端的幂等重试补上。
fn bring_float_into_view(app: &AppHandle) -> Result<(), String> {
    let window = app.get_webview_window(FLOAT_LABEL).ok_or("悬浮窗尚未创建")?;
    let (x, y) = start_position(&window);
    // 定位失败就不要再 show 了 —— 那只是把窗口「显示」在屏幕外的停车场里，
    // 用户看到的是「点了开关没反应」，比直接报错更难查。
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    let _ = window.set_always_on_top(true);
    let _ = app.emit_to(FLOAT_LABEL, SHOWN_EVENT, ());
    Ok(())
}

/// 显示判定与落地必须在**同一把锁**里一口气做完：读 wanted/ready → 决定 → 挪窗口 → 记 visible。
/// 中间不能放锁，否则两个调用方（主窗的 `set_float_visible` 与浮窗自己的 `reveal_float`）
/// 可能都判定「不用我显示」，结果谁都没显示。
fn ensure_float_visible_locked(app: &AppHandle, want: &mut FloatWant) -> Result<(), String> {
    if !want.wanted || !want.ready || want.visible {
        return Ok(());
    }
    bring_float_into_view(app)?;
    want.visible = true;
    Ok(())
}

/// ⚠️ 必须声明为 async：Tauri 官方 Known issues 明确写了「Windows 上用同步命令建窗会死锁」。
#[tauri::command]
pub async fn set_float_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    if !visible {
        park_float(&app);
        return Ok(());
    }
    let mut want = FLOAT_WANT.lock().map_err(|_| "悬浮窗状态锁异常".to_string())?;
    want.wanted = true;
    // 页面还没就绪就先记账，等 reveal_float 再挪（免得先闪一块白底）。
    ensure_float_visible_locked(&app, &mut want)
}

/// 悬浮窗页面跑起来后自己调这个。
#[tauri::command]
pub fn reveal_float(app: AppHandle) -> Result<(), String> {
    let mut want = FLOAT_WANT.lock().map_err(|_| "悬浮窗状态锁异常".to_string())?;
    want.ready = true;
    ensure_float_visible_locked(&app, &mut want)
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) {
    show_main(&app);
}

/// 悬浮窗展开 / 收拢。拖文件经过、压缩进行中、结果展示都得把胶囊撑成卡片，
/// 所以这个开关由前端按「当前视图是不是 idle」来推，不再只服务于拖拽态。
#[tauri::command]
pub async fn float_card_mode(app: AppHandle, active: bool) -> Result<(), String> {
    let window = app.get_webview_window(FLOAT_LABEL).ok_or("悬浮窗尚未创建")?;
    set_card_mode(&window, active)
}

fn set_card_mode(window: &WebviewWindow, active: bool) -> Result<(), String> {
    if active {
        if FLOAT_EXPANDED.swap(true, Ordering::SeqCst) {
            return Ok(()); // 已经展开了
        }
        let scale = window.scale_factor().unwrap_or(1.0);
        let position = window.outer_position().map_err(|error| error.to_string())?;
        let size = window.outer_size().map_err(|error| error.to_string())?;
        let (px, py) = (position.x, position.y);
        let (pw, ph) = (size.width as i32, size.height as i32);
        if let Ok(mut slot) = FLOAT_IDLE_RECT.lock() {
            *slot = Some((px, py, pw, ph));
        }
        let width = (FLOAT_DRAG_WIDTH * scale).round() as i32;
        let height = (FLOAT_DRAG_HEIGHT * scale).round() as i32;
        // 以原胶囊中心为锚点膨胀，视觉上是「原地展开」而不是从左上角长出来
        let center_x = px + pw / 2;
        let center_y = py + ph / 2;
        let (x, y) = clamp_to_monitor(window, center_x - width / 2, center_y - height / 2, width, height);
        window
            .set_size(PhysicalSize::new(width as u32, height as u32))
            .map_err(|error| error.to_string())?;
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| error.to_string())?;
        Ok(())
    } else {
        if !FLOAT_EXPANDED.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let Some((x, y, width, height)) = FLOAT_IDLE_RECT.lock().ok().and_then(|mut slot| slot.take())
        else {
            return Ok(());
        };
        window
            .set_size(PhysicalSize::new(width as u32, height as u32))
            .map_err(|error| error.to_string())?;
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// 把矩形限制在当前显示器之内，免得浮窗贴着屏幕右边时膨胀出去一半。
fn clamp_to_monitor(window: &WebviewWindow, x: i32, y: i32, width: i32, height: i32) -> (i32, i32) {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return (x, y);
    };
    let origin = monitor.position();
    let size = monitor.size();
    let max_x = origin.x + size.width as i32 - width;
    let max_y = origin.y + size.height as i32 - height;
    (
        x.clamp(origin.x, max_x.max(origin.x)),
        y.clamp(origin.y, max_y.max(origin.y)),
    )
}

// ===== 右键菜单（HTML 窗口版）=====

/// 悬浮窗右键菜单。用**独立的 HTML 小窗**渲染，而不是系统原生菜单：
/// 原生菜单在 Windows 上只能是系统样式，画不出设计稿那种圆角卡片 + 图标。
/// 悬浮窗本身只有 196×68，HTML 菜单画在它里面会被裁掉，所以单独开一个窗口。
#[tauri::command]
pub async fn float_context_menu(app: AppHandle) -> Result<(), String> {
    let menu = app.get_webview_window(MENU_LABEL).ok_or("菜单窗口尚未创建")?;
    let scale = menu.scale_factor().unwrap_or(1.0);
    let cursor = app.cursor_position().map_err(|error| error.to_string())?;
    let (cursor_x, cursor_y) = (cursor.x.round() as i32, cursor.y.round() as i32);
    let width = (MENU_WIDTH * scale).round() as i32;
    let height = (MENU_HEIGHT * scale).round() as i32;
    let (x, y) = menu_position(&app, cursor_x, cursor_y, width, height);
    menu.set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())?;
    menu.show().map_err(|error| error.to_string())?;
    let _ = menu.set_always_on_top(true);
    let _ = menu.set_focus();
    let _ = app.emit_to(MENU_LABEL, MENU_SHOWN_EVENT, ());
    Ok(())
}

/// 菜单默认贴在光标右下 6px；贴边时翻到另一侧，最后再夹进显示器范围。
fn menu_position(app: &AppHandle, cursor_x: i32, cursor_y: i32, width: i32, height: i32) -> (i32, i32) {
    const GAP: i32 = 6;
    let mut x = cursor_x + GAP;
    let mut y = cursor_y + GAP;
    let monitor = app
        .monitor_from_point(cursor_x as f64, cursor_y as f64)
        .ok()
        .flatten();
    let Some(monitor) = monitor else {
        return (x, y);
    };
    let origin = monitor.position();
    let size = monitor.size();
    let (left, top) = (origin.x, origin.y);
    let (right, bottom) = (left + size.width as i32, top + size.height as i32);
    if x + width > right {
        x = cursor_x - width - GAP;
    }
    if y + height > bottom {
        y = cursor_y - height - GAP;
    }
    (
        x.clamp(left, (right - width).max(left)),
        y.clamp(top, (bottom - height).max(top)),
    )
}

/// 菜单项被点中后由页面回报。菜单动作集中在 Rust 侧，页面只管好看。
#[tauri::command]
pub fn float_menu_action(app: AppHandle, action: String) {
    hide_menu(&app);
    match action.as_str() {
        "show_main" => show_main(&app),
        "hide_float" => {
            park_float(&app);
            // 通知主窗把设置里的开关同步关掉，否则开关还显示「开」、悬浮窗却不见了。
            let _ = app.emit_to(MAIN_LABEL, HIDDEN_EVENT, ());
        }
        "quit" => exit_app(&app),
        _ => {}
    }
}

/// 菜单失去焦点时（点了别处）自己收起来，行为对齐系统原生菜单。
#[tauri::command]
pub fn close_float_menu(app: AppHandle) {
    hide_menu(&app);
}

fn hide_menu(app: &AppHandle) {
    if let Some(menu) = app.get_webview_window(MENU_LABEL) {
        let _ = menu.hide();
    }
}

#[tauri::command]
pub fn resolve_close(app: AppHandle, action: String, persist: bool) {
    if persist {
        remember_close_action(&action);
    }
    if action == "tray" {
        hide_main(&app);
    } else {
        exit_app(&app);
    }
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    exit_app(&app);
}

// 悬浮窗**不要**用 WebviewWindowBuilder 建。原因见文件头：builder 建的窗口收不到系统拖放，
// 表现为「往悬浮窗拖文件什么反应都没有」。窗口由 tauri.conf.json 声明，
// prepare_float 只负责补上「建完之后才该做的事」。

/// 悬浮窗由配置声明，建窗工作交给 Tauri；这里补上「建完之后才该做的事」。
fn prepare_float(app: &AppHandle) {
    let Some(window) = app.get_webview_window(FLOAT_LABEL) else {
        return;
    };
    // 见 ensure_float 里的说明：这三个只能在建完窗之后设。
    let _ = window.set_resizable(false);
    let _ = window.set_maximizable(false);
    let _ = window.set_minimizable(false);
    let _ = window.set_always_on_top(true);
    // 配置里声明成 visible: true + 停在屏幕外，就是为了让 WebView2 早点合成、
    // 早点把 OLE drop target 注册好。这里再确认一次位置，防止上次异常退出留下的坐标。
    let _ = window.set_position(PhysicalPosition::new(PARK_POS, PARK_POS));

    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(position) = event {
            record_float_pos(&handle, position);
        }
    });
}

fn prepare_menu(app: &AppHandle) {
    let Some(menu) = app.get_webview_window(MENU_LABEL) else {
        return;
    };
    let _ = menu.set_resizable(false);
    let _ = menu.set_maximizable(false);
    let _ = menu.set_minimizable(false);
    let _ = menu.set_always_on_top(true);
    let _ = menu.hide();
}

// ===== 主窗口 / 退出 =====

/// 显示主窗（托盘左键、菜单「显示主界面」、前端按钮都走这里）。
///
/// ⚠️ 主窗**全程不挪位置**：位置由 `tauri.conf.json` 的 `center: true` 在创建时一次算好，
/// 之后没有任何代码动它。上一版让它停在屏幕外、等页面加载完再挪回中央，用户看到的是
/// 「窗口先出现在一个位置，加载完又跳到另一个位置」—— 位置跳动比那几百毫秒空白难受得多，
/// 所以 `reveal_main` / `center_on_primary` / 3.5s 兜底线程整套已删除，**别再加回来**。
///
/// 白屏由窗口 `backgroundColor: "#f5f6f8"`（= 前端 `--bg`）解决：该值同时落到窗口底色和
/// WebView2 的 `DefaultBackgroundColor`，所以首帧之前露出来的是应用底色，不是白板。
///
/// ⚠️ 别为了「加载完再显示」改成 `visible: false`：WebView2 只在窗口**首次被 DWM 合成**时
/// 注册 OLE drop target，那样建出来的主窗永远拿不到，主界面「拖放图片到此处」会整个失效。
fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.hide();
    }
}

fn exit_app(app: &AppHandle) {
    write_float_pos(app);
    app.exit(0);
}

// ===== 窗口位置持久化 =====

fn state_file(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|directory| directory.join("window-state.json"))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn load_window_state(app: &AppHandle) {
    let Some(path) = state_file(app) else { return };
    let Ok(text) = fs::read_to_string(&path) else { return };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return };
    let Some(position) = value.get("floatPos") else { return };
    let (Some(x), Some(y)) = (
        position.get("x").and_then(Value::as_i64),
        position.get("y").and_then(Value::as_i64),
    ) else {
        return;
    };
    if let Ok(mut slot) = FLOAT_POS.lock() {
        *slot = Some((x as i32, y as i32));
    }
}

fn write_float_pos(app: &AppHandle) {
    let Some(path) = state_file(app) else { return };
    let Some((x, y)) = FLOAT_POS.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    if let Some(directory) = path.parent() {
        let _ = fs::create_dir_all(directory);
    }
    let payload = serde_json::json!({ "floatPos": { "x": x, "y": y } });
    let _ = fs::write(&path, payload.to_string());
}

/// 位置写盘节流：`Moved` 在拖动时是高频事件，不能每次都落盘。
/// 另外两类位置变化不能记：停车场坐标、卡片展开时的临时坐标。
fn record_float_pos(app: &AppHandle, position: &PhysicalPosition<i32>) {
    if FLOAT_EXPANDED.load(Ordering::SeqCst) || is_parked(position.x, position.y) {
        return;
    }
    if let Ok(mut slot) = FLOAT_POS.lock() {
        *slot = Some((position.x, position.y));
    }
    let now = now_millis();
    if now.saturating_sub(LAST_POS_WRITE.load(Ordering::SeqCst)) < POS_WRITE_INTERVAL_MS {
        return;
    }
    LAST_POS_WRITE.store(now, Ordering::SeqCst);
    write_float_pos(app);
}

/// [x, y, width, height]
type MonitorRect = (i32, i32, i32, i32);

fn monitor_rects(window: &WebviewWindow) -> Vec<MonitorRect> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            (
                position.x,
                position.y,
                size.width as i32,
                size.height as i32,
            )
        })
        .collect()
}

/// 左上角落在某块屏幕里就算位置可用。拖到屏幕边上导致悬浮窗大部分在屏外是
/// 用户自己拖的，不做自动纠正；这里只处理「换了分辨率 / 拔了副屏」这种情况。
fn corner_on_monitor(monitors: &[MonitorRect], x: i32, y: i32) -> bool {
    if monitors.is_empty() {
        return true;
    }
    monitors.iter().any(|(left, top, width, height)| {
        x >= *left && x < left + width && y >= *top && y < top + height
    })
}

fn default_position(monitor: MonitorRect, scale: f64) -> (i32, i32) {
    let (left, top, width, height) = monitor;
    let window_width = (FLOAT_WIDTH * scale).round() as i32;
    let window_height = (FLOAT_HEIGHT * scale).round() as i32;
    let margin = (FLOAT_MARGIN * scale).round() as i32;
    (
        left + width - window_width - margin,
        top + (height as f64 * FLOAT_VERTICAL_RATIO).round() as i32 - window_height / 2,
    )
}

/// 悬浮窗该出现在哪。返回 `(x, y)`，**一定会给出一组坐标** ——
/// 调用方拿它去 `set_position`，返回 None 会让窗口留在屏幕外，用户开开关却看不到东西。
fn start_position(window: &WebviewWindow) -> (i32, i32) {
    let monitors = monitor_rects(window);
    if let Some((x, y)) = FLOAT_POS.lock().ok().and_then(|slot| *slot) {
        if !is_parked(x, y) && corner_on_monitor(&monitors, x, y) {
            return (x, y);
        }
    }
    match window.primary_monitor().ok().flatten() {
        Some(monitor) => {
            let size = monitor.size();
            let position = monitor.position();
            default_position(
                (
                    position.x,
                    position.y,
                    size.width as i32,
                    size.height as i32,
                ),
                monitor.scale_factor(),
            )
        }
        // 拿不到显示器（远程会话、显示器热插拔的过程中）：给个安全兜底，
        // 位置不精确能接受，落在屏幕外就完全看不到了。
        None => (120, 160),
    }
}

// ===== 托盘与菜单 =====

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "tray_show_main", "显示主界面", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "tray_quit", "退出微图", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show_item, &separator, &quit_item])?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        // 左键留给「显示主界面」，菜单走右键，符合 Windows 托盘习惯。
        .show_menu_on_left_click(false)
        .tooltip("微图 TinyPic")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    // 托盘是「最小化到托盘」之后唯一的入口，图标不能缺：优先用应用图标，
    // 万一取不到就拿 32×32 兜底，避免出现一个看不见的空白托盘项。
    let icon = app
        .default_window_icon()
        .cloned()
        .or_else(|| tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok());
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "tray_show_main" => show_main(app),
        "tray_quit" => exit_app(app),
        _ => {}
    }
}

fn request_main_close(app: &AppHandle) {
    match mirrored_string("closeAction").as_deref() {
        Some("tray") => hide_main(app),
        Some("exit") => exit_app(app),
        // 还没选过：交给前端弹提示（首次询问），选完由 resolve_close 落地。
        _ => {
            let _ = app.emit_to(MAIN_LABEL, CLOSE_EVENT, ());
        }
    }
}

// ===== 对外入口 =====

pub fn setup(app: &AppHandle) {
    load_window_state(app);
    app.on_menu_event(|app, event| handle_menu(app, event.id().as_ref()));
    if let Err(error) = setup_tray(app) {
        eprintln!("托盘初始化失败: {error}");
    }
    prepare_float(app);
    prepare_menu(app);
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    match event {
        WindowEvent::CloseRequested { api, .. } => match window.label() {
            MAIN_LABEL => {
                api.prevent_close();
                request_main_close(window.app_handle());
            }
            FLOAT_LABEL => {
                // 悬浮窗没有关闭按钮，但 Alt+F4 之类还是会送关闭请求。
                // 这里**不 hide()**（hide 会吊销 drop target，之后又「拖进去没反应」），
                // 直接挪回停车场 —— 与「关掉开关」同一语义；顺手通知主窗把设置开关同步关掉。
                api.prevent_close();
                park_float(window.app_handle());
                let _ = window.app_handle().emit_to(MAIN_LABEL, HIDDEN_EVENT, ());
            }
            MENU_LABEL => {
                // 菜单窗不依赖 drop target，隐藏而不销毁即可。
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        },
        // 菜单点到别处就收起来，行为对齐系统原生菜单。
        WindowEvent::Focused(false) if window.label() == MENU_LABEL => {
            let _ = window.hide();
        }
        _ => {}
    }
}

pub fn on_exit(app: &AppHandle) {
    write_float_pos(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_guard_is_exclusive_and_released_on_drop() {
        let first = BusyGuard::acquire().expect("首次应能拿到闸门");
        assert!(BusyGuard::acquire().is_err(), "第二个任务应被拒绝");
        drop(first);
        assert!(BusyGuard::acquire().is_ok(), "释放后应能再次拿到闸门");
    }

    #[test]
    fn default_position_sits_near_the_right_edge() {
        let (x, y) = default_position((0, 0, 1920, 1080), 1.0);
        assert_eq!(x, 1920 - FLOAT_WIDTH as i32 - 40);
        assert_eq!(y, (1080.0 * FLOAT_VERTICAL_RATIO).round() as i32 - FLOAT_HEIGHT as i32 / 2);
    }

    #[test]
    fn default_position_respects_monitor_origin_and_scale() {
        let (x, y) = default_position((-1920, 0, 1920, 1080), 1.5);
        assert_eq!(
            x,
            -1920 + 1920 - (FLOAT_WIDTH * 1.5).round() as i32 - (FLOAT_MARGIN * 1.5).round() as i32
        );
        assert!(y > 0 && y < 1080);
    }

    #[test]
    fn corner_on_monitor_detects_offscreen_positions() {
        let monitors = vec![(0, 0, 1920, 1080)];
        assert!(corner_on_monitor(&monitors, 100, 100));
        assert!(corner_on_monitor(&monitors, 0, 0));
        assert!(!corner_on_monitor(&monitors, 1920, 100), "正好在右边界外");
        assert!(!corner_on_monitor(&monitors, 100, 1080));
        assert!(!corner_on_monitor(&monitors, -200, 100));
        assert!(corner_on_monitor(&[], -9999, -9999), "拿不到屏幕信息时不做判断");
    }

    #[test]
    fn float_window_keeps_the_pill_shadow_inside_its_bounds() {
        // 胶囊 168×40 + 两侧各 14px 透明边：改尺寸时别把 CSS 阴影挤出窗口
        assert_eq!(FLOAT_WIDTH as i32, 168 + FLOAT_SHADOW_PAD * 2);
        assert_eq!(FLOAT_HEIGHT as i32, 40 + FLOAT_SHADOW_PAD * 2);
    }

    #[test]
    fn card_mode_rect_also_keeps_the_shadow_inside() {
        assert_eq!(FLOAT_DRAG_WIDTH as i32, FLOAT_DRAG_CARD_WIDTH + FLOAT_SHADOW_PAD * 2);
        assert_eq!(FLOAT_DRAG_HEIGHT as i32, FLOAT_DRAG_CARD_HEIGHT + FLOAT_SHADOW_PAD * 2);
        // 菜单窗同理：透明边要罩得住 CSS 阴影
        assert_eq!(MENU_WIDTH as i32, 184 + MENU_SHADOW_PAD * 2);
        assert_eq!(MENU_HEIGHT as i32, 140 + MENU_SHADOW_PAD * 2);
    }

    #[test]
    fn parked_positions_are_recognised() {
        assert!(is_parked(PARK_POS, PARK_POS));
        assert!(is_parked(-9999, 100));
        assert!(!is_parked(0, 0));
        assert!(!is_parked(1684, 376));
    }
}
