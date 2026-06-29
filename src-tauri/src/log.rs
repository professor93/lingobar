//! App logging. Every `log::line` emits a `log_line` event so the Tail-logs
//! window can show events live — even when file-logging is off. The
//! `log_to_files` pref additionally appends timestamped lines to
//! `<app_data_dir>/lingobar.log` (capped at 5 MB).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_store::StoreExt;

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOCK: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Ring buffer of the last `RECENT_CAP` log lines, kept in memory so the
/// Tail-logs window can show recent events even when file-logging is off. Bounded
/// (oldest dropped) so it never grows — cheap, and safe for a long-running app.
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const RECENT_CAP: usize = 100;

/// Read the saved pref on startup, prime the enabled flag, and stamp a header.
pub fn init(app: &AppHandle) {
    let on = app
        .store("settings.json")
        .ok()
        .and_then(|s| s.get("log_to_files"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    set_enabled(on);
    line(
        app,
        &format!("=== LingoBar v{} started ===", app.package_info().version),
    );
}

/// Toggle logging at runtime (called when the `log_to_files` pref changes).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether lines are being appended to the log file.
pub fn file_logging_on() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The in-memory ring buffer (last `RECENT_CAP` lines) as text — Tail-logs seeds
/// from this when file-logging is off.
pub fn recent() -> String {
    RECENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Path to the log file (creating the data dir), if it resolves.
pub fn file_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("lingobar.log"))
}

/// Emit a timestamped line as a `log_line` event (so an open Tail-logs window
/// shows it live) and — only when `log_to_files` is on — append it to the file.
pub fn line(app: &AppHandle, msg: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let entry = format!("[{ts}] {msg}");
    // Always keep the last RECENT_CAP lines in memory (oldest dropped) for Tail-logs.
    {
        let mut buf = RECENT.lock().unwrap_or_else(|e| e.into_inner());
        buf.push_back(entry.clone());
        while buf.len() > RECENT_CAP {
            buf.pop_front();
        }
    }
    // File write is gated on the "Log to files" pref...
    if ENABLED.load(Ordering::Relaxed) {
        if let Some(path) = file_path(app) {
            let _guard = LOCK.lock().unwrap();
            if std::fs::metadata(&path)
                .map(|m| m.len() > MAX_BYTES)
                .unwrap_or(false)
            {
                let _ = std::fs::write(&path, b"");
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{entry}");
            }
        }
    }
    // ...but the live event always fires, so the Tail-logs window can show
    // events even when file-logging is off.
    let _ = app.emit("log_line", entry);
}
