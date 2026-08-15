import { describe, expect, it } from "vitest";

import { hideUnder } from "../src/components/Fleet";
import { SimTransport } from "../src/transport/sim";
import { fleetKey, type FleetNode, type FleetNodeKind } from "../src/types";

/**
 * A row of the fleet tree. Only the fields the collapse walk reads are
 * meaningful; the rest exist because the type requires them.
 */
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

/**
 * The shape `Store::forest_of` produces: document order, each row directly
 * below the one that owns it, nesting expressed only by `depth`.
 *
 *   work-a
 *     session-a          ← has a run and a nested session under it
 *       run-1
 *       session-b
 *         run-2
 *   work-b
 *     run-3
 */
function forest(): FleetNode[] {
  return [
    node("work", "work-a", 0),
    node("session", "session-a", 1),
    node("run", "run-1", 2),
    node("session", "session-b", 2),
    node("run", "run-2", 3),
    node("work", "work-b", 0),
    node("run", "run-3", 1),
  ];
}

const ids = (nodes: FleetNode[]) => nodes.map((n) => n.id.id);

describe("hideUnder", () => {
  it("shows the whole forest when nothing is collapsed", () => {
    expect(ids(hideUnder(forest(), new Set()))).toEqual([
      "work-a",
      "session-a",
      "run-1",
      "session-b",
      "run-2",
      "work-b",
      "run-3",
    ]);
  });

  it("keeps a collapsed row and drops only its subtree", () => {
    // The collapsed row itself must survive — it is the thing you click to
    // expand again.
    const collapsed = new Set([fleetKey({ kind_tag: "session", id: "session-a" })]);
    expect(ids(hideUnder(forest(), collapsed))).toEqual([
      "work-a",
      "session-a",
      "work-b",
      "run-3",
    ]);
  });

  it("drops a whole work without touching its sibling", () => {
    const collapsed = new Set([fleetKey({ kind_tag: "work", id: "work-a" })]);
    expect(ids(hideUnder(forest(), collapsed))).toEqual(["work-a", "work-b", "run-3"]);
  });

  it("collapses several subtrees at once", () => {
    const collapsed = new Set([
      fleetKey({ kind_tag: "session", id: "session-b" }),
      fleetKey({ kind_tag: "work", id: "work-b" }),
    ]);
    expect(ids(hideUnder(forest(), collapsed))).toEqual([
      "work-a",
      "session-a",
      "run-1",
      "session-b",
      "work-b",
    ]);
  });

  it("ignores a collapsed row buried inside another collapsed one", () => {
    // The regression a `parent`-link filter gets wrong: `session-b` is already
    // hidden under `session-a`, and re-entering the hiding state at its deeper
    // depth would wrongly un-hide `work-b`, which is shallower.
    const collapsed = new Set([
      fleetKey({ kind_tag: "session", id: "session-a" }),
      fleetKey({ kind_tag: "session", id: "session-b" }),
    ]);
    expect(ids(hideUnder(forest(), collapsed))).toEqual([
      "work-a",
      "session-a",
      "work-b",
      "run-3",
    ]);
  });

  it("is a no-op on an empty forest", () => {
    expect(hideUnder([], new Set([fleetKey({ kind_tag: "work", id: "gone" })]))).toEqual([]);
  });
});

/**
 * Both drivers satisfy `Transport`, so a panel written against one must work
 * against the other. The simulation is what the HUD falls back to with no
 * orchestrator, and a fleet panel that threw there would take the page down.
 */
describe("the simulation driver's fleet", () => {
  /**
   * A sim with agents in it.
   *
   * `start()` staggers its blueprints onto timers so the graph assembles rather
   * than popping into place, which means a freshly constructed driver has an
   * empty fleet. Spawning directly is the synchronous way in, and it is the
   * same call the command palette makes.
   */
  async function populated(): Promise<SimTransport> {
    const sim = new SimTransport("test");
    await sim.spawn({
      name: "one",
      harness: "claude_code",
      cwd: "/work/alpha",
      prompt: "first",
    });
    await sim.spawn({
      name: "two",
      harness: "claude_code",
      cwd: "/work/alpha",
      prompt: "second",
    });
    await sim.spawn({
      name: "three",
      harness: "open_code",
      cwd: "/work/beta",
      prompt: "third",
    });
    return sim;
  }

  it("answers with a tree rather than throwing", async () => {
    const nodes = await (await populated()).fleet();

    expect(nodes.some((n) => n.kind === "work")).toBe(true);
    expect(nodes.some((n) => n.kind === "run")).toBe(true);
  });

  it("is empty rather than broken before anything has launched", async () => {
    // The first paint, before the staggered blueprints land. The panel must
    // draw "no work yet" here, not crash.
    expect(await new SimTransport("test").fleet()).toEqual([]);
  });

  it("puts every run under a work, never at the root", async () => {
    // The panel indents by `depth`; a run at depth 0 would render as a
    // top-level heading and the tree would read as a flat list.
    const nodes = await (await populated()).fleet();
    expect(nodes.filter((n) => n.kind === "run").length).toBe(3);
    for (const run of nodes.filter((n) => n.kind === "run")) {
      expect(run.depth).toBeGreaterThan(0);
      expect(run.parent).not.toBeNull();
    }
  });

  it("groups the two runs sharing a directory under one work", async () => {
    const nodes = await (await populated()).fleet();
    expect(nodes.filter((n) => n.kind === "work").length).toBe(2);
  });

  it("emits document order, each row at most one level deeper", async () => {
    // `hideUnder` depends on this ordering, so it is worth pinning on the
    // driver that produces it synthetically.
    const nodes = await (await populated()).fleet();
    expect(nodes[0].depth).toBe(0);
    for (let i = 1; i < nodes.length; i += 1) {
      expect(nodes[i].depth).toBeLessThanOrEqual(nodes[i - 1].depth + 1);
    }
  });
});
