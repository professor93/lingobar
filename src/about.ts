// About window: fill in the version and open the developer link externally.
import { invoke } from "@tauri-apps/api/core";

invoke("app_version")
  .then((v) => {
    const el = document.getElementById("ver");
    if (el && typeof v === "string") el.textContent = "v" + v;
  })
  .catch(console.error);

document.getElementById("dev-link")?.addEventListener("click", (e) => {
  // Open externally via the app; the href is a graceful fallback if IPC is down.
  e.preventDefault();
  invoke("open_developer_link");
});
