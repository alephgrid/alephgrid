import { defineConfig } from "vite";

// `npm run dev` proxies the API to a running `alephgrid` server on :7878.
export default defineConfig({
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:7878",
      "/ws": { target: "ws://127.0.0.1:7878", ws: true },
    },
  },
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 1000 },
});
