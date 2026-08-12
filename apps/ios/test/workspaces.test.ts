import { describe, expect, it } from "vitest";

import {
  ALL,
  MENU,
  digit,
  filtering,
  first,
  fromDigit,
  fromLetter,
  index,
  isList,
  last,
  letter,
  matches,
  menuName,
  newListState,
  reconcile,
  reconcileTo,
  slot,
  sortName,
  sorts,
  step,
  title,
  type ListState,
} from "../src/workspaces";

/**
 * Case for case with the `mod tests` in `cli/src/tui/workspace.rs`.
 *
 * The README claims the phone and the terminal cannot drift quietly, and this
 * file is the part of that claim covering navigation: every assertion here has a
 * counterpart in the Rust suite, so a change to one screen's letter, digit, sort
 * order or cursor rule fails on both sides or neither.
 */
describe("the workspace map, against cli/src/tui/workspace.rs", () => {
  it("gives every tab a letter and a digit of its own", () => {
    const letters: string[] = [];
    const digits: string[] = [];
    for (const w of MENU) {
      const l = letter(w);
      const d = digit(w);
      expect(l, `${w} needs a letter`).not.toBeNull();
      expect(d, `${w} needs a digit`).not.toBeNull();
      expect(letters, `${l} is claimed twice`).not.toContain(l);
      expect(digits, `${d} is claimed twice`).not.toContain(d);
      letters.push(l!);
      digits.push(d!);
    }
  });

  it("sends a letter and its digit to the same workspace", () => {
    for (const w of MENU) {
      expect(fromLetter(letter(w)!)).toBe(w);
      expect(fromDigit(digit(w)!)).toBe(w);
    }
  });

  /**
   * The graph is memory's second level, so it has no digit — a digit would
   * promise you could land there without a node to look at.
   */
  it("does not make the local graph directly addressable", () => {
    expect(letter("memory-graph")).toBeNull();
    expect(digit("memory-graph")).toBeNull();
    expect(MENU).not.toContain("memory-graph");
    // But it is still a workspace, so it still gets a slot and a title.
    expect(ALL).toContain("memory-graph");
  });

  it("reaches nothing from an unknown digit", () => {
    expect(fromDigit("0")).toBeNull();
    expect(fromDigit("x")).toBeNull();
    expect(fromDigit("")).toBeNull();
    // Two digits is not a destination, and must not be read as its first.
    expect(fromDigit("11")).toBeNull();
  });

  it("reaches nothing from an unknown letter", () => {
    expect(fromLetter("z")).toBeNull();
  });

  it("gives every workspace a slot of its own", () => {
    const seen: number[] = [];
    for (const w of ALL) {
      expect(seen, `${w} shares a slot`).not.toContain(slot(w));
      seen.push(slot(w));
    }
  });

  /**
   * The sort control must never be a thing that does nothing, so every screen
   * declares at least one order — including the two whose order is "—".
   */
  it("names at least one sort order for every workspace", () => {
    for (const w of ALL) {
      expect(sorts(w).length, `${w}`).toBeGreaterThan(0);
      expect(sortName(w, 99), `${w} wraps rather than throwing`).not.toBe("");
    }
  });

  it("wraps the sort name in both directions rather than reading undefined", () => {
    expect(sortName("fleet", 0)).toBe("running first");
    expect(sortName("fleet", 4)).toBe("running first");
    expect(sortName("fleet", -1)).toBe("spend");
  });

  /** Chat is home; every other screen is a list. */
  it("treats every screen but chat as a list", () => {
    expect(isList("chat")).toBe(false);
    for (const w of ALL.filter((w) => w !== "chat")) {
      expect(isList(w), `${w}`).toBe(true);
    }
  });

  /** The titles are the TUI's, so the two name the same screen the same way. */
  it("titles the screens the way the TUI does", () => {
    expect(title("memory")).toBe("memory · list");
    expect(title("memory-graph")).toBe("memory · local graph");
    expect(title("hooks")).toBe("webhooks");
  });

  /** One word per tab, so the row of tabs reads as a row. */
  it("gives the tab bar one word per row", () => {
    expect(menuName("hooks")).toBe("hooks");
    expect(menuName("memory")).toBe("memory");
    expect(menuName("memory-graph")).toBe("memory");
    for (const w of MENU) {
      expect(menuName(w), `${w} must be one word`).not.toContain(" ");
    }
  });

  it("puts chat first, because chat is home", () => {
    expect(MENU[0]).toBe("chat");
    expect(digit("chat")).toBe("1");
  });
});

describe("the filter", () => {
  it("matches a subsequence whatever the case", () => {
    expect(matches("prsr", "port-the-parser")).toBe(true);
    expect(matches("PORT", "port-the-parser")).toBe(true);
    expect(matches("", "anything at all")).toBe(true);
    expect(matches("zzz", "port-the-parser")).toBe(false);
  });

  it("ignores the spaces typed into it", () => {
    expect(matches("port parser", "port-the-parser")).toBe(true);
  });

  /**
   * A subsequence, not a bag of characters: the same letter twice needs two
   * occurrences, in order.
   */
  it("needs the characters in order, and needs each one separately", () => {
    expect(matches("rap", "parser")).toBe(false);
    expect(matches("rr", "parser")).toBe(true);
    expect(matches("rrr", "parser")).toBe(false);
  });
});

describe("a list's cursor", () => {
  const ids = (...names: string[]) => names;

  /**
   * The fleet re-sorts under the cursor as runs finish. Tracking a row index
   * would move the selection onto a different run the moment one did.
   */
  it("follows the item when the list re-sorts", () => {
    let list = newListState();
    const before = ids("a", "b", "c");
    list = reconcile(list, before);
    list = step(list, 1, before);
    expect(list.selected).toBe("b");

    const after = ids("c", "b", "a");
    list = reconcile(list, after);
    expect(list.selected, "still on the same item").toBe("b");
    expect(index(list, after)).toBe(1);
  });

  it("falls back to the top when its selection disappeared", () => {
    let list: ListState = { ...newListState(), selected: "gone" };
    list = reconcile(list, ids("a", "b"));
    expect(list.selected).toBe("a");
  });

  /**
   * The fleet's first row is the pinned main chat, which is not an agent. The
   * cursor starts on the work, because managing the work is what opening the
   * list means.
   */
  it("can land somewhere other than the top row when told to", () => {
    let list = newListState();
    list = reconcileTo(list, ids("chat", "agent-1", "agent-2"), "agent-1");
    expect(list.selected).toBe("agent-1");
  });

  it("ignores a fallback that is not in the list", () => {
    let list = newListState();
    list = reconcileTo(list, ids("a", "b"), "not-here");
    expect(list.selected).toBe("a");
  });

  it("selects nothing in an empty list", () => {
    let list = newListState();
    list = reconcile(list, []);
    expect(list.selected).toBeNull();
    list = step(list, 1, []);
    expect(list.selected).toBeNull();
  });

  it("stops at both ends rather than wrapping", () => {
    let list = newListState();
    const rows = ids("a", "b", "c");
    list = reconcile(list, rows);
    list = step(list, -1, rows);
    expect(list.selected).toBe("a");
    list = step(list, 9, rows);
    expect(list.selected).toBe("c");
  });

  it("reaches the ends of the list directly", () => {
    let list = newListState();
    const rows = ids("a", "b", "c");
    list = last(list, rows);
    expect(list.selected).toBe("c");
    list = first(list, rows);
    expect(list.selected).toBe("a");
  });

  it("leaves the selection alone when it is still there", () => {
    const rows = ids("a", "b", "c");
    const list: ListState = { ...newListState(), selected: "b" };
    expect(reconcile(list, rows)).toBe(list);
  });

  /**
   * An open-but-empty filter owns the keyboard without hiding anything, so
   * opening the search field never makes the list appear to empty itself.
   */
  it("hides nothing while the filter is open but empty", () => {
    const list: ListState = { ...newListState(), filter: "" };
    expect(filtering(list)).toBe(false);
    expect(filtering({ ...list, filter: "port" })).toBe(true);
    expect(filtering(newListState()), "not opened at all").toBe(false);
  });
});
