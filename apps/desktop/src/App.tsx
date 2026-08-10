import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, onAgentEvent } from "./api";
import { AgentList } from "./components/AgentList";
import { EventStream } from "./components/EventStream";
import { SpawnForm } from "./components/SpawnForm";
import type { AgentEnvelope, AgentSummary, SpawnArgs, SystemStatus } from "./types";

/** Coalesce the summary refetch — a chatty agent must not cause a request per line. */
const REFRESH_DEBOUNCE_MS = 300;

export default function App() {
  const [status, setStatus] = useState<SystemStatus | null>(null);
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [eventsByAgent, setEventsByAgent] = useState<Record<string, AgentEnvelope[]>>({});
  const [error, setError] = useState<string | null>(null);

  const refreshTimer = useRef<number | null>(null);

  const refreshAgents = useCallback(async () => {
    try {
      setAgents(await api.listAgents());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current !== null) return;
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      void refreshAgents();
    }, REFRESH_DEBOUNCE_MS);
  }, [refreshAgents]);

  useEffect(() => {
    api.systemStatus().then(setStatus).catch((e) => setError(String(e)));
    void refreshAgents();
  }, [refreshAgents]);

  useEffect(() => {
    const unlisten = onAgentEvent((envelope) => {
      setEventsByAgent((prev) => {
        const existing = prev[envelope.agent_id] ?? [];
        // Backfill and the live feed can overlap; `seq` is the source of truth.
        if (existing.some((e) => e.seq === envelope.seq)) return prev;
        return { ...prev, [envelope.agent_id]: [...existing, envelope] };
      });
      scheduleRefresh();
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [scheduleRefresh]);

  const selectAgent = useCallback(async (id: string) => {
    setSelectedId(id);
    try {
      // Backfill history, in case this agent ran before the view existed.
      const history = await api.agentEvents(id);
      setEventsByAgent((prev) => {
        const live = prev[id] ?? [];
        const merged = new Map(history.map((e) => [e.seq, e]));
        for (const e of live) merged.set(e.seq, e);
        return { ...prev, [id]: [...merged.values()].sort((a, b) => a.seq - b.seq) };
      });
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const spawn = useCallback(
    async (args: SpawnArgs) => {
      setError(null);
      try {
        const agent = await api.spawnAgent(args);
        await refreshAgents();
        void selectAgent(agent.id);
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshAgents, selectAgent],
  );

  const kill = useCallback(
    async (id: string) => {
      try {
        await api.killAgent(id);
        await refreshAgents();
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshAgents],
  );

  const openTerminal = useCallback(async (id: string) => {
    try {
      await api.openInTerminal(id);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const selected = useMemo(
    () => agents.find((a) => a.id === selectedId) ?? null,
    [agents, selectedId],
  );

  const blockers = useMemo(() => {
    if (!status) return [];
    const out: string[] = [];
    if (!status.supervisor_available) {
      out.push(
        "jod-run was not found — it supervises every agent, so nothing can start.",
      );
    }
    if (!status.harnesses.some((h) => h.available)) {
      out.push("No agent harness found. Install Claude Code or OpenCode.");
    }
    return out;
  }, [status]);

  const running = agents.filter((a) => a.status === "running").length;
  const totalCost = agents.reduce((sum, a) => sum + (a.usage.cost_usd ?? 0), 0);

  return (
    <div className="app">
      <aside>
        <header className="brand">
          <h1>Jod</h1>
          <p>
            {running} running · {agents.length} total
            {totalCost > 0 ? ` · $${totalCost.toFixed(4)}` : ""}
          </p>
        </header>

        {blockers.map((b) => (
          <p key={b} className="banner warn">
            {b}
          </p>
        ))}
        {error ? (
          <p className="banner error" onClick={() => setError(null)}>
            {error}
          </p>
        ) : null}

        {status ? (
          <SpawnForm
            harnesses={status.harnesses}
            defaultWorkdir={status.default_workdir}
            disabled={blockers.length > 0}
            onSpawn={spawn}
          />
        ) : (
          <p className="empty">Checking this machine…</p>
        )}

        <h2 className="agents-title">Agents</h2>
        <AgentList agents={agents} selectedId={selectedId} onSelect={selectAgent} />
      </aside>

      <main>
        {selected ? (
          <EventStream
            agent={selected}
            events={eventsByAgent[selected.id] ?? []}
            onKill={kill}
            onOpenTerminal={openTerminal}
          />
        ) : (
          <div className="placeholder">
            <h2>Jod delegates. It does not do the work.</h2>
            <p>
              Every task you hand over is launched as its own supervised process,
              driven by a real agent harness — Claude Code or OpenCode — and
              streamed back here.
            </p>
          </div>
        )}
      </main>
    </div>
  );
}
