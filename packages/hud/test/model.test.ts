import { describe, expect, it } from "vitest";
import { contentionLinks, engagementOf, isHotContention, massOf, rankForDisplay } from "../src/graph/model";
import { WorldStore, type AgentNode } from "../src/state/world";
import type { AgentStatus, AgentSummary, HarnessKind, Report } from "../src/types";

function node(
  id: string,
  cwd: string,
  status: AgentStatus = "running",
  over: Partial<AgentNode> = {},
): AgentNode {
  const summary = {
    id,
    name: id,
    harness: "claude_code" as HarnessKind,
    harness_label: "Claude Code",
    status,
    cwd,
    model: null,
    permission: "ask",
    pid: 4242,
    pgid: 4242,
    process_alive: true,
    watch_command: `jod watch ${id}`,
    created_at_ms: 1000,
    session_id: null,
    usage: {},
    event_count: 0,
    last_message: null,
  } as AgentSummary;

  return {
    summary,
    phase: "thinking",
    heat: 0.5,
    lastEventAt: 1000,
    tools: [],
    inFlight: null,
    recentEventTimes: [],
    thought: null,
    errorCount: 0,
    events: [],
    eventsComplete: false,
    ...over,
  };
}

describe("contentionLinks", () => {
  it("links two live agents sharing a working directory", () => {
    const links = contentionLinks([node("a", "/repo/Jod"), node("b", "/repo/Jod")]);
    expect(links).toHaveLength(1);
    expect(isHotContention(links[0])).toBe(true);
  });

  it("does not link agents in different directories", () => {
    expect(contentionLinks([node("a", "/repo/Jod"), node("b", "/repo/other")])).toHaveLength(0);
  });

  it("links all pairs when three agents share one directory", () => {
    const links = contentionLinks([
      node("a", "/x"),
      node("b", "/x"),
      node("c", "/x"),
    ]);
    expect(links).toHaveLength(3);
  });

  it("weakens the link when only one end is still live", () => {
    const links = contentionLinks([node("a", "/x"), node("b", "/x", "completed")]);
    expect(links).toHaveLength(1);
    expect(isHotContention(links[0])).toBe(false);
  });

  it("drops the link entirely once both agents have finished", () => {
    // Two finished agents in one directory cannot collide with anything.
    expect(
      contentionLinks([node("a", "/x", "completed"), node("b", "/x", "failed")]),
    ).toHaveLength(0);
  });

  it("returns nothing for a single agent", () => {
    expect(contentionLinks([node("a", "/x")])).toHaveLength(0);
  });
});

describe("engagementOf", () => {
  it("releases a finished agent to the rim", () => {
    expect(engagementOf(node("a", "/x", "completed"), Date.now())).toBe(0);
  });

  it("rates an agent mid-tool-call above one that has gone idle", () => {
    const acting = engagementOf(node("a", "/x", "running", { phase: "acting", heat: 0.9 }), 0);
    const idle = engagementOf(node("b", "/x", "running", { phase: "idle", heat: 0.02 }), 0);
    expect(acting).toBeGreaterThan(idle);
  });

  it("stays within 0..1", () => {
    const hot = engagementOf(
      node("a", "/x", "running", { phase: "acting", heat: 1, recentEventTimes: Array(60).fill(Date.now()) }),
      Date.now(),
    );
    expect(hot).toBeGreaterThanOrEqual(0);
    expect(hot).toBeLessThanOrEqual(1);
  });
});

describe("massOf", () => {
  it("grows with token burn but stays bounded", () => {
    const light = massOf(node("a", "/x"));
    const heavy = massOf(
      node("b", "/x", "running", {
        summary: { ...node("b", "/x").summary, usage: { input_tokens: 2_000_000 } },
      }),
    );
    expect(heavy).toBeGreaterThan(light);
    expect(heavy).toBeLessThan(4);
  });
});

describe("rankForDisplay", () => {
  function world(nodes: AgentNode[]) {
    const store = new WorldStore();
    const report: Report = {
      running: 0, completed: 0, failed: 0, killed: 0, total_cost_usd: 0,
      agents: nodes.map((n) => n.summary),
    };
    store.setReport(report);
    // Re-apply the richer test fixtures over the freshly created nodes.
    for (const n of nodes) store.world.agents.set(n.summary.id, n);
    return store.world;
  }

  it("returns everything when under budget, hiding nothing", () => {
    const r = rankForDisplay(world([node("a", "/x"), node("b", "/y")]), 10);
    expect(r.visible).toHaveLength(2);
    expect(r.hidden).toBe(0);
  });

  it("prefers live agents over finished ones when over budget", () => {
    const nodes = [
      node("done1", "/x", "completed"),
      node("done2", "/x", "completed"),
      node("live", "/y", "running"),
    ];
    const r = rankForDisplay(world(nodes), 1);
    expect(r.visible).toEqual(["live"]);
    expect(r.hidden).toBe(2);
  });

  it("reports the hidden count honestly rather than truncating silently", () => {
    const nodes = Array.from({ length: 30 }, (_, i) => node(`n${i}`, "/x", "completed"));
    const r = rankForDisplay(world(nodes), 8);
    expect(r.visible).toHaveLength(8);
    expect(r.hidden).toBe(22);
    expect(r.visible.length + r.hidden).toBe(30);
  });
});
