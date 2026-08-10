/**
 * The reducer, held to the same behaviour as `cli/src/tui/app.rs`.
 *
 * Where a test corresponds to one in the Rust suite, it asserts the same thing
 * — because the point of this app is that a run started from the phone behaves
 * like a run started from the terminal, and "behaves like" has to be checkable
 * rather than asserted in a README.
 */

import { describe, expect, it } from "vitest";

import type { AgentEvent } from "../src/contract";
import {
  abandonTurn,
  applyEvent,
  attachAgent,
  beginTurn,
  clearTranscript,
  defaultName,
  firstLines,
  newConversation,
  newSession,
  oneLine,
  push,
  resolveSession,
  resumeSession,
  setAgents,
  setFollowing,
  setHarness,
  setInput,
  setModel,
  setTeam,
  statusLine,
  takeInput,
  toAgentLines,
  toggleDetails,
  togglePane,
  toggleThinking,
  toolDetail,
  usageSummary,
  type AgentLine,
  type Entry,
  type SessionState,
} from "../src/session";

function session(): SessionState {
  return newSession("claude_code");
}

function typed(text: string): SessionState {
  return setInput(session(), text);
}

describe("the composer", () => {
  it("hands over the typed line and clears itself", () => {
    const { state, prompt } = takeInput(typed("ship it"));
    expect(prompt).toBe("ship it");
    expect(state.input).toBe("");
  });

  it("trims, because a trailing space is not part of the task", () => {
    expect(takeInput(typed("  ship it  ")).prompt).toBe("ship it");
  });

  it("refuses to send whitespace alone", () => {
    const { state, prompt } = takeInput(typed("   "));
    expect(prompt).toBeNull();
    // The input is left exactly as it was — nothing was taken.
    expect(state.input).toBe("   ");
  });

  it("refuses to send nothing at all", () => {
    expect(takeInput(session()).prompt).toBeNull();
  });
});

describe("following the transcript", () => {
  it("starts pinned to the bottom", () => {
    expect(session().following).toBe(true);
  });

  it("does not yank a reader who scrolled up back down", () => {
    // The TUI's rule, and the reason this state exists at all: new output must
    // not move the view while someone is reading something further back.
    let s = setFollowing(session(), false);
    s = push(s, { kind: "agent", text: "one" });
    s = push(s, { kind: "agent", text: "two" });
    expect(s.following).toBe(false);
    expect(s.transcript).toHaveLength(2);
  });

  it("keeps chasing the bottom for a reader who never left it", () => {
    const s = push(session(), { kind: "agent", text: "one" });
    expect(s.following).toBe(true);
  });

  it("re-pins when the transcript is cleared", () => {
    const s = clearTranscript(setFollowing(typed("x"), false));
    expect(s.following).toBe(true);
    expect(s.transcript).toEqual([]);
  });
});

describe("folding events into the transcript", () => {
  it("threads the next turn through the session the harness reported", () => {
    // The load-bearing one. `resume` must become *this* session, not "the most
    // recent", which could belong to a delegation someone else started.
    const s = applyEvent(session(), {
      kind: "started",
      session_id: "sess-9",
      model: "claude-opus-5",
    });
    expect(s.session).toBe("sess-9");
    expect(s.resume).toEqual({ session: "sess-9" });
    // Reported, not requested — see "what the harness says it is using" below.
    expect(s.reportedModel).toBe("claude-opus-5");
  });

  it("leaves the resume cursor alone when the harness reported no session", () => {
    const s = applyEvent(session(), {
      kind: "started",
      session_id: null,
      model: null,
    });
    expect(s.resume).toBe("fresh");
    expect(s.session).toBeNull();
  });

  it("keeps a model already known when a later start omits it", () => {
    let s = applyEvent(session(), {
      kind: "started",
      session_id: "a",
      model: "claude-opus-5",
    });
    s = applyEvent(s, { kind: "started", session_id: "b", model: null });
    expect(s.reportedModel).toBe("claude-opus-5");
    expect(s.resume).toEqual({ session: "b" });
  });

  it("hides reasoning until asked", () => {
    const s = applyEvent(session(), { kind: "thinking", text: "hmm" });
    expect(s.transcript).toEqual([]);
  });

  it("shows reasoning once toggled on", () => {
    const s = applyEvent(toggleThinking(session()), { kind: "thinking", text: "hmm" });
    expect(s.transcript).toContainEqual({ kind: "thinking", text: "hmm" });
  });

  it("says which way the toggle went", () => {
    expect(toggleThinking(session()).transcript).toContainEqual({
      kind: "notice",
      text: "thinking shown",
    });
    expect(toggleThinking(toggleThinking(session())).transcript).toContainEqual({
      kind: "notice",
      text: "thinking hidden",
    });
  });

  it("keeps assistant prose", () => {
    const s = applyEvent(session(), { kind: "message", text: "done" });
    expect(s.transcript).toEqual([{ kind: "agent", text: "done" }]);
  });

  it("shows a tool call, with no detail when the harness sent no arguments", () => {
    const s = applyEvent(session(), { kind: "tool_call", name: "Bash" });
    expect(s.transcript).toEqual([
      { kind: "tool", name: "Bash", detail: null, failed: false },
    ]);
  });

  /**
   * A result that worked used to be discarded outright, which is what made the
   * transcript a conclusion rather than a view of the work. It is kept now —
   * `/details` is the switch, and the fuller rules are exercised under
   * "watching a tool work" below.
   */
  it("keeps a tool result that worked", () => {
    const s = applyEvent(session(), {
      kind: "tool_result",
      name: "Bash",
      summary: "ok",
      is_error: false,
    });
    expect(s.transcript).toContainEqual({ kind: "tool_out", text: "ok", failed: false });
  });

  it("keeps a tool result that failed, because it explains the wrong answer", () => {
    const s = applyEvent(session(), {
      kind: "tool_result",
      name: "Bash",
      is_error: true,
    });
    expect(s.transcript).toEqual([
      { kind: "tool", name: "Bash", detail: null, failed: true },
    ]);
  });

  it("ends the turn and banks the cost", () => {
    const s = applyEvent({ ...session(), busy: true }, {
      kind: "finished",
      is_error: false,
      usage: { output_tokens: 1200, cost_usd: 0.021 },
    });
    expect(s.busy).toBe(false);
    expect(s.costUsd).toBeCloseTo(0.021);
    expect(s.transcript).toEqual([
      { kind: "done", text: "1200 out · $0.0210", failed: false },
    ]);
  });

  it("accumulates cost across turns rather than replacing it", () => {
    const finished: AgentEvent = {
      kind: "finished",
      is_error: false,
      usage: { cost_usd: 0.01 },
    };
    const s = applyEvent(applyEvent(session(), finished), finished);
    expect(s.costUsd).toBeCloseTo(0.02);
  });

  it("marks a failed run failed", () => {
    const s = applyEvent(session(), { kind: "finished", is_error: true, usage: {} });
    expect(s.transcript).toEqual([{ kind: "done", text: "", failed: true }]);
  });

  it("survives a harness that reported no usage at all", () => {
    // AGY reports no cost. A client that assumed the field existed would show
    // `$NaN` on every AGY run.
    const s = applyEvent(session(), {
      kind: "finished",
      is_error: false,
      usage: {},
    });
    expect(s.costUsd).toBe(0);
    expect(statusLine(s)).not.toContain("NaN");
  });

  it("surfaces an error as a notice", () => {
    const s = applyEvent(session(), { kind: "error", message: "jod-run is missing" });
    expect(s.transcript).toEqual([{ kind: "notice", text: "jod-run is missing" }]);
  });

  it("surfaces a line it could not classify rather than dropping it", () => {
    // Core's whole reason for emitting `raw`: hiding it would turn "we did not
    // understand this" into "this never happened".
    const s = applyEvent(session(), { kind: "raw", line: "warning: odd" });
    expect(s.transcript).toEqual([{ kind: "raw", text: "warning: odd" }]);
  });

  it("drops a blank raw line, which carries nothing", () => {
    const s = applyEvent(session(), { kind: "raw", line: "   \n " });
    expect(s.transcript).toEqual([]);
  });

  it("handles every event kind the contract defines", () => {
    // A harness upgrade that adds a kind should fail here, not in someone's
    // hand at the far end of a tailnet.
    const kinds: AgentEvent[] = [
      { kind: "started", session_id: null, model: null },
      { kind: "thinking", text: "t" },
      { kind: "message", text: "m" },
      { kind: "tool_call", name: "T" },
      { kind: "tool_result", name: "T", is_error: false },
      { kind: "finished", is_error: false, usage: {} },
      { kind: "raw", line: "r" },
      { kind: "error", message: "e" },
    ];
    expect(kinds).toHaveLength(8);
    for (const event of kinds) {
      expect(() => applyEvent(session(), event)).not.toThrow();
    }
  });
});

describe("the turn", () => {
  it("goes busy and echoes the prompt before any agent id is known", () => {
    // Busy has to be true across the spawn round trip, or a second tap in that
    // window starts a second agent in the same working directory.
    const s = beginTurn(session(), "ship it");
    expect(s.busy).toBe(true);
    expect(s.currentAgentId).toBeNull();
    expect(s.transcript).toEqual([{ kind: "you", text: "ship it" }]);
  });

  it("snaps back to the bottom when the user sends, wherever they were reading", () => {
    const s = beginTurn(setFollowing(session(), false), "ship it");
    expect(s.following).toBe(true);
  });

  it("names the delegation once the daemon has answered", () => {
    const s = attachAgent(beginTurn(session(), "ship it"), "agent-1");
    expect(s.currentAgentId).toBe("agent-1");
    expect(s.busy).toBe(true);
  });

  it("releases the composer for a turn that never started", () => {
    const s = abandonTurn(beginTurn(session(), "ship it"));
    expect(s.busy).toBe(false);
  });
});

describe("the status line", () => {
  it("names the harness and says it is ready", () => {
    expect(statusLine(session())).toBe("Claude Code · ready");
  });

  it("says working while a turn is in flight", () => {
    expect(statusLine({ ...session(), busy: true })).toBe("Claude Code · working");
  });

  it("adds the model and the spend once they are known", () => {
    const s = { ...session(), model: "claude-opus-5", costUsd: 0.5 };
    expect(statusLine(s)).toBe("Claude Code · claude-opus-5 · $0.5000 · ready");
  });

  it("hides a zero cost rather than showing $0.0000", () => {
    expect(statusLine(session())).not.toContain("$");
  });

  it("labels all three harnesses the way core does", () => {
    expect(statusLine(newSession("open_code"))).toContain("OpenCode");
    expect(statusLine(newSession("agy"))).toContain("AGY");
    expect(statusLine(newSession("claude_code"))).toContain("Claude Code");
  });
});

describe("usageSummary", () => {
  it("reports whichever halves the harness gave", () => {
    expect(usageSummary({ output_tokens: 10, cost_usd: 1 })).toBe("10 out · $1.0000");
    expect(usageSummary({ output_tokens: 10 })).toBe("10 out");
    expect(usageSummary({ cost_usd: 1 })).toBe("$1.0000");
    expect(usageSummary({})).toBe("");
    expect(usageSummary(undefined)).toBe("");
  });

  it("keeps a zero cost visible, because free is a fact and missing is not", () => {
    expect(usageSummary({ cost_usd: 0 })).toBe("$0.0000");
  });
});

describe("defaultName", () => {
  // Ported from `default_name` in cli/src/main.rs, so a run started on the
  // phone is named the same as one started in the terminal.
  it("takes the first five words", () => {
    expect(defaultName("one two three four five six seven")).toBe(
      "one two three four five",
    );
  });

  it("falls back when there is nothing to name it after", () => {
    expect(defaultName("   ")).toBe("agent");
    expect(defaultName("")).toBe("agent");
  });

  it("truncates a long name with an ellipsis", () => {
    const name = defaultName("a".repeat(60));
    expect([...name]).toHaveLength(48);
    expect(name.endsWith("…")).toBe(true);
  });

  it("counts characters, not bytes, so multi-byte names are not cut short", () => {
    const name = defaultName("é".repeat(60));
    expect([...name]).toHaveLength(48);
  });

  it("collapses the runs of whitespace a phone keyboard produces", () => {
    expect(defaultName("  ship    it  now ")).toBe("ship it now");
  });
});

describe("the agents sheet", () => {
  it("toggles open and shut", () => {
    expect(togglePane(session(), "agents").pane).toBe("agents");
    expect(togglePane(togglePane(session(), "agents"), "agents").pane).toBe("chat");
  });

  it("keeps the fields the roster renders", () => {
    const lines = toAgentLines([
      {
        id: "a1",
        name: "ship it",
        harness_label: "Claude Code",
        status: "running",
      } as never,
    ]);
    expect(lines).toEqual([
      { id: "a1", name: "ship it", harness: "Claude Code", status: "running" },
    ]);
  });
});

describe("immutability", () => {
  it("never mutates the state handed to it", () => {
    // `useSyncExternalStore` compares snapshots by identity: a reducer that
    // mutated in place would render once and then go silent.
    const before = session();
    const frozen = JSON.stringify(before);
    push(before, { kind: "agent", text: "x" });
    applyEvent(before, { kind: "message", text: "x" });
    takeInput(before);
    toggleThinking(before);
    expect(JSON.stringify(before)).toBe(frozen);
  });
});

// ─── live tool output ───────────────────────────────────────────────────────
//
// The part of #37 that changed what watching an agent *feels* like: before, a
// tool call showed only its name and a successful result was discarded
// outright. These hold the port to the same rules.

function entries(state: SessionState): Entry[] {
  return state.transcript;
}

function call(name: string, input?: unknown): AgentEvent {
  return input === undefined
    ? { kind: "tool_call", name }
    : { kind: "tool_call", name, input };
}

function result(name: string, summary?: string, isError = false): AgentEvent {
  return summary === undefined
    ? { kind: "tool_result", name, is_error: isError }
    : { kind: "tool_result", name, summary, is_error: isError };
}

describe("the most useful field of a tool call", () => {
  it("prefers a command over anything else", () => {
    expect(toolDetail({ command: "cargo test", description: "run tests" })).toBe(
      "cargo test",
    );
  });

  it("finds a path however the harness spells it", () => {
    // AGY names its parameters `TargetFile` and `DirectoryPath`; without this
    // its calls rendered as raw JSON.
    expect(toolDetail({ file_path: "/tmp/a.py" })).toBe("/tmp/a.py");
    expect(toolDetail({ filePath: "/tmp/a.py" })).toBe("/tmp/a.py");
    expect(toolDetail({ TargetFile: "/tmp/a.py" })).toBe("/tmp/a.py");
    expect(toolDetail({ DirectoryPath: "/tmp" })).toBe("/tmp");
  });

  it("falls back to compact JSON rather than dropping an unknown shape", () => {
    expect(toolDetail({ weird: 1 })).toBe('{"weird":1}');
  });

  it("says nothing when there is nothing to say", () => {
    expect(toolDetail(null)).toBeNull();
    expect(toolDetail(undefined)).toBeNull();
    expect(toolDetail({})).toBeNull();
    expect(toolDetail("   ")).toBeNull();
  });

  /**
   * A recognised key holding only whitespace does *not* win — the lookup falls
   * through to the JSON fallback, so the argument is still visible rather than
   * silently becoming a bare tool name. Matches the Rust `tool_detail`.
   */
  it("falls through a blank value rather than showing nothing", () => {
    // Collapsed to one line on the way out, which is why the two spaces in the
    // JSON become one.
    expect(toolDetail({ command: "  " })).toBe('{"command":" "}');
  });

  it("collapses to one line so a payload cannot own the transcript", () => {
    expect(toolDetail({ prompt: "line one\nline two" })).toBe("line one line two");
    expect(oneLine("x".repeat(200), 90)).toHaveLength(91); // 90 plus the ellipsis
    expect(oneLine("x".repeat(200), 90).endsWith("…")).toBe(true);
  });
});

describe("keeping the head of a tool's output", () => {
  it("leaves short output alone", () => {
    expect(firstLines("one\ntwo", 6)).toBe("one\ntwo");
  });

  it("truncates long output and says how much was left", () => {
    const ten = Array.from({ length: 10 }, (_, i) => `line ${i}`).join("\n");
    expect(firstLines(ten, 6)).toBe(
      "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\n… (+4 more lines)",
    );
  });

  /** A trailing newline is not a line — Rust's `str::lines` agrees. */
  it("does not count a trailing newline as a line", () => {
    const six = Array.from({ length: 6 }, (_, i) => `line ${i}`).join("\n");
    expect(firstLines(`${six}\n`, 6)).toBe(six);
  });
});

describe("watching a tool work", () => {
  it("shows a call with its most useful argument", () => {
    const state = applyEvent(session(), call("Bash", { command: "cargo test" }));
    expect(entries(state)).toEqual([
      { kind: "tool", name: "Bash", detail: "cargo test", failed: false },
    ]);
  });

  it("shows what a successful tool gave back, because details are on", () => {
    let state = applyEvent(session(), call("Bash", { command: "echo hi" }));
    state = applyEvent(state, result("Bash", "hi"));
    expect(entries(state)).toEqual([
      { kind: "tool", name: "Bash", detail: "echo hi", failed: false },
      { kind: "tool_out", text: "hi", failed: false },
    ]);
  });

  it("hides successful output once details are off, and keeps the call", () => {
    let state = toggleDetails(session()); // notice + showDetails false
    state = applyEvent(state, call("Bash", { command: "echo hi" }));
    state = applyEvent(state, result("Bash", "hi"));
    expect(entries(state).filter((e) => e.kind === "tool_out")).toEqual([]);
    expect(entries(state).some((e) => e.kind === "tool")).toBe(true);
  });

  /** A failure is the reason the answer is about to be wrong. */
  it("shows a failure even with details off", () => {
    let state = toggleDetails(session());
    state = applyEvent(state, call("Bash", { command: "false" }));
    state = applyEvent(state, result("Bash", "exit 1", true));
    expect(entries(state)).toContainEqual({
      kind: "tool_out",
      text: "exit 1",
      failed: true,
    });
  });

  /**
   * OpenCode reports a fast tool as already `completed`, so no call ever
   * arrives and the output rendered as a bare `└ Wrote file successfully.` —
   * an answer with its question missing.
   */
  it("invents the call line when a result arrives unannounced", () => {
    const state = applyEvent(session(), result("Write", "Wrote it."));
    expect(entries(state)).toEqual([
      { kind: "tool", name: "Write", detail: null, failed: false },
      { kind: "tool_out", text: "Wrote it.", failed: false },
    ]);
  });

  it("marks the call failed when the result is, even after a clean call line", () => {
    let state = applyEvent(session(), call("Bash", { command: "false" }));
    state = applyEvent(state, result("Bash", "boom", true));
    const tools = entries(state).filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    expect(tools.at(-1)).toEqual({ kind: "tool", name: "Bash", detail: null, failed: true });
  });

  it("says nothing extra for a result with no summary", () => {
    let state = applyEvent(session(), call("Read", { path: "/tmp/a" }));
    state = applyEvent(state, result("Read"));
    expect(entries(state)).toHaveLength(1);
  });

  it("announces itself when details are toggled", () => {
    expect(entries(toggleDetails(session())).at(-1)).toEqual({
      kind: "notice",
      text: "tool output hidden",
    });
    expect(entries(toggleDetails(toggleDetails(session()))).at(-1)).toEqual({
      kind: "notice",
      text: "tool output shown",
    });
  });
});

// ─── the model the harness reports ──────────────────────────────────────────

describe("what the harness says it is using", () => {
  /**
   * The bug this exists to stop: writing the reported name into the *requested*
   * one sends it straight back on the next turn, and a name one harness reports
   * is not a name another accepts.
   */
  it("is shown but never becomes the next request", () => {
    const state = applyEvent(session(), {
      kind: "started",
      session_id: "s1",
      model: "claude-opus-5",
    });
    expect(state.reportedModel).toBe("claude-opus-5");
    expect(state.model).toBeNull();
    expect(statusLine(state)).toContain("claude-opus-5");
  });

  it("beats the requested name in the status bar", () => {
    let state = setModel(session(), "opus");
    state = applyEvent(state, {
      kind: "started",
      session_id: null,
      model: "claude-opus-5",
    });
    expect(statusLine(state)).toContain("claude-opus-5");
    expect(statusLine(state)).not.toContain("· opus ·");
  });

  it("stands in with the request before the first turn", () => {
    expect(statusLine(setModel(session(), "haiku"))).toContain("haiku");
  });
});

describe("switching harness", () => {
  it("drops the old model, both names, and the old spend", () => {
    let state = session();
    state = { ...state, model: "opus", reportedModel: "claude-opus-5", costUsd: 0.11 };
    state = setHarness(state, "open_code");

    // Neither name may survive: OpenCode rejects both, and passing either made
    // the switch look like it had not happened at all.
    expect(state.model).toBeNull();
    expect(state.reportedModel).toBeNull();
    expect(state.costUsd).toBe(0);
    expect(state.harness).toBe("open_code");
  });

  /** A session id belongs to the harness that issued it. */
  it("starts a fresh conversation", () => {
    let state = resumeSession(session(), "ses-1");
    state = setHarness(state, "agy");
    expect(state.resume).toBe("fresh");
    expect(state.session).toBeNull();
  });

  it("keeps the chosen model when the harness does not actually change", () => {
    let state = setModel(session(), "haiku");
    state = setHarness(state, "claude_code");
    expect(state.model).toBe("haiku");
  });

  it("says what it did", () => {
    const state = setHarness(session(), "agy");
    expect(entries(state).at(-1)).toEqual({
      kind: "notice",
      text: "AGY from the next turn — fresh conversation, its own default model",
    });
  });

  /**
   * A run already in flight keeps streaming, exactly as the TUI's `current`
   * does: only the *next* turn is affected.
   */
  it("does not orphan a run that is already streaming", () => {
    let state = attachAgent(session(), "agent-1");
    state = setHarness(state, "agy");
    expect(state.currentAgentId).toBe("agent-1");
  });
});

describe("a fresh conversation", () => {
  it("forgets the cursor, the spend and the screen", () => {
    let state = resumeSession(session(), "ses-1");
    state = push({ ...state, costUsd: 0.5 }, { kind: "agent", text: "hi" });
    state = newConversation(state);

    expect(state.resume).toBe("fresh");
    expect(state.session).toBeNull();
    expect(state.costUsd).toBe(0);
    expect(entries(state)).toEqual([{ kind: "notice", text: "new conversation" }]);
  });
});

// ─── /resume ────────────────────────────────────────────────────────────────

function line(over: Partial<AgentLine> = {}): AgentLine {
  return {
    id: "abcdef1234",
    name: "ship it",
    harness: "Claude Code",
    status: "completed",
    session: "ses-xyz",
    ...over,
  };
}

describe("working out what /resume was given", () => {
  it("takes an exact conversation id", () => {
    const state = setAgents(session(), [line()]);
    expect(resolveSession(state, "ses-xyz")).toEqual({
      kind: "session",
      session: "ses-xyz",
    });
  });

  /**
   * The sheet shows a shortened *agent* id and `/sessions` says to feed it to
   * `/resume`, which wants a *conversation* id. A prefix of either is accepted
   * and translated, or that instruction is a trap.
   */
  it("translates a prefix of the agent id shown on screen", () => {
    const state = setAgents(session(), [line()]);
    expect(resolveSession(state, "abcdef12")).toEqual({
      kind: "session",
      session: "ses-xyz",
    });
  });

  it("passes an id it does not recognise straight through", () => {
    const state = setAgents(session(), [line()]);
    expect(resolveSession(state, "from-elsewhere")).toEqual({
      kind: "verbatim",
      typed: "from-elsewhere",
    });
  });

  /** Resuming it would silently start a fresh context instead. */
  it("refuses an agent that never reported a conversation", () => {
    const state = setAgents(session(), [line({ session: null })]);
    expect(resolveSession(state, "abcdef12")).toEqual({
      kind: "no_session",
      agent: "abcdef1234",
    });
  });

  it("asks for more when a prefix names several", () => {
    const state = setAgents(session(), [
      line({ id: "ab1", session: "s1" }),
      line({ id: "ab2", session: "s2" }),
    ]);
    expect(resolveSession(state, "ab")).toEqual({ kind: "ambiguous", count: 2 });
  });
});

// ─── the team sheet ─────────────────────────────────────────────────────────

describe("the team sheet", () => {
  it("toggles open and shut independently of the agents sheet", () => {
    expect(togglePane(session(), "team").pane).toBe("team");
    expect(togglePane(togglePane(session(), "team"), "team").pane).toBe("chat");
    // Opening one from the other switches rather than closing.
    expect(togglePane(togglePane(session(), "agents"), "team").pane).toBe("team");
  });

  it("holds the roster and board the daemon reported", () => {
    const state = setTeam(
      session(),
      "crew",
      [
        {
          team: "crew",
          name: "scout",
          harness: "agy",
          role: "research",
          status: "ready",
          agent_id: null,
          session_id: "s1",
        },
      ],
      [{ id: "t1", title: "read the docs", owner: null, status: "open" }],
    );
    expect(state.team).toBe("crew");
    expect(state.members[0]!.name).toBe("scout");
    expect(state.tasks[0]!.title).toBe("read the docs");
  });

  it("starts with no team, so the sheet can say so rather than show an empty board", () => {
    expect(session().team).toBeNull();
    expect(session().members).toEqual([]);
  });
});
