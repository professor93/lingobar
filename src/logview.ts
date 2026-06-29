// Live log-tail window. Seeds from the on-disk log (read_log), then appends
// each new line pushed by the backend's `log_line` event, auto-scrolling to the
// bottom unless the user has scrolled up to read history.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const log = document.getElementById("log") as HTMLElement;
const MAX_LINES = 2000;

// Events can arrive before the async seed resolves; buffer them so the seed's
// `textContent =` doesn't wipe an early line.
let seeded = false;
const pending: string[] = [];

function trim(): void {
  // Cap retained lines so a long logging session doesn't grow memory unbounded.
  const lines = log.textContent ? log.textContent.split("\n") : [];
  if (lines.length > MAX_LINES) {
    log.textContent = lines.slice(lines.length - MAX_LINES).join("\n");
  }
}

function append(line: string): void {
  if (!seeded) {
    pending.push(line);
    return;
  }
  const stick = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  log.textContent += line + "\n";
  trim();
  if (stick) log.scrollTop = log.scrollHeight;
}

invoke<string>("read_log")
  .then((text) => {
    log.textContent = text;
  })
  .catch(() => {})
  .finally(() => {
    seeded = true;
    if (pending.length) {
      log.textContent += pending.join("\n") + "\n";
      pending.length = 0;
    }
    trim();
    log.scrollTop = log.scrollHeight;
  });

void listen<string>("log_line", (e) => append(e.payload));

document.getElementById("clear")?.addEventListener("click", () => {
  log.textContent = "";
});

export {};
