import { useEffect, useRef } from "react";

import type { AgentEnvelope, AgentSummary, Usage } from "../types";

function time(ms: number) {
  return new Date(ms).toLocaleTimeString();
}

function formatUsage(usage: Usage) {
  const parts: string[] = [];
  if (usage.input_tokens != null) parts.push(`${usage.input_tokens.toLocaleString()} in`);
  if (usage.output_tokens != null) parts.push(`${usage.output_tokens.toLocaleString()} out`);
  if (usage.cache_read_tokens) parts.push(`${usage.cache_read_tokens.toLocaleString()} cached`);
  if (usage.cost_usd != null) parts.push(`$${usage.cost_usd.toFixed(4)}`);
  return parts.join(" · ");
}

/** Label and body for one event, so the renderer stays a single flat map. */
function describe(event: AgentEnvelope): { tag: string; body: string; tone: string } {
  switch (event.kind) {
    case "started":
      return {
        tag: "started",
        tone: "muted",
        body: [event.model, event.session_id].filter(Boolean).join(" · ") || "session opened",
      };
    case "thinking":
      return { tag: "thinking", tone: "muted", body: event.text };
    case "message":
      return { tag: "message", tone: "message", body: event.text };
    case "tool_call":
      return {
        tag: `tool → ${event.name}`,
        tone: "tool",
        body: event.input == null ? "" : JSON.stringify(event.input, null, 2),
      };
    case "tool_result":
      return {
        tag: `tool ← ${event.name}`,
        tone: event.is_error ? "error" : "tool",
        body: event.summary ?? "",
      };
    case "finished": {
      const usage = formatUsage(event.usage);
      return {
        tag: event.is_error ? "failed" : "finished",
        tone: event.is_error ? "error" : "done",
        body: [event.text, usage].filter(Boolean).join("\n\n") || "no output",
      };
    }
    case "raw":
      return { tag: "raw", tone: "muted", body: event.line };
    case "error":
      return { tag: "error", tone: "error", body: event.message };
  }
}

interface Props {
  agent: AgentSummary;
  events: AgentEnvelope[];
  onKill: (id: string) => void;
  onOpenTerminal: (id: string) => void;
}

export function EventStream({ agent, events, onKill, onOpenTerminal }: Props) {
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  return (
    <section className="stream">
      <header className="stream-head">
        <div>
          <h2>
            {agent.name} <span className={`badge ${agent.status}`}>{agent.status}</span>
          </h2>
          <p className="sub">
            {agent.harness_label}
            {agent.model ? ` · ${agent.model}` : ""} · {agent.cwd}
          </p>
          {agent.session_closed ? (
            <p className="sub">tmux session closed</p>
          ) : (
            <>
              <p className="sub mono">{agent.attach_command}</p>
              <p className="sub mono">{agent.switch_command} <span className="hint">(from inside tmux)</span></p>
            </>
          )}
        </div>
        <div className="actions">
          <button
            onClick={() => onOpenTerminal(agent.id)}
            disabled={agent.session_closed}
          >
            Watch in tmux
          </button>
          {/* The session outlives the agent, so this stays available after a
              run finishes — that is the only way to reclaim it. */}
          <button
            className="danger"
            onClick={() => onKill(agent.id)}
            disabled={agent.session_closed}
          >
            {agent.status === "running" ? "Kill" : "Close session"}
          </button>
        </div>
      </header>

      {agent.usage.cost_usd != null || agent.usage.output_tokens != null ? (
        <p className="usage">{formatUsage(agent.usage)}</p>
      ) : null}

      <div className="events">
        {events.length === 0 ? (
          <p className="empty">Waiting for the harness to say something…</p>
        ) : (
          events.map((event) => {
            const { tag, body, tone } = describe(event);
            return (
              <article key={event.seq} className={`event ${tone}`}>
                <header>
                  <span className="tag">{tag}</span>
                  <span className="at">{time(event.at_ms)}</span>
                </header>
                {body ? <pre>{body}</pre> : null}
              </article>
            );
          })
        )}
        <div ref={bottom} />
      </div>
    </section>
  );
}
