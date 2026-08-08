import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  AgentEnvelope,
  AgentSummary,
  Report,
  SpawnArgs,
  SystemStatus,
} from "./types";

/** Must match `AGENT_EVENT` in src-tauri/src/lib.rs. */
const AGENT_EVENT = "jod://agent-event";

export const api = {
  systemStatus: () => invoke<SystemStatus>("system_status"),
  spawnAgent: (args: SpawnArgs) => invoke<AgentSummary>("spawn_agent", { args }),
  listAgents: () => invoke<AgentSummary[]>("list_agents"),
  agentEvents: (id: string) => invoke<AgentEnvelope[]>("agent_events", { id }),
  killAgent: (id: string) => invoke<void>("kill_agent", { id }),
  report: () => invoke<Report>("report"),
  openInTerminal: (id: string) => invoke<void>("open_in_terminal", { id }),
};

/** Subscribe to live agent activity. Returns an unlisten function. */
export function onAgentEvent(handler: (envelope: AgentEnvelope) => void) {
  return listen<AgentEnvelope>(AGENT_EVENT, (event) => handler(event.payload));
}
