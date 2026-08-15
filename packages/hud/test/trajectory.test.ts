import { describe, expect, it } from "vitest";
import { WorldStore } from "../src/state/world";
import type { AgentNode } from "../src/state/world";
import { buildTrajectory, byTurn, filterRows } from "../src/graph/trajectory";
import type { AgentEnvelope, AgentSummary, Report } from "../src/types";

const T0 = 1_700_000_000_000;
const NOW = T0 + 60_000;

function agent(over: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "a1",
    name: "probe",
    harness: "claude_code",
    harness_label: "Claude Code",
    status: "running",
    cwd: "/repo/Jod",
    model: "claude-opus-5",
    permission: "ask",
    pid: 4242,
    pgid: 4242,
    process_alive: true,
    watch_command: "jod watch a1",
    created_at_ms: T0,
    session_id: "sess-1",
    usage: {},
    event_count: 0,
    last_message: null,
    ...over,
  };
}

function report(a: AgentSummary): Report {
  return { running: 1, completed: 0, failed: 0, killed: 0, total_cost_usd: 0, agents: [a] };
}

/** Envelopes numbered from 0, because the API's cursor is exclusive and seq 0 is real. */
function stream(
  events: (Partial<AgentEnvelope> & Pick<AgentEnvelope, "kind">)[],
  startSeq = 0,
): AgentEnvelope[] {
  return events.map(
    (e, i) =>
      ({ agent_id: "a1", at_ms: T0 + i * 1000, seq: startSeq + i, ...e }) as AgentEnvelope,
  );
}

/** A node with a real fold behind it — the same path the live HUD takes. */
function nodeWith(events: AgentEnvelope[], over: Partial<AgentSummary> = {}): AgentNode {
  const store = new WorldStore();
  store.setReport(report(agent(over)));
  for (const e of events) store.ingest(e);
  return store.world.agents.get("a1")!;
}

describe("buildTrajectory", () => {
  it("opens on the session's setup even when the run has emitted nothing", () => {
    const t = buildTrajectory(nodeWith([]), { now: NOW });

    expect(t.rows).toHaveLength(1);
    expect(t.rows[0].kind).toBe("system");
    expect(t.rows[0].summary).toContain("claude-opus-5");
    expect(t.rows[0].summary).toContain("sess-1");
    expect(t.turns).toBe(0);
  });

  /**
   * The one row this view must never invent. No harness reports its system
   * prompt, so the SYSTEM row states the settings and says so — a fabricated
   * prompt would be read as the real one.
   */
  it("says plainly that the harness system prompt is not in the stream", () => {
    const t = buildTrajectory(nodeWith([]), { now: NOW });

    expect(t.rows[0].detail).toContain("no harness system prompt");
    expect(t.rows[0].detail).toContain("/repo/Jod");
    expect(t.rows[0].detail).toContain("pgid 4242");
  });

  it("shows the opening prompt as a USER turn when the transcript store had one", () => {
    const withPrompt = buildTrajectory(nodeWith([]), { now: NOW, prompt: "ship the HUD" });
    expect(withPrompt.rows.map((r) => r.kind)).toEqual(["system", "user"]);
    expect(withPrompt.rows[1].detail).toBe("ship the HUD");

    // And nothing at all when it could not be recovered, rather than a guess.
    const without = buildTrajectory(nodeWith([]), { now: NOW });
    expect(without.rows.some((r) => r.kind === "user")).toBe(false);
  });

  it("counts one turn per model invocation, not one per message", () => {
    const t = buildTrajectory(
      nodeWith(
        stream([
          { kind: "started", session_id: "sess-1", model: "claude-opus-5" },
          { kind: "thinking", text: "planning" },
          { kind: "message", text: "on it" },
          { kind: "tool_call", name: "Read", input: { file_path: "/repo/AGENTS.md" } },
          { kind: "tool_result", name: "Read", summary: "charter", is_error: false },
          // The model is invoked again here — turn 2.
          { kind: "message", text: "done" },
        ]),
      ),
      { now: NOW },
    );

    expect(t.turns).toBe(2);
    expect(t.toolCalls).toBe(1);
    const turns = byTurn(t.rows).map((g) => g.turn);
    expect(turns).toEqual([0, 1, 2]);
  });

  it("pairs a result onto its call and measures how long the call took", () => {
    const t = buildTrajectory(
      nodeWith([
        ...stream([{ kind: "tool_call", name: "Bash", input: { command: "cargo test" } }]),
        ...stream(
          [{ kind: "tool_result", name: "Bash", summary: "42 passed", is_error: false }],
          1,
        ).map((e) => ({ ...e, at_ms: T0 + 8_000 })),
      ]),
      { now: NOW },
    );

    const tool = t.rows.find((r) => r.kind === "tool")!;
    expect(tool.summary).toBe("Bash(cargo test)");
    expect(tool.durationMs).toBe(8_000);
    expect(tool.open).toBe(false);
    expect(tool.detail).toContain("cargo test");
    expect(tool.detail).toContain("42 passed");
    // One row, not two: a result is the closing half of its call.
    expect(t.rows.filter((r) => r.kind === "tool")).toHaveLength(1);
  });

  it("pairs same-named calls in the order they were issued", () => {
    const t = buildTrajectory(
      nodeWith(
        stream([
          { kind: "tool_call", name: "Read", input: { file_path: "/a" } },
          { kind: "tool_call", name: "Read", input: { file_path: "/b" } },
          { kind: "tool_result", name: "Read", summary: "b came back first", is_error: true },
        ]),
      ),
      { now: NOW },
    );

    const reads = t.rows.filter((r) => r.kind === "tool");
    expect(reads).toHaveLength(2);
    // The newest open call takes the result, so /b is the one that errored.
    expect(reads[1].isError).toBe(true);
    expect(reads[0].isError).toBe(false);
    expect(reads[0].open).toBe(true);
  });

  it("leaves a call that never returned open on a live run", () => {
    const live = buildTrajectory(
      nodeWith(stream([{ kind: "tool_call", name: "Bash", input: { command: "sleep 900" } }])),
      { now: NOW },
    );
    expect(live.rows.find((r) => r.kind === "tool")?.open).toBe(true);

    // On a run that is over, the same call is abandoned rather than in flight.
    const dead = buildTrajectory(
      nodeWith(stream([{ kind: "tool_call", name: "Bash", input: { command: "sleep 900" } }]), {
        status: "failed",
      }),
      { now: NOW },
    );
    expect(dead.rows.find((r) => r.kind === "tool")?.open).toBe(false);
  });

  /**
   * The gap this view exists to close. A nine-minute think emits only ticks,
   * and a transcript that drops them jumps from a tool result to a message with
   * nothing in between — which is what a hung run looks like.
   */
  it("collapses tick and fragment frames into one working row rather than dropping them", () => {
    const frames: (Partial<AgentEnvelope> & Pick<AgentEnvelope, "kind">)[] = [];
    for (let i = 0; i < 40; i++) {
      frames.push({ kind: "progress", thinking_tokens: (i + 1) * 100 });
    }
    const t = buildTrajectory(
      nodeWith([
        ...stream([{ kind: "tool_result", name: "Bash", summary: "ok", is_error: false }]),
        ...stream(frames, 1),
        ...stream([{ kind: "message", text: "finally" }], 41),
      ]),
      { now: NOW },
    );

    const working = t.rows.filter((r) => r.kind === "stream");
    expect(working).toHaveLength(1);
    expect(working[0].frames).toBe(40);
    expect(working[0].summary).toContain("4,000 thinking tokens");
    expect(working[0].durationMs).toBe(39_000);
    // It resolved: the message it was waiting for arrived.
    expect(working[0].open).toBe(false);
  });

  /**
   * Observed live against the daemon: recent models emit thinking blocks with
   * the reasoning text withheld, so the stream carries `thinking` events whose
   * text is empty. A blank row reads as a broken view rather than as the thing
   * that actually happened.
   */
  it("names a block the harness delivered with no text in it", () => {
    const t = buildTrajectory(
      nodeWith(
        stream([
          { kind: "thinking", text: "" },
          { kind: "message", text: "   " },
        ]),
      ),
      { now: NOW },
    );

    const think = t.rows.find((r) => r.kind === "thinking")!;
    expect(think.summary).toBe("reasoning block with no text");
    // Nothing to expand into, so the row is not offered as expandable.
    expect(think.detail).toBeNull();
    expect(t.rows.find((r) => r.kind === "assistant")!.summary).toBe(
      "message block with no text",
    );
  });

  it("keeps a raw line and a lost session as their own rows", () => {
    const t = buildTrajectory(
      nodeWith(
        stream([
          { kind: "raw", line: "{\"type\":\"whatever\"}" },
          { kind: "session_lost", session_id: "sess-gone" },
        ]),
      ),
      { now: NOW },
    );

    expect(t.rows.find((r) => r.kind === "raw")?.detail).toContain("whatever");
    const lost = t.rows.find((r) => r.kind === "error")!;
    expect(lost.summary).toContain("sess-gone");
    expect(lost.isError).toBe(true);
  });

  it("puts the run's usage on the finish row", () => {
    const t = buildTrajectory(
      nodeWith(
        stream([
          {
            kind: "finished",
            text: "shipped",
            exit_code: 0,
            is_error: false,
            usage: { input_tokens: 1200, output_tokens: 340, cost_usd: 0.0812 },
          },
        ]),
      ),
      { now: NOW },
    );

    const finish = t.rows.find((r) => r.kind === "finish")!;
    expect(finish.summary).toBe("shipped");
    expect(finish.detail).toContain("1,200");
    expect(finish.detail).toContain("$0.0812");
  });

  it("reports history the retention cap dropped instead of starting mid-run", () => {
    // A run adopted mid-flight: its first retained event is seq 400.
    const t = buildTrajectory(nodeWith(stream([{ kind: "message", text: "…" }], 400)), {
      now: NOW,
    });

    expect(t.dropped).toBe(400);
    expect(t.complete).toBe(false);
  });

  it("ends a live run's timeline at now, and a finished one at its last event", () => {
    const events = stream([
      { kind: "message", text: "one" },
      { kind: "message", text: "two" },
    ]);

    expect(buildTrajectory(nodeWith(events), { now: NOW }).endedAt).toBe(NOW);
    expect(
      buildTrajectory(nodeWith(events, { status: "completed" }), { now: NOW }).endedAt,
    ).toBe(T0 + 1000);
  });
});

describe("the band", () => {
  const events = stream([
    { kind: "thinking", text: "planning" },
    { kind: "tool_call", name: "Bash", input: { command: "ls" } },
    { kind: "tool_result", name: "Bash", summary: "ok", is_error: false },
    { kind: "message", text: "done" },
  ]);

  it("puts model work and tool work in their own lanes", () => {
    const t = buildTrajectory(nodeWith(events, { status: "completed" }), { now: NOW });
    const lanes = t.band.map((s) => s.lane);

    expect(lanes).toContain("model");
    expect(lanes).toContain("tools");
    expect(t.band.find((s) => s.label === "Bash")?.lane).toBe("tools");
  });

  it("spans the whole width under duration, in order and without gaps", () => {
    const t = buildTrajectory(nodeWith(events, { status: "completed" }), {
      now: NOW,
      scale: "duration",
    });

    expect(t.band[0].from).toBe(0);
    expect(t.band[t.band.length - 1].to).toBeCloseTo(1, 5);
    for (const s of t.band) {
      expect(s.from).toBeGreaterThanOrEqual(0);
      expect(s.to).toBeLessThanOrEqual(1);
      expect(s.to).toBeGreaterThanOrEqual(s.from);
    }
  });

  /**
   * The reason the other two scales exist: a call that took a thousandth of the
   * run is invisible under `duration` and readable under `calls`.
   */
  it("gives every block equal width under calls, whatever the wall-clock said", () => {
    const lopsided = [
      ...stream([{ kind: "tool_call", name: "Fast", input: {} }]),
      ...stream([{ kind: "tool_result", name: "Fast", is_error: false }], 1).map((e) => ({
        ...e,
        at_ms: T0 + 5,
      })),
      ...stream([{ kind: "message", text: "after a long wait" }], 2).map((e) => ({
        ...e,
        at_ms: T0 + 600_000,
      })),
    ];

    const byTime = buildTrajectory(nodeWith(lopsided, { status: "completed" }), {
      now: NOW,
      scale: "duration",
    });
    const fast = byTime.band.find((s) => s.label === "Fast")!;
    expect(fast.to - fast.from).toBeLessThan(0.001);

    const byCall = buildTrajectory(nodeWith(lopsided, { status: "completed" }), {
      now: NOW,
      scale: "calls",
    });
    const widths = byCall.band.map((s) => s.to - s.from);
    for (const w of widths) expect(w).toBeCloseTo(widths[0], 10);
  });

  it("gives every turn an equal slice under turns", () => {
    const twoTurns = stream([
      { kind: "message", text: "turn one" },
      { kind: "tool_call", name: "Read", input: {} },
      { kind: "tool_result", name: "Read", is_error: false },
      { kind: "message", text: "turn two" },
    ]);
    const t = buildTrajectory(nodeWith(twoTurns, { status: "completed" }), {
      now: NOW,
      scale: "turns",
    });

    const turnOne = t.band.filter((s) => s.turn === 1);
    const turnTwo = t.band.filter((s) => s.turn === 2);
    const width = (segs: typeof t.band) =>
      Math.max(...segs.map((s) => s.to)) - Math.min(...segs.map((s) => s.from));

    expect(width(turnOne)).toBeCloseTo(width(turnTwo), 10);
  });
});

describe("filterRows", () => {
  const t = buildTrajectory(
    nodeWith(
      stream([
        { kind: "message", text: "rebasing onto main" },
        { kind: "tool_call", name: "Bash", input: { command: "git rebase main" } },
        { kind: "tool_result", name: "Bash", summary: "ok", is_error: false },
      ]),
    ),
    { now: NOW },
  );

  it("matches the badge, the tool name and the summary", () => {
    expect(filterRows(t.rows, "TOOL").every((r) => r.kind === "tool")).toBe(true);
    expect(filterRows(t.rows, "bash")).toHaveLength(1);
    expect(filterRows(t.rows, "rebasing")).toHaveLength(1);
  });

  /** A row's detail is text the view will display, so search has to reach it. */
  it("matches text that is only in the expanded detail", () => {
    expect(filterRows(t.rows, "git rebase main")).toHaveLength(1);
    expect(filterRows(t.rows, "pgid")).toHaveLength(1);
  });

  it("returns everything for an empty query", () => {
    expect(filterRows(t.rows, "   ")).toHaveLength(t.rows.length);
  });
});
