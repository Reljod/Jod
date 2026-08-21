import type { AgentNode, Phase, World } from "../state/world";
import { eventRate } from "../state/world";
import { totalTokens } from "../types";
import type { Body, Link } from "./physics";
import { clamp01, seedPosition } from "./physics";

/**
 * How engaged an agent is, 0..1 — the quantity that decides how close to the
 * core it sits. Heat carries the recent event burst; phase carries what it is
 * doing; a finished agent is released entirely and drifts to the rim.
 */
export function engagementOf(node: AgentNode, now: number): number {
  if (node.summary.status !== "running") return 0;
  const phaseWeight: Record<Phase, number> = {
    booting: 0.7,
    thinking: 0.6,
    acting: 0.95,
    speaking: 0.8,
    idle: 0.15,
    done: 0,
    failed: 0,
  };
  const rate = clamp01(eventRate(node, now) / 1.2);
  return clamp01(node.heat * 0.45 + phaseWeight[node.phase] * 0.4 + rate * 0.15);
}

/**
 * Node mass, from cumulative token burn. A long-running agent that has spent
 * real money becomes heavy and anchors the layout; a fresh one is nimble.
 */
export function massOf(node: AgentNode): number {
  const tokens = totalTokens(node.summary.usage);
  return 1 + Math.log10(1 + tokens / 1000) * 0.55;
}

/**
 * Agents that share a working directory.
 *
 * This repo's charter says teammates share one checkout, so one owner per path.
 * Two live agents in the same `cwd` is therefore not a curiosity — it is the
 * failure mode the charter exists to prevent, and the reason these edges are
 * drawn hot rather than as neutral structure.
 */
export function contentionLinks(nodes: AgentNode[]): Link[] {
  const byCwd = new Map<string, AgentNode[]>();
  for (const n of nodes) {
    const list = byCwd.get(n.summary.cwd) ?? [];
    list.push(n);
    byCwd.set(n.summary.cwd, list);
  }

  const links: Link[] = [];
  for (const group of byCwd.values()) {
    if (group.length < 2) continue;
    for (let i = 0; i < group.length; i++) {
      for (let j = i + 1; j < group.length; j++) {
        const a = group[i];
        const b = group[j];
        // Two finished agents in one directory cannot collide with anything.
        const bothLive =
          a.summary.status === "running" && b.summary.status === "running";
        const eitherLive =
          a.summary.status === "running" || b.summary.status === "running";
        if (!eitherLive) continue;
        links.push({
          a: a.summary.id,
          b: b.summary.id,
          weight: bothLive ? 1 : 0.35,
        });
      }
    }
  }
  return links;
}

/** True when both ends are live — the case worth alarming about. */
export function isHotContention(link: Link): boolean {
  return link.weight >= 1;
}

export interface DisplayOptions {
  /**
   * Plot only the sessions that are running.
   *
   * What the tactical view asks for. The graph is a picture of *now* — heat,
   * phase, contention over a shared directory — and every one of those is zero
   * for a finished run, so a fleet with four live agents and three hundred
   * completed ones drew four moving nodes in a field of three hundred inert
   * ones. The history is not lost; it is in the sessions list, the fleet tree
   * and the timeline, which are the surfaces that are about the past.
   */
  onlyRunning?: boolean;
}

/**
 * Which agents actually get drawn, most important first.
 *
 * A restarted daemon rehydrates its whole run history, so the roster is not
 * guaranteed to be small. Rendering four hundred nodes would produce a hairball
 * that says less than twenty well-placed ones, so the graph takes a budget and
 * the UI states plainly how many it left out — a silent truncation would read
 * as "this is the whole fleet".
 *
 * `hidden` counts everything left out, whether by the budget or by
 * `onlyRunning`. One number, because the caller draws one chip, and because a
 * filter whose effect is not counted is a filter that looks like a bug.
 */
export function rankForDisplay(
  world: World,
  limit: number,
  options: DisplayOptions = {},
): {
  visible: string[];
  hidden: number;
} {
  const nodes: AgentNode[] = [];
  let excluded = 0;
  for (const id of world.order) {
    const n = world.agents.get(id);
    if (!n) continue;
    if (options.onlyRunning && n.summary.status !== "running") {
      excluded += 1;
      continue;
    }
    nodes.push(n);
  }
  if (nodes.length <= limit) {
    return { visible: nodes.map((n) => n.summary.id), hidden: excluded };
  }

  const score = (n: AgentNode): number => {
    // Live agents always outrank finished ones; within each, recency and heat.
    const live = n.summary.status === "running" ? 1e12 : 0;
    const faulted = n.summary.status === "failed" ? 1e11 : 0;
    return live + faulted + n.summary.created_at_ms + n.heat * 1e6;
  };

  const sorted = [...nodes].sort((a, b) => score(b) - score(a));
  return {
    visible: sorted.slice(0, limit).map((n) => n.summary.id),
    hidden: sorted.length - limit + excluded,
  };
}

/**
 * Reconcile the body list against the world: add bodies for new agents, drop
 * them for vanished ones, and refresh the physical properties of the rest.
 * Positions are preserved across calls so the layout never resets.
 */
export function syncBodies(
  bodies: Map<string, Body>,
  world: World,
  now: number,
  visible?: Set<string>,
): Body[] {
  const seen = new Set<string>();
  let index = 0;

  for (const id of world.order) {
    const node = world.agents.get(id);
    if (!node) continue;
    if (visible && !visible.has(id)) continue;
    seen.add(id);

    let body = bodies.get(id);
    if (!body) {
      const seed = seedPosition(index);
      body = {
        id,
        x: seed.x,
        y: seed.y,
        vx: 0,
        vy: 0,
        mass: 1,
        engagement: 0,
        group: node.summary.harness,
      };
      bodies.set(id, body);
    }
    body.mass = massOf(node);
    body.engagement = engagementOf(node, now);
    body.group = node.summary.harness;
    index += 1;
  }

  for (const id of [...bodies.keys()]) {
    if (!seen.has(id)) bodies.delete(id);
  }

  return [...bodies.values()];
}
