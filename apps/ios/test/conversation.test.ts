/**
 * The app's actual behaviour, driven headless.
 *
 * These are the rules that decide whether the thing works on a phone at the far
 * end of a tailnet: not sending twice, not losing the thread, not double-drawing
 * a replayed event, not going quiet after the screen locks.
 */

import { beforeEach, describe, expect, it } from "vitest";

import { JodClient, type Scope } from "../src/client";
import { Conversation, type ScopeMemory } from "../src/conversation";
import type { Entry } from "../src/session";
import { EventSourceSpy, FakeFetch, agent, settle } from "./fakes";

let http: FakeFetch;
let spy: EventSourceSpy;
let memory: ScopeMemory & { value: Scope | null };

/** An in-memory stand-in for `localStorage`, which Node does not have. */
function fakeMemory(initial: Scope | null = null) {
  return {
    value: initial,
    read() {
      return this.value;
    },
    write(scope: Scope) {
      this.value = scope;
    },
  };
}

function build(): Conversation {
  return new Conversation({
    client: new JodClient({
      fetch: http.fetch,
      eventSource: spy.factory,
      newKey: () => "key-1",
    }),
    scopeMemory: memory,
  });
}

/** A conversation already holding a write session. */
async function connected(scope: "read" | "write" = "write"): Promise<Conversation> {
  http.on("POST /v1/session", { status: 201, body: { scope, expires_at_ms: 1 } });
  http.on("GET /v1/agents", { body: [] });
  const conversation = build();
  await conversation.connect("tok");
  await settle();
  return conversation;
}

function transcript(conversation: Conversation): Entry[] {
  return conversation.getSnapshot().session.transcript;
}

function notices(conversation: Conversation): string[] {
  return transcript(conversation)
    .filter((e) => e.kind === "notice")
    .map((e) => (e.kind === "notice" ? e.text : ""));
}

beforeEach(() => {
  http = new FakeFetch();
  spy = new EventSourceSpy();
  memory = fakeMemory();
});

describe("finding out where we stand", () => {
  it("asks for the token when the device has no session", async () => {
    http.on("GET /v1/harnesses", { status: 401, body: { detail: "no" } });
    const conversation = build();
    await conversation.probe();

    expect(conversation.getSnapshot().link).toEqual({
      phase: "auth",
      reason: "this device needs a token",
    });
  });

  it("goes live and loads the roster when the cookie is still good", async () => {
    http.on("GET /v1/harnesses", { body: [{ id: "claude_code", label: "Claude Code", available: true, path: "/bin/claude" }] });
    http.on("GET /v1/agents", { body: [agent({ name: "earlier run" })] });

    const conversation = build();
    await conversation.probe();
    await settle();

    expect(conversation.getSnapshot().link.phase).toBe("live");
    // Runs started from the terminal, before this phone ever connected, are
    // exactly what the agents sheet is for.
    expect(conversation.getSnapshot().session.agents[0].name).toBe("earlier run");
  });

  it("says so when the daemon is unreachable, rather than asking for a token", async () => {
    http.on("GET /v1/harnesses", { status: 502, body: { detail: "bad gateway" } });
    const conversation = build();
    await conversation.probe();

    expect(conversation.getSnapshot().link).toEqual({
      phase: "offline",
      reason: "bad gateway",
    });
  });

  it("warns when the box has no harness installed at all", async () => {
    http.on("GET /v1/harnesses", {
      body: [{ id: "claude_code", label: "Claude Code", available: false, path: null }],
    });
    http.on("GET /v1/agents", { body: [] });

    const conversation = build();
    await conversation.probe();
    await settle();

    expect(notices(conversation)).toContain(
      "no harness is installed on the daemon — nothing can start",
    );
  });
});

describe("connecting", () => {
  it("takes a write token and enables sending", async () => {
    const conversation = await connected("write");
    expect(conversation.getSnapshot().canSend).toBe(true);
    expect(conversation.getSnapshot().link).toEqual({ phase: "live", scope: "write" });
  });

  it("says out loud that a read token cannot delegate", async () => {
    const conversation = await connected("read");
    expect(conversation.getSnapshot().canSend).toBe(false);
    expect(notices(conversation)).toContain(
      "this token is read-only — you can watch, but not delegate",
    );
  });

  it("refuses an empty token without calling the daemon", async () => {
    const conversation = build();
    await conversation.connect("   ");
    expect(conversation.getSnapshot().link).toEqual({
      phase: "auth",
      reason: "a token is required",
    });
    expect(http.calls).toHaveLength(0);
  });

  it("remembers the scope, so a relaunch is not read-only until you re-paste", async () => {
    // The session cookie is HttpOnly and survives a restart, but no script can
    // read it — so without remembering the scope the composer would be dead on
    // every launch despite the session being perfectly valid.
    await connected("write");
    expect(memory.value).toBe("write");

    http.on("GET /v1/harnesses", { body: [] });
    http.on("GET /v1/agents", { body: [] });
    const relaunched = build();
    await relaunched.probe();
    await settle();

    expect(relaunched.getSnapshot().canSend).toBe(true);
  });

  it("remembers a read scope as read", async () => {
    await connected("read");
    expect(memory.value).toBe("read");
  });

  it("starts read-only when nothing was remembered", async () => {
    http.on("GET /v1/harnesses", { body: [] });
    http.on("GET /v1/agents", { body: [] });
    const conversation = build();
    await conversation.probe();
    await settle();

    expect(conversation.getSnapshot().canSend).toBe(false);
  });

  it("stays on the gate when the token is refused", async () => {
    http.on("POST /v1/session", { status: 401, body: { detail: "no" } });
    const conversation = build();
    await conversation.connect("bad");
    expect(conversation.getSnapshot().link).toEqual({
      phase: "auth",
      reason: "that token was refused",
    });
  });
});

describe("sending a turn", () => {
  it("delegates, echoes the prompt, and goes busy", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { status: 201, body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [agent({ id: "a1" })] });

    conversation.setInput("ship it");
    await conversation.send();
    await settle();

    expect(http.callsTo("POST /v1/agents")[0].body).toMatchObject({
      prompt: "ship it",
      name: "ship it",
      harness: "claude_code",
      resume: "fresh",
    });
    expect(transcript(conversation)).toContainEqual({ kind: "you", text: "ship it" });
    expect(conversation.getSnapshot().session.busy).toBe(true);
    expect(conversation.getSnapshot().session.input).toBe("");
  });

  it("follows the new agent's stream", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("ship it");
    await conversation.send();

    expect(spy.last.url).toBe("/v1/agents/a1/stream");
  });

  it("refuses a second prompt while one is in flight", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("first");
    await conversation.send();
    await settle();

    conversation.setInput("second");
    await conversation.send();

    expect(notices(conversation)).toContain("still working — wait for this turn to finish");
    expect(http.callsTo("POST /v1/agents")).toHaveLength(1);
  });

  it("sends nothing when nothing was typed", async () => {
    const conversation = await connected();
    conversation.setInput("   ");
    await conversation.send();
    expect(http.callsTo("POST /v1/agents")).toHaveLength(0);
  });

  it("keeps the typed text when a read-only session refuses it", async () => {
    // Losing what you typed because the token was the wrong scope would be a
    // second insult; the text is still there to copy or re-send after re-auth.
    const conversation = await connected("read");
    conversation.setInput("ship it");
    await conversation.send();

    expect(notices(conversation)).toContain("this session is read-only");
    expect(conversation.getSnapshot().session.input).toBe("ship it");
    expect(http.callsTo("POST /v1/agents")).toHaveLength(0);
  });

  it("cannot be made to start two agents by a double tap", async () => {
    // The composer is disabled by `busy`, and `busy` has to be true across the
    // spawn round trip — not only once the daemon has answered. Over a tailnet
    // that round trip is long enough to tap twice, and two agents in the same
    // working directory is the collision one-owner-per-path exists to prevent.
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("ship it");
    const first = conversation.send();
    // No await: this is the second tap, landing while the first is in flight.
    const second = conversation.send();
    await Promise.all([first, second]);
    await settle();

    expect(http.callsTo("POST /v1/agents")).toHaveLength(1);
    expect(notices(conversation)).toContain("still working — wait for this turn to finish");
  });

  it("refuses a prompt typed while the first spawn is still in flight", async () => {
    // The realistic version of the race: send, then immediately type the next
    // thought and send again before the daemon has answered. The composer is
    // non-empty by then, so an emptied input is not what protects this —
    // `busy` being true across the round trip is.
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("first");
    const inFlight = conversation.send();

    conversation.setInput("second");
    await conversation.send();
    await inFlight;
    await settle();

    expect(http.callsTo("POST /v1/agents")).toHaveLength(1);
    expect(http.callsTo("POST /v1/agents")[0].body).toMatchObject({ prompt: "first" });
    // And the second thought is still in the box, not silently discarded.
    expect(conversation.getSnapshot().session.input).toBe("second");
  });

  it("shows the prompt immediately, not after the daemon answers", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("ship it");
    const inFlight = conversation.send();
    expect(transcript(conversation)).toContainEqual({ kind: "you", text: "ship it" });
    expect(conversation.getSnapshot().session.busy).toBe(true);

    await inFlight;
    await settle();
  });

  it("releases the composer when the spawn was refused", async () => {
    // A refused spawn that left `busy` set would wedge the app until restart.
    const conversation = await connected();
    http.on("POST /v1/agents", { status: 400, body: { detail: "no" } });

    conversation.setInput("ship it");
    await conversation.send();

    expect(conversation.getSnapshot().session.busy).toBe(false);
    expect(conversation.getSnapshot().canSend).toBe(true);
  });

  it("reports a refusal from the daemon in the transcript", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", {
      status: 400,
      body: { detail: "cwd is outside the allowlist" },
    });

    conversation.setInput("ship it");
    await conversation.send();

    expect(notices(conversation)).toContain(
      "could not start: cwd is outside the allowlist",
    );
    expect(conversation.getSnapshot().session.busy).toBe(false);
  });

  it("sends the reader back to the gate when the session expired mid-send", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { status: 401, body: { detail: "no" } });

    conversation.setInput("ship it");
    await conversation.send();

    expect(conversation.getSnapshot().link).toEqual({
      phase: "auth",
      reason: "the session expired",
    });
  });
});

describe("threading the conversation", () => {
  it("continues the exact session the harness reported, not the most recent", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("first");
    await conversation.send();
    await settle();

    spy.last.send({
      kind: "started",
      session_id: "sess-9",
      model: "claude-opus-5",
      agent_id: "a1",
      at_ms: 1,
      seq: 0,
    });
    spy.last.send({
      kind: "finished",
      is_error: false,
      usage: { cost_usd: 0.01 },
      agent_id: "a1",
      at_ms: 2,
      seq: 1,
    });
    await settle();

    http.on("POST /v1/agents", { body: agent({ id: "a2" }) });
    http.on("GET /v1/agents", { body: [] });
    conversation.setInput("and again");
    await conversation.send();

    expect(http.callsTo("POST /v1/agents")[1].body).toMatchObject({
      resume: { session: "sess-9" },
    });
    // The model the harness reported is carried forward too.
    expect(http.callsTo("POST /v1/agents")[1].body).toMatchObject({
      model: "claude-opus-5",
    });
  });

  it("ends the turn and refreshes the roster when the run finishes", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    conversation.setInput("ship it");
    await conversation.send();
    await settle();

    http.on("GET /v1/agents", { body: [agent({ id: "a1", status: "completed" })] });
    spy.last.send({
      kind: "finished",
      is_error: false,
      usage: { output_tokens: 5, cost_usd: 0.02 },
      agent_id: "a1",
      at_ms: 2,
      seq: 0,
    });
    await settle();

    const view = conversation.getSnapshot();
    expect(view.session.busy).toBe(false);
    expect(view.session.costUsd).toBeCloseTo(0.02);
    expect(view.session.agents[0].status).toBe("completed");
  });
});

describe("the live stream", () => {
  async function running(): Promise<Conversation> {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });
    conversation.setInput("ship it");
    await conversation.send();
    await settle();
    return conversation;
  }

  it("draws a replayed event exactly once", async () => {
    // The per-agent stream is resumable, so the server may legally re-send
    // something already drawn. Deduping on seq is the contract, not a patch.
    const conversation = await running();
    const message = {
      kind: "message" as const,
      text: "hello",
      agent_id: "a1",
      at_ms: 1,
      seq: 0,
    };
    spy.last.send(message);
    spy.last.send(message);
    await settle();

    expect(transcript(conversation).filter((e) => e.kind === "agent")).toHaveLength(1);
  });

  it("ignores an event belonging to some other delegation", async () => {
    // Another agent's output belongs in the sheet, never in this transcript.
    const conversation = await running();
    spy.last.send({
      kind: "message",
      text: "not yours",
      agent_id: "someone-else",
      at_ms: 1,
      seq: 0,
    });
    await settle();

    expect(transcript(conversation).filter((e) => e.kind === "agent")).toHaveLength(0);
  });

  it("re-reads what the daemon dropped, and says that it did", async () => {
    const conversation = await running();
    spy.last.send({ kind: "message", text: "one", agent_id: "a1", at_ms: 1, seq: 0 });
    await settle();

    http.on("GET /v1/agents/a1/events", {
      body: {
        events: [
          { kind: "message", text: "two", agent_id: "a1", at_ms: 2, seq: 1 },
          { kind: "message", text: "three", agent_id: "a1", at_ms: 3, seq: 2 },
        ],
        last_seq: 2,
      },
    });
    spy.last.emit("lagged", '{"missed":2}');
    await settle(6);

    expect(notices(conversation)).toContain("the daemon dropped 2 events — re-reading");
    // Recovery reads from the last event actually rendered, exclusive.
    expect(http.callsTo("GET /v1/agents/a1/events")[0].url).toContain("after_seq=0");
    expect(transcript(conversation).filter((e) => e.kind === "agent")).toHaveLength(3);
  });

  it("shows the gate when the stream closes on an expired session", async () => {
    const conversation = await running();
    spy.last.fail(true);

    expect(conversation.getSnapshot().link).toEqual({
      phase: "auth",
      reason: "the session expired",
    });
    expect(spy.last.closed).toBe(true);
  });

  it("rides out a transient drop, because EventSource reconnects itself", async () => {
    const conversation = await running();
    spy.last.fail(false);

    expect(conversation.getSnapshot().link.phase).toBe("live");
    expect(spy.last.closed).toBe(false);
  });

  it("never runs two streams at once", async () => {
    // Two live streams would double-apply every event.
    const conversation = await running();
    const first = spy.last;
    conversation.follow("a2");

    expect(first.closed).toBe(true);
    expect(spy.created).toHaveLength(2);
  });
});

describe("coming back from the background", () => {
  it("catches up over REST, then re-opens the stream from there", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });
    conversation.setInput("ship it");
    await conversation.send();
    await settle();

    spy.last.send({ kind: "message", text: "one", agent_id: "a1", at_ms: 1, seq: 0 });
    await settle();

    // The phone slept here: the socket died and a run finished unseen.
    http.on("GET /v1/agents/a1/events", {
      body: {
        events: [
          {
            kind: "finished",
            is_error: false,
            usage: { cost_usd: 0.03 },
            agent_id: "a1",
            at_ms: 9,
            seq: 1,
          },
        ],
        last_seq: 1,
      },
    });
    http.on("GET /v1/agents", { body: [agent({ id: "a1", status: "completed" })] });

    await conversation.resumeAfterBackground();
    await settle();

    expect(conversation.getSnapshot().session.busy).toBe(false);
    expect(conversation.getSnapshot().session.costUsd).toBeCloseTo(0.03);
    // Re-followed from the last event rendered, not from the beginning.
    expect(spy.last.url).toContain("after_seq=1");
  });

  it("just refreshes the roster when no conversation is in flight", async () => {
    const conversation = await connected();
    http.on("GET /v1/agents", { body: [agent({ id: "z" })] });

    await conversation.resumeAfterBackground();
    await settle();

    expect(conversation.getSnapshot().session.agents[0].id).toBe("z");
    expect(spy.created).toHaveLength(0);
  });

  it("shows the gate when the session died while the phone slept", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });
    conversation.setInput("ship it");
    await conversation.send();
    await settle();

    http.on("GET /v1/agents/a1/events", { status: 401, body: { detail: "no" } });
    await conversation.resumeAfterBackground();

    expect(conversation.getSnapshot().link.phase).toBe("auth");
  });
});

describe("stopping an agent", () => {
  it("kills it and refreshes the roster", async () => {
    const conversation = await connected();
    http.on("DELETE /v1/agents/a1", { status: 204 });
    http.on("GET /v1/agents", { body: [agent({ id: "a1", status: "killed" })] });

    await conversation.kill("a1");
    await settle();

    expect(http.calledOnce("DELETE /v1/agents/a1")).toBe(true);
    expect(conversation.getSnapshot().session.agents[0].status).toBe("killed");
  });

  it("releases the composer when the killed agent was the one being watched", async () => {
    const conversation = await connected();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });
    conversation.setInput("ship it");
    await conversation.send();
    await settle();

    http.on("DELETE /v1/agents/a1", { status: 204 });
    http.on("GET /v1/agents", { body: [] });
    await conversation.kill("a1");
    await settle();

    expect(conversation.getSnapshot().session.busy).toBe(false);
  });

  it("says why when the kill was refused", async () => {
    const conversation = await connected();
    http.on("DELETE /v1/agents/a1", { status: 403, body: { detail: "read-only" } });

    await conversation.kill("a1");
    expect(notices(conversation)).toContain("could not stop: read-only");
  });
});

describe("the store contract", () => {
  it("hands out a stable snapshot until something changes", async () => {
    // `useSyncExternalStore` compares by identity: a new object every call is
    // an infinite render loop.
    const conversation = await connected();
    expect(conversation.getSnapshot()).toBe(conversation.getSnapshot());

    conversation.setInput("x");
    expect(conversation.getSnapshot().session.input).toBe("x");
  });

  it("notifies subscribers, and stops when unsubscribed", async () => {
    const conversation = await connected();
    let count = 0;
    const off = conversation.subscribe(() => count++);

    conversation.setInput("a");
    expect(count).toBe(1);

    off();
    conversation.setInput("b");
    expect(count).toBe(1);
  });

  it("does not churn when following is set to what it already is", async () => {
    const conversation = await connected();
    let count = 0;
    conversation.subscribe(() => count++);

    conversation.setFollowing(true);
    expect(count).toBe(0);

    conversation.setFollowing(false);
    expect(count).toBe(1);
  });

  it("keeps a roster failure out of the transcript", async () => {
    // The transcript is the app; a roster that failed to load is not worth
    // interrupting someone over.
    const conversation = await connected();
    http.on("GET /v1/agents", { status: 500, body: { detail: "boom" } });

    await conversation.refreshRoster();
    expect(notices(conversation)).not.toContain("boom");
  });
});
