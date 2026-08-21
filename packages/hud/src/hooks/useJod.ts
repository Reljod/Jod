import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { WorldStore } from "../state/world";
import type { Transport, TransportFactory, WorkDeletion } from "../transport";
import { createTransport, modeFromLocation } from "../transport/factory";
import type { AgentSummary, FleetNode, HarnessInfo, SpawnRequest } from "../types";

/** How often the DOM panels re-render. The canvas is independent, at 60fps. */
const PANEL_HZ = 10;

/**
 * How often the fleet tree is re-queried.
 *
 * The fleet is a *query*, not a stream: it is built from the database, so it
 * reflects works and sessions created by any process, and no event on this
 * HUD's stream announces "a work was created elsewhere". The event stream still
 * drives it — see `useFleet` — this is the floor beneath that, for the changes
 * no envelope describes.
 */
const FLEET_POLL_MS = 4000;

/**
 * How long a lifecycle event waits before it re-queries the tree.
 *
 * A run starting or finishing reshapes the forest, and waiting up to a full
 * poll to redraw it is what made the fleet look asleep while three agents were
 * working. Debounced because a burst — a manager finishing and the four
 * engineers it stopped finishing with it — is one reshape, not five, and the
 * same 400ms the transport already uses to reconcile the roster.
 */
const FLEET_SETTLE_MS = 400;

export interface JodApi {
  store: WorldStore;
  /** Bumped whenever the world changed; panels read through this. */
  revision: number;
  transportLabel: string;
  harnesses: HarnessInfo[];
  /** The fleet tree — works, sessions, runs. Empty until the first query. */
  fleet: FleetNode[];
  /**
   * The live driver, or null until it has been chosen.
   *
   * Exposed for the reads that are not part of the world — a run's history and
   * its opening prompt, both fetched on demand for one selected agent rather
   * than streamed for all of them.
   */
  transport: Transport | null;
  spawn(request: SpawnRequest): Promise<AgentSummary | null>;
  kill(agentId: string): Promise<void>;
  /**
   * Delete runs, and say how it went.
   *
   * Takes a list because that is how the panels ask: a selection is a set, and
   * one refusal in it must not abandon the rest. Every id is attempted, the
   * ones that worked are dropped from the world, and the failures come back
   * with their reasons for the caller to show.
   */
  deleteRuns(agentIds: string[]): Promise<BulkOutcome>;
  /** The same, for sessions. Refused for main and for a session inside a work. */
  deleteConversations(conversationIds: string[]): Promise<BulkOutcome>;
  /**
   * Delete a work, and report a refusal rather than throwing it.
   *
   * Returns `null` when there is no transport. Otherwise the server's answer —
   * check `deleted` before treating it as done. Repeating the call inside the
   * window is what confirms it.
   */
  deleteWork(workId: string): Promise<WorkDeletion | null>;
  /** Re-query the fleet tree now, rather than waiting for the next poll. */
  refreshFleet(): Promise<void>;
  /** Exchange a bearer token for a session cookie. Throws on rejection. */
  authenticate(token: string): Promise<void>;
  /** Last action error, for the console line. */
  lastError: string | null;
  /**
   * Put a message in front of the operator, or clear it with `null`.
   *
   * For a caller that made several calls and knows what the whole of it came
   * to. The bulk deletes deliberately report nothing themselves — see
   * [`summariseFailures`] — because each one clearing the last one's refusal is
   * how a screenful of failures came to display as silence.
   */
  reportError(message: string | null): void;
  clearError(): void;
}

/** What a bulk delete actually managed. */
export interface BulkOutcome {
  deleted: string[];
  /** One entry per id that was refused, carrying the server's own sentence. */
  failed: { id: string; reason: string }[];
}

/**
 * One line for a pile of refusals, or null when there were none.
 *
 * The first reason in full plus a count of the rest. Refusals in a bulk delete
 * are nearly always the same refusal repeated — five live runs give five copies
 * of "stop it before deleting it" — so showing one and counting is both shorter
 * and more accurate than a list. A toast per failure would bury the panel under
 * its own error reporting.
 */
export function summariseFailures(
  failed: readonly { reason: string }[],
): string | null {
  if (failed.length === 0) return null;
  if (failed.length === 1) return failed[0].reason;
  return `${failed[0].reason} (and ${failed.length - 1} more)`;
}

/**
 * Owns the transport and the world.
 *
 * React is not in the hot path on purpose: event bursts arrive faster than a
 * paint, so the store absorbs them and only publishes to the panels ten times a
 * second. The tactical canvas ignores React entirely and reads `store.world`
 * on every frame.
 */
export function useJod(makeTransport?: TransportFactory): JodApi {
  const store = useMemo(() => new WorldStore(), []);
  const transportRef = useRef<Transport | null>(null);
  // Mirrored into state as well as the ref: a ref does not re-render, and a
  // panel that fetches through the transport has to be told when one exists.
  const [transport, setTransport] = useState<Transport | null>(null);
  const [transportLabel, setTransportLabel] = useState("…");
  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [fleet, setFleet] = useState<FleetNode[]>([]);
  const [lastError, setLastError] = useState<string | null>(null);
  // Held in a ref rather than state: it is cleared and re-armed from inside the
  // envelope handler, which must not re-render for every event on the stream.
  const settleRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Pinned on first render. A shell that passes an inline arrow would otherwise
  // change this every paint, and the effect below would tear the connection
  // down and rebuild it each time — reconnecting the SSE stream forever.
  const makeRef = useRef(makeTransport);

  /**
   * Read the fleet tree again.
   *
   * Declared above the effect that polls it, because that effect names it as a
   * dependency and a `const` referenced before its initialiser has run is a
   * temporal-dead-zone throw, not a hoisted function.
   */
  const pullFleet = useCallback(async () => {
    try {
      const nodes = await transportRef.current?.fleet();
      if (nodes) setFleet(nodes);
    } catch {
      /* the tree keeps its last good shape; the next tick retries */
    }
  }, []);

  /**
   * Re-read the tree shortly, because something on the stream reshaped it.
   *
   * Deliberately a nudge to the existing query rather than a second way of
   * knowing the tree. The forest is assembled from works, conversations and
   * runs in the database and changes for reasons this HUD's stream never
   * mentions, so the poll below stays as the floor — see its comment. What an
   * envelope adds is *promptness* for the subset it does describe, which is the
   * subset somebody is usually watching for.
   */
  const settleFleet = useCallback(() => {
    if (settleRef.current) return;
    settleRef.current = setTimeout(() => {
      settleRef.current = null;
      void pullFleet();
    }, FLEET_SETTLE_MS);
  }, [pullFleet]);

  useEffect(() => {
    let disposed = false;
    let active: Transport | null = null;

    void (async () => {
      const factory = makeRef.current;
      const transport = await (factory ? factory() : createTransport(modeFromLocation()));
      if (disposed) return;
      active = transport;
      transportRef.current = transport;
      setTransport(transport);
      setTransportLabel(transport.label);

      transport.start({
        onEnvelope: (env) => {
          store.ingest(env);
          // The three kinds that change the *shape* of the tree rather than
          // what a row says. A `started` is a run appearing under a session
          // that may itself be new; a `finished` retires it. Text events are
          // deliberately not in this list — a manager narrating for four
          // minutes must not re-query the forest on every sentence.
          if (env.kind === "started" || env.kind === "finished" || env.kind === "error") {
            settleFleet();
          }
        },
        onReport: (report) => store.setReport(report),
        onLink: (link) => store.setLink(link),
      });

      try {
        const list = await transport.harnesses();
        if (!disposed) setHarnesses(list);
      } catch {
        /* harness list is chrome, not load-bearing */
      }
    })();

    return () => {
      disposed = true;
      active?.stop();
      transportRef.current = null;
      setTransport(null);
      if (settleRef.current) clearTimeout(settleRef.current);
      settleRef.current = null;
    };
  }, [store, settleFleet]);

  // Publish to the panels on a fixed cadence rather than per event.
  useEffect(() => {
    const timer = setInterval(() => store.flush(), 1000 / PANEL_HZ);
    return () => clearInterval(timer);
  }, [store]);

  // Keep the fleet tree current.
  //
  // Deliberately a poll and not a stream. The forest is assembled from works,
  // conversations and runs in the database, so it changes for reasons this
  // HUD's event stream never mentions — a work created from `jod tui`, a
  // session attached by an agent, a run started by another process entirely.
  // Subscribing to envelopes would keep the tree in step with only the subset
  // of changes that happen to pass through here, which is the same mistake
  // that left the fleet invisible in the first place.
  // The same `pullFleet` a delete calls, so there is one definition of "read
  // the tree again" rather than a poll's copy and an action's copy that can
  // drift apart.
  useEffect(() => {
    void pullFleet();
    const timer = setInterval(() => void pullFleet(), FLEET_POLL_MS);
    return () => clearInterval(timer);
  }, [pullFleet]);

  const revision = useSyncExternalStore(
    useCallback((fn: () => void) => store.subscribe(fn), [store]),
    () => store.world.revision,
    () => store.world.revision,
  );

  const spawn = useCallback(
    async (request: SpawnRequest) => {
      try {
        setLastError(null);
        return (await transportRef.current?.spawn(request)) ?? null;
      } catch (err) {
        setLastError(err instanceof Error ? err.message : String(err));
        return null;
      }
    },
    [],
  );

  const authenticate = useCallback(async (token: string) => {
    const transport = transportRef.current;
    if (!transport) throw new Error("No transport connected");
    await transport.authenticate(token);
    try {
      setHarnesses(await transport.harnesses());
    } catch {
      /* harness list is chrome, not load-bearing */
    }
  }, []);

  const kill = useCallback(async (agentId: string) => {
    try {
      setLastError(null);
      await transportRef.current?.kill(agentId);
    } catch (err) {
      setLastError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  /**
   * The shared body of both bulk deletes.
   *
   * Sequential rather than `Promise.all`, on purpose. These are writes against
   * one SQLite file behind one lock, so firing thirty at once buys nothing and
   * makes the failure list depend on scheduling. Doing them in order also means
   * a partial run is a prefix — the caller can say "deleted 4 of 9" and mean it.
   *
   * **It never touches `lastError`.** It used to, and that was a bug worth
   * naming: a confirmed delete calls this twice, once for runs and once for
   * conversations, and the second call — usually with an empty list — cleared
   * the refusal the first one had just reported. Two live runs were refused and
   * the screen said nothing at all. Reporting belongs to whoever knows about
   * *all* the calls, which is the caller. → [`summariseFailures`]
   */
  const deleteEach = useCallback(
    async (ids: string[], remove: (id: string) => Promise<void>): Promise<BulkOutcome> => {
      const outcome: BulkOutcome = { deleted: [], failed: [] };
      for (const id of ids) {
        try {
          await remove(id);
          outcome.deleted.push(id);
        } catch (err) {
          outcome.failed.push({
            id,
            reason: err instanceof Error ? err.message : String(err),
          });
        }
      }
      return outcome;
    },
    [],
  );

  const deleteRuns = useCallback(
    async (agentIds: string[]) => {
      const transport = transportRef.current;
      if (!transport || agentIds.length === 0) return { deleted: [], failed: [] };
      const outcome = await deleteEach(agentIds, (id) => transport.deleteRun(id));
      // Only what the server actually took. Dropping a refused run locally
      // would hide a live agent from the one panel that lists it.
      for (const id of outcome.deleted) store.forget(id);
      store.flush();
      void pullFleet();
      return outcome;
    },
    [store, deleteEach, pullFleet],
  );

  const deleteConversations = useCallback(
    async (conversationIds: string[]) => {
      const transport = transportRef.current;
      if (!transport || conversationIds.length === 0) return { deleted: [], failed: [] };
      const outcome = await deleteEach(conversationIds, (id) =>
        transport.deleteConversation(id),
      );
      void pullFleet();
      return outcome;
    },
    [deleteEach, pullFleet],
  );

  const deleteWork = useCallback(
    async (workId: string) => {
      const transport = transportRef.current;
      if (!transport) return null;
      try {
        setLastError(null);
        const outcome = await transport.deleteWork(workId);
        if (outcome.deleted) void pullFleet();
        return outcome;
      } catch (err) {
        setLastError(err instanceof Error ? err.message : String(err));
        return null;
      }
    },
    [pullFleet],
  );

  return {
    store,
    revision,
    transport,
    transportLabel,
    harnesses,
    fleet,
    spawn,
    kill,
    deleteRuns,
    deleteConversations,
    deleteWork,
    refreshFleet: pullFleet,
    authenticate,
    lastError,
    reportError: useCallback((message: string | null) => setLastError(message), []),
    clearError: useCallback(() => setLastError(null), []),
  };
}
