import type { AgentSummary } from "../types";

interface Props {
  agents: AgentSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

export function AgentList({ agents, selectedId, onSelect }: Props) {
  if (agents.length === 0) {
    return <p className="empty">No agents yet. Delegate something.</p>;
  }

  return (
    <ul className="agents">
      {agents.map((agent) => (
        <li key={agent.id}>
          <button
            className={agent.id === selectedId ? "agent selected" : "agent"}
            onClick={() => onSelect(agent.id)}
          >
            <span className={`dot ${agent.status}`} aria-hidden="true" />
            <span className="agent-main">
              <span className="agent-name">{agent.name}</span>
              <span className="agent-meta">
                {agent.harness_label} · {agent.status}
              </span>
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}
