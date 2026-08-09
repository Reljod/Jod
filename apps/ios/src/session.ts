/**
 * The conversation's state, and every decision it makes.
 *
 * This is a port of `cli/src/tui/app.rs` — deliberately free of rendering and
 * of I/O, so the behaviour that is easy to get subtly wrong (which turn a
 * message belongs to, when the view is allowed to jump, which session the next
 * turn continues) is testable without a screen.
 *
 * Parity with the TUI is the point of this file. Where it departs, it departs
 * because the platform makes the TUI's mechanism meaningless, and each of those
 * is called out:
 *
 * 1. **Scroll is a boolean, not a line count.** The TUI counts lines scrolled
 *    up from the bottom because it owns the viewport. A UIWebView owns its own
 *    scroll position, so the only part worth porting is the *rule* — new output
 *    pulls the view down only if it was already at the bottom. That survives as
 *    `following`, set from the DOM rather than computed here.
 * 2. **No cursor arithmetic.** `app.rs` tracks a byte cursor and implements
 *    Ctrl-W/Ctrl-U by hand. iOS gives a real text field with a real caret;
 *    reimplementing readline on top of it would be worse, not more faithful.
 * 3. **No quit confirmation.** Backgrounding an app is not quitting it, and
 *    the agent keeps running either way — which is the outcome `confirm_quit`
 *    existed to protect.
 *
 * Everything else — the entry vocabulary, which events reach the transcript,
 * the resume cursor, the busy guard, the status line — is the same behaviour.
 */

import type {
  AgentEvent,
  AgentStatus,
  AgentSummary,
  HarnessKind,
  Resume,
  Usage,
} from "./contract";

/**
 * One line in the transcript, tagged with what produced it so the view can
 * style it without re-inspecting the event.
 *
 * Mirrors `enum Entry` in `cli/src/tui/app.rs`.
 */
export type Entry =
  /** What the user typed. */
  | { kind: "you"; text: string }
  /** Assistant prose. */
  | { kind: "agent"; text: string }
  /** The agent's reasoning, shown only when thinking is toggled on. */
  | { kind: "thinking"; text: string }
  /** A tool the agent called. */
  | { kind: "tool"; name: string; failed: boolean }
  /** A run finished: the summary line. */
  | { kind: "done"; text: string; failed: boolean }
  /** Something Jod itself wants to say. */
  | { kind: "notice"; text: string }
  /** A line the harness printed that we could not classify. */
  | { kind: "raw"; text: string };

/** Which surface is showing. The agents list is a sheet, not a mode. */
export type Pane = "chat" | "agents";

/** One row in the agents sheet — the mobile form of `Ctrl-A`. */
export interface AgentLine {
  id: string;
  name: string;
  harness: string;
  status: AgentStatus | string;
}

export interface SessionState {
  transcript: Entry[];
  /** The composer's text. The text field owns the caret; this owns the value. */
  input: string;
  /**
   * Whether the transcript is pinned to the bottom. `true` means new output
   * scrolls into view; `false` means the reader has scrolled up and must not
   * be yanked back down.
   */
  following: boolean;
  harness: HarnessKind;
  model: string | null;
  /** The harness session this conversation is threaded through. */
  session: string | null;
  /** What the *next* turn will ask the harness to continue. */
  resume: Resume;
  costUsd: number;
  showThinking: boolean;
  pane: Pane;
  /** True while an agent is working, so the UI can refuse a second prompt. */
  busy: boolean;
  agents: AgentLine[];
  /**
   * The delegation whose output belongs on screen. Events from any other agent
   * are ignored by the transcript; the agents sheet is where they show up.
   */
  currentAgentId: string | null;
}

export function newSession(
  harness: HarnessKind,
  model: string | null = null,
  resume: Resume = "fresh",
): SessionState {
  return {
    transcript: [],
    input: "",
    following: true,
    harness,
    model,
    session: null,
    resume,
    costUsd: 0,
    showThinking: false,
    pane: "chat",
    busy: false,
    agents: [],
    currentAgentId: null,
  };
}

/**
 * Append one entry.
 *
 * The TUI's `push` increments the scroll offset so the reader's position holds
 * still under new output. Here the equivalent is simply *not* touching
 * `following` — a reader who has scrolled up stays scrolled up, and the view
 * effect only chases the bottom while `following` is true.
 */
export function push(state: SessionState, entry: Entry): SessionState {
  return { ...state, transcript: [...state.transcript, entry] };
}

/**
 * Take the typed line, clearing the input. Returns a `null` prompt when there
 * was nothing to send — whitespace alone is nothing.
 */
export function takeInput(state: SessionState): {
  state: SessionState;
  prompt: string | null;
} {
  const text = state.input.trim();
  if (text === "") return { state, prompt: null };
  return { state: { ...state, input: "" }, prompt: text };
}

export function setInput(state: SessionState, input: string): SessionState {
  return { ...state, input };
}

export function setFollowing(
  state: SessionState,
  following: boolean,
): SessionState {
  return { ...state, following };
}

export function toggleThinking(state: SessionState): SessionState {
  const showThinking = !state.showThinking;
  return push({ ...state, showThinking }, {
    kind: "notice",
    text: `thinking ${showThinking ? "shown" : "hidden"}`,
  });
}

export function togglePane(state: SessionState): SessionState {
  return { ...state, pane: state.pane === "agents" ? "chat" : "agents" };
}

export function setPane(state: SessionState, pane: Pane): SessionState {
  return { ...state, pane };
}

/** `Ctrl-L`. Clears the view, never the harness's memory of the conversation. */
export function clearTranscript(state: SessionState): SessionState {
  return { ...state, transcript: [], following: true };
}

export function setAgents(
  state: SessionState,
  agents: AgentLine[],
): SessionState {
  return { ...state, agents };
}

/**
 * Record that a turn has started.
 *
 * `busy` is set **here**, before the spawn request is sent, and not when the
 * daemon answers. The round trip is a real fraction of a second over a tailnet,
 * and a composer that stays enabled across it lets a double-tap start two
 * agents on the same working directory — the exact collision the charter's
 * one-owner-per-path rule exists to prevent.
 *
 * Which agent is producing the output is not known yet; `attachAgent` supplies
 * it once the daemon has answered.
 */
export function beginTurn(state: SessionState, prompt: string): SessionState {
  return push(
    { ...state, busy: true, following: true },
    { kind: "you", text: prompt },
  );
}

/** Name the delegation whose events belong on screen. */
export function attachAgent(
  state: SessionState,
  agentId: string,
): SessionState {
  return { ...state, currentAgentId: agentId };
}

/** Give up on a turn that never started. */
export function abandonTurn(state: SessionState): SessionState {
  return { ...state, busy: false };
}

/**
 * Fold one event from the harness into the transcript.
 *
 * Mirrors `App::apply`. The choices worth keeping in step:
 *
 * - `started` moves the resume cursor to *this exact session*, not "the most
 *   recent one", which could belong to another delegation entirely;
 * - a tool call that **worked** is noise and is dropped; one that failed is the
 *   reason the answer is about to be wrong, so it is kept;
 * - `raw` is surfaced rather than swallowed, because core emits it for anything
 *   a harness said that it could not classify. Blank lines are the exception —
 *   they carry nothing.
 */
export function applyEvent(
  state: SessionState,
  event: AgentEvent,
): SessionState {
  switch (event.kind) {
    case "started": {
      let next = state;
      if (event.session_id) {
        next = {
          ...next,
          session: event.session_id,
          resume: { session: event.session_id },
        };
      }
      if (event.model) next = { ...next, model: event.model };
      return next;
    }

    case "thinking":
      return state.showThinking
        ? push(state, { kind: "thinking", text: event.text })
        : state;

    case "message":
      return push(state, { kind: "agent", text: event.text });

    case "tool_call":
      return push(state, { kind: "tool", name: event.name, failed: false });

    case "tool_result":
      return event.is_error
        ? push(state, { kind: "tool", name: event.name, failed: true })
        : state;

    case "finished": {
      const usage = event.usage ?? {};
      const costUsd =
        state.costUsd + (typeof usage.cost_usd === "number" ? usage.cost_usd : 0);
      return push(
        { ...state, costUsd, busy: false },
        { kind: "done", text: usageSummary(usage), failed: event.is_error },
      );
    }

    case "error":
      return push(state, { kind: "notice", text: event.message });

    case "raw":
      return event.line.trim() === ""
        ? state
        : push(state, { kind: "raw", text: event.line });
  }
}

/** `"1234 out · $0.0210"` — whichever halves the harness actually reported. */
export function usageSummary(usage: Usage | undefined): string {
  const bits: string[] = [];
  if (usage && typeof usage.output_tokens === "number") {
    bits.push(`${usage.output_tokens} out`);
  }
  if (usage && typeof usage.cost_usd === "number") {
    bits.push(`$${usage.cost_usd.toFixed(4)}`);
  }
  return bits.join(" · ");
}

export const HARNESS_LABEL: Record<HarnessKind, string> = {
  claude_code: "Claude Code",
  open_code: "OpenCode",
  agy: "AGY",
};

/** The one-line summary shown in the status bar. Mirrors `App::status`. */
export function statusLine(state: SessionState): string {
  const parts: string[] = [HARNESS_LABEL[state.harness]];
  if (state.model) parts.push(state.model);
  if (state.costUsd > 0) parts.push(`$${state.costUsd.toFixed(4)}`);
  parts.push(state.busy ? "working" : "ready");
  return parts.join(" · ");
}

/**
 * The name a delegation gets when the user did not pick one.
 *
 * Ported from `default_name` in `cli/src/main.rs`, ellipsis included, so a run
 * started from the phone is named the same as one started from the terminal.
 */
export function defaultName(prompt: string): string {
  const name = prompt.split(/\s+/).filter(Boolean).slice(0, 5).join(" ");
  if (name === "") return "agent";
  const chars = [...name];
  if (chars.length > 48) return `${chars.slice(0, 47).join("")}…`;
  return name;
}

/** Roster rows, in the shape the sheet renders. */
export function toAgentLines(agents: AgentSummary[]): AgentLine[] {
  return agents.map((a) => ({
    id: a.id,
    name: a.name,
    harness: a.harness_label,
    status: a.status,
  }));
}
