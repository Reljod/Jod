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

/**
 * `AgentEvent` is an internally-tagged enum; `kind` discriminates it.
 *
 * All eleven variants, not the eight that are renderable. `progress`, `delta`
 * and `session_lost` were on the wire long before they were on this union, and
 * an event kind TypeScript does not know about is not merely undrawn: every
 * exhaustive `switch` over `kind` silently returns `undefined` for it, which is
 * how `heatFor` came to make an agent's heat `NaN` the first time a real Claude
 * Code run streamed a partial message.
 */
export type AgentEvent =
  | { kind: "started"; session_id: string | null; model: string | null }
  | { kind: "thinking"; text: string }
  /**
   * A tick, not content: the harness is mid-turn with nothing renderable yet.
   * The only thing on the wire while a turn reasons for nine minutes, so it is
   * the only thing that can distinguish "still working" from "died".
   */
  | { kind: "progress"; thinking_tokens?: number }
  /**
   * A fragment of a content block still being emitted. Duplicates text that
   * arrives complete in the following `message`/`tool_call`, so a consumer
   * that only wants finished forms ignores it.
   */
  | { kind: "delta"; text: string }
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
  /** The harness was asked to resume a conversation it no longer holds. */
  | { kind: "session_lost"; session_id: string }
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

// ─── core/src/conversation.rs ───────────────────────────────────────────────

/** What kind of turn a message is. Mirrors the `role` column's vocabulary. */
export type Role = "user" | "assistant" | "thinking" | "tool_call" | "tool_result" | "system";

/**
 * One node of the transcript DAG.
 *
 * The event stream and this are two records of the same run, kept for different
 * reasons: the stream is what a UI watches, this is what a replay reads. Only
 * one of them carries the turn that *opened* the run — a run's prompt is
 * appended here as a `user` message and never appears as an event — which is
 * why the trajectory joins across.
 */
export interface Message {
  id: number;
  /** The conversation that *minted* this message, not a visibility filter. */
  conversation_id: string;
  parent_id: number | null;
  role: Role;
  text: string;
  tool_name: string | null;
  /** The structured payload, kept whole rather than summarised. */
  tool_input: unknown;
  /** The run that produced this message, when a run did. */
  run_id: string | null;
  /** Where this sat in its run's event stream. `null` for a typed message. */
  run_seq: number | null;
  at_ms: number;
  /** `false` once a compaction has summarised this out of the live window. */
  active: boolean;
}

/** A conversation as a list renders it. */
export interface ConversationSummary {
  id: string;
  /** The stored title, or the opening user message, truncated. */
  title: string;
  harness: string;
  model: string | null;
  /** The harness-side session, which is the join back to a run. */
  session_id: string | null;
  head_id: number | null;
  forked_from: string | null;
  message_count: number;
  updated_at_ms: number;
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

// ─── the fleet tree ──────────────────────────────────────────────────────────

/** Mirrors `tree::NodeKind` in `core/src/tree.rs`. */
export type FleetNodeKind = "work" | "session" | "run";

/**
 * Mirrors `tree::NodeId` — a row's identity, stable across a rebuild.
 *
 * Two rows can share an `id` string across kinds, so the pair is the key. The
 * Rust side spells the discriminant `kind_tag` because `kind` was taken.
 */
export interface FleetNodeId {
  kind_tag: string;
  id: string;
}

/**
 * One already-flattened row of the fleet tree. Mirrors `tree::Node`.
 *
 * This is the *same forest the TUI draws* — `Store::forest_of` in `jod-core`
 * flattens it once and both surfaces render that. `depth` is what makes it a
 * tree: rows arrive in document order, each one directly below its parent.
 */
export interface FleetNode {
  id: FleetNodeId;
  parent: FleetNodeId | null;
  kind: FleetNodeKind;
  depth: number;
  label: string;
  /** Newest message or tool call. Already one line. */
  summary: string;
  running: boolean;
  /** Open cards anywhere in this row's subtree. */
  cards: number;
  /** Of those, the ones blocking. */
  blocked: number;
  colour: string;
  has_children: boolean;
}

/** The same key `NodeId` is, as something usable in a `Map` or a `key=`. */
export function fleetKey(id: FleetNodeId): string {
  return `${id.kind_tag}:${id.id}`;
}
