import { describe, expect, it } from "vitest";
import { buildLanes, ticks, windowFor } from "../src/graph/timeline";
import { WorldStore } from "../src/state/world";
import type { AgentEnvelope, AgentSummary, Report } from "../src/types";

const NOW = 1_000_000_000;
const SPAN = 120_000;

function agent(id: string): AgentSummary {
  return {
    id,
    name: id,
    harness: "claude_code",
    harness_label: "Claude Code",
    status: "running",
    cwd: "/repo/Jod",
    model: null,
    permission: "ask",
    tmux_session: `t-${id}`,
    attach_command: "",
    switch_command: "",
    session_closed: false,
    created_at_ms: NOW - SPAN,
    session_id: null,
    usage: {},
    event_count: 0,
    last_message: null,
    stream_path: "",
  };
}

function report(ids: string[]): Report {
  return {
    running: ids.length,
    completed: 0,
    failed: 0,
    killed: 0,
    total_cost_usd: 0,
    agents: ids.map(agent),
  };
}

let seq = 0;
function ev(over: Partial<AgentEnvelope> & Pick<AgentEnvelope, "kind">): AgentEnvelope {
  return { agent_id: "a", at_ms: NOW, seq: ++seq, ...over } as AgentEnvelope;
}

function storeWith(fn: (s: WorldStore) => void): WorldStore {
  const s = new WorldStore();
  s.setReport(report(["a"]));
  fn(s);
  return s;
}

const W = windowFor(NOW, SPAN);

describe("buildLanes", () => {
  it("returns one lane per requested agent", () => {
    const s = new WorldStore();
    s.setReport(report(["a", "b"]));
    expect(buildLanes(s.world, ["a", "b"], W)).toHaveLength(2);
  });

  it("skips ids that are not in the world", () => {
    const s = new WorldStore();
    s.setReport(report(["a"]));
    expect(buildLanes(s.world, ["a", "ghost"], W)).toHaveLength(1);
  });

  it("places a completed tool span at the right fraction of the window", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "Bash", input: {}, at_ms: NOW - 60_000 }));
      st.ingest(
        ev({ kind: "tool_result", name: "Bash", is_error: false, at_ms: NOW - 30_000 }),
      );
    });
    const [lane] = buildLanes(s.world, ["a"], W);
    expect(lane.spans).toHaveLength(1);
    expect(lane.spans[0].from).toBeCloseTo(0.5, 5);
    expect(lane.spans[0].to).toBeCloseTo(0.75, 5);
    expect(lane.spans[0].open).toBe(false);
  });

  it("runs an in-flight span all the way to now and marks it open", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "Bash", input: {}, at_ms: NOW - 30_000 }));
    });
    const [lane] = buildLanes(s.world, ["a"], W);
    expect(lane.spans[0].open).toBe(true);
    expect(lane.spans[0].to).toBeCloseTo(1, 5);
  });

  it("clips a span that began before the window instead of dropping it", () => {
    // A cargo test running for ten minutes must not make the agent look idle.
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "Bash", input: {}, at_ms: NOW - 600_000 }));
    });
    const [lane] = buildLanes(s.world, ["a"], W);
    expect(lane.spans).toHaveLength(1);
    expect(lane.spans[0].from).toBe(0);
    expect(lane.spans[0].to).toBeCloseTo(1, 5);
  });

  it("drops a span that finished entirely before the window", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "Old", input: {}, at_ms: NOW - 900_000 }));
      st.ingest(ev({ kind: "tool_result", name: "Old", is_error: false, at_ms: NOW - 800_000 }));
    });
    expect(buildLanes(s.world, ["a"], W)[0].spans).toHaveLength(0);
  });

  it("carries the error flag onto the span", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "Bash", input: {}, at_ms: NOW - 20_000 }));
      st.ingest(ev({ kind: "tool_result", name: "Bash", is_error: true, at_ms: NOW - 10_000 }));
    });
    expect(buildLanes(s.world, ["a"], W)[0].spans[0].isError).toBe(true);
  });

  it("keeps every fraction inside 0..1", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "tool_call", name: "A", input: {}, at_ms: NOW - 999_000 }));
      st.ingest(ev({ kind: "message", text: "hi", at_ms: NOW - 1000 }));
    });
    const [lane] = buildLanes(s.world, ["a"], W);
    for (const s2 of lane.spans) {
      expect(s2.from).toBeGreaterThanOrEqual(0);
      expect(s2.to).toBeLessThanOrEqual(1);
    }
    for (const m of lane.marks) {
      expect(m.at).toBeGreaterThanOrEqual(0);
      expect(m.at).toBeLessThanOrEqual(1);
    }
  });

  it("marks messages, errors and finishes but not thinking", () => {
    const s = storeWith((st) => {
      st.ingest(ev({ kind: "message", text: "m", at_ms: NOW - 5000 }));
      st.ingest(ev({ kind: "thinking", text: "t", at_ms: NOW - 4000 }));
      st.ingest(ev({ kind: "error", message: "e", at_ms: NOW - 3000 }));
    });
    const kinds = buildLanes(s.world, ["a"], W)[0].marks.map((m) => m.kind);
    expect(kinds).toContain("message");
    expect(kinds).toContain("error");
    expect(kinds).not.toContain("thinking");
  });

  it("gives an agent with no traffic an empty but present lane", () => {
    const s = new WorldStore();
    s.setReport(report(["a"]));
    const [lane] = buildLanes(s.world, ["a"], W);
    expect(lane.spans).toHaveLength(0);
    expect(lane.marks).toHaveLength(0);
    expect(lane.name).toBe("a");
  });
});

describe("ticks", () => {
  it("labels the right edge as now and spans the window", () => {
    const t = ticks(W, 4);
    expect(t).toHaveLength(5);
    expect(t[0].at).toBe(0);
    expect(t.at(-1)!.at).toBe(1);
    expect(t.at(-1)!.label).toBe("now");
    expect(t[0].label).toBe("-120s");
  });
});
