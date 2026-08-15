import { useEffect, useState } from "react";
import type { WorldStore } from "../state/world";
import { openingPrompt } from "../state/prompt";
import type { Transport } from "../transport";
import { EVENT_PAGE } from "../transport/http";

/**
 * How many pages of history one selection will fetch.
 *
 * Bounded because the cursor only walks forward: reaching the end of a very
 * long run means paging through all of it. The store keeps the newest events
 * within its own cap regardless, so this bounds the *requests*, not what is
 * displayed — and when it bites, the count of skipped events is shown rather
 * than swallowed.
 */
const MAX_PAGES = 60;

export interface TrajectoryFeed {
  /** The run's opening prompt, once recovered. Null when there is none to have. */
  prompt: string | null;
  loading: boolean;
  /** Why the history could not be fetched. The view says so rather than looking empty. */
  error: string | null;
}

/**
 * Make sure the selected run's whole history is in the store, and find its ask.
 *
 * Deliberately not a poll. Every live envelope already reaches the store
 * through the transport's stream, so tailing is free; the only thing missing is
 * what happened *before this page loaded*, which is a one-shot fetch per run.
 * A run this client watched from its first event is already complete and costs
 * no request at all.
 */
export function useTrajectory(
  store: WorldStore,
  transport: Transport | null,
  agentId: string | null,
): TrajectoryFeed {
  const [prompt, setPrompt] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The session id is what the prompt lookup joins on, and it arrives with
  // `started` rather than with the roster — so this effect has to re-run when
  // it lands, not only when the selection changes.
  const sessionId = agentId ? (store.world.agents.get(agentId)?.summary.session_id ?? null) : null;

  useEffect(() => {
    setPrompt(null);
    setError(null);
    if (!agentId || !transport) return;

    let disposed = false;

    void (async () => {
      const node = store.world.agents.get(agentId);
      if (!node) return;

      if (!node.eventsComplete) {
        setLoading(true);
        try {
          let cursor: number | undefined;
          for (let page = 0; page < MAX_PAGES; page++) {
            const batch = await transport.events(agentId, cursor);
            if (disposed) return;
            store.backfill(agentId, batch);
            if (batch.length < EVENT_PAGE) break;
            cursor = batch[batch.length - 1].seq;
          }
          store.flush();
        } catch (err) {
          if (!disposed) setError(err instanceof Error ? err.message : String(err));
        } finally {
          if (!disposed) setLoading(false);
        }
      }

      try {
        const opening = await openingPrompt(transport, { id: agentId, session_id: sessionId });
        if (!disposed) setPrompt(opening);
      } catch {
        // The transcript store is optional — a daemon can run without one, and
        // an older API has no conversation routes at all. The trajectory is
        // complete without the prompt, so this failure is not the view's
        // problem to report.
      }
    })();

    return () => {
      disposed = true;
    };
  }, [store, transport, agentId, sessionId]);

  return { prompt, loading, error };
}
