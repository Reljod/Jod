/**
 * The nine workspaces, and the one rule that binds them.
 *
 * A direct port of [`cli/src/tui/workspace.rs`](../../../cli/src/tui/workspace.rs).
 * Chat is home; every other screen is somewhere you went *from* chat and come
 * back to. Keeping that as data — a digit, a letter, a title, a set of sort
 * orders — rather than as parallel `switch`es in three components is what lets
 * the tab bar, the command list and the direct-jump digits agree without anyone
 * remembering to update all three.
 *
 * Ported as *rules*, not as keystrokes. Where the TUI's mechanism has no meaning
 * under a thumb the rule is kept and the mechanism replaced — the precedent
 * `Ctrl-W` set in this app's README. So `letter` and `digit` still exist,
 * because they drive `/open` and the terminal habit, even though the phone's
 * primary gesture is a tap.
 */

/**
 * One screen. `Chat` is home and is never on the back stack.
 *
 * String-valued rather than an enum so it survives `JSON.stringify` into
 * `localStorage` and reads legibly in a test failure.
 */
export type Workspace =
  | "chat"
  | "fleet"
  | "memory"
  | "memory-graph"
  | "schedules"
  | "goals"
  | "hooks"
  | "tasks"
  | "activity"
  | "team";

/** Every workspace, including the ones with no digit of their own. */
export const ALL: readonly Workspace[] = [
  "chat",
  "fleet",
  "memory",
  "memory-graph",
  "schedules",
  "goals",
  "hooks",
  "tasks",
  "activity",
  "team",
] as const;

/**
 * The tab bar, in the order it is drawn. This is also the order the digits
 * follow, so `/open schedules` and `4` are visibly the same destination.
 *
 * `memory-graph` is deliberately absent: it is memory's second level and means
 * nothing without a node to focus on.
 */
export const MENU: readonly Workspace[] = [
  "chat",
  "fleet",
  "memory",
  "schedules",
  "goals",
  "hooks",
  "tasks",
  "activity",
  "team",
] as const;

/**
 * The letter that reaches this workspace.
 *
 * `w` for team is "who", because `t` is spoken for by tasks — the TUI's choice,
 * kept so the two agree.
 */
export function letter(w: Workspace): string | null {
  switch (w) {
    case "chat":
      return "c";
    case "fleet":
      return "f";
    case "memory":
      return "m";
    case "schedules":
      return "s";
    case "goals":
      return "g";
    case "hooks":
      return "h";
    case "tasks":
      return "t";
    case "activity":
      return "a";
    case "team":
      return "w";
    case "memory-graph":
      return null;
  }
}

export function fromLetter(c: string): Workspace | null {
  return MENU.find((w) => letter(w) === c) ?? null;
}

/** The digit that jumps straight here from another workspace. */
export function digit(w: Workspace): string | null {
  const at = MENU.indexOf(w);
  return at === -1 ? null : String(at + 1);
}

export function fromDigit(c: string): Workspace | null {
  if (!/^[1-9]$/.test(c)) return null;
  return MENU[Number(c) - 1] ?? null;
}

/** What the title bar calls it. */
export function title(w: Workspace): string {
  switch (w) {
    case "chat":
      return "chat";
    case "fleet":
      return "fleet";
    case "memory":
      return "memory · list";
    case "memory-graph":
      return "memory · local graph";
    case "schedules":
      return "schedules";
    case "goals":
      return "goals";
    case "hooks":
      return "webhooks";
    case "tasks":
      return "tasks";
    case "activity":
      return "activity";
    case "team":
      return "team";
  }
}

/**
 * What the tab bar calls it — one word, so the row of tabs reads as a row.
 */
export function menuName(w: Workspace): string {
  switch (w) {
    case "hooks":
      return "hooks";
    case "memory":
    case "memory-graph":
      return "memory";
    default:
      return title(w);
  }
}

/** True for every screen where the body is a list rather than a conversation. */
export function isList(w: Workspace): boolean {
  return w !== "chat";
}

/**
 * The sort orders the sort control cycles through. The first is the default, and
 * every screen has one so the control never does nothing.
 */
export function sorts(w: Workspace): readonly string[] {
  switch (w) {
    case "fleet":
      return ["running first", "newest", "name", "spend"];
    case "memory":
      return ["degree", "confidence", "name", "age"];
    case "schedules":
      return ["next", "name", "last"];
    case "goals":
      return ["progress", "name", "next"];
    case "hooks":
      return ["deliveries", "name", "last"];
    case "tasks":
      return ["state", "name", "age"];
    case "activity":
      return ["newest", "unread first", "source"];
    case "team":
      return ["name", "status"];
    case "chat":
    case "memory-graph":
      return ["—"];
  }
}

/** The name of the sort currently in force. Wraps rather than throwing. */
export function sortName(w: Workspace, at: number): string {
  const orders = sorts(w);
  // `%` alone yields -1 for a negative index, which would read as undefined.
  const index = ((at % orders.length) + orders.length) % orders.length;
  return orders[index];
}

/** Where this workspace's list cursor, filter and sort live. */
export function slot(w: Workspace): number {
  const at = ALL.indexOf(w);
  return at === -1 ? 0 : at;
}

// ─── one list's cursor, filter and sort order ───────────────────────────────

/**
 * The selection is an **id**, never a row index: the fleet re-sorts under the
 * cursor as runs finish, and an index would silently move the cursor onto a
 * different run at the moment one did.
 */
export interface ListState {
  selected: string | null;
  /**
   * Non-null once the search field has been opened, even while still empty — an
   * empty filter that is *open* still owns the keyboard, and dismissing still
   * has something to clear.
   */
  filter: string | null;
  /** True while the filter field has focus. */
  editingFilter: boolean;
  sort: number;
}

export function newListState(): ListState {
  return { selected: null, filter: null, editingFilter: false, sort: 0 };
}

/** Does this list have a filter that hides rows? */
export function filtering(list: ListState): boolean {
  return list.filter !== null && list.filter !== "";
}

/**
 * Keep the cursor on a row that still exists, preferring the one it was on.
 * Called after every refresh and every filter change.
 */
export function reconcile(list: ListState, ids: readonly string[]): ListState {
  return reconcileTo(list, ids, ids[0] ?? null);
}

/**
 * [`reconcile`], but landing somewhere other than the top row when the cursor
 * has nowhere to go.
 *
 * The fleet needs it: its first row is the pinned main chat, which is not an
 * agent, and a cursor that defaulted there would put every one of the list's
 * verbs — stop, watch — one tap away from the thing they are for. The chat is
 * *drawn* first because it is the anchor; the cursor starts on the work,
 * because managing the work is what opening this list means.
 */
export function reconcileTo(
  list: ListState,
  ids: readonly string[],
  fallback: string | null,
): ListState {
  if (ids.length === 0) return { ...list, selected: null };

  const stillThere = list.selected !== null && ids.includes(list.selected);
  if (stillThere) return list;

  const landed =
    fallback !== null && ids.includes(fallback) ? fallback : ids[0];
  return { ...list, selected: landed };
}

/** Where the cursor is, as a row index into `ids`. */
export function index(list: ListState, ids: readonly string[]): number {
  if (list.selected === null) return 0;
  const at = ids.indexOf(list.selected);
  return at === -1 ? 0 : at;
}

/**
 * Move by `delta`, clamped at both ends rather than wrapping: in a list that
 * changes under you, overshooting lands somewhere unrelated.
 */
export function step(
  list: ListState,
  delta: number,
  ids: readonly string[],
): ListState {
  if (ids.length === 0) return { ...list, selected: null };
  const at = index(list, ids);
  const landed = Math.min(Math.max(at + delta, 0), ids.length - 1);
  return { ...list, selected: ids[landed] };
}

export function first(list: ListState, ids: readonly string[]): ListState {
  return { ...list, selected: ids[0] ?? null };
}

export function last(list: ListState, ids: readonly string[]): ListState {
  return { ...list, selected: ids[ids.length - 1] ?? null };
}

/**
 * Does `text` match what was typed into the filter?
 *
 * Case-insensitive subsequence, which is what "fuzzy" means to everyone who has
 * used one: `prsr` finds `port-the-parser` without anyone learning a syntax.
 * Spaces are ignored so a typed phrase still matches a hyphenated name, and an
 * empty needle matches everything — so an open-but-empty filter hides nothing.
 */
export function matches(needle: string, text: string): boolean {
  const haystack = [...text.toLowerCase()];
  let at = 0;
  for (const want of needle.toLowerCase()) {
    if (/\s/.test(want)) continue;
    const found = haystack.indexOf(want, at);
    if (found === -1) return false;
    at = found + 1;
  }
  return true;
}
