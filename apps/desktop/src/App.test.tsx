import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { agent, envelope, harness, systemStatus } from "./test/factories";
import type { AgentEnvelope } from "./types";

const api = {
  systemStatus: vi.fn(),
  spawnAgent: vi.fn(),
  listAgents: vi.fn(),
  agentEvents: vi.fn(),
  killAgent: vi.fn(),
  report: vi.fn(),
  openInTerminal: vi.fn(),
};

/** Captures the handler App registers, so a test can push events at it. */
let emit: (envelope: AgentEnvelope) => void = () => {};
const unlisten = vi.fn();
const onAgentEvent = vi.fn((handler: (e: AgentEnvelope) => void) => {
  emit = handler;
  return Promise.resolve(unlisten);
});

vi.mock("./api", () => ({ api, onAgentEvent: (h: never) => onAgentEvent(h) }));

const App = (await import("./App")).default;

beforeEach(() => {
  vi.clearAllMocks();
  api.systemStatus.mockResolvedValue(systemStatus());
  api.listAgents.mockResolvedValue([]);
  api.agentEvents.mockResolvedValue([]);
  api.spawnAgent.mockResolvedValue(agent());
  api.killAgent.mockResolvedValue(undefined);
  api.openInTerminal.mockResolvedValue(undefined);
});

const task = () => screen.getByRole("textbox", { name: /Task/ });

describe("starting up", () => {
  it("asks the backend what this machine can do, and what is already running", async () => {
    render(<App />);
    await waitFor(() => expect(api.systemStatus).toHaveBeenCalled());
    expect(api.listAgents).toHaveBeenCalled();
  });

  it("says it is still checking until the status arrives", async () => {
    let settle: (s: unknown) => void = () => {};
    api.systemStatus.mockReturnValue(new Promise((resolve) => (settle = resolve)));

    render(<App />);
    expect(screen.getByText(/Checking this machine/)).toBeInTheDocument();

    settle(systemStatus());
    await waitFor(() => expect(screen.getByText(/Delegate a task/)).toBeInTheDocument());
  });

  it("explains itself while no agent is selected", async () => {
    render(<App />);
    expect(await screen.findByText(/Jod delegates\. It does not do the work\./)).toBeInTheDocument();
  });

  it("subscribes to the live feed and unsubscribes when it goes away", async () => {
    const { unmount } = render(<App />);
    await waitFor(() => expect(onAgentEvent).toHaveBeenCalled());

    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalled());
  });
});

describe("blockers", () => {
  it("says nothing can start when tmux is missing", async () => {
    api.systemStatus.mockResolvedValue(systemStatus({ tmux_available: false }));
    render(<App />);
    expect(await screen.findByText(/tmux was not found/)).toBeInTheDocument();
  });

  it("says nothing can start when no harness is installed", async () => {
    api.systemStatus.mockResolvedValue(
      systemStatus({ harnesses: [harness({ available: false })] }),
    );
    render(<App />);
    expect(await screen.findByText(/No agent harness found/)).toBeInTheDocument();
  });

  it("disables the form while a blocker stands", async () => {
    api.systemStatus.mockResolvedValue(systemStatus({ tmux_available: false }));
    render(<App />);

    await screen.findByText(/tmux was not found/);
    await userEvent.type(task(), "do it");
    expect(screen.getByRole("button", { name: /Delegate/ })).toBeDisabled();
  });

  it("shows no blockers on a healthy machine", async () => {
    render(<App />);
    await screen.findByText(/Delegate a task/);
    expect(screen.queryByText(/was not found/)).not.toBeInTheDocument();
  });
});

describe("the fleet summary", () => {
  it("counts what is running out of the total", async () => {
    api.listAgents.mockResolvedValue([
      agent({ id: "a", status: "running" }),
      agent({ id: "b", status: "completed" }),
    ]);
    render(<App />);
    expect(await screen.findByText(/1 running · 2 total/)).toBeInTheDocument();
  });

  it("totals what the fleet has cost", async () => {
    api.listAgents.mockResolvedValue([
      agent({ id: "a", usage: { cost_usd: 0.5 } }),
      agent({ id: "b", usage: { cost_usd: 0.25 } }),
    ]);
    render(<App />);
    expect(await screen.findByText(/\$0\.7500/)).toBeInTheDocument();
  });

  it("stays quiet about cost until there is some", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a", usage: {} })]);
    render(<App />);

    await screen.findByText(/1 running · 1 total/);
    expect(screen.queryByText(/\$/)).not.toBeInTheDocument();
  });
});

describe("delegating", () => {
  it("spawns, refreshes the list, and opens the new agent", async () => {
    const spawned = agent({ id: "new-1", name: "scout" });
    api.spawnAgent.mockResolvedValue(spawned);
    api.listAgents.mockResolvedValueOnce([]).mockResolvedValue([spawned]);

    render(<App />);
    await screen.findByText(/Delegate a task/);
    await userEvent.type(task(), "do it");
    await userEvent.click(screen.getByRole("button", { name: /Delegate/ }));

    await waitFor(() => expect(api.spawnAgent).toHaveBeenCalled());
    expect(api.listAgents).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(api.agentEvents).toHaveBeenCalledWith("new-1"));
  });

  it("reports a spawn that failed and leaves the fleet alone", async () => {
    api.spawnAgent.mockRejectedValue(new Error("harness not found"));

    render(<App />);
    await screen.findByText(/Delegate a task/);
    await userEvent.type(task(), "do it");
    await userEvent.click(screen.getByRole("button", { name: /Delegate/ }));

    expect(await screen.findByText(/harness not found/)).toBeInTheDocument();
  });
});

describe("selecting an agent", () => {
  it("backfills the history of a run that started before the window opened", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout" })]);
    api.agentEvents.mockResolvedValue([
      envelope({ kind: "message", text: "from history" }, { agent_id: "a1", seq: 0 }),
    ]);

    render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: /scout/ }));

    expect(await screen.findByText("from history")).toBeInTheDocument();
  });

  it("reports a history it could not fetch", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout" })]);
    api.agentEvents.mockRejectedValue(new Error("unknown agent"));

    render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: /scout/ }));

    expect(await screen.findByText(/unknown agent/)).toBeInTheDocument();
  });

  /**
   * Backfill and the live feed overlap, so the same event can arrive twice.
   * `seq` is the source of truth for what is already known.
   */
  it("merges live events with the backfill without duplicating them", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout" })]);
    api.agentEvents.mockResolvedValue([
      envelope({ kind: "message", text: "first" }, { agent_id: "a1", seq: 0 }),
      envelope({ kind: "message", text: "second" }, { agent_id: "a1", seq: 1 }),
    ]);

    const { container } = render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: /scout/ }));
    await screen.findByText("second");

    // The same seq the backfill already carried.
    emit(envelope({ kind: "message", text: "first" }, { agent_id: "a1", seq: 0 }));

    await waitFor(() => expect(container.querySelectorAll("article.event")).toHaveLength(2));
  });

  it("appends a genuinely new live event", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout" })]);

    render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: /scout/ }));

    emit(envelope({ kind: "message", text: "live now" }, { agent_id: "a1", seq: 5 }));

    expect(await screen.findByText("live now")).toBeInTheDocument();
  });

  /** A chatty agent must not cause one refetch per line. */
  it("coalesces the refresh a burst of events triggers", async () => {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout" })]);
    render(<App />);
    await screen.findByRole("button", { name: /scout/ });
    const before = api.listAgents.mock.calls.length;

    for (let seq = 0; seq < 10; seq++) {
      emit(envelope({ kind: "message", text: `line ${seq}` }, { agent_id: "a1", seq }));
    }

    await waitFor(() => expect(api.listAgents.mock.calls.length).toBe(before + 1));
    // Ten events, one refetch — that is the whole point of the debounce.
    expect(api.listAgents.mock.calls.length).toBe(before + 1);
  });
});

// Note, surfaced by these tests rather than assumed: App schedules its refresh
// with window.setTimeout but its event effect's cleanup only unlistens — it
// never clears `refreshTimer`. A timer scheduled just before unmount still
// fires, calling api.listAgents for a component that no longer exists. Harmless
// in the app (one stray fetch on close) but it leaks across tests, which is why
// the assertions above measure deltas rather than absolute call counts.

describe("acting on the selected agent", () => {
  async function openAgent() {
    api.listAgents.mockResolvedValue([agent({ id: "a1", name: "scout", status: "running" })]);
    render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: /scout/ }));
  }

  it("kills it and refreshes", async () => {
    await openAgent();
    // A delta, not an absolute count: App never clears its debounce timer on
    // unmount, so a timer scheduled by an earlier test can still fire here and
    // add a stray listAgents call. See the note in App.test.tsx's tail.
    const before = api.listAgents.mock.calls.length;

    await userEvent.click(screen.getByRole("button", { name: "Kill" }));

    expect(api.killAgent).toHaveBeenCalledWith("a1");
    await waitFor(() =>
      expect(api.listAgents.mock.calls.length).toBeGreaterThan(before),
    );
  });

  it("reports a kill that failed", async () => {
    api.killAgent.mockRejectedValue(new Error("no such session"));
    await openAgent();
    await userEvent.click(screen.getByRole("button", { name: "Kill" }));

    expect(await screen.findByText(/no such session/)).toBeInTheDocument();
  });

  it("opens a terminal on it", async () => {
    await openAgent();
    await userEvent.click(screen.getByRole("button", { name: /Watch in tmux/ }));
    expect(api.openInTerminal).toHaveBeenCalledWith("a1");
  });

  it("reports a terminal that would not open", async () => {
    api.openInTerminal.mockRejectedValue(new Error("no terminal"));
    await openAgent();
    await userEvent.click(screen.getByRole("button", { name: /Watch in tmux/ }));

    expect(await screen.findByText(/no terminal/)).toBeInTheDocument();
  });
});

describe("errors", () => {
  it("surfaces a failure to read the machine's status", async () => {
    api.systemStatus.mockRejectedValue(new Error("backend down"));
    render(<App />);
    expect(await screen.findByText(/backend down/)).toBeInTheDocument();
  });

  it("surfaces a failure to list agents", async () => {
    api.listAgents.mockRejectedValue(new Error("cannot list"));
    render(<App />);
    expect(await screen.findByText(/cannot list/)).toBeInTheDocument();
  });

  it("lets the reader dismiss the banner", async () => {
    api.listAgents.mockRejectedValue(new Error("cannot list"));
    render(<App />);

    const banner = await screen.findByText(/cannot list/);
    await userEvent.click(banner);

    await waitFor(() => expect(screen.queryByText(/cannot list/)).not.toBeInTheDocument());
  });
});
