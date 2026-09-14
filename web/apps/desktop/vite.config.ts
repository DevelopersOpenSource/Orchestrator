import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// O Tauri abre esta porta em desenvolvimento e empacota `dist` no build.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5179,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    chunkSizeWarningLimit: 2500,
  },
});
