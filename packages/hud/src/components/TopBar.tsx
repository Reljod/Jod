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

/**
 * The header: where you are connected, how the fleet stands, and which view.
 *
 * ## Why it lost half its contents
 *
 * It carried the wordmark, the link readout, the transport label, five tallies,
 * one chip per installed harness, three view buttons, a RECENTRE button, a ⌘K
 * button and a clock. Laid out with a fixed gap and nothing allowed to shrink,
 * which had a consequence beyond looking busy: below about 1400px the row could
 * not fit, so it set a floor on the page's width and pushed the right-hand rail
 * off the edge of the window. The fleet panel was being clipped by the header.
 *
 * What went, and where it went instead:
 *
 * - **The harness chips.** Which harnesses are installed matters when you are
 *   choosing one, and the command palette lists them at that moment.
 * - **KILLED.** A tally nobody acts on. The count is still in the sessions
 *   list, where each row shows its own state.
 * - **RECENTRE.** The tactical canvas has had its own fit control (⤢) in the
 *   corner since the zoom buttons landed; this was a second one, further away
 *   from the thing it acts on.
 * - **The transport label.** "HTTP" beside a live link that already names the
 *   origin it is connected to.
 *
 * Everything left is either state you must not miss (are we connected, is this
 * token read-only, is anything running) or navigation. The row is also allowed
 * to shrink now, so a narrow window loses the least important thing rather than
 * the panel on the far side of the page.
 */
export function TopBar({
  world,
  harnesses,
  transportLabel,
  view,
  onView,
  onCommand,
}: Props) {
  const clock = useClock();
  const { report, link } = world;

  const linkText =
    link.phase === "live"
      ? link.origin.replace(/^https?:\/\//, "")
      : link.phase === "simulated"
        ? "SIMULATED"
        : link.phase === "probing"
          ? "PROBING…"
          : link.phase === "auth"
            ? "NO SESSION"
            : `LOST — ${Math.round(link.retryInMs / 100) / 10}s`;

  const missing = harnesses.filter((h) => !h.available);

  return (
    <header className="topbar">
      <div className="brand">
        <Mascot size={16} busy={report.running > 0} />
        <span className="wordmark">JOD</span>
      </div>

      <div
        className={`link link-${link.phase}`}
        title={
          link.phase === "simulated" || link.phase === "lost" || link.phase === "auth"
            ? link.reason
            : `${transportLabel}${missing.length ? ` · missing: ${missing.map((h) => h.label).join(", ")}` : ""}`
        }
      >
        <i className="dot" />
        <span className="origin">{linkText}</span>
        {/* A read-only session must be obvious before somebody reaches for a
            control it cannot use. This is the one badge that stays. */}
        {link.phase === "live" && link.scope === "read" && <span className="scope scope-read">READ</span>}
      </div>

      <div className="tallies">
        <Tally label="ACTIVE" value={report.running} tone="live" />
        <Tally label="DONE" value={report.completed} tone="ok" />
        <Tally label="FAULT" value={report.failed} tone="bad" />
        <Tally label="SPEND" value={`$${report.total_cost_usd.toFixed(2)}`} tone="warn" />
      </div>

      <div className="viewswitch">
        <button className={view === "tactical" ? "on" : ""} onClick={() => onView("tactical")}>
          LIVE
        </button>
        <button className={view === "timeline" ? "on" : ""} onClick={() => onView("timeline")}>
          TIMELINE
        </button>
        <button
          className={view === "trajectory" ? "on" : ""}
          title="Read the selected session end to end"
          onClick={() => onView("trajectory")}
        >
          READ
        </button>
      </div>

      <div className="topbar-actions">
        <button onClick={onCommand} title="Command palette (⌘K)">
          ⌘K
        </button>
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
