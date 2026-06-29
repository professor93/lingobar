//! Tray menu, global hotkeys, app commands, and window/app wiring.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// Global app handle for ObjC callback access
pub(crate) static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_store::StoreExt;

mod config;
mod history;
mod injection;
mod log;
mod platform;
mod session;
mod shortcuts;
mod tabs;

#[tauri::command]
fn session_update_text(app: AppHandle, tab_id: String, text: String) {
    log::line(
        &app,
        &format!("cmd session_update_text tab={tab_id} len={}", text.len()),
    );
    if session::is_enabled(&app) {
        session::update_text(&tab_id, text);
    }
}

/// Plain-text clipboard contents, for the "Paste from clipboard" prompt.
#[tauri::command]
fn clipboard_text(app: AppHandle) -> Option<String> {
    log::line(&app, "cmd clipboard_text");
    platform::read_clipboard().filter(|s| !s.trim().is_empty())
}

/// Record a copied translation: `source` + languages from the page, translation
/// from the clipboard (Google just copied it there).
#[tauri::command]
fn history_add(
    app: AppHandle,
    source: String,
    source_lang: Option<String>,
    target_lang: Option<String>,
) {
    log::line(&app, &format!("cmd history_add src_len={}", source.len()));
    let translation = platform::read_clipboard().unwrap_or_default();
    history::add(
        &app,
        source,
        translation,
        source_lang.unwrap_or_default(),
        target_lang.unwrap_or_default(),
    );
}

/// All copy-history entries (newest first) for the History window.
#[tauri::command]
fn history_list(webview: tauri::Webview) -> Vec<history::Entry> {
    if tabs::is_remote_tab(webview.label()) {
        return Vec::new();
    }
    history::list()
}

/// Remove one history entry by its (newest-first) index.
#[tauri::command]
fn history_remove(webview: tauri::Webview, app: AppHandle, index: usize) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    history::remove(&app, index);
}

#[tauri::command]
fn history_clear(webview: tauri::Webview, app: AppHandle) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    history::clear(&app);
}

/// Re-open a history entry in a new translator window, preserving its source
/// text + from/to languages (encoded in the translate.google.com URL).
#[tauri::command]
fn history_open(webview: tauri::Webview, app: AppHandle, index: usize) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    let Some(e) = history::entry_at(index) else {
        return;
    };
    let mut url = String::from("https://translate.google.com/?op=translate");
    if !e.source_lang.is_empty() {
        url.push_str(&format!("&sl={}", e.source_lang));
    }
    if !e.target_lang.is_empty() {
        url.push_str(&format!("&tl={}", e.target_lang));
    }
    url.push_str(&format!("&text={}", urlencode(&e.source)));
    tabs::open_url(&app, &url);
}

/// Write text to the system clipboard (History window Copy source / translated).
#[tauri::command]
fn set_clipboard(webview: tauri::Webview, app: AppHandle, text: String) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    log::line(&app, &format!("cmd set_clipboard len={}", text.len()));
    platform::write_clipboard(&text);
}

/// Percent-encode a string for a URL query value (RFC 3986 unreserved pass
/// through, everything else `%XX`). Avoids pulling in a urlencoding crate.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Append a line to the log from the webview (no-op unless enabled).
#[tauri::command]
fn lb_log(app: AppHandle, msg: String) {
    log::line(&app, &format!("js: {msg}"));
}

/// Open the app's data folder in Finder — settings.json, history.json, the
/// session file, and lingobar.log all live here.
#[tauri::command]
fn open_app_folder(webview: tauri::Webview, app: AppHandle) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
}

/// Tail of the log (last ~64 KB) for the live log-tail window.
#[tauri::command]
fn read_log(webview: tauri::Webview, app: AppHandle) -> String {
    if tabs::is_remote_tab(webview.label()) {
        return String::new();
    }
    // File-logging on → seed from the file; off → from the in-memory last-100.
    if !log::file_logging_on() {
        return log::recent();
    }
    let Some(p) = log::file_path(&app) else {
        return log::recent();
    };
    let data = std::fs::read(&p).unwrap_or_default();
    const CAP: usize = 64 * 1024;
    let start = data.len().saturating_sub(CAP);
    String::from_utf8_lossy(&data[start..]).into_owned()
}

/// Open (or focus) the live log-tail window.
#[tauri::command]
fn open_log_window(webview: tauri::Webview, app: AppHandle) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    if let Some(win) = app.get_webview_window("logview") {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(&app, "logview", WebviewUrl::App("logview.html".into()))
        .title("LingoBar Logs")
        .inner_size(640.0, 420.0)
        .resizable(true)
        .visible(true)
        .build();
}

const DEFAULT_TOGGLE: &str = "Cmd+Ctrl+Shift+KeyT";

#[cfg(target_os = "macos")]
fn settings_json_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support/com.lingobar.menubar/settings.json"),
    )
}
#[cfg(not(target_os = "macos"))]
fn settings_json_path() -> Option<std::path::PathBuf> {
    None
}

/// Read a string setting directly from the store file (used in `run()` before
/// the app/store exist, to load custom shortcuts).
fn load_setting_string(key: &str, default: &str) -> String {
    settings_json_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
        .and_then(|v| v.get(key).and_then(|x| x.as_str().map(String::from)))
        .unwrap_or_else(|| default.to_string())
}

#[derive(serde::Serialize)]
struct Prefs {
    shortcut_toggle: String,
    session_restore: bool,
    launch_at_login: bool,
    log_to_files: bool,
    sleep_idle_tabs: bool,
}

#[tauri::command]
fn get_prefs(app: AppHandle) -> Prefs {
    let store = app.store("settings.json").ok();
    let s = |k: &str, d: &str| {
        store
            .as_ref()
            .and_then(|st| st.get(k))
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| d.to_string())
    };
    let b = |k: &str, d: bool| {
        store
            .as_ref()
            .and_then(|st| st.get(k))
            .and_then(|v| v.as_bool())
            .unwrap_or(d)
    };
    Prefs {
        shortcut_toggle: s("shortcut_toggle", DEFAULT_TOGGLE),
        session_restore: b("session_restore", false),
        launch_at_login: app.autolaunch().is_enabled().unwrap_or(false),
        log_to_files: b("log_to_files", false),
        sleep_idle_tabs: b("sleep_idle_tabs", false),
    }
}

#[tauri::command]
fn set_pref(webview: tauri::Webview, app: AppHandle, key: String, value: bool) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    match key.as_str() {
        "launch_at_login" => {
            let auto = app.autolaunch();
            let _ = if value { auto.enable() } else { auto.disable() };
        }
        "session_restore" | "log_to_files" | "sleep_idle_tabs" => {
            if let Ok(store) = app.store("settings.json") {
                store.set(&key, serde_json::json!(value));
                let _ = store.save();
            }
            if key == "log_to_files" {
                log::set_enabled(value);
                if value {
                    log::line(&app, "=== logging enabled ===");
                }
            }
            // Turning session-restore on captures the current state immediately,
            // matching the old tray toggle.
            if key == "session_restore" && value {
                session::touch();
                session::flush(&app);
            }
        }
        _ => {}
    }
}

/// Current appearance / default settings for the Preferences window.
#[derive(serde::Serialize)]
struct Appearance {
    zoom: i32,
    opacity: i32,
    lang_from: String,
    lang_to: String,
    win_w: i32,
    win_h: i32,
    max_w: i32,
    max_h: i32,
}

#[tauri::command]
fn get_appearance(app: AppHandle) -> Appearance {
    let store = app.store("settings.json").ok();
    let int = |k: &str, d: i64| {
        store
            .as_ref()
            .and_then(|s| s.get(k))
            .and_then(|v| v.as_i64())
            .unwrap_or(d)
    };
    let text = |k: &str, d: &str| {
        store
            .as_ref()
            .and_then(|s| s.get(k))
            .and_then(|v| v.as_str().map(|x| x.to_string()))
            .unwrap_or_else(|| d.to_string())
    };
    let flt = |k: &str, d: f64| {
        store
            .as_ref()
            .and_then(|s| s.get(k))
            .and_then(|v| v.as_f64())
            .filter(|v| *v > 0.0)
            .unwrap_or(d)
    };
    // Max slider bound = the primary monitor's logical size (fallback 1920x1080).
    let (max_w, max_h) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let s = m.size();
            let sf = m.scale_factor();
            ((s.width as f64 / sf) as i32, (s.height as f64 / sf) as i32)
        })
        .unwrap_or((1920, 1080));
    Appearance {
        zoom: int("zoom_level", 100) as i32,
        opacity: (int("opacity", 100) as i32).clamp(30, 100),
        lang_from: text("lang_from", "uz"),
        lang_to: text("lang_to", "en"),
        win_w: flt("last_win_w", 560.0) as i32,
        win_h: flt("last_win_h", 460.0) as i32,
        max_w,
        max_h,
    }
}

/// Set the zoom level: persist + apply live to all open windows.
#[tauri::command]
fn set_zoom_pref(webview: tauri::Webview, app: AppHandle, level: i32) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    log::line(&app, &format!("cmd set_zoom_pref {level}"));
    if let Ok(store) = app.store("settings.json") {
        store.set("zoom_level", serde_json::json!(level));
        let _ = store.save();
    }
    tabs::set_zoom_all(&app, level as f64 / 100.0);
}

/// Set the window opacity (30–100): persist + apply live to all open windows.
#[tauri::command]
fn set_opacity_pref(webview: tauri::Webview, app: AppHandle, percent: i32) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    let percent = percent.clamp(30, 100);
    log::line(&app, &format!("cmd set_opacity_pref {percent}"));
    if let Ok(store) = app.store("settings.json") {
        store.set("opacity", serde_json::json!(percent));
        let _ = store.save();
    }
    tabs::set_opacity_all(&app, percent);
}

/// Resize all translator windows + save the size as the default for new windows
/// (the Preferences width/height sliders).
#[tauri::command]
fn set_window_size(webview: tauri::Webview, app: AppHandle, width: i32, height: i32) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    log::line(&app, &format!("cmd set_window_size {width}x{height}"));
    tabs::set_size_all(&app, width as f64, height as f64);
}

/// Set the DEFAULT language pair for new tabs/windows (persist only; already-open
/// tabs keep their pair — windows read the store when created).
#[tauri::command]
fn set_default_languages(webview: tauri::Webview, app: AppHandle, from: String, to: String) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    log::line(&app, &format!("cmd set_default_languages {from}->{to}"));
    // Accept only known codes (this is reachable from page content and is
    // formatted into a URL): `to` must be a configured language; `from` may also
    // be "auto" (the source picker's Auto-detect option).
    let known = |c: &str| config::LANGUAGES.iter().any(|(code, _)| *code == c);
    if !(from == "auto" || known(&from)) || !known(&to) {
        return;
    }
    if let Ok(store) = app.store("settings.json") {
        store.set("lang_from", serde_json::json!(from));
        store.set("lang_to", serde_json::json!(to));
        let _ = store.save();
    }
}

/// Reset all open windows to the default size.
#[tauri::command]
fn reset_window_size(webview: tauri::Webview, app: AppHandle) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    log::line(&app, "cmd reset_window_size");
    tabs::reset_size_all(&app);
}

/// The configured (code, name) language list for the Preferences pickers.
#[tauri::command]
fn get_languages() -> Vec<(String, String)> {
    config::LANGUAGES
        .iter()
        .map(|(c, n)| (c.to_string(), n.to_string()))
        .collect()
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Open the developer's page in the default browser (About window link).
#[tauri::command]
fn open_developer_link(webview: tauri::Webview) {
    if tabs::is_remote_tab(webview.label()) {
        return;
    }
    let _ = std::process::Command::new("open")
        .arg("https://github.com/professor93")
        .spawn();
}

/// Validate + save a rebound global shortcut (applied on next launch).
#[tauri::command]
fn set_shortcut(webview: tauri::Webview, app: AppHandle, which: String, combo: String) -> bool {
    if tabs::is_remote_tab(webview.label()) {
        return false;
    }
    if combo.parse::<Shortcut>().is_err() {
        return false;
    }
    if which != "toggle" {
        return false;
    }
    let Ok(store) = app.store("settings.json") else {
        return false;
    };
    store.set("shortcut_toggle", serde_json::json!(combo));
    let _ = store.save();
    true
}

/// Open (or focus) a local HTML window (Preferences, History, log viewer).
fn open_app_window(app: &AppHandle, label: &str, url: &str, title: &str, w: f64, h: f64) {
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(w, h)
        .resizable(false)
        .visible(true)
        .build();
}

/// Open (or focus) the Preferences window — used by the tray and the ⌘, hotkey.
pub(crate) fn open_preferences(app: &AppHandle) {
    open_app_window(app, "prefs", "prefs.html", "LingoBar Preferences", 380.0, 560.0);
}

/// Open (or focus) the resizable History window.
fn show_history_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("history") {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "history", WebviewUrl::App("history.html".into()))
        .title("LingoBar History")
        .inner_size(560.0, 520.0)
        .resizable(true)
        .visible(true)
        .build();
}

fn toggle_window(app: &AppHandle) {
    tabs::toggle(app);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Flush the session on SIGTERM/SIGINT (kill, Ctrl+C, logout). SIGKILL can't
    // be caught — the continuous debounced writer covers that case (<=~1s loss).
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        if let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT]) {
            std::thread::spawn(move || {
                // Block until the first SIGTERM/SIGINT, flush, then exit.
                if signals.forever().next().is_some() {
                    if let Some(app) = APP_HANDLE.get() {
                        session::flush(app);
                    }
                    std::process::exit(0);
                }
            });
        }
    }

    let hotkey_enabled = Arc::new(AtomicBool::new(true));
    let hotkey_enabled_for_menu = hotkey_enabled.clone();

    let last_toggle = Arc::new(AtomicU64::new(0));
    let last_toggle_clone = last_toggle.clone();

    // Global toggle shortcut (customizable in Preferences; default ⌃⌘⇧T). Loaded
    // from settings, falling back to the default.
    let default_toggle = Shortcut::new(
        Some(Modifiers::META | Modifiers::CONTROL | Modifiers::SHIFT),
        Code::KeyT,
    );
    let toggle_shortcut = load_setting_string("shortcut_toggle", DEFAULT_TOGGLE)
        .parse::<Shortcut>()
        .unwrap_or(default_toggle);
    let toggle_for_menu = toggle_shortcut;

    // Register the global toggle up front. If the OS rejects it (a conflict), log
    // and continue WITHOUT the global hotkey instead of panicking at launch — it
    // can be rebound in Preferences.
    let gs_plugin = match tauri_plugin_global_shortcut::Builder::new()
        .with_shortcuts([toggle_shortcut])
    {
        Ok(builder) => builder
            .with_handler(move |app, _shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                // Toggle (debounced: ignore if < 500ms since last toggle).
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let last = last_toggle_clone.load(Ordering::SeqCst);
                if now.saturating_sub(last) > 500 {
                    last_toggle_clone.store(now, Ordering::SeqCst);
                    toggle_window(app);
                }
            })
            .build(),
        Err(e) => {
            eprintln!(
                "LingoBar: global shortcut registration failed ({e}); continuing without global hotkeys"
            );
            tauri_plugin_global_shortcut::Builder::new().build()
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(gs_plugin)
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            session_update_text,
            clipboard_text,
            history_add,
            history_list,
            history_remove,
            history_clear,
            history_open,
            set_clipboard,
            get_prefs,
            set_pref,
            set_shortcut,
            app_version,
            open_developer_link,
            lb_log,
            open_app_folder,
            read_log,
            open_log_window,
            get_appearance,
            set_zoom_pref,
            set_opacity_pref,
            set_window_size,
            set_default_languages,
            reset_window_size,
            get_languages,
            tabs::tab_new,
            tabs::tab_select,
            tabs::tab_close,
            tabs::tab_rename,
            tabs::tab_list,
            tabs::window_new,
            tabs::window_close,
            tabs::window_pin
        ])
        .setup(move |app| {
            let _ = APP_HANDLE.set(app.handle().clone());

            // File logging (off unless the hidden Preferences toggle is on).
            log::init(app.handle());

            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            // Standard Edit menu so macOS routes ⌘Z / ⌘⇧Z / ⌘X / ⌘C / ⌘V / ⌘A to
            // the focused text field. A menu-bar (accessory) app has no app menu by
            // default, so without this those edit shortcuts silently do nothing.
            {
                let h = app.handle().clone();
                let edit = Submenu::with_items(
                    &h,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(&h, None)?,
                        &PredefinedMenuItem::redo(&h, None)?,
                        &PredefinedMenuItem::separator(&h)?,
                        &PredefinedMenuItem::cut(&h, None)?,
                        &PredefinedMenuItem::copy(&h, None)?,
                        &PredefinedMenuItem::paste(&h, None)?,
                        &PredefinedMenuItem::select_all(&h, None)?,
                    ],
                )?;
                app.set_menu(Menu::with_items(&h, &[&edit])?)?;
            }

            // The translator is a custom frameless multi-webview window, created
            // on demand by the tray / global hotkey (menubar pattern: nothing
            // shows on launch).

            // In-app hotkeys (Cmd+T/N/`, Ctrl+Tab, speak/mic, Esc) via a local
            // NSEvent monitor.
            platform::install_key_monitor();

            // Debounced session writer + restore-on-launch (when enabled).
            session::start_writer(app.handle());
            session::restore(app.handle());

            // Debug aid (off by default): LINGOBAR_SHOW=1 opens a window on launch.
            if std::env::var("LINGOBAR_SHOW").is_ok() {
                tabs::toggle(app.handle());
            }

            let autostart_manager = app.autolaunch();
            let autostart_enabled = autostart_manager.is_enabled().unwrap_or(false);

            let autostart_item = CheckMenuItem::with_id(
                app,
                "autostart",
                "Launch at Login",
                true,
                autostart_enabled,
                None::<&str>,
            )?;

            let hotkey_item = CheckMenuItem::with_id(
                app,
                "hotkey",
                "Enable Hotkey (⌘⌃⇧T)",
                true,
                true,
                None::<&str>,
            )?;

            // Tray item opens the History window.
            history::load(app.handle());
            let history_item = MenuItem::with_id(app, "history", "History", true, None::<&str>)?;

            let separator = MenuItem::with_id(app, "sep", "─────────────", false, None::<&str>)?;
            let prefs_item =
                MenuItem::with_id(app, "preferences", "Preferences…", true, None::<&str>)?;
            let about_item = MenuItem::with_id(app, "about", "About", true, None::<&str>)?;
            let restart_item = MenuItem::with_id(app, "restart", "Restart", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(
                app,
                &[
                    &autostart_item,
                    &hotkey_item,
                    &history_item,
                    &separator,
                    &prefs_item,
                    &about_item,
                    &restart_item,
                    &quit_item,
                ],
            )?;

            // Create tray icon - template icon for macOS light/dark mode support.
            // Decoded via Tauri's built-in PNG support (image-png feature) so we
            // don't pull in the heavy `image` crate just for one small icon.
            let icon = Image::from_bytes(include_bytes!("../icons/iconTemplate.png"))
                .expect("Failed to load tray icon");

            let shortcut_for_menu = toggle_for_menu;

            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .icon_as_template(true)
                .tooltip("LingoBar")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "autostart" => {
                        let autostart = app.autolaunch();
                        let is_enabled = autostart.is_enabled().unwrap_or(false);
                        if is_enabled {
                            let _ = autostart.disable();
                        } else {
                            let _ = autostart.enable();
                        }
                    }
                    "hotkey" => {
                        let current = hotkey_enabled_for_menu.load(Ordering::SeqCst);
                        hotkey_enabled_for_menu.store(!current, Ordering::SeqCst);

                        if current {
                            let _ = app.global_shortcut().unregister(shortcut_for_menu);
                        } else {
                            let _ = app.global_shortcut().register(shortcut_for_menu);
                        }
                    }
                    "about" => {
                        // Custom About window.
                        open_app_window(app, "about", "about.html", "About LingoBar", 460.0, 400.0);
                    }
                    "preferences" => {
                        open_app_window(app, "prefs", "prefs.html", "LingoBar Preferences", 380.0, 560.0);
                    }
                    "restart" => {
                        app.restart();
                    }
                    "quit" => {
                        session::flush(app);
                        app.exit(0);
                    }
                    "history" => show_history_window(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);

                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Final flush on graceful quit (covers Cmd+Q, tray Quit, logout).
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                session::flush(app_handle);
            }
        });
}
