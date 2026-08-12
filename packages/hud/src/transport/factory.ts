import type { Transport } from "./index";
import { HttpTransport, probeApi } from "./http";
import { SimTransport } from "./sim";

export type TransportMode = "auto" | "live" | "sim";

/**
 * Pick a driver.
 *
 * `auto` asks `/v1/health` — the one unauthenticated route — and uses the real
 * orchestrator if something answers. Nothing answering is the normal case while
 * the API layer is still being built, so it falls back to simulation rather
 * than showing an empty screen with an error in it.
 *
 * Override with `?feed=sim` or `?feed=live` for a deterministic demo or to
 * force a connection failure into view.
 */
export async function createTransport(mode: TransportMode = "auto"): Promise<Transport> {
  if (mode === "sim") return new SimTransport("forced by ?feed=sim");
  if (mode === "live") return new HttpTransport();

  const alive = await probeApi();
  return alive
    ? new HttpTransport()
    : new SimTransport("no orchestrator on /v1/health — showing a simulated fleet");
}

export function modeFromLocation(search = location.search): TransportMode {
  const feed = new URLSearchParams(search).get("feed");
  return feed === "sim" || feed === "live" ? feed : "auto";
}
