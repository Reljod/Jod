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
  };
}

/** One line of human-readable text for any event kind. */
export function describe(env: AgentEnvelope): string {
  switch (env.kind) {
    case "started":
      return `session ${env.session_id ?? "?"} · ${env.model ?? "default model"}`;
    case "thinking":
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
        node.summary.session_closed = true;
        node.summary.usage = env.usage;
        node.inFlight = null;
        if (env.text) node.summary.last_message = env.text;
        break;
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

function terminalPhase(status: AgentStatus): Phase {
  return status === "completed" ? "done" : "failed";
}

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
    case "error":
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
