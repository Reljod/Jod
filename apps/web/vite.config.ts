import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The API layer is being built by a sibling session. Point the proxy at it with
// JOD_API_ORIGIN and the web app talks to the real orchestrator; leave it unset
// and the app runs on its simulation driver instead (see src/transport).
const apiOrigin = process.env.JOD_API_ORIGIN ?? "http://127.0.0.1:8787";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: apiOrigin, changeOrigin: true },
      "/ws": { target: apiOrigin, ws: true, changeOrigin: true },
    },
  },
  test: {
    globals: true,
    environment: "node",
    include: ["test/**/*.test.ts"],
  },
});
