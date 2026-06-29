//! JavaScript injected into every Google Translate webview.
//!
//! Hides the page chrome outside the translator, tracks copy-button clicks for
//! history, captures/restores the source text for session persistence, and adds
//! window resize grips. `build()` prepends each webview's own tab id as
//! `__TAB_ID__` (alongside the session flag). Esc and ⌘C are owned by the native
//! key monitor — not this script; ⌘C clicks Google's own Copy button rather than
//! a Rust command. Zoom is the webview's native pageZoom, set from Rust.

/// Build the injected script for a webview.
///
/// `session_on` enables debounced source-text capture; `restore_text` (when
/// present) is typed back into the Source textarea once the page is ready.
pub fn build(tab_id: &str, session_on: bool, restore_text: Option<&str>) -> String {
    let restore = match restore_text {
        Some(text) => format!("const __RESTORE_TEXT__ = {text:?};\n"),
        None => "const __RESTORE_TEXT__ = null;\n".to_string(),
    };
    format!("const __TAB_ID__ = {tab_id:?};\nconst __SESSION_ON__ = {session_on};\n{restore}{SCRIPT}")
}

const SCRIPT: &str = r#"
(function() {
    // Transient toast (top-center, auto-fades). Exposed for hotkey feedback.
    window.__lbToast = function(msg) {
        try {
            var d = document.createElement('div');
            d.id = 'lingobar-toast';
            d.textContent = msg;
            d.style.cssText = 'position:fixed;top:14px;left:50%;transform:translateX(-50%);z-index:2147483647;background:rgba(32,33,36,.96);color:#fff;padding:8px 14px;border-radius:8px;font:13px/1.2 -apple-system,system-ui,sans-serif;box-shadow:0 4px 14px rgba(0,0,0,.35);pointer-events:none;opacity:0;transition:opacity .18s';
            (document.body || document.documentElement).appendChild(d);
            requestAnimationFrame(function(){ d.style.opacity = '1'; });
            setTimeout(function(){ d.style.opacity = '0'; setTimeout(function(){ d.remove(); }, 220); }, 1700);
        } catch (e) {}
    };

    // Invoke a Tauri command from either a local page (window.__TAURI__) or a
    // remote one. The friendly `__TAURI__` global is NOT injected into remote
    // pages (translate.google.com); only the IPC primitive `__TAURI_INTERNALS__`
    // is, via the capability's `remote` scope — so fall back to it. Returns the
    // invoke promise (or a resolved one if neither is present).
    function lbInvoke(cmd, args) {
        try {
            if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
                return window.__TAURI__.core.invoke(cmd, args || {});
            }
            if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
                return window.__TAURI_INTERNALS__.invoke(cmd, args || {});
            }
        } catch (e) {}
        return Promise.resolve();
    }

    // Append a line to the app's log (no-op unless logging is enabled; and
    // only reaches the app if the webview has working IPC — its presence/absence
    // in the log is itself a useful signal).
    function lbLog(msg) {
        try { lbInvoke('lb_log', { msg: String(msg) }); } catch (e) {}
    }

    // Tags to preserve (don't hide these)
    const PRESERVE_TAGS = new Set(['SCRIPT', 'STYLE', 'LINK', 'META', 'NOSCRIPT', 'HEAD']);

    // Hide all siblings of an element except preserved tags and our overlays.
    function hideSiblings(el) {
        if (!el || !el.parentElement) return;
        Array.from(el.parentElement.children).forEach(sibling => {
            if (sibling === el || PRESERVE_TAGS.has(sibling.tagName)
                || sibling.id === 'gtranslate-toast' || sibling.id === 'lingobar-toast'
                || sibling.id === 'lingobar-tts' || sibling.id === 'lingobar-paste'
                || sibling.id === 'lingobar-ctx' || sibling.id === 'lingobar-resize') return;
            // Already hidden — skip the write (every cssText set forces a style recalc).
            if (sibling.style.display === 'none') return;
            sibling.style.cssText = 'display: none !important;';
        });
    }

    // Hide siblings from target up to body
    function hideAncestorSiblings(target) {
        let current = target;
        while (current && current.parentElement && current.parentElement !== document.documentElement) {
            hideSiblings(current);
            current = current.parentElement;
        }
        // Also hide body siblings
        if (document.body) {
            hideSiblings(document.body);
        }
    }

    function hideElements() {
        const main = document.querySelector('c-wiz[role="main"]');
        if (main) {
            hideAncestorSiblings(main);
            // Make main a flex column (setProperty, not cssText) so the
            // stylesheet's fill-the-viewport rules apply — cssText would wipe
            // the flex-direction/min-height the sheet sets, re-opening the white gap.
            main.style.setProperty('display', 'flex', 'important');
        }
        // Google's "Send feedback" control: its own stylesheet outranks our
        // injected CSS, so force it hidden inline (inline !important always wins).
        document.querySelectorAll('[aria-label="Send feedback"]').forEach(function (el) {
            el.style.setProperty('display', 'none', 'important');
        });
    }

    function addStyles() {
        if (document.getElementById('gtranslate-hide-styles')) return;
        const style = document.createElement('style');
        style.id = 'gtranslate-hide-styles';
        style.textContent = `
            html, body {
                margin: 0 !important;
                padding: 0 !important;
                background: #fff !important;
                font-size: 56% !important;
                height: auto !important;
                min-height: 100vh !important;
                overflow: auto !important;
            }
            * {
                font-size: inherit;
            }
            /* Fill the viewport so short content never leaves a white canvas gap
               below it: stretch main to full height and let its primary child
               grow to absorb the slack. */
            c-wiz[role="main"] {
                overflow: visible !important;
                height: auto !important;
                max-height: none !important;
                min-height: 100vh !important;
                display: flex !important;
                flex-direction: column !important;
                padding-bottom: 0 !important;
                margin-bottom: 0 !important;
            }
            c-wiz[role="main"] > *:first-child {
                flex: 1 1 auto !important;
            }
            /* Remove any spacing after main element */
            c-wiz[role="main"] ~ * {
                display: none !important;
            }
            body > *:not(c-wiz):not(#gtranslate-toast):not(#lingobar-toast):not(#lingobar-tts):not(#lingobar-paste):not(#lingobar-ctx):not(#lingobar-resize), html > *:not(body):not(head) {
                display: none !important;
            }
            #gtranslate-toast, #lingobar-toast {
                display: block !important;
            }
            /* Hide Google's "Send feedback" control. aria-label is localized,
               so this matches the English UI; revisit if it reappears in
               another language. */
            [aria-label="Send feedback"] {
                display: none !important;
            }
        `;
        (document.head || document.documentElement).appendChild(style);
    }

    function showCopiedToast() {
        let toast = document.getElementById('gtranslate-toast');
        if (!toast) {
            toast = document.createElement('div');
            toast.id = 'gtranslate-toast';
            toast.textContent = 'Copied';
            toast.style.cssText = `
                position: fixed !important;
                top: 10px !important;
                left: 50% !important;
                transform: translateX(-50%) !important;
                background: rgba(0, 0, 0, 0.85) !important;
                color: white !important;
                padding: 8px 20px !important;
                border-radius: 0 0 6px 6px !important;
                font-size: 14px !important;
                font-weight: 500 !important;
                z-index: 2147483647 !important;
                opacity: 0 !important;
                transition: opacity 0.3s !important;
                pointer-events: none !important;
            `;
            document.body.appendChild(toast);
        }
        toast.style.opacity = '1';
        setTimeout(() => { toast.style.opacity = '0'; }, 2000);
    }

    // Copy translation by clicking Google's copy button. Identify it from any
    // element inside it: prefer the exact `button[aria-label="Copy translation"]`,
    // with a localized aria-label regex fallback for non-English UIs.
    // (Keep the regex below in sync with shortcuts.rs JS_COPY.)
    function isCopyButton(el) {
        if (!el || !el.closest) return null;
        const exact = el.closest('button[aria-label="Copy translation"]');
        if (exact) return exact;
        const btn = el.closest('button');
        if (!btn) return null;
        const aria = (btn.getAttribute('aria-label') || '').toLowerCase();
        return /copy|copia|copiar|copier|kopi|kopya|копир|скопир|nusxa|salin|sao ch|복사|コピー|复制|拷貝|拷贝|คัดลอก|कॉपी|प्रतिलि|نسخ|העתק|αντιγρ/.test(aria) ? btn : null;
    }

    // Record history whenever the copy button is clicked — via ⌘C OR a manual
    // click on Google's own copy icon.
    function setupCopyTracking() {
        document.addEventListener('click', function (e) {
            if (isCopyButton(e.target)) {
                lbLog('copy button click detected');
                showCopiedToast();
                recordHistory();
            }
        }, true);
    }

    // Read the active from/to language CODES from Google's selected language
    // tabs. The resolved code is used (auto-detect yields the detected code,
    // not "auto"); first selected tab = source, second = target. Falls back to
    // the sl/tl URL params if the tabs aren't present.
    // (Keep this selector in sync with tabs.rs record_copy_history.)
    function lbLangs() {
        try {
            var tabs = document.querySelectorAll('[role="tab"][aria-selected="true"][data-language-code]:not([data-language-code=""])');
            var p = new URLSearchParams(location.search);
            return {
                from: (tabs[0] && tabs[0].getAttribute('data-language-code')) || p.get('sl') || '',
                to: (tabs[1] && tabs[1].getAttribute('data-language-code')) || p.get('tl') || ''
            };
        } catch (e) { return { from: '', to: '' }; }
    }

    // Record a copied translation into history (source from the page; the
    // translation is read from the clipboard Rust-side a moment later).
    function recordHistory() {
        setTimeout(() => {
            const ta = sourceTextarea();
            const L = lbLangs();
            lbLog('recordHistory: textarea ' + (ta ? 'found len=' + ta.value.length : 'NOT FOUND'));
            lbInvoke('history_add', { source: ta ? ta.value : '', sourceLang: L.from, targetLang: L.to });
        }, 150);
    }

    // Source text: Google's labelled source textarea (fallback: first textarea).
    function sourceTextarea() {
        return document.querySelector('textarea[aria-label="Source text"]') || document.querySelector('textarea');
    }

    // Restore saved text and/or capture edits for session persistence.
    function setupSession() {
        if (__RESTORE_TEXT__) {
            let tries = 0;
            const iv = setInterval(() => {
                const ta = sourceTextarea();
                if (ta) {
                    // Native value setter + input/change events so Google's
                    // framework (React/Closure) observes the change and re-translates.
                    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
                    setter.call(ta, __RESTORE_TEXT__);
                    ta.dispatchEvent(new Event('input', { bubbles: true }));
                    ta.dispatchEvent(new Event('change', { bubbles: true }));
                    clearInterval(iv);
                } else if (++tries > 60) {
                    clearInterval(iv);
                }
            }, 200);
        }
        if (__SESSION_ON__) {
            let timer = null;
            document.addEventListener('input', (e) => {
                if (!e.target || e.target.tagName !== 'TEXTAREA') return;
                const text = e.target.value;
                clearTimeout(timer);
                timer = setTimeout(() => {
                    lbInvoke('session_update_text', { tabId: __TAB_ID__, text });
                }, 300);
            }, true);
        }
    }

    // ---- Speaking (TTS) control ----------------------------------------
    let __ttsPaused = false;

    function styleTtsBtn(b) {
        b.style.cssText = 'cursor:pointer!important;color:#fff!important;font-size:14px!important;padding:0 3px!important;line-height:1!important;user-select:none!important;';
    }

    function ensureTtsBar() {
        let bar = document.getElementById('lingobar-tts');
        if (bar) return bar;
        bar = document.createElement('div');
        bar.id = 'lingobar-tts';
        bar.style.cssText = 'position:fixed!important;top:0!important;left:50%!important;transform:translateX(-50%)!important;display:none;align-items:center!important;gap:9px!important;background:rgba(20,20,20,0.9)!important;color:#fff!important;padding:5px 12px!important;border-radius:0 0 10px 10px!important;z-index:2147483647!important;font:500 12px -apple-system,system-ui,sans-serif!important;box-shadow:0 2px 8px rgba(0,0,0,0.35)!important;';
        const label = document.createElement('span');
        label.textContent = '🔊 Speaking';
        const pp = document.createElement('span');
        pp.id = 'lingobar-tts-pp';
        pp.textContent = '⏸';
        styleTtsBtn(pp);
        pp.onclick = () => {
            if (__ttsPaused) { window.__tts.resume(); __ttsPaused = false; pp.textContent = '⏸'; }
            else { window.__tts.pause(); __ttsPaused = true; pp.textContent = '▶'; }
        };
        const stop = document.createElement('span');
        stop.textContent = '✕';
        styleTtsBtn(stop);
        stop.onclick = () => { window.__tts.stop(); hideTtsBar(); };
        bar.appendChild(label);
        bar.appendChild(pp);
        bar.appendChild(stop);
        (document.body || document.documentElement).appendChild(bar);
        return bar;
    }
    function showTtsBar() {
        const bar = ensureTtsBar();
        __ttsPaused = false;
        const pp = document.getElementById('lingobar-tts-pp');
        if (pp) pp.textContent = '⏸';
        bar.style.display = 'flex';
    }
    function hideTtsBar() {
        const bar = document.getElementById('lingobar-tts');
        if (bar) bar.style.display = 'none';
    }

    function setupTTS() {
        const mediaEls = new Set();
        const audioCtxs = new Set();
        const liveNodes = new Set();
        const playing = new Set();
        const refresh = () => { if (playing.size > 0) showTtsBar(); else hideTtsBar(); };

        // HTMLMediaElement: media events don't bubble -> capture on document.
        document.addEventListener('playing', (e) => {
            if (e.target instanceof HTMLMediaElement) { mediaEls.add(e.target); playing.add(e.target); refresh(); }
        }, true);
        document.addEventListener('ended', (e) => {
            if (e.target instanceof HTMLMediaElement) { playing.delete(e.target); mediaEls.delete(e.target); refresh(); }
        }, true);
        const realPlay = HTMLMediaElement.prototype.play;
        HTMLMediaElement.prototype.play = function(...a) { mediaEls.add(this); return realPlay.apply(this, a); };

        // Web Audio: patch AudioBufferSourceNode.start (one-shot).
        const AC = window.AudioContext || window.webkitAudioContext;
        if (AC && window.AudioBufferSourceNode) {
            const startProto = AudioBufferSourceNode.prototype.start;
            AudioBufferSourceNode.prototype.start = function(...a) {
                liveNodes.add(this); audioCtxs.add(this.context); playing.add(this); refresh();
                this.addEventListener('ended', () => { liveNodes.delete(this); playing.delete(this); refresh(); });
                return startProto.apply(this, a);
            };
        }

        window.__tts = {
            pause() {
                mediaEls.forEach(el => { if (!el.paused) el.pause(); });
                audioCtxs.forEach(c => { if (c.state === 'running') c.suspend(); });
            },
            resume() {
                mediaEls.forEach(el => { if (el.paused && !el.ended) el.play().catch(() => {}); });
                audioCtxs.forEach(c => { if (c.state === 'suspended') c.resume(); });
            },
            stop() {
                mediaEls.forEach(el => { el.pause(); try { el.currentTime = 0; } catch (e) {} });
                liveNodes.forEach(n => { try { n.stop(0); } catch (e) {} });
                liveNodes.clear(); playing.clear();
                audioCtxs.forEach(c => { try { c.suspend(); } catch (e) {} });
                refresh();
            }
        };
    }

    // ---- "Paste from clipboard" prompt ---------------------------------
    function hidePaste() {
        const bar = document.getElementById('lingobar-paste');
        if (bar) bar.style.display = 'none';
    }

    function showPaste(text) {
        let bar = document.getElementById('lingobar-paste');
        if (!bar) {
            bar = document.createElement('div');
            bar.id = 'lingobar-paste';
            bar.style.cssText = 'position:fixed!important;top:0!important;left:50%!important;transform:translateX(-50%)!important;display:flex;align-items:center!important;gap:6px!important;background:rgba(20,20,20,0.9)!important;color:#fff!important;padding:5px 12px!important;border-radius:0 0 10px 10px!important;z-index:2147483646!important;font:500 12px -apple-system,system-ui,sans-serif!important;cursor:pointer!important;box-shadow:0 2px 8px rgba(0,0,0,0.35)!important;';
            bar.addEventListener('click', () => {
                const ta = sourceTextarea();
                if (ta && typeof bar.dataset.text === 'string') {
                    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
                    setter.call(ta, bar.dataset.text);
                    ta.dispatchEvent(new Event('input', { bubbles: true }));
                    ta.focus();
                }
                hidePaste();
            });
            (document.body || document.documentElement).appendChild(bar);
        }
        bar.dataset.text = text;
        const preview = text.length > 30 ? text.slice(0, 30) + '…' : text;
        bar.textContent = '📋 Paste: ' + preview;
        bar.style.display = 'flex';
    }

    // Offer to paste when the window is focused, the textarea is empty, and the
    // clipboard has text.
    function maybeShowPaste() {
        const ta = sourceTextarea();
        if (!ta || ta.value.trim() !== '') { hidePaste(); return; }
        lbInvoke('clipboard_text').then((text) => {
            const cur = sourceTextarea();
            if (text && cur && cur.value.trim() === '') showPaste(text);
        }).catch(() => {});
    }

    function setupPaste() {
        window.addEventListener('focus', maybeShowPaste);
        // Hide the prompt once the user starts typing/pasting.
        document.addEventListener('input', (e) => {
            if (e.target && e.target.tagName === 'TEXTAREA' && e.target.value.trim() !== '') hidePaste();
        }, true);
        setTimeout(maybeShowPaste, 600);
    }

    // ---- Right-click menu: Copy/Paste on text fields only --------------
    // The default WebKit menu is always suppressed; a minimal Copy/Paste menu
    // appears only over a textarea/input, nothing over the rest of the page.
    function setupContextMenu() {
        function closeMenu() {
            var m = document.getElementById('lingobar-ctx');
            if (m) m.remove();
        }
        document.addEventListener('contextmenu', function (e) {
            e.preventDefault();
            closeMenu();
            var field = (e.target && e.target.closest) ? e.target.closest('textarea, input') : null;
            if (!field) return; // no menu outside text fields
            var menu = document.createElement('div');
            menu.id = 'lingobar-ctx';
            menu.style.cssText = 'position:fixed;z-index:2147483647;background:rgba(40,40,42,0.98);color:#fff;border-radius:7px;padding:4px;min-width:120px;box-shadow:0 6px 20px rgba(0,0,0,.4);font:13px -apple-system,system-ui,sans-serif;user-select:none;';
            function addItem(label, fn) {
                var it = document.createElement('div');
                it.textContent = label;
                it.style.cssText = 'padding:5px 12px;border-radius:4px;cursor:default;';
                it.onmouseenter = function () { it.style.background = 'rgba(255,255,255,0.14)'; };
                it.onmouseleave = function () { it.style.background = 'transparent'; };
                it.onmousedown = function (ev) { ev.preventDefault(); };
                it.onclick = function () { closeMenu(); fn(); };
                menu.appendChild(it);
            }
            addItem('Copy', function () {
                try { document.execCommand('copy'); } catch (err) {}
            });
            addItem('Cut', function () {
                try { document.execCommand('cut'); } catch (err) {}
            });
            addItem('Paste', function () {
                lbInvoke('clipboard_text').then(function (text) {
                    if (!text) return;
                    field.focus();
                    var s = field.selectionStart, en = field.selectionEnd;
                    if (typeof s === 'number' && field.setRangeText) {
                        field.setRangeText(text, s, en, 'end');
                    } else {
                        field.value += text;
                    }
                    field.dispatchEvent(new Event('input', { bubbles: true }));
                }).catch(function () {});
            });
            (document.body || document.documentElement).appendChild(menu);
            var mw = menu.offsetWidth, mh = menu.offsetHeight;
            menu.style.left = Math.min(e.clientX, window.innerWidth - mw - 4) + 'px';
            menu.style.top = Math.min(e.clientY, window.innerHeight - mh - 4) + 'px';
        }, true);
        document.addEventListener('mousedown', function (e) {
            var m = document.getElementById('lingobar-ctx');
            if (m && !m.contains(e.target)) closeMenu();
        }, true);
        document.addEventListener('keydown', function (e) { if (e.key === 'Escape') closeMenu(); }, true);
        window.addEventListener('scroll', closeMenu, true);
        window.addEventListener('blur', closeMenu);
    }

    // Keep the caret visible while typing in the source textarea. It auto-grows;
    // when its bottom drops below the visible area we scroll the real scroll
    // container *just enough* to bring it back \u2014 never past the content, so no
    // white gap is ever exposed. No fragile caret-pixel mirror (that mis-measured
    // at small sizes and over-scrolled, showing the white page background).
    function setupCaretFollow() {
        var cachedScroller = null;
        function findScroller(el) {
            // Reuse the cached scroller while it's still attached — avoids a
            // getComputedStyle ancestor-walk (a forced layout) every keystroke.
            if (cachedScroller && cachedScroller.isConnected) return cachedScroller;
            var n = el.parentElement;
            while (n && n !== document.documentElement) {
                var oy = getComputedStyle(n).overflowY;
                if ((oy === 'auto' || oy === 'scroll') && n.scrollHeight > n.clientHeight + 2) { cachedScroller = n; return n; }
                n = n.parentElement;
            }
            cachedScroller = document.scrollingElement || document.documentElement;
            return cachedScroller;
        }
        function follow() {
            var ta = sourceTextarea();
            if (!ta || ta.tagName !== 'TEXTAREA' || document.activeElement !== ta) return;
            try {
                var sc = findScroller(ta);
                var win = (sc === document.scrollingElement || sc === document.documentElement || sc === document.body);
                var vBot = win ? window.innerHeight : sc.getBoundingClientRect().bottom;
                var bottom = ta.getBoundingClientRect().bottom;
                var margin = 12;
                if (bottom > vBot - margin) {
                    sc.scrollTop += bottom - (vBot - margin);
                }
            } catch (e) {}
        }
        // Coalesce input+keyup into a single rAF pass rather than running the
        // layout read synchronously on every event.
        var scheduled = false;
        function schedule() {
            if (scheduled) return;
            scheduled = true;
            requestAnimationFrame(function () { scheduled = false; follow(); });
        }
        document.addEventListener('input', schedule, true);
        document.addEventListener('keyup', schedule, true);
    }

    // Thicker resize grab-area: invisible edge/corner strips driving the window's
    // native resize (the OS border on a frameless window is too thin). Covers the
    // left/right/bottom edges + bottom corners; the full overlay is click-through
    // except the thin strips, so the page stays interactive.
    function setupResizeGrips() {
        if (document.getElementById('lingobar-resize')) return;
        // Window label for the low-level resize command (__TAB_ID__ = win_N_tab_M).
        var winLabel = (__TAB_ID__ || '').replace(/_tab_\d+$/, '');
        var box = document.createElement('div');
        box.id = 'lingobar-resize';
        box.style.cssText = 'position:fixed;inset:0;z-index:2147483646;pointer-events:none;';
        function grip(css, dir, cursor) {
            var g = document.createElement('div');
            g.style.cssText = 'position:absolute;pointer-events:auto;' + css;
            g.style.cursor = cursor;
            g.addEventListener('mousedown', function (e) {
                e.preventDefault();
                e.stopPropagation();
                lbInvoke('plugin:window|start_resize_dragging', { label: winLabel, value: dir });
            });
            box.appendChild(g);
        }
        var T = '7px', C = '14px';
        grip('left:0;top:0;bottom:0;width:' + T, 'West', 'ew-resize');
        grip('right:0;top:0;bottom:0;width:' + T, 'East', 'ew-resize');
        grip('left:0;right:0;bottom:0;height:' + T, 'South', 'ns-resize');
        grip('left:0;bottom:0;width:' + C + ';height:' + C, 'SouthWest', 'nesw-resize');
        grip('right:0;bottom:0;width:' + C + ';height:' + C, 'SouthEast', 'nwse-resize');
        (document.body || document.documentElement).appendChild(box);
    }

    // Block stray ASCII control-character insertion in text fields. In a
    // frameless window an arrow key at a text boundary (Left at start, Right at
    // end, etc.) can insert FS/GS/RS/US (U+001C–U+001F) instead of doing nothing.
    // Prevent it at the source, and strip any that slip through. Tab/newline/CR
    // (U+0009/000A/000D) are allowed.
    function setupControlCharGuard() {
        var bad = '';
        for (var i = 0; i < 0x20; i++) {
            if (i !== 9 && i !== 10 && i !== 13) bad += String.fromCharCode(i);
        }
        function has(s) {
            for (var i = 0; i < s.length; i++) if (bad.indexOf(s[i]) !== -1) return true;
            return false;
        }
        function strip(s) {
            var out = '';
            for (var i = 0; i < s.length; i++) if (bad.indexOf(s[i]) === -1) out += s[i];
            return out;
        }
        document.addEventListener('beforeinput', function (e) {
            if (e.data && has(e.data)) e.preventDefault();
        }, true);
        document.addEventListener('input', function (e) {
            var t = e.target;
            if (!t || (t.tagName !== 'TEXTAREA' && t.tagName !== 'INPUT')) return;
            if (!has(t.value)) return;
            var pos = t.selectionStart || 0;
            var before = strip(t.value.slice(0, pos));
            t.value = strip(t.value);
            try { t.setSelectionRange(before.length, before.length); } catch (e2) {}
        }, true);
    }

    // One-time setup: listeners, observers, monkeypatches. Idempotent re-hiding
    // for late-loading DOM is handled separately below.
    function init() {
        // Only the top frame is the translator; skip the chrome-hiding / resize /
        // paste machinery in Google's iframes (they're hidden anyway).
        if (window.top !== window.self) return;
        if (window.__lingobarInit) return;
        window.__lingobarInit = true;

        lbLog('injection init: __TAURI__=' + (!!window.__TAURI__) + ' INTERNALS=' + (!!window.__TAURI_INTERNALS__) + ' tab=' + __TAB_ID__);
        addStyles();
        hideElements();
        setupCopyTracking();
        setupSession();
        setupTTS();
        setupPaste();
        setupContextMenu();
        setupCaretFollow();
        setupResizeGrips();

        // Observe DOM changes and re-hide chrome, coalesced to one pass per
        // animation frame. Google's SPA mutates heavily during live translation,
        // so running hideElements per-mutation would peg the CPU on every tab.
        let hideScheduled = false;
        function scheduleHide() {
            if (hideScheduled) return;
            hideScheduled = true;
            requestAnimationFrame(() => {
                hideScheduled = false;
                hideElements();
            });
        }
        const observer = new MutationObserver(scheduleHide);
        function observeBody(opts) {
            const target = document.body || document.documentElement;
            if (target) { observer.disconnect(); observer.observe(target, opts); }
        }
        // While the page builds, watch the whole subtree; narrow it once settled.
        observeBody({ childList: true, subtree: true });

        // Sweep every 500ms only while the page is still building. Once the
        // translator (c-wiz[role="main"]) is up, narrow the observer to top-level
        // childList (late chrome appears at body/ancestor level, not deep in the
        // translation subtree — subtree observation is the wasteful part) and stop
        // the sweep; the debounced observer alone keeps chrome hidden, so an idle
        // tab costs ~no CPU.
        let sweeps = 0;
        const settle = setInterval(() => {
            hideElements();
            if (document.querySelector('c-wiz[role="main"]')) {
                observeBody({ childList: true });
                clearInterval(settle);
            } else if (++sweeps >= 20) {
                clearInterval(settle);
            }
        }, 500);
    }

    // Start as soon as possible.
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    // Re-apply styles/hiding for late-loading Translate DOM (init itself is
    // guarded, so its listeners/observers are only ever installed once).
    function reHide() { addStyles(); hideElements(); }
    setTimeout(init, 0);
    setTimeout(reHide, 100);
    setTimeout(reHide, 500);
    setTimeout(reHide, 1000);
})();
"#;
