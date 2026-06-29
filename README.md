# LingoBar

**Instant Google Translate from your macOS menu bar — in tabbed, pinnable popup windows.**

LingoBar is a lightweight (~1.8 MB) menu-bar app that puts Google Translate one keystroke away. It's an independent, unofficial client: it sends **no API requests** and does **no parsing** — it simply loads `translate.google.com` in a webview, like a browser would, and hides everything outside the translator itself.

## Features

- **Tabs & windows** — open translations as tabs in one window or as separate floating windows
- **Pin** a window so it stays open and can't be closed by accident
- **Voice input** and **read-aloud** via Google's own controls
- **Copy history**, **auto-detect language**, adjustable **zoom** and **window opacity**
- **Rebindable global hotkeys**, launch at login, optional session restore

## Install

Download from the [latest release](https://github.com/professor93/lingobar/releases/latest):

- **Apple Silicon (M1+):** `LingoBar_x.y.z_aarch64.dmg` (~1.8 MB)
- **Intel / universal:** `LingoBar_x.y.z_universal.dmg` (~3.8 MB)

Open the DMG and drag **LingoBar** into **Applications**.

The app isn't notarized (there's no Apple Developer account behind it), so on first
launch macOS may say **"LingoBar is damaged"** or **"Apple could not verify … malware."**
The app is fine — that's just Gatekeeper blocking an unsigned download. To allow it, run
this once in **Terminal**:

```sh
xattr -cr /Applications/LingoBar.app
```

Then open LingoBar normally. (Alternatively: **System Settings → Privacy & Security**,
scroll to the LingoBar prompt, and click **Open Anyway**.)

Requires **macOS 12.3+**.

## Hotkeys

| Shortcut | Action |
| --- | --- |
| `⌃⌘⇧T` | Toggle LingoBar |
| `⌘T` / `⌘N` | New tab / new window |
| `⌘W` | Close tab (closing the last one hides the window) |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab |
| `` ⌘` `` | Switch windows |
| `⌘⇧S` / `⌘⇧M` | Speak translation / voice input |
| `⌘C` | Copy translation |
| `Esc` / `⇧Esc` | Hide all / hide current |
| `⌘Q` | Hide all (quit only from the menu-bar menu) |

## Preferences

Open **Preferences** from the menu-bar menu:

- **Appearance** — translator **zoom** and **window opacity**
- **Default languages** — the source/target pair used for new tabs and windows
- **Shortcuts** — rebind the global toggle (`⌃⌘⇧T`) hotkey
- **Sleep inactive tabs** — optionally unload idle tabs to save memory (they reload on return)
- **Launch at login** and optional **session restore**

## Contributing

Contributions are welcome! Found a bug or have an idea? **[Open an issue](https://github.com/professor93/lingobar/issues/new/choose)** (there are templates for bug reports and feature requests), or send a **pull request** (please follow the PR template).

**A note on Google Translate changes:** LingoBar relies on the live structure of the Google Translate page. If Google changes that page, some conveniences may stop working — e.g. the **copy** shortcut, the **speak/mic** hotkeys, or the **hiding of non-translator elements**. The app won't fully break (the translator still loads), but those features may need a small selector update. If you hit this, an issue or a PR fixing the selector is hugely appreciated.

## Build from source

```bash
npm install
npm run tauri dev      # run in development
npm run tauri build    # build the .app / .dmg
```

Built with [Tauri](https://tauri.app) — Rust plus the system WebView — which is why it's a few megabytes instead of hundreds.

## Disclaimer

Google Translate is a trademark of Google LLC. LingoBar is an independent, unofficial client and is not affiliated with or endorsed by Google.
