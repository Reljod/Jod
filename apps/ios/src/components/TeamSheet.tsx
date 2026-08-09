import { taskIsClaimed, taskIsDone, type Member, type TeamTask } from "../contract";
import { HARNESS_LABEL } from "../session";

export interface TeamSheetProps {
  /** The team being watched, or `null` when none was named. */
  team: string | null;
  /** Every team the daemon knows about, for the picker. */
  teams: string[];
  members: Member[];
  tasks: TeamTask[];
  onWatch(team: string): void;
  onClose(): void;
}

/**
 * The mobile form of the TUI's `Ctrl-G` panel.
 *
 * A team is the thing no single harness can do: a lead on Claude Code with
 * teammates on AGY and OpenCode, coordinating through one inbox. This is the
 * window onto it — who is on the team, what each is doing, and the shared
 * board.
 *
 * **Read-only, and that is the design.** Joining, claiming and messaging are
 * how a *teammate* participates, and a teammate is an agent on the box with a
 * tmux session. A phone watches the board; it does not play on it. So the
 * daemon exposes `GET /v1/teams/{team}` and nothing else, and there is no
 * button here that would need more.
 */
export function TeamSheet({
  team,
  teams,
  members,
  tasks,
  onWatch,
  onClose,
}: TeamSheetProps) {
  return (
    <div
      className="sheet"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Team"
    >
      {/* Taps inside the panel must not fall through to the backdrop. */}
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <header>
          <span>{team === null ? "TEAM" : `TEAM ${team.toUpperCase()}`}</span>
          <span style={{ flex: 1 }} />
          <button className="iconbtn" onClick={onClose}>
            CLOSE
          </button>
        </header>

        {team === null && teams.length > 0 ? (
          // Several boards exist and none was named. `jod tui` takes `--team`
          // on the command line; a phone gets to tap instead of guessing.
          <ul>
            {teams.map((name) => (
              <li key={name}>
                <span className="name">{name}</span>
                <button className="resume" onClick={() => onWatch(name)}>
                  WATCH
                </button>
              </li>
            ))}
          </ul>
        ) : team === null ? (
          <p className="placeholder">
            No team. Start one on the box with <code>jod team start</code>, then
            it shows up here.
          </p>
        ) : members.length === 0 ? (
          <p className="placeholder">No members yet.</p>
        ) : (
          <ul>
            {members.map((m) => (
              <li key={m.name}>
                <span className="name">{m.name}</span>
                <span className="meta">{HARNESS_LABEL[m.harness]}</span>
                <span className={`badge ${m.status}`}>{m.status.toUpperCase()}</span>
                <span className="role">{m.role}</span>
              </li>
            ))}
          </ul>
        )}

        {tasks.length === 0 ? null : (
          <>
            <header className="subhead">
              <span>BOARD · {tasks.filter(taskIsDone).length}/{tasks.length}</span>
            </header>
            <ul className="board">
              {tasks.map((t) => (
                <li key={t.id}>
                  {/* Open / claimed / done, so progress reads at a glance —
                      the same three marks the TUI draws. */}
                  <span className={`mark ${taskIsDone(t) ? "done" : taskIsClaimed(t) ? "claimed" : "open"}`}>
                    {taskIsDone(t) ? "✓" : taskIsClaimed(t) ? "◐" : "○"}
                  </span>
                  <span className="name">{t.title}</span>
                  {t.owner ? <span className="meta">({t.owner})</span> : null}
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </div>
  );
}
