// @vitest-environment jsdom
/**
 * The screen, driven through a real `Conversation`.
 *
 * The logic suites prove the rules; this proves the rules reach the glass —
 * that the components render every entry kind, that the controls are wired to
 * the store, and that the gate appears instead of a frozen shell when the
 * session is gone. Rendering is checked, not styled: a test that asserted
 * colours would break on every design change and catch nothing.
 */

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { App } from "../src/App";
import { JodClient } from "../src/client";
import { Conversation } from "../src/conversation";
import { EventSourceSpy, FakeFetch, agent, member, settle, teamTask } from "./fakes";

let http: FakeFetch;
let spy: EventSourceSpy;

function build(): Conversation {
  return new Conversation({
    client: new JodClient({
      fetch: http.fetch,
      eventSource: spy.factory,
      newKey: () => "key-1",
    }),
    scopeMemory: { read: () => "write", write: () => {} },
  });
}

/** Render with a live session and an empty roster. */
async function mounted(): Promise<Conversation> {
  http.on("GET /v1/harnesses", {
    body: [{ id: "claude_code", label: "Claude Code", available: true, path: "/bin/claude" }],
  });
  http.on("GET /v1/agents", { body: [] });

  const conversation = build();
  render(<App conversation={conversation} />);
  await waitFor(() => expect(conversation.getSnapshot().link.phase).toBe("live"));
  return conversation;
}

beforeEach(() => {
  http = new FakeFetch();
  spy = new EventSourceSpy();
});

afterEach(cleanup);

describe("the address gate (packaged app only)", () => {
  it("asks where the daemon is before asking for a token", async () => {
    // Served from tauri://localhost there is no same-origin API, so a token
    // would be a credential for nowhere.
    const conversation = new Conversation({
      client: new JodClient({ fetch: http.fetch, eventSource: spy.factory }),
      scopeMemory: { read: () => null, write: () => {} },
      originMemory: { read: () => null, write: () => {} },
      protocol: "tauri:",
    });
    render(<App conversation={conversation} />);

    await screen.findByText("where is the daemon?");
    expect(screen.getByPlaceholderText("jod-cloud:8787")).toBeDefined();
    expect(screen.queryByPlaceholderText("Bearer token")).toBeNull();
  });

  it("moves on to the token gate once an address is accepted", async () => {
    http.on("GET http://jod-cloud:8787/v1/harnesses", { status: 401, body: { detail: "no" } });
    let stored: string | null = null;
    const conversation = new Conversation({
      client: new JodClient({ fetch: http.fetch, eventSource: spy.factory }),
      scopeMemory: { read: () => null, write: () => {} },
      originMemory: { read: () => stored, write: (o) => (stored = o) },
      protocol: "tauri:",
    });
    render(<App conversation={conversation} />);

    await screen.findByPlaceholderText("jod-cloud:8787");
    fireEvent.change(screen.getByPlaceholderText("jod-cloud:8787"), {
      target: { value: "jod-cloud:8787" },
    });
    fireEvent.click(screen.getByText("CONNECT"));

    await screen.findByPlaceholderText("Bearer token");
    expect(stored).toBe("http://jod-cloud:8787");
  });
});

describe("the gate", () => {
  it("stands in front of the app when this device has no session", async () => {
    http.on("GET /v1/harnesses", { status: 401, body: { detail: "no" } });
    const conversation = build();
    render(<App conversation={conversation} />);

    await screen.findByText("this device needs a token");
    expect(screen.getByPlaceholderText("Bearer token")).toBeDefined();
    // The composer is not merely disabled — it is not on screen at all.
    expect(screen.queryByPlaceholderText("Delegate something")).toBeNull();
  });

  it("exchanges the typed token and lets the app through", async () => {
    http.on("GET /v1/harnesses", { status: 401, body: { detail: "no" } });
    const conversation = build();
    render(<App conversation={conversation} />);

    await screen.findByPlaceholderText("Bearer token");
    http.on("POST /v1/session", { status: 201, body: { scope: "write", expires_at_ms: 1 } });
    http.on("GET /v1/agents", { body: [] });

    fireEvent.change(screen.getByPlaceholderText("Bearer token"), {
      target: { value: "tok" },
    });
    fireEvent.click(screen.getByText("CONNECT"));

    await screen.findByPlaceholderText("Delegate something");
    expect(http.callsTo("POST /v1/session")[0].headers.authorization).toBe("Bearer tok");
  });

  it("never leaves the token on screen after a failed attempt", async () => {
    http.on("GET /v1/harnesses", { status: 401, body: { detail: "no" } });
    const conversation = build();
    render(<App conversation={conversation} />);

    await screen.findByPlaceholderText("Bearer token");
    http.on("POST /v1/session", { status: 401, body: { detail: "no" } });

    fireEvent.change(screen.getByPlaceholderText("Bearer token"), {
      target: { value: "wrong" },
    });
    fireEvent.click(screen.getByText("CONNECT"));

    await screen.findByText("that token was refused");
    const field = screen.getByPlaceholderText("Bearer token") as HTMLInputElement;
    expect(field.value).toBe("");
  });

  it("keeps the gate up while the token is being checked", async () => {
    // Flipping the link to `probing` mid-exchange would unmount the gate,
    // flash the main shell behind it, and mount a fresh one on failure —
    // wiping the field out from under whoever is looking at it.
    http.on("GET /v1/harnesses", { status: 401, body: { detail: "no" } });
    const conversation = build();
    render(<App conversation={conversation} />);

    await screen.findByPlaceholderText("Bearer token");
    http.on("POST /v1/session", { status: 401, body: { detail: "no" } });

    fireEvent.change(screen.getByPlaceholderText("Bearer token"), {
      target: { value: "wrong" },
    });
    let inFlight!: Promise<void>;
    act(() => {
      inFlight = conversation.connect("wrong");
    });
    // Still the gate, never the composer, at every point in between.
    expect(screen.queryByPlaceholderText("Delegate something")).toBeNull();
    await act(async () => {
      await inFlight;
    });
    expect(screen.queryByPlaceholderText("Delegate something")).toBeNull();
  });
});

describe("the shell", () => {
  it("shows the status line the TUI would show", async () => {
    await mounted();
    expect(screen.getByText("Claude Code · ready")).toBeDefined();
  });

  it("invites a first delegation when nothing has run", async () => {
    await mounted();
    expect(screen.getByText(/Jod delegates. It does not do the work./)).toBeDefined();
  });

  it("will not send an empty prompt", async () => {
    await mounted();
    const send = screen.getByText("SEND") as HTMLButtonElement;
    expect(send.disabled).toBe(true);

    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "   " },
    });
    expect(send.disabled).toBe(true);
  });

  it("delegates what was typed and echoes it", async () => {
    const conversation = await mounted();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "ship it" },
    });
    fireEvent.click(screen.getByText("SEND"));

    await screen.findByText("ship it");
    expect(http.callsTo("POST /v1/agents")[0].body).toMatchObject({ prompt: "ship it" });
    // Busy: the composer is closed for the duration of the turn.
    await waitFor(() =>
      expect((screen.getByText("SEND") as HTMLButtonElement).disabled).toBe(true),
    );
    expect(conversation.getSnapshot().session.busy).toBe(true);
  });

  it("renders each kind of thing an agent produces", async () => {
    const conversation = await mounted();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "ship it" },
    });
    fireEvent.click(screen.getByText("SEND"));
    await waitFor(() => expect(spy.created).toHaveLength(1));

    await act(async () => {
      spy.last.send({ kind: "message", text: "on it", agent_id: "a1", at_ms: 1, seq: 0 });
      spy.last.send({
        kind: "tool_call",
        name: "Bash",
        input: { command: "cargo test" },
        agent_id: "a1",
        at_ms: 2,
        seq: 1,
      });
      spy.last.send({
        kind: "tool_result",
        name: "Bash",
        summary: "362 passed",
        is_error: false,
        agent_id: "a1",
        at_ms: 3,
        seq: 2,
      });
      spy.last.send({
        kind: "tool_call",
        name: "Read",
        input: { file_path: "/tmp/gone" },
        agent_id: "a1",
        at_ms: 4,
        seq: 3,
      });
      spy.last.send({
        kind: "tool_result",
        name: "Read",
        summary: "no such file",
        is_error: true,
        agent_id: "a1",
        at_ms: 5,
        seq: 4,
      });
      spy.last.send({ kind: "raw", line: "odd line", agent_id: "a1", at_ms: 6, seq: 5 });
      spy.last.send({
        kind: "finished",
        is_error: false,
        usage: { output_tokens: 12, cost_usd: 0.02 },
        agent_id: "a1",
        at_ms: 7,
        seq: 6,
      });
      await settle();
    });

    await screen.findByText("on it");
    // A call carries its most useful argument, and what the tool gave back is
    // on screen underneath it — the whole point of watching a harness work
    // rather than waiting for its conclusion.
    expect(screen.getByText("Bash · cargo test")).toBeDefined();
    expect(screen.getByText("362 passed")).toBeDefined();
    // A failure is shown as the call line plus its output, both marked failed.
    expect(screen.getByText("Read · /tmp/gone")).toBeDefined();
    expect(screen.getByText("no such file").closest(".entry")?.className).toContain(
      "failed",
    );
    // `raw` is surfaced, never swallowed — it is the harness-upgrade seam.
    expect(screen.getByText("odd line")).toBeDefined();
    expect(screen.getByText("done · 12 out · $0.0200")).toBeDefined();
    // The turn is over, so the composer reopens and the spend is on the bar.
    await screen.findByText("Claude Code · $0.0200 · ready");
    expect(conversation.getSnapshot().session.busy).toBe(false);
  });

  it("shows reasoning until THINK is pressed to hide it", async () => {
    await mounted();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "ship it" },
    });
    fireEvent.click(screen.getByText("SEND"));
    await waitFor(() => expect(spy.created).toHaveLength(1));

    await act(async () => {
      spy.last.send({ kind: "thinking", text: "pondering", agent_id: "a1", at_ms: 1, seq: 0 });
      await settle();
    });
    await screen.findByText("pondering");

    fireEvent.click(screen.getByText("THINK"));
    await act(async () => {
      spy.last.send({
        kind: "thinking",
        text: "still pondering",
        agent_id: "a1",
        at_ms: 2,
        seq: 1,
      });
      await settle();
    });
    expect(screen.queryByText("still pondering")).toBeNull();
  });
});

describe("the agents sheet", () => {
  it("lists every delegation the daemon knows about, including older ones", async () => {
    http.on("GET /v1/harnesses", { body: [] });
    http.on("GET /v1/agents", {
      body: [agent({ id: "old", name: "started from the terminal", status: "completed" })],
    });
    const conversation = build();
    render(<App conversation={conversation} />);
    await waitFor(() =>
      expect(conversation.getSnapshot().session.agents).toHaveLength(1),
    );

    fireEvent.click(screen.getByText("AGENTS"));
    await screen.findByText("started from the terminal");
    expect(screen.getByText("COMPLETED")).toBeDefined();
  });

  it("offers STOP only for a run that is still going", async () => {
    http.on("GET /v1/harnesses", { body: [] });
    http.on("GET /v1/agents", {
      body: [agent({ id: "a1", status: "running" }), agent({ id: "a2", status: "completed" })],
    });
    const conversation = build();
    render(<App conversation={conversation} />);
    await waitFor(() =>
      expect(conversation.getSnapshot().session.agents).toHaveLength(2),
    );

    fireEvent.click(screen.getByText("AGENTS"));
    await screen.findAllByText("ship it");
    expect(screen.getAllByText("STOP")).toHaveLength(1);

    http.on("DELETE /v1/agents/a1", { status: 204 });
    http.on("GET /v1/agents", { body: [] });
    fireEvent.click(screen.getByText("STOP"));
    await waitFor(() => expect(http.calledOnce("DELETE /v1/agents/a1")).toBe(true));
  });

  it("closes again", async () => {
    await mounted();
    fireEvent.click(screen.getByText("AGENTS"));
    await screen.findByText("CLOSE");

    fireEvent.click(screen.getByText("CLOSE"));
    await waitFor(() => expect(screen.queryByText("CLOSE")).toBeNull());
  });
});

describe("reading back through a run", () => {
  it("offers a way back to the bottom once the reader scrolls up", async () => {
    const conversation = await mounted();
    expect(screen.queryByText("↓ LATEST")).toBeNull();

    // jsdom reports every element as zero-height, so the scroll handler cannot
    // be triggered by a real scroll. Setting the state directly is the honest
    // seam: the rule under test is "not following ⇒ offer the way back", and
    // the DOM measurement that decides `following` is asserted in the handler.
    act(() => conversation.setFollowing(false));
    await screen.findByText("↓ LATEST");

    fireEvent.click(screen.getByText("↓ LATEST"));
    await waitFor(() => expect(conversation.getSnapshot().session.following).toBe(true));
  });
});

// ─── slash commands, on the glass ───────────────────────────────────────────

describe("the completion list", () => {
  it("stays out of the way for a plain prompt", async () => {
    await mounted();
    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "ship it" },
    });
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("opens on a slash and narrows as you type", async () => {
    await mounted();
    const box = screen.getByPlaceholderText("Delegate something");

    fireEvent.change(box, { target: { value: "/" } });
    expect(screen.getAllByRole("option").length).toBeGreaterThan(5);

    fireEvent.change(box, { target: { value: "/th" } });
    const only = screen.getAllByRole("option");
    expect(only).toHaveLength(1);
    expect(only[0]!.textContent).toContain("/thinking");
  });

  /** The whole reason arguments are completed: three spellings, soft keyboard. */
  it("offers the harnesses by name", async () => {
    await mounted();
    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "/harness " },
    });
    const shown = screen.getAllByRole("option").map((o) => o.textContent);
    expect(shown.join(" ")).toContain("/harness claude");
    expect(shown.join(" ")).toContain("/harness agy");
    expect(shown.join(" ")).toContain("AGY");
  });

  /** Tap replaces `Tab`; the row goes straight into the composer. */
  it("puts the tapped line in the composer", async () => {
    const conversation = await mounted();
    const box = screen.getByPlaceholderText("Delegate something") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "/harn" } });
    fireEvent.mouseDown(screen.getAllByRole("option")[0]!);

    await waitFor(() =>
      expect(conversation.getSnapshot().session.input).toBe("/harness "),
    );
  });
});

describe("running a command from the composer", () => {
  it("keeps SEND live for a command while a turn is running", async () => {
    const conversation = await mounted();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    const box = screen.getByPlaceholderText("Delegate something");
    fireEvent.change(box, { target: { value: "ship it" } });
    fireEvent.click(screen.getByText("SEND"));
    await waitFor(() => expect(conversation.getSnapshot().session.busy).toBe(true));

    // A prompt is refused while busy; a command is not.
    fireEvent.change(box, { target: { value: "/model haiku" } });
    await waitFor(() =>
      expect((screen.getByText("SEND") as HTMLButtonElement).disabled).toBe(false),
    );
    fireEvent.click(screen.getByText("SEND"));
    await waitFor(() =>
      expect(conversation.getSnapshot().session.model).toBe("haiku"),
    );
  });

  it("shows the help list in the transcript", async () => {
    await mounted();
    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "/help" },
    });
    fireEvent.click(screen.getByText("SEND"));
    await screen.findByText(/\/harness <name>/);
  });
});

describe("the tool-output toggle", () => {
  it("hides what tools returned, and keeps the call", async () => {
    const conversation = await mounted();
    http.on("POST /v1/agents", { body: agent({ id: "a1" }) });
    http.on("GET /v1/agents", { body: [] });

    fireEvent.click(screen.getByText("TOOLS")); // details off
    fireEvent.change(screen.getByPlaceholderText("Delegate something"), {
      target: { value: "ship it" },
    });
    fireEvent.click(screen.getByText("SEND"));
    await waitFor(() => expect(spy.created).toHaveLength(1));

    await act(async () => {
      spy.last.send({
        kind: "tool_call",
        name: "Bash",
        input: { command: "cargo test" },
        agent_id: "a1",
        at_ms: 1,
        seq: 0,
      });
      spy.last.send({
        kind: "tool_result",
        name: "Bash",
        summary: "362 passed",
        is_error: false,
        agent_id: "a1",
        at_ms: 2,
        seq: 1,
      });
      await settle();
    });

    expect(screen.getByText("Bash · cargo test")).toBeDefined();
    expect(screen.queryByText("362 passed")).toBeNull();
    expect(conversation.getSnapshot().session.showDetails).toBe(false);
  });
});

// ─── the team sheet ─────────────────────────────────────────────────────────

/** Render with a live session watching a team the daemon knows about. */
async function watching(): Promise<Conversation> {
  http.on("GET /v1/harnesses", {
    body: [{ id: "claude_code", label: "Claude Code", available: true, path: "/bin/claude" }],
  });
  http.on("GET /v1/agents", { body: [] });
  http.on("GET /v1/teams/crew", {
    body: {
      team: "crew",
      members: [member(), member({ name: "lead", harness: "claude_code", status: "busy" })],
      tasks: [teamTask(), teamTask({ id: "t2", title: "ship it", owner: "lead", status: "done" })],
    },
  });
  const conversation = new Conversation({
    client: new JodClient({
      fetch: http.fetch,
      eventSource: spy.factory,
      newKey: () => "key-1",
    }),
    team: "crew",
    scopeMemory: { read: () => "write", write: () => {} },
  });
  render(<App conversation={conversation} />);
  await waitFor(() => expect(conversation.getSnapshot().link.phase).toBe("live"));
  return conversation;
}

describe("the team sheet", () => {
  it("lists a cross-harness team with what each member is doing", async () => {
    await watching();
    http.on("GET /v1/teams/crew", {
      body: {
        team: "crew",
        members: [member(), member({ name: "lead", harness: "claude_code", status: "busy" })],
        tasks: [],
      },
    });
    fireEvent.click(screen.getByText("TEAM"));

    await screen.findByText("scout");
    // The thing no single harness can do: one team, two harnesses.
    expect(screen.getByText("AGY")).toBeDefined();
    expect(screen.getByText("Claude Code")).toBeDefined();
    expect(screen.getByText("READY")).toBeDefined();
    expect(screen.getByText("BUSY")).toBeDefined();
  });

  it("shows the board with progress and who owns what", async () => {
    await watching();
    fireEvent.click(screen.getByText("TEAM"));

    await screen.findByText("read the docs");
    expect(screen.getByText("ship it")).toBeDefined();
    expect(screen.getByText("(lead)")).toBeDefined();
    expect(screen.getByText("BOARD · 1/2")).toBeDefined();
  });

  it("says there is no team rather than showing an empty board", async () => {
    await mounted();
    fireEvent.click(screen.getByText("TEAM"));
    await screen.findByText(/No team/);
  });

  it("closes again", async () => {
    await watching();
    fireEvent.click(screen.getByText("TEAM"));
    await screen.findByText("scout");
    fireEvent.click(screen.getByText("CLOSE"));
    await waitFor(() => expect(screen.queryByText("scout")).toBeNull());
  });
});

describe("resuming from the agents sheet", () => {
  it("threads the next turn through the conversation that row reported", async () => {
    http.on("GET /v1/harnesses", {
      body: [{ id: "claude_code", label: "Claude Code", available: true, path: "/bin/claude" }],
    });
    http.on("GET /v1/agents", {
      body: [agent({ id: "older", name: "yesterday", status: "completed", session_id: "ses-7" })],
    });
    const conversation = build();
    render(<App conversation={conversation} />);
    await waitFor(() => expect(conversation.getSnapshot().link.phase).toBe("live"));

    http.on("GET /v1/agents", {
      body: [agent({ id: "older", name: "yesterday", status: "completed", session_id: "ses-7" })],
    });
    fireEvent.click(screen.getByText("AGENTS"));
    await screen.findByText("yesterday");
    fireEvent.click(screen.getByText("RESUME"));

    await waitFor(() =>
      expect(conversation.getSnapshot().session.resume).toEqual({ session: "ses-7" }),
    );
  });
});
