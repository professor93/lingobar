//! Continuous session persistence (opt-in via the tray toggle).
//!
//! A debounced background writer snapshots the open windows — geometry, pin
//! state, active tab, and each tab's title + captured source text — to
//! `session.json`. On launch (when enabled) `restore()` rebuilds them via
//! `tabs::restore_session`. Text is captured per tab webview label; geometry
//! per window label.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;

/// A saved tab: its title and source text.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedTab {
    pub title: String,
    pub text: String,
}

/// A saved window: geometry, pin, active tab index, and its tabs in order.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedWindow {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub pinned: bool,
    pub active: usize,
    pub tabs: Vec<SavedTab>,
}

/// Captured per-tab source text, keyed by tab webview label.
static TEXTS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
/// Captured per-window geometry (outer x,y + inner logical w,h), keyed by window label.
static GEOMETRY: Mutex<BTreeMap<String, (i32, i32, u32, u32)>> = Mutex::new(BTreeMap::new());

static DIRTY: AtomicBool = AtomicBool::new(false);
static LAST_CHANGE_MS: AtomicU64 = AtomicU64::new(0);
static DIRTY_SINCE_MS: AtomicU64 = AtomicU64::new(0);
/// Serializes flushes (writer thread, signal handler, quit) so they never race
/// on the shared temp file.
static FLUSH_LOCK: Mutex<()> = Mutex::new(());

const DEBOUNCE_MS: u64 = 800;
const POLL_MS: u64 = 250;
/// Force a flush after this long even if the user is still typing.
const MAX_AGE_MS: u64 = 4000;

/// Whether session restore is enabled (tray toggle, default off).
pub fn is_enabled(app: &AppHandle) -> bool {
    app.store("settings.json")
        .ok()
        .and_then(|s| s.get("session_restore").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mark_dirty() {
    // Stamp the start of a dirty streak (for the max-age cap) on 0->1 transitions.
    if !DIRTY.swap(true, Ordering::SeqCst) {
        DIRTY_SINCE_MS.store(now_ms(), Ordering::SeqCst);
    }
    LAST_CHANGE_MS.store(now_ms(), Ordering::SeqCst);
}

/// Record a tab's current source text (keyed by its webview label).
pub fn update_text(label: &str, text: String) {
    TEXTS.lock().unwrap().insert(label.to_string(), text);
    mark_dirty();
}

/// Pre-populate a tab's text without waiting for a page event (used on restore).
pub fn seed_text(label: &str, text: &str) {
    if !text.is_empty() {
        TEXTS
            .lock()
            .unwrap()
            .insert(label.to_string(), text.to_string());
    }
}

/// Record a window's geometry for the next snapshot.
pub fn update_geometry(label: &str, x: i32, y: i32, w: u32, h: u32) {
    GEOMETRY
        .lock()
        .unwrap()
        .insert(label.to_string(), (x, y, w, h));
    mark_dirty();
}

/// Record a structural change (tab/window added or removed).
pub fn touch() {
    mark_dirty();
}

/// Drop a closed tab's captured text. Tab labels are monotonic (never reused),
/// so without this the map would grow for the whole session.
pub fn forget_tab(label: &str) {
    TEXTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(label);
}

/// Drop a destroyed window's geometry plus any of its tabs' captured text.
pub fn forget_window(win: &str) {
    GEOMETRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(win);
    let prefix = format!("{win}_tab_");
    TEXTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|k, _| !k.starts_with(&prefix));
}

fn session_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("session.json"))
}

/// Write the captured text map to disk now (atomic temp + rename). Safe from any
/// thread — it never touches live windows.
pub fn flush(app: &AppHandle) {
    if !is_enabled(app) {
        return;
    }
    // Serialize concurrent flushes so they don't race on the temp file.
    let _guard = FLUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Clear dirty before snapshotting: a change arriving mid-flush re-sets it.
    DIRTY.store(false, Ordering::SeqCst);
    let snapshot = build_snapshot();
    let Some(path) = session_path(app) else {
        return;
    };
    let Ok(json) = serde_json::to_string(&snapshot) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Assemble the full session snapshot from the live tab layout + captured text
/// and geometry. Reads the tab layout first (releasing the registry lock) before
/// locking the geometry/text maps.
fn build_snapshot() -> Vec<SavedWindow> {
    let layout = crate::tabs::session_layout();
    let geo = GEOMETRY.lock().unwrap();
    let texts = TEXTS.lock().unwrap();
    layout
        .into_iter()
        .filter(|w| !w.tabs.is_empty())
        .map(|w| {
            let (x, y, wd, ht) = geo.get(&w.win).copied().unwrap_or((120, 120, 560, 460));
            let tabs = w
                .tabs
                .iter()
                .map(|t| SavedTab {
                    title: t.title.clone(),
                    text: texts.get(&t.label).cloned().unwrap_or_default(),
                })
                .collect();
            SavedWindow {
                x,
                y,
                w: wd,
                h: ht,
                pinned: w.pinned,
                active: w.active_index,
                tabs,
            }
        })
        .collect()
}

/// Load the saved session snapshot, if any.
fn load(app: &AppHandle) -> Vec<SavedWindow> {
    let Some(path) = session_path(app) else {
        return Vec::new();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Rebuild the saved windows/tabs on launch (only when enabled).
pub fn restore(app: &AppHandle) {
    if !is_enabled(app) {
        return;
    }
    let snapshot = load(app);
    if !snapshot.is_empty() {
        crate::tabs::restore_session(app, snapshot);
    }
}

/// Spawn the debounced background writer.
pub fn start_writer(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        if DIRTY.load(Ordering::SeqCst) {
            let now = now_ms();
            let idle = now.saturating_sub(LAST_CHANGE_MS.load(Ordering::SeqCst)) >= DEBOUNCE_MS;
            let stale = now.saturating_sub(DIRTY_SINCE_MS.load(Ordering::SeqCst)) >= MAX_AGE_MS;
            if idle || stale {
                flush(&app);
            }
        }
    });
}
