// Mirrors the serde representation of jod's `core/` crate.
// Source of truth: core/src/{event,service,store,team}.rs and core/src/harness/mod.rs.
//
// Verified against that crate directly — NOT against apps/desktop/src/types.ts,
// which is an unmaintained mirror since the desktop app left the workspace.

// ─── core/src/harness/mod.rs ────────────────────────────────────────────────

/** Three harnesses now. Anything switching exhaustively must handle `agy`. */
export type HarnessKind = "claude_code" | "open_code" | "agy";

export const HARNESS_KINDS: readonly HarnessKind[] = [
  "claude_code",
  "open_code",
  "agy",
] as const;

export type PermissionPolicy = "ask" | "accept_edits" | "bypass";

/**
 * Externally-tagged: `"fresh"` | `"last"` | `{ session: "<id>" }`.
 * This is what lets Jod hold a conversation rather than fire one-shot tasks.
 */
export type Resume = "fresh" | "last" | { session: string };

/** What the caller asked for. Harness-neutral on purpose. */
export interface SpawnRequest {
  name: string;
  harness: HarnessKind;
  prompt: string;
  /** Required by the Rust struct — it has no serde default. */
  cwd: string;
  model?: string | null;
  permission?: PermissionPolicy;
  resume?: Resume;
}

// ─── core/src/event.rs ──────────────────────────────────────────────────────

export interface Usage {
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  cost_usd?: number;
}

/** `AgentEvent` is an internally-tagged enum; `kind` discriminates it. */
export type AgentEvent =
  | { kind: "started"; session_id: string | null; model: string | null }
  | { kind: "thinking"; text: string }
  | { kind: "message"; text: string }
  | { kind: "tool_call"; name: string; input?: unknown }
  | { kind: "tool_result"; name: string; summary?: string; is_error: boolean }
  | {
      kind: "finished";
      text?: string;
      exit_code?: number;
      is_error: boolean;
      usage: Usage;
    }
  | { kind: "raw"; line: string }
  | { kind: "error"; message: string };

export type AgentEventKind = AgentEvent["kind"];

/** The event, flattened together with its envelope fields. */
export type AgentEnvelope = AgentEvent & {
  agent_id: string;
  at_ms: number;
  /** Monotonic per-agent sequence number, so a late-joining UI can resume. */
  seq: number;
};

// ─── core/src/service.rs ────────────────────────────────────────────────────

export type AgentStatus = "running" | "completed" | "failed" | "killed";

/** Whether a harness can actually be used on this machine. */
export interface HarnessInfo {
  id: string;
  label: string;
  available: boolean;
  path: string | null;
}

/** The client-facing view of one agent. */
export interface AgentSummary {
  id: string;
  name: string;
  harness: HarnessKind;
  harness_label: string;
  status: AgentStatus;
  cwd: string;
  model: string | null;
  permission: PermissionPolicy;
  /** The supervising `jod-run` process, and the group holding it and the
   *  harness. Null before the launch; kept afterwards, so a finished run still
   *  says what ran it. */
  pid: number | null;
  pgid: number | null;
  /** Whether that process group still exists. A different question to
   *  `status`: a run can be marked running with nothing alive behind it. */
  process_alive: boolean;
  /** What a human runs to follow this agent — `jod watch <id>`. */
  watch_command: string;
  created_at_ms: number;
  session_id: string | null;
  usage: Usage;
  event_count: number;
  last_message: string | null;
}

export interface Report {
  running: number;
  completed: number;
  failed: number;
  killed: number;
  total_cost_usd: number;
  agents: AgentSummary[];
}

// ─── core/src/team.rs ───────────────────────────────────────────────────────

/**
 * A teammate's coarse lifecycle. Unknown text becomes `error` on the Rust side
 * rather than failing the read, so this union is exhaustive on the wire.
 */
export type MemberStatus =
  | "ready"
  | "busy"
  | "shutdown_requested"
  | "shutdown"
  | "error";

/** One teammate on a cross-harness team. */
export interface Member {
  team: string;
  name: string;
  harness: HarnessKind;
  role: string;
  status: MemberStatus;
  /** The run currently embodying it, if any. */
  agent_id: string | null;
  /** The harness-side conversation to resume. */
  session_id: string | null;
}

/** One item on a team's shared board. */
export interface TeamTask {
  id: string;
  title: string;
  owner: string | null;
  /** `open` | `claimed` | `done`, plus whatever a future Jod writes. */
  status: string;
}

/** `GET /v1/teams/{team}` — roster and board in one answer. */
export interface TeamView {
  team: string;
  members: Member[];
  tasks: TeamTask[];
}

// ─── core/src/store.rs ──────────────────────────────────────────────────────

/** One persisted delegation — run history that survives a restart. */
export interface StoredRun {
  id: string;
  name: string;
  harness: string;
  status: string;
  cwd: string;
  session_id: string | null;
  pid: number | null;
  pgid: number | null;
  created_at_ms: number;
  /** The full client-facing summary, kept verbatim. Shape of `AgentSummary`. */
  summary: Partial<AgentSummary> & Record<string, unknown>;
}

/** Jod's memory. Bitemporal, FTS-searchable. */
export interface Fact {
  id: number;
  subject: string;
  predicate: string;
  object: string;
  source: string | null;
  valid_from: string | null;
  valid_to: string | null;
  recorded_at_ms: number;
  state: string;
}

// ─── derived helpers ────────────────────────────────────────────────────────

/** Total tokens across every bucket the harness reported. */
export function totalTokens(usage: Usage | undefined): number {
  if (!usage) return 0;
  return (
    (usage.input_tokens ?? 0) +
    (usage.output_tokens ?? 0) +
    (usage.cache_read_tokens ?? 0) +
    (usage.cache_write_tokens ?? 0)
  );
}

export function isLive(agent: AgentSummary): boolean {
  return agent.status === "running";
}

/** Short display code per harness, for dense HUD chrome. */
export function harnessCode(h: HarnessKind): string {
  switch (h) {
    case "claude_code":
      return "CLDE";
    case "open_code":
      return "OPNC";
    case "agy":
      return "AGY";
  }
}

export function resumeLabel(r: Resume | undefined): string {
  if (!r || r === "fresh") return "FRESH";
  if (r === "last") return "CONTINUE";
  return `SESSION ${r.session.slice(0, 8)}`;
}

/** Mirrors `TeamTask::is_done` in `core/src/team.rs`. */
export function taskIsDone(task: TeamTask): boolean {
  return task.status === "done";
}

/** Mirrors `TeamTask::is_claimed`. */
export function taskIsClaimed(task: TeamTask): boolean {
  return task.owner !== null && task.owner !== undefined;
}
