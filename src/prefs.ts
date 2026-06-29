import { invoke } from "@tauri-apps/api/core";

type Prefs = {
  shortcut_toggle: string;
  session_restore: boolean;
  launch_at_login: boolean;
  log_to_files: boolean;
  sleep_idle_tabs: boolean;
};

const MOD_SYMBOL: Record<string, string> = { Cmd: "⌘", Ctrl: "⌃", Alt: "⌥", Shift: "⇧" };

// Pretty-print a Tauri combo string ("Cmd+Ctrl+Shift+KeyT") as "⌘⌃⇧T".
function display(combo: string): string {
  if (!combo) return "—";
  const parts = combo.split("+");
  const key = parts.pop() ?? "";
  const mods = parts.map((m) => MOD_SYMBOL[m] ?? m).join("");
  const keyName = key.replace(/^Key/, "").replace(/^Digit/, "").replace("Backquote", "`");
  return mods + keyName;
}

// Build a Tauri combo string from a keydown event, or null if invalid.
function comboFromEvent(e: KeyboardEvent): string | null {
  const mods: string[] = [];
  if (e.metaKey) mods.push("Cmd");
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (/^(Meta|Control|Alt|Shift)(Left|Right)$/.test(e.code)) return null; // modifier-only
  if (mods.length === 0) return null; // require at least one modifier
  return [...mods, e.code].join("+");
}

async function load(): Promise<void> {
  const p = await invoke<Prefs>("get_prefs");
  (document.getElementById("rec-toggle") as HTMLElement).textContent = display(p.shortcut_toggle);
  (document.getElementById("session_restore") as HTMLInputElement).checked = p.session_restore;
  (document.getElementById("launch_at_login") as HTMLInputElement).checked = p.launch_at_login;
  (document.getElementById("sleep_idle_tabs") as HTMLInputElement).checked = p.sleep_idle_tabs;
  (document.getElementById("log_to_files") as HTMLInputElement).checked = p.log_to_files;
}

// Only one recorder may be armed at a time — arming a second cancels the first
// (otherwise both capture the next keypress and clobber each other's shortcut).
let activeCleanup: (() => void) | null = null;

document.querySelectorAll<HTMLElement>(".rec").forEach((rec) => {
  rec.addEventListener("click", () => {
    if (rec.classList.contains("recording")) return;
    if (activeCleanup) activeCleanup(); // cancel any other armed recorder first

    const prev = rec.textContent ?? "—";
    rec.classList.add("recording");
    rec.textContent = "Press keys…";

    const cleanup = (restore = true) => {
      rec.classList.remove("recording");
      if (restore) rec.textContent = prev;
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
      if (activeCleanup === cleanup) activeCleanup = null;
    };
    const onBlur = () => cleanup();
    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      if (e.key === "Escape") {
        cleanup();
        return;
      }
      const combo = comboFromEvent(e);
      if (!combo) return;
      cleanup(false);
      const ok = await invoke<boolean>("set_shortcut", { which: rec.dataset.which, combo });
      if (ok) rec.textContent = display(combo);
      else void load();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    activeCleanup = cleanup;
  });
});

document.querySelectorAll<HTMLInputElement>("input[data-pref]").forEach((cb) => {
  cb.addEventListener("change", () => {
    void invoke("set_pref", { key: cb.dataset.pref, value: cb.checked });
  });
});

type Appearance = {
  zoom: number;
  opacity: number;
  lang_from: string;
  lang_to: string;
  win_w: number;
  win_h: number;
  max_w: number;
  max_h: number;
};

const ZOOM_LEVELS = [50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100, 110, 120, 130, 140, 150];
const OPACITY_LEVELS = [30, 40, 50, 60, 70, 80, 90, 100];

function fill(el: HTMLSelectElement, opts: [string, string][], val: string): void {
  el.replaceChildren(...opts.map(([v, t]) => new Option(t, v)));
  el.value = val;
}

// A range slider: integer steps (no manual entry), value shown live, tick marks
// every 10 units via the --tick CSS var, persisted on release. (Webviews have
// no haptic/resistance API, so the ticks are the visual "detent" equivalent.)
function setupSlider(
  id: string,
  outId: string,
  min: number,
  max: number,
  value: number,
  unit: string,
  save: (v: number) => void,
): void {
  const el = document.getElementById(id) as HTMLInputElement;
  const out = document.getElementById(outId) as HTMLOutputElement;
  el.min = String(min);
  el.max = String(max);
  el.step = "1";
  el.value = String(Math.min(Math.max(value, min), max));
  // Ticks every 10 units for small ranges (zoom/opacity — the "feel at 10");
  // for large ranges (window size) cap at ~10 ticks so they aren't too dense.
  const tickUnits = Math.max(10, Math.round((max - min) / 10));
  el.style.setProperty("--tick", `${(tickUnits / (max - min)) * 100}%`);
  const show = (): void => {
    out.textContent = `${el.value}${unit}`;
  };
  show();
  el.addEventListener("input", show);
  el.addEventListener("change", () => save(Number(el.value)));
}

async function loadAppearance(): Promise<void> {
  const langs = await invoke<[string, string][]>("get_languages");
  const a = await invoke<Appearance>("get_appearance");

  setupSlider("zoom", "zoom-out", Math.min(...ZOOM_LEVELS), Math.max(...ZOOM_LEVELS), a.zoom, "%", (v) =>
    void invoke("set_zoom_pref", { level: v }),
  );
  setupSlider("opacity", "opacity-out", Math.min(...OPACITY_LEVELS), Math.max(...OPACITY_LEVELS), a.opacity, "%", (v) =>
    void invoke("set_opacity_pref", { percent: v }),
  );

  const winW = document.getElementById("win_w") as HTMLInputElement;
  const winH = document.getElementById("win_h") as HTMLInputElement;
  const saveSize = (): void =>
    void invoke("set_window_size", { width: Number(winW.value), height: Number(winH.value) });
  setupSlider("win_w", "win_w-out", 252, a.max_w, a.win_w, "px", saveSize);
  setupSlider("win_h", "win_h-out", 155, a.max_h, a.win_h, "px", saveSize);

  const from = document.getElementById("lang_from") as HTMLSelectElement;
  const to = document.getElementById("lang_to") as HTMLSelectElement;
  fill(from, [["auto", "Auto-detect"], ...langs], a.lang_from);
  fill(to, langs, a.lang_to);
  const saveLangs = () => void invoke("set_default_languages", { from: from.value, to: to.value });
  from.addEventListener("change", saveLangs);
  to.addEventListener("change", saveLangs);
}

// Hidden Logging section: 10 clicks on the title reveals it.
let titleClicks = 0;
document.querySelector("h1")?.addEventListener("click", () => {
  if (++titleClicks >= 10) {
    const sec = document.getElementById("logging-section");
    if (sec) sec.style.display = "block";
  }
});
document
  .getElementById("open_app_folder")
  ?.addEventListener("click", () => void invoke("open_app_folder"));
document
  .getElementById("tail_log")
  ?.addEventListener("click", () => void invoke("open_log_window"));
// Wired here (not inside loadAppearance) so it attaches even if the appearance
// IPC fails.
document
  .getElementById("reset_size")
  ?.addEventListener("click", () => void invoke("reset_window_size"));

load().catch(console.error);
loadAppearance().catch(console.error);
