import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const listen = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: (...args: unknown[]) => listen(...args) }));

const { api, onAgentEvent } = await import("./api");

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  invoke.mockResolvedValue(undefined);
});

describe("the command surface", () => {
  it("names every command the Rust side exposes", async () => {
    await api.systemStatus();
    await api.listAgents();
    await api.report();

    expect(invoke.mock.calls.map(([name]) => name)).toEqual([
      "system_status",
      "list_agents",
      "report",
    ]);
  });

  it("passes an agent id under the key the Rust command expects", async () => {
    await api.agentEvents("a1");
    expect(invoke).toHaveBeenCalledWith("agent_events", { id: "a1" });

    await api.killAgent("a2");
    expect(invoke).toHaveBeenCalledWith("kill_agent", { id: "a2" });

    await api.openInTerminal("a3");
    expect(invoke).toHaveBeenCalledWith("open_in_terminal", { id: "a3" });
  });

  it("wraps spawn arguments in the `args` envelope", async () => {
    const args = { name: "n", harness: "claude_code" as const, prompt: "p" };
    await api.spawnAgent(args);
    expect(invoke).toHaveBeenCalledWith("spawn_agent", { args });
  });

  it("lets a rejected command reach the caller rather than swallowing it", async () => {
    invoke.mockRejectedValueOnce(new Error("tmux not found"));
    await expect(api.systemStatus()).rejects.toThrow("tmux not found");
  });
});

describe("the live event subscription", () => {
  it("listens on the channel name the Rust side emits", () => {
    onAgentEvent(() => {});
    expect(listen).toHaveBeenCalledWith("jod://agent-event", expect.any(Function));
  });

  /** The handler wants the envelope, not Tauri's wrapper around it. */
  it("hands the handler the payload, unwrapped", () => {
    const handler = vi.fn();
    onAgentEvent(handler);

    const [, forward] = listen.mock.calls[0] as [string, (e: unknown) => void];
    forward({ event: "jod://agent-event", id: 1, payload: { seq: 7 } });

    expect(handler).toHaveBeenCalledWith({ seq: 7 });
  });

  it("returns whatever listen returned, so callers can unlisten", () => {
    const unlisten = Promise.resolve(() => {});
    listen.mockReturnValueOnce(unlisten);
    expect(onAgentEvent(() => {})).toBe(unlisten);
  });
});
