import { useMemo, useState } from "react";
import type { AgentNode, World } from "../state/world";
import { eventRate, truncate } from "../state/world";
import { PHASE_LABEL, STATUS_LABEL } from "../render/palette";
import { harnessCode, totalTokens } from "../types";
import { formatTokens } from "./Roster";

interface Props {
  world: World;
  selectedId: string | null;
  onKill(id: string): void;
  onResume(node: AgentNode): void;
  /** Open this run in the trajectory view. */
  onRead(id: string): void;
  canWrite: boolean;
}

/** Everything known about one agent, including how to reach into its session. */
export function Dossier({ world, selectedId, onKill, onResume, onRead, canWrite }: Props) {
  const node = selectedId ? world.agents.get(selectedId) : undefined;
  const [copied, setCopied] = useState<string | null>(null);

  const feed = useMemo(
    () => (node ? world.feed.filter((f) => f.agentId === node.summary.id).slice(-90) : []),
    [world.feed, world.revision, node],
  );

  if (!node) {
    return (
      <aside className="panel dossier empty-dossier">
        <h2>DOSSIER</h2>
        <p className="empty">
          Select a node to inspect it.
          <br />
          <span className="hint">Drag to reposition · scroll to zoom · double-click to clear</span>
        </p>
      </aside>
    );
  }

  const s = node.summary;
  const live = s.status === "running";
  const now = Date.now();
  const tokens = totalTokens(s.usage);

  const copy = async (text: string, key: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(key);
      setTimeout(() => setCopied((c) => (c === key ? null : c)), 1400);
    } catch {
      setCopied(null);
    }
  };

  return (
    <aside className="panel dossier">
      <h2>
        DOSSIER
        <span className={`badge st-${s.status}`}>{STATUS_LABEL[s.status]}</span>
      </h2>

      <div className="dz-name">
        <i className={`hx hx-${s.harness}`}>{harnessCode(s.harness)}</i>
        <span>{s.name}</span>
      </div>

      <div className={`dz-phase ph-${node.phase}`}>
        {node.inFlight ? (
          <>
            <span className="spin" /> {node.inFlight.name}
            <span className="elapsed">
              {((now - node.inFlight.startedAt) / 1000).toFixed(1)}s
            </span>
          </>
        ) : (
          PHASE_LABEL[node.phase]
        )}
      </div>

      {node.thought && (
        <p className="dz-thought" title="Latest reasoning the harness surfaced">
          “{truncate(node.thought, 220)}”
        </p>
      )}

      <dl className="dz-facts">
        <Fact k="MODEL" v={s.model ?? "—"} />
        <Fact k="CWD" v={s.cwd} mono wrap />
        <Fact k="PERMISSION" v={s.permission.replace("_", " ").toUpperCase()} />
        <Fact k="SESSION" v={s.session_id ?? "pending"} mono />
        <Fact
          k="PROCESS"
          v={
            s.pgid == null
              ? "not launched"
              : `pgid ${s.pgid} · ${s.process_alive ? "ALIVE" : "GONE"}`
          }
          mono
        />
        <Fact k="EVENTS" v={String(s.event_count)} />
        <Fact k="RATE" v={live ? `${eventRate(node, now).toFixed(2)}/s` : "—"} />
        <Fact k="TOKENS" v={formatTokens(tokens)} />
        <Fact k="SPEND" v={s.usage.cost_usd != null ? `$${s.usage.cost_usd.toFixed(4)}` : "—"} />
        <Fact k="FAULTS" v={String(node.errorCount)} />
      </dl>

      {/* Offered whatever the run's state: `jod watch` replays a finished run
          from the store as readily as it follows a live one. There is no longer
          a second command for "from inside tmux", because there is no tmux. */}
      {/* Two ways to read the same run. `jod watch` replays it in a terminal;
          the trajectory reads it here, which is the only one available to
          someone holding a phone. */}
      <div className="dz-attach">
        <button onClick={() => onRead(s.id)} title="Read this session end to end">
          READ SESSION
        </button>
        <button onClick={() => copy(s.watch_command, "watch")}>
          {copied === "watch" ? "COPIED" : "COPY WATCH"}
        </button>
      </div>

      <div className="dz-actions">
        <button
          className="danger"
          disabled={!live || !canWrite}
          title={!canWrite ? "Read-only session" : live ? "Terminate this run" : "Not running"}
          onClick={() => onKill(s.id)}
        >
          TERMINATE
        </button>
        <button
          disabled={!s.session_id || !canWrite}
          title={
            !canWrite
              ? "Read-only session"
              : s.session_id
                ? "Spawn a new agent continuing this conversation"
                : "No session id yet"
          }
          onClick={() => onResume(node)}
        >
          CONTINUE THREAD
        </button>
      </div>

      <h3>TOOL TRACE</h3>
      <div className="dz-tools">
        {node.tools.length === 0 && <p className="empty">No tool calls yet.</p>}
        {[...node.tools].reverse().map((t, i) => (
          <div key={i} className={`tool ${t.endedAt === null ? "live" : t.isError ? "bad" : ""}`}>
            <span className="tname">{t.name}</span>
            <span className="tdur">
              {t.endedAt === null
                ? `${((now - t.startedAt) / 1000).toFixed(1)}s…`
                : `${((t.endedAt - t.startedAt) / 1000).toFixed(1)}s`}
            </span>
            {t.summary && <span className="tsum">{truncate(t.summary, 60)}</span>}
          </div>
        ))}
      </div>

      <h3>STREAM</h3>
      <div className="dz-stream">
        {feed.map((f) => (
          <div key={f.id} className={`ev k-${f.kind} ${f.isError ? "bad" : ""}`}>
            <span className="seq">{String(f.seq).padStart(4, "0")}</span>
            <span className="kind">{f.kind}</span>
            <span className="txt">{truncate(f.text, 120)}</span>
          </div>
        ))}
      </div>
    </aside>
  );
}

function Fact({ k, v, mono, wrap }: { k: string; v: string; mono?: boolean; wrap?: boolean }) {
  return (
    <>
      <dt>{k}</dt>
      <dd className={[mono ? "mono" : "", wrap ? "wrap" : ""].join(" ")} title={v}>
        {v}
      </dd>
    </>
  );
}
