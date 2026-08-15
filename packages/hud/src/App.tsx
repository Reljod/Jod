import { useCallback, useEffect, useState } from "react";
import { useJod } from "./hooks/useJod";
import { TacticalView } from "./components/TacticalView";
import { TopBar, type ViewMode } from "./components/TopBar";
import { TimelineView } from "./components/TimelineView";
import { TrajectoryView } from "./components/TrajectoryView";
import { Roster } from "./components/Roster";
import { Dossier } from "./components/Dossier";
import { Fleet } from "./components/Fleet";
import { SigintFeed } from "./components/SigintFeed";
import { CommandPalette } from "./components/CommandPalette";
import { AuthGate } from "./components/AuthGate";
import type { AgentNode } from "./state/world";
import type { TransportFactory } from "./transport";
import type { HarnessKind, Resume, SpawnRequest } from "./types";

type Seed = { resume: Resume; cwd: string; harness: HarnessKind; name: string } | null;

export interface HudProps {
  /** Override the driver. Omitted, the HUD probes `/v1/health` and picks one. */
  makeTransport?: TransportFactory;
}

export default function App({ makeTransport }: HudProps = {}) {
  const jod = useJod(makeTransport);
  const world = jod.store.world;

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [seed, setSeed] = useState<Seed>(null);
  const [recentreNonce, setRecentreNonce] = useState(0);
  const [view, setView] = useState<ViewMode>("tactical");

  // Write actions follow the session's scope, which `POST /v1/session` returns.
  // A read token cannot spawn or kill, so the controls are disabled rather than
  // firing a request that 403s. Anything other than an explicit "write" — an
  // absent field, a lost link, a pending probe — is treated as read.
  const canWrite =
    world.link.phase === "simulated" ||
    (world.link.phase === "live" && world.link.scope === "write");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSeed(null);
        setPaletteOpen((v) => !v);
      } else if (e.key === "Escape") {
        setPaletteOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const onResume = useCallback((node: AgentNode) => {
    if (!node.summary.session_id) return;
    setSeed({
      resume: { session: node.summary.session_id },
      cwd: node.summary.cwd,
      harness: node.summary.harness,
      name: node.summary.name,
    });
    setPaletteOpen(true);
  }, []);

  const onSpawn = useCallback(
    (req: SpawnRequest) => {
      void jod.spawn(req).then((created) => {
        if (created) setSelectedId(created.id);
      });
    },
    [jod],
  );

  return (
    <div className="app">
      <TopBar
        world={world}
        harnesses={jod.harnesses}
        transportLabel={jod.transportLabel}
        view={view}
        onView={setView}
        onRecentre={() => setRecentreNonce((n) => n + 1)}
        onCommand={() => {
          setSeed(null);
          setPaletteOpen(true);
        }}
      />

      <main className="stage">
        <Roster world={world} selectedId={selectedId} onSelect={setSelectedId} />

        {view === "tactical" ? (
          <TacticalView
            store={jod.store}
            selectedId={selectedId}
            onSelect={setSelectedId}
            recentreNonce={recentreNonce}
          />
        ) : view === "timeline" ? (
          <TimelineView store={jod.store} selectedId={selectedId} onSelect={setSelectedId} />
        ) : (
          <TrajectoryView
            store={jod.store}
            transport={jod.transport}
            selectedId={selectedId}
          />
        )}

        {/*
          The right-hand column is the fleet, as it is in `jod tui`. The dossier
          is stacked beneath it rather than replaced: the fleet answers "what is
          going on", the dossier "what is this one doing", and the fleet is the
          one you look at first — it is also the only panel here that shows work
          this daemon did not start.
        */}
        <div className="right-rail">
          <Fleet nodes={jod.fleet} selectedId={selectedId} onSelect={setSelectedId} />
          <Dossier
            world={world}
            selectedId={selectedId}
            onKill={(id) => void jod.kill(id)}
            onResume={onResume}
            onRead={(id) => {
              setSelectedId(id);
              setView("trajectory");
            }}
            canWrite={canWrite}
          />
        </div>
      </main>

      <SigintFeed world={world} selectedId={selectedId} onSelect={setSelectedId} />

      {jod.lastError && (
        <div className="toast" role="alert" onClick={jod.clearError}>
          <strong>REFUSED</strong> {jod.lastError}
          <span className="dismiss">dismiss</span>
        </div>
      )}

      {world.link.phase === "simulated" && (
        <div className="sim-banner" title={world.link.reason}>
          SIMULATED FLEET — no orchestrator on /v1/health
        </div>
      )}

      {world.link.phase === "auth" && (
        <AuthGate reason={world.link.reason} onSubmit={jod.authenticate} />
      )}

      <CommandPalette
        open={paletteOpen}
        world={world}
        harnesses={jod.harnesses}
        canWrite={canWrite}
        seed={seed}
        onClose={() => setPaletteOpen(false)}
        onSpawn={onSpawn}
        onSelect={setSelectedId}
      />
    </div>
  );
}
