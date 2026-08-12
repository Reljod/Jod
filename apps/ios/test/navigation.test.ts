import { describe, expect, it } from "vitest";

import {
  back,
  currentList,
  cycleSort,
  dismiss,
  focusNode,
  followJump,
  go,
  jump,
  newNavState,
  openFilter,
  popFocus,
  setFilter,
  withList,
} from "../src/navigation";
import { newListState, slot } from "../src/workspaces";

/**
 * The rules `cli/src/tui/workspace.rs` and `cli/src/tui/graph.rs` state, driven
 * headless. These are the ones that break quietly: a back stack that grows a
 * cycle, a dismiss gesture that eats two things at once, or a graph walk with no
 * way out of it.
 */
describe("chat is home", () => {
  it("starts there", () => {
    expect(newNavState().at).toBe("chat");
  });

  it("never puts chat on the back stack", () => {
    const nav = go(go(newNavState(), "fleet"), "memory");
    expect(nav.back).toEqual(["fleet"]);
    expect(nav.back).not.toContain("chat");
  });

  /** Returning home is a reset, not another step away from where you were. */
  it("empties the back stack on the way home", () => {
    const nav = go(go(go(newNavState(), "fleet"), "memory"), "chat");
    expect(nav.at).toBe("chat");
    expect(nav.back).toEqual([]);
  });

  it("does nothing going back from chat", () => {
    const nav = newNavState();
    expect(back(nav)).toBe(nav);
  });

  it("lands home going back from a workspace reached directly", () => {
    expect(back(go(newNavState(), "goals")).at).toBe("chat");
  });

  /** Retrace, rather than teleport home — an activity row can send you onward. */
  it("retraces the way it came", () => {
    let nav = go(go(newNavState(), "activity"), "schedules");
    nav = back(nav);
    expect(nav.at).toBe("activity");
    nav = back(nav);
    expect(nav.at).toBe("chat");
  });

  it("treats going where you already are as nothing happening", () => {
    const nav = go(newNavState(), "fleet");
    expect(go(nav, "fleet")).toBe(nav);
  });
});

describe("jumping by key", () => {
  it("reaches the same screen by digit or by letter", () => {
    expect(jump(newNavState(), "4").at).toBe("schedules");
    expect(jump(newNavState(), "s").at).toBe("schedules");
  });

  it("ignores a key that names nothing", () => {
    const nav = newNavState();
    expect(jump(nav, "0")).toBe(nav);
    expect(jump(nav, "z")).toBe(nav);
  });

  /** The graph has no key of its own, so no key can reach it. */
  it("cannot reach the local graph", () => {
    for (const key of ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]) {
      expect(jump(newNavState(), key).at).not.toBe("memory-graph");
    }
  });
});

describe("dismissing unwinds one thing at a time", () => {
  it("gives up the keyboard first, keeping what was typed", () => {
    let nav = openFilter(go(newNavState(), "fleet"));
    nav = setFilter(nav, "port");
    expect(currentList(nav).editingFilter).toBe(true);

    nav = dismiss(nav);
    expect(currentList(nav).editingFilter).toBe(false);
    expect(currentList(nav).filter, "the filter survives").toBe("port");
    expect(nav.at, "and so does the screen").toBe("fleet");
  });

  it("then closes the filter, still without leaving", () => {
    let nav = setFilter(openFilter(go(newNavState(), "fleet")), "port");
    nav = dismiss(dismiss(nav));
    expect(currentList(nav).filter).toBeNull();
    expect(nav.at).toBe("fleet");
  });

  it("and only then leaves the screen", () => {
    let nav = setFilter(openFilter(go(newNavState(), "fleet")), "port");
    nav = dismiss(dismiss(dismiss(nav)));
    expect(nav.at).toBe("chat");
  });

  it("leaves straight away when there is no filter to unwind", () => {
    expect(dismiss(go(newNavState(), "fleet")).at).toBe("chat");
  });

  /** Opening the field must not appear to empty the list. */
  it("opens the filter without hiding anything", () => {
    const nav = openFilter(go(newNavState(), "fleet"));
    expect(currentList(nav).filter).toBe("");
    expect(currentList(nav).editingFilter).toBe(true);
  });

  it("reopening does not discard what was already typed", () => {
    let nav = setFilter(openFilter(go(newNavState(), "fleet")), "port");
    nav = dismiss(nav);
    nav = openFilter(nav);
    expect(currentList(nav).filter).toBe("port");
  });
});

describe("each screen keeps its own list", () => {
  it("does not carry a filter from one screen to another", () => {
    let nav = setFilter(openFilter(go(newNavState(), "fleet")), "port");
    nav = go(nav, "goals");
    expect(currentList(nav).filter).toBeNull();
  });

  it("finds the filter still there on returning", () => {
    let nav = setFilter(openFilter(go(newNavState(), "fleet")), "port");
    nav = go(go(nav, "goals"), "fleet");
    expect(currentList(nav).filter).toBe("port");
  });

  it("touches only the slot it was told to", () => {
    const nav = withList(newNavState(), "goals", {
      ...newListState(),
      selected: "ship",
    });
    expect(nav.lists[slot("goals")].selected).toBe("ship");
    expect(nav.lists[slot("fleet")].selected).toBeNull();
  });

  it("gives every workspace a list of its own, including the graph", () => {
    expect(newNavState().lists).toHaveLength(10);
  });
});

describe("the sort cycle", () => {
  it("advances through this screen's orders and wraps", () => {
    let nav = go(newNavState(), "fleet"); // 4 orders
    expect(currentList(nav).sort).toBe(0);
    nav = cycleSort(cycleSort(cycleSort(nav)));
    expect(currentList(nav).sort).toBe(3);
    nav = cycleSort(nav);
    expect(currentList(nav).sort, "wraps to the default").toBe(0);
  });

  /** Chat declares one order, so the control must not divide by zero or stick. */
  it("is harmless on a screen with a single order", () => {
    const nav = cycleSort(newNavState());
    expect(currentList(nav).sort).toBe(0);
  });
});

describe("the graph's visit stack", () => {
  const node = (id: number) => ({ id, name: `n${id}` });

  it("is entered from a node, not from a key", () => {
    const nav = focusNode(go(newNavState(), "memory"), node(1));
    expect(nav.at).toBe("memory-graph");
    expect(nav.focus).toEqual(node(1));
    expect(nav.trail, "nowhere to go back to yet").toEqual([]);
  });

  it("remembers where you came from as you re-centre", () => {
    let nav = focusNode(go(newNavState(), "memory"), node(1));
    nav = focusNode(nav, node(2));
    nav = focusNode(nav, node(3));
    expect(nav.focus).toEqual(node(3));
    expect(nav.trail).toEqual([node(1), node(2)]);
  });

  it("walks back out the way it came", () => {
    let nav = focusNode(go(newNavState(), "memory"), node(1));
    nav = focusNode(nav, node(2));
    nav = popFocus(nav);
    expect(nav.focus).toEqual(node(1));
    expect(nav.trail).toEqual([]);
    expect(nav.at).toBe("memory-graph");
  });

  /** Walking a graph with no way out of it is how you get lost in one. */
  it("leaves for the list above once the trail is empty", () => {
    let nav = focusNode(go(newNavState(), "memory"), node(1));
    nav = popFocus(nav);
    expect(nav.at).toBe("memory");
    expect(nav.focus).toBeNull();
    expect(nav.trail).toEqual([]);
  });

  it("does not stack the same node twice when re-centring on the focus", () => {
    let nav = focusNode(go(newNavState(), "memory"), node(1));
    nav = focusNode(nav, node(1));
    expect(nav.trail).toEqual([]);
  });

  /** Inside the graph, dismissing means "the node I came from". */
  it("pops the walk rather than leaving, while there is a walk to pop", () => {
    let nav = focusNode(go(newNavState(), "memory"), node(1));
    nav = focusNode(nav, node(2));
    nav = dismiss(nav);
    expect(nav.at).toBe("memory-graph");
    expect(nav.focus).toEqual(node(1));
  });
});

describe("following an activity row", () => {
  it("goes where the row points, with the named row already selected", () => {
    const nav = followJump(go(newNavState(), "activity"), ["schedules", "nightly"]);
    expect(nav.at).toBe("schedules");
    expect(nav.lists[slot("schedules")].selected).toBe("nightly");
    // And back retraces to the feed it came from.
    expect(back(nav).at).toBe("activity");
  });

  it("reaches goals as well", () => {
    const nav = followJump(newNavState(), ["goals", "ship-it"]);
    expect(nav.at).toBe("goals");
    expect(nav.lists[slot("goals")].selected).toBe("ship-it");
  });

  it("stays put for a row with nowhere to go", () => {
    const nav = go(newNavState(), "activity");
    expect(followJump(nav, null)).toBe(nav);
  });

  /** A destination from a newer daemon is not an error. */
  it("stays put rather than throwing on a destination it does not know", () => {
    const nav = go(newNavState(), "activity");
    expect(followJump(nav, ["telepathy", "x"])).toBe(nav);
  });
});
