/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: { target: "es2022", outDir: "dist", chunkSizeWarningLimit: 4096 },
  envPrefix: ["VITE_", "TAURI_"],
  test: {
    environment: "happy-dom",
    globals: false,
    setupFiles: ["./src/__tests__/setup.ts"],
    // App 全量挂载的冒烟用例在 CI 双核 runner 上会超 5s 默认值（ubuntu/windows 实测 flaky 超时）
    testTimeout: 15_000,
    hookTimeout: 15_000,
  },
});
