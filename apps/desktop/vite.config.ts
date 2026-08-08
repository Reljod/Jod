import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri drives this dev server, so the port is fixed and failures must be loud
// rather than silently landing on a port the Rust side is not pointing at.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
