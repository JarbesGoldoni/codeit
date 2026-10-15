import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `bun run dev` serves the app with hot reload; run `codeit web --no-open` next to it for the API.
const api = process.env.CODEIT_URL ?? "http://localhost:7317";

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "../crates/codeit/web-dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 1500,
  },
  server: {
    port: 5317,
    proxy: {
      "/api": {
        target: api,
        ws: true,
        changeOrigin: true,
        // codeit only answers its own page: present the requests as coming from it.
        configure: (proxy) => {
          proxy.on("proxyReq", (req) => req.setHeader("origin", api));
          proxy.on("proxyReqWs", (req) => req.setHeader("origin", api));
        },
      },
    },
  },
});
