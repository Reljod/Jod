import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

// jsdom implements no layout, so this is a no-op stub rather than a missing
// function that would throw inside EventStream's scroll-to-bottom effect.
window.HTMLElement.prototype.scrollIntoView = vi.fn();

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
