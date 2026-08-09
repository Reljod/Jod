import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { agent, envelope } from "../test/factories";
import { EventStream } from "./EventStream";

function renderStream(props: Partial<Parameters<typeof EventStream>[0]> = {}) {
  const onKill = vi.fn();
  const onOpenTerminal = vi.fn();
  const view = render(
    <EventStream
      agent={agent()}
      events={[]}
      onKill={onKill}
      onOpenTerminal={onOpenTerminal}
      {...props}
    />,
  );
  return { ...view, onKill, onOpenTerminal };
}

describe("the header", () => {
  it("identifies the agent and how it is configured", () => {
    renderStream({
      agent: agent({
        name: "scout",
        status: "running",
        harness_label: "Claude Code",
        model: "claude-opus-5",
        cwd: "/work",
      }),
    });

    expect(screen.getByRole("heading", { name: /scout/ })).toBeInTheDocument();
    expect(screen.getByText("running")).toBeInTheDocument();
    expect(screen.getByText("Claude Code · claude-opus-5 · /work")).toBeInTheDocument();
  });

  it("omits the model when the harness never reported one", () => {
    renderStream({ agent: agent({ harness_label: "OpenCode", model: null, cwd: "/work" }) });
    expect(screen.getByText("OpenCode · /work")).toBeInTheDocument();
  });

  /** Most people running Jod already live in tmux, where `attach` refuses. */
  it("offers both the attach and the switch command while the session is open", () => {
    renderStream({
      agent: agent({
        attach_command: "tmux attach -t jod-a",
        switch_command: "tmux switch-client -t jod-a",
      }),
    });

    expect(screen.getByText("tmux attach -t jod-a")).toBeInTheDocument();
    expect(screen.getByText(/tmux switch-client -t jod-a/)).toBeInTheDocument();
    expect(screen.getByText(/from inside tmux/)).toBeInTheDocument();
  });

  it("replaces the commands with a note once the session is gone", () => {
    renderStream({ agent: agent({ session_closed: true }) });

    expect(screen.getByText("tmux session closed")).toBeInTheDocument();
    expect(screen.queryByText(/tmux attach/)).not.toBeInTheDocument();
  });

  it("disables both actions once there is no session to act on", () => {
    // Still "Kill" rather than "Close session": the label tracks the agent's
    // status, and this agent is running even though its session is gone.
    renderStream({ agent: agent({ session_closed: true, status: "running" }) });

    expect(screen.getByRole("button", { name: /Watch in tmux/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Kill" })).toBeDisabled();
  });

  /**
   * The session outlives the agent, so the button stays available after a run
   * finishes — that is the only way to reclaim it — but it stops saying "Kill".
   */
  it("offers to kill a running agent and to close a finished one's session", () => {
    const { rerender } = renderStream({ agent: agent({ status: "running" }) });
    expect(screen.getByRole("button", { name: "Kill" })).toBeEnabled();

    rerender(
      <EventStream
        agent={agent({ status: "completed" })}
        events={[]}
        onKill={vi.fn()}
        onOpenTerminal={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Close session" })).toBeEnabled();
  });

  it("reports the agent id when an action is taken", async () => {
    const { onKill, onOpenTerminal } = renderStream({ agent: agent({ id: "a7" }) });

    await userEvent.click(screen.getByRole("button", { name: /Watch in tmux/ }));
    await userEvent.click(screen.getByRole("button", { name: "Kill" }));

    expect(onOpenTerminal).toHaveBeenCalledWith("a7");
    expect(onKill).toHaveBeenCalledWith("a7");
  });
});

describe("the usage line", () => {
  it("stays hidden until the harness has reported something", () => {
    const { container } = renderStream({ agent: agent({ usage: {} }) });
    expect(container.querySelector(".usage")).toBeNull();
  });

  it("appears once there is a cost", () => {
    const { container } = renderStream({ agent: agent({ usage: { cost_usd: 0.1234 } }) });
    expect(container.querySelector(".usage")).toHaveTextContent("$0.1234");
  });

  it("appears once there are output tokens, even with no cost", () => {
    const { container } = renderStream({ agent: agent({ usage: { output_tokens: 12 } }) });
    expect(container.querySelector(".usage")).toHaveTextContent("12 out");
  });

  it("reads large counts with separators and drops what was not reported", () => {
    const { container } = renderStream({
      agent: agent({
        usage: { input_tokens: 12345, output_tokens: 67, cache_read_tokens: 8900, cost_usd: 1.5 },
      }),
    });

    const usage = container.querySelector(".usage")!.textContent!;
    expect(usage).toContain("12,345 in");
    expect(usage).toContain("67 out");
    expect(usage).toContain("8,900 cached");
    expect(usage).toContain("$1.5000");
  });

  /** A zero cache read is noise, not information. */
  it("omits a zero cache read", () => {
    const { container } = renderStream({
      agent: agent({ usage: { output_tokens: 1, cache_read_tokens: 0 } }),
    });
    expect(container.querySelector(".usage")).not.toHaveTextContent("cached");
  });
});

describe("the event list", () => {
  it("says what it is waiting for when nothing has arrived", () => {
    renderStream({ events: [] });
    expect(screen.getByText(/Waiting for the harness/)).toBeInTheDocument();
  });

  it("renders one article per event", () => {
    const { container } = renderStream({
      events: [
        envelope({ kind: "message", text: "one" }, { seq: 0 }),
        envelope({ kind: "message", text: "two" }, { seq: 1 }),
      ],
    });
    expect(container.querySelectorAll("article.event")).toHaveLength(2);
  });

  it("shows a started event's model and session, and says so when it has neither", () => {
    renderStream({
      events: [
        envelope({ kind: "started", model: "claude-opus-5", session_id: "s1" }, { seq: 0 }),
        envelope({ kind: "started", model: null, session_id: null }, { seq: 1 }),
      ],
    });

    expect(screen.getByText("claude-opus-5 · s1")).toBeInTheDocument();
    expect(screen.getByText("session opened")).toBeInTheDocument();
  });

  it("distinguishes thinking from a message", () => {
    const { container } = renderStream({
      events: [
        envelope({ kind: "thinking", text: "pondering" }, { seq: 0 }),
        envelope({ kind: "message", text: "the answer" }, { seq: 1 }),
      ],
    });

    expect(screen.getByText("thinking")).toBeInTheDocument();
    expect(screen.getByText("pondering")).toBeInTheDocument();
    expect(container.querySelector(".event.message")).toHaveTextContent("the answer");
  });

  it("pretty-prints a tool call's input and copes with one that has none", () => {
    renderStream({
      events: [
        envelope({ kind: "tool_call", name: "bash", input: { cmd: "ls" } }, { seq: 0 }),
        envelope({ kind: "tool_call", name: "read" }, { seq: 1 }),
      ],
    });

    expect(screen.getByText("tool → bash")).toBeInTheDocument();
    expect(screen.getByText(/"cmd": "ls"/)).toBeInTheDocument();
    expect(screen.getByText("tool → read")).toBeInTheDocument();
  });

  it("marks a failed tool result as an error", () => {
    const { container } = renderStream({
      events: [
        envelope({ kind: "tool_result", name: "bash", summary: "boom", is_error: true }, { seq: 0 }),
        envelope({ kind: "tool_result", name: "read", summary: "ok", is_error: false }, { seq: 1 }),
      ],
    });

    expect(screen.getByText("tool ← bash")).toBeInTheDocument();
    expect(container.querySelector(".event.error")).toHaveTextContent("boom");
    expect(container.querySelector(".event.tool")).toHaveTextContent("ok");
  });

  it("survives a tool result with no summary at all", () => {
    renderStream({
      events: [envelope({ kind: "tool_result", name: "bash", is_error: false }, { seq: 0 })],
    });
    expect(screen.getByText("tool ← bash")).toBeInTheDocument();
  });

  it("shows a finished run's answer above its usage", () => {
    renderStream({
      events: [
        envelope(
          { kind: "finished", text: "all done", is_error: false, usage: { cost_usd: 0.5 } },
          { seq: 0 },
        ),
      ],
    });

    expect(screen.getByText("finished")).toBeInTheDocument();
    expect(screen.getByText(/all done/)).toBeInTheDocument();
    expect(screen.getByText(/\$0\.5000/)).toBeInTheDocument();
  });

  it("calls a failed run failed", () => {
    const { container } = renderStream({
      events: [envelope({ kind: "finished", is_error: true, usage: {} }, { seq: 0 })],
    });

    expect(screen.getByText("failed")).toBeInTheDocument();
    expect(container.querySelector(".event.error")).toBeInTheDocument();
  });

  it("says so when a run finished having produced nothing", () => {
    renderStream({
      events: [envelope({ kind: "finished", is_error: false, usage: {} }, { seq: 0 })],
    });
    expect(screen.getByText("no output")).toBeInTheDocument();
  });

  it("keeps unrecognised output rather than hiding it", () => {
    renderStream({ events: [envelope({ kind: "raw", line: "warning: something" }, { seq: 0 })] });

    expect(screen.getByText("raw")).toBeInTheDocument();
    expect(screen.getByText("warning: something")).toBeInTheDocument();
  });

  it("surfaces an error event's message", () => {
    const { container } = renderStream({
      events: [envelope({ kind: "error", message: "it broke" }, { seq: 0 })],
    });

    expect(container.querySelector(".event.error")).toHaveTextContent("it broke");
  });

  it("stamps each event with its arrival time", () => {
    const at = new Date("2026-08-09T12:34:56Z").getTime();
    renderStream({ events: [envelope({ kind: "message", text: "hi" }, { seq: 0, at_ms: at })] });

    expect(screen.getByText(new Date(at).toLocaleTimeString())).toBeInTheDocument();
  });

  /** Nobody wants to scroll down to see what just happened. */
  it("scrolls to the newest event as events arrive", () => {
    const scroll = window.HTMLElement.prototype.scrollIntoView as ReturnType<typeof vi.fn>;
    renderStream({ events: [envelope({ kind: "message", text: "hi" }, { seq: 0 })] });
    expect(scroll).toHaveBeenCalled();
  });
});
