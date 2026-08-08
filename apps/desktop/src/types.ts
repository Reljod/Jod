// Mirrors the serde representation of jod-core's public types.
// Keep in step with crates/jod-core/src/{event,service,harness}.rs.

export type HarnessKind = "claude_code" | "open_code";
export type PermissionPolicy = "ask" | "accept_edits" | "bypass";
export type AgentStatus = "running" | "completed" | "failed" | "killed";

export interface Usage {
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  cost_usd?: number;
}

export interface HarnessInfo {
  id: string;
  label: string;
  available: boolean;
  path: string | null;
}

export interface SystemStatus {
  harnesses: HarnessInfo[];
  tmux_available: boolean;
  default_workdir: string;
}

export interface AgentSummary {
  id: string;
  name: string;
  harness: HarnessKind;
  harness_label: string;
  status: AgentStatus;
  cwd: string;
  model: string | null;
  permission: PermissionPolicy;
  tmux_session: string;
  attach_command: string;
  /** Use this instead of attach_command from inside an existing tmux session. */
  switch_command: string;
  /** Sessions outlive the agent, so this is a different question to `status`. */
  session_closed: boolean;
  created_at_ms: number;
  session_id: string | null;
  usage: Usage;
  event_count: number;
  last_message: string | null;
  stream_path: string;
}

export interface Report {
  running: number;
  completed: number;
  failed: number;
  killed: number;
  total_cost_usd: number;
  agents: AgentSummary[];
}

/** `AgentEvent` is an internally-tagged enum; `kind` discriminates it. */
export type AgentEvent =
  | { kind: "started"; session_id: string | null; model: string | null }
  | { kind: "thinking"; text: string }
  | { kind: "message"; text: string }
  | { kind: "tool_call"; name: string; input?: unknown }
  | { kind: "tool_result"; name: string; summary?: string; is_error: boolean }
  | {
      kind: "finished";
      text?: string;
      exit_code?: number;
      is_error: boolean;
      usage: Usage;
    }
  | { kind: "raw"; line: string }
  | { kind: "error"; message: string };

/** The event, flattened together with its envelope fields. */
export type AgentEnvelope = AgentEvent & {
  agent_id: string;
  at_ms: number;
  seq: number;
};

export interface SpawnArgs {
  name: string;
  harness: HarnessKind;
  prompt: string;
  cwd?: string;
  model?: string;
  permission?: PermissionPolicy;
}
