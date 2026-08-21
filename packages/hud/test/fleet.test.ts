import { describe, expect, it } from "vitest";

import {
  deletable,
  hint,
  hideUnder,
  isAgentRow,
  isLiveRow,
  openable,
} from "../src/components/Fleet";
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
 * The whole chain of command, as the server hands it over: **folded**.
 *
 * `jod_core::tree::condense` has already run, so there are no work rows and no
 * run rows. What a run was is carried in `runOf`, keyed the way a row is keyed.
 *
 *   jod                        ← main; answers for jod-run
 *   tetris                     ← project: a heading, holding no rank
 *     manager                  ← owns the repository; answers for mgr-run
 *     lead                     ← an engineer; answers for eng-run
 */
function chain(): { nodes: FleetNode[]; runOf: Map<string, string> } {
  const main = node("main", "main-conv", 0);
  const project = node("project", "tetris", 0);
  const manager = node("manager", "mgr-conv", 1, project);
  const session = node("session", "lead", 1, project);
  main.has_children = false;
  manager.has_children = false;
  session.has_children = false;
  return {
    nodes: [main, project, manager, session],
    runOf: new Map([
      ["main:main-conv", "jod-run"],
      ["manager:mgr-conv", "mgr-run"],
      ["session:lead", "eng-run"],
    ]),
  };
}

describe("tiersOf", () => {
  it("ranks each row by who owns it", () => {
    const { row } = tiersOf(chain().nodes);
    expect(row.get("main:main-conv")).toBe("jod");
    expect(row.get("manager:mgr-conv")).toBe("manager");
    expect(row.get("session:lead")).toBe("engineer");
  });

  it("gives a run the rank of the row that answers for it", () => {
    // The point of the whole exercise: three runs, three ranks, and nothing
    // about a run itself says which. Only the tree does — and after the fold it
    // says so through `runOf` rather than through a row.
    const { nodes, runOf } = chain();
    const { run } = tiersOf(nodes, runOf);
    expect(run.get("jod-run")).toBe("jod");
    expect(run.get("mgr-run")).toBe("manager");
    expect(run.get("eng-run")).toBe("engineer");
  });

  it("still ranks a run that arrives as a row of its own", () => {
    // An older daemon answers with the unfolded forest. The rank has to survive
    // that too, or the sessions list loses its colours against one.
    const work = node("work", "work-a", 0);
    const session = node("session", "lead", 1, work);
    const run = node("run", "run-1", 2, session);
    expect(tiersOf([work, session, run]).run.get("run-1")).toBe("engineer");
  });

  it("leaves a project unranked — it is the repository, not a party in it", () => {
    expect(tiersOf(chain().nodes).row.has("project:tetris")).toBe(false);
  });

  it("ranks a loose work and the agents under it as engineers", () => {
    // A work with no project keeps its own heading through the fold rather than
    // turning its agents loose at the top level. It is still somebody working.
    const work = node("work", "work-a", 0);
    const session = node("session", "lead", 1, work);
    const tiers = tiersOf([work, session], new Map([["session:lead", "run-1"]]));
    expect(tiers.row.get("work:work-a")).toBe("engineer");
    expect(tiers.run.get("run-1")).toBe("engineer");
  });

  it("leaves a row whose parent link is missing unranked, rather than guessing", () => {
    // Rank is read off `parent`, and the server sets it on every row below the
    // top level. A row without one did not come from there, and inventing a
    // rank would draw a confident colour over an answer nobody knows.
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
  it("opens a manager on the run it answers for", () => {
    // The bug this whole branch exists for. The manager row was a permanent
    // leaf with no run beneath it, so this returned null and the button
    // rendered disabled — a row visibly there that could not be clicked.
    const { nodes, runOf } = chain();
    const manager = nodes.find((n) => n.kind === "manager")!;
    expect(openable(nodes, manager, runOf)).toBe("mgr-run");
  });

  it("opens jod on his own run and not on a manager's", () => {
    const { nodes, runOf } = chain();
    const main = nodes.find((n) => n.kind === "main")!;
    expect(openable(nodes, main, runOf)).toBe("jod-run");
  });

  it("opens a repository on the first run inside it", () => {
    // A project holds no run of its own, so it takes one from underneath —
    // which is what somebody clicking the repository's row means.
    const { nodes, runOf } = chain();
    const project = nodes.find((n) => n.kind === "project")!;
    nodes[1].has_children = true;
    expect(openable(nodes, project, runOf)).toBe("mgr-run");
  });

  it("does not reach past a row's own subtree", () => {
    // `lead` sits under `tetris`. A second repository's agent must not be what
    // clicking the first one opens.
    const { nodes, runOf } = chain();
    const other = node("project", "zuma", 0);
    other.has_children = false;
    const all = [...nodes, other];
    expect(openable(all, other, runOf)).toBeNull();
  });

  it("is null for a manager nobody has asked anything", () => {
    const project = node("project", "tetris", 0);
    const manager = node("manager", "mgr-conv", 1, project);
    manager.has_children = false;
    expect(openable([project, manager], manager, new Map())).toBeNull();
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
  it("covers exactly the kinds the delete can route", () => {
    // Pinned against `App.deleteFleetRows`'s switch. When these disagree, a
    // selected row is silently dropped and the delete reports success over
    // having done nothing — which is how it behaved before.
    const kinds = chain().nodes.filter(deletable).map((n) => n.kind);
    expect(new Set(kinds)).toEqual(new Set(["session"]));
  });

  it("refuses jod, a project and a manager", () => {
    for (const kind of ["main", "project", "manager"] as const) {
      expect(deletable(node(kind, "x", 0))).toBe(false);
    }
  });

  it("still takes a work heading and a run, on an unfolded tree", () => {
    expect(deletable(node("work", "w", 0))).toBe(true);
    expect(deletable(node("run", "r", 0))).toBe(true);
  });
});

describe("isLiveRow", () => {
  it("believes the event stream about a row the tree has not caught up with", () => {
    // The fleet is a four-second poll. An agent that started 200ms ago is
    // working, and a panel that waits for the next query to say so is the panel
    // somebody is complaining looks asleep.
    const { nodes, runOf } = chain();
    const session = nodes.find((n) => n.kind === "session")!;
    expect(isLiveRow(session, new Set(), runOf)).toBe(false);
    expect(isLiveRow(session, new Set(["eng-run"]), runOf)).toBe(true);
  });

  it("takes the tree's word for a heading, which holds no run", () => {
    // A closed work deliberately stops claiming to be running even with
    // something alive under it, and the roster cannot see that.
    const work = node("work", "work-a", 0);
    expect(isLiveRow(work, new Set(["work-a"]), new Map())).toBe(false);
    work.running = true;
    expect(isLiveRow(work, new Set(), new Map())).toBe(true);
  });
});

describe("isAgentRow", () => {
  it("counts the three that stand for somebody working", () => {
    const agents = chain().nodes.filter(isAgentRow).map((n) => n.kind);
    expect(agents).toEqual(["main", "manager", "session"]);
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
    const { nodes } = await (await populated()).fleet();

    expect(nodes.some((n) => n.kind === "project")).toBe(true);
    expect(nodes.some((n) => n.kind === "session")).toBe(true);
  });

  it("is empty rather than broken before anything has launched", async () => {
    // The first paint, before the staggered blueprints land. The panel must
    // draw "no work yet" here, not crash.
    const fleet = await new SimTransport("test").fleet();
    expect(fleet.nodes).toEqual([]);
    expect(fleet.runOf.size).toBe(0);
  });

  it("draws the folded shape, with no work rows and no run rows", async () => {
    // The server folds before it answers, so these two kinds never reach the
    // panel. A simulation that emitted them would be exercising a shape the
    // real driver cannot produce.
    const { nodes } = await (await populated()).fleet();
    expect(nodes.filter((n) => n.kind === "work")).toEqual([]);
    expect(nodes.filter((n) => n.kind === "run")).toEqual([]);
  });

  it("puts every agent under a repository, never at the root", async () => {
    // The panel indents by `depth`; an agent at depth 0 would render as a
    // top-level heading and the tree would read as a flat list.
    const { nodes } = await (await populated()).fleet();
    const agents = nodes.filter((n) => n.kind === "session");
    expect(agents.length).toBe(3);
    for (const agent of agents) {
      expect(agent.depth).toBe(1);
      expect(agent.parent).not.toBeNull();
    }
  });

  it("answers for a run on every agent's row", async () => {
    // With no run rows left, this is the only thing that makes a row openable.
    const { nodes, runOf } = await (await populated()).fleet();
    for (const agent of nodes.filter((n) => n.kind === "session")) {
      expect(runOf.get(fleetKey(agent.id))).toBe(agent.id.id);
    }
  });

  it("groups the two agents sharing a directory under one repository", async () => {
    const { nodes } = await (await populated()).fleet();
    expect(nodes.filter((n) => n.kind === "project").length).toBe(2);
  });

  it("emits document order, each row at most one level deeper", async () => {
    // `hideUnder` depends on this ordering, so it is worth pinning on the
    // driver that produces it synthetically.
    const { nodes } = await (await populated()).fleet();
    expect(nodes[0].depth).toBe(0);
    for (let i = 1; i < nodes.length; i += 1) {
      expect(nodes[i].depth).toBeLessThanOrEqual(nodes[i - 1].depth + 1);
    }
  });
});