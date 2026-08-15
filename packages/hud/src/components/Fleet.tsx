import { useMemo, useState } from "react";
import { fleetKey, type FleetNode, type FleetNodeId } from "../types";

interface Props {
  nodes: FleetNode[];
  /** The selected *run*, shared with the rest of the HUD. */
  selectedId: string | null;
  onSelect(id: string): void;
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
 * ## Why this panel and `Roster` are both here
 *
 * They answer different questions from different places. `Roster` lists the
 * agents in *this daemon's memory*; the fleet is a query against the database,
 * so it shows work started by any process — the TUI, a schedule, another shell.
 * A run appears here whether or not the process serving this page launched it.
 *
 * ## Collapse is held by id
 *
 * The tree reshapes underneath the cursor: runs finish, a session gains a
 * child, a work closes. An index survives none of that, so collapsed rows are
 * remembered by `NodeId` — the same rule `core/src/tree.rs` states for
 * selection, and for the same reason.
 */
export function Fleet({ nodes, selectedId, onSelect }: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());

  const visible = useMemo(() => hideUnder(nodes, collapsed), [nodes, collapsed]);

  const running = nodes.filter((n) => n.kind === "run" && n.running).length;
  // Counts live on every row of a subtree, so summing them would count a run's
  // cards once per ancestor. The roots already carry their whole subtree.
  const blocked = nodes.filter((n) => n.depth === 0).reduce((t, n) => t + n.blocked, 0);

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
        {running > 0 && <span className="badge st-running">{running} RUNNING</span>}
        {blocked > 0 && <span className="badge st-blocked">{blocked} BLOCKED</span>}
      </h2>

      {nodes.length === 0 ? (
        <p className="empty">
          No work yet.
          <br />
          <span className="hint">
            Works, sessions and runs appear here however they were started.
          </span>
        </p>
      ) : (
        <ul className="fleet-tree">
          {visible.map((node) => {
            const key = fleetKey(node.id);
            const isRun = node.kind === "run";
            const selected = isRun && node.id.id === selectedId;
            return (
              <li
                key={key}
                className={`fleet-row k-${node.kind}${selected ? " selected" : ""}${
                  node.running ? " live" : ""
                }`}
                style={{ paddingLeft: `${node.depth * 14 + 8}px` }}
                onClick={() => {
                  if (isRun) onSelect(node.id.id);
                  else if (node.has_children) toggle(node.id);
                }}
              >
                <span className="twisty">
                  {node.has_children ? (collapsed.has(key) ? "▸" : "▾") : "·"}
                </span>
                <span className={`dot k-${node.kind}`} />
                <span className="fleet-label" title={node.summary || node.label}>
                  {node.label}
                </span>
                {node.blocked > 0 ? (
                  <span className="badge st-blocked">{node.blocked} blocked</span>
                ) : node.cards > 0 ? (
                  <span className="badge st-cards">{node.cards} open</span>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </aside>
  );
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
