import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    rollupOptions: {
      input: {
        chrome: "chrome.html",
        prefs: "prefs.html",
        about: "about.html",
        logview: "logview.html",
        history: "history.html",
      },
    },
  },
});
