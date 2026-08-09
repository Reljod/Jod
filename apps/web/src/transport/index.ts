import type {
  AgentEnvelope,
  AgentSummary,
  HarnessInfo,
  Report,
  SpawnRequest,
  StoredRun,
} from "../types";

/** Whether the HUD is talking to a real orchestrator or running on simulation. */
export type LinkState =
  | { phase: "probing" }
  | { phase: "live"; origin: string }
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
  /** Backfill for a late-joining view. */
  events(agentId: string, sinceSeq?: number): Promise<AgentEnvelope[]>;
  harnesses(): Promise<HarnessInfo[]>;
  history(limit: number): Promise<StoredRun[]>;
}

export const EMPTY_REPORT: Report = {
  running: 0,
  completed: 0,
  failed: 0,
  killed: 0,
  total_cost_usd: 0,
  agents: [],
};
