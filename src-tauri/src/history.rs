//! Copy-history: records {source, translation, langs, time} each time a
//! translation is copied and persists to `history.json`. Shown in a dedicated
//! History window (the tray "History" item opens it) — no longer a tray submenu.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

const MAX: usize = 50;

static HISTORY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub source: String,
    pub translation: String,
    // Older history.json files predate these fields — default them so they
    // still deserialize cleanly.
    #[serde(default)]
    pub source_lang: String,
    #[serde(default)]
    pub target_lang: String,
    #[serde(default)]
    pub time_ms: u64,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("history.json"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Load saved history from disk (call once at startup).
pub fn load(app: &AppHandle) {
    let Some(p) = path(app) else {
        return;
    };
    let Ok(json) = std::fs::read_to_string(&p) else {
        return;
    };
    let Ok(entries) = serde_json::from_str::<Vec<Entry>>(&json) else {
        return;
    };
    *HISTORY.lock().unwrap() = entries;
}

/// Append a copied translation (newest first; capped, deduped) and notify the
/// History window via the `history_changed` event.
pub fn add(
    app: &AppHandle,
    source: String,
    translation: String,
    source_lang: String,
    target_lang: String,
) {
    if source.trim().is_empty() && translation.trim().is_empty() {
        return;
    }
    let entry = Entry {
        source,
        translation,
        source_lang,
        target_lang,
        time_ms: now_ms(),
    };
    {
        // Dedup the ⌘C double-fire (the native copy path AND the injection's
        // click-tracking each call add() with identical content). Check + insert
        // under ONE lock so the two can't both pass the check and double-insert.
        let mut h = HISTORY.lock().unwrap();
        if let Some(first) = h.first() {
            if first.source == entry.source && first.translation == entry.translation {
                return;
            }
        }
        h.insert(0, entry);
        h.truncate(MAX);
    }
    persist(app);
    let _ = app.emit("history_changed", ());
}

/// All entries, newest first.
pub fn list() -> Vec<Entry> {
    HISTORY.lock().unwrap().clone()
}

/// The entry at `index` (newest-first), if present.
pub fn entry_at(index: usize) -> Option<Entry> {
    HISTORY.lock().unwrap().get(index).cloned()
}

/// Remove the entry at `index` (newest-first).
pub fn remove(app: &AppHandle, index: usize) {
    {
        let mut h = HISTORY.lock().unwrap();
        if index >= h.len() {
            return;
        }
        h.remove(index);
    }
    persist(app);
    let _ = app.emit("history_changed", ());
}

/// Clear all history.
pub fn clear(app: &AppHandle) {
    HISTORY.lock().unwrap().clear();
    persist(app);
    let _ = app.emit("history_changed", ());
}

fn persist(app: &AppHandle) {
    let Some(p) = path(app) else {
        return;
    };
    let snapshot = HISTORY.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&snapshot) {
        // Atomic write: a crash mid-write must not corrupt history.json (load
        // silently drops the entire file on a parse error).
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}
