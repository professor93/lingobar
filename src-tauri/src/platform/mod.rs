//! Platform-specific native bits.
//!
//! macOS gets frameless-window rounding, opacity, clipboard reads, and the
//! in-app key monitor via `objc2`. Other platforms get no-op fallbacks.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    install_key_monitor, make_frameless_rounded, read_clipboard, set_window_opacity,
    write_clipboard,
};

#[cfg(not(target_os = "macos"))]
mod fallback;
#[cfg(not(target_os = "macos"))]
pub use fallback::{
    install_key_monitor, make_frameless_rounded, read_clipboard, set_window_opacity,
    write_clipboard,
};
