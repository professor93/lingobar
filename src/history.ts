// History window: copied translations (newest first) shown as a TABLE
// (Source preview · From · To · When). Clicking a row expands a detail row with
// the full source + translation and the per-entry actions. Entries hold
// untrusted Google-page text, so EVERYTHING is rendered with `textContent`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Entry {
  source: string;
  translation: string;
  source_lang: string;
  target_lang: string;
  time_ms: number;
}

const rowsEl = document.getElementById("rows") as HTMLElement;
const tableEl = document.getElementById("tbl") as HTMLElement;
const emptyEl = document.getElementById("empty") as HTMLElement;

// code → display name for the langs columns (best-effort; falls back to the code).
const langNames = new Map<string, string>();

function langLabel(code: string): string {
  if (!code) return "—";
  if (code === "auto") return "auto";
  return langNames.get(code) || code;
}

function timeLabel(ms: number): string {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  } catch {
    return "—";
  }
}

function snippet(s: string): string {
  const t = s.replace(/\s+/g, " ").trim();
  if (!t) return "(empty)";
  return t.length > 90 ? t.slice(0, 90) + "…" : t;
}

function actionButton(label: string, fn: () => void, cls = ""): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  if (cls) b.className = cls;
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    fn();
  });
  return b;
}

function field(label: string, text: string): HTMLElement {
  const f = document.createElement("div");
  f.className = "field";
  const l = document.createElement("div");
  l.className = "label";
  l.textContent = label;
  const t = document.createElement("div");
  t.className = "text";
  t.textContent = text || "(empty)";
  f.append(l, t);
  return f;
}

// Append an entry's two table rows: the clickable summary row and the (hidden)
// detail row that expands beneath it.
function appendEntry(e: Entry, index: number): void {
  const row = document.createElement("tr");
  row.className = "row";
  row.dataset.time = String(e.time_ms);

  const src = document.createElement("td");
  src.className = "src";
  src.textContent = snippet(e.source);
  src.title = e.source;

  const from = document.createElement("td");
  from.className = "lang";
  from.textContent = langLabel(e.source_lang);

  const to = document.createElement("td");
  to.className = "lang";
  to.textContent = langLabel(e.target_lang);

  const when = document.createElement("td");
  when.className = "when";
  when.textContent = timeLabel(e.time_ms);

  row.append(src, from, to, when);

  const detail = document.createElement("tr");
  detail.className = "detail";
  detail.hidden = true;
  const cell = document.createElement("td");
  cell.colSpan = 4;
  cell.append(field("Source", e.source), field("Translation", e.translation));

  const actions = document.createElement("div");
  actions.className = "actions";
  actions.append(
    actionButton("Open in new window", () => void invoke("history_open", { index })),
    actionButton("Copy source", () => void invoke("set_clipboard", { text: e.source })),
    actionButton("Copy translated", () => void invoke("set_clipboard", { text: e.translation })),
    actionButton("Remove", () => void invoke("history_remove", { index }), "danger"),
  );
  cell.append(actions);
  detail.append(cell);

  row.addEventListener("click", () => {
    const open = detail.hidden;
    detail.hidden = !open;
    row.classList.toggle("open", open);
  });

  rowsEl.append(row, detail);
}

function render(): void {
  // Preserve the expanded row + scroll across a refresh (e.g. a copy landing
  // while the window is open) so the open row doesn't snap shut / the list jump.
  const list = document.getElementById("list");
  const openTime = rowsEl.querySelector<HTMLElement>(".row.open")?.dataset.time;
  const scroll = list?.scrollTop ?? 0;
  void invoke<Entry[]>("history_list")
    .then((entries) => {
      rowsEl.textContent = "";
      const has = entries.length > 0;
      tableEl.hidden = !has;
      emptyEl.hidden = has;
      entries.forEach((e, i) => appendEntry(e, i));
      if (openTime) {
        const row = rowsEl.querySelector<HTMLElement>(`.row[data-time="${openTime}"]`);
        const detail = row?.nextElementSibling as HTMLElement | null;
        if (row && detail) {
          row.classList.add("open");
          detail.hidden = false;
        }
      }
      if (list) list.scrollTop = scroll;
    })
    .catch(console.error);
}

// Seed the language names first (so the first render shows names, not codes),
// then render. Subsequent backend changes re-render via the event.
void invoke<[string, string][]>("get_languages")
  .then((langs) => langs.forEach(([code, name]) => langNames.set(code, name)))
  .catch(console.error)
  .finally(render);

document.getElementById("clear")?.addEventListener("click", () => void invoke("history_clear"));

void listen("history_changed", () => render());

export {};
