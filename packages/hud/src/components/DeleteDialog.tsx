import { useEffect } from "react";

/** What one confirmation is about, counted by kind. */
export interface DeleteRequest {
  runs: string[];
  conversations: string[];
  works: string[];
  /**
   * What the server said the last time this was attempted.
   *
   * Present only after a work delete came back refused. The refusal carries the
   * counts and the worktrees at stake, and repeating the request inside its
   * window is what confirms it — so the same dialog is shown again with the
   * server's own sentence in it, and CONFIRM sends the same call.
   */
  notice: string | null;
  /** True once the first attempt has run, so the button says what it does. */
  armed: boolean;
}

interface Props {
  request: DeleteRequest | null;
  busy: boolean;
  onCancel(): void;
  onConfirm(): void;
}

/**
 * The one confirmation in the HUD, and the only thing that stands between a
 * selection and a delete.
 *
 * It states what will go rather than asking "are you sure": a count of each
 * kind, and the fact that a work takes every session inside it. That last part
 * is the one people do not expect, so it is on screen rather than in a doc.
 *
 * When the server refuses — which it does the first time a work holds git
 * worktrees — its sentence is shown verbatim. It already names what is dirty
 * and what is unmerged, and paraphrasing it here would be a second copy of a
 * warning that has to stay exact.
 */
export function DeleteDialog({ request, busy, onCancel, onConfirm }: Props) {
  useEffect(() => {
    if (!request) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, onCancel]);

  if (!request) return null;

  const parts = [
    describe(request.runs.length, "session"),
    describe(request.conversations.length, "conversation"),
    describe(request.works.length, "work", "works"),
  ].filter(Boolean);

  return (
    <div className="palette-backdrop" onClick={onCancel}>
      <div className="confirm" onClick={(e) => e.stopPropagation()}>
        <h2>DELETE</h2>
        <p className="what">{parts.length > 0 ? parts.join(" · ") : "Nothing selected"}</p>

        {request.works.length > 0 && (
          <p className="note">Deleting a work takes every session inside it.</p>
        )}

        {request.notice && <p className="notice">{request.notice}</p>}

        <div className="confirm-actions">
          <button onClick={onCancel} disabled={busy}>
            CANCEL
          </button>
          <button className="danger" onClick={onConfirm} disabled={busy}>
            {busy ? "WORKING…" : request.armed ? "CONFIRM" : "DELETE"}
          </button>
        </div>
      </div>
    </div>
  );
}

function describe(n: number, singular: string, plural = `${singular}s`): string {
  if (n === 0) return "";
  return `${n} ${n === 1 ? singular : plural}`;
}
