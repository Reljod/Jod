import { defineConfig } from "vitest/config";

// The suites here cover the parts of the HUD that are pure logic — graph
// physics, camera framing, the world reducer, the timeline and the simulation
// driver. They are deliberately `environment: "node"`: none of them touch the
// DOM, and keeping it that way is what makes them fast enough to run on every
// edit.
export default defineConfig({
  test: {
    globals: true,
    environment: "node",
    include: ["test/**/*.test.ts"],
  },
});
