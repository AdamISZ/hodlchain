import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Vite config tuned for Tauri dev: fixed port 1420 (matches
// tauri.conf.json devUrl), no auto-clear of console, HMR over WS on
// the same port. See https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    hmr: {
      protocol: "ws",
      host: "127.0.0.1",
      port: 1421,
    },
    watch: {
      // tauri builds into target/, don't reload on rust changes.
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: true,
  },
});
