/**
 * The transport, against the frozen `jod-api` contract.
 *
 * The tests worth having here are the ones about the contract's sharp edges —
 * the exclusive `after_seq` cursor, the flattened envelope, the problem+json
 * error body — because those are where a client goes subtly wrong and still
 * looks like it is working.
 */

import { describe, expect, it } from "vitest";

import {
  JodClient,
  UnauthorizedError,
  normaliseScope,
  parseEnvelope,
  parseMissed,
  problemDetail,
} from "../src/client";
import { EventSourceSpy, FakeFetch, agent } from "./fakes";

function client(http: FakeFetch, spy = new EventSourceSpy()) {
  return {
    client: new JodClient({
      fetch: http.fetch,
      eventSource: spy.factory,
      newKey: () => "key-1",
    }),
    spy,
  };
}

describe("authenticating", () => {
  it("presents the bearer token and returns the scope", async () => {
    const http = new FakeFetch().on("POST /v1/session", {
      status: 201,
      body: { scope: "write", expires_at_ms: 42 },
    });
    const info = await client(http).client.authenticate("  tok-abc  ");

    expect(info).toEqual({ scope: "write", expires_at_ms: 42 });
    const call = http.callsTo("POST /v1/session")[0];
    // Trimmed, because a token pasted on a phone arrives with whitespace.
    expect(call.headers.authorization).toBe("Bearer tok-abc");
  });

  it("treats a refused token as unauthorized rather than a generic failure", async () => {
    const http = new FakeFetch().on("POST /v1/session", {
      status: 401,
      body: { detail: "no" },
    });
    await expect(client(http).client.authenticate("bad")).rejects.toBeInstanceOf(
      UnauthorizedError,
    );
  });

  it("treats anything that is not write as read", () => {
    // Fail safe: a missing scope must not grant the ability to start processes.
    expect(normaliseScope("write")).toBe("write");
    expect(normaliseScope("read")).toBe("read");
    expect(normaliseScope(undefined)).toBe("read");
    expect(normaliseScope("admin")).toBe("read");
    expect(normaliseScope(null)).toBe("read");
  });
});

describe("spawning", () => {
  it("carries an idempotency key, so a retry cannot start a second agent", async () => {
    const http = new FakeFetch().on("POST /v1/agents", {
      status: 201,
      body: agent(),
    });
    await client(http).client.spawn({ prompt: "ship it" });

    const call = http.callsTo("POST /v1/agents")[0];
    expect(call.headers["idempotency-key"]).toBe("key-1");
    expect(call.body).toEqual({ prompt: "ship it" });
  });

  it("sends the resume cursor, which is what makes it a conversation", async () => {
    const http = new FakeFetch().on("POST /v1/agents", { body: agent() });
    await client(http).client.spawn({
      prompt: "and again",
      resume: { session: "sess-9" },
    });
    expect(http.callsTo("POST /v1/agents")[0].body).toMatchObject({
      resume: { session: "sess-9" },
    });
  });

  it("surfaces the daemon's refusal verbatim", async () => {
    const http = new FakeFetch().on("POST /v1/agents", {
      status: 403,
      body: { detail: "permission `bypass` exceeds this daemon's ceiling" },
    });
    await expect(
      client(http).client.spawn({ prompt: "x" }),
    ).rejects.toThrow(/exceeds this daemon's ceiling/);
  });
});

describe("backfilling events", () => {
  it("omits the cursor entirely on a first load", async () => {
    // `after_seq` is exclusive and `seq` starts at 0, so sending 0 here would
    // skip `started` — the event carrying session_id and model.
    const http = new FakeFetch().on("GET /v1/agents/a1/events", {
      body: { events: [], last_seq: null },
    });
    await client(http).client.events("a1");
    expect(http.calls[0].url).toBe("/v1/agents/a1/events");
    expect(http.calls[0].url).not.toContain("after_seq");
  });

  it("sends seq 0 when the client really has seen event 0", async () => {
    const http = new FakeFetch().on("GET /v1/agents/a1/events", {
      body: { events: [], last_seq: null },
    });
    await client(http).client.events("a1", 0);
    expect(http.calls[0].url).toContain("after_seq=0");
  });

  it("escapes an id rather than pasting it into the path", async () => {
    const http = new FakeFetch().on("GET /v1/agents/a%2F1/events", {
      body: { events: [], last_seq: null },
    });
    await client(http).client.events("a/1");
    expect(http.calls[0].url).toContain("a%2F1");
  });

  it("tolerates an older daemon that returns a bare array", async () => {
    const http = new FakeFetch().on("GET /v1/agents/a1/events", {
      body: [{ kind: "message", text: "hi", agent_id: "a1", at_ms: 1, seq: 7 }],
    });
    const page = await client(http).client.events("a1");
    expect(page.events).toHaveLength(1);
    expect(page.last_seq).toBe(7);
  });
});

describe("killing", () => {
  it("accepts a 204 with no body", async () => {
    const http = new FakeFetch().on("DELETE /v1/agents/a1", { status: 204 });
    await expect(client(http).client.kill("a1")).resolves.toBeUndefined();
  });
});

describe("the live stream", () => {
  it("subscribes to the per-agent stream, which replays then goes live", async () => {
    const { client: c, spy } = client(new FakeFetch());
    c.stream("a1", {});
    expect(spy.last.url).toBe("/v1/agents/a1/stream");
  });

  it("resumes from the last event actually rendered", () => {
    const { client: c, spy } = client(new FakeFetch());
    c.stream("a1", {}, 11);
    expect(spy.last.url).toContain("after_seq=11");
  });

  it("delivers flattened envelopes", () => {
    const { client: c, spy } = client(new FakeFetch());
    const seen: unknown[] = [];
    c.stream("a1", { onEnvelope: (e) => seen.push(e) });

    // `kind` sits beside the event's own fields — not nested under a payload.
    spy.last.send({ kind: "message", text: "hi", agent_id: "a1", at_ms: 1, seq: 0 });
    expect(seen).toEqual([
      { kind: "message", text: "hi", agent_id: "a1", at_ms: 1, seq: 0 },
    ]);
  });

  it("drops a malformed frame instead of killing the conversation", () => {
    const { client: c, spy } = client(new FakeFetch());
    const seen: unknown[] = [];
    c.stream("a1", { onEnvelope: (e) => seen.push(e) });

    spy.last.emit("agent", "{not json");
    spy.last.emit("agent", JSON.stringify({ no: "kind" }));
    spy.last.send({ kind: "message", text: "ok", agent_id: "a1", at_ms: 1, seq: 1 });
    expect(seen).toHaveLength(1);
  });

  it("reports how many events the daemon dropped", () => {
    const { client: c, spy } = client(new FakeFetch());
    const missed: number[] = [];
    c.stream("a1", { onLagged: (n) => missed.push(n) });

    spy.last.emit("lagged", '{"missed":12}');
    expect(missed).toEqual([12]);
  });

  it("ignores a lagged frame claiming nothing was missed", () => {
    const { client: c, spy } = client(new FakeFetch());
    const missed: number[] = [];
    c.stream("a1", { onLagged: (n) => missed.push(n) });
    spy.last.emit("lagged", '{"missed":0}');
    spy.last.emit("lagged", "garbage");
    expect(missed).toEqual([]);
  });

  it("calls a transient drop transient, because EventSource retries by itself", () => {
    const { client: c, spy } = client(new FakeFetch());
    const errors: { unauthorized: boolean }[] = [];
    c.stream("a1", { onError: (e) => errors.push(e) });

    spy.last.fail(false);
    expect(errors).toEqual([{ unauthorized: false }]);
  });

  it("calls a closed stream an expired session, because that is the only way it closes", () => {
    const { client: c, spy } = client(new FakeFetch());
    const errors: { unauthorized: boolean }[] = [];
    c.stream("a1", { onError: (e) => errors.push(e) });

    spy.last.fail(true);
    expect(errors).toEqual([{ unauthorized: true }]);
  });

  it("hands back a working close", () => {
    const { client: c, spy } = client(new FakeFetch());
    const stop = c.stream("a1", {});
    stop();
    expect(spy.last.closed).toBe(true);
  });
});

describe("reading an error the user can act on", () => {
  it("prefers the problem document's detail", async () => {
    const res = { status: 403, json: async () => ({ detail: "cwd not allowed" }) };
    expect(await problemDetail(res)).toBe("cwd not allowed");
  });

  it("falls back through the other conventional fields", async () => {
    expect(await problemDetail({ status: 400, json: async () => ({ title: "t" }) })).toBe("t");
    expect(await problemDetail({ status: 400, json: async () => ({ message: "m" }) })).toBe("m");
    expect(await problemDetail({ status: 400, json: async () => ({ error: "e" }) })).toBe("e");
  });

  it("never renders an empty string as the whole explanation", async () => {
    const res = { status: 500, statusText: "Server Error", json: async () => ({ detail: "  " }) };
    expect(await problemDetail(res)).toBe("500 Server Error");
  });

  it("still says something when the body is not JSON at all", async () => {
    const res = {
      status: 502,
      json: async () => {
        throw new SyntaxError("nope");
      },
    };
    expect(await problemDetail(res)).toBe("HTTP 502");
  });
});

describe("frame parsing", () => {
  it("requires both the kind and the sequence number", () => {
    expect(parseEnvelope(JSON.stringify({ kind: "message", seq: 0 }))).not.toBeNull();
    expect(parseEnvelope(JSON.stringify({ kind: "message" }))).toBeNull();
    expect(parseEnvelope(JSON.stringify({ seq: 0 }))).toBeNull();
    expect(parseEnvelope("null")).toBeNull();
    expect(parseEnvelope(42)).toBeNull();
    expect(parseEnvelope(undefined)).toBeNull();
  });

  it("accepts seq 0, which is a real event and not a missing one", () => {
    const envelope = parseEnvelope(JSON.stringify({ kind: "started", seq: 0 }));
    expect(envelope?.seq).toBe(0);
  });

  it("reads a missed count, and treats anything else as none", () => {
    expect(parseMissed('{"missed":3}')).toBe(3);
    expect(parseMissed('{"missed":"3"}')).toBe(0);
    expect(parseMissed("{}")).toBe(0);
    expect(parseMissed("garbage")).toBe(0);
    expect(parseMissed(7)).toBe(0);
  });
});
