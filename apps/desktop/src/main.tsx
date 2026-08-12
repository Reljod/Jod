import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Hud, HttpTransport } from "@jod/hud";
import "@jod/hud/styles.css";

/**
 * The token this window was launched with.
 *
 * The Rust shell serves `index.html` from the API's own origin and injects this
 * before any module script runs, but only to a request carrying the launch key.
 * → `src-tauri/src/server.rs`
 *
 * It is read once here and the global is deleted, so nothing later in the page —
 * an extension, a dependency, a stray `console` paste — can read a write
 * credential back out of `window`.
 */
function takeBootstrapToken(): string | undefined {
  const w = window as unknown as { __JOD_BOOTSTRAP__?: { token?: string } };
  const token = w.__JOD_BOOTSTRAP__?.token;
  delete w.__JOD_BOOTSTRAP__;
  return token || undefined;
}

/**
 * Drop the launch key out of the address bar.
 *
 * The key is in `location.search`, and anything that can read `location` can
 * re-request `/?k=…` and be handed the API token again — so deleting the global
 * above is only half the job. Replacing the history entry closes the other half.
 *
 * This is defence in depth, not a boundary: a script running *before* this line
 * still sees the key. What it removes is the durable copy — the one that would
 * otherwise sit in the address bar for the window's whole life, be read by
 * anything loaded later, and follow a `location.href` into a referrer.
 */
function stripLaunchKey(): void {
  try {
    if (!location.search) return;
    history.replaceState(null, "", location.pathname + location.hash);
  } catch {
    /* a shell that forbids history rewriting still runs; the token is in hand */
  }
}

const token = takeBootstrapToken();
stripLaunchKey();

// Same origin as the API, so the base is empty — the difference from the web
// shell is the bearer token, which also moves the event stream onto `fetch`
// because `EventSource` cannot carry a header.
//
// No token means the page was opened without a valid launch key. Rather than
// silently degrade, hand the HUD a transport that will surface the 401: its
// auth gate is the honest place for "this window is not authorised".
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Hud makeTransport={() => new HttpTransport("", token)} />
  </StrictMode>,
);
