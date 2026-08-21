import { useEffect, useMemo, useState } from "react";
import {
  BAND_LANES,
  buildTrajectory,
  byTurn,
  filterRows,
  type BandScale,
  type TrajectoryRow,
} from "../graph/trajectory";
import { useTrajectory } from "../hooks/useTrajectory";
import type { WorldStore } from "../state/world";
import type { Transport } from "../transport";
import { harnessCode } from "../types";

const SCALES: { id: BandScale; label: string; hint: string }[] = [
  { id: "duration", label: "DURATION", hint: "Blocks sized by wall-clock — where the time went" },
  { id: "turns", label: "TURNS", hint: "Every model turn the same width — the shape of the loop" },
  { id: "calls", label: "CALLS", hint: "Every step the same width — structure, not cost" },
];

const LANE_LABEL: Record<(typeof BAND_LANES)[number], string> = {
  input: "Input",
  model: "Model",
  tools: "Tools",
};

interface Props {
  store: WorldStore;
  transport: Transport | null;
  selectedId: string | null;
}

/**
 * One session, read end to end: setup, ask, every turn, every call.
 *
 * The third view, and the one that answers the question the other two cannot.
 * The graph shows the fleet's shape now and the swimlanes show where the
 * wall-clock went across agents; neither can tell you what a *particular* run
 * was asked to do or what it said back. That is what someone opening a finished
 * run actually wants, and until now it lived only in `jod watch`.
 */
export function TrajectoryView({ store, transport, selectedId }: Props) {
  const [scale, setScale] = useState<BandScale>("duration");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [frameTick, setFrameTick] = useState(0);

  const feed = useTrajectory(store, transport, selectedId);
  const node = selectedId ? store.world.agents.get(selectedId) : undefined;
  const live = node?.summary.status === "running";

  // A live run's last block runs to *now*, which keeps moving whether or not an
  // event arrives. Finished runs are static and cost nothing here.
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setFrameTick((n) => n + 1), 500);
    return () => clearInterval(t);
  }, [live]);

  // Collapse everything when the selection moves: an expanded row id from the
  // previous run would otherwise re-open an unrelated row with the same seq.
  useEffect(() => setExpanded(new Set()), [selectedId]);

  // `revision` and `frameTick` are the two things that move underneath this:
  // the node is mutated in place as events land, so depending on the node alone
  // would memoise a transcript that never grows, and a live run's last block
  // runs to `now` whether or not anything arrived.
  const revision = store.world.revision;
  const trajectory = useMemo(
    () => (node ? buildTrajectory(node, { now: Date.now(), prompt: feed.prompt, scale }) : null),
    [node, revision, frameTick, feed.prompt, scale],
  );

  if (!node || !trajectory) {
    return (
      <div className="trajectory">
        <div className="tj-controls">
          <span className="tj-title">TRAJECTORY</span>
        </div>
        <p className="empty">Select a session to read it.</p>
      </div>
    );
  }

  const s = node.summary;
  const rows = filterRows(trajectory.rows, query);
  const groups = byTurn(rows);

  return (
    <div className="trajectory">
      <div className="tj-controls">
        <span className="tj-title">TRAJECTORY</span>
        <i className={`hx hx-${s.harness}`}>{harnessCode(s.harness)}</i>
        <span className="tj-name">{s.name}</span>
        <span className={`tj-stat st-${s.status}`}>{s.status.toUpperCase()}</span>

        <span className="tj-metrics">
          <b>{trajectory.turns}</b>t · <b>{trajectory.toolCalls}</b>c ·{" "}
          <b>{formatDuration(trajectory.durationMs)}</b>
        </span>

        <span className="tj-scales">
          {SCALES.map((sc) => (
            <button
              key={sc.id}
              className={scale === sc.id ? "on" : ""}
              title={sc.hint}
              onClick={() => setScale(sc.id)}
            >
              {sc.label}
            </button>
          ))}
        </span>

        <input
          className="tj-search"
          placeholder="Search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="Search this session"
        />
      </div>

      <Band trajectory={trajectory} />

      {/* Only rendered when it has something to say. An always-present strip of
          status text is four lines of chrome above a transcript that is the
          reason anybody opened this view. Truncation still gets a line — a
          transcript that begins at turn six reads as a run that began at turn
          six, and that must never be silent. */}
      {(feed.loading || feed.error || trajectory.dropped > 0 || query !== "") && (
        <div className="tj-notes">
          {feed.loading && <span className="tj-note">loading…</span>}
          {feed.error && <span className="tj-note bad">history unavailable — {feed.error}</span>}
          {trajectory.dropped > 0 && (
            <span className="tj-note warn">
              {trajectory.dropped} earlier event{trajectory.dropped === 1 ? "" : "s"} not retained
            </span>
          )}
          {query !== "" && (
            <span className="tj-note">
              {rows.length}/{trajectory.rows.length}
            </span>
          )}
        </div>
      )}

      <div className="tj-rows">
        {groups.map((group) => (
          <div key={`t${group.turn}-${group.rows[0].id}`} className="tj-turn">
            {group.turn > 0 && <div className="tj-turnmark">Turn {group.turn}</div>}
            {group.rows.map((row) => (
              <Row
                key={row.id}
                row={row}
                open={expanded.has(row.id)}
                onToggle={() =>
                  setExpanded((prev) => {
                    const next = new Set(prev);
                    if (next.has(row.id)) next.delete(row.id);
                    else next.add(row.id);
                    return next;
                  })
                }
              />
            ))}
          </div>
        ))}
        {rows.length === 0 && <p className="empty">Nothing in this session matches “{query}”.</p>}
      </div>
    </div>
  );
}

/**
 * Three lanes over the run: what was put in, what the model produced, what the
 * tools took. Reading them together is the point — a turn that is all orange is
 * a run waiting on its tools, and one that is all violet is a run thinking.
 *
 * Plain elements on a percentage track rather than an SVG, because the band has
 * to reflow with the panel and SVG geometry attributes take no `calc()`. The
 * timeline view measures its host to draw in user units; this one has no ticks
 * to place, so it can let the browser do the arithmetic.
 */
function Band({ trajectory }: { trajectory: NonNullable<ReturnType<typeof buildTrajectory>> }) {
  return (
    <div className="tj-band" role="img" aria-label="Where this session's time went">
      {BAND_LANES.map((lane) => (
        <div className="tj-bandlane" key={lane}>
          <span className="tj-lanelabel">{LANE_LABEL[lane]}</span>
          <div className="tj-track">
            {trajectory.band
              .filter((seg) => seg.lane === lane)
              .map((seg, i) => (
                <i
                  key={i}
                  className={`tj-block tj-lane-${seg.lane} ${seg.isError ? "bad" : ""}`}
                  style={{
                    left: `${seg.from * 100}%`,
                    // A block with no measurable width is still a thing that
                    // happened, so it is floored to a visible sliver.
                    width: `max(2px, ${Math.max(0, seg.to - seg.from) * 100}%)`,
                  }}
                  title={`turn ${seg.turn} · ${seg.label}`}
                />
              ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function Row({
  row,
  open,
  onToggle,
}: {
  row: TrajectoryRow;
  open: boolean;
  onToggle(): void;
}) {
  const expandable = row.detail !== null && row.detail !== row.summary;

  return (
    <div className={`tj-row tj-k-${row.kind} ${row.isError ? "bad" : ""} ${open ? "open" : ""}`}>
      <button
        className="tj-head"
        onClick={onToggle}
        disabled={!expandable}
        title={expandable ? "Expand" : undefined}
      >
        <span className={`tj-badge tj-b-${row.kind}`}>{row.badge}</span>
        <span className="tj-summary">{row.summary}</span>
        <span className="tj-meta">
          {row.open && <i className="tj-live" title="still in flight" />}
          {row.durationMs != null && row.durationMs > 0 && formatDuration(row.durationMs)}
        </span>
      </button>
      {open && row.detail && <pre className="tj-detail">{row.detail}</pre>}
    </div>
  );
}

/** Short enough for a dense row, precise enough to compare two calls. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return `${minutes}m${String(seconds).padStart(2, "0")}s`;
}
