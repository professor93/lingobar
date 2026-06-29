//! Native macOS bits via `objc2`: frameless-window corner rounding, window
//! opacity, clipboard reads, and the in-app key-event monitor.
//!
//! The tab bar and window controls are HTML in the chrome webview now, so there
//! is no native titlebar chrome here. We borrow Tauri's `NSWindow` through its
//! raw pointer with `Retained::retain` (a +1), never `from_raw`.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString,
    NSWindow,
};
use objc2_foundation::NSString;

/// Borrow Tauri's `NSWindow` from a bare multi-webview `Window`, taking our own
/// +1 reference.
fn ns_window(window: &tauri::Window) -> Option<Retained<NSWindow>> {
    let ptr = window.ns_window().ok()? as *mut NSWindow;
    unsafe { Retained::retain(ptr) }
}

/// Round a frameless window's corners by clipping its content view's layer.
/// Pairs with `WindowBuilder::transparent(true)` so the clipped corners show
/// through. No-op if the `NSWindow` can't be borrowed. Must run on the main
/// thread (callers in `setup` / window-event callbacks already are).
pub fn make_frameless_rounded(window: &tauri::Window, radius: f64) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    unsafe {
        let Some(content) = ns_window.contentView() else {
            return;
        };
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            let _: () = msg_send![&*layer, setCornerRadius: radius];
            let _: () = msg_send![&*layer, setMasksToBounds: true];
        }
    }
}

/// Set a custom window's overall opacity (clamped 0.30–1.00; 1.0 = fully opaque).
pub fn set_window_opacity(window: &tauri::Window, percent: i32) {
    if let Some(w) = ns_window(window) {
        w.setAlphaValue((percent as f64 / 100.0).clamp(0.3, 1.0));
    }
}

/// Read the general pasteboard's plain-text contents.
pub fn read_clipboard() -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let text = unsafe { pasteboard.stringForType(NSPasteboardTypeString) }?;
    Some(text.to_string())
}

/// Write plain text to the general pasteboard (used by the History window's
/// Copy source / Copy translated actions).
pub fn write_clipboard(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    let ns = NSString::from_str(text);
    unsafe {
        pasteboard.clearContents();
        let _ = pasteboard.setString_forType(&ns, NSPasteboardTypeString);
    }
}

// ---- In-app key monitor ---------------------------------------------------

/// Install a local key-down monitor that runs LingoBar's in-app shortcuts and
/// swallows the matched combos (returning null), passing everything else through
/// so the webview keeps normal paste/select/copy behaviour. Fires only while the
/// app is active.
pub fn install_key_monitor() {
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { event.as_ref() };
        if ev.r#type() == NSEventType::KeyDown {
            if let Some(action) = match_event(ev) {
                if let Some(app) = crate::APP_HANDLE.get() {
                    crate::shortcuts::dispatch(app, action);
                }
                return std::ptr::null_mut(); // swallow
            }
        }
        event.as_ptr() // pass through
    });

    let mask = NSEventMask::KeyDown;
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
    if let Some(m) = monitor {
        // Keep the monitor alive for the app's lifetime.
        std::mem::forget(m);
    }
}

/// Translate a key-down event into a LingoBar action (Tab=keyCode 48, `=50, Esc=53).
fn match_event(event: &NSEvent) -> Option<crate::shortcuts::Action> {
    let flags = event.modifierFlags();
    let cmd = flags.contains(NSEventModifierFlags::Command);
    let ctrl = flags.contains(NSEventModifierFlags::Control);
    let shift = flags.contains(NSEventModifierFlags::Shift);
    let keycode = event.keyCode();
    // Only map_key's Cmd/Ctrl branches consult `ch`, so skip the per-keydown
    // string allocation otherwise (this monitor fires for every key event).
    let ch = if cmd || ctrl {
        event
            .charactersIgnoringModifiers()
            .and_then(|s| s.to_string().chars().next())
    } else {
        None
    };
    crate::shortcuts::map_key(cmd, ctrl, shift, ch, keycode == 48, keycode == 50, keycode == 53)
}
