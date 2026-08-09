import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { harness } from "../test/factories";
import { SpawnForm } from "./SpawnForm";

function renderForm(props: Partial<Parameters<typeof SpawnForm>[0]> = {}) {
  const onSpawn = vi.fn().mockResolvedValue(undefined);
  const view = render(
    <SpawnForm
      harnesses={[harness()]}
      defaultWorkdir="/home/reljod"
      disabled={false}
      onSpawn={onSpawn}
      {...props}
    />,
  );
  return { ...view, onSpawn };
}

const task = () => screen.getByRole("textbox", { name: /Task/ });
const submit = () => screen.getByRole("button", { name: /Delegate|Starting/ });

describe("what the form offers", () => {
  it("starts in the working directory the backend suggested", () => {
    renderForm({ defaultWorkdir: "/somewhere" });
    expect(screen.getByRole("textbox", { name: /Working directory/ })).toHaveValue("/somewhere");
  });

  it("lists every harness, marking the ones that are not installed", () => {
    renderForm({
      harnesses: [
        harness({ id: "claude_code", label: "Claude Code", available: true }),
        harness({ id: "open_code", label: "OpenCode", available: false }),
      ],
    });

    expect(screen.getByRole("option", { name: "Claude Code" })).toBeEnabled();
    expect(screen.getByRole("option", { name: /OpenCode — not installed/ })).toBeDisabled();
  });

  it("offers every permission policy", () => {
    renderForm();
    for (const label of ["Ask", "Accept edits", "Bypass"]) {
      expect(screen.getByRole("option", { name: label })).toBeInTheDocument();
    }
  });
});

describe("when the form refuses to submit", () => {
  it("stays disabled until there is a task to delegate", async () => {
    renderForm();
    expect(submit()).toBeDisabled();

    await userEvent.type(task(), "do the thing");
    expect(submit()).toBeEnabled();
  });

  it("treats whitespace as no task at all", async () => {
    renderForm();
    await userEvent.type(task(), "   ");
    expect(submit()).toBeDisabled();
  });

  it("stays disabled while the machine is missing something", async () => {
    renderForm({ disabled: true });
    await userEvent.type(task(), "do the thing");
    expect(submit()).toBeDisabled();
  });

  it("stays disabled when no harness is installed", async () => {
    renderForm({ harnesses: [harness({ available: false })] });
    await userEvent.type(task(), "do the thing");
    expect(submit()).toBeDisabled();
  });
});

describe("submitting", () => {
  it("sends what was typed", async () => {
    const { onSpawn } = renderForm();

    await userEvent.type(screen.getByRole("textbox", { name: /Agent name/ }), "scout");
    await userEvent.type(task(), "summarise the repo");
    await userEvent.type(screen.getByRole("textbox", { name: /Model/ }), "claude-opus-5");
    await userEvent.selectOptions(screen.getByRole("combobox", { name: /Permissions/ }), "bypass");
    await userEvent.click(submit());

    expect(onSpawn).toHaveBeenCalledWith({
      name: "scout",
      harness: "claude_code",
      prompt: "summarise the repo",
      cwd: "/home/reljod",
      model: "claude-opus-5",
      permission: "bypass",
    });
  });

  it("names an unnamed agent rather than sending an empty string", async () => {
    const { onSpawn } = renderForm();

    await userEvent.type(screen.getByRole("textbox", { name: /Agent name/ }), "   ");
    await userEvent.type(task(), "do it");
    await userEvent.click(submit());

    expect(onSpawn).toHaveBeenCalledWith(expect.objectContaining({ name: "agent" }));
  });

  it("passes on the chosen harness", async () => {
    const { onSpawn } = renderForm({
      harnesses: [
        harness({ id: "claude_code", label: "Claude Code" }),
        harness({ id: "open_code", label: "OpenCode" }),
      ],
    });

    await userEvent.selectOptions(screen.getByRole("combobox", { name: /Harness/ }), "open_code");
    await userEvent.type(task(), "do it");
    await userEvent.click(submit());

    expect(onSpawn).toHaveBeenCalledWith(expect.objectContaining({ harness: "open_code" }));
  });

  /** Only the task is one-shot — the rest is a setup you keep reusing. */
  it("clears the task and name but keeps the settings", async () => {
    renderForm();

    await userEvent.type(screen.getByRole("textbox", { name: /Agent name/ }), "scout");
    await userEvent.type(task(), "do it");
    await userEvent.type(screen.getByRole("textbox", { name: /Model/ }), "claude-opus-5");
    await userEvent.click(submit());

    await waitFor(() => expect(task()).toHaveValue(""));
    expect(screen.getByRole("textbox", { name: /Agent name/ })).toHaveValue("");
    expect(screen.getByRole("textbox", { name: /Model/ })).toHaveValue("claude-opus-5");
    expect(screen.getByRole("textbox", { name: /Working directory/ })).toHaveValue("/home/reljod");
  });

  it("says it is working, and refuses a second submission until it is done", async () => {
    let release: () => void = () => {};
    const onSpawn = vi.fn(() => new Promise<void>((resolve) => (release = resolve)));
    render(
      <SpawnForm
        harnesses={[harness()]}
        defaultWorkdir="/w"
        disabled={false}
        onSpawn={onSpawn}
      />,
    );

    await userEvent.type(task(), "do it");
    await userEvent.click(submit());

    expect(screen.getByRole("button", { name: "Starting…" })).toBeDisabled();

    release();
    await waitFor(() => expect(screen.getByRole("button", { name: "Delegate" })).toBeInTheDocument());
    expect(onSpawn).toHaveBeenCalledTimes(1);
  });

  /**
   * The parent reports failures itself (App catches and shows a banner), so
   * onSpawn resolves either way — the form's job is only to stop being busy.
   *
   * Note: SpawnForm has no catch of its own, only a `finally`. An onSpawn that
   * actually rejects escapes as an unhandled rejection. That is fine against
   * App, whose spawn() swallows everything, but it is a real constraint on the
   * prop contract worth knowing before wiring a different parent to it.
   */
  it("becomes usable again after a spawn the parent reported as failed", async () => {
    const onSpawn = vi.fn().mockResolvedValue(undefined);
    render(
      <SpawnForm harnesses={[harness()]} defaultWorkdir="/w" disabled={false} onSpawn={onSpawn} />,
    );

    await userEvent.type(task(), "do it");
    await userEvent.click(submit());

    // Back to "Delegate" rather than "Starting…". It is disabled again only
    // because clearing the task left nothing to submit.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Delegate" })).toBeInTheDocument(),
    );
    expect(task()).toHaveValue("");

    await userEvent.type(task(), "another task");
    expect(screen.getByRole("button", { name: "Delegate" })).toBeEnabled();
  });

  it("sends a working directory the user changed", async () => {
    const { onSpawn } = renderForm();

    const cwd = screen.getByRole("textbox", { name: /Working directory/ });
    await userEvent.clear(cwd);
    await userEvent.type(cwd, "/elsewhere");
    await userEvent.type(task(), "do it");
    await userEvent.click(submit());

    expect(onSpawn).toHaveBeenCalledWith(expect.objectContaining({ cwd: "/elsewhere" }));
  });
});
