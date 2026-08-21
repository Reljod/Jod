import { describe, expect, it } from "vitest";

import { openable } from "../src/components/Fleet";
import { nextAll, pruneSelection } from "../src/hooks/useSelection";
import { summariseFailures } from "../src/hooks/useJod";
import { rankForDisplay } from "../src/graph/model";
import { WorldStore } from "../src/state/world";
import { SimTransport } from "../src/transport/sim";
import type {
  AgentEnvelope,
  AgentStatus,
  AgentSummary,
  FleetNode,
  FleetNodeKind,
  Report,
} from "../src/types";

// ─── fixtures ────────────────────────────────────────────────────────────────

function summary(id: string, status: AgentStatus = "running"): AgentSummary {
  return {
    id,
    name: id,
    harness: "claude_code",
    harness_label: "Claude Code",
    status,
    cwd: "/work",
    model: null,
    permission: "ask",
    pid: null,
    pgid: null,
    process_alive: status === "running",
    watch_command: `jod watch ${id}`,
    created_at_ms: 1,
    session_id: `sess-${id}`,
    usage: {},
    event_count: 0,
    last_message: null,
  };
}

function report(agents: AgentSummary[]): Report {
  return {
    running: agents.filter((a) => a.status === "running").length,
    completed: agents.filter((a) => a.status === "completed").length,
    failed: 0,
    killed: 0,
    total_cost_usd: 0,
    agents,
  };
}

function node(kind: FleetNodeKind, id: string, depth: number): FleetNode {
  return {
    id: { kind_tag: kind, id },
    parent: null,
    kind,
    depth,
    label: id,
    summary: "",
    running: false,
    cards: 0,
    blocked: 0,
    colour: "cyan",
    has_children: kind !== "run",
  };
}

// ─── which row a click opens ─────────────────────────────────────────────────

/**
 *   work-a
 *     session-a
 *       run-1
 *       session-b
 *         run-2
 *   work-b            ← nothing under it at all
 */
function forest(): FleetNode[] {
  return [
    node("work", "work-a", 0),
    node("session", "session-a", 1),
    node("run", "run-1", 2),
    node("session", "session-b", 2),
    node("run", "run-2", 3),
    node("work", "work-b", 0),
  ];
}

describe("openable", () => {
  it("opens a run row as itself", () => {
    const all = forest();
    expect(openable(all, all[2])).toBe("run-1");
  });

  it("opens the newest run beneath a session", () => {
    // `session-a` has `run-1` directly and `run-2` through a nested session.
    // Newest is last in document order, which is `run-2`.
    const all = forest();
    expect(openable(all, all[1])).toBe("run-2");
  });

  it("opens the newest run beneath a work", () => {
    const all = forest();
    expect(openable(all, all[0])).toBe("run-2");
  });

  /** A row that opens nothing is disabled, not silently inert. */
  it("has nothing to open for a row with no runs under it", () => {
    const all = forest();
    expect(openable(all, all[5])).toBeNull();
  });

  /**
   * The subtree walk stops at the next row at or above its own depth. Without
   * that, a work would reach into its *sibling's* runs and clicking one heading
   * would open a run belonging to another piece of work entirely.
   */
  it("never reaches past its own subtree", () => {
    const all = [
      node("work", "work-a", 0),
      node("work", "work-b", 0),
      node("run", "run-b", 1),
    ];
    expect(openable(all, all[0])).toBeNull();
    expect(openable(all, all[1])).toBe("run-b");
  });
});

// ─── selecting across a list that moves ──────────────────────────────────────

describe("pruneSelection", () => {
  it("drops ids that are no longer on offer", () => {
    const kept = pruneSelection(new Set(["a", "b", "c"]), new Set(["a", "c"]));
    expect([...kept].sort()).toEqual(["a", "c"]);
  });

  /**
   * Identity, not contents. This runs in an effect that sets state, so
   * returning a fresh Set of the same ids re-renders, re-runs the effect and
   * loops for ever.
   */
  it("returns the very same set when nothing left the list", () => {
    const chosen = new Set(["a", "b"]);
    expect(pruneSelection(chosen, new Set(["a", "b", "c"]))).toBe(chosen);
  });

  it("returns the same empty set rather than a new one", () => {
    const chosen = new Set<string>();
    expect(pruneSelection(chosen, new Set(["a"]))).toBe(chosen);
  });
});

describe("nextAll", () => {
  it("selects everything on offer", () => {
    expect([...nextAll(new Set(), new Set(["a", "b"]))].sort()).toEqual(["a", "b"]);
  });

  it("clears when everything on offer is already selected", () => {
    expect(nextAll(new Set(["a", "b"]), new Set(["a", "b"])).size).toBe(0);
  });

  /**
   * Judged against what is on offer. A stale id held from a previous list must
   * not make an unselected list read as fully selected.
   */
  it("ignores held ids that are no longer on offer", () => {
    const next = nextAll(new Set(["gone"]), new Set(["a", "b"]));
    expect([...next].sort()).toEqual(["a", "b"]);
  });

  it("does nothing with nothing on offer", () => {
    expect(nextAll(new Set(["a"]), new Set()).size).toBe(0);
  });
});

// ─── reporting what a bulk delete refused ────────────────────────────────────

describe("summariseFailures", () => {
  it("says nothing when nothing failed", () => {
    expect(summariseFailures([])).toBeNull();
  });

  it("gives one refusal in full", () => {
    expect(summariseFailures([{ reason: "still running" }])).toBe("still running");
  });

  /**
   * Refusals in a bulk delete are nearly always the same refusal repeated, so
   * one in full plus a count beats a list of five identical sentences.
   */
  it("gives the first refusal and counts the rest", () => {
    const many = [
      { reason: "run `a` is still running" },
      { reason: "run `b` is still running" },
      { reason: "run `c` is still running" },
    ];
    expect(summariseFailures(many)).toBe("run `a` is still running (and 2 more)");
  });
});

// ─── forgetting a session ────────────────────────────────────────────────────

function envelope(agentId: string, seq: number): AgentEnvelope {
  return { kind: "message", text: "hello", agent_id: agentId, at_ms: 10, seq };
}

describe("WorldStore.forget", () => {
  it("removes the agent, its order entry, and its feed lines", () => {
    const store = new WorldStore();
    store.setReport(report([summary("a"), summary("b")]));
    store.ingest(envelope("a", 0));
    store.ingest(envelope("b", 0));

    expect(store.forget("a")).toBe(true);
    expect(store.world.agents.has("a")).toBe(false);
    expect(store.world.order).toEqual(["b"]);
    expect(store.world.feed.every((f) => f.agentId !== "a")).toBe(true);
    expect(store.world.pulses.every((p) => p.agentId !== "a")).toBe(true);

    // The neighbour is untouched.
    expect(store.world.agents.has("b")).toBe(true);
    expect(store.world.feed.some((f) => f.agentId === "b")).toBe(true);
  });

  it("says so when there was nothing to forget", () => {
    const store = new WorldStore();
    expect(store.forget("never-existed")).toBe(false);
  });

  /**
   * The tally is the server's. Guessing at it here would put a number on screen
   * that the next roster refresh silently corrects.
   */
  it("leaves the report alone", () => {
    const store = new WorldStore();
    store.setReport(report([summary("a"), summary("b")]));
    store.forget("a");
    expect(store.world.report.running).toBe(2);
  });
});

// ─── what the tactical view plots ────────────────────────────────────────────

describe("rankForDisplay with onlyRunning", () => {
  function worldOf(...agents: AgentSummary[]) {
    const store = new WorldStore();
    store.setReport(report(agents));
    return store.world;
  }

  it("plots only the running sessions", () => {
    const world = worldOf(summary("live"), summary("old", "completed"), summary("live2"));
    const { visible } = rankForDisplay(world, 48, { onlyRunning: true });
    expect(visible.sort()).toEqual(["live", "live2"]);
  });

  /** Never a silent filter: what was left out is counted, so the chip can say. */
  it("counts what the filter removed", () => {
    const world = worldOf(summary("live"), summary("a", "completed"), summary("b", "killed"));
    expect(rankForDisplay(world, 48, { onlyRunning: true }).hidden).toBe(2);
  });

  it("adds the filtered-out ones to what the budget dropped", () => {
    const world = worldOf(
      summary("l1"),
      summary("l2"),
      summary("l3"),
      summary("done", "completed"),
    );
    // Budget of two takes one live agent, and `done` was filtered — three
    // plotted-out things in total, reported as one number.
    expect(rankForDisplay(world, 2, { onlyRunning: true }).hidden).toBe(2);
  });

  it("is unchanged without the option, so every other caller sees the fleet", () => {
    const world = worldOf(summary("live"), summary("old", "completed"));
    const { visible, hidden } = rankForDisplay(world, 48);
    expect(visible.sort()).toEqual(["live", "old"]);
    expect(hidden).toBe(0);
  });
});

// ─── the simulation driver refuses what the API refuses ──────────────────────

describe("the simulation driver's deletes", () => {
  async function populated(): Promise<SimTransport> {
    const sim = new SimTransport("test");
    await sim.spawn({ name: "one", harness: "claude_code", cwd: "/work/alpha", prompt: "a" });
    await sim.spawn({ name: "two", harness: "claude_code", cwd: "/work/alpha", prompt: "b" });
    return sim;
  }

  it("refuses to delete a run that is still going", async () => {
    const sim = await populated();
    const [first] = (await sim.fleet()).filter((n) => n.kind === "run");
    await expect(sim.deleteRun(first.id.id)).rejects.toThrow(/still running/);
  });

  it("deletes a finished run and drops it from the fleet", async () => {
    const sim = await populated();
    const [first] = (await sim.fleet()).filter((n) => n.kind === "run");
    await sim.kill(first.id.id);
    await sim.deleteRun(first.id.id);

    const left = (await sim.fleet()).filter((n) => n.kind === "run").map((n) => n.id.id);
    expect(left).not.toContain(first.id.id);
  });

  it("refuses a work while anything in it is running, and says how many", async () => {
    const sim = await populated();
    const outcome = await sim.deleteWork("/work/alpha");
    expect(outcome.deleted).toBe(false);
    expect(outcome.detail).toContain("2");
  });

  it("takes every session in a work once they have all stopped", async () => {
    const sim = await populated();
    for (const run of (await sim.fleet()).filter((n) => n.kind === "run")) {
      await sim.kill(run.id.id);
    }
    const outcome = await sim.deleteWork("/work/alpha");
    expect(outcome.deleted).toBe(true);
    expect(outcome.doomed.sessions).toBe(2);
    expect(await sim.fleet()).toEqual([]);
  });

  it("throws for a work that was never there", async () => {
    const sim = await populated();
    await expect(sim.deleteWork("/nowhere")).rejects.toThrow(/no work/);
  });
});
