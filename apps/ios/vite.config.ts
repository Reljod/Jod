import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The daemon this app talks to. In development it is proxied so the browser
 * sees same-origin routes and the session cookie behaves exactly as it will on
 * the device; in production the app is served *by* the daemon (or reached over
 * the tailnet), so every path stays relative.
 */
const apiOrigin = process.env.JOD_API_ORIGIN ?? "http://127.0.0.1:8787";

export default defineConfig({
  plugins: [react()],

  server: {
    // `tauri ios dev` runs the app on a real device or simulator, which reaches
    // this server across the network rather than on loopback.
    host: process.env.TAURI_DEV_HOST ?? "0.0.0.0",
    port: 5174,
    strictPort: true,
    proxy: {
      "/v1": {
        target: apiOrigin,
        changeOrigin: true,
        // SSE must not be buffered, or the transcript arrives in one lump when
        // the run ends instead of streaming.
        ws: false,
      },
    },
    fs: {
      // `src/contract.ts` re-exports the shared API types from `apps/web`, one
      // directory up and outside this project's root.
      allow: [".", ".."],
    },
  },

  build: {
    // iOS 15 is the floor: Tauri v2's minimum, and old enough to cover any
    // phone that will run this.
    target: "es2020",
  },

  test: {
    globals: true,
    // Node by default — the reducer, the transport and the store are all
    // platform-free, and a DOM for them would only be slower. The component
    // suite opts into jsdom with a `@vitest-environment` docblock.
    environment: "node",
    include: ["test/**/*.test.ts", "test/**/*.test.tsx"],
  },
});
