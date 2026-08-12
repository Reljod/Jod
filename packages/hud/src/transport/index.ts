import type {
  AgentEnvelope,
  AgentSummary,
  HarnessInfo,
  Report,
  SpawnRequest,
  StoredRun,
} from "../types";

/** A token's authority. Absent scope is treated as `read` — fail safe. */
export type Scope = "read" | "write";

/** Whether the HUD is talking to a real orchestrator or running on simulation. */
export type LinkState =
  | { phase: "probing" }
  | { phase: "live"; origin: string; scope: Scope }
  /** The orchestrator is reachable but this browser has no valid session. */
  | { phase: "auth"; reason: string }
  | { phase: "simulated"; reason: string }
  | { phase: "lost"; reason: string; retryInMs: number };

export interface TransportHandlers {
  /** One normalised agent event. The HUD's entire animation is driven by these. */
  onEnvelope(envelope: AgentEnvelope): void;
  /** A fresh roster snapshot. Sent on connect and after spawn/kill. */
  onReport(report: Report): void;
  onLink(state: LinkState): void;
}

/**
 * Everything the HUD needs from the world below it.
 *
 * The API layer is being built in a sibling session; until its framing is
 * settled this seam is what keeps the UI honest — the simulation driver and the
 * HTTP driver satisfy the same contract, so switching is a one-line change in
 * `createTransport`, not a rewrite of the views.
 */
export interface Transport {
  readonly label: string;
  start(handlers: TransportHandlers): void;
  stop(): void;
  spawn(request: SpawnRequest): Promise<AgentSummary | null>;
  kill(agentId: string): Promise<void>;
  /**
   * Backfill. `sinceSeq` is an *exclusive* cursor, and `seq` starts at 0 —
   * so passing 0 skips the `started` event, which is the one carrying
   * `session_id` and `model`. Omit it entirely for a first load.
   */
  events(agentId: string, sinceSeq?: number): Promise<AgentEnvelope[]>;
  /** Exchange a bearer token for a session cookie. Returns its scope. */
  authenticate(token: string): Promise<Scope>;
  harnesses(): Promise<HarnessInfo[]>;
  history(limit: number): Promise<StoredRun[]>;
}

/**
 * How a shell supplies its own driver.
 *
 * The browser shell has no reason to pass one — `createTransport` probes
 * `/v1/health` and picks. The desktop shell always does: it talks to `jod-api`
 * through the Tauri process rather than from the webview, because the API sets
 * no CORS headers and its session cookie is `SameSite=Strict`, both on purpose.
 */
export type TransportFactory = () => Transport | Promise<Transport>;

export const EMPTY_REPORT: Report = {
  running: 0,
  completed: 0,
  failed: 0,
  killed: 0,
  total_cost_usd: 0,
  agents: [],
};
