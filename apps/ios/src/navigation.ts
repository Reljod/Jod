/**
 * Where you are, how you got there, and how you get back.
 *
 * The navigation half of the nine-workspace port. `workspaces.ts` is the *map* —
 * names, letters, digits, sort orders; this is the *state machine* over it, and
 * it holds the three rules that are easy to get subtly wrong:
 *
 *   1. **Chat is home and is never on the back stack.** Going back from a
 *      workspace lands in chat, and going back from chat does nothing.
 *   2. **Dismissing unwinds one thing at a time** — a filter being typed into
 *      first, then an open filter, then the screen. One gesture that closed both
 *      would throw away the list you were narrowing on the way out.
 *   3. **The local graph is reached from a focused node**, and walking it keeps a
 *      visit stack, because walking a graph without being able to walk back out
 *      of it is how you get lost in one.
 *
 * Kept free of React for the same reason `session.ts` is: the rules are tested
 * headless, and the components are a projection of the result.
 *
 * The visit stack is the happy part of this port. `cli/src/tui/graph.rs` argues
 * there is no layout algorithm worth having — one focus node, `⏎` to re-centre,
 * `Backspace` to pop — and a phone's push navigation *is* that model natively.
 */

import {
  ALL,
  type ListState,
  type Workspace,
  fromDigit,
  fromLetter,
  newListState,
  slot,
  sorts,
} from "./workspaces";

/** The node the graph is centred on, as its id and the name to show. */
export interface Focus {
  id: number;
  name: string;
}

export interface NavState {
  /** The screen on show. */
  at: Workspace;
  /**
   * Where dismissing goes. Chat is never in here — it is the floor, not a step.
   *
   * A stack rather than one "previous" because an activity row can send you to
   * schedules, and going back should retrace rather than teleport home.
   */
  back: Workspace[];
  /**
   * One `ListState` per workspace, indexed by `slot`, so each screen keeps its
   * own cursor, filter and sort and leaving does not reset it.
   */
  lists: ListState[];
  /**
   * The graph's focus. `null` means the graph has never been entered — which is
   * exactly why it cannot be reached by a digit: the digit would promise a
   * screen with nothing on it.
   */
  focus: Focus | null;
  /** Where you have been, oldest first. The focus is *not* in it. */
  trail: Focus[];
}

export function newNavState(): NavState {
  return {
    at: "chat",
    back: [],
    lists: ALL.map(() => newListState()),
    focus: null,
    trail: [],
  };
}

/** The `ListState` of the screen on show. */
export function currentList(nav: NavState): ListState {
  return nav.lists[slot(nav.at)];
}

/** Replace one workspace's `ListState`, leaving the others alone. */
export function withList(nav: NavState, w: Workspace, list: ListState): NavState {
  const lists = nav.lists.slice();
  lists[slot(w)] = list;
  return { ...nav, lists };
}

/**
 * Go to a workspace.
 *
 * Chat is home, so arriving there empties the back stack rather than adding to
 * it — otherwise dismissing from chat would walk backwards through screens you
 * had already left, which is not what "home" means.
 *
 * Going where you already are is a no-op rather than a self-referential entry.
 */
export function go(nav: NavState, to: Workspace): NavState {
  if (to === nav.at) return nav;
  if (to === "chat") return { ...nav, at: "chat", back: [] };
  return {
    ...nav,
    at: to,
    back: [...nav.back, nav.at].filter((w) => w !== "chat"),
  };
}

/**
 * Jump by the digit or letter the TUI uses.
 *
 * Kept even though the phone's gesture is a tap, because `/open` and the
 * terminal habit both arrive here — one path to a destination rather than three
 * that can disagree.
 */
export function jump(nav: NavState, key: string): NavState {
  const to = fromDigit(key) ?? fromLetter(key);
  return to === null ? nav : go(nav, to);
}

/**
 * The one dismiss gesture, unwinding a single thing.
 *
 * The order is the whole point. A filter with focus gives up the keyboard but
 * keeps what was typed; an open filter closes and stops hiding rows; only then
 * does the screen itself go.
 */
export function dismiss(nav: NavState): NavState {
  const list = currentList(nav);

  if (list.editingFilter) {
    return withList(nav, nav.at, { ...list, editingFilter: false });
  }
  if (list.filter !== null) {
    return withList(nav, nav.at, { ...list, filter: null });
  }
  return back(nav);
}

/**
 * Leave this screen for the one before it.
 *
 * Inside the local graph this pops the visit stack instead, because there "back"
 * means the node you came from — going straight out to the list would discard
 * the walk.
 */
export function back(nav: NavState): NavState {
  if (nav.at === "memory-graph" && nav.trail.length > 0) return popFocus(nav);

  const previous = nav.back.at(-1);
  if (previous === undefined) {
    return nav.at === "chat" ? nav : { ...nav, at: "chat", back: [] };
  }
  return { ...nav, at: previous, back: nav.back.slice(0, -1) };
}

/**
 * Enter the local graph on a node, or re-centre on a neighbour.
 *
 * Pushes where you were so the walk can be retraced. Re-centring on the node
 * already focused is a no-op — otherwise tapping the focus row would fill the
 * stack with one node repeated.
 */
export function focusNode(nav: NavState, on: Focus): NavState {
  if (nav.at === "memory-graph" && nav.focus?.id === on.id) return nav;

  const trail =
    nav.at === "memory-graph" && nav.focus !== null
      ? [...nav.trail, nav.focus]
      : nav.trail;
  return { ...go(nav, "memory-graph"), focus: on, trail };
}

/**
 * Step back out to the node you came from.
 *
 * With an empty trail this leaves the graph entirely and lands on the memory
 * list — the level above, and where the node came from.
 */
export function popFocus(nav: NavState): NavState {
  const previous = nav.trail.at(-1);
  if (previous === undefined) {
    return { ...go(nav, "memory"), focus: null, trail: [] };
  }
  return { ...nav, focus: previous, trail: nav.trail.slice(0, -1) };
}

/** Advance this screen's sort to the next order it declares. */
export function cycleSort(nav: NavState): NavState {
  const list = currentList(nav);
  return withList(nav, nav.at, {
    ...list,
    sort: (list.sort + 1) % sorts(nav.at).length,
  });
}

/** Open the search field, without yet hiding anything. */
export function openFilter(nav: NavState): NavState {
  const list = currentList(nav);
  return withList(nav, nav.at, {
    ...list,
    filter: list.filter ?? "",
    editingFilter: true,
  });
}

export function setFilter(nav: NavState, text: string): NavState {
  return withList(nav, nav.at, { ...currentList(nav), filter: text });
}

/**
 * Follow an activity row to what it names.
 *
 * `jump_to` arrives as a Rust tuple — `["schedules" | "goals", name]` — and a
 * row that names a schedule but cannot reach it is the screen without the point
 * of it. The named row is pre-selected so the cursor is already on it when the
 * list loads.
 *
 * An unknown destination is a row from a newer daemon, not an error, so it
 * navigates nowhere rather than throwing.
 */
export function followJump(
  nav: NavState,
  jumpTo: [string, string] | null,
): NavState {
  if (jumpTo === null) return nav;
  const [where, name] = jumpTo;
  const to = where === "schedules" ? "schedules" : where === "goals" ? "goals" : null;
  if (to === null) return nav;

  const moved = go(nav, to);
  return withList(moved, to, { ...moved.lists[slot(to)], selected: name });
}
