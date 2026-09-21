import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The Tauri webview is WKWebView, so the build targets Safari, and the dev
// server is pinned to the port `tauri.conf.json` names.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "safari15",
    outDir: "dist",
    emptyOutDir: true,
  },
  // The store reducers are pure and the tests drive them with canned event
  // sequences (04 §17), so there is nothing here that wants a DOM.
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
