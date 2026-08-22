/**
 * The wire types for the seven read-only workspaces.
 *
 * Mirrors the serde representation of `jod_core::{store, schedule, webhook}` as
 * served by `api/src/workspaces.rs` — ten `GET` routes, all `Scope::Read`.
 *
 * ## Why these live here rather than in the shared mirror
 *
 * `contract.ts` re-exports the checked mirror of `core/src/{event,service,
 * store}.rs`, and that file belongs to another lane. Adding to it from here
 * would break the one-owner-per-path rule for a file three clients read. So
 * these are declared here, and **should migrate into the shared mirror once a
 * second client needs them** — at which point this file becomes a re-export like
 * `contract.ts`. Until then a duplicate is the lesser cost, because the
 * alternative is two lanes editing one file.
 *
 * ## What the API deliberately does not send
 *
 * Not `cli/src/tui/data.rs`'s row structs. Those are *presentation* types: one
 * field holds a cron already glossed to `"02:00 every day"`, another holds a
 * webhook's signing state already rendered as a tick and a relative age, and a
 * third holds a seven-slot sparkline. A gloss is an English sentence and a
 * relative timestamp is true for the second it was rendered, so serving them
 * would push the terminal's rendering onto a 393pt screen and stale in any
 * cache.
 *
 * This app therefore writes its own gloss — see `gloss.ts`, which ports the
 * wording from `data::gloss` so the phone and the terminal agree.
 */

import type { AgentSummary } from "./contract";

// ─── core/src/store.rs — memory ─────────────────────────────────────────────

/** One entity in Jod's memory graph. */
export interface MemoryNode {
  id: number;
  scope: string;
  name: string;
  kind: string;
  last_seen_ms: number;
  /**
   * Edges in either direction. The cheapest honest answer to whether this
   * memory is load-bearing or was written once and never used again.
   */
  degree: number;
}

/** One edge, from the point of view of the entity being looked at. */
export interface MemoryEdge {
  predicate: string;
  other_id: number;
  other: string;
  /** True when the focus node is the subject rather than the object. */
  outgoing: boolean;
}

/** `GET /v1/memory` — a page of nodes, plus counts for the whole graph. */
export interface MemoryPage {
  nodes: MemoryNode[];
  /**
   * Counts describe the **entire** graph, not the page. So `?limit=0` is a
   * cheap counts-only call, and honoured rather than snapped to a default.
   */
  node_count: number;
  edge_count: number;
}

/** `GET /v1/memory/{id}` — the node flattened together with its edges. */
export type MemoryNodeView = MemoryNode & {
  in_edges: MemoryEdge[];
  out_edges: MemoryEdge[];
};

/** One node in a local graph, with its distance from the root. */
export interface GraphNode {
  id: number;
  name: string;
  kind: string;
  /** 0 for the root itself. */
  hops: number;
}

export interface GraphEdge {
  from: number;
  to: number;
  predicate: string;
}

/** `GET /v1/memory/{id}/graph` — one node's neighbourhood. */
export interface LocalGraph {
  root_id: number;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

// ─── core/src/schedule.rs — schedules ───────────────────────────────────────

/** What to do about a fire that was missed while Jod was down. */
export type Misfire = "fire_once" | "skip" | "fire_all";

/** What to do when a fire arrives and the last run is still going. */
export type Overlap = "skip" | "replace" | "allow";

export type ScheduleState = "armed" | "paused" | "broken";

export interface Schedule {
  id: string;
  name: string;
  /**
   * What the agent is asked to do. Untouched by Jod — it goes to the harness as
   * an argument, never through a shell.
   */
  prompt: string;
  harness: string;
  cwd: string;
  model: string | null;
  /** A cron expression, as croner parses it. */
  cron: string;
  /**
   * An IANA zone *name*, never a captured offset. An offset is only correct
   * until the next transition, and a schedule outlives transitions — which is
   * why the gloss must be computed from this rather than from a stored offset.
   */
  timezone: string;
  state: ScheduleState;
  misfire: Misfire;
  overlap: Overlap;
  grace_ms: number;
  jitter_ms: number;
  next_fire_at_ms: number | null;
  last_fire_at_ms: number | null;
  consecutive_failures: number;
  created_at_ms: number;
}

/**
 * How a due schedule was resolved.
 *
 * Every outcome is written down, including the ones where nothing ran — a skip
 * nobody recorded is a silent failure, and "it never fired" and "it fired and
 * was skipped" are different bugs.
 */
export type FireOutcome =
  | "ran"
  | "skipped_overlap"
  | "skipped_misfire"
  | "replaced"
  | "replace_failed"
  | "spawn_failed"
  | "abandoned"
  | "monitor_quiet"
  | "unknown";

export interface Fire {
  id: number;
  schedule_id: string;
  /** The instant this fire was *for*, which is not when it happened. */
  due_at_ms: number;
  fired_at_ms: number;
  run_id: string | null;
  outcome: FireOutcome;
  detail: string | null;
}

/** `GET /v1/schedules/{name}` — the schedule flattened with its recent fires. */
export type ScheduleView = Schedule & { fires: Fire[] };

// ─── core/src/schedule.rs — goals ───────────────────────────────────────────

export type GoalState =
  | "running"
  | "paused"
  | "satisfied"
  | "stalled"
  | "exhausted"
  | "blocked";

export interface Goal {
  id: string;
  name: string;
  objective: string;
  /**
   * The check that decides "done". Deterministic and run *before* anything is
   * asked to judge progress, so a passing gate is evidence rather than an
   * opinion.
   */
  done_when: string | null;
  harness: string;
  cwd: string;
  model: string | null;
  cron: string;
  timezone: string;
  state: GoalState;
  iteration: number;
  max_iterations: number | null;
  budget_usd: number | null;
  spent_usd: number;
  /** Iterations that may finish without progress before this is called stalled. */
  stall_after: number;
  no_progress: number;
  next_fire_at_ms: number | null;
  created_at_ms: number;
}

// ─── core/src/webhook.rs — hooks ────────────────────────────────────────────

/** Extra tests a payload must pass beyond matching event and action. */
export interface Conditions {
  labels: string[];
  branch: string | null;
  author: string | null;
  draft: boolean | null;
}

export interface Rule {
  id: string;
  name: string;
  /** `github`, for now. Only the signature check is provider-specific. */
  source: string;
  /** `owner/repo`, or the any-repo sentinel. */
  repo: string;
  event: string;
  /** `null` matches every action of the event. */
  action: string | null;
  conditions: Conditions;
  /** The prompt template; `{{placeholders}}` are filled as quoted data. */
  prompt: string;
  harness: string;
  cwd: string;
  model: string | null;
  enabled: boolean;
  created_at_ms: number;
}

export type DeliveryStatus =
  | "accepted"
  | "no_match"
  | "rejected"
  | "duplicate"
  | "failed";

export interface Delivery {
  id: number;
  /** GitHub's `X-GitHub-Delivery`. Unique, because GitHub is at-least-once. */
  delivery_id: string;
  source: string;
  event: string;
  action: string | null;
  repo: string | null;
  rule_id: string | null;
  run_id: string | null;
  status: DeliveryStatus;
  detail: string | null;
  received_at_ms: number;
}

/** `GET /v1/hooks` — each rule flattened with its recent deliveries. */
export type HookView = Rule & { deliveries: Delivery[] };

// ─── api/src/workspaces.rs — activity ───────────────────────────────────────

/**
 * Where one activity line came from.
 *
 * `hook` arrived when the feed's projection moved into `jod_core::activity`.
 * Before that the HTTP feed was built separately from the terminal's and had no
 * webhook source at all, so a rejected signature reached nobody holding a phone.
 *
 * `run` and `memory` are in core's vocabulary but have no producer yet; they are
 * absent here until one lands, because a type that promises a variant the server
 * never sends is a type you cannot exhaustively switch on with confidence.
 */
export type ActivitySource = "cron" | "goal" | "hook";

/**
 * One line in the activity feed.
 *
 * `needs_you` is the field this screen exists for. A schedule Jod could not
 * start, or one whose claimant died, is silence — nothing else in the product
 * reports it, so it surfaces here or not at all.
 */
export interface ActivityItem {
  id: string;
  at_ms: number;
  source: ActivitySource;
  text: string;
  needs_you: boolean;
  /**
   * Where this line points, as a Rust tuple:
   * `["schedules" | "goals" | "hooks", name]`.
   *
   * It must actually navigate. An activity row that names a schedule and cannot
   * reach it is the feature without the point of it. For a webhook row the
   * second element is the rule's *name*, not the id the delivery stores —
   * core does that translation so no client has to.
   */
  jump_to: [string, string] | null;
}

// ─── core/src/conversation.rs — the main chat ───────────────────────────────

/**
 * What kind of turn a message is. Mirrors the `role` column's vocabulary.
 *
 * Six, not two. A real thread comes back with `thinking` turns interleaved and
 * `tool_call`/`tool_result` rows carrying `tool_name`, so a screen that assumes
 * user-or-assistant renders a conversation that never happened.
 */
export type Role =
  | "user"
  | "assistant"
  /** Reasoning the harness chose to surface. Kept separate from `assistant`
   *  because most harnesses will not accept another model's thinking as input. */
  | "thinking"
  | "tool_call"
  | "tool_result"
  /** Not the agent and not the person: a runner error, or a note from Jod. */
  | "system";

export interface Message {
  id: number;
  /**
   * The conversation that *minted* this message. Not a visibility filter: a
   * fork's thread walks through messages minted by its parent.
   */
  conversation_id: string;
  /** `null` only at a root. This is a real tree, not a list. */
  parent_id: number | null;
  role: Role;
  /**
   * The readable, searchable view of this turn. For a tool call this is a
   * truncated rendering of the input; the whole thing is in `tool_input`.
   */
  text: string;
  tool_name: string | null;
  /** The structured payload, kept whole. Carries `{"is_error":true}` on a
   *  failed tool result, which is the only thing distinguishing it. */
  tool_input: unknown | null;
  run_id: string | null;
  /** `null` for a message a person typed. With `run_id` it is the idempotence
   *  key, which is what lets a run be replayed without duplicating it. */
  run_seq: number | null;
  at_ms: number;
  /** `false` once a compaction has summarised this out of the live window. It
   *  is still stored and still searchable. */
  active: boolean;
}

export interface Conversation {
  id: string;
  title: string;
  harness: string;
  cwd: string;
  /** In the target harness's own spelling. `null` means the harness picks. */
  model: string | null;
  /**
   * `null` is not a mode — it is the absence of one, meaning "whatever the
   * caller passed". An old row must not suddenly acquire an opinion it never
   * had, so this stays nullable rather than defaulting.
   */
  permission: string | null;
  session_id: string | null;
  /** The leaf being talked to. Moving this *is* switching branches. */
  head_id: number | null;
  forked_from: string | null;
  forked_at_id: number | null;
  created_at_ms: number;
  updated_at_ms: number;
}

/** A conversation as a list renders it. */
export interface ConversationSummary {
  id: string;
  /** The stored title, or the opening user message truncated — an unnamed
   *  conversation is unfindable, and deriving one costs no model call. */
  title: string;
  harness: string;
  model: string | null;
  session_id: string | null;
  head_id: number | null;
  forked_from: string | null;
  /** Messages minted by *this* conversation. A fork starts at zero even though
   *  its thread is long, which is the honest number. */
  message_count: number;
  updated_at_ms: number;
}

/**
 * `GET /v1/conversations/main` — the pinned chat, and its live window.
 *
 * **`conversation` is `null` before anyone has spoken.** That is a state to
 * render, not a 404: the pinned row draws from first launch, the way the TUI's
 * does. Reading it also does not *create* it — the GET path uses
 * `pinned_conversation` rather than core's get-or-create `main_conversation`,
 * because a GET that creates is a GET a prefetcher can fire.
 */
export interface MainChat {
  conversation: Conversation | null;
  messages: Message[];
}

/**
 * Why the live window is due for a compaction, when it is.
 *
 * Not a boolean. Read from `api/src/conversations.rs` rather than from a
 * summary of it — a flag would say a compaction is owed without saying what
 * crossed the threshold, and `chars` is the number that makes it actionable.
 */
export interface CompactionDue {
  reason: string;
  chars: number;
}

/**
 * What `POST /v1/conversations/main/messages` answers with. `201` on success,
 * or `200` when an `Idempotency-Key` replay returns the original.
 */
export interface HandedOver {
  /** The run that was started. The full summary, so nothing needs re-fetching. */
  agent: AgentSummary;
  /**
   * The conversation it landed in. Returned rather than looked up again,
   * because resolving the pinned chat twice is two chances to disagree.
   */
  conversation_id: string;
  /**
   * `null` unless the live window has grown past a threshold. **Advisory** — the
   * turn still ran, so this must not be rendered as a failure.
   */
  compaction_due: CompactionDue | null;
}
