import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Kept separate from vite.config.ts: that file configures the dev server Tauri
// drives, and none of it (fixed port, strictPort, src-tauri watch ignores)
// applies to a test run.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    coverage: {
      provider: "v8",
      // The floor is a hard failure, matching the Rust side. A coverage number
      // nobody blocks on is a number that only ever goes down.
      thresholds: { lines: 95, functions: 95, statements: 95, branches: 90 },
      include: ["src/**/*.{ts,tsx}"],
      exclude: [
        // The Vite entrypoint: three lines that mount React into the DOM.
        // Testing it would assert that ReactDOM works.
        "src/main.tsx",
        // Type-only; erased at compile time, so there is nothing to execute.
        "src/types.ts",
        "src/test/**",
      ],
    },
  },
});
