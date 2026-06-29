//! Non-macOS fallbacks. The native chrome bits are no-ops off macOS for now.

/// No-op: frameless rounded corners are macOS-only for now.
pub fn make_frameless_rounded(_window: &tauri::Window, _radius: f64) {}

/// No-op: window opacity has no cross-platform wiring yet.
pub fn set_window_opacity(_window: &tauri::Window, _percent: i32) {}

/// No-op: the macOS NSEvent key monitor has no non-macOS equivalent yet
/// (the JS keydown bridge handles shortcuts there).
pub fn install_key_monitor() {}

/// No clipboard read wired on non-macOS yet.
pub fn read_clipboard() -> Option<String> {
    None
}

/// No clipboard write wired on non-macOS yet.
pub fn write_clipboard(_text: &str) {}
