import type { Completion } from "../commands";

export interface CompletionsProps {
  items: Completion[];
  onAccept(line: string): void;
}

/**
 * The slash-command completion list, above the composer.
 *
 * The TUI floats this over the transcript and drives it with `Tab` and the
 * arrow keys. A phone has neither, and the mechanism that replaces them is not
 * a keyboard emulation — it is the obvious one: **tap the row you want.** There
 * is therefore no highlighted entry to track, which is the whole of `suggestion`
 * / `next_suggestion` / `prev_suggestion` in `app.rs` gone, and nothing lost:
 * those existed to move a highlight the finger goes straight to.
 *
 * What is kept exactly is the part with the judgement in it — *which* lines are
 * offered, in what order, with what hint. That is `commands.completions`, a
 * pure function tested against the same cases as the Rust one.
 *
 * Rendered above the composer rather than over the transcript because the
 * bottom of an iPhone screen is where the thumb already is, and because the
 * on-screen keyboard would cover a popup drawn anywhere else.
 */
export function Completions({ items, onAccept }: CompletionsProps) {
  if (items.length === 0) return null;
  return (
    <div className="completions" role="listbox" aria-label="Commands">
      {items.map((item) => (
        <button
          key={item.line}
          className="completion"
          role="option"
          aria-selected={false}
          // `onMouseDown` rather than `onClick`: a tap here would otherwise
          // blur the composer first, which on iOS dismisses the keyboard and
          // makes accepting a completion cost two taps instead of one.
          onMouseDown={(e) => {
            e.preventDefault();
            onAccept(item.line);
          }}
        >
          <span className="cmd">{item.line.trimEnd()}</span>
          <span className="hint">{item.hint}</span>
        </button>
      ))}
    </div>
  );
}
