import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { WorldStore } from "../state/world";
import type { Transport } from "../transport";
import { createTransport, modeFromLocation } from "../transport/factory";
import type { AgentSummary, HarnessInfo, SpawnRequest } from "../types";

/** How often the DOM panels re-render. The canvas is independent, at 60fps. */
const PANEL_HZ = 10;

export interface JodApi {
  store: WorldStore;
  /** Bumped whenever the world changed; panels read through this. */
  revision: number;
  transportLabel: string;
  harnesses: HarnessInfo[];
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
export function useJod(): JodApi {
  const store = useMemo(() => new WorldStore(), []);
  const transportRef = useRef<Transport | null>(null);
  const [transportLabel, setTransportLabel] = useState("…");
  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [lastError, setLastError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let active: Transport | null = null;

    void (async () => {
      const transport = await createTransport(modeFromLocation());
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
    spawn,
    kill,
    authenticate,
    lastError,
    clearError: useCallback(() => setLastError(null), []),
  };
}
