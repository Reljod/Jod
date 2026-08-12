import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Hud } from "@jod/hud";
import "@jod/hud/styles.css";

// The browser shell passes no transport: the HUD probes `/v1/health` and picks
// the real orchestrator or its simulation driver on its own. Same-origin is the
// whole reason this shell is the simple one — `jod-api` sets no CORS headers and
// its session cookie is `SameSite=Strict`, so the browser must be served from
// the same origin as the API (or proxied to it, as `vite.config.ts` does in dev).
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Hud />
  </StrictMode>,
);
