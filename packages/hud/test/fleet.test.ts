import { describe, expect, it } from "vitest";

import { deletable, hint, hideUnder, isLiveRow, openable } from "../src/components/Fleet";
import { SimTransport } from "../src/transport/sim";
import { fleetKey, tiersOf, type FleetNode, type FleetNodeKind } from "../src/types";

/**
 * A row of the fleet tree. Only the fields the collapse walk reads are
 * meaningful; the rest exist because the type requires them.
 */
function node(
  kind: FleetNodeKind,
  id: string,
  depth: number,
  parent: FleetNode | null = null,
): FleetNode {
  return {
    id: { kind_tag: kind, id },
    parent: parent ? parent.id : null,
    kind,
    depth,
    label: id,
    summary: "",
    running: false,
    status: kind === "run" ? "completed" : null,
    stalled_for_ms: null,
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
 * The whole chain of command, as `Store::forest_of` emits it.
 *
 *   jod                        ← main, and his own run under him
 *     jod-run
 *   tetris                     ← project: a heading, holding no rank
 *     manager                  ← the row that owns the repository
 *       mgr-run
 *     port the parser          ← work
 *       lead                   ← session
 *         eng-run
 */
function chain(): FleetNode[] {
  const main = node("main", "main-conv", 0);
  const mainRun = node("run", "jod-run", 1, main);
  const project = node("project", "tetris", 0);
  const manager = node("manager", "mgr-conv", 1, project);
  const managerRun = node("run", "mgr-run", 2, manager);
  const work = node("work", "work-a", 1, project);
  const session = node("session", "lead", 2, work);
  const engineerRun = node("run", "eng-run", 3, session);
  return [main, mainRun, project, manager, managerRun, work, session, engineerRun];
}

describe("tiersOf", () => {
  it("ranks each row by who owns it", () => {
    const { row } = tiersOf(chain());
    expect(row.get("main:main-conv")).toBe("jod");
    expect(row.get("manager:mgr-conv")).toBe("manager");
    expect(row.get("work:work-a")).toBe("engineer");
    expect(row.get("session:lead")).toBe("engineer");
  });

  it("gives a run the rank of whatever owns it", () => {
    // The point of the whole exercise: three runs, three ranks, and nothing
    // about a run itself says which. Only the tree does.
    const { run } = tiersOf(chain());
    expect(run.get("jod-run")).toBe("jod");
    expect(run.get("mgr-run")).toBe("manager");
    expect(run.get("eng-run")).toBe("engineer");
  });

  it("leaves a project unranked — it is the repository, not a party in it", () => {
    expect(tiersOf(chain()).row.has("project:tetris")).toBe(false);
  });

  it("ranks a loose work and its runs as engineers", () => {
    // A work opened before projects were recorded sits at the top level with no
    // project above it. It is still somebody doing the work.
    const work = node("work", "work-a", 0);
    const session = node("session", "lead", 1, work);
    const run = node("run", "run-1", 2, session);
    const tiers = tiersOf([work, session, run]);
    expect(tiers.row.get("work:work-a")).toBe("engineer");
    expect(tiers.run.get("run-1")).toBe("engineer");
  });

  it("leaves a row whose parent link is missing unranked, rather than guessing", () => {
    // Rank is read off `parent`, and `Store::forest_of` sets it on every row
    // below the top level. A row without one is a forest that did not come from
    // there, and inventing a rank for it would draw a confident colour over an
    // answer nobody knows.
    const orphan = node("run", "run-1", 2);
    expect(tiersOf([orphan]).run.has("run-1")).toBe(false);
  });

  it("is empty rather than broken on an empty forest", () => {
    const { row, run } = tiersOf([]);
    expect(row.size).toBe(0);
    expect(run.size).toBe(0);
  });
});

describe("openable", () => {
  it("opens a manager on the run beneath it", () => {
    // The bug this whole change exists for. The manager row was a permanent
    // leaf, so this returned null and the button rendered disabled — a row that
    // was visibly there and could not be clicked.
    const all = chain();
    const manager = all.find((n) => n.kind === "manager")!;
    expect(openable(all, manager)).toBe("mgr-run");
  });

  it("opens jod on his own run and not on a manager's", () => {
    const all = chain();
    const main = all.find((n) => n.kind === "main")!;
    expect(openable(all, main)).toBe("jod-run");
  });

  it("does not reach past a row's own subtree", () => {
    // `mgr-run` is the last run under the manager; `eng-run` lives under the
    // work beside it and must not be what clicking the manager opens.
    const all = chain();
    const project = all.find((n) => n.kind === "project")!;
    expect(openable(all, project)).toBe("eng-run");
  });

  it("is null for a manager nobody has asked anything", () => {
    const project = node("project", "tetris", 0);
    const manager = node("manager", "mgr-conv", 1, project);
    manager.has_children = false;
    expect(openable([project, manager], manager)).toBeNull();
  });

  it("a run is itself", () => {
    const all = chain();
    const run = all.find((n) => n.id.id === "eng-run")!;
    expect(openable(all, run)).toBe("eng-run");
  });
});

describe("hint", () => {
  it("says why a manager with no run cannot be opened", () => {
    const manager = node("manager", "mgr-conv", 1);
    expect(hint(manager, null)).toMatch(/not been asked anything/);
  });

  it("shows what the row last said once there is something to open", () => {
    const manager = node("manager", "mgr-conv", 1);
    manager.summary = "Bash…";
    expect(hint(manager, "mgr-run")).toBe("Bash…");
  });
});

describe("deletable", () => {
  it("covers exactly the three kinds the delete can route", () => {
    // Pinned against `App.deleteFleetRows`'s switch. When these disagree, a
    // selected row is silently dropped and the delete reports success over
    // having done nothing — which is how it behaved before.
    const kinds = chain().filter(deletable).map((n) => n.kind);
    expect(new Set(kinds)).toEqual(new Set(["run", "work", "session"]));
  });

  it("refuses jod, a project and a manager", () => {
    for (const kind of ["main", "project", "manager"] as const) {
      expect(deletable(node(kind, "x", 0))).toBe(false);
    }
  });
});

describe("isLiveRow", () => {
  it("believes the event stream about a run the tree has not caught up with", () => {
    // The fleet is a four-second poll. A run that started 200ms ago is running,
    // and a panel that waits for the next query to say so is the panel somebody
    // is complaining looks asleep.
    const run = node("run", "eng-run", 1);
    expect(isLiveRow(run, new Set())).toBe(false);
    expect(isLiveRow(run, new Set(["eng-run"]))).toBe(true);
  });

  it("takes the tree's word for every row that is not a run", () => {
    // A closed work deliberately stops claiming to be running even with
    // something alive under it, and the roster cannot see that.
    const work = node("work", "work-a", 0);
    expect(isLiveRow(work, new Set(["work-a"]))).toBe(false);
    work.running = true;
    expect(isLiveRow(work, new Set())).toBe(true);
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
