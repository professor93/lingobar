// Custom tab-bar chrome. Renders the tab list supplied by Rust, drives
// switch / add / close / rename over IPC, and reconciles on `tabs_changed`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface TabInfo {
  id: number;
  title: string;
}
interface TabsPayload {
  win: string;
  tabs: TabInfo[];
  active: number;
  pinned: boolean;
}

const tabsEl = document.getElementById("tabs") as HTMLElement;

// Double-click → inline rename. Enter saves, Esc cancels, blur saves.
function startRename(label: HTMLElement, id: number): void {
  const title = label.textContent || "";
  const input = document.createElement("input");
  input.className = "tab-edit";
  input.value = title;

  let done = false;
  const finish = (save: boolean): void => {
    if (done) return;
    done = true;
    const v = input.value.trim();
    const next = save && v ? v : title;
    label.textContent = next; // restore the label inline (current title)
    input.replaceWith(label);
    if (save && v && v !== title) invoke("tab_rename", { id, title: v });
  };

  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      finish(true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      finish(false);
    }
  });
  input.addEventListener("blur", () => finish(true));

  label.replaceWith(input);
  input.focus();
  input.select();
}

function createTab(t: TabInfo): HTMLElement {
  const tab = document.createElement("div");
  tab.className = "tab";
  tab.dataset.id = String(t.id);
  tab.addEventListener("click", () => invoke("tab_select", { id: t.id }));
  tab.addEventListener("dblclick", (e: MouseEvent) => {
    e.stopPropagation();
    const lbl = tab.querySelector<HTMLElement>(".tab-label");
    if (lbl) startRename(lbl, t.id);
  });

  const label = document.createElement("span");
  label.className = "tab-label";
  label.textContent = t.title;
  tab.appendChild(label);

  const close = document.createElement("button");
  close.className = "tab-close";
  close.textContent = "×";
  close.title = "Close tab";
  close.addEventListener("click", (e: MouseEvent) => {
    e.stopPropagation();
    invoke("tab_close", { id: t.id });
  });
  tab.appendChild(close);

  return tab;
}

// Reconcile by id so element identity (and any in-progress rename input)
// survives selection re-renders.
let prevActive = -1;
function render(p: TabsPayload): void {
  const seen = new Set<number>();
  for (const t of p.tabs) {
    seen.add(t.id);
    let tab = tabsEl.querySelector<HTMLElement>(`.tab[data-id="${t.id}"]`);
    if (!tab) {
      tab = createTab(t);
      tabsEl.appendChild(tab);
    }
    tab.classList.toggle("active", t.id === p.active);
    const label = tab.querySelector<HTMLElement>(".tab-label");
    if (label && label.textContent !== t.title) label.textContent = t.title;
  }
  for (const el of Array.from(tabsEl.children)) {
    if (!seen.has(Number((el as HTMLElement).dataset.id))) el.remove();
  }
  // Scroll the active tab into view only when it actually changed, so a
  // title/pin update doesn't yank the horizontal scroll while scrolling.
  if (p.active !== prevActive) {
    tabsEl
      .querySelector<HTMLElement>(`.tab[data-id="${p.active}"]`)
      ?.scrollIntoView({ inline: "nearest", block: "nearest" });
    prevActive = p.active;
  }
  document.body.classList.toggle("pinned", p.pinned);
}

const plus = document.querySelector(".plus");
if (plus) plus.addEventListener("click", () => invoke("tab_new"));

// Window controls (the 3 dots): ✕ hides the window, 📌 pins, ⊞ new window.
const wire = (selector: string, cmd: string) => {
  const el = document.querySelector(selector);
  if (el) el.addEventListener("click", () => invoke(cmd));
};
wire(".ctl.close", "window_close");
wire(".ctl.pin", "window_pin");
wire(".ctl.neww", "window_new");

// Only render updates for THIS window. `tab_list` (a per-webview command) is
// authoritative for our window label; ignore `tabs_changed` events meant for
// other windows so multiple windows never cross-render each other's tabs.
let myWin: string | null = null;
listen("tabs_changed", (e: { payload: TabsPayload }) => {
  // tabs_changed is emitted only to this window's chrome (emit_to), so the
  // payload is always ours — if the tab_list seed hasn't set myWin yet, adopt it
  // here so a failed seed can't leave the bar permanently blank.
  if (myWin === null) myWin = e.payload.win;
  if (e.payload.win === myWin) render(e.payload);
});
// Seed from the authoritative per-webview tab list; retry a few times so a
// transient failure doesn't leave a permanently blank bar.
function seed(attempt = 0): void {
  invoke("tab_list")
    .then((p: TabsPayload) => {
      myWin = p.win;
      render(p);
    })
    .catch((err) => {
      console.error("tab_list failed", err);
      if (attempt < 3) setTimeout(() => seed(attempt + 1), 150 * (attempt + 1));
    });
}
seed();

// No context menu in the header bar: suppress the native menu and show nothing.
// (Tab actions live on the affordances themselves — × button, double-click to
// rename, the pin dot, and + / ⌘T / ⌘W.)
document.addEventListener("contextmenu", (e: MouseEvent) => e.preventDefault());

export {};
