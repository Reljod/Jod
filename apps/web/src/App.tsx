import { useCallback, useEffect, useState } from "react";
import { useJod } from "./hooks/useJod";
import { TacticalView } from "./components/TacticalView";
import { TopBar, type ViewMode } from "./components/TopBar";
import { TimelineView } from "./components/TimelineView";
import { Roster } from "./components/Roster";
import { Dossier } from "./components/Dossier";
import { SigintFeed } from "./components/SigintFeed";
import { CommandPalette } from "./components/CommandPalette";
import type { AgentNode } from "./state/world";
import type { HarnessKind, Resume, SpawnRequest } from "./types";

type Seed = { resume: Resume; cwd: string; harness: HarnessKind; name: string } | null;

export default function App() {
  const jod = useJod();
  const world = jod.store.world;

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [seed, setSeed] = useState<Seed>(null);
  const [recentreNonce, setRecentreNonce] = useState(0);
  const [view, setView] = useState<ViewMode>("tactical");

  // Until `POST /v1/session` exists and reports a scope, assume read-only when
  // talking to a real orchestrator — failing safe beats a form that 403s. The
  // simulation is always writable, since nothing real can happen there.
  const canWrite = world.link.phase === "simulated";

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
        ) : (
          <TimelineView store={jod.store} selectedId={selectedId} onSelect={setSelectedId} />
        )}

        <Dossier
          world={world}
          selectedId={selectedId}
          onKill={(id) => void jod.kill(id)}
          onResume={onResume}
          canWrite={canWrite}
        />
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
