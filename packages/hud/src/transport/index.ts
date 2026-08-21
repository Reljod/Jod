import type {
  AgentEnvelope,
  AgentSummary,
  ConversationSummary,
  FleetNode,
  HarnessInfo,
  Message,
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

/** One git worktree a work delete would leave on disk, and how it stands. */
export interface WorkLease {
  worktree_path: string;
  branch: string;
  dirty: boolean;
  merged: boolean;
  missing: boolean;
}

/** What deleting a work would take, counted before anything is taken. */
export interface WorkDoomed {
  work_id: string;
  title: string;
  sessions: number;
  transcripts: number;
  unanswered_cards: number;
  mail: number;
  /** Runs that lose their last transcript. Their rows and costs are kept. */
  orphaned_runs: number;
  leases: WorkLease[];
}

/**
 * One read of the fleet: the rows, and the run each one stands for.
 *
 * Mirrors `FleetPage` in `api/src/workspaces.rs`. It is an object rather than a
 * bare list of rows because the fold that produced them also decided something
 * a row can no longer say for itself — with no run rows left, "open this agent"
 * has nothing to reach for, and this is where that went.
 */
export interface Fleet {
  nodes: FleetNode[];
  /**
   * The run a row's verbs act on, keyed by [`fleetKey`] — the live one if there
   * is one, otherwise the last one it took. Absent for a row that has never run
   * anything, which is a real state and not a missing answer: a manager nobody
   * has given an instruction to has no run to open.
   */
  runOf: ReadonlyMap<string, string>;
}

/** An empty fleet, for the drivers and the states that have nothing to show. */
export const NO_FLEET: Fleet = { nodes: [], runOf: new Map() };

/**
 * The answer to a work delete, whichever way it went.
 *
 * `deleted` is the only thing that says which. A caller must never infer it
 * from the presence of `doomed`, which is populated either way — on a refusal
 * it is the confirmation dialog's contents.
 */
export interface WorkDeletion {
  deleted: boolean;
  /** One sentence, safe to show verbatim. */
  detail: string;
  doomed: WorkDoomed;
  /** Paths left on disk. Never removed: a branch may hold uncommitted work. */
  worktrees_left?: string[];
  /** When the armed confirmation expires, on a refusal. */
  confirm_before_ms?: number;
}

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
   * Forget a finished run — its row and its events.
   *
   * Deliberately not `kill` with a flag. Killing ends a run and keeps the
   * record; this removes the record, and the API refuses while the run is still
   * alive rather than stopping it on the caller's behalf. A UI offering "stop
   * and delete" makes both calls, in that order, and can say which one failed.
   */
  deleteRun(agentId: string): Promise<void>;
  /**
   * Remove a session and its thread.
   *
   * Refused for the pinned main chat and for any session that belongs to a
   * work — delete the work instead. Both refusals arrive as a thrown error
   * carrying the server's own sentence, which already says what to do.
   */
  deleteConversation(conversationId: string): Promise<void>;
  /**
   * Remove a work and every session in it.
   *
   * Two-step when the work holds git worktrees: the first call resolves to a
   * {@link WorkDeletion} with `deleted: false` and everything the delete would
   * take, and repeating the call inside the window goes through. That is the
   * server's protocol, not a convention invented here — see `docs/jod-api.md`.
   */
  deleteWork(workId: string): Promise<WorkDeletion>;
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
  /**
   * The fleet tree — the repositories, and the agents inside them.
   *
   * A **query**, not a subscription, and deliberately so: the server builds it
   * from the database rather than from the answering process's memory, which is
   * why it shows runs this daemon never launched. `/v1/agents` cannot say that.
   *
   * Two levels, because `jod_core::tree::condense` folds it before it goes on
   * the wire — the same fold `jod tui` draws, done once in Rust rather than
   * again here. Works and runs are not rows. A run is still reachable: the
   * conversation that owns it answers for it through [`Fleet.runOf`].
   *
   * Returns an empty fleet rather than throwing when a driver has none to
   * offer, so a panel renders "no work yet" instead of an error.
   */
  fleet(): Promise<Fleet>;
  /**
   * Recent conversations, newest first.
   *
   * Wanted here for one thing the event stream does not carry: the turn that
   * *opened* a run. A prompt is appended to the transcript as a `user` message
   * and never emitted as an event, so a trajectory built from events alone
   * cannot say what the session was asked to do.
   */
  conversations(limit: number): Promise<ConversationSummary[]>;
  /** One conversation's thread, oldest first. */
  messages(conversationId: string): Promise<Message[]>;
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
