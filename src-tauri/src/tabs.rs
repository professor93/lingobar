//! Custom multi-webview tab windows (replacing native NSWindow tabbing).
//!
//! Each logical window is one frameless, rounded, always-on-top Tauri `Window`
//! labelled `win_N`, hosting a chrome webview `win_N_chrome` (the HTML tab bar)
//! across the top and one live `translate.google.com` webview per tab
//! (`win_N_tab_{id}`) below it. Rust owns the per-window `TabState`; the chrome
//! drives it over IPC (`tab_new/tab_select/tab_close/tab_rename/tab_list`) and
//! re-renders from the `tabs_changed` event.
//!
//! A `win_*` window grants the default capability to its child webviews
//! (`capabilities/default.json`); `add_child` runs inline on the main thread,
//! so building from setup/commands never deadlocks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::webview::WebviewBuilder;
use tauri::window::WindowBuilder;
use tauri::{
    AppHandle, Emitter, EventTarget, LogicalPosition, LogicalSize, Manager, PhysicalPosition,
    WebviewUrl, WindowEvent,
};
use tauri_plugin_store::StoreExt;

use crate::{config, injection, platform, session};

/// Base chrome strip height (window controls + tab bar) at 100% zoom, logical px.
const BASE_CHROME_H: f64 = 34.0;
/// How much the chrome strip follows the content zoom (0 = fixed height, 1 = the
/// content's full zoom). Kept gentle so the bar stays usable at extreme zooms.
const CHROME_ZOOM_DAMPING: f64 = 0.3;
/// Window corner radius, logical px.
const CORNER_RADIUS: f64 = 10.0;
const DEFAULT_W: f64 = 560.0;
const DEFAULT_H: f64 = 460.0;
/// Hard caps: at most 10 windows, 10 tabs per window, 30 tabs total. Past any
/// of these, creating a new window/tab silently does nothing.
const MAX_WINDOWS: usize = 10;
const MAX_TABS_PER_WINDOW: usize = 10;
const MAX_TOTAL_TABS: usize = 30;
/// When the opt-in "sleep inactive tabs" pref is on, keep this many
/// most-recently-used *inactive* tab webviews live (a global LRU); older
/// inactive tabs are unloaded to free RAM and reloaded when re-activated.
const SLEEP_CACHE: usize = 3;

/// Per-window tab state, keyed by window label (`win_N`).
fn registry() -> &'static Mutex<HashMap<String, TabState>> {
    static R: OnceLock<Mutex<HashMap<String, TabState>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Global LRU of LIVE *inactive* tab webview labels (most-recent first). A
/// window's active tab is never listed. When it grows past `SLEEP_CACHE`, the
/// least-recently-used tab is slept (webview closed). Only used while the
/// `sleep_idle_tabs` pref is on.
fn inactive_lru() -> &'static Mutex<Vec<String>> {
    static L: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Vec::new()))
}

/// Monotonic source of window labels.
static WIN_SEQ: AtomicU32 = AtomicU32::new(0);

/// Throttle for persisting the "last window size" to settings.json — macOS fires
/// Moved/Resized every frame while a window is dragged/resized, so the
/// synchronous disk write must not run per-frame.
static LAST_SIZE_SAVE_MS: AtomicU64 = AtomicU64::new(0);
const SIZE_SAVE_THROTTLE_MS: u64 = 800;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Number of open custom (`win_*`) windows.
fn window_count(app: &AppHandle) -> usize {
    app.windows()
        .keys()
        .filter(|l| l.starts_with("win_"))
        .count()
}

/// True for a remote translate webview (`win_N_tab_M`). Tauri's ACL doesn't gate
/// app commands, so sensitive commands reject callers matching this; the chrome
/// (`win_N_chrome`) and the local windows (prefs/about/logview/
/// history) don't contain `_tab_`, so they pass.
pub(crate) fn is_remote_tab(label: &str) -> bool {
    label.starts_with("win_") && label.contains("_tab_")
}

/// Total tabs across all custom windows.
fn total_tab_count() -> usize {
    registry()
        .lock()
        .unwrap()
        .values()
        .map(|s| s.tabs.len())
        .sum()
}

#[derive(Serialize, Clone)]
pub(crate) struct TabInfo {
    id: u32,
    title: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct TabsPayload {
    win: String,
    tabs: Vec<TabInfo>,
    active: u32,
    pinned: bool,
}

// ---- Window / tab lifecycle ----------------------------------------------

/// Build a new frameless tab window (chrome webview + empty tab state, hidden).
/// Callers add the first tab, then show it.
fn build_window(app: &AppHandle) -> tauri::Result<tauri::Window> {
    let n = WIN_SEQ.fetch_add(1, Ordering::Relaxed);
    let win = format!("win_{n}");
    let (init_w, init_h) = saved_size(app);

    let window = WindowBuilder::new(app, &win)
        .inner_size(init_w, init_h)
        .min_inner_size(252.0, 155.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(true)
        .skip_taskbar(true)
        .visible(false)
        .center()
        .build()?;

    // Cascade so a new window doesn't stack exactly over the previous one
    // (perfectly overlapping windows look like one confused window). Restored
    // windows override this with their saved geometry.
    if n > 0 {
        if let Ok(pos) = window.outer_position() {
            let off = ((n % 5) as i32) * 32;
            let _ = window.set_position(PhysicalPosition::new(pos.x + off, pos.y + off));
        }
    }

    platform::make_frameless_rounded(&window, CORNER_RADIUS);
    platform::set_window_opacity(&window, saved_opacity(app));

    // Respond to the activating click on an inactive window (button + drag)
    // instead of swallowing it (macOS first-mouse behaviour).
    let mut chrome_wb = WebviewBuilder::new(
        format!("{win}_chrome"),
        WebviewUrl::App("chrome.html".into()),
    )
    .accept_first_mouse(true);
    // Scale the bar to match the (dampened) chrome height at this zoom — applied
    // at document-start so the static bar renders pre-scaled (no flicker).
    let cs = chrome_scale(app);
    if (cs - 1.0).abs() > f64::EPSILON {
        chrome_wb =
            chrome_wb.initialization_script(format!("document.documentElement.style.zoom={cs};"));
    }
    window.add_child(
        chrome_wb,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(init_w, chrome_h(app)),
    )?;

    registry()
        .lock()
        .unwrap()
        .insert(win.clone(), TabState::default());

    let geo_win = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Resized(_) => {
            relayout(&geo_win);
            save_geometry(&geo_win);
        }
        WindowEvent::Moved(_) => save_geometry(&geo_win),
        _ => {}
    });

    crate::log::line(app, &format!("window built {win}"));
    Ok(window)
}

/// Create a new frameless tab window with one initial tab; returns its label.
pub fn new_window(app: &AppHandle) -> tauri::Result<String> {
    if window_count(app) >= MAX_WINDOWS || total_tab_count() >= MAX_TOTAL_TABS {
        crate::log::line(app, "limit reached: not creating a new window");
        return Ok(String::new());
    }
    let window = build_window(app)?;
    add_tab(app, &window, None, None);
    window.show()?;
    let _ = window.set_focus();
    save_geometry(&window);
    Ok(window.label().to_string())
}

/// Open a new window whose first tab loads a specific URL (History re-open: the
/// entry's source text + languages are encoded in the translate.google.com URL).
pub fn open_url(app: &AppHandle, url: &str) {
    if window_count(app) >= MAX_WINDOWS || total_tab_count() >= MAX_TOTAL_TABS {
        crate::log::line(app, "limit reached: not opening a history window");
        return;
    }
    if let Ok(window) = build_window(app) {
        add_tab(app, &window, None, Some(url));
        let _ = window.show();
        let _ = window.set_focus();
        save_geometry(&window);
    }
}

/// Add a tab to an existing window (used by `+` / `⌘T`).
pub fn new_tab(app: &AppHandle, win: &str) {
    if let Some(window) = app.get_window(win) {
        add_tab(app, &window, None, None);
    }
}

/// Make tab `id` active in `win`.
pub fn select_tab(app: &AppHandle, win: &str, id: u32) {
    if let Some(window) = app.get_window(win) {
        let prev = {
            let mut reg = registry().lock().unwrap();
            let Some(state) = reg.get_mut(win) else {
                return;
            };
            let prev = state.active;
            state.select(id);
            prev
        };
        if sleep_enabled(app) {
            manage_sleep(app, &window, win, prev, id);
        }
        // Invariant (regardless of the pref): the active tab always has a live
        // webview — wakes it if it was slept, so toggling sleep off never leaves
        // a blank active tab.
        ensure_active_live(app, &window, win);
        relayout(&window);
        emit_tabs(app, win);
    }
}

/// Select the nth tab (1-indexed) in `win`, if it exists (⌘1-9).
pub fn select_nth(app: &AppHandle, win: &str, n: usize) {
    if n == 0 {
        return;
    }
    let id = registry()
        .lock()
        .unwrap()
        .get(win)
        .and_then(|s| s.tabs.get(n - 1).map(|t| t.id));
    if let Some(id) = id {
        select_tab(app, win, id);
    }
}

/// Select the last tab in `win` (⌘0).
pub fn select_last(app: &AppHandle, win: &str) {
    let id = registry()
        .lock()
        .unwrap()
        .get(win)
        .and_then(|s| s.tabs.last().map(|t| t.id));
    if let Some(id) = id {
        select_tab(app, win, id);
    }
}

/// Close tab `id` in `win`. Closing the last tab destroys the window when others
/// remain; for the only window it resets to a fresh tab and hides it (never quits).
pub fn close_tab(app: &AppHandle, win: &str, id: u32) {
    let Some(window) = app.get_window(win) else {
        return;
    };
    let is_last_tab = registry()
        .lock()
        .unwrap()
        .get(win)
        .map(|s| s.tabs.len() <= 1)
        .unwrap_or(true);
    let last_window = window_count(app) <= 1;

    if is_last_tab {
        // Pinned windows resist closing — closing the last tab would close/hide the
        // window, so it's a no-op while pinned.
        if is_pinned(win) {
            return;
        }
        if !last_window {
            // Other windows remain → destroy this whole window + its single tab.
            close_window(app, &window);
            return;
        }
        // The only window → never quit, never destroy it. Open a fresh tab so it
        // isn't left empty; the old tab is closed below and the window hidden
        // (Esc/⌘Q-style — the tray / global hotkey re-shows it with the new tab).
        new_tab(app, win);
    }

    let remaining = {
        let mut reg = registry().lock().unwrap();
        reg.get_mut(win).and_then(|s| s.close(id))
    };
    let label = format!("{win}_tab_{id}");
    if let Some(v) = app.get_webview(&label) {
        let _ = v.close();
    }
    session::forget_tab(&label);
    lru_remove(&label);
    if remaining.is_some() {
        // The closed tab may have been active; its replacement could be slept —
        // wake it so we never relayout onto a blank active tab.
        ensure_active_live(app, &window, win);
        relayout(&window);
        emit_tabs(app, win);
    }

    if is_last_tab && last_window {
        let _ = window.hide();
    }
}

/// Close the active tab of `win` (⌘W); last-tab behavior matches `close_tab`.
pub fn close_active(app: &AppHandle, win: &str) {
    let active = registry()
        .lock()
        .unwrap()
        .get(win)
        .map(|s| s.active)
        .unwrap_or(0);
    if active != 0 {
        close_tab(app, win, active);
    }
}

/// Close an entire window: destroy it when other custom windows remain, else
/// hide it (so the app keeps a re-showable window and isn't left empty). A
/// hidden window is re-shown by the tray / global toggle; a destroyed one is
/// gone. Pinned windows resist closing.
fn close_window(app: &AppHandle, window: &tauri::Window) {
    let win = window.label().to_string();
    // Pinned windows resist closing (the × dot is disabled while pinned).
    if is_pinned(&win) {
        return;
    }
    // The last remaining window is HIDDEN, never destroyed — the app quits only
    // via the tray menu, so one re-showable window always stays alive (the tray /
    // global toggle brings it back; see `toggle`). The × button destroys any
    // window while OTHERS remain, taking all its tabs with it.
    if window_count(app) <= 1 {
        crate::log::line(app, &format!("close_window {win} (hide — last window)"));
        let _ = window.hide();
        return;
    }
    crate::log::line(app, &format!("close_window {win} (destroy)"));
    registry().lock().unwrap().remove(&win);
    session::forget_window(&win);
    lru_remove_window(&win);
    let _ = window.destroy();
}

/// Rename tab `id` in `win` (trimmed + capped to 15 chars).
pub fn rename_tab(app: &AppHandle, win: &str, id: u32, title: String) {
    let title = cap_title(&title);
    if title.is_empty() {
        return;
    }
    if let Some(state) = registry().lock().unwrap().get_mut(win) {
        state.rename(id, title);
    }
    emit_tabs(app, win);
}

/// Trim and cap a user-entered tab title to 15 Unicode characters.
fn cap_title(s: &str) -> String {
    s.trim().chars().take(15).collect()
}

/// Switch to the next (`forward`) or previous tab in `win`.
pub fn cycle_tab(app: &AppHandle, win: &str, forward: bool) {
    let changed = {
        let mut reg = registry().lock().unwrap();
        reg.get_mut(win).map(|s| {
            let prev = s.active;
            let new = if forward { s.next() } else { s.prev() };
            (prev, new)
        })
    };
    if let Some((prev, new)) = changed {
        if new != 0 {
            if let Some(window) = app.get_window(win) {
                if sleep_enabled(app) {
                    manage_sleep(app, &window, win, prev, new);
                }
                ensure_active_live(app, &window, win);
                relayout(&window);
                emit_tabs(app, win);
            }
        }
    }
}

/// Label of the currently focused custom tab window (`win_*`), if any.
pub fn focused_window(app: &AppHandle) -> Option<String> {
    let wins: Vec<(String, tauri::Window)> = app
        .windows()
        .into_iter()
        .filter(|(l, _)| l.starts_with("win_"))
        .collect();
    // Prefer the key window. Frameless / always-on-top windows can report
    // `isKeyWindow` unreliably, so fall back to a visible window, then any —
    // otherwise every in-app hotkey would silently no-op.
    wins.iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .or_else(|| wins.iter().find(|(_, w)| w.is_visible().unwrap_or(false)))
        .or_else(|| wins.first())
        .map(|(l, _)| l.clone())
}

/// Eval JS in the active tab's webview of the focused custom window. Returns
/// true if a target webview was found (used for speak/mic on the active tab).
pub fn eval_active(app: &AppHandle, js: &str) -> bool {
    let Some(win) = focused_window(app) else {
        return false;
    };
    let active = registry()
        .lock()
        .unwrap()
        .get(&win)
        .map(|s| s.active)
        .unwrap_or(0);
    if active == 0 {
        return false;
    }
    match app.get_webview(&format!("{win}_tab_{active}")) {
        Some(v) => {
            let _ = v.eval(js);
            true
        }
        None => false,
    }
}

/// Record copy-history natively (IPC-independent): read the active tab's source
/// text via an eval callback, then pair it with the just-copied clipboard text.
/// Used by ⌘C so history works even though the remote webview can't invoke.
pub fn record_copy_history(app: &AppHandle) {
    let Some(win) = focused_window(app) else {
        return;
    };
    let active = registry()
        .lock()
        .unwrap()
        .get(&win)
        .map(|s| s.active)
        .unwrap_or(0);
    if active == 0 {
        return;
    }
    let Some(v) = app.get_webview(&format!("{win}_tab_{active}")) else {
        return;
    };
    let app2 = app.clone();
    // Return the source text + the active from/to language codes, read from
    // Google's selected language tabs (the resolved code — auto-detect gives the
    // detected code, not "auto"); first selected tab = source, second = target.
    // Falls back to the sl/tl URL params if the tabs aren't present.
    // (Keep this selector in sync with injection.rs lbLangs.)
    let js = r#"(function(){var t=document.querySelector('textarea[aria-label="Source text"]')||document.querySelector('textarea');var tabs=document.querySelectorAll('[role="tab"][aria-selected="true"][data-language-code]:not([data-language-code=""])');var p=new URLSearchParams(location.search);var sl=(tabs[0]&&tabs[0].getAttribute('data-language-code'))||p.get('sl')||'';var tl=(tabs[1]&&tabs[1].getAttribute('data-language-code'))||p.get('tl')||'';return JSON.stringify({source:t?t.value:'',sl:sl,tl:tl});})()"#;
    let _ = v.eval_with_callback(js, move |result| {
        // The eval result is the JSON-encoded JS return value (our JSON string),
        // so unwrap the outer string then parse the inner object.
        let inner = serde_json::from_str::<String>(&result).unwrap_or(result);
        #[derive(serde::Deserialize)]
        struct CopyInfo {
            source: String,
            #[serde(default)]
            sl: String,
            #[serde(default)]
            tl: String,
        }
        let info = serde_json::from_str::<CopyInfo>(&inner).unwrap_or(CopyInfo {
            source: inner,
            sl: String::new(),
            tl: String::new(),
        });
        let app3 = app2.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let translation = platform::read_clipboard().unwrap_or_default();
            crate::log::line(
                &app3,
                &format!(
                    "native history: src_len={} tr_len={} {}->{}",
                    info.source.len(),
                    translation.len(),
                    info.sl,
                    info.tl
                ),
            );
            crate::history::add(&app3, info.source, translation, info.sl, info.tl);
        });
    });
}

// ---- internals -------------------------------------------------------------

/// Parse `win_N_tab_M` into (`win_N`, M).
fn parse_tab_label(label: &str) -> Option<(String, u32)> {
    let (win, id) = label.rsplit_once("_tab_")?;
    Some((win.to_string(), id.parse().ok()?))
}

/// Store a (possibly updated) URL onto a tab's state, keyed by webview label.
fn set_tab_url(label: &str, url: &str) {
    if let Some((win, id)) = parse_tab_label(label) {
        if let Some(t) = registry()
            .lock()
            .unwrap()
            .get_mut(&win)
            .and_then(|s| s.tabs.iter_mut().find(|t| t.id == id))
        {
            t.url = url.to_string();
        }
    }
}

/// Create a tab webview from its stored url + pending text. Used by both
/// `add_tab` and waking a slept tab. Returns false if creation failed.
fn spawn_tab_webview(app: &AppHandle, window: &tauri::Window, win: &str, id: u32) -> bool {
    let label = format!("{win}_tab_{id}");
    let (url, pending) = {
        let reg = registry().lock().unwrap();
        match reg.get(win).and_then(|s| s.tabs.iter().find(|t| t.id == id)) {
            Some(t) => (t.url.clone(), t.pending_text.clone()),
            None => return false,
        }
    };
    let Ok(parsed) = url.parse() else {
        crate::log::line(app, &format!("spawn_tab_webview: invalid url {url:?} for {label}"));
        return false;
    };
    let session_on = session::is_enabled(app);
    let (cw, ch) = content_size(window);
    let scale = saved_zoom_scale(app);
    // Native pageZoom (the browser's own zoom): set on creation + page-load so the
    // first contentful paint is already zoomed (no 100%→zoom flash).
    let tab_wb = WebviewBuilder::new(&label, WebviewUrl::External(parsed))
        .accept_first_mouse(true)
        .on_page_load(move |wv, payload| {
            if matches!(
                payload.event(),
                tauri::webview::PageLoadEvent::Started | tauri::webview::PageLoadEvent::Finished
            ) {
                let _ = wv.set_zoom(scale);
            }
        })
        .initialization_script(injection::build(&label, session_on, pending.as_deref()));
    match window.add_child(
        tab_wb,
        LogicalPosition::new(0.0, chrome_h(app)),
        LogicalSize::new(cw, ch),
    ) {
        Ok(webview) => {
            let _ = webview.set_zoom(scale);
            if pending.is_some() {
                if let Some(t) = registry()
                    .lock()
                    .unwrap()
                    .get_mut(win)
                    .and_then(|s| s.tabs.iter_mut().find(|t| t.id == id))
                {
                    t.pending_text = None;
                }
            }
            true
        }
        Err(_) => {
            crate::log::line(app, &format!("spawn_tab_webview: creation failed for {label}"));
            false
        }
    }
}

// ---- inactive-tab sleeping (opt-in `sleep_idle_tabs`) ----------------------

/// Whether the "sleep inactive tabs to save memory" pref is on (default off).
fn sleep_enabled(app: &AppHandle) -> bool {
    app.store("settings.json")
        .ok()
        .and_then(|s| s.get("sleep_idle_tabs").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// A tab became active (visible): it leaves the inactive cache.
fn lru_mark_active(label: &str) {
    inactive_lru().lock().unwrap().retain(|l| l != label);
}

/// A tab became inactive: move it to the front of the cache (most recent).
fn lru_mark_inactive(label: &str) {
    let mut lru = inactive_lru().lock().unwrap();
    lru.retain(|l| l != label);
    lru.insert(0, label.to_string());
}

/// Drop a tab from the cache (it was closed).
fn lru_remove(label: &str) {
    inactive_lru().lock().unwrap().retain(|l| l != label);
}

/// Drop all of a window's tabs from the cache (the window was destroyed).
fn lru_remove_window(win: &str) {
    let prefix = format!("{win}_tab_");
    inactive_lru()
        .lock()
        .unwrap()
        .retain(|l| !l.starts_with(&prefix));
}

/// Pop the labels beyond `SLEEP_CACHE` off the back of the cache (least recent).
fn lru_evictions() -> Vec<String> {
    let mut lru = inactive_lru().lock().unwrap();
    let mut out = Vec::new();
    while lru.len() > SLEEP_CACHE {
        if let Some(l) = lru.pop() {
            out.push(l);
        }
    }
    out
}

/// Sleep a tab: capture its live URL (so a later wake restores the translation),
/// then close its webview to free the RAM. The TabState entry stays — the pill
/// remains and activating it wakes the tab.
fn sleep_tab(app: &AppHandle, label: &str) {
    let Some(v) = app.get_webview(label) else {
        return;
    };
    let app2 = app.clone();
    let label2 = label.to_string();
    let _ = v.eval_with_callback("location.href", move |result| {
        let url = serde_json::from_str::<String>(&result).unwrap_or(result);
        if url.starts_with("http") {
            set_tab_url(&label2, &url);
        }
        // If the tab was re-selected before this async eviction callback ran,
        // it's the active tab again — keep it live (don't close the one we just
        // revived).
        if label_is_active(&label2) {
            return;
        }
        if let Some(wv) = app2.get_webview(&label2) {
            crate::log::line(&app2, &format!("sleep {label2}"));
            let _ = wv.close();
        }
    });
}

/// Apply the keep-live policy after `prev`→`new` became active in `win` (only
/// when the sleep pref is on): wake `new` if it was slept, keep the active tab
/// out of the cache, push the just-deactivated tab into the cache, and sleep
/// anything past `SLEEP_CACHE`.
fn manage_sleep(app: &AppHandle, window: &tauri::Window, win: &str, prev: u32, new: u32) {
    if new != 0 {
        let active_label = format!("{win}_tab_{new}");
        if app.get_webview(&active_label).is_none() {
            crate::log::line(app, &format!("wake {active_label}"));
            spawn_tab_webview(app, window, win, new);
        }
        lru_mark_active(&active_label);
    }
    if prev != 0 && prev != new {
        lru_mark_inactive(&format!("{win}_tab_{prev}"));
    }
    for label in lru_evictions() {
        sleep_tab(app, &label);
    }
}

/// Invariant enforced on every activation/close, INDEPENDENT of the sleep pref:
/// a window's active tab always has a live webview. Wakes it (from its stored
/// url) if it was slept — so toggling sleep off, or closing onto a slept
/// neighbour, never leaves a blank active tab.
fn ensure_active_live(app: &AppHandle, window: &tauri::Window, win: &str) {
    let active = registry()
        .lock()
        .unwrap()
        .get(win)
        .map(|s| s.active)
        .unwrap_or(0);
    if active == 0 {
        return;
    }
    let label = format!("{win}_tab_{active}");
    if app.get_webview(&label).is_none() {
        crate::log::line(app, &format!("wake {label}"));
        spawn_tab_webview(app, window, win, active);
        lru_mark_active(&label);
    }
}

/// Is `label` (`win_N_tab_M`) currently its window's active tab?
fn label_is_active(label: &str) -> bool {
    if let Some((win, id_str)) = label.rsplit_once("_tab_") {
        if let Ok(id) = id_str.parse::<u32>() {
            return registry()
                .lock()
                .unwrap()
                .get(win)
                .map(|s| s.active == id)
                .unwrap_or(false);
        }
    }
    false
}

fn add_tab(
    app: &AppHandle,
    window: &tauri::Window,
    restore_text: Option<&str>,
    url_override: Option<&str>,
) {
    let win = window.label().to_string();
    // Per-window and total tab caps — silently no-op past the limit.
    let win_tabs = registry()
        .lock()
        .unwrap()
        .get(&win)
        .map(|s| s.tabs.len())
        .unwrap_or(0);
    if win_tabs >= MAX_TABS_PER_WINDOW || total_tab_count() >= MAX_TOTAL_TABS {
        crate::log::line(app, &format!("limit reached: not adding a tab to {win}"));
        return;
    }
    let url = url_override
        .map(str::to_string)
        .unwrap_or_else(|| current_url(app));

    let id = {
        let mut reg = registry().lock().unwrap();
        let state = reg.entry(win.clone()).or_default();
        let title = format!("Tab {}", state.tabs.len() + 1);
        state.add(title, url, restore_text.map(str::to_string))
    };

    // `spawn_tab_webview` reads the url + pending text back from state and creates
    // the webview; on failure (e.g. a bad URL) roll the tab back so the bar never
    // shows a pill with no webview behind it.
    if !spawn_tab_webview(app, window, &win, id) {
        if let Some(state) = registry().lock().unwrap().get_mut(&win) {
            let _ = state.close(id);
        }
        lru_remove(&format!("{win}_tab_{id}"));
    }

    relayout(window);
    emit_tabs(app, &win);
}

/// Lay out the chrome strip (full width, fixed height) and show the active
/// tab's webview (filling the rest) while hiding the others.
fn relayout(window: &tauri::Window) {
    let ch_h = chrome_h(window.app_handle());
    let win = window.label().to_string();
    let active = registry()
        .lock()
        .unwrap()
        .get(&win)
        .map(|s| s.active)
        .unwrap_or(0);
    let chrome = format!("{win}_chrome");
    let active_label = format!("{win}_tab_{active}");
    let tab_prefix = format!("{win}_tab_");
    let (cw, ch) = content_size(window);

    for v in window.webviews() {
        let l = v.label();
        if l == chrome {
            let _ = v.set_position(LogicalPosition::new(0.0, 0.0));
            let _ = v.set_size(LogicalSize::new(cw, ch_h));
        } else if l == active_label {
            let _ = v.set_position(LogicalPosition::new(0.0, ch_h));
            let _ = v.set_size(LogicalSize::new(cw, ch));
            let _ = v.show();
        } else if l.starts_with(&tab_prefix) {
            let _ = v.hide();
        }
    }
}

/// Logical (width, height-below-chrome) of the window's content area.
fn content_size(window: &tauri::Window) -> (f64, f64) {
    let sf = window.scale_factor().unwrap_or(1.0);
    let (w, h) = window
        .inner_size()
        .map(|s| (s.width as f64 / sf, s.height as f64 / sf))
        .unwrap_or((DEFAULT_W, DEFAULT_H));
    (w, (h - chrome_h(window.app_handle())).max(0.0))
}

/// Build the translate URL from the currently saved language pair.
fn current_url(app: &AppHandle) -> String {
    config::build_translate_url(
        &read_lang(app, "lang_from", "uz"),
        &read_lang(app, "lang_to", "en"),
    )
}

fn read_lang(app: &AppHandle, key: &str, default: &str) -> String {
    app.store("settings.json")
        .ok()
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_str().map(|x| x.to_string()))
        .unwrap_or_else(|| default.to_string())
}

/// Saved window opacity percent (30–100; default 100).
fn saved_opacity(app: &AppHandle) -> i32 {
    app.store("settings.json")
        .ok()
        .and_then(|s| s.get("opacity").and_then(|v| v.as_i64()))
        .map(|v| (v as i32).clamp(30, 100))
        .unwrap_or(100)
}

/// Saved zoom as a scale factor (default 1.0).
fn saved_zoom_scale(app: &AppHandle) -> f64 {
    app.store("settings.json")
        .ok()
        .and_then(|s| s.get("zoom_level").and_then(|v| v.as_i64()))
        .unwrap_or(100) as f64
        / 100.0
}

/// Dampened chrome scale derived from the content zoom (1.0 at 100%): the strip
/// follows the zoom only partway, so the bar stays usable at extreme zooms.
fn chrome_scale(app: &AppHandle) -> f64 {
    1.0 + (saved_zoom_scale(app) - 1.0) * CHROME_ZOOM_DAMPING
}

/// Chrome strip height for the current zoom, logical px.
fn chrome_h(app: &AppHandle) -> f64 {
    BASE_CHROME_H * chrome_scale(app)
}

/// Last window size the user left, in logical px (falls back to the defaults).
fn saved_size(app: &AppHandle) -> (f64, f64) {
    let store = app.store("settings.json").ok();
    let read = |key: &str, default: f64| {
        store
            .as_ref()
            .and_then(|s| s.get(key).and_then(|v| v.as_f64()))
            .filter(|v| *v > 0.0)
            .unwrap_or(default)
    };
    (read("last_win_w", DEFAULT_W), read("last_win_h", DEFAULT_H))
}

/// Push the current tab list + active id to the window's chrome webview.
fn emit_tabs(app: &AppHandle, win: &str) {
    let payload = {
        let reg = registry().lock().unwrap();
        let Some(state) = reg.get(win) else {
            return;
        };
        snapshot(win, state)
    };
    let _ = app.emit_to(
        EventTarget::webview(format!("{win}_chrome")),
        "tabs_changed",
        payload,
    );
}

fn snapshot(win: &str, state: &TabState) -> TabsPayload {
    TabsPayload {
        win: win.to_string(),
        tabs: state
            .tabs
            .iter()
            .map(|t| TabInfo {
                id: t.id,
                title: t.title.clone(),
            })
            .collect(),
        active: state.active,
        pinned: state.pinned,
    }
}

// ---- IPC commands (called from the chrome webview) -------------------------

#[tauri::command]
pub fn tab_new(webview: tauri::Webview) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    new_tab(&app, webview.window().label());
}

#[tauri::command]
pub fn tab_select(webview: tauri::Webview, id: u32) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    select_tab(&app, webview.window().label(), id);
}

#[tauri::command]
pub fn tab_close(webview: tauri::Webview, id: u32) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    close_tab(&app, webview.window().label(), id);
}

#[tauri::command]
pub fn tab_rename(webview: tauri::Webview, id: u32, title: String) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    rename_tab(&app, webview.window().label(), id, title);
}

#[tauri::command]
pub fn tab_list(webview: tauri::Webview) -> TabsPayload {
    let win = webview.window().label().to_string();
    if is_remote_tab(webview.label()) {
        return TabsPayload {
            win,
            tabs: Vec::new(),
            active: 0,
            pinned: false,
        };
    }
    let reg = registry().lock().unwrap();
    match reg.get(&win) {
        Some(state) => snapshot(&win, state),
        None => TabsPayload {
            win,
            tabs: Vec::new(),
            active: 0,
            pinned: false,
        },
    }
}

// ---- window controls (called from the chrome webview) ----------------------

#[tauri::command]
pub fn window_new(webview: tauri::Webview) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    let _ = new_window(&app);
}

#[tauri::command]
pub fn window_close(webview: tauri::Webview) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    let win = webview.window().label().to_string();
    if let Some(window) = app.get_window(&win) {
        close_window(&app, &window);
    }
}

#[tauri::command]
pub fn window_pin(webview: tauri::Webview) {
    if is_remote_tab(webview.label()) {
        return;
    }
    let app = webview.app_handle().clone();
    let win = webview.window().label().to_string();
    if let Some(state) = registry().lock().unwrap().get_mut(&win) {
        state.pinned = !state.pinned;
    }
    emit_tabs(&app, &win);
}

/// Is the custom window `win` pinned?
pub fn is_pinned(win: &str) -> bool {
    registry()
        .lock()
        .unwrap()
        .get(win)
        .map(|s| s.pinned)
        .unwrap_or(false)
}

// ---- tray / menu helpers (operate on all `win_*` windows) ------------------

/// Labels of all live custom windows.
fn all_windows(app: &AppHandle) -> Vec<String> {
    app.windows()
        .into_keys()
        .filter(|l| l.starts_with("win_"))
        .collect()
}

/// Is `label` a tab webview (`win_*_tab_*`), not a chrome webview?
fn is_tab_webview(label: &str) -> bool {
    label.starts_with("win_") && label.contains("_tab_")
}

/// Tray toggle: create a window if none exist; else hide all if any are
/// visible, otherwise show + focus them all.
pub fn toggle(app: &AppHandle) {
    let wins = all_windows(app);
    if wins.is_empty() {
        let _ = new_window(app);
        return;
    }
    // Pinned windows stay put: the toggle only hides/shows unpinned ones, and
    // decides hide-vs-show from whether any *unpinned* window is visible.
    let any_visible = wins
        .iter()
        .filter(|l| !is_pinned(l))
        .filter_map(|l| app.get_window(l))
        .any(|w| w.is_visible().unwrap_or(false));
    if any_visible {
        for l in &wins {
            if is_pinned(l) {
                continue;
            }
            if let Some(w) = app.get_window(l) {
                let _ = w.hide();
            }
        }
    } else {
        for l in &wins {
            if let Some(w) = app.get_window(l) {
                let _ = w.show();
            }
        }
        if let Some(w) = wins.first().and_then(|l| app.get_window(l)) {
            let _ = w.set_focus();
        }
    }
}

/// Focus the next VISIBLE app window (⌘`) — translator (`win_*`) windows plus
/// the open auxiliary windows (Preferences / History / Tail-logs / About). ⌘Tab is the macOS system app-switcher, so ⌘` is the in-app one.
pub fn cycle_windows(app: &AppHandle) {
    const AUX: [&str; 4] = ["prefs", "about", "logview", "history"];
    let mut wins: Vec<String> = app
        .windows()
        .into_iter()
        .filter(|(l, w)| {
            (l.starts_with("win_") || AUX.contains(&l.as_str())) && w.is_visible().unwrap_or(false)
        })
        .map(|(l, _)| l)
        .collect();
    wins.sort();
    if wins.len() < 2 {
        return;
    }
    // The actually-focused window (any kind), so cycling starts from where we are.
    let focused = app
        .windows()
        .into_iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .map(|(l, _)| l);
    let idx = focused
        .and_then(|f| wins.iter().position(|w| *w == f))
        .unwrap_or(0);
    let next = &wins[(idx + 1) % wins.len()];
    if let Some(w) = app.get_window(next) {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Apply a zoom scale to every open tab webview (live change from Preferences) —
/// native pageZoom, matching how new tabs are zoomed.
pub fn set_zoom_all(app: &AppHandle, scale: f64) {
    for (label, webview) in app.webviews() {
        if is_tab_webview(&label) {
            let _ = webview.set_zoom(scale);
        }
    }
    // The chrome strip follows the zoom (dampened): re-zoom each chrome webview's
    // content and relayout every window so the strip height tracks it live.
    let cs = 1.0 + (scale - 1.0) * CHROME_ZOOM_DAMPING;
    for (label, webview) in app.webviews() {
        if label.starts_with("win_") && label.ends_with("_chrome") {
            let _ = webview.eval(format!("document.documentElement.style.zoom={cs};"));
        }
    }
    for l in all_windows(app) {
        if let Some(w) = app.get_window(&l) {
            relayout(&w);
        }
    }
}

/// Apply window opacity (percent) to every custom window.
pub fn set_opacity_all(app: &AppHandle, percent: i32) {
    for l in all_windows(app) {
        if let Some(w) = app.get_window(&l) {
            platform::set_window_opacity(&w, percent);
        }
    }
}

/// Resize every translator window to (w, h) and persist as the default size
/// (used by new windows + restore). Clamped to the window minimum (252x155).
pub fn set_size_all(app: &AppHandle, w: f64, h: f64) {
    let w = w.max(252.0);
    let h = h.max(155.0);
    for l in all_windows(app) {
        if let Some(window) = app.get_window(&l) {
            let _ = window.set_size(LogicalSize::new(w, h));
        }
    }
    if let Ok(store) = app.store("settings.json") {
        store.set("last_win_w", w);
        store.set("last_win_h", h);
        let _ = store.save();
    }
}

/// Reset every custom window to the default size.
pub fn reset_size_all(app: &AppHandle) {
    for l in all_windows(app) {
        if let Some(w) = app.get_window(&l) {
            let _ = w.set_size(LogicalSize::new(DEFAULT_W, DEFAULT_H));
        }
    }
}

// ---- session snapshot / restore -------------------------------------------

/// Record a window's outer position + inner (logical) size for session restore.
fn save_geometry(window: &tauri::Window) {
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
        let sf = window.scale_factor().unwrap_or(1.0);
        let lw = (size.width as f64 / sf).round();
        let lh = (size.height as f64 / sf).round();
        // Per-frame in-memory capture; the session writer debounces the disk write.
        session::update_geometry(window.label(), pos.x, pos.y, lw as u32, lh as u32);
        // Remember the size so the next new window opens at it — but throttle the
        // synchronous disk write so a drag/resize doesn't hammer settings.json.
        let now = now_ms();
        if now.saturating_sub(LAST_SIZE_SAVE_MS.load(Ordering::Relaxed)) >= SIZE_SAVE_THROTTLE_MS {
            LAST_SIZE_SAVE_MS.store(now, Ordering::Relaxed);
            if let Ok(store) = window.app_handle().store("settings.json") {
                store.set("last_win_w", serde_json::json!(lw));
                store.set("last_win_h", serde_json::json!(lh));
                let _ = store.save();
            }
        }
    }
}

/// A tab's persistable identity (its webview label + current title).
pub struct LayoutTab {
    pub label: String,
    pub title: String,
}

/// A window's live layout for session persistence.
pub struct LayoutWindow {
    pub win: String,
    pub pinned: bool,
    pub active_index: usize,
    pub tabs: Vec<LayoutTab>,
}

/// Snapshot the live window/tab structure (order, titles, active, pin) for the
/// session writer. Per-tab text + window geometry are merged in by `session`.
pub fn session_layout() -> Vec<LayoutWindow> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .map(|(win, state)| {
            let active_index = state
                .tabs
                .iter()
                .position(|t| t.id == state.active)
                .unwrap_or(0);
            let tabs = state
                .tabs
                .iter()
                .map(|t| LayoutTab {
                    label: format!("{win}_tab_{}", t.id),
                    title: t.title.clone(),
                })
                .collect();
            LayoutWindow {
                win: win.clone(),
                pinned: state.pinned,
                active_index,
                tabs,
            }
        })
        .collect()
}

/// Rebuild windows + tabs from a saved session (geometry, pin, titles, active);
/// each tab's text is passed as restore text so the page pastes + re-translates.
pub fn restore_session(app: &AppHandle, windows: Vec<session::SavedWindow>) {
    let sleep = sleep_enabled(app);
    let mut built = 0usize;
    // In sleep mode, only each window's active tab is created at restore; collect
    // them so their webviews can be spawned staggered (not all at once).
    let mut staggered: Vec<(String, u32)> = Vec::new();
    for sw in windows {
        if sw.tabs.is_empty() {
            continue;
        }
        if built >= MAX_WINDOWS {
            break;
        }
        let Ok(window) = build_window(app) else {
            continue;
        };
        let _ = window.set_position(PhysicalPosition::new(sw.x, sw.y));
        let _ = window.set_size(LogicalSize::new(sw.w as f64, sw.h as f64));
        let win = window.label().to_string();

        if !sleep {
            // ---- all-live restore (sleep off): create every tab's webview ----
            for tab in &sw.tabs {
                let text = (!tab.text.is_empty()).then_some(tab.text.as_str());
                add_tab(app, &window, text, None);
            }
            // If every add_tab no-op'd against the 30-tab cap, destroy the empty
            // shell instead of leaving a ghost window (chrome strip, no tab).
            let has_tabs = registry()
                .lock()
                .unwrap()
                .get(&win)
                .map(|s| !s.tabs.is_empty())
                .unwrap_or(false);
            if !has_tabs {
                registry().lock().unwrap().remove(&win);
                session::forget_window(&win);
                let _ = window.destroy();
                continue;
            }
            built += 1;
            let mut seeds: Vec<(String, String)> = Vec::new();
            {
                let mut reg = registry().lock().unwrap();
                if let Some(state) = reg.get_mut(&win) {
                    for (i, t) in state.tabs.iter_mut().enumerate() {
                        if let Some(saved) = sw.tabs.get(i) {
                            t.title = saved.title.clone();
                            if !saved.text.is_empty() {
                                seeds.push((format!("{win}_tab_{}", t.id), saved.text.clone()));
                            }
                        }
                    }
                    state.pinned = sw.pinned;
                    let idx = sw.active.min(state.tabs.len().saturating_sub(1));
                    if let Some(t) = state.tabs.get(idx) {
                        state.active = t.id;
                    }
                }
            }
            // Seed captured text so a quick re-quit (before the paste fires its own
            // capture) doesn't drop the restored text.
            for (label, text) in seeds {
                session::seed_text(&label, &text);
            }
        } else {
            // ---- sleep mode: populate state for ALL tabs (pending text kept),
            // but create only the active tab's webview; the rest start slept. ----
            let base = current_url(app);
            let mut seeds: Vec<(String, String)> = Vec::new();
            let mut active_id = 0u32;
            {
                let mut reg = registry().lock().unwrap();
                let used: usize = reg.values().map(|s| s.tabs.len()).sum();
                let mut budget = MAX_TOTAL_TABS.saturating_sub(used);
                let state = reg.entry(win.clone()).or_default();
                for tab in &sw.tabs {
                    if state.tabs.len() >= MAX_TABS_PER_WINDOW || budget == 0 {
                        break;
                    }
                    let pending = (!tab.text.is_empty()).then(|| tab.text.clone());
                    let id = state.add(tab.title.clone(), base.clone(), pending);
                    if !tab.text.is_empty() {
                        seeds.push((format!("{win}_tab_{id}"), tab.text.clone()));
                    }
                    budget -= 1;
                }
                state.pinned = sw.pinned;
                let idx = sw.active.min(state.tabs.len().saturating_sub(1));
                if let Some(t) = state.tabs.get(idx) {
                    state.active = t.id;
                    active_id = t.id;
                }
            }
            if active_id == 0 {
                registry().lock().unwrap().remove(&win);
                session::forget_window(&win);
                let _ = window.destroy();
                continue;
            }
            built += 1;
            for (label, text) in seeds {
                session::seed_text(&label, &text);
            }
            staggered.push((win.clone(), active_id));
        }

        relayout(&window);
        emit_tabs(app, &win);
        let _ = window.show();
        let _ = window.set_focus();
        save_geometry(&window);
    }

    // Sleep mode: spawn the per-window active-tab webviews one at a time so a
    // multi-window restore doesn't load them all simultaneously.
    if !staggered.is_empty() {
        let app = app.clone();
        std::thread::spawn(move || {
            for (win, id) in staggered {
                let app2 = app.clone();
                let win2 = win.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(w) = app2.get_window(&win2) {
                        spawn_tab_webview(&app2, &w, &win2, id);
                        relayout(&w);
                    }
                });
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });
    }
}

// Pure tab-state model (no Tauri types) so it's unit-testable in isolation;
// the per-tab webview label is `format!("{win}_tab_{id}")`.

/// One tab: a stable id and a user-facing title.
struct Tab {
    id: u32,
    title: String,
    /// URL to (re)load this tab's webview — the configured translate URL, or a
    /// history/selection URL; updated to the live `location.href` before sleeping
    /// so a wake restores the exact text + languages.
    url: String,
    /// Text to paste into the source box on first load (selection / history /
    /// session restore); cleared once the webview is created and consumes it.
    pending_text: Option<String>,
}

/// Per-window tab list with an active selection. Ids start at 1 (0 = "none").
#[derive(Default)]
struct TabState {
    tabs: Vec<Tab>,
    active: u32,
    next_id: u32,
    pinned: bool,
}

impl TabState {
    /// Append a tab, make it active, and return its new id.
    fn add(&mut self, title: String, url: String, pending_text: Option<String>) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        self.tabs.push(Tab {
            id,
            title,
            url,
            pending_text,
        });
        self.active = id;
        id
    }

    /// Make `id` the active tab if it exists.
    fn select(&mut self, id: u32) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active = id;
        }
    }

    /// Remove `id` (no-op if absent). If it was active, the neighbour (the tab
    /// that slid into its slot, else the new last tab) becomes active. Returns
    /// the new active id, or `None` only when no tabs remain — so a missing id
    /// never spuriously signals an empty window.
    fn close(&mut self, id: u32) -> Option<u32> {
        if let Some(idx) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(idx);
            if self.active == id && !self.tabs.is_empty() {
                let new_idx = idx.min(self.tabs.len() - 1);
                self.active = self.tabs[new_idx].id;
            }
        }
        if self.tabs.is_empty() {
            self.active = 0;
            None
        } else {
            Some(self.active)
        }
    }

    /// Cycle to the next tab (wrapping) and return its id (0 if empty).
    fn next(&mut self) -> u32 {
        self.step(1)
    }

    /// Cycle to the previous tab (wrapping) and return its id (0 if empty).
    fn prev(&mut self) -> u32 {
        self.step(-1)
    }

    fn step(&mut self, delta: i32) -> u32 {
        if self.tabs.is_empty() {
            return 0;
        }
        let n = self.tabs.len() as i32;
        let cur = self
            .tabs
            .iter()
            .position(|t| t.id == self.active)
            .unwrap_or(0) as i32;
        let ni = (((cur + delta) % n) + n) % n;
        self.active = self.tabs[ni as usize].id;
        self.active
    }

    /// Rename the tab with `id` (no-op if absent).
    fn rename(&mut self, id: u32, title: String) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.title = title;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TabState;

    #[test]
    fn add_makes_active_and_unique() {
        let mut s = TabState::default();
        let a = s.add("Tab 1".into(), String::new(), None);
        let b = s.add("Tab 2".into(), String::new(), None);
        assert_ne!(a, b);
        assert_eq!(s.active, b);
    }

    #[test]
    fn close_active_falls_back_to_neighbor() {
        let mut s = TabState::default();
        let a = s.add("1".into(), String::new(), None);
        let b = s.add("2".into(), String::new(), None);
        let new_active = s.close(b);
        assert_eq!(new_active, Some(a));
        assert_eq!(s.active, a);
    }

    #[test]
    fn close_last_returns_none() {
        let mut s = TabState::default();
        let a = s.add("1".into(), String::new(), None);
        assert_eq!(s.close(a), None);
        assert!(s.tabs.is_empty());
    }

    #[test]
    fn close_missing_is_noop() {
        let mut s = TabState::default();
        let a = s.add("1".into(), String::new(), None);
        assert_eq!(s.close(999), Some(a));
        assert_eq!(s.active, a);
    }

    #[test]
    fn next_and_prev_cycle() {
        let mut s = TabState::default();
        let a = s.add("1".into(), String::new(), None);
        let b = s.add("2".into(), String::new(), None);
        s.select(a);
        assert_eq!(s.next(), b);
        assert_eq!(s.next(), a); // wraps
        assert_eq!(s.prev(), b); // wraps back
    }

    #[test]
    fn rename_changes_title() {
        let mut s = TabState::default();
        let a = s.add("old".into(), String::new(), None);
        s.rename(a, "new".into());
        assert_eq!(s.tabs[0].title, "new");
    }
}
