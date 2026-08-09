/**
 * Slash commands, tested against the same cases as `cli/src/tui/command.rs`.
 *
 * Deliberately a case-for-case port rather than a fresh set: this file and the
 * Rust `mod tests` are the only thing keeping the two parsers honest with each
 * other, and a test that exists on one side only is exactly where they will
 * drift. Where a case is *not* portable it is marked and explained.
 */

import { describe, expect, it } from "vitest";

import {
  HELP,
  completions,
  harnessFrom,
  parse,
  shortName,
  type Slash,
} from "../src/commands";
import { HARNESS_KINDS, type HarnessKind } from "../src/contract";

function lines(input: string): string[] {
  return completions(input).map((c) => c.line);
}

describe("completing a command", () => {
  it("offers nothing for a plain prompt", () => {
    expect(completions("hello")).toEqual([]);
    expect(completions("")).toEqual([]);
  });

  it("offers everything for a bare slash", () => {
    expect(completions("/")).toHaveLength(HELP.length);
  });

  it("narrows as you type", () => {
    const some = lines("/t");
    expect(some).toContain("/thinking");
    expect(some).toContain("/team");
    expect(some).not.toContain("/help");

    expect(lines("/th")).toEqual(["/thinking"]);
  });

  it("gives a command that takes an argument a trailing space", () => {
    expect(lines("/harn")).toEqual(["/harness "]);
    expect(lines("/hel")).toEqual(["/help"]);
  });

  it("completes nonsense to nothing", () => {
    expect(completions("/zzzz")).toEqual([]);
  });

  /** The bit that saves remembering three spellings on a soft keyboard. */
  it("completes harness arguments too", () => {
    const all = lines("/harness ");
    expect(all).toHaveLength(HARNESS_KINDS.length);
    expect(all).toContain("/harness claude");
    expect(all).toContain("/harness agy");

    expect(lines("/harness op")).toEqual(["/harness opencode"]);
  });

  /**
   * Every offered harness spelling must be one the parser accepts, or the list
   * would suggest something that then fails.
   */
  it("only suggests harnesses that parse", () => {
    for (const c of completions("/harness ")) {
      expect(parse(c.line)?.kind, `${c.line} was suggested`).toBe("harness");
    }
  });

  /** Likewise for names: a suggestion must never parse as unknown. */
  it("only suggests commands that parse", () => {
    for (const c of completions("/")) {
      const parsed = parse(c.line.trim());
      expect(parsed, `${c.line} was suggested`).not.toBeNull();
      expect(parsed?.kind, `${c.line} was suggested`).not.toBe("unknown");
    }
  });

  /**
   * A command needing an argument completes to *itself* plus a space. In the
   * TUI that was how Enter got swallowed instead of running the command; here
   * accepting it is a tap, so nothing is swallowed — but the shape has to hold
   * or the same class of bug arrives with the first keyboard shortcut.
   */
  it("completes a command needing an argument to itself", () => {
    expect(lines("/resume")).toEqual(["/resume "]);
    expect(lines("/harness")).toEqual(["/harness "]);
    for (const input of ["/resume", "/harness"]) {
      expect(completions(input)[0]!.line.trimEnd()).toBe(input);
    }
  });

  it("offers a hint next to every suggestion", () => {
    for (const c of completions("/")) expect(c.hint).not.toBe("");
    for (const c of completions("/harness ")) expect(c.hint).not.toBe("");
  });
});

describe("parsing a command", () => {
  it("does not treat a plain prompt as a command", () => {
    expect(parse("hello there")).toBeNull();
    expect(parse("")).toBeNull();
    expect(parse("what about / this")).toBeNull();
  });

  /** A bare slash is a typo, not a command, and must not be swallowed. */
  it("does not treat a bare slash as a command", () => {
    expect(parse("/")).toBeNull();
    expect(parse("/   ")).toBeNull();
  });

  it("answers /help by both spellings", () => {
    expect(parse("/help")).toEqual({ kind: "help" });
    expect(parse("/?")).toEqual({ kind: "help" });
  });

  it("names every harness, including its short forms", () => {
    const cases: [string, HarnessKind][] = [
      ["/harness claude", "claude_code"],
      ["/harness cc", "claude_code"],
      ["/harness opencode", "open_code"],
      ["/harness oc", "open_code"],
      ["/harness agy", "agy"],
      ["/harness antigravity", "agy"],
    ];
    for (const [text, harness] of cases) {
      expect(parse(text), text).toEqual({ kind: "harness", harness });
    }
  });

  /**
   * Every harness the build knows must be reachable by name, or a new one
   * would be spawnable from the daemon and invisible from the phone.
   */
  it("leaves no harness unreachable", () => {
    for (const kind of HARNESS_KINDS) {
      expect(harnessFrom(kind), kind).toBe(kind);
      expect(harnessFrom(shortName(kind)), kind).toBe(kind);
    }
  });

  it("reports an unknown harness rather than guessing", () => {
    expect(parse("/harness gpt")).toEqual({
      kind: "unknown",
      what: "/harness gpt",
    });
  });

  it("says what /harness wants when given nothing", () => {
    expect(parse("/harness")).toEqual({
      kind: "needs_argument",
      usage: "/harness <claude|opencode|agy>",
    });
  });

  it("takes a model name, or resets to the default", () => {
    expect(parse("/model anthropic/claude-sonnet-5")).toEqual({
      kind: "model",
      model: "anthropic/claude-sonnet-5",
    });
    expect(parse("/model")).toEqual({ kind: "model", model: null });
    expect(parse("/model default")).toEqual({ kind: "model", model: null });
    expect(parse("/model clear")).toEqual({ kind: "model", model: null });
  });

  it("needs an id to resume", () => {
    expect(parse("/resume ses-1")).toEqual({ kind: "resume", id: "ses-1" });
    expect(parse("/continue ses-1")).toEqual({ kind: "resume", id: "ses-1" });
    expect(parse("/resume")).toEqual({
      kind: "needs_argument",
      usage: "/resume <session-id>",
    });
  });

  it("parses all the simple commands", () => {
    const simple: [string, Slash["kind"]][] = [
      ["/thinking", "thinking"],
      ["/reasoning", "thinking"],
      ["/details", "details"],
      ["/output", "details"],
      ["/new", "new"],
      ["/sessions", "sessions"],
      ["/agents", "agents"],
      ["/team", "team"],
      ["/clear", "clear"],
      ["/exit", "exit"],
      ["/quit", "exit"],
      ["/q", "exit"],
    ];
    for (const [text, kind] of simple) {
      expect(parse(text)?.kind, text).toBe(kind);
    }
  });

  it("is case-insensitive and tolerates spacing", () => {
    expect(parse("/HELP")).toEqual({ kind: "help" });
    expect(parse("/Thinking")).toEqual({ kind: "thinking" });
    expect(parse("/harness    OpenCode")).toEqual({
      kind: "harness",
      harness: "open_code",
    });
  });

  it("names an unknown command back rather than sending it to the agent", () => {
    expect(parse("/wibble")).toEqual({ kind: "unknown", what: "/wibble" });
    // The ones OpenCode has and Jod does not: reported, not silently inert.
    for (const missing of ["/compact", "/undo", "/share", "/themes"]) {
      expect(parse(missing), missing).toEqual({ kind: "unknown", what: missing });
    }
  });

  /** `/help` must not list a command the parser rejects. */
  it("parses every documented command", () => {
    for (const [usage] of HELP) {
      const word = usage.split(" ")[0]!;
      const parsed = parse(word);
      expect(parsed, `${word} did not parse`).not.toBeNull();
      expect(parsed?.kind, `${usage} is documented but unknown`).not.toBe("unknown");
    }
  });

  /**
   * A leading slash is judged by the whole first *word*, not the character, so
   * a path is named back rather than silently run as some command it prefixes.
   * A line that does not start at column zero is not a command at all.
   */
  it("names a path back instead of running part of it", () => {
    expect(parse("/usr/bin/foo is missing")).toEqual({
      kind: "unknown",
      what: "/usr/bin/foo",
    });
    expect(parse(" /help")).toBeNull();
  });
});
