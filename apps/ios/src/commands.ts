/**
 * Slash commands.
 *
 * A port of `cli/src/tui/command.rs`, and for the same reason: parsing is
 * separated from doing, so the whole of "what did the user ask for" is a pure
 * function over a string and can be tested without a screen, a daemon or an
 * agent.
 *
 * The set is deliberately the TUI's — every command here maps onto something
 * Jod can actually do from a phone, and one that would need a capability this
 * client does not have is *absent* rather than present and inert. Unrecognised
 * input is reported, never swallowed.
 *
 * One divergence from the terminal, called out where it matters: `/exit` cannot
 * quit an iOS app (only the user closes an app), so it means what the TUI's
 * `/exit` actually achieves — stop watching, leave the agent running.
 */

import type { HarnessKind } from "./contract";

/** What a `/…` line asked for. Mirrors `enum Slash`. */
export type Slash =
  | { kind: "help" }
  /** Use a different harness for the next turn. */
  | { kind: "harness"; harness: HarnessKind }
  /** Set the model, or clear it back to the harness default. */
  | { kind: "model"; model: string | null }
  | { kind: "thinking" }
  /** Show or hide what tools gave back. */
  | { kind: "details" }
  /** Start a fresh conversation, forgetting the session cursor. */
  | { kind: "new" }
  /** List conversations that can be resumed. */
  | { kind: "sessions" }
  /** Continue a specific conversation by its harness-assigned id. */
  | { kind: "resume"; id: string }
  | { kind: "agents" }
  | { kind: "team" }
  /** Clear the transcript on screen. The conversation is untouched. */
  | { kind: "clear" }
  | { kind: "exit" }
  /** A `/word` nobody knows. Reported rather than sent to the agent. */
  | { kind: "unknown"; what: string }
  /** A known command missing its argument. */
  | { kind: "needs_argument"; usage: string };

/**
 * Parse a line as a slash command.
 *
 * `null` means "this is not a command" — including a bare `/`, and anything
 * with leading whitespace, so a prompt that happens to start with a slash
 * (`/usr/bin/foo is missing`) still reaches the agent as long as it is a real
 * path rather than a single word.
 */
export function parse(line: string): Slash | null {
  if (!line.startsWith("/")) return null;
  const rest = line.slice(1);
  const parts = rest.split(/\s+/).filter((p) => p !== "");
  const name = parts.shift()?.toLowerCase();
  if (name === undefined) return null;
  const arg = parts.join(" ").trim();

  switch (name) {
    case "help":
    case "?":
      return { kind: "help" };
    case "harness":
    case "agent": {
      const harness = harnessFrom(arg);
      if (harness) return { kind: "harness", harness };
      if (arg === "") {
        return { kind: "needs_argument", usage: "/harness <claude|opencode|agy>" };
      }
      return { kind: "unknown", what: `/harness ${arg}` };
    }
    case "model":
    case "models":
      return arg === "" || arg === "default" || arg === "clear"
        ? { kind: "model", model: null }
        : { kind: "model", model: arg };
    case "thinking":
    case "reasoning":
      return { kind: "thinking" };
    case "details":
    case "output":
      return { kind: "details" };
    case "new":
      return { kind: "new" };
    case "sessions":
      return { kind: "sessions" };
    case "resume":
    case "continue":
      return arg === ""
        ? { kind: "needs_argument", usage: "/resume <session-id>" }
        : { kind: "resume", id: arg };
    case "agents":
      return { kind: "agents" };
    case "team":
      return { kind: "team" };
    case "clear":
      return { kind: "clear" };
    case "exit":
    case "quit":
    case "q":
      return { kind: "exit" };
    default:
      return { kind: "unknown", what: `/${name}` };
  }
}

/** Every spelling of a harness the parser accepts. Mirrors `harness_from`. */
export function harnessFrom(name: string): HarnessKind | null {
  switch (name.toLowerCase()) {
    case "claude":
    case "claude-code":
    case "claude_code":
    case "cc":
      return "claude_code";
    case "opencode":
    case "open-code":
    case "open_code":
    case "oc":
      return "open_code";
    case "agy":
    case "antigravity":
      return "agy";
    default:
      return null;
  }
}

/** The spelling offered for a harness — the shortest one `parse` accepts. */
export function shortName(harness: HarnessKind): string {
  switch (harness) {
    case "claude_code":
      return "claude";
    case "open_code":
      return "opencode";
    case "agy":
      return "agy";
  }
}

/**
 * One line of `/help`, so the list and the parser cannot drift apart: every
 * command that appears here is one `parse` accepts.
 *
 * The hints name the *phone's* gestures where the terminal named keys — a
 * `Ctrl-A` on a device with no control key is a lie in the help text.
 */
export const HELP: readonly (readonly [string, string])[] = [
  ["/help", "this list"],
  ["/harness <name>", "claude, opencode or agy — takes effect next turn"],
  ["/model <name>", "set the model; no argument restores the default"],
  ["/thinking", "show or hide reasoning"],
  ["/details", "show or hide what tools returned"],
  ["/new", "start a fresh conversation"],
  ["/sessions", "conversations you can pick up"],
  ["/resume <id>", "continue one of them"],
  ["/agents", "the delegations sheet"],
  ["/team", "the team sheet"],
  ["/clear", "clear the transcript on screen"],
  ["/exit", "stop watching; running agents keep going"],
] as const;

/** One thing the completion list can offer. */
export interface Completion {
  /** The whole line to put in the composer if this is chosen. */
  line: string;
  /** What is shown next to it. */
  hint: string;
}

const HARNESS_KINDS_IN_ORDER: readonly HarnessKind[] = [
  "claude_code",
  "open_code",
  "agy",
] as const;

const HARNESS_LABELS: Record<HarnessKind, string> = {
  claude_code: "Claude Code",
  open_code: "OpenCode",
  agy: "AGY",
};

/**
 * What could complete the line being typed.
 *
 * Empty means "no list": either this is not a command, or it is already
 * finished. Completing arguments as well as names matters more on a phone than
 * it does in a terminal — `/harness ` is the point where a user would otherwise
 * have to remember three spellings *and* type them on a soft keyboard.
 */
export function completions(input: string): Completion[] {
  if (!input.startsWith("/")) return [];
  const rest = input.slice(1);

  // Still typing the command word: offer names.
  if (!/\s/.test(rest)) {
    const typed = rest.toLowerCase();
    return HELP.filter(([usage]) => usage.slice(1).split(" ")[0]!.startsWith(typed)).map(
      ([usage, hint]) => {
        const name = usage.split(" ")[0]!;
        const takesArgument = usage.includes("<");
        return {
          // A command that takes an argument gets a trailing space, so
          // accepting it leaves the caret where the argument goes.
          line: takesArgument ? `${name} ` : name,
          hint,
        };
      },
    );
  }

  // Past the name: offer arguments for the commands that have a fixed set.
  const at = rest.search(/\s/);
  const name = rest.slice(0, at).toLowerCase();
  const typed = rest.slice(at).trimStart().toLowerCase();
  if (name !== "harness" && name !== "agent") return [];
  return HARNESS_KINDS_IN_ORDER.filter(
    (k) => k.replace(/_/g, "").startsWith(typed) || k.startsWith(typed),
  ).map((k) => ({
    line: `/${name} ${shortName(k)}`,
    hint: HARNESS_LABELS[k],
  }));
}
