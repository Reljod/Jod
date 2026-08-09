export interface StatusBarProps {
  /** Already formatted by `statusLine` — the same string the TUI shows. */
  text: string;
  busy: boolean;
  /** Shown when the link is anything other than live. */
  note: string | null;
}

/**
 * The one-line status bar, carrying exactly what the TUI's does: which harness,
 * which model, what it has cost, and whether it is working or ready.
 *
 * `busy` is coloured rather than animated. A spinner on a status bar is motion
 * that never means anything different; the transcript is where progress shows.
 */
export function StatusBar({ text, busy, note }: StatusBarProps) {
  return (
    <div className="status">
      <span className={busy ? "working" : "ready"}>{busy ? "●" : "○"}</span>
      <span>{text}</span>
      {note ? (
        <>
          <span style={{ flex: 1 }} />
          <span className="working">{note}</span>
        </>
      ) : null}
    </div>
  );
}
