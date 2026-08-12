import { useMemo } from "react";
import type { AgentNode, World } from "../state/world";
import { eventRate, statusRank, truncate } from "../state/world";
import { PHASE_LABEL } from "../render/palette";
import { harnessCode, totalTokens } from "../types";
import { shortPath } from "../render/renderer";
import { Sparkline } from "./Sparkline";

interface Props {
  world: World;
  selectedId: string | null;
  onSelect(id: string): void;
}

export function Roster({ world, selectedId, onSelect }: Props) {
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

  // Directories with more than one live agent — the charter's collision case.
  const contended = useMemo(() => {
    const counts = new Map<string, number>();
    for (const n of nodes) {
      if (n.summary.status !== "running") continue;
      counts.set(n.summary.cwd, (counts.get(n.summary.cwd) ?? 0) + 1);
    }
    return new Set([...counts].filter(([, c]) => c > 1).map(([cwd]) => cwd));
  }, [nodes]);

  const now = Date.now();

  return (
    <aside className="panel roster">
      <h2>FLEET <span className="count">{nodes.length}</span></h2>
      <div className="roster-list">
        {nodes.length === 0 && <p className="empty">Awaiting roster…</p>}
        {nodes.map((n) => {
          const live = n.summary.status === "running";
          const rate = eventRate(n, now);
          return (
            <button
              key={n.summary.id}
              className={[
                "roster-row",
                `st-${n.summary.status}`,
                selectedId === n.summary.id ? "sel" : "",
              ].join(" ")}
              onClick={() => onSelect(n.summary.id)}
            >
              <span className="rr-top">
                <i className={`hx hx-${n.summary.harness}`}>{harnessCode(n.summary.harness)}</i>
                <span className="rr-name">{n.summary.name}</span>
                {contended.has(n.summary.cwd) && (
                  <i className="warn" title="Another live agent shares this directory">⚠</i>
                )}
                {/* status and process_alive are different questions: a run can
                    read as finished while its process group is still winding
                    down, and — the case that matters — one marked running with
                    nothing alive behind it never reported how it ended. */}
                {n.summary.process_alive && !live && (
                  <i className="sess" title="its process group is still alive">▣</i>
                )}
              </span>
              <span className="rr-mid">
                <span className={`phase ph-${n.phase}`}>
                  {n.inFlight ? `▸ ${n.inFlight.name}` : PHASE_LABEL[n.phase]}
                </span>
                <Sparkline times={n.recentEventTimes} now={now} live={live} />
              </span>
              <span className="rr-bot">
                <span className="cwd" title={n.summary.cwd}>{shortPath(n.summary.cwd)}</span>
                <span className="nums">
                  {formatTokens(totalTokens(n.summary.usage))}
                  {n.summary.usage.cost_usd != null &&
                    ` · $${n.summary.usage.cost_usd.toFixed(2)}`}
                  {live && rate > 0 && ` · ${rate.toFixed(1)}/s`}
                </span>
              </span>
              {n.summary.last_message && (
                <span className="rr-msg">{truncate(n.summary.last_message, 84)}</span>
              )}
            </button>
          );
        })}
      </div>
    </aside>
  );
}

export function formatTokens(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}
