/**
 * The workspace and conversation routes, against `api/src/workspaces.rs` and
 * `api/src/conversations.rs`.
 *
 * The assertions worth having are the contract's sharp edges — `limit=0` meaning
 * something, a `null` conversation being a state rather than a 404, the
 * idempotency key on the one verb that starts a process, and a `403` that must
 * reach the screen with the daemon's own words on it. Those are where a client
 * goes subtly wrong while still looking like it works.
 *
 * The JSON bodies here are the payloads the `api/` lane captured from a live
 * daemon against a copy of Reljod's own database, not invented fixtures.
 */

import { describe, expect, it } from "vitest";

import { JodClient, UnauthorizedError, query } from "../src/client";
import { FakeFetch } from "./fakes";

function client(http: FakeFetch) {
  return new JodClient({ fetch: http.fetch, newKey: () => "key-1" });
}

/** The `?` of a call, or `""` when there was none. */
function queryOf(http: FakeFetch, at = 0): string {
  const url = http.calls[at].url;
  const mark = url.indexOf("?");
  return mark === -1 ? "" : url.slice(mark);
}

describe("the query builder", () => {
  it("omits what was not given", () => {
    expect(query({ limit: undefined, scope: null })).toBe("");
  });

  /**
   * The daemon honours `limit=0` as "no rows, but still tell me the counts", so
   * dropping it as falsy would silently turn a cheap counts-only call into a
   * full page.
   */
  it("keeps a zero and a false, which mean something here", () => {
    expect(query({ limit: 0 })).toBe("?limit=0");
    expect(query({ needs_you: false })).toBe("?needs_you=false");
  });

  it("escapes what it is given", () => {
    expect(query({ team: "a b&c" })).toBe("?team=a+b%26c");
  });
});

describe("memory", () => {
  it("reads a page whose counts describe the whole graph, not the page", async () => {
    const http = new FakeFetch().on("GET /v1/memory", {
      body: {
        nodes: [
          {
            id: 1,
            scope: "default",
            name: "reljod",
            kind: "thing",
            last_seen_ms: 1786464180543,
            degree: 1,
          },
        ],
        node_count: 2,
        edge_count: 1,
      },
    });
    const page = await client(http).memory({ limit: 1 });

    expect(page.nodes).toHaveLength(1);
    expect(page.node_count, "the graph, not the page").toBe(2);
    expect(page.edge_count).toBe(1);
  });

  /** A counts-only call, which `limit=0` is the cheap way to make. */
  it("passes limit=0 through rather than treating it as absent", async () => {
    const http = new FakeFetch().on("GET /v1/memory", {
      body: { nodes: [], node_count: 2, edge_count: 1 },
    });
    const page = await client(http).memory({ limit: 0 });

    expect(queryOf(http)).toBe("?limit=0");
    expect(page.nodes).toEqual([]);
    expect(page.node_count).toBe(2);
  });

  it("reads one node with its edges in both directions", async () => {
    const http = new FakeFetch().on("GET /v1/memory/1", {
      body: {
        id: 1,
        scope: "default",
        name: "reljod",
        kind: "thing",
        last_seen_ms: 1786464180543,
        degree: 1,
        in_edges: [],
        out_edges: [
          {
            predicate: "prefers",
            other_id: 2,
            other: "linear for tasks, never a shadow copy",
            outgoing: true,
          },
        ],
      },
    });
    const node = await client(http).memoryNode(1);

    // Flattened: the node's own fields sit beside its edges.
    expect(node.name).toBe("reljod");
    expect(node.out_edges[0].predicate).toBe("prefers");
    expect(node.in_edges).toEqual([]);
  });

  it("asks for a neighbourhood at a given depth", async () => {
    const http = new FakeFetch().on("GET /v1/memory/1/graph", {
      body: {
        root_id: 1,
        nodes: [
          { id: 1, name: "reljod", kind: "thing", hops: 0 },
          { id: 2, name: "linear", kind: "thing", hops: 1 },
        ],
        edges: [{ from: 1, to: 2, predicate: "prefers" }],
      },
    });
    const graph = await client(http).memoryGraph(1, { depth: 2 });

    expect(queryOf(http)).toBe("?depth=2");
    expect(graph.root_id).toBe(1);
    // The root is in its own graph, at zero hops.
    expect(graph.nodes.find((n) => n.id === 1)?.hops).toBe(0);
  });
});

describe("schedules, goals and hooks", () => {
  it("reads the raw cron rather than a rendered sentence", async () => {
    const http = new FakeFetch().on("GET /v1/schedules", {
      body: [{ name: "nightly", cron: "0 2 * * *", timezone: "Asia/Manila" }],
    });
    const rows = await client(http).schedules();

    // The gloss is this app's job; the wire carries what it is computed from.
    expect(rows[0].cron).toBe("0 2 * * *");
    expect(rows[0].timezone).toBe("Asia/Manila");
  });

  it("reads one schedule flattened with its fires", async () => {
    const http = new FakeFetch().on("GET /v1/schedules/nightly", {
      body: {
        name: "nightly",
        cron: "0 2 * * *",
        fires: [{ id: 1, outcome: "spawn_failed", due_at_ms: 1, fired_at_ms: 2 }],
      },
    });
    const view = await client(http).schedule("nightly");

    expect(view.name).toBe("nightly");
    expect(view.fires[0].outcome).toBe("spawn_failed");
  });

  it("escapes a name in the path", async () => {
    const http = new FakeFetch().on("GET /v1/schedules/a%20b", { body: {} });
    await client(http).schedule("a b");
    expect(http.calls[0].url).toContain("/v1/schedules/a%20b");
  });

  it("reads each hook rule with its deliveries", async () => {
    const http = new FakeFetch().on("GET /v1/hooks", {
      body: [
        {
          id: "r1",
          name: "prs",
          enabled: true,
          conditions: { labels: [], branch: null, author: null, draft: null },
          deliveries: [{ id: 9, status: "no_match", delivery_id: "d1" }],
        },
      ],
    });
    const rows = await client(http).hooks(5);

    expect(queryOf(http)).toBe("?limit=5");
    expect(rows[0].deliveries[0].status).toBe("no_match");
  });
});

describe("tasks and activity", () => {
  /**
   * With no team named the daemon picks the first team that has a member, so a
   * board whose team nobody joined is unreachable without naming it. The client
   * must not invent a name to paper over that.
   */
  it("names no team when none was asked for", async () => {
    const http = new FakeFetch().on("GET /v1/tasks", { body: [] });
    await client(http).tasks();
    expect(queryOf(http)).toBe("");
  });

  it("names the team when one was asked for", async () => {
    const http = new FakeFetch().on("GET /v1/tasks", { body: [] });
    await client(http).tasks("crew");
    expect(queryOf(http)).toBe("?team=crew");
  });

  it("carries jump_to as the two-element tuple it is", async () => {
    const http = new FakeFetch().on("GET /v1/activity", {
      body: [
        {
          id: "f-1",
          at_ms: 5,
          source: "cron",
          text: "nightly could not start",
          needs_you: true,
          jump_to: ["schedules", "nightly"],
        },
      ],
    });
    const rows = await client(http).activity({ needsYou: true });

    expect(queryOf(http)).toBe("?needs_you=true");
    // A row that names a schedule has to be able to reach it.
    expect(rows[0].jump_to).toEqual(["schedules", "nightly"]);
    expect(rows[0].needs_you).toBe(true);
  });

  it("asks for everything when needs_you was not specified", async () => {
    const http = new FakeFetch().on("GET /v1/activity", { body: [] });
    await client(http).activity();
    expect(queryOf(http)).toBe("");
  });
});

describe("the pinned main chat", () => {
  /**
   * `null` before anyone has spoken is a state to render, not a failure. The
   * pinned fleet row draws from first launch.
   */
  it("treats an unspoken conversation as a state, not an error", async () => {
    const http = new FakeFetch().on("GET /v1/conversations/main", {
      body: { conversation: null, messages: [] },
    });
    const main = await client(http).mainChat();

    expect(main.conversation).toBeNull();
    expect(main.messages).toEqual([]);
  });

  it("reads the thread with its roles and tool rows intact", async () => {
    const http = new FakeFetch().on("GET /v1/conversations/main", {
      body: {
        conversation: { id: "8ce8211e", title: "main", head_id: 251 },
        messages: [
          { id: 229, role: "user", text: "hello, what are you?", parent_id: null },
          { id: 230, role: "thinking", text: "considering", parent_id: 229 },
          {
            id: 231,
            role: "tool_call",
            text: "Bash",
            tool_name: "Bash",
            tool_input: { command: "ls" },
            parent_id: 230,
          },
        ],
      },
    });
    const main = await client(http).mainChat();

    // Six roles, not two: a screen assuming user/assistant draws a
    // conversation that never happened.
    expect(main.messages.map((m) => m.role)).toEqual([
      "user",
      "thinking",
      "tool_call",
    ]);
    expect(main.messages[2].tool_name).toBe("Bash");
    // The tree is real even though this app renders it flat along `head_id`.
    expect(main.messages[1].parent_id).toBe(229);
    expect(main.conversation?.head_id).toBe(251);
  });

  /**
   * The same reason `spawn` carries one: a tap on a flaky link, or iOS resuming
   * a suspended request, must not start a second orchestrator run.
   */
  it("carries an idempotency key when handing over an instruction", async () => {
    const http = new FakeFetch().on("POST /v1/conversations/main/messages", {
      status: 201,
      body: { agent: {}, conversation_id: "8ce8211e", compaction_due: false },
    });
    const handed = await client(http).sendToMain({ instruction: "ship it" });

    const call = http.calls[0];
    expect(call.headers["idempotency-key"]).toBe("key-1");
    expect(call.body).toEqual({ instruction: "ship it" });
    expect(handed.conversation_id).toBe("8ce8211e");
  });

  it("sends harness and cwd only when given", async () => {
    const http = new FakeFetch()
      .on("POST /v1/conversations/main/messages", {
        status: 201,
        body: { agent: {}, conversation_id: "c", compaction_due: false },
      })
      .on("POST /v1/conversations/main/messages", {
        status: 201,
        body: { agent: {}, conversation_id: "c", compaction_due: false },
      });
    const c = client(http);

    await c.sendToMain({ instruction: "a" });
    expect(http.calls[0].body).toEqual({ instruction: "a" });

    await c.sendToMain({ instruction: "b", harness: "agy", cwd: "/tmp" });
    expect(http.calls[1].body).toEqual({
      instruction: "b",
      harness: "agy",
      cwd: "/tmp",
    });
  });

  /**
   * The main chat runs at `accept_edits` by construction, so a daemon capped at
   * `ask` refuses. The refusal names the mode and the setting, and that text is
   * the only thing telling the operator which knob to turn — so it must survive
   * as far as the screen.
   */
  it("surfaces a permission refusal with the daemon's own words", async () => {
    const refusal = {
      status: 403,
      body: {
        detail:
          "the main chat needs accept_edits; this daemon's max_permission is ask",
      },
    };
    // Queued twice: one reply per call, and this asserts two things about the
    // same refusal.
    const http = new FakeFetch()
      .on("POST /v1/conversations/main/messages", refusal)
      .on("POST /v1/conversations/main/messages", refusal);
    const c = client(http);

    await expect(c.sendToMain({ instruction: "ship it" })).rejects.toThrow(
      /accept_edits.*max_permission is ask/,
    );
    // Not a generic failure: the app distinguishes an authorisation refusal so
    // it can correct scope in place rather than bouncing to the token gate.
    await expect(
      c.sendToMain({ instruction: "ship it" }),
    ).rejects.toBeInstanceOf(UnauthorizedError);
  });

  it("lists conversations, and one thread oldest first", async () => {
    const http = new FakeFetch()
      .on("GET /v1/conversations", {
        body: [{ id: "041f731b", title: "Hello!", message_count: 3 }],
      })
      .on("GET /v1/conversations/041f731b/messages", {
        body: [
          { id: 1, role: "user", text: "first" },
          { id: 2, role: "assistant", text: "second" },
        ],
      });
    const c = client(http);

    const list = await c.conversations(3);
    expect(queryOf(http)).toBe("?limit=3");
    expect(list[0].message_count).toBe(3);

    const thread = await c.conversationMessages("041f731b");
    expect(thread.map((m) => m.id)).toEqual([1, 2]);
  });
});
