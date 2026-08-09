import { describe, expect, it } from "vitest";
import { WorldStore, describe as describeEvent, eventRate } from "../src/state/world";
import type { AgentEnvelope, AgentSummary, Report } from "../src/types";

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
    tmux_session: "jod-probe",
    attach_command: "tmux attach -t jod-probe",
    switch_command: "tmux switch-client -t jod-probe",
    session_closed: false,
    created_at_ms: 1_000_000,
    session_id: null,
    usage: {},
    event_count: 0,
    last_message: null,
    stream_path: "/tmp/a1.jsonl",
    ...over,
  };
}

function report(agents: AgentSummary[]): Report {
  return {
    running: agents.filter((a) => a.status === "running").length,
    completed: 0,
    failed: 0,
    killed: 0,
    total_cost_usd: 0,
    agents,
  };
}

let seq = 0;
function env(over: Partial<AgentEnvelope> & Pick<AgentEnvelope, "kind">): AgentEnvelope {
  return { agent_id: "a1", at_ms: 2_000_000, seq: ++seq, ...over } as AgentEnvelope;
}

describe("WorldStore", () => {
  it("creates a node from the roster and keeps insertion order", () => {
    const store = new WorldStore();
    store.setReport(report([agent({ id: "a" }), agent({ id: "b" })]));

    expect(store.world.order).toEqual(["a", "b"]);
    expect(store.world.agents.get("a")?.phase).toBe("booting");
  });

  it("ignores an event for an agent the roster has not introduced yet", () => {
    const store = new WorldStore();
    expect(() => store.ingest(env({ kind: "message", text: "hi" }))).not.toThrow();
    expect(store.world.feed).toHaveLength(0);
  });

  it("pairs a tool_result with its in-flight tool_call", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));

    store.ingest(env({ kind: "tool_call", name: "Bash", input: { command: "ls" } }));
    const node = store.world.agents.get("a1")!;
    expect(node.phase).toBe("acting");
    expect(node.inFlight?.name).toBe("Bash");

    store.ingest(env({ kind: "tool_result", name: "Bash", summary: "ok", is_error: false }));
    expect(node.inFlight).toBeNull();
    expect(node.tools[0].endedAt).not.toBeNull();
    expect(node.tools[0].summary).toBe("ok");
    expect(node.phase).toBe("thinking");
  });

  it("closes the right call when two tools are open at once", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));

    store.ingest(env({ kind: "tool_call", name: "Read", input: {} }));
    store.ingest(env({ kind: "tool_call", name: "Bash", input: {} }));
    store.ingest(env({ kind: "tool_result", name: "Read", summary: "done", is_error: false }));

    const node = store.world.agents.get("a1")!;
    const read = node.tools.find((t) => t.name === "Read")!;
    const bash = node.tools.find((t) => t.name === "Bash")!;
    expect(read.endedAt).not.toBeNull();
    expect(bash.endedAt).toBeNull();
  });

  it("counts faults from both error events and failed tool results", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));

    store.ingest(env({ kind: "tool_call", name: "Bash", input: {} }));
    store.ingest(env({ kind: "tool_result", name: "Bash", summary: "boom", is_error: true }));
    store.ingest(env({ kind: "error", message: "runner died" }));

    expect(store.world.agents.get("a1")!.errorCount).toBe(2);
  });

  it("marks a failed finish as failed, not completed", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    store.ingest(
      env({ kind: "finished", is_error: true, exit_code: 3, usage: { cost_usd: 0.27 } }),
    );

    const node = store.world.agents.get("a1")!;
    expect(node.summary.status).toBe("failed");
    expect(node.phase).toBe("failed");
    expect(node.summary.session_closed).toBe(true);
    expect(node.summary.usage.cost_usd).toBe(0.27);
  });

  it("keeps the feed capped and in arrival order", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    for (let i = 0; i < 400; i++) store.ingest(env({ kind: "message", text: `m${i}` }));

    const feed = store.world.feed;
    expect(feed.length).toBeLessThanOrEqual(300);
    expect(feed.at(-1)!.text).toBe("m399");
  });

  it("cools an agent that stops emitting", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    store.ingest(env({ kind: "message", text: "hello" }));

    const node = store.world.agents.get("a1")!;
    const hot = node.heat;
    store.tick(Date.now(), 4000);
    expect(node.heat).toBeLessThan(hot);
  });

  it("marks a silent running agent idle", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    const now = Date.now();
    store.ingest(env({ kind: "message", text: "hi", at_ms: now }));

    store.tick(now + 20_000, 16);
    expect(store.world.agents.get("a1")!.phase).toBe("idle");
  });

  it("does not idle an agent that is blocked inside a long tool call", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    const now = Date.now();
    store.ingest(env({ kind: "tool_call", name: "Bash", input: {}, at_ms: now }));

    store.tick(now + 40_000, 16);
    expect(store.world.agents.get("a1")!.phase).toBe("acting");
  });

  it("only notifies subscribers once per flush, and not when clean", () => {
    const store = new WorldStore();
    let calls = 0;
    store.subscribe(() => calls++);

    store.setReport(report([agent()]));
    store.ingest(env({ kind: "message", text: "a" }));
    store.ingest(env({ kind: "message", text: "b" }));
    store.flush();
    expect(calls).toBe(1);

    store.flush();
    expect(calls).toBe(1);
  });

  it("reaps only pulses older than the lifetime", () => {
    const store = new WorldStore();
    store.setReport(report([agent()]));
    store.ingest(env({ kind: "message", text: "x" }));
    expect(store.world.pulses.length).toBe(1);

    const born = store.world.pulses[0].born;
    store.reapPulses(born + 500, 1000);
    expect(store.world.pulses.length).toBe(1);
    store.reapPulses(born + 1500, 1000);
    expect(store.world.pulses.length).toBe(0);
  });
});

describe("describe", () => {
  it("renders every one of the eight event kinds", () => {
    const kinds: AgentEnvelope[] = [
      env({ kind: "started", session_id: "s1", model: "m" }),
      env({ kind: "thinking", text: "hm" }),
      env({ kind: "message", text: "hi" }),
      env({ kind: "tool_call", name: "Bash", input: { command: "ls -la" } }),
      env({ kind: "tool_result", name: "Bash", summary: "ok", is_error: false }),
      env({ kind: "finished", is_error: false, usage: {} }),
      env({ kind: "raw", line: "{unparsed}" }),
      env({ kind: "error", message: "nope" }),
    ];
    for (const e of kinds) {
      const text = describeEvent(e);
      expect(typeof text).toBe("string");
      expect(text.length).toBeGreaterThan(0);
    }
  });

  it("surfaces the salient field of a tool input rather than raw JSON", () => {
    expect(describeEvent(env({ kind: "tool_call", name: "Bash", input: { command: "ls -la" } })))
      .toContain("ls -la");
    expect(describeEvent(env({ kind: "tool_call", name: "Read", input: { file_path: "/x/y.rs" } })))
      .toContain("/x/y.rs");
  });
});

describe("eventRate", () => {
  it("counts only events inside the window", () => {
    const now = 100_000;
    const node = { recentEventTimes: [now - 30_000, now - 1000, now - 500] } as never;
    expect(eventRate(node, now, 10_000)).toBeCloseTo(0.2, 5);
  });
});
