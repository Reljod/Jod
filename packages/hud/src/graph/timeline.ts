import type { AgentNode, World } from "../state/world";

/**
 * The tactical graph answers "what is happening right now". It cannot answer
 * "what did this run actually do", because a node that has moved on leaves no
 * trace. This derives the second view: one lane per agent, time on X.
 *
 * Pure, so the interesting cases — a tool still in flight, a span that started
 * before the window opened, an agent with no traffic at all — are testable
 * without a DOM.
 */

export interface Span {
  name: string;
  /** Fractions of the window, 0..1, already clipped to it. */
  from: number;
  to: number;
  isError: boolean;
  /** Still running: the bar should read as open-ended. */
  open: boolean;
  startedAt: number;
}

export interface Mark {
  at: number;
  kind: "message" | "error" | "finished" | "started";
  text: string;
}

export interface Lane {
  id: string;
  name: string;
  harness: string;
  status: string;
  spans: Span[];
  marks: Mark[];
}

export interface TimelineWindow {
  start: number;
  end: number;
  spanMs: number;
}

export function windowFor(now: number, spanMs: number): TimelineWindow {
  return { start: now - spanMs, end: now, spanMs };
}

function frac(t: number, w: TimelineWindow): number {
  return (t - w.start) / w.spanMs;
}

const clamp01 = (v: number) => (v < 0 ? 0 : v > 1 ? 1 : v);

/**
 * Build one lane per agent.
 *
 * A tool span that started before the window is clipped, not dropped — a
 * `cargo test` that has been running for two minutes is precisely the thing
 * worth seeing, and dropping it would make the busiest agent look idle.
 */
export function buildLanes(
  world: World,
  ids: string[],
  w: TimelineWindow,
): Lane[] {
  const lanes: Lane[] = [];

  for (const id of ids) {
    const node = world.agents.get(id);
    if (!node) continue;
    lanes.push({
      id,
      name: node.summary.name,
      harness: node.summary.harness,
      status: node.summary.status,
      spans: spansFor(node, w),
      marks: marksFor(world, id, w),
    });
  }
  return lanes;
}

function spansFor(node: AgentNode, w: TimelineWindow): Span[] {
  const out: Span[] = [];
  for (const t of node.tools) {
    const end = t.endedAt ?? w.end;
    if (end < w.start || t.startedAt > w.end) continue; // wholly outside
    out.push({
      name: t.name,
      from: clamp01(frac(t.startedAt, w)),
      to: clamp01(frac(end, w)),
      isError: t.isError,
      open: t.endedAt === null,
      startedAt: t.startedAt,
    });
  }
  return out;
}

function marksFor(world: World, agentId: string, w: TimelineWindow): Mark[] {
  const out: Mark[] = [];
  for (const f of world.feed) {
    if (f.agentId !== agentId) continue;
    if (f.at < w.start || f.at > w.end) continue;
    if (f.kind === "message") out.push({ at: clamp01(frac(f.at, w)), kind: "message", text: f.text });
    else if (f.kind === "error") out.push({ at: clamp01(frac(f.at, w)), kind: "error", text: f.text });
    else if (f.kind === "finished")
      out.push({ at: clamp01(frac(f.at, w)), kind: "finished", text: f.text });
    else if (f.kind === "started")
      out.push({ at: clamp01(frac(f.at, w)), kind: "started", text: f.text });
  }
  return out;
}

/** Evenly spaced gridlines, labelled in seconds before now. */
export function ticks(w: TimelineWindow, count = 6): { at: number; label: string }[] {
  const out: { at: number; label: string }[] = [];
  for (let i = 0; i <= count; i++) {
    const at = i / count;
    const secondsAgo = Math.round((w.spanMs * (1 - at)) / 1000);
    out.push({ at, label: secondsAgo === 0 ? "now" : `-${secondsAgo}s` });
  }
  return out;
}
