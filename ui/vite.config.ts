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
  },
});
