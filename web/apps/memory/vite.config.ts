import { defineConfig } from "vite";

// A página é servida pelo memoryd (embutida no binário). Em desenvolvimento,
// o Vite repassa /api para o memoryd rodando na máquina.
export default defineConfig({
  base: "./",
  server: {
    port: 5178,
    strictPort: true,
    proxy: {
      "/api": {
        target: `http://127.0.0.1:${process.env.ORCHESTRATOR_MEMORY_PORT ?? "10000"}`,
        // O memoryd só aceita Host de loopback na porta dele.
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    chunkSizeWarningLimit: 1500,
  },
});
