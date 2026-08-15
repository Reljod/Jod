import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { WorldStore } from "../state/world";
import type { Transport, TransportFactory } from "../transport";
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

export interface JodApi {
  store: WorldStore;
  /** Bumped whenever the world changed; panels read through this. */
  revision: number;
  transportLabel: string;
  harnesses: HarnessInfo[];
  /** The fleet tree — works, sessions, runs. Empty until the first query. */
  fleet: FleetNode[];
  spawn(request: SpawnRequest): Promise<AgentSummary | null>;
  kill(agentId: string): Promise<void>;
  /** Exchange a bearer token for a session cookie. Throws on rejection. */
  authenticate(token: string): Promise<void>;
  /** Last action error, for the console line. */
  lastError: string | null;
  clearError(): void;
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
  const [transportLabel, setTransportLabel] = useState("…");
  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [fleet, setFleet] = useState<FleetNode[]>([]);
  const [lastError, setLastError] = useState<string | null>(null);

  // Pinned on first render. A shell that passes an inline arrow would otherwise
  // change this every paint, and the effect below would tear the connection
  // down and rebuild it each time — reconnecting the SSE stream forever.
  const makeRef = useRef(makeTransport);

  useEffect(() => {
    let disposed = false;
    let active: Transport | null = null;

    void (async () => {
      const factory = makeRef.current;
      const transport = await (factory ? factory() : createTransport(modeFromLocation()));
      if (disposed) return;
      active = transport;
      transportRef.current = transport;
      setTransportLabel(transport.label);

      transport.start({
        onEnvelope: (env) => store.ingest(env),
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
    };
  }, [store]);

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
  useEffect(() => {
    let disposed = false;

    const pull = async () => {
      try {
        const nodes = await transportRef.current?.fleet();
        if (!disposed && nodes) setFleet(nodes);
      } catch {
        /* the tree keeps its last good shape; the next tick retries */
      }
    };

    void pull();
    const timer = setInterval(() => void pull(), FLEET_POLL_MS);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, []);

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

  return {
    store,
    revision,
    transportLabel,
    harnesses,
    fleet,
    spawn,
    kill,
    authenticate,
    lastError,
    clearError: useCallback(() => setLastError(null), []),
  };
}
