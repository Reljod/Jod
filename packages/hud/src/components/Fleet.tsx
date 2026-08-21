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
  /** The selected *run*, shared with the rest of the HUD. */
  selectedId: string | null;
  /**
   * Runs the live stream currently says are going, by run id.
   *
   * The tree itself is a poll — a database query on a four-second timer — so on
   * its own it is up to four seconds behind on the one fact people watch it
   * for. The roster behind this set is reconciled off the event stream within
   * about 400ms of a run starting or finishing, so it is what a row's pulse
   * actually follows. Only run rows take it: a work, a session and a project
   * each decide their own liveness on the Rust side for reasons this set cannot
   * see — a closed work deliberately stops claiming to be running even with
   * something alive underneath it — and second-guessing that here would put the
   * two surfaces into disagreement.
   */
  liveRuns: ReadonlySet<string>;
  /** Open a run: select it and read it. */
  onOpen(id: string): void;
  /** Delete these rows. Keys are `fleetKey` values, so kind travels with id. */
  onDelete(keys: string[]): void;
  canWrite: boolean;
}

/**
 * The fleet tree — works, the sessions under them, and the runs under those.
 *
 * The same forest `jod tui` draws, and not a second implementation of it:
 * `Store::forest_of` in `jod-core` does the flatten once, `GET /v1/fleet` hands
 * the result over unchanged, and this renders it. `depth` is the whole layout —
 * rows arrive in document order, each directly below its parent — so nothing
 * here rebuilds a hierarchy that already came flattened.
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
 * Two targets, because they are two intents. Clicking a row opens the newest
 * run beneath it in the trajectory — including on a work or a session row,
 * which is what somebody clicking "the thing that is running" means. Collapsing
 * is the twisty alone. When one target did both, every attempt to read a
 * session first folded it away.
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
  onOpen,
  onDelete,
  canWrite,
}: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const seeded = useRef(false);

  // Jod's row arrives folded, once, on the first tree that has one.
  //
  // Every instruction he has ever been given is a run in that one conversation
  // — a real fleet had twenty-six under it — and unfolded they push every
  // repository off the top of the panel. Nothing is hidden by this: his row
  // still aggregates their liveness and still pulses while he is working, the
  // twisty says there is more inside, and opening it opens the newest one. It
  // is a fold, which is the idiom this tree already has, and not a cap.
  //
  // Seeded rather than filtered so it survives being reopened: once the user
  // expands it, later polls must not fold it back up under them.
  useEffect(() => {
    if (seeded.current || nodes.length === 0) return;
    seeded.current = true;
    const folded = nodes
      .filter((n) => n.kind === "main" && n.has_children)
      .map((n) => fleetKey(n.id));
    if (folded.length > 0) setCollapsed((prev) => new Set([...prev, ...folded]));
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

  const running = nodes.filter((n) => isLiveRow(n, liveRuns)).length;

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
            const target = openable(nodes, node);
            const picked = selection.has(key);
            const tier = tiers.row.get(key);
            const live = isLiveRow(node, liveRuns);
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
 * A run may be believed by two sources — the tree, which is a poll, and the
 * roster, which the event stream reconciles — so it takes either. Every other
 * kind of row takes the tree's word alone. → the `liveRuns` prop.
 */
export function isLiveRow(node: FleetNode, liveRuns: ReadonlySet<string>): boolean {
  if (node.kind !== "run") return node.running;
  return node.running || liveRuns.has(node.id.id);
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
 * A run row is itself. Every other row is the newest run anywhere beneath it,
 * which is what "show me what this is doing" means for a heading — the
 * alternative is a row that looks clickable and does nothing.
 *
 * This is unchanged, and it is the *tree* that changed under it. A manager was
 * unclickable here for as long as it existed, and the cause was not this
 * function: `Store::forest_of` emitted the row as a permanent leaf, so there
 * was never a run beneath one to find. Now a manager's runs hang from it the
 * way a session's do, and the same walk that always worked for a work finds
 * them. A manager that has genuinely never run still returns null, and the
 * button says why rather than going quiet. → [`hint`]
 *
 * Newest by document position rather than by a timestamp, because a `FleetNode`
 * carries no clock. `Store::forest_of` emits each parent's children in the
 * order the tree holds them, so the last descendant is the one added most
 * recently. Exported for its test.
 */
export function openable(all: FleetNode[], row: FleetNode): string | null {
  if (row.kind === "run") return row.id.id;

  const start = all.indexOf(row);
  if (start === -1) return null;
  let found: string | null = null;
  for (let i = start + 1; i < all.length; i++) {
    // Everything under a row is the rows after it that are deeper, up to the
    // next row at or above its own depth.
    if (all[i].depth <= row.depth) break;
    if (all[i].kind === "run") found = all[i].id.id;
  }
  return found;
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
