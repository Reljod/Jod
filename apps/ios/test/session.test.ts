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
  newSession,
  push,
  setFollowing,
  setInput,
  statusLine,
  takeInput,
  toAgentLines,
  togglePane,
  toggleThinking,
  usageSummary,
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
    expect(s.model).toBe("claude-opus-5");
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
    expect(s.model).toBe("claude-opus-5");
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

  it("shows a tool call", () => {
    const s = applyEvent(session(), { kind: "tool_call", name: "Bash" });
    expect(s.transcript).toEqual([{ kind: "tool", name: "Bash", failed: false }]);
  });

  it("drops a tool result that worked, because it is noise", () => {
    const s = applyEvent(session(), {
      kind: "tool_result",
      name: "Bash",
      is_error: false,
    });
    expect(s.transcript).toEqual([]);
  });

  it("keeps a tool result that failed, because it explains the wrong answer", () => {
    const s = applyEvent(session(), {
      kind: "tool_result",
      name: "Bash",
      is_error: true,
    });
    expect(s.transcript).toEqual([{ kind: "tool", name: "Bash", failed: true }]);
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
    const s = applyEvent(session(), { kind: "error", message: "tmux is missing" });
    expect(s.transcript).toEqual([{ kind: "notice", text: "tmux is missing" }]);
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
    expect(togglePane(session()).pane).toBe("agents");
    expect(togglePane(togglePane(session())).pane).toBe("chat");
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
