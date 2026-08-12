import { useEffect, useRef, useState } from "react";
import { buildLanes, ticks, windowFor } from "../graph/timeline";
import { rankForDisplay } from "../graph/model";
import type { WorldStore } from "../state/world";
import { truncate } from "../state/world";
import { harnessCode } from "../types";

const SPANS = [
  { label: "30s", ms: 30_000 },
  { label: "2m", ms: 120_000 },
  { label: "10m", ms: 600_000 },
];

const LANE_H = 30;
const LABEL_W = 168;
const LANE_BUDGET = 18;

interface Props {
  store: WorldStore;
  selectedId: string | null;
  onSelect(id: string): void;
}

/**
 * Swimlanes: one row per agent, time running left to right, now at the right
 * edge.
 *
 * Complements the graph rather than duplicating it. The graph shows the fleet's
 * current shape; this shows the shape of the work — which tool calls actually
 * cost the wall-clock, where an agent sat blocked, and how a fault propagated
 * through a run. A long unbroken bar is the thing to look for.
 */
export function TimelineView({ store, selectedId, onSelect }: Props) {
  // 30s by default: individual tool calls run 0.5–3s, and at a wider window
  // they compress into indistinguishable specks. Wider spans stay a click away
  // for looking at the shape of a whole run.
  const [spanMs, setSpanMs] = useState(30_000);
  const [, setFrameTick] = useState(0);
  const hostRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(900);

  // Re-render on a timer rather than per event: the window scrolls continuously
  // whether or not anything arrives.
  useEffect(() => {
    const t = setInterval(() => setFrameTick((n) => n + 1), 200);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  const now = Date.now();
  const w = windowFor(now, spanMs);
  const { visible, hidden } = rankForDisplay(store.world, LANE_BUDGET);
  const lanes = buildLanes(store.world, visible, w);
  const plotW = Math.max(120, width - LABEL_W - 16);
  const height = Math.max(60, lanes.length * LANE_H + 26);
  const x = (f: number) => LABEL_W + f * plotW;

  return (
    <div className="timeline" ref={hostRef}>
      <div className="tl-controls">
        <span className="tl-title">TIMELINE</span>
        {SPANS.map((s) => (
          <button
            key={s.ms}
            className={spanMs === s.ms ? "on" : ""}
            onClick={() => setSpanMs(s.ms)}
          >
            {s.label}
          </button>
        ))}
        {hidden > 0 && <span className="tl-hidden">+{hidden} not shown</span>}
      </div>

      <div className="tl-scroll">
        <svg width={width} height={height} role="img" aria-label="Agent activity timeline">
          {/* gridlines */}
          {ticks(w).map((t, i) => (
            <g key={i}>
              <line x1={x(t.at)} y1={18} x2={x(t.at)} y2={height} className="tl-grid" />
              <text x={x(t.at)} y={12} className="tl-tick" textAnchor="middle">
                {t.label}
              </text>
            </g>
          ))}

          {lanes.map((lane, i) => {
            const y = 22 + i * LANE_H;
            const sel = lane.id === selectedId;
            return (
              <g
                key={lane.id}
                className={`tl-lane hx-${lane.harness} st-${lane.status} ${sel ? "sel" : ""}`}
                onClick={() => onSelect(lane.id)}
              >
                <rect x={0} y={y} width={width} height={LANE_H - 2} className="tl-lanebg" />
                <text x={8} y={y + 14} className="tl-code">
                  {harnessCode(lane.harness as never)}
                </text>
                <text x={46} y={y + 14} className="tl-name">
                  {truncate(lane.name, 15)}
                </text>

                {/* tool spans */}
                {lane.spans.map((s, j) => {
                  const x0 = x(s.from);
                  // A sub-second call still has to be clickable and visible.
                  const x1 = Math.max(x0 + 4, x(s.to));
                  return (
                    <g key={j}>
                      <rect
                        x={x0}
                        y={y + 5}
                        width={x1 - x0}
                        height={12}
                        rx={1.5}
                        className={`tl-span ${s.isError ? "bad" : ""} ${s.open ? "open" : ""}`}
                      >
                        <title>
                          {s.name} — {s.open ? "in flight" : `${((s.to - s.from) * w.spanMs / 1000).toFixed(1)}s`}
                        </title>
                      </rect>
                      {x1 - x0 > 34 && (
                        <text x={x0 + 4} y={y + 14} className="tl-spanlabel">
                          {truncate(s.name, Math.floor((x1 - x0) / 6))}
                        </text>
                      )}
                    </g>
                  );
                })}

                {/* event marks */}
                {lane.marks.map((m, j) => (
                  <g key={`m${j}`} transform={`translate(${x(m.at)}, ${y + 23})`}>
                    <circle r={2.6} className={`tl-mark tl-${m.kind}`}>
                      <title>{truncate(m.text, 140)}</title>
                    </circle>
                  </g>
                ))}
              </g>
            );
          })}

          {/* now line */}
          <line x1={x(1)} y1={16} x2={x(1)} y2={height} className="tl-now" />
        </svg>
        {lanes.length === 0 && <p className="empty">No agents yet.</p>}
      </div>
    </div>
  );
}
