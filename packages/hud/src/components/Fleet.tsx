import { useEffect, useMemo, useRef, useState } from "react";
import {
  TIER_LABEL,
  fleetKey,
  tiersOf,
  type FleetNode,
  type FleetNodeId,
} from "../types";
import { useSelection } from "../hooks/useSelection";
import { SelectionBar } from "./SelectionBar";

interface Props {
  nodes: FleetNode[];
  /**
   * The run each row's verbs act on, from the server's own fold.
   *
   * Clicking a row used to mean "find the newest run row beneath this one",
   * which worked while the tree carried run rows. It does not any more:
   * `jod_core::tree::condense` drops them and hands this over instead, so the
   * row that *says* an agent is running is the row that opens it.
   */
  runOf: ReadonlyMap<string, string>;
  /** The selected *run*, shared with the rest of the HUD. */
  selectedId: string | null;
  /**
   * Runs the live stream currently says are going, by run id.
   *
   * The tree itself is a poll — a database query on a four-second timer — so on
   * its own it is up to four seconds behind on the one fact people watch it
   * for. The roster behind this set is reconciled off the event stream within
   * about 400ms of a run starting or finishing, so it is what a row's pulse
   * actually follows. Reached through the row's own run — see [`isLiveRow`] —
   * so a heading keeps the tree's word alone and a closed work goes on
   * declining to claim it is running.
   */
  liveRuns: ReadonlySet<string>;
  /** Open a run: select it and read it. */
  onOpen(id: string): void;
  /** Delete these rows. Keys are `fleetKey` values, so kind travels with id. */
  onDelete(keys: string[]): void;
  canWrite: boolean;
}

/**
 * The fleet tree — the repositories, and the agents inside them.
 *
 * The same tree `jod tui` draws, and not a second implementation of it:
 * `jod-core` flattens the forest and folds it — `Store::forest_of`, then
 * `tree::condense` — `GET /v1/fleet` hands the result over unchanged, and this
 * renders it. `depth` is the whole layout: rows arrive in document order, each
 * directly below its parent, so nothing here rebuilds a hierarchy that already
 * came flattened, and nothing here folds one that already came folded.
 *
 * Two levels, not five. A work and a run are not rows — the fold drops them,
 * because "who is working on this repository right now" was three expansions
 * deep. Neither becomes unreachable: a run is inside the conversation that
 * started it, and `runOf` is how a row still opens one.
 *
 * ## Why this panel and `Sessions` are both here
 *
 * They answer different questions from different places. `Sessions` lists the
 * runs in *this daemon's memory*; the fleet is a query against the database, so
 * it shows work started by any process — the TUI, a schedule, another shell. A
 * run appears here whether or not the process serving this page launched it.
 *
 * ## Collapse is held by id
 *
 * The tree reshapes underneath the cursor: runs finish, a session gains a
 * child, a work closes. An index survives none of that, so collapsed rows are
 * remembered by `NodeId` — the same rule `core/src/tree.rs` states for
 * selection, and for the same reason.
 *
 * ## The twisty collapses; the row reads
 *
 * Two targets, because they are two intents. Clicking a row opens the run it
 * stands for in the trajectory — including on a repository's row, which is what
 * somebody clicking "the thing that is running" means. Collapsing is the twisty
 * alone. When one target did both, every attempt to read a session first folded
 * it away.
 *
 * ## Colour is rank, and only rank
 *
 * Three hues, for the three ranks of the chain of command — Jod, a manager, an
 * engineer. → [`tiersOf`]. This is the one panel that draws the hierarchy, so
 * it is the one place where knowing which of the three you are looking at is
 * worth a colour; everywhere else in the HUD, hue means harness. A project row
 * takes no hue at all, because it is the repository being argued about rather
 * than one of the parties.
 */
export function Fleet({
  nodes,
  selectedId,
  liveRuns,
  runOf,
  onOpen,
  onDelete,
  canWrite,
}: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const seeded = useRef(false);

  // Every repository arrives shut, once, on the first tree that has one.
  //
  // The same default `jod tui` opens on, and for its reason: with every agent
  // in every repository on screen at once the fleet is a wall of rows and the
  // one repository you came to look at is somewhere in it. Shut, the panel
  // opens as the list of repositories and one click opens the one you want.
  //
  // Seeded rather than filtered so it survives being opened: once the user
  // expands a project, later polls must not fold it back up under them. A
  // repository that appears afterwards is left open, because it appeared while
  // somebody was watching and folding it away would hide the new thing.
  useEffect(() => {
    if (seeded.current || nodes.length === 0) return;
    seeded.current = true;
    const shut = nodes
      .filter((n) => n.kind === "project" && n.has_children)
      .map((n) => fleetKey(n.id));
    if (shut.length > 0) setCollapsed((prev) => new Set([...prev, ...shut]));
  }, [nodes]);

  const visible = useMemo(() => hideUnder(nodes, collapsed), [nodes, collapsed]);
  // Only the rows a delete can actually take. The three the action does not
  // handle used to be selectable anyway, and picking one did nothing at all:
  // `App`'s switch routes a key by its `kind_tag` and quietly dropped anything
  // it did not recognise, so selecting a manager and pressing delete reported
  // success over having done nothing. Offering exactly what the action supports
  // is the fix; select-all now means "everything deletable here".
  const keys = useMemo(
    () => visible.filter(deletable).map((n) => fleetKey(n.id)),
    [visible],
  );
  const selection = useSelection(keys);
  const tiers = useMemo(() => tiersOf(nodes), [nodes]);

  // Agents, not rows. A live session also makes its project's row live, and
  // counting both would report two agents working where there is one.
  const running = nodes.filter(
    (n) => isAgentRow(n) && isLiveRow(n, liveRuns, runOf),
  ).length;

  const toggle = (id: FleetNodeId) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      const key = fleetKey(id);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  return (
    <aside className="panel fleet">
      <h2>
        FLEET
        {running > 0 && <span className="live-count">{running}</span>}
        <span className="count">{nodes.length}</span>
      </h2>

      {nodes.length === 0 ? (
        <p className="empty">No work yet.</p>
      ) : (
        <ul className="fleet-tree">
          {visible.map((node) => {
            const key = fleetKey(node.id);
            const target = openable(nodes, node, runOf);
            const picked = selection.has(key);
            const tier = tiers.row.get(key);
            const live = isLiveRow(node, liveRuns, runOf);
            return (
              <li
                key={key}
                className={`fleet-row k-${node.kind}${tier ? ` t-${tier}` : ""}${
                  target && target === selectedId ? " selected" : ""
                }${live ? " live" : ""}${node.stalled_for_ms !== null ? " stalled" : ""}${
                  picked ? " picked" : ""
                }`}
                style={{ paddingLeft: `${node.depth * 12 + 4}px` }}
              >
                {deletable(node) ? (
                  <button
                    className="pick"
                    role="checkbox"
                    aria-checked={picked}
                    aria-label={`Select ${node.label}`}
                    onClick={() => selection.toggle(key)}
                  >
                    <i />
                  </button>
                ) : (
                  // The column still has to be held, or every row below a
                  // heading would sit a checkbox's width to its left.
                  <span className="pick-gap" />
                )}
                <button
                  className="twisty"
                  disabled={!node.has_children}
                  aria-label={node.has_children ? "Collapse" : undefined}
                  onClick={() => node.has_children && toggle(node.id)}
                >
                  {node.has_children ? (collapsed.has(key) ? "▸" : "▾") : ""}
                </button>
                <button
                  className="fleet-open"
                  disabled={!target}
                  title={hint(node, target)}
                  onClick={() => target && onOpen(target)}
                >
                  <span className={`dot k-${node.kind}${tier ? ` t-${tier}` : ""}`} />
                  {tier && node.kind !== "run" && (
                    <span className={`rank t-${tier}`}>{TIER_LABEL[tier]}</span>
                  )}
                  <span className="fleet-label">{node.label}</span>
                  {node.stalled_for_ms !== null && (
                    <span className="badge st-stalled">{humanMs(node.stalled_for_ms)}</span>
                  )}
                  {node.blocked > 0 && <span className="badge st-blocked">{node.blocked}</span>}
                </button>
              </li>
            );
          })}
        </ul>
      )}

      <SelectionBar
        selection={selection}
        canWrite={canWrite}
        noun="row"
        onDelete={() => onDelete(selection.chosen)}
      />
    </aside>
  );
}

/**
 * Whether a delete can take this row.
 *
 * Exactly the three kinds `App.deleteFleetRows` knows how to route — a run, a
 * conversation, a work. The other three are not merely unimplemented, they are
 * things that should not be deleted from a tree: Jod's own chat is pinned and
 * the server refuses it, a manager holds everything a repository has learned,
 * and a project is untracked through `jod project` rather than swept up in a
 * multi-select.
 */
export function deletable(node: FleetNode): boolean {
  return node.kind === "run" || node.kind === "session" || node.kind === "work";
}

/**
 * Whether this row should draw as live right now.
 *
 * Two sources, and either will do. The tree is a four-second poll, so a run
 * that started two hundred milliseconds ago is running and the tree does not
 * know yet; the roster behind `liveRuns` is reconciled off the event stream and
 * does. → the `liveRuns` prop.
 *
 * The roster is consulted through the row's *own* run, which is what keeps this
 * from contradicting the fold. A heading — a project, a closed work — holds no
 * run, so it keeps the tree's word alone, and a closed work goes on declining
 * to claim it is running even with something alive underneath it.
 */
export function isLiveRow(
  node: FleetNode,
  liveRuns: ReadonlySet<string>,
  runOf: ReadonlyMap<string, string>,
): boolean {
  if (node.running) return true;
  const run = runOf.get(fleetKey(node.id));
  return run !== undefined && liveRuns.has(run);
}

/** The rows that stand for somebody working, as opposed to a heading. */
export function isAgentRow(node: FleetNode): boolean {
  return node.kind === "main" || node.kind === "manager" || node.kind === "session";
}

/**
 * The row's tooltip — what it last said, or why it cannot be opened.
 *
 * A disabled button with no explanation is the state this panel was already in
 * for every manager: the row was there, the pointer said no, and nothing said
 * why. A manager nobody has given an instruction to yet genuinely has nothing
 * to show, and saying so is the difference between "not yet" and "broken".
 */
export function hint(node: FleetNode, target: string | null): string {
  if (target) return node.summary || node.label;
  if (node.kind === "manager") return "this manager has not been asked anything yet";
  if (node.kind === "main") return "jod has not run anything yet";
  if (node.kind === "project") return "nothing has run in this repository yet";
  return node.summary || node.label;
}

/** A silence, at the coarsest unit that still reads as a duration. */
export function humanMs(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h`;
}

/**
 * The run a click on this row should open, or null if there is none.
 *
 * Asked of the server rather than worked out here. This used to walk the rows
 * beneath a heading looking for the newest run row, which was the only thing
 * available while the tree carried them — and it is why a manager was
 * unclickable for as long as one existed, since `forest_of` emitted that row as
 * a permanent leaf with no run beneath it to find.
 *
 * `jod_core::tree::condense` decides it now, for every surface at once, and by
 * a better rule than document position: the run still going if there is one,
 * otherwise the last one the conversation took. A row that has never run
 * anything still answers null, and the button says why rather than going quiet.
 * → [`hint`]
 *
 * A project answers for the whole repository beneath it: it holds no run of its
 * own, so it takes the first one any row under it offers. Exported for its test.
 */
export function openable(
  all: FleetNode[],
  row: FleetNode,
  runOf: ReadonlyMap<string, string>,
): string | null {
  const own = runOf.get(fleetKey(row.id));
  if (own) return own;
  if (!row.has_children) return null;

  const start = all.indexOf(row);
  if (start === -1) return null;
  for (let i = start + 1; i < all.length; i++) {
    // Everything under a row is the rows after it that are deeper, up to the
    // next row at or above its own depth.
    if (all[i].depth <= row.depth) break;
    const run = runOf.get(fleetKey(all[i].id));
    if (run) return run;
  }
  return null;
}

/**
 * Drop every row beneath a collapsed one.
 *
 * Exported for its test. Written against `depth` rather than by walking
 * `parent` links because the forest arrives already flattened in document
 * order: everything under a row is exactly the rows after it that are deeper,
 * up to the next row at or above its own depth. That also makes it correct for
 * a subtree nested inside another collapsed subtree, which a parent-link filter
 * gets wrong unless it is applied transitively.
 */
export function hideUnder(nodes: FleetNode[], collapsed: Set<string>): FleetNode[] {
  const out: FleetNode[] = [];
  // The depth of the shallowest collapsed row we are currently inside, or null.
  let hidingBelow: number | null = null;

  for (const node of nodes) {
    if (hidingBelow !== null && node.depth > hidingBelow) continue;
    hidingBelow = null;
    out.push(node);
    if (collapsed.has(fleetKey(node.id))) hidingBelow = node.depth;
  }
  return out;
}
