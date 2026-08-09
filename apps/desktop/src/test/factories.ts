import type {
  AgentEnvelope,
  AgentEvent,
  AgentSummary,
  HarnessInfo,
  SystemStatus,
} from "../types";

/** A running Claude Code agent. Override only what a test is about. */
export function agent(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "agent-1",
    name: "scout",
    harness: "claude_code",
    harness_label: "Claude Code",
    status: "running",
    cwd: "/work",
    model: null,
    permission: "ask",
    tmux_session: "jod-agent-1",
    attach_command: "tmux attach -t jod-agent-1",
    switch_command: "tmux switch-client -t jod-agent-1",
    session_closed: false,
    created_at_ms: 0,
    session_id: null,
    usage: {},
    event_count: 0,
    last_message: null,
    stream_path: "/runs/agent-1/stream.jsonl",
    ...overrides,
  };
}

/**
 * An envelope around one event.
 *
 * The event half is typed as `AgentEvent`, not `Omit<AgentEnvelope, …>` —
 * `Omit` over a union keeps only the keys every member shares, which would
 * reject `text`, `name` and the rest.
 */
export function envelope(
  event: AgentEvent,
  overrides: { agent_id?: string; at_ms?: number; seq?: number } = {},
): AgentEnvelope {
  return { agent_id: "agent-1", at_ms: 0, seq: 0, ...overrides, ...event };
}

export function harness(overrides: Partial<HarnessInfo> = {}): HarnessInfo {
  return {
    id: "claude_code",
    label: "Claude Code",
    available: true,
    path: "/usr/local/bin/claude",
    ...overrides,
  };
}

export function systemStatus(overrides: Partial<SystemStatus> = {}): SystemStatus {
  return {
    harnesses: [harness()],
    tmux_available: true,
    default_workdir: "/home/reljod",
    ...overrides,
  };
}
