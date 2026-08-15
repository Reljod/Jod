import { useEffect, useState } from "react";
import type { World } from "../state/world";
import type { HarnessInfo } from "../types";
import { Mascot } from "./Mascot";

export type ViewMode = "tactical" | "timeline" | "trajectory";

interface Props {
  world: World;
  harnesses: HarnessInfo[];
  transportLabel: string;
  view: ViewMode;
  onView(v: ViewMode): void;
  onRecentre(): void;
  onCommand(): void;
}

/** Zulu clock — an ops surface should not make you think about time zones. */
function useClock(): string {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);
  return now.toISOString().slice(11, 19);
}

export function TopBar({
  world,
  harnesses,
  transportLabel,
  view,
  onView,
  onRecentre,
  onCommand,
}: Props) {
  const clock = useClock();
  const { report, link } = world;

  const linkText =
    link.phase === "live"
      ? `LINK ${link.origin.replace(/^https?:\/\//, "")}`
      : link.phase === "simulated"
        ? "SIMULATED FEED"
        : link.phase === "probing"
          ? "PROBING…"
          : link.phase === "auth"
            ? "NO SESSION"
            : `LINK LOST — RETRY ${Math.round(link.retryInMs / 100) / 10}s`;

  return (
    <header className="topbar">
      <div className="brand">
        <Mascot size={16} busy={report.running > 0} />
        <span className="wordmark">JOD</span>
        <span className="sub">TACTICAL</span>
      </div>

      <div className={`link link-${link.phase}`} title={
        link.phase === "simulated" ? link.reason
        : link.phase === "lost" ? link.reason
        : link.phase === "auth" ? link.reason
        : undefined
      }>
        <i className="dot" />
        {linkText}
        {/* A read-only session must be obvious before someone fills in a form. */}
        {link.phase === "live" && (
          <span className={`scope scope-${link.scope}`}>{link.scope.toUpperCase()}</span>
        )}
        <span className="via">{transportLabel}</span>
      </div>

      <div className="tallies">
        <Tally label="ACTIVE" value={report.running} tone="live" />
        <Tally label="DONE" value={report.completed} tone="ok" />
        <Tally label="FAULT" value={report.failed} tone="bad" />
        <Tally label="KILLED" value={report.killed} tone="mute" />
        <Tally
          label="SPEND"
          value={`$${report.total_cost_usd.toFixed(2)}`}
          tone="warn"
        />
      </div>

      <div className="harnesses">
        {harnesses.map((h) => (
          <span key={h.id} className={h.available ? "hz on" : "hz off"} title={h.path ?? "not found"}>
            {h.label}
          </span>
        ))}
      </div>

      <div className="viewswitch">
        <button className={view === "tactical" ? "on" : ""} onClick={() => onView("tactical")}>
          TACTICAL
        </button>
        <button className={view === "timeline" ? "on" : ""} onClick={() => onView("timeline")}>
          TIMELINE
        </button>
        <button
          className={view === "trajectory" ? "on" : ""}
          title="Read the selected session end to end"
          onClick={() => onView("trajectory")}
        >
          TRAJECTORY
        </button>
      </div>

      <div className="topbar-actions">
        {view === "tactical" && (
          <button onClick={onRecentre} title="Reset the camera">RECENTRE</button>
        )}
        <button onClick={onCommand} title="Command palette (⌘K)">⌘K</button>
        <span className="clock">{clock}Z</span>
      </div>
    </header>
  );
}

function Tally({
  label,
  value,
  tone,
}: {
  label: string;
  value: number | string;
  tone: string;
}) {
  return (
    <div className={`tally tone-${tone}`}>
      <span className="tally-value">{value}</span>
      <span className="tally-label">{label}</span>
    </div>
  );
}
