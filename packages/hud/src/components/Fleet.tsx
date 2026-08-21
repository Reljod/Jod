import { useMemo, useState } from "react";
import { fleetKey, type FleetNode, type FleetNodeId } from "../types";
import { useSelection } from "../hooks/useSelection";
import { SelectionBar } from "./SelectionBar";

interface Props {
  nodes: FleetNode[];
  /** The selected *run*, shared with the rest of the HUD. */
  selectedId: string | null;
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
 */
export function Fleet({ nodes, selectedId, onOpen, onDelete, canWrite }: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());

  const visible = useMemo(() => hideUnder(nodes, collapsed), [nodes, collapsed]);
  const keys = useMemo(() => visible.map((n) => fleetKey(n.id)), [visible]);
  const selection = useSelection(keys);

  const running = nodes.filter((n) => n.kind === "run" && n.running).length;

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
            return (
              <li
                key={key}
                className={`fleet-row k-${node.kind}${
                  target && target === selectedId ? " selected" : ""
                }${node.running ? " live" : ""}${picked ? " picked" : ""}`}
                style={{ paddingLeft: `${node.depth * 12 + 4}px` }}
              >
                <button
                  className="pick"
                  role="checkbox"
                  aria-checked={picked}
                  aria-label={`Select ${node.label}`}
                  onClick={() => selection.toggle(key)}
                >
                  <i />
                </button>
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
                  title={node.summary || node.label}
                  onClick={() => target && onOpen(target)}
                >
                  <span className={`dot k-${node.kind}`} />
                  <span className="fleet-label">{node.label}</span>
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
 * The run a click on this row should open, or null if there is none.
 *
 * A run row is itself. A work or a session row is the newest run anywhere
 * beneath it, which is what "show me what this is doing" means for a heading —
 * the alternative is a row that looks clickable and does nothing.
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
