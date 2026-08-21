import { useMemo, useState } from "react";
import type { AgentNode, World } from "../state/world";
import { eventRate, truncate } from "../state/world";
import { PHASE_LABEL, STATUS_LABEL } from "../render/palette";
import { harnessCode, totalTokens } from "../types";
import { formatTokens } from "./Sessions";

interface Props {
  world: World;
  selectedId: string | null;
  onKill(id: string): void;
  onResume(node: AgentNode): void;
  onDelete(id: string): void;
  canWrite: boolean;
}

/**
 * The selected session: what it is doing, what it cost, and the three things
 * you can do to it.
 *
 * ## Stop, resume, delete — and why delete does not stop first
 *
 * They are three verbs because they are three intents. Stop ends a run and
 * keeps the record. Resume opens a new run continuing the same conversation.
 * Delete removes the record, and it is **disabled while the session is live**
 * rather than quietly stopping it first: the server refuses that for a reason
 * — the run's row carries the process group id and is the last thing that can
 * reach the harness — and a button that silently did two dangerous things
 * because one of them was blocked is worse than a button you cannot press.
 *
 * ## Facts, not a form
 *
 * Ten labelled rows became four. Model, permission, event count, rate, fault
 * tally and the pid line were all things you could read here and act on
 * nowhere, and the trajectory shows the same run in more detail whenever the
 * detail is what you want. What is left is what changes a decision: where it is
 * working, which conversation it holds, and what it has spent.
 */
export function Dossier({ world, selectedId, onKill, onResume, onDelete, canWrite }: Props) {
  const node = selectedId ? world.agents.get(selectedId) : undefined;
  const [copied, setCopied] = useState(false);

  const feed = useMemo(
    () => (node ? world.feed.filter((f) => f.agentId === node.summary.id).slice(-60) : []),
    [world.feed, world.revision, node],
  );

  if (!node) {
    return (
      <aside className="panel dossier empty-dossier">
        <h2>DETAIL</h2>
        <p className="empty">Select a session.</p>
      </aside>
    );
  }

  const s = node.summary;
  const live = s.status === "running";
  const now = Date.now();

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(s.watch_command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  };

  return (
    <aside className="panel dossier">
      <h2>
        DETAIL
        <span className={`badge st-${s.status}`}>{STATUS_LABEL[s.status]}</span>
      </h2>

      <div className="dz-name">
        <i className={`hx hx-${s.harness}`}>{harnessCode(s.harness)}</i>
        <span title={s.name}>{s.name}</span>
      </div>

      <div className={`dz-phase ph-${node.phase}`}>
        {node.inFlight ? (
          <>
            <span className="spin" /> {node.inFlight.name}
            <span className="elapsed">{((now - node.inFlight.startedAt) / 1000).toFixed(1)}s</span>
          </>
        ) : (
          PHASE_LABEL[node.phase]
        )}
      </div>

      {/* The three verbs, in the order a person reaches for them. */}
      <div className="dz-actions">
        <button
          disabled={!live || !canWrite}
          title={!canWrite ? "Read-only session" : live ? "Stop this run" : "Not running"}
          onClick={() => onKill(s.id)}
        >
          STOP
        </button>
        <button
          disabled={!s.session_id || !canWrite}
          title={
            !canWrite
              ? "Read-only session"
              : s.session_id
                ? "Start a run continuing this conversation"
                : "No session id yet"
          }
          onClick={() => onResume(node)}
        >
          RESUME
        </button>
        <button
          className="danger"
          disabled={live || !canWrite}
          title={
            !canWrite
              ? "Read-only session"
              : live
                ? "Stop it first — deleting a live run would strand its process group"
                : "Delete this session"
          }
          onClick={() => onDelete(s.id)}
        >
          DELETE
        </button>
      </div>

      <dl className="dz-facts">
        <Fact k="CWD" v={s.cwd} mono wrap />
        <Fact k="SESSION" v={s.session_id ?? "pending"} mono />
        <Fact k="TOKENS" v={formatTokens(totalTokens(s.usage))} />
        <Fact
          k="SPEND"
          v={s.usage.cost_usd != null ? `$${s.usage.cost_usd.toFixed(4)}` : "—"}
        />
        {live && <Fact k="RATE" v={`${eventRate(node, now).toFixed(2)}/s`} />}
      </dl>

      <div className="dz-attach">
        <button onClick={copy} title="Copy `jod watch <id>`">
          {copied ? "COPIED" : "COPY WATCH"}
        </button>
      </div>

      <h3>STREAM</h3>
      <div className="dz-stream">
        {feed.length === 0 && <p className="empty">Nothing yet.</p>}
        {feed.map((f) => (
          <div key={f.id} className={`ev k-${f.kind} ${f.isError ? "bad" : ""}`}>
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
