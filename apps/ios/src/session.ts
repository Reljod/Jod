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
 * the tool detail and tool output lines, the resume cursor, the busy guard, the
 * status line, the panes — is the same behaviour.
 */

import type {
  AgentEvent,
  AgentStatus,
  AgentSummary,
  HarnessKind,
  Member,
  Resume,
  TeamTask,
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
  /**
   * A tool the agent called, with a one-line summary of its argument —
   * `Bash · cargo test`, not a bare `Bash`.
   */
  | { kind: "tool"; name: string; detail: string | null; failed: boolean }
  /**
   * What a tool gave back. Shown when details are on, which is the point of
   * watching a harness work rather than waiting for its conclusion.
   */
  | { kind: "tool_out"; text: string; failed: boolean }
  /** A run finished: the summary line. */
  | { kind: "done"; text: string; failed: boolean }
  /** Something Jod itself wants to say. */
  | { kind: "notice"; text: string }
  /** A line the harness printed that we could not classify. */
  | { kind: "raw"; text: string };

/**
 * Which surface is showing.
 *
 * The TUI calls these panes and floats them over the transcript; here they are
 * bottom sheets. Same three, same toggle rule: opening the one already open
 * closes it.
 */
export type Pane = "chat" | "agents" | "team";

/** One row in the agents sheet — the mobile form of `Ctrl-A`. */
export interface AgentLine {
  id: string;
  name: string;
  harness: string;
  status: AgentStatus | string;
  /**
   * The harness's own conversation id, once it has reported one. This is what
   * `/resume` actually needs — the sheet shows Jod's agent id, which is a
   * different thing entirely.
   */
  session: string | null;
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
  /**
   * The model to *ask* for, or `null` for whatever the harness picks itself.
   * Only `/model` and the app's own configuration set this.
   */
  model: string | null;
  /**
   * The model the harness said it was using. Display only — it must never feed
   * back into a spawn, because a name one harness reports (say
   * `claude-sonnet-4-5`) is not a name another harness accepts.
   */
  reportedModel: string | null;
  /** The harness session this conversation is threaded through. */
  session: string | null;
  /** What the *next* turn will ask the harness to continue. */
  resume: Resume;
  costUsd: number;
  /**
   * Whether the agent's reasoning is shown. On by default, like
   * {@link SessionState.showDetails} and for the same reason: with it off the
   * transcript is a list of tool calls, and a list of tool calls does not say
   * why any of them happened.
   */
  showThinking: boolean;
  /**
   * Whether tool output is shown. On by default: the reason to watch a harness
   * work is to see what it is doing.
   */
  showDetails: boolean;
  pane: Pane;
  /** True while an agent is working, so the UI can refuse a second prompt. */
  busy: boolean;
  agents: AgentLine[];
  /**
   * The team this session is watching, if any. `null` means teams are not in
   * play and the sheet says so rather than showing an empty board.
   */
  team: string | null;
  /**
   * Every team the daemon knows about.
   *
   * The TUI takes its team from `--team <name>` on the command line. A phone
   * has no command line, so the names are asked for instead: one team is
   * adopted automatically, several are offered as a list to tap.
   */
  teams: string[];
  members: Member[];
  tasks: TeamTask[];
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
    reportedModel: null,
    session: null,
    resume,
    costUsd: 0,
    showThinking: true,
    showDetails: true,
    pane: "chat",
    busy: false,
    agents: [],
    team: null,
    teams: [],
    members: [],
    tasks: [],
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

/** `/details`. What tools gave back, on or off. */
export function toggleDetails(state: SessionState): SessionState {
  const showDetails = !state.showDetails;
  return push({ ...state, showDetails }, {
    kind: "notice",
    text: `tool output ${showDetails ? "shown" : "hidden"}`,
  });
}

/**
 * Open a pane, or close it if it is the one already open.
 *
 * Mirrors the TUI's `Ctrl-A` / `Ctrl-G`, which toggle rather than switch.
 */
export function togglePane(state: SessionState, pane: Exclude<Pane, "chat">): SessionState {
  return { ...state, pane: state.pane === pane ? "chat" : pane };
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

/** The teams the daemon knows about, for the picker. */
export function setTeams(state: SessionState, teams: string[]): SessionState {
  return { ...state, teams };
}

/** What the team sheet draws. Written by teammates, so only the daemon knows. */
export function setTeam(
  state: SessionState,
  team: string | null,
  members: Member[],
  tasks: TeamTask[],
): SessionState {
  return { ...state, team, members, tasks };
}

/** `/model`. `null` restores whatever the harness picks for itself. */
export function setModel(state: SessionState, model: string | null): SessionState {
  return push({ ...state, model }, {
    kind: "notice",
    text: model === null ? "model: the harness default" : `model: ${model}`,
  });
}

/**
 * `/harness`. Use a different harness from the next turn.
 *
 * Conversations belong to a harness, so the session cursor cannot cross: it
 * would try to resume a conversation the new harness has never heard of.
 *
 * When the harness actually changes, the model goes with it. `claude-sonnet-5`
 * means nothing to OpenCode or AGY, so keeping either the requested or the
 * reported name would hand the new harness a model it rejects — and the switch
 * would look like it simply did not work. Spend is dropped for the same reason:
 * it belongs to the conversation being abandoned.
 */
export function setHarness(state: SessionState, harness: HarnessKind): SessionState {
  const changed = state.harness !== harness;
  const next: SessionState = {
    ...state,
    harness,
    resume: "fresh",
    session: null,
    model: changed ? null : state.model,
    reportedModel: changed ? null : state.reportedModel,
    costUsd: changed ? 0 : state.costUsd,
  };
  return push(next, {
    kind: "notice",
    text: `${HARNESS_LABEL[harness]} from the next turn — fresh conversation, its own default model`,
  });
}

/**
 * `/new`. Forget the cursor and start over; the old conversation is untouched.
 *
 * `currentAgentId` deliberately survives, exactly as the TUI's `current` does:
 * a run already in flight keeps streaming here rather than going silent, and
 * only the *next* turn starts fresh.
 */
export function newConversation(state: SessionState): SessionState {
  return push(
    {
      ...state,
      resume: "fresh",
      session: null,
      costUsd: 0,
      transcript: [],
      following: true,
    },
    { kind: "notice", text: "new conversation" },
  );
}

/** Continue a named conversation from the next turn. */
export function resumeSession(state: SessionState, session: string): SessionState {
  return { ...state, resume: { session }, session };
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
 * The most useful single field of a tool's arguments.
 *
 * A port of `tool_detail` in `cli/src/tui/app.rs`, keys and order included.
 * Harnesses name things differently, so the common keys are tried in order of
 * how much they tell a reader, and anything unrecognised falls back to compact
 * JSON rather than being dropped.
 */
export function toolDetail(input: unknown): string | null {
  // Compared with case and underscores ignored, so `file_path`, `filePath` and
  // `FilePath` all match one entry. The harnesses genuinely disagree here: AGY
  // names its parameters `TargetFile` and `DirectoryPath`, and without them its
  // calls rendered as raw JSON.
  const KEYS = [
    "command",
    "cmd",
    "filepath",
    "path",
    "targetfile",
    "directorypath",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "searchterm",
  ] as const;
  const normalise = (key: string) => key.replace(/_/g, "").toLowerCase();

  if (input === null || input === undefined) return null;

  if (typeof input === "object" && !Array.isArray(input)) {
    const map = input as Record<string, unknown>;
    for (const key of KEYS) {
      for (const [found, value] of Object.entries(map)) {
        if (normalise(found) !== key) continue;
        if (typeof value === "string" && value.trim() !== "") {
          return oneLine(value, 90);
        }
      }
    }
  }

  if (typeof input === "string") {
    return input.trim() === "" ? null : oneLine(input, 90);
  }

  const text = JSON.stringify(input);
  if (text === undefined || text === "{}") return null;
  return oneLine(text, 90);
}

/** Collapse to one line and truncate, so a payload cannot own the transcript. */
export function oneLine(s: string, max: number): string {
  const flat = s.split(/\s+/).filter((p) => p !== "").join(" ");
  const chars = [...flat];
  if (chars.length <= max) return flat;
  return `${chars.slice(0, max).join("")}…`;
}

/** Keep the first `n` lines of tool output, saying how much was left. */
export function firstLines(s: string, n: number): string {
  const lines = s.split("\n");
  // Rust's `str::lines` drops a single trailing newline; `split` does not, so
  // an output ending in "\n" would otherwise be one line longer here and get
  // truncated a line early.
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  if (lines.length <= n) return s.replace(/\s+$/, "");
  return `${lines.slice(0, n).join("\n")}\n… (+${lines.length - n} more lines)`;
}

/**
 * Fold one event from the harness into the transcript.
 *
 * Mirrors `App::apply`. The choices worth keeping in step:
 *
 * - `started` moves the resume cursor to *this exact session*, not "the most
 *   recent one", which could belong to another delegation entirely, and records
 *   the model it reports **separately** from the one that was asked for;
 * - a tool call carries the most useful of its arguments, so the transcript
 *   reads `Bash · cargo test` rather than a bare `Bash`;
 * - a tool *result* is shown when details are on, and always when it failed —
 *   a failure is the reason the answer is about to be wrong;
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
      // Reported, not requested. Writing this into `model` would send it back
      // to the daemon on the next turn, and a harness-reported name is not
      // always a name any harness accepts.
      if (event.model) next = { ...next, reportedModel: event.model };
      return next;
    }

    case "thinking":
      return state.showThinking
        ? push(state, { kind: "thinking", text: event.text })
        : state;

    case "message":
      return push(state, { kind: "agent", text: event.text });

    case "tool_call":
      return push(state, {
        kind: "tool",
        name: event.name,
        detail: toolDetail(event.input),
        failed: false,
      });

    case "tool_result": {
      // A result also needs a call line above it when none was announced.
      // OpenCode reports a fast tool as already `completed`, so no `tool_call`
      // ever arrives and the output rendered as a bare `└ Wrote file
      // successfully.` — an answer with its question missing.
      const last = state.transcript[state.transcript.length - 1];
      const announced = last?.kind === "tool" && last.name === event.name;
      let next = state;
      if (event.is_error || !announced) {
        next = push(next, {
          kind: "tool",
          name: event.name,
          detail: null,
          failed: event.is_error,
        });
      }
      const summary = event.summary;
      if (summary !== undefined && summary.trim() !== "") {
        if (event.is_error || state.showDetails) {
          next = push(next, {
            kind: "tool_out",
            text: firstLines(summary, 6),
            failed: event.is_error,
          });
        }
      }
      return next;
    }

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

/** What `/resume <id>` turned out to mean. Mirrors `enum Resolved`. */
export type Resolved =
  /** A conversation the harness can continue. */
  | { kind: "session"; session: string }
  /** Not recognised here; hand it to the harness as typed. */
  | { kind: "verbatim"; typed: string }
  /** Matches a known agent that has no conversation id yet. */
  | { kind: "no_session"; agent: string }
  /** Matches this many agents, so it names none of them. */
  | { kind: "ambiguous"; count: number };

/**
 * Turn what the user typed at `/resume` into a harness session id.
 *
 * The agents sheet shows a *shortened Jod agent id*, and `/sessions` tells you
 * to feed it to `/resume` — but `/resume` hands its argument to the harness as
 * a conversation id, which an agent id never is. So a prefix of either is
 * accepted and translated, and anything unrecognised is passed through
 * untouched, because a session id copied from elsewhere is still a legitimate
 * thing to type.
 */
export function resolveSession(state: SessionState, typed: string): Resolved {
  const exact = state.agents.find((a) => a.session === typed);
  if (exact?.session) return { kind: "session", session: exact.session };

  const matches = state.agents.filter(
    (a) => a.id.startsWith(typed) || (a.session?.startsWith(typed) ?? false),
  );
  if (matches.length === 0) return { kind: "verbatim", typed };
  if (matches.length === 1) {
    const only = matches[0]!;
    // Known agent, but it never reported a conversation — resuming it would
    // silently start a fresh one instead.
    return only.session
      ? { kind: "session", session: only.session }
      : { kind: "no_session", agent: only.id };
  }
  return { kind: "ambiguous", count: matches.length };
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

/**
 * The one-line summary shown in the status bar. Mirrors `App::status`.
 *
 * What the harness actually ran beats what was asked for; before the first turn
 * there is nothing to report, so the request stands in.
 */
export function statusLine(state: SessionState): string {
  const parts: string[] = [HARNESS_LABEL[state.harness]];
  const model = state.reportedModel ?? state.model;
  if (model) parts.push(model);
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
    session: a.session_id,
  }));
}
