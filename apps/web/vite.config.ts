import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Point the proxy at a running `jod-api` with JOD_API_ORIGIN and the web app
// talks to the real orchestrator; leave it unset and the HUD falls back to its
// simulation driver (see packages/hud/src/transport).
const apiOrigin = process.env.JOD_API_ORIGIN ?? "http://127.0.0.1:8787";

const hud = fileURLToPath(new URL("../../packages/hud/src", import.meta.url));

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@jod/hud/styles.css": `${hud}/styles.css`,
      "@jod/hud": hud,
    },
    // The HUD is consumed as source from outside this app's root, and it has
    // its own node_modules for running its tests. Without this, Vite resolves
    // `react` from there as well as from here and the app loads two copies —
    // which breaks hooks with an error that does not name the real cause.
    dedupe: ["react", "react-dom"],
  },
  server: {
    port: 5173,
    proxy: {
      "/v1": { target: apiOrigin, changeOrigin: true },
    },
  },
});
