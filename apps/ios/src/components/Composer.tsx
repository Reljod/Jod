import { useLayoutEffect, useRef } from "react";

export interface ComposerProps {
  value: string;
  disabled: boolean;
  busy: boolean;
  onChange(value: string): void;
  onSend(): void;
}

/**
 * The input box.
 *
 * A textarea rather than an input because a delegation is usually a paragraph,
 * not a search term, and it grows with the text up to a third of the screen.
 *
 * **Enter inserts a newline; it does not send.** That is the opposite of the
 * TUI, and deliberately: on a phone the return key is under the thumb and a
 * mis-sent half-written prompt starts a real process on the box. Sending is an
 * explicit tap.
 */
export function Composer({ value, disabled, busy, onChange, onSend }: ComposerProps) {
  const ref = useRef<HTMLTextAreaElement>(null);

  // Grow to fit. Reset to `auto` first or the box can only ever get taller.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [value]);

  const sendable = !disabled && !busy && value.trim() !== "";

  return (
    <div className="composer">
      <textarea
        ref={ref}
        rows={1}
        value={value}
        disabled={disabled}
        placeholder={busy ? "working…" : "Delegate something"}
        onChange={(e) => onChange(e.target.value)}
        // iOS keyboard hints: sentence case, and no autocorrect mangling of
        // paths and flags, which is most of what gets typed here.
        autoCapitalize="sentences"
        autoCorrect="off"
        spellCheck={false}
        enterKeyHint="enter"
      />
      <button className="send" onClick={onSend} disabled={!sendable}>
        SEND
      </button>
    </div>
  );
}
