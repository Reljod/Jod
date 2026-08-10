import { useEffect, useRef } from "react";

import type { Entry } from "../session";

/** The glyph column, mirroring the prefixes `cli/src/tui/ui.rs` draws. */
function glyph(entry: Entry): string {
  switch (entry.kind) {
    case "you":
      return "›";
    case "agent":
      return "";
    case "thinking":
      return "·";
    case "tool":
      return entry.failed ? "✗" : "⚙";
    // The TUI indents tool output under its call with `└`, so output reads as
    // belonging to the tool above it rather than as the agent speaking.
    case "tool_out":
      return "└";
    case "done":
      return entry.failed ? "✗" : "✓";
    case "notice":
      return "!";
    case "raw":
      return "│";
  }
}

function text(entry: Entry): string {
  switch (entry.kind) {
    // `Bash · cargo test`, not a bare `Bash` — the argument is most of what
    // makes watching a harness work worth doing.
    case "tool":
      return entry.detail ? `${entry.name} · ${entry.detail}` : entry.name;
    case "tool_out":
      return entry.text;
    case "done":
      return entry.text === ""
        ? entry.failed
          ? "run failed"
          : "done"
        : `${entry.failed ? "failed" : "done"} · ${entry.text}`;
    default:
      return entry.text;
  }
}

function className(entry: Entry): string {
  const failed =
    (entry.kind === "tool" || entry.kind === "tool_out" || entry.kind === "done") &&
    entry.failed;
  return `entry ${entry.kind}${failed ? " failed" : ""}`;
}

export interface TranscriptProps {
  entries: Entry[];
  /** True while the view is pinned to the bottom. */
  following: boolean;
  onFollowingChange(following: boolean): void;
}

/**
 * The scrolling transcript.
 *
 * The one behaviour worth protecting is the TUI's: **new output must not yank
 * the view back down**. Reading something while an agent keeps talking has to
 * work, so the scroll position is only chased while the reader is already at
 * the bottom, and a jump button appears when they are not.
 */
export function Transcript({ entries, following, onFollowingChange }: TranscriptProps) {
  const ref = useRef<HTMLDivElement>(null);

  // Chase the bottom only while following. Depending on `entries` rather than
  // its length also covers an entry being replaced in place.
  useEffect(() => {
    if (!following) return;
    const el = ref.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [entries, following]);

  function onScroll() {
    const el = ref.current;
    if (!el) return;
    // A few pixels of slack: momentum scrolling on iOS routinely stops one or
    // two pixels short, and an exact comparison would drop out of follow mode
    // every time the user flicked down.
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    onFollowingChange(atBottom);
  }

  return (
    <div className="scrollwrap">
      <div className="transcript" ref={ref} onScroll={onScroll}>
        {entries.length === 0 ? (
          <div className="placeholder">
            <strong>Jod delegates. It does not do the work.</strong>
            Ask for something. It runs on the box, in its own process, and
            streams back here. <code>/help</code> lists the commands.
          </div>
        ) : (
          entries.map((entry, i) => (
            <div className={className(entry)} key={i}>
              <span className="glyph">{glyph(entry)}</span>
              <span>{text(entry)}</span>
            </div>
          ))
        )}
      </div>

      {following ? null : (
        <button
          className="jump"
          onClick={() => {
            onFollowingChange(true);
            const el = ref.current;
            if (el) el.scrollTop = el.scrollHeight;
          }}
        >
          ↓ LATEST
        </button>
      )}
    </div>
  );
}
