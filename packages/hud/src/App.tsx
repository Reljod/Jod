import { useCallback, useEffect, useMemo, useState } from "react";
import { summariseFailures, useJod } from "./hooks/useJod";
import { TacticalView } from "./components/TacticalView";
import { TopBar, type ViewMode } from "./components/TopBar";
import { TimelineView } from "./components/TimelineView";
import { TrajectoryView } from "./components/TrajectoryView";
import { Sessions } from "./components/Sessions";
import { Dossier } from "./components/Dossier";
import { Fleet } from "./components/Fleet";
import { SigintFeed } from "./components/SigintFeed";
import { CommandPalette } from "./components/CommandPalette";
import { AuthGate } from "./components/AuthGate";
import { DeleteDialog, type DeleteRequest } from "./components/DeleteDialog";
import type { AgentNode } from "./state/world";
import type { TransportFactory } from "./transport";
import { tiersOf, type HarnessKind, type Resume, type SpawnRequest } from "./types";

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
  const [view, setView] = useState<ViewMode>("tactical");
  const [pending, setPending] = useState<DeleteRequest | null>(null);
  const [deleting, setDeleting] = useState(false);

  // Write actions follow the session's scope, which `POST /v1/session` returns.
  // A read token cannot spawn, kill or delete, so the controls are disabled
  // rather than firing a request that 403s. Anything other than an explicit
  // "write" — an absent field, a lost link, a pending probe — is treated as
  // read.
  const canWrite =
    world.link.phase === "simulated" ||
    (world.link.phase === "live" && world.link.scope === "write");

  /**
   * The runs the roster currently believes are going.
   *
   * The fleet tree is a poll and the roster is reconciled off the event stream,
   * so this is the fresher of the two answers by up to a full poll interval —
   * which is exactly the interval during which somebody watching a manager pick
   * up a job sees nothing happen. Recomputed on `revision`, the same tick every
   * other panel redraws on.
   */
  /**
   * The chain of command, derived once and shared by the panels that draw it.
   *
   * Computed here rather than inside each panel because both need it and the
   * fleet is the only source: a roster entry says nothing about rank, so the
   * sessions list borrows the tree's answer through `tiers.run`.
   */
  const tiers = useMemo(
    () => tiersOf(jod.fleet.nodes, jod.fleet.runOf),
    [jod.fleet],
  );

  const liveRuns = useMemo(() => {
    const ids = new Set<string>();
    for (const [id, node] of world.agents) {
      if (node.summary.status === "running") ids.add(id);
    }
    return ids;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `world` is a
    // mutable store; `revision` is what says it changed.
  }, [world, jod.revision]);

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

  /**
   * Select a session and read it, in one gesture.
   *
   * Both lists call this. Choosing a row and opening it were two steps for as
   * long as there were two panels, and nobody ever wanted only the first.
   */
  const open = useCallback((id: string) => {
    setSelectedId(id);
    setView("trajectory");
  }, []);

  /**
   * Run a confirmed delete.
   *
   * Works go last and are the reason this is not three parallel calls: the
   * server refuses a work holding worktrees, arms itself, and expects the same
   * request again. So a refusal is kept — not thrown — and put back in front of
   * the person with the server's own sentence, `armed` set, and CONFIRM sending
   * exactly the same thing. Runs and conversations that already went are not
   * sent twice: they are cleared from the request as they succeed.
   */
  const confirmDelete = useCallback(async () => {
    if (!pending) return;
    setDeleting(true);
    try {
      const runs = await jod.deleteRuns(pending.runs);
      const conversations = await jod.deleteConversations(pending.conversations);

      const refusals: string[] = [];
      const worksLeft: string[] = [];
      for (const id of pending.works) {
        const outcome = await jod.deleteWork(id);
        if (!outcome) continue; // no transport, or an error already surfaced
        if (!outcome.deleted) {
          refusals.push(outcome.detail);
          worksLeft.push(id);
        }
      }

      // One message for the whole confirmation. Composed here because this is
      // the only place that knows about all three calls; each of them reporting
      // for itself is what once let an empty conversation list silently clear a
      // run refusal, so a screenful of failures showed as nothing happening.
      jod.reportError(summariseFailures([...runs.failed, ...conversations.failed]));

      const stillPending: DeleteRequest = {
        runs: pending.runs.filter((id) => !runs.deleted.includes(id)),
        conversations: pending.conversations.filter(
          (id) => !conversations.deleted.includes(id),
        ),
        works: worksLeft,
        notice: refusals.join(" "),
        armed: true,
      };

      const nothingLeft =
        stillPending.runs.length === 0 &&
        stillPending.conversations.length === 0 &&
        stillPending.works.length === 0;
      // A run refused for a reason repeating will not fix — it is still
      // running — has already put its reason in the error toast, so the dialog
      // closes rather than inviting a second identical attempt. Only a work
      // refusal arms anything, and only that keeps the dialog open.
      setPending(nothingLeft || worksLeft.length === 0 ? null : stillPending);
    } finally {
      setDeleting(false);
    }
  }, [jod, pending]);

  /**
   * A fleet row's key is `kind:id`, so the split is by what was selected.
   *
   * Only three kinds reach here — `Fleet.deletable` is what decides that, and
   * it is deliberately the same three this switch handles. A `main`, `manager`
   * or `project` key arriving would mean those two have drifted apart, so it is
   * reported rather than dropped: the old behaviour was to ignore the key
   * silently, which showed a confirmation and then a success for a delete that
   * never happened.
   */
  const deleteFleetRows = useCallback(
    (keys: string[]) => {
      const request: DeleteRequest = {
        runs: [],
        conversations: [],
        works: [],
        notice: null,
        armed: false,
      };
      const refused: string[] = [];
      for (const key of keys) {
        const cut = key.indexOf(":");
        const kind = key.slice(0, cut);
        const id = key.slice(cut + 1);
        if (kind === "run") request.runs.push(id);
        else if (kind === "session") request.conversations.push(id);
        else if (kind === "work") request.works.push(id);
        else refused.push(kind);
      }
      if (refused.length > 0) {
        jod.reportError(`a ${refused[0]} row cannot be deleted from the fleet`);
      }
      if (request.runs.length + request.conversations.length + request.works.length > 0) {
        setPending(request);
      }
    },
    [jod],
  );

  const deleteSessions = useCallback((ids: string[]) => {
    setPending({ runs: ids, conversations: [], works: [], notice: null, armed: false });
  }, []);

  return (
    <div className="app">
      <TopBar
        world={world}
        harnesses={jod.harnesses}
        transportLabel={jod.transportLabel}
        view={view}
        onView={setView}
        onCommand={() => {
          setSeed(null);
          setPaletteOpen(true);
        }}
      />

      <main className="stage">
        <Sessions
          world={world}
          selectedId={selectedId}
          tiers={tiers.run}
          onOpen={open}
          onDelete={deleteSessions}
          canWrite={canWrite}
        />

        {view === "tactical" ? (
          <TacticalView store={jod.store} selectedId={selectedId} onSelect={setSelectedId} />
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
          The right-hand column is the fleet, as it is in `jod tui`. The detail
          panel is stacked beneath it rather than replaced: the fleet answers
          "what is going on", the detail "what is this one doing", and the fleet
          is the one you look at first — it is also the only panel here that
          shows work this daemon did not start.
        */}
        <div className="right-rail">
          <Fleet
            nodes={jod.fleet.nodes}
            runOf={jod.fleet.runOf}
            selectedId={selectedId}
            liveRuns={liveRuns}
            onOpen={open}
            onDelete={deleteFleetRows}
            canWrite={canWrite}
          />
          <Dossier
            world={world}
            selectedId={selectedId}
            onKill={(id) => void jod.kill(id)}
            onResume={onResume}
            onDelete={(id) => deleteSessions([id])}
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
          SIMULATED FLEET
        </div>
      )}

      {world.link.phase === "auth" && (
        <AuthGate reason={world.link.reason} onSubmit={jod.authenticate} />
      )}

      <DeleteDialog
        request={pending}
        busy={deleting}
        onCancel={() => setPending(null)}
        onConfirm={() => void confirmDelete()}
      />

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
