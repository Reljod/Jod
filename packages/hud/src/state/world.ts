import type {
  AgentEnvelope,
  AgentEventKind,
  AgentStatus,
  AgentSummary,
  Report,
} from "../types";
import type { LinkState } from "../transport";
import { EMPTY_REPORT } from "../transport";

/**
 * What an agent is doing *right now*, inferred from its event stream.
 *
 * The roster only tells you an agent is "running". That is the least
 * interesting thing about it. This is the distinction the tactical view is
 * built to show: two running agents look nothing alike if one is blocked on a
 * 40-second `cargo test` and the other is streaming prose.
 */
export type Phase =
  | "booting"
  | "thinking"
  | "acting" // a tool call is in flight
  | "speaking"
  | "idle" // running, but nothing has happened for a while
  | "done"
  | "failed";

export interface ToolTrace {
  name: string;
  startedAt: number;
  /** null while the call is still in flight. */
  endedAt: number | null;
  isError: boolean;
  summary: string | null;
  input: unknown;
}

export interface AgentNode {
  summary: AgentSummary;
  phase: Phase;
  /** 0..1, decays with time; drives glow, ring speed and node mass. */
  heat: number;
  lastEventAt: number;
  /** Most recent tool calls, newest last. Capped. */
  tools: ToolTrace[];
  /** The tool currently in flight, if any. */
  inFlight: ToolTrace | null;
  /** Rolling count for the throughput sparkline. */
  recentEventTimes: number[];
  /** Latest thinking text — the "inner voice" line in the dossier. */
  thought: string | null;
  errorCount: number;
  /**
   * This agent's own events, in `seq` order — what the trajectory view reads.
   *
   * Kept per agent rather than taken from `feed`, which is a global ring of the
   * last few hundred *lines* across the whole fleet: it holds no structured
   * input, and one chatty agent evicts another's history. Retaining here is
   * also what makes the trajectory tail live without polling, since every
   * envelope already passes through `ingest`.
   */
  events: AgentEnvelope[];
  /**
   * Whether everything from seq 0 is present. False until a backfill lands, so
   * the view can tell "this run started before the page did" from "this run has
   * done nothing yet" — the difference between fetching history and showing an
   * empty timeline that looks like a bug.
   */
  eventsComplete: boolean;
}

export interface FeedItem {
  id: string;
  agentId: string;
  agentName: string;
  kind: AgentEventKind;
  at: number;
  seq: number;
  text: string;
  isError: boolean;
}

/** A transient visual event: something to animate along an edge. */
export interface Pulse {
  id: number;
  agentId: string;
  kind: "out" | "in" | "error" | "speak";
  born: number;
  toolName?: string;
}

export interface World {
  agents: Map<string, AgentNode>;
  /** Insertion order, so the roster does not reshuffle every frame. */
  order: string[];
  feed: FeedItem[];
  pulses: Pulse[];
  report: Report;
  link: LinkState;
  /** Monotonic, bumped whenever anything structural changed. */
  revision: number;
}

const FEED_CAP = 300;
const TOOL_CAP = 24;
const PULSE_CAP = 400;
const IDLE_AFTER_MS = 6000;
/**
 * Per-agent retained history.
 *
 * Set from a measurement rather than a guess: across a real 36-run fleet the
 * *whole* history was 605 kB and the longest single run was 260 events / 298 kB
 * — about 1.1 kB an event, since a `tool_call` carries its arguments whole.
 * This is ~6× the longest run seen, so it does not bite in practice, and it
 * bounds the case that would: a run streaming `delta` frames emits them by the
 * thousand, and a tab left open on a busy fleet must not grow without limit.
 * When it does bite, the trajectory counts what it dropped rather than quietly
 * starting mid-run.
 */
const EVENT_CAP = 1500;

export function emptyWorld(): World {
  return {
    agents: new Map(),
    order: [],
    feed: [],
    pulses: [],
    report: EMPTY_REPORT,
    link: { phase: "probing" },
    revision: 0,
  };
}

function newNode(summary: AgentSummary): AgentNode {
  return {
    summary,
    phase: "booting",
    heat: 0.6,
    lastEventAt: summary.created_at_ms,
    tools: [],
    inFlight: null,
    recentEventTimes: [],
    thought: null,
    errorCount: 0,
    events: [],
    // A run this client watched from its first event needs no backfill. Only
    // one already in flight when the roster arrived does, and `ingest` decides
    // which this is from the seq it first sees.
    eventsComplete: false,
  };
}

/** One line of human-readable text for any event kind. */
export function describe(env: AgentEnvelope): string {
  switch (env.kind) {
    case "started":
      return `session ${env.session_id ?? "?"} · ${env.model ?? "default model"}`;
    case "thinking":
      return env.text;
    case "progress":
      return env.thinking_tokens != null
        ? `thinking… ${env.thinking_tokens} tokens`
        : "thinking…";
    case "delta":
      return env.text;
    case "message":
      return env.text;
    case "tool_call":
      return `${env.name}(${previewInput(env.input)})`;
    case "tool_result":
      return `${env.name} → ${env.summary ?? (env.is_error ? "error" : "ok")}`;
    case "finished":
      return env.text ?? (env.is_error ? "failed" : "completed");
    case "raw":
      return env.line;
    case "session_lost":
      return `session ${env.session_id} is gone — the harness refused to resume it`;
    case "error":
      return env.message;
  }
}

function previewInput(input: unknown): string {
  if (input == null) return "";
  if (typeof input === "string") return truncate(input, 48);
  if (typeof input === "object") {
    const rec = input as Record<string, unknown>;
    const key = ["command", "file_path", "pattern", "query", "url"].find(
      (k) => typeof rec[k] === "string",
    );
    if (key) return truncate(String(rec[key]), 48);
    return truncate(JSON.stringify(input), 48);
  }
  return truncate(String(input), 48);
}

export function truncate(s: string, max: number): string {
  const flat = s.replace(/\s+/g, " ").trim();
  return flat.length <= max ? flat : `${flat.slice(0, max - 1)}…`;
}

export function statusRank(status: AgentStatus): number {
  return { running: 0, failed: 1, killed: 2, completed: 3 }[status];
}

/**
 * The single mutable world the whole HUD reads.
 *
 * React is deliberately not the owner here. Event bursts arrive faster than a
 * paint, and the canvas wants the freshest state every frame, not a snapshot
 * from the last commit. Panels subscribe on a throttle; the renderer reads
 * `world` directly.
 */
export class WorldStore {
  readonly world: World = emptyWorld();
  private listeners = new Set<() => void>();
  private pulseId = 0;
  private dirty = false;

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Notify panels. Called on a rAF throttle by the host, not per event. */
  flush(): void {
    if (!this.dirty) return;
    this.dirty = false;
    this.world.revision += 1;
    for (const fn of this.listeners) fn();
  }

  setLink(link: LinkState): void {
    this.world.link = link;
    this.dirty = true;
  }

  setReport(report: Report): void {
    const w = this.world;
    w.report = report;
    for (const summary of report.agents) {
      const existing = w.agents.get(summary.id);
      if (existing) {
        existing.summary = summary;
        // A roster that says the run is over overrides whatever the event
        // stream last inferred; the reverse is not true, since the stream is
        // ahead of the roster while a run is live.
        if (summary.status !== "running") {
          existing.phase = terminalPhase(summary.status);
        }
      } else {
        w.agents.set(summary.id, newNode(summary));
        w.order.push(summary.id);
      }
    }
    this.dirty = true;
  }

  ingest(env: AgentEnvelope): void {
    const w = this.world;
    const node = w.agents.get(env.agent_id);
    if (!node) return; // roster snapshot has not arrived yet; it will backfill

    // Retain before folding, and bail on a duplicate: the transport dedupes its
    // own stream, but a trajectory backfill is fetched outside it and legally
    // overlaps. Folding the same event twice would double-count the heat, the
    // event rate and the fault tally.
    if (!retain(node, env)) return;

    const now = env.at_ms;
    node.lastEventAt = now;
    node.heat = Math.min(1, node.heat + heatFor(env.kind));
    node.recentEventTimes.push(now);
    if (node.recentEventTimes.length > 120) node.recentEventTimes.shift();
    node.summary.event_count = Math.max(node.summary.event_count, env.seq);

    switch (env.kind) {
      case "started":
        node.phase = "thinking";
        if (env.model) node.summary.model = env.model;
        if (env.session_id) node.summary.session_id = env.session_id;
        break;
      case "thinking":
        node.phase = "thinking";
        node.thought = env.text;
        break;
      case "progress":
        // A tick carrying no content. It says one thing — the turn is still
        // reasoning — and the common path above has already refreshed
        // `lastEventAt`, which is what stops `tick` calling a nine-minute
        // think idle.
        node.phase = "thinking";
        break;
      case "delta":
        // A fragment of a block still being written. Deliberately does not
        // touch `last_message` or `thought`: the complete text arrives in the
        // `message`/`tool_call` that follows, and rendering the running
        // fragment as the finished turn would show a truncated one.
        break;
      case "message":
        node.phase = "speaking";
        node.summary.last_message = env.text;
        this.pulse(env.agent_id, "speak");
        break;
      case "tool_call": {
        node.phase = "acting";
        const trace: ToolTrace = {
          name: env.name,
          startedAt: now,
          endedAt: null,
          isError: false,
          summary: null,
          input: env.input,
        };
        node.tools.push(trace);
        if (node.tools.length > TOOL_CAP) node.tools.shift();
        node.inFlight = trace;
        this.pulse(env.agent_id, "out", env.name);
        break;
      }
      case "tool_result": {
        // Close the matching in-flight call; fall back to the newest open one.
        const open =
          node.inFlight && node.inFlight.name === env.name
            ? node.inFlight
            : [...node.tools].reverse().find((t) => t.endedAt === null && t.name === env.name);
        if (open) {
          open.endedAt = now;
          open.isError = env.is_error;
          open.summary = env.summary ?? null;
        }
        if (node.inFlight === open) node.inFlight = null;
        if (env.is_error) node.errorCount += 1;
        node.phase = "thinking";
        this.pulse(env.agent_id, env.is_error ? "error" : "in", env.name);
        break;
      }
      case "finished":
        node.phase = env.is_error ? "failed" : "done";
        node.summary.status = env.is_error ? "failed" : "completed";
        // The supervisor exits with the run, so a finished run has no live
        // process group. Under tmux the session outlived the agent and this
        // was the opposite claim.
        node.summary.process_alive = false;
        node.summary.usage = env.usage;
        node.inFlight = null;
        if (env.text) node.summary.last_message = env.text;
        break;
      case "session_lost":
      case "error":
        node.errorCount += 1;
        node.phase = node.summary.status === "running" ? "thinking" : node.phase;
        this.pulse(env.agent_id, "error");
        break;
      case "raw":
        break;
    }

    w.feed.push({
      id: `${env.agent_id}:${env.seq}`,
      agentId: env.agent_id,
      agentName: node.summary.name,
      kind: env.kind,
      at: now,
      seq: env.seq,
      text: describe(env),
      isError:
        (env.kind === "tool_result" && env.is_error) ||
        env.kind === "error" ||
        (env.kind === "finished" && env.is_error),
    });
    if (w.feed.length > FEED_CAP) w.feed.splice(0, w.feed.length - FEED_CAP);

    this.dirty = true;
  }

  /**
   * Fold a fetched history into an agent, oldest first.
   *
   * Routed through `ingest` rather than assigned, so backfilled events build
   * the same tool traces, tallies and phase the live ones do — the alternative
   * is a run whose derived state depends on whether you were watching when it
   * happened. `retain` drops the overlap.
   */
  backfill(agentId: string, envelopes: AgentEnvelope[]): void {
    const node = this.world.agents.get(agentId);
    if (!node) return;
    for (const env of [...envelopes].sort((a, b) => a.seq - b.seq)) this.ingest(env);
    // Set after the fold, and unconditionally: a run that has emitted nothing
    // yet backfills to an empty list, and that is a complete history of
    // nothing rather than a fetch to retry forever.
    node.eventsComplete = true;
    this.dirty = true;
  }

  /**
   * Drop an agent and everything the world holds about it.
   *
   * Called after a delete the server accepted, so the row disappears on the
   * click rather than on the next roster refresh — which for a deleted run
   * never comes, because the roster is rebuilt from what the daemon still has.
   *
   * The feed and the pulses go with it. A stream line naming an agent that is
   * no longer in `agents` renders as an untitled row and, worse, selecting it
   * would select nothing; a pulse animates along an edge whose node is gone.
   *
   * `report` is deliberately left alone. It is the server's tally, and the
   * server is the one that gets to change it — guessing here would put a
   * count on screen that the next refresh silently corrects.
   */
  forget(agentId: string): boolean {
    const w = this.world;
    if (!w.agents.delete(agentId)) return false;
    w.order = w.order.filter((id) => id !== agentId);
    w.feed = w.feed.filter((f) => f.agentId !== agentId);
    w.pulses = w.pulses.filter((p) => p.agentId !== agentId);
    this.dirty = true;
    return true;
  }

  private pulse(agentId: string, kind: Pulse["kind"], toolName?: string): void {
    const w = this.world;
    w.pulses.push({ id: this.pulseId++, agentId, kind, born: performance.now(), toolName });
    if (w.pulses.length > PULSE_CAP) w.pulses.splice(0, w.pulses.length - PULSE_CAP);
  }

  /**
   * Advance time-based state. Called once per frame by the renderer.
   * Heat decays so an agent that stops emitting visibly cools rather than
   * sitting at full brightness forever.
   */
  tick(nowMs: number, dtMs: number): void {
    for (const node of this.world.agents.values()) {
      const decay = Math.exp(-dtMs / 2600);
      node.heat *= decay;
      if (node.summary.status === "running") {
        if (nowMs - node.lastEventAt > IDLE_AFTER_MS && node.phase !== "acting") {
          node.phase = "idle";
        }
      } else if (node.phase !== "done" && node.phase !== "failed") {
        node.phase = terminalPhase(node.summary.status);
      }
    }
  }

  /** Drop pulses older than their animation window. */
  reapPulses(nowPerf: number, lifetimeMs: number): void {
    const w = this.world;
    let i = 0;
    while (i < w.pulses.length && nowPerf - w.pulses[i].born > lifetimeMs) i += 1;
    if (i > 0) w.pulses.splice(0, i);
  }
}

/**
 * Keep one envelope in the agent's transcript, in `seq` order.
 *
 * Returns false if it was already there, which is the caller's signal to fold
 * nothing. Live events append — the fast path, and the only one that runs at
 * event rate — while a backfill splices in behind them.
 */
function retain(node: AgentNode, env: AgentEnvelope): boolean {
  const log = node.events;
  const last = log.length > 0 ? log[log.length - 1] : null;

  if (last === null || env.seq > last.seq) {
    log.push(env);
  } else {
    let lo = 0;
    let hi = log.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (log[mid].seq < env.seq) lo = mid + 1;
      else hi = mid;
    }
    if (lo < log.length && log[lo].seq === env.seq) return false;
    log.splice(lo, 0, env);
  }

  // Witnessing seq 0 is as good as fetching it: seq starts at 0, so an agent
  // whose first event is 0 has been watched from its birth.
  if (env.seq === 0) node.eventsComplete = true;
  // Trim the oldest, never the newest: the tail is what a live view is showing.
  if (log.length > EVENT_CAP) log.splice(0, log.length - EVENT_CAP);
  return true;
}

function terminalPhase(status: AgentStatus): Phase {
  return status === "completed" ? "done" : "failed";
}

/**
 * Exhaustive over every kind core can emit, and it has to stay that way: a
 * missing arm returns `undefined`, `heat + undefined` is `NaN`, and a node with
 * `NaN` heat renders at a `NaN` radius — it vanishes. That is what a
 * `progress` or `delta` frame did to this function before the union carried
 * them, which made a *healthy* streaming run the one that disappeared.
 */
function heatFor(kind: AgentEventKind): number {
  switch (kind) {
    case "tool_call":
      return 0.34;
    case "tool_result":
      return 0.26;
    case "message":
      return 0.3;
    case "thinking":
      return 0.18;
    // Both are mid-turn liveness rather than content, so they keep an agent
    // warm without ever making it look as busy as one that produced something.
    case "progress":
      return 0.06;
    case "delta":
      return 0.08;
    case "error":
      return 0.5;
    case "session_lost":
      return 0.5;
    case "started":
      return 0.6;
    case "finished":
      return 0.45;
    case "raw":
      return 0.05;
  }
}

/** Events per second over the last window — the throughput readout. */
export function eventRate(node: AgentNode, now: number, windowMs = 10000): number {
  const cutoff = now - windowMs;
  let n = 0;
  for (let i = node.recentEventTimes.length - 1; i >= 0; i--) {
    if (node.recentEventTimes[i] < cutoff) break;
    n += 1;
  }
  return n / (windowMs / 1000);
}
