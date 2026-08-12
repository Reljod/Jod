import { useEffect, useMemo, useRef, useState } from "react";
import type { World } from "../state/world";
import { truncate } from "../state/world";
import type { AgentEventKind } from "../types";

interface Props {
  world: World;
  selectedId: string | null;
  onSelect(id: string): void;
}

const KINDS: AgentEventKind[] = [
  "thinking",
  "message",
  "tool_call",
  "tool_result",
  "finished",
  "error",
  "started",
  "raw",
];

/**
 * The fleet-wide event feed.
 *
 * `raw` is collapsed by default rather than dropped. Core emits it for anything
 * a harness said that it could not classify, which makes it the debugging seam
 * for a harness upgrade — hiding it entirely would turn "we did not understand
 * this" into "this never happened".
 */
export function SigintFeed({ world, selectedId, onSelect }: Props) {
  const [muted, setMuted] = useState<Set<AgentEventKind>>(() => new Set(["raw"]));
  const [follow, setFollow] = useState(true);
  const [onlySelected, setOnlySelected] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const items = useMemo(() => {
    return world.feed.filter(
      (f) =>
        !muted.has(f.kind) &&
        (!onlySelected || !selectedId || f.agentId === selectedId),
    );
  }, [world.feed, world.revision, muted, onlySelected, selectedId]);

  useEffect(() => {
    if (!follow) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items.length, follow]);

  const toggle = (k: AgentEventKind) =>
    setMuted((prev) => {
      const next = new Set(prev);
      next.has(k) ? next.delete(k) : next.add(k);
      return next;
    });

  return (
    <section className="panel feed">
      <div className="feed-head">
        <h2>SIGINT<span className="count">{items.length}</span></h2>
        <div className="filters">
          {KINDS.map((k) => (
            <button
              key={k}
              className={muted.has(k) ? "f off" : "f on"}
              onClick={() => toggle(k)}
              title={muted.has(k) ? `Show ${k}` : `Hide ${k}`}
            >
              {k.replace("_", " ")}
            </button>
          ))}
          <span className="sep" />
          <button
            className={onlySelected ? "f on" : "f off"}
            onClick={() => setOnlySelected((v) => !v)}
            title="Restrict to the selected agent"
          >
            focus
          </button>
          <button
            className={follow ? "f on" : "f off"}
            onClick={() => setFollow((v) => !v)}
            title="Auto-scroll"
          >
            follow
          </button>
        </div>
      </div>

      <div
        className="feed-body"
        ref={scrollRef}
        onWheel={() => setFollow(false)}
      >
        {items.length === 0 && <p className="empty">No traffic.</p>}
        {items.map((f) => (
          <button
            key={f.id}
            className={`fr k-${f.kind} ${f.isError ? "bad" : ""} ${
              selectedId === f.agentId ? "sel" : ""
            }`}
            onClick={() => onSelect(f.agentId)}
          >
            <span className="t">{new Date(f.at).toISOString().slice(11, 19)}</span>
            <span className="who">{truncate(f.agentName, 16)}</span>
            <span className="k">{f.kind.replace("_", " ")}</span>
            <span className="x">{truncate(f.text, 150)}</span>
          </button>
        ))}
      </div>
    </section>
  );
}
