import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { agent } from "../test/factories";
import { AgentList } from "./AgentList";

describe("AgentList", () => {
  it("invites the first delegation when there is nothing to show", () => {
    render(<AgentList agents={[]} selectedId={null} onSelect={vi.fn()} />);
    expect(screen.getByText(/No agents yet/i)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("shows each agent's name, harness and status", () => {
    render(
      <AgentList
        agents={[agent({ id: "a", name: "scout", harness_label: "Claude Code", status: "running" })]}
        selectedId={null}
        onSelect={vi.fn()}
      />,
    );

    expect(screen.getByText("scout")).toBeInTheDocument();
    expect(screen.getByText("Claude Code · running")).toBeInTheDocument();
  });

  it("lists every agent it is given", () => {
    render(
      <AgentList
        agents={[agent({ id: "a", name: "one" }), agent({ id: "b", name: "two" })]}
        selectedId={null}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getAllByRole("button")).toHaveLength(2);
  });

  it("marks only the selected agent as selected", () => {
    render(
      <AgentList
        agents={[agent({ id: "a", name: "one" }), agent({ id: "b", name: "two" })]}
        selectedId="b"
        onSelect={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: /one/ }).className).toBe("agent");
    expect(screen.getByRole("button", { name: /two/ }).className).toBe("agent selected");
  });

  it("reports which agent was clicked", async () => {
    const onSelect = vi.fn();
    render(
      <AgentList
        agents={[agent({ id: "wanted", name: "one" })]}
        selectedId={null}
        onSelect={onSelect}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /one/ }));

    expect(onSelect).toHaveBeenCalledWith("wanted");
  });

  /** The dot is how status reads at a glance, so it must track the status. */
  it("colours the status dot by status", () => {
    const { container } = render(
      <AgentList agents={[agent({ status: "failed" })]} selectedId={null} onSelect={vi.fn()} />,
    );
    expect(container.querySelector(".dot")).toHaveClass("failed");
  });
});
