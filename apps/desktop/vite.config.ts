import { fileURLToPath, URL } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const hud = fileURLToPath(new URL("../../packages/hud/src", import.meta.url));

// The desktop has no dev server of its own.
//
// The window is pointed at the local `jod-api`, which serves the built HUD from
// its own origin — that same-origin-ness is what lets the API keep its
// `SameSite=Strict` cookie and no CORS headers. A Vite dev server on another
// port would be a different origin and would need both of those relaxed.
//
// So `tauri dev` runs `pnpm build` and the Rust side serves `dist/`. Iterating
// on the HUD with hot reload is what `apps/web` is for — it is the same code.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      "@jod/hud/styles.css": `${hud}/styles.css`,
      "@jod/hud": hud,
    },
    // The HUD lives outside this app's root and carries its own node_modules
    // for its tests. Without this, React is resolved twice and hooks break.
    dedupe: ["react", "react-dom"],
  },
  build: {
    // Served by axum and embedded with rust-embed, not opened from a file://
    // URL, so absolute asset paths are correct.
    outDir: "dist",
    emptyOutDir: true,
  },
});
