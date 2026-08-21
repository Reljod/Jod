import { useMemo } from "react";
import type { AgentNode, World } from "../state/world";
import { statusRank } from "../state/world";
import { TIER_LABEL, harnessCode, totalTokens, type Tier } from "../types";
import { shortPath } from "../render/renderer";
import { useSelection } from "../hooks/useSelection";
import { SelectionBar } from "./SelectionBar";

interface Props {
  world: World;
  selectedId: string | null;
  /**
   * Which rank each run belongs to, by run id, from the fleet tree.
   *
   * This panel lists the daemon's roster, and neither a roster entry nor an
   * `AgentEnvelope` carries a conversation, a work or a project — a run's rank
   * is not on that wire at all. The fleet is where the answer lives, so it is
   * borrowed rather than guessed from the run's name. A run the tree has not
   * caught up with yet simply has no entry and draws untiered, which is the
   * right answer for the half-second before the next query lands.
   */
  tiers: ReadonlyMap<string, Tier>;
  /** Select a session *and* open it — one click, both. */
  onOpen(id: string): void;
  onDelete(ids: string[]): void;
  canWrite: boolean;
}

/**
 * Every session this daemon knows about, and the place they are deleted from.
 *
 * ## Why it says so little
 *
 * It used to carry, per row: a harness badge, the name, two status glyphs, a
 * phase caption, a throughput sparkline, the working directory, a token count,
 * a dollar figure, an events-per-second figure, and eighty-four characters of
 * the last message. Eleven facts about a run you have not selected. At forty
 * rows that is a wall of text with no shape, and the one question the panel is
 * actually for — *which of these do I want to look at* — got harder to answer
 * with every fact added.
 *
 * So a row is now three things: whether it is alive, what it is called, and
 * where it is working. Everything else moved to the dossier, which is the panel
 * for the run you *have* chosen. Nothing was deleted from the HUD; it was put
 * where it answers a question somebody is asking.
 *
 * ## Clicking a row opens it
 *
 * Selecting and reading were two gestures — click here, then find READ SESSION
 * in the dossier. They are one now. A list of sessions whose rows do not open
 * the session is a list that makes you learn a second step for the only thing
 * anybody does with it.
 *
 * ## The checkbox is not the row
 *
 * Selecting for deletion and choosing what to read are different intents, so
 * they are different targets: the checkbox is its own button and stops the
 * click from reaching the row. A single click target doing both would mean
 * every attempt to read a session is one mis-aim away from arming a delete.
 */
export function Sessions({
  world,
  selectedId,
  tiers,
  onOpen,
  onDelete,
  canWrite,
}: Props) {
  const nodes = useMemo(() => {
    const list: AgentNode[] = [];
    for (const id of world.order) {
      const n = world.agents.get(id);
      if (n) list.push(n);
    }
    // Live first, then by most recent activity — the order an operator scans.
    return list.sort(
      (a, b) =>
        statusRank(a.summary.status) - statusRank(b.summary.status) ||
        b.lastEventAt - a.lastEventAt,
    );
  }, [world, world.revision]);

  const ids = useMemo(() => nodes.map((n) => n.summary.id), [nodes]);
  const selection = useSelection(ids);

  // Directories with more than one live agent — the charter's collision case,
  // and the one warning worth keeping on a row you have not opened.
  const contended = useMemo(() => {
    const counts = new Map<string, number>();
    for (const n of nodes) {
      if (n.summary.status !== "running") continue;
      counts.set(n.summary.cwd, (counts.get(n.summary.cwd) ?? 0) + 1);
    }
    return new Set([...counts].filter(([, c]) => c > 1).map(([cwd]) => cwd));
  }, [nodes]);

  const running = nodes.filter((n) => n.summary.status === "running").length;

  return (
    <aside className="panel sessions">
      <h2>
        SESSIONS
        {running > 0 && <span className="live-count">{running}</span>}
        <span className="count">{nodes.length}</span>
      </h2>

      <div className="session-list">
        {nodes.length === 0 && <p className="empty">No sessions.</p>}
        {nodes.map((n) => {
          const s = n.summary;
          const live = s.status === "running";
          const picked = selection.has(s.id);
          const tier = tiers.get(s.id);
          return (
            <div
              key={s.id}
              className={[
                "session-row",
                `st-${s.status}`,
                tier ? `t-${tier}` : "",
                selectedId === s.id ? "sel" : "",
                picked ? "picked" : "",
              ].join(" ")}
            >
              <button
                className="pick"
                role="checkbox"
                aria-checked={picked}
                aria-label={`Select ${s.name}`}
                onClick={() => selection.toggle(s.id)}
              >
                <i />
              </button>
              <button className="session-open" onClick={() => onOpen(s.id)} title={s.name}>
                <span className="sr-top">
                  <i className={`state st-${s.status}${live ? " live" : ""}`} />
                  <span className="sr-name">{s.name}</span>
                  {contended.has(s.cwd) && (
                    <i className="warn" title="Another live session shares this directory">
                      ⚠
                    </i>
                  )}
                </span>
                <span className="sr-bot">
                  {tier && <span className={`rank t-${tier}`}>{TIER_LABEL[tier]}</span>}
                  <span className="cwd">{shortPath(s.cwd)}</span>
                  <span className="hxq">{harnessCode(s.harness)}</span>
                </span>
              </button>
            </div>
          );
        })}
      </div>

      <SelectionBar
        selection={selection}
        canWrite={canWrite}
        noun="session"
        onDelete={() => onDelete(selection.chosen)}
      />
    </aside>
  );
}

export function formatTokens(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}

/** Total tokens for one node, for the panels that show a single figure. */
export function nodeTokens(node: AgentNode): number {
  return totalTokens(node.summary.usage);
}
