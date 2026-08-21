import type { Selection } from "../hooks/useSelection";

interface Props {
  selection: Selection;
  canWrite: boolean;
  /** Singular. "3 sessions" and "1 session" are both built from this. */
  noun: string;
  onDelete(): void;
}

/**
 * The bar that appears at the foot of a panel once something is selected.
 *
 * Absent until it has something to say. A permanently visible toolbar with a
 * greyed-out DELETE is a row of chrome on every panel for the one moment in a
 * session when it is useful; this costs nothing until you select something and
 * then says exactly what will happen and to how many.
 *
 * SELECT ALL is here rather than in the header for the same reason: it is a
 * gesture that only makes sense once you are already selecting.
 */
export function SelectionBar({ selection, canWrite, noun, onDelete }: Props) {
  if (selection.size === 0) return null;

  return (
    <div className="selbar">
      <span className="selcount">
        {selection.size} {noun}
        {selection.size === 1 ? "" : "s"}
      </span>
      <button onClick={selection.toggleAll}>{selection.all ? "NONE" : "ALL"}</button>
      <button onClick={selection.clear}>CLEAR</button>
      <button
        className="danger"
        disabled={!canWrite}
        title={canWrite ? `Delete ${selection.size} ${noun}(s)` : "Read-only session"}
        onClick={onDelete}
      >
        DELETE
      </button>
    </div>
  );
}
