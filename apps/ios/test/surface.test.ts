/**
 * The parts of the client and the store that the flow suites never reach.
 *
 * Everything here is small and boring on purpose — a pane toggle, an error
 * stringifier, the fallback that keeps a spawn working on a phone without a
 * secure context. They are also the parts most likely to be quietly broken by a
 * refactor, precisely because no bigger test walks through them.
 */

import { describe as suite, expect, it } from "vitest";

import { JodClient } from "../src/client";
import { Conversation, describe as describeThrown } from "../src/conversation";
import { FakeFetch, EventSourceSpy, agent } from "./fakes";

function build(http: FakeFetch): Conversation {
  return new Conversation({
    client: new JodClient({
      fetch: http.fetch,
      eventSource: new EventSourceSpy().factory,
      newKey: () => "key-1",
    }),
    scopeMemory: { read: () => "write", write: () => {} },
  });
}

suite("reading the fleet", () => {
  it("lists the agents the daemon knows about", async () => {
    const http = new FakeFetch();
    http.on("GET /v1/agents", { body: [agent({ id: "a1", name: "scout" })] });
    const client = new JodClient({ fetch: http.fetch });

    const agents = await client.agents();

    expect(agents.map((a) => a.id)).toEqual(["a1"]);
  });

  it("fetches the report", async () => {
    const http = new FakeFetch();
    http.on("GET /v1/report", {
      body: { running: 1, completed: 2, failed: 0, killed: 0, total_cost_usd: 0.5, agents: [] },
    });
    const client = new JodClient({ fetch: http.fetch });

    const report = await client.report();

    expect(report.running).toBe(1);
    expect(report.total_cost_usd).toBe(0.5);
  });
});

suite("the idempotency key", () => {
  /**
   * `crypto.randomUUID` needs a secure context. A phone reaching the daemon
   * over plain http on the tailnet is not one, and a spawn that threw there
   * would be worse than a weaker key.
   */
  it("falls back to random bytes where randomUUID is unavailable", () => {
    const original = globalThis.crypto;
    try {
      Object.defineProperty(globalThis, "crypto", {
        value: {
          getRandomValues: (b: Uint8Array) => {
            b.fill(0xab);
            return b;
          },
        },
        configurable: true,
      });

      const http = new FakeFetch();
      http.on("POST /v1/agents", { status: 201, body: agent({ id: "a1" }) });
      const client = new JodClient({ fetch: http.fetch });

      return client.spawn({ prompt: "hi" }).then(() => {
        const key = http.calls[0].headers["idempotency-key"];
        expect(key).toBe("ab".repeat(16));
      });
    } finally {
      Object.defineProperty(globalThis, "crypto", { value: original, configurable: true });
    }
  });

  /** No randomness source at all must still produce a key, not throw. */
  it("still produces a key with no randomness source at all", async () => {
    const original = globalThis.crypto;
    try {
      Object.defineProperty(globalThis, "crypto", { value: undefined, configurable: true });

      const http = new FakeFetch();
      http.on("POST /v1/agents", { status: 201, body: agent({ id: "a1" }) });
      const client = new JodClient({ fetch: http.fetch });

      await client.spawn({ prompt: "hi" });

      expect(http.calls[0].headers["idempotency-key"]).toHaveLength(32);
    } finally {
      Object.defineProperty(globalThis, "crypto", { value: original, configurable: true });
    }
  });
});

suite("panes and the transcript", () => {
  it("switches pane and back", () => {
    const c = build(new FakeFetch());
    const first = c.getSnapshot().session.pane;

    c.setPane("agents");
    expect(c.getSnapshot().session.pane).toBe("agents");

    c.setPane(first);
    expect(c.getSnapshot().session.pane).toBe(first);
  });

  it("clears the transcript", () => {
    const c = build(new FakeFetch());
    c.greet("welcome");
    expect(c.getSnapshot().session.transcript.length).toBeGreaterThan(0);

    c.clear();

    expect(c.getSnapshot().session.transcript).toHaveLength(0);
  });

  it("greets with a notice, mirroring the TUI's opening line", () => {
    const c = build(new FakeFetch());
    c.greet("type to talk");

    const [entry] = c.getSnapshot().session.transcript;
    expect(entry.kind).toBe("notice");
    // `Entry` is a union and only some arms carry `text`, so narrow first.
    if (entry.kind !== "tool") expect(entry.text).toContain("type to talk");
  });
});

suite("making a thrown value readable", () => {
  it("uses an Error's message", () => {
    expect(describeThrown(new Error("it broke"))).toBe("it broke");
  });

  it("passes a thrown string straight through", () => {
    expect(describeThrown("plain failure")).toBe("plain failure");
  });

  /** A thrown object must not surface as "[object Object]" with no context. */
  it("stringifies anything else rather than throwing itself", () => {
    expect(describeThrown(404)).toBe("404");
    expect(typeof describeThrown({ odd: true })).toBe("string");
    expect(describeThrown(null)).toBe("null");
  });
});
