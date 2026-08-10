import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { SimTransport } from "../src/transport/sim";
import type { AgentEnvelope, Report } from "../src/types";
import type { LinkState } from "../src/transport";

/**
 * The simulation driver is the only thing exercising the client end-to-end
 * until a real orchestrator is on the machine, so the properties the HTTP
 * driver depends on have to hold here too — above all that `seq` starts at 0.
 */
function collect() {
  const envelopes: AgentEnvelope[] = [];
  const reports: Report[] = [];
  const links: LinkState[] = [];
  return {
    envelopes,
    reports,
    links,
    handlers: {
      onEnvelope: (e: AgentEnvelope) => envelopes.push(e),
      onReport: (r: Report) => reports.push(r),
      onLink: (l: LinkState) => links.push(l),
    },
  };
}

describe("SimTransport", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("numbers each agent's first event seq 0, matching the API", () => {
    // `after_seq` is exclusive over a sequence starting at 0, so a driver that
    // seeded from 1 would hide the off-by-one this whole client has to get right.
    const c = collect();
    const t = new SimTransport("test");
    t.start(c.handlers);
    vi.advanceTimersByTime(600);

    const first = c.envelopes[0];
    expect(first).toBeDefined();
    expect(first.seq).toBe(0);
    expect(first.kind).toBe("started");
    t.stop();
  });

  it("increments seq contiguously per agent", () => {
    const c = collect();
    const t = new SimTransport("test");
    t.start(c.handlers);
    vi.advanceTimersByTime(20_000);

    const byAgent = new Map<string, number[]>();
    for (const e of c.envelopes) {
      byAgent.set(e.agent_id, [...(byAgent.get(e.agent_id) ?? []), e.seq]);
    }
    expect(byAgent.size).toBeGreaterThan(0);
    for (const seqs of byAgent.values()) {
      expect(seqs).toEqual(seqs.map((_, i) => i));
    }
    t.stop();
  });

  it("returns the started event when the cursor is omitted, and skips it at 0", async () => {
    const c = collect();
    const t = new SimTransport("test");
    t.start(c.handlers);
    vi.advanceTimersByTime(8000);

    const id = c.envelopes[0].agent_id;
    const all = await t.events(id);
    const after0 = await t.events(id, 0);

    expect(all[0].seq).toBe(0);
    expect(all[0].kind).toBe("started");
    // The exact trap the API session flagged: `?after_seq=0` drops `started`.
    expect(after0.every((e) => e.seq > 0)).toBe(true);
    expect(all.length).toBe(after0.length + 1);
    t.stop();
  });

  it("reports itself as a simulated link, with a reason", () => {
    const c = collect();
    const t = new SimTransport("no orchestrator");
    t.start(c.handlers);
    expect(c.links[0]).toEqual({ phase: "simulated", reason: "no orchestrator" });
    t.stop();
  });

  it("is always writable — nothing real can happen in simulation", async () => {
    const t = new SimTransport("test");
    expect(await t.authenticate()).toBe("write");
  });

  it("replays identically for the same seed", () => {
    const run = () => {
      const c = collect();
      const t = new SimTransport("test", 1234);
      t.start(c.handlers);
      vi.advanceTimersByTime(12_000);
      t.stop();
      return c.envelopes.map((e) => `${e.seq}:${e.kind}`);
    };
    expect(run()).toEqual(run());
  });

  it("stops emitting once stopped", () => {
    const c = collect();
    const t = new SimTransport("test");
    t.start(c.handlers);
    vi.advanceTimersByTime(5000);
    const n = c.envelopes.length;
    t.stop();
    vi.advanceTimersByTime(30_000);
    expect(c.envelopes.length).toBe(n);
  });

  it("marks a killed agent killed and closes its session", async () => {
    const c = collect();
    const t = new SimTransport("test");
    t.start(c.handlers);
    vi.advanceTimersByTime(3000);

    const id = c.reports.at(-1)!.agents[0].id;
    await t.kill(id);

    const agent = c.reports.at(-1)!.agents.find((a) => a.id === id)!;
    expect(agent.status).toBe("killed");
    expect(agent.process_alive).toBe(false);
    t.stop();
  });
});
