import type { AgentLine } from "../session";

export interface AgentsSheetProps {
  agents: AgentLine[];
  currentAgentId: string | null;
  canWrite: boolean;
  onKill(id: string): void;
  /** Continue this delegation's conversation — the tap form of `/resume`. */
  onResume(id: string): void;
  onClose(): void;
}

/**
 * The mobile form of the TUI's `Ctrl-A` panel.
 *
 * This is the part that makes the app an orchestrator's window rather than a
 * chat client: Jod's job is watching several agents, and this lists every
 * delegation the daemon knows about — including ones started from the terminal,
 * or before this phone ever connected, because the daemon rehydrates them from
 * SQLite.
 *
 * A bottom sheet rather than a full screen so the conversation stays visible
 * behind it, and so it is dismissible with a thumb.
 */
export function AgentsSheet({
  agents,
  currentAgentId,
  canWrite,
  onKill,
  onResume,
  onClose,
}: AgentsSheetProps) {
  return (
    <div
      className="sheet"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Agents"
    >
      {/* Taps inside the panel must not fall through to the backdrop. */}
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <header>
          <span>AGENTS · {agents.length}</span>
          <span style={{ flex: 1 }} />
          <button className="iconbtn" onClick={onClose}>
            CLOSE
          </button>
        </header>

        {agents.length === 0 ? (
          <p className="placeholder">Nothing has run yet.</p>
        ) : (
          <ul>
            {agents.map((agent) => (
              <li key={agent.id}>
                <span className="name">
                  {agent.id === currentAgentId ? "› " : ""}
                  {agent.name}
                </span>
                <span className="meta">{agent.harness}</span>
                <span className={`badge ${agent.status}`}>
                  {String(agent.status).toUpperCase()}
                </span>
                {/* The id `/resume` actually needs. The TUI makes you read it
                    off the panel and type it back; here it is a tap. An agent
                    that never reported a conversation offers nothing, because
                    resuming it would silently start a fresh one. */}
                {agent.session && agent.id !== currentAgentId ? (
                  <button className="resume" onClick={() => onResume(agent.session!)}>
                    RESUME
                  </button>
                ) : null}
                {canWrite && agent.status === "running" ? (
                  <button className="stop" onClick={() => onKill(agent.id)}>
                    STOP
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
