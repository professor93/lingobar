//! Logical hotkey actions, the pure key->action mapping, and dispatch.
//!
//! The macOS `NSEvent` monitor (and a future non-macOS JS bridge) translate
//! raw key events into these actions, so the mapping itself stays testable and
//! platform-independent. The "primary" modifier is Cmd on macOS, Ctrl elsewhere.

use tauri::AppHandle;

use crate::tabs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    NewWindow,
    CloseTab,
    Copy,
    NextTab,
    PrevTab,
    SwitchWindow,
    HideAll,
    HideCurrent,
    Speak,
    Mic,
    /// Jump to the nth tab (1-indexed) in the focused window.
    SelectTab(usize),
    /// Jump to the last tab in the focused window.
    SelectLastTab,
    /// Open (or focus) the Preferences window.
    OpenPrefs,
}

/// Pure mapping from modifier state + key to an action.
///
/// `cmd` is the primary modifier (Cmd on macOS). Primary shortcuts require the
/// Control key to be *up* so they never collide with the global toggle
/// (Cmd+Ctrl+Shift+T); tab cycling is the only Control+Tab combo.
pub fn map_key(
    cmd: bool,
    ctrl: bool,
    shift: bool,
    ch: Option<char>,
    is_tab: bool,
    is_backtick: bool,
    is_escape: bool,
) -> Option<Action> {
    // Escape hides everything; Shift+Escape hides just the focused window.
    // Handled natively (not via webview IPC) so it works regardless of the
    // remote page's command access.
    if is_escape {
        return Some(if shift {
            Action::HideCurrent
        } else {
            Action::HideAll
        });
    }
    if cmd && !ctrl {
        if is_backtick {
            return Some(Action::SwitchWindow);
        }
        match ch {
            Some('t') if !shift => return Some(Action::NewTab),
            Some('n') => return Some(Action::NewWindow),
            Some('w') if !shift => return Some(Action::CloseTab),
            Some('c') if !shift => return Some(Action::Copy),
            // ⌘Q hides instead of quitting — quit is only via the tray menu.
            Some('q') => return Some(Action::HideAll),
            // Cmd+Shift+S/M click Google Translate's own listen / voice-input icons.
            Some('s') if shift => return Some(Action::Speak),
            Some('m') if shift => return Some(Action::Mic),
            // ⌘, opens (or focuses) Preferences — the standard macOS shortcut.
            Some(',') => return Some(Action::OpenPrefs),
            // ⌘1-9 jump to that tab (1-indexed); ⌘0 jumps to the last tab.
            // `!shift` so ⌘⇧3/⌘⇧4 (system screenshots) pass through untouched.
            Some('0') if !shift => return Some(Action::SelectLastTab),
            Some(d) if !shift && d.is_ascii_digit() => {
                return Some(Action::SelectTab(d as usize - '0' as usize));
            }
            _ => {}
        }
    }
    if ctrl && !cmd && is_tab {
        return Some(if shift {
            Action::PrevTab
        } else {
            Action::NextTab
        });
    }
    None
}

/// Whether an auxiliary app window (Preferences / About / log
/// viewer) — not a translator window — currently has focus.
fn aux_window_focused(app: &AppHandle) -> bool {
    use tauri::Manager;
    app.webview_windows().into_values().any(|w| {
        w.is_focused().unwrap_or(false)
            && matches!(w.label(), "about" | "prefs" | "logview")
    })
}

/// Run an action.
pub fn dispatch(app: &AppHandle, action: Action) {
    crate::log::line(app, &format!("action {action:?}"));
    // Tab/window/copy actions must never act on a translator window while an
    // auxiliary window is focused — only Esc (hide) may, and it already handles
    // auxiliary windows itself.
    if !matches!(
        action,
        Action::HideAll | Action::HideCurrent | Action::OpenPrefs
    ) && aux_window_focused(app)
    {
        return;
    }
    match action {
        // New tab on the focused custom window (or a fresh window if none focused).
        Action::NewTab => match tabs::focused_window(app) {
            Some(win) => tabs::new_tab(app, &win),
            None => {
                let _ = tabs::new_window(app);
            }
        },
        Action::NewWindow => {
            let _ = tabs::new_window(app);
        }
        // ⌘W closes the active tab; closing the last tab just hides the window.
        Action::CloseTab => {
            if let Some(win) = tabs::focused_window(app) {
                tabs::close_active(app, &win);
            }
        }
        // ⌘C copies the selection, or the translation if nothing is selected.
        Action::Copy => {
            tabs::eval_active(app, JS_COPY);
            // Record history natively — the remote webview's IPC is unreliable.
            tabs::record_copy_history(app);
        }
        Action::NextTab => {
            if let Some(win) = tabs::focused_window(app) {
                tabs::cycle_tab(app, &win, true);
            }
        }
        Action::PrevTab => {
            if let Some(win) = tabs::focused_window(app) {
                tabs::cycle_tab(app, &win, false);
            }
        }
        Action::SelectTab(n) => {
            if let Some(win) = tabs::focused_window(app) {
                tabs::select_nth(app, &win, n);
            }
        }
        Action::SelectLastTab => {
            if let Some(win) = tabs::focused_window(app) {
                tabs::select_last(app, &win);
            }
        }
        Action::OpenPrefs => crate::open_preferences(app),
        Action::SwitchWindow => tabs::cycle_windows(app),
        Action::HideAll => hide_windows(app, false),
        Action::HideCurrent => hide_windows(app, true),
        // Click Google's own listen / voice controls in the active tab.
        Action::Speak => {
            tabs::eval_active(app, JS_SPEAK);
        }
        Action::Mic => {
            tabs::eval_active(app, JS_MIC);
        }
    }
}

// Buttons are matched primarily by their exact (English) aria-label — the
// selectors Google currently uses — with the Material SVG icon-path as a
// fallback for other UI languages (the icon geometry is locale-independent).
//
// Copy:  button[aria-label="Copy translation"]      (see injection.rs)
// Speak: button[aria-label="Listen to translation"]  / volume_up path "M3 9v6h4l5 5V4…"
// Mic:   button[aria-label="Translate by voice"]     / mic path "M12 14c1.66 0 3-1.34 3-3V5…"

/// Toggle play/stop for the *translation* listen button. While playing, Google
/// adds a "Stop listening" button, so: if one is present, click it (stop);
/// otherwise click "Listen to translation" (start). Falls back to the rightmost
/// speaker-icon button (source listen is leftmost) for non-English UIs.
const JS_SPEAK: &str = r#"(function(){
  var t=window.__lbToast||function(){};
  function vis(x){return x&&x.getBoundingClientRect().width>0;}
  var stop=document.querySelector('button[aria-label="Stop listening"]');
  if(vis(stop)){stop.click();t('⏹  Stopped');return;}
  var b=document.querySelector('button[aria-label="Listen to translation"]');
  if(!vis(b)){
    var SPK='M3 9v6h4l5 5V4';
    var c=[].slice.call(document.querySelectorAll('button')).filter(function(x){
      return vis(x)&&[].slice.call(x.querySelectorAll('svg path')).some(function(p){
        return (p.getAttribute('d')||'').indexOf(SPK)===0;});
    });
    c.sort(function(m,n){return m.getBoundingClientRect().x-n.getBoundingClientRect().x;});
    b=c[c.length-1];
  }
  if(!vis(b)){t('🔊  Nothing to play yet');return;}
  b.click();
  t('🔊  Playing translation');
})();"#;

/// Toggle the "Translate by voice" (mic) button. While recording, Google adds a
/// "Stop translation by voice" button, so: if one is present, click it (stop);
/// otherwise click "Translate by voice" (start). Falls back to the mic-icon path.
const JS_MIC: &str = r#"(function(){
  var t=window.__lbToast||function(){};
  function vis(x){return x&&x.getBoundingClientRect().width>0;}
  var stop=document.querySelector('button[aria-label="Stop translation by voice"]');
  if(vis(stop)){stop.click();t('🎤  Stopped listening');return;}
  var b=document.querySelector('button[aria-label="Translate by voice"]');
  if(!vis(b)){
    var MIC='M12 14c1.66 0 3-1.34 3-3V5';
    b=[].slice.call(document.querySelectorAll('button')).filter(function(x){
      return vis(x)&&[].slice.call(x.querySelectorAll('svg path')).some(function(p){
        return (p.getAttribute('d')||'').indexOf(MIC)===0;});
    })[0];
  }
  if(!vis(b)){t('🎤  Voice input unavailable here');return;}
  b.click();
  t('🎤  Listening… (allow microphone if asked)');
})();"#;

/// ⌘C: copy the current selection, or click Google's "Copy translation" button
/// when nothing is selected. Driven from the native key monitor so it works
/// regardless of which webview holds focus.
/// (Keep the localized copy-button regex below in sync with injection.rs isCopyButton.)
const JS_COPY: &str = r#"(function(){
  var s=window.getSelection();
  if(s&&s.toString().trim()){document.execCommand('copy');return;}
  var b=document.querySelector('button[aria-label="Copy translation"]');
  if(!b){
    var bs=[].slice.call(document.querySelectorAll('button'));
    for(var i=0;i<bs.length;i++){
      var a=(bs[i].getAttribute('aria-label')||'').toLowerCase();
      if(/copy|copia|copiar|copier|kopi|kopya|копир|скопир|nusxa|salin|sao ch|복사|コピー|复制|拷貝|拷贝|คัดลอก|कॉपी|प्रतिलि|نسخ|העתק|αντιγρ/.test(a)){b=bs[i];break;}
    }
  }
  if(b)b.click();
})();"#;

/// Hide translator windows (skipping pinned ones). With `current_only`, hide
/// just the focused window. Prefs windows are left alone.
fn hide_windows(app: &AppHandle, current_only: bool) {
    use tauri::Manager;
    // Esc inside an auxiliary window (About / Preferences / log
    // viewer) dismisses just that window — never the translator windows behind it.
    if let Some(focused) = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false))
    {
        if matches!(focused.label(), "about" | "prefs" | "logview") {
            let _ = focused.hide();
            return;
        }
    }
    // Hide custom translator windows (skip pinned ones).
    for (label, window) in app.windows() {
        if !label.starts_with("win_") || tabs::is_pinned(&label) {
            continue;
        }
        if current_only && !window.is_focused().unwrap_or(false) {
            continue;
        }
        let _ = window.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_core_shortcuts() {
        assert_eq!(
            map_key(true, false, false, Some('t'), false, false, false),
            Some(Action::NewTab)
        );
        assert_eq!(
            map_key(true, false, false, Some('n'), false, false, false),
            Some(Action::NewWindow)
        );
        assert_eq!(
            map_key(true, false, false, Some('w'), false, false, false),
            Some(Action::CloseTab)
        );
        assert_eq!(
            map_key(true, false, false, Some('c'), false, false, false),
            Some(Action::Copy)
        );
        assert_eq!(
            map_key(true, false, false, Some('q'), false, false, false),
            Some(Action::HideAll)
        );
        assert_eq!(
            map_key(true, false, false, None, false, true, false),
            Some(Action::SwitchWindow)
        );
        assert_eq!(
            map_key(false, true, false, None, true, false, false),
            Some(Action::NextTab)
        );
        assert_eq!(
            map_key(false, true, true, None, true, false, false),
            Some(Action::PrevTab)
        );
    }

    #[test]
    fn maps_escape_to_hide() {
        assert_eq!(
            map_key(false, false, false, None, false, false, true),
            Some(Action::HideAll)
        );
        assert_eq!(
            map_key(false, false, true, None, false, false, true),
            Some(Action::HideCurrent)
        );
    }

    #[test]
    fn maps_tab_number_shortcuts() {
        assert_eq!(
            map_key(true, false, false, Some('1'), false, false, false),
            Some(Action::SelectTab(1))
        );
        assert_eq!(
            map_key(true, false, false, Some('9'), false, false, false),
            Some(Action::SelectTab(9))
        );
        assert_eq!(
            map_key(true, false, false, Some('0'), false, false, false),
            Some(Action::SelectLastTab)
        );
        // ⌘⇧3 / ⌘⇧4 are system screenshots — must NOT be captured.
        assert_eq!(
            map_key(true, false, true, Some('3'), false, false, false),
            None
        );
    }

    #[test]
    fn ignores_unrelated_and_global_toggle() {
        // Global toggle (Cmd+Ctrl+Shift+T) must pass through to the plugin.
        assert_eq!(
            map_key(true, true, true, Some('t'), false, false, false),
            None
        );
        // Plain keys with no primary modifier pass through.
        assert_eq!(
            map_key(false, false, false, Some('t'), false, false, false),
            None
        );
    }
}
