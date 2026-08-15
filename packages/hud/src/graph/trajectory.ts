import type { AgentNode } from "../state/world";
import { truncate } from "../state/world";
import type { AgentEnvelope } from "../types";

/**
 * One run, read end to end.
 *
 * The tactical graph answers *what is the fleet doing*, the swimlanes answer
 * *where did the wall-clock go across agents*. Neither answers the question you
 * actually have about a run that has finished: **what did this session say, ask
 * for, and get back, in order** — so this derives the third view from the same
 * event stream.
 *
 * Pure, like the rest of `graph/`. The interesting cases are all edge cases —
 * a tool call that never returned, a nine-minute think that emitted only ticks,
 * a run whose first turn was evicted by the retention cap — and every one of
 * them is a data shape, testable without a DOM.
 */

/**
 * What a row *is*, not what colour it is.
 *
 * `stream` has no counterpart in `AgentEvent`: it is a contiguous run of
 * `progress`/`delta` frames collapsed into one line. Those frames are the only
 * thing on the wire during a long think or a long write, so dropping them would
 * redraw the exact gap they were added to close — a transcript that jumps from
 * a tool result to a message nine minutes later with nothing in between, which
 * is indistinguishable from a hung run.
 */
export type RowKind =
  | "system"
  | "user"
  | "assistant"
  | "thinking"
  | "stream"
  | "tool"
  | "raw"
  | "error"
  | "finish";

export interface TrajectoryRow {
  /** Stable across rebuilds, so React keys and the expanded set survive a tick. */
  id: string;
  kind: RowKind;
  /** 1-based. `0` for what precedes the first model turn: setup and the ask. */
  turn: number;
  at: number;
  /** `-1` for a row Jod synthesised rather than received. */
  seq: number;
  badge: string;
  /** The one-line form. Always present, always single-line. */
  summary: string;
  /** The whole thing, when there is more of it than the summary showed. */
  detail: string | null;
  toolName: string | null;
  /** Tool rows and stream rows. `null` while a call is still in flight. */
  durationMs: number | null;
  isError: boolean;
  /** Still open: a tool call with no result, or a stream that has not resolved. */
  open: boolean;
  /** How many wire frames a `stream` row stands for. */
  frames: number;
}

/** One block in the band across the top. Fractions of the chosen axis, 0..1. */
export interface BandSegment {
  lane: BandLane;
  from: number;
  to: number;
  label: string;
  turn: number;
  isError: boolean;
}

export type BandLane = "input" | "model" | "tools";

export const BAND_LANES: readonly BandLane[] = ["input", "model", "tools"] as const;

/**
 * What the band's x-axis measures.
 *
 * `duration` is the truth about cost and the default: a nine-minute think is
 * nine minutes wide. It is also the one that makes structure unreadable, since
 * every fast turn collapses to a sliver — so `turns` and `calls` re-space the
 * same blocks evenly, trading the wall-clock for the shape of the loop.
 */
export type BandScale = "duration" | "turns" | "calls";

export interface Trajectory {
  rows: TrajectoryRow[];
  band: BandSegment[];
  /** How many times the model was invoked. */
  turns: number;
  toolCalls: number;
  startedAt: number;
  /** `now` while the run is live. */
  endedAt: number;
  durationMs: number;
  /**
   * Events the retention cap dropped off the front. Reported rather than
   * hidden: a transcript that silently begins at turn 6 reads as a run that
   * began at turn 6.
   */
  dropped: number;
  /** False while the run's history predates this page and has not been fetched. */
  complete: boolean;
}

const BADGES: Record<RowKind, string> = {
  system: "SYSTEM",
  user: "USER",
  assistant: "ASSISTANT",
  thinking: "THINKING",
  stream: "WORKING",
  tool: "TOOL",
  raw: "RAW",
  error: "ERROR",
  finish: "FINISH",
};

/** Which lane of the band a row's time belongs to. */
function laneFor(kind: RowKind): BandLane | null {
  switch (kind) {
    case "user":
    case "system":
      return "input";
    case "assistant":
    case "thinking":
    case "stream":
      return "model";
    case "tool":
      return "tools";
    default:
      return null;
  }
}

export interface BuildOptions {
  /** Where a live run's timeline ends. */
  now: number;
  /** The run's opening prompt, when the transcript store had one. */
  prompt?: string | null;
  scale?: BandScale;
}

/**
 * Fold one agent's retained events into a readable session.
 *
 * Takes the node rather than a bare event list because the header rows are not
 * in the stream: what model, what harness, what directory and under what
 * permission the session ran are properties of the run, and a trajectory that
 * opened on `thinking` would be missing the only context that makes the rest
 * legible.
 */
export function buildTrajectory(node: AgentNode, opts: BuildOptions): Trajectory {
  const events = node.events;
  const s = node.summary;
  const rows: TrajectoryRow[] = [];

  const startedAt = events.length > 0 ? events[0].at_ms : s.created_at_ms;
  const live = s.status === "running";
  const lastAt = events.length > 0 ? events[events.length - 1].at_ms : startedAt;
  const endedAt = live ? Math.max(opts.now, lastAt) : lastAt;

  rows.push({
    id: "row:system",
    kind: "system",
    turn: 0,
    at: s.created_at_ms,
    seq: -1,
    badge: BADGES.system,
    summary: sessionLine(node),
    detail: sessionDetail(node),
    toolName: null,
    durationMs: null,
    isError: false,
    open: false,
    frames: 0,
  });

  // The ask, when it can be had. It is deliberately not synthesised from
  // anything: a run whose prompt the transcript store has not yielded shows no
  // USER row at all rather than a plausible-looking guess at what was asked.
  if (opts.prompt) {
    rows.push({
      id: "row:prompt",
      kind: "user",
      turn: 0,
      at: s.created_at_ms,
      seq: -1,
      badge: BADGES.user,
      summary: truncate(opts.prompt, 160),
      detail: opts.prompt,
      toolName: null,
      durationMs: null,
      isError: false,
      open: false,
      frames: 0,
    });
  }

  let turn = 0;
  /** A result has come back, so the next model output is a fresh invocation. */
  let awaitingModel = true;
  let toolCalls = 0;
  /** The open `stream` row absorbing consecutive tick/fragment frames. */
  let streaming: TrajectoryRow | null = null;
  const openCalls: TrajectoryRow[] = [];

  const closeStream = () => {
    streaming = null;
  };

  for (const env of events) {
    // A model-authored event after a tool came back means the harness invoked
    // the model again: that is the turn boundary, and the only one the stream
    // states. Counting `started` or messages alone would merge a ten-step
    // agentic loop into one turn.
    const authored =
      env.kind === "thinking" ||
      env.kind === "message" ||
      env.kind === "tool_call" ||
      env.kind === "delta" ||
      env.kind === "progress";
    if (authored && awaitingModel) {
      turn += 1;
      awaitingModel = false;
    }

    switch (env.kind) {
      case "started":
        // Folded into the SYSTEM row above, which already carries the session
        // and model this event reports. A second row saying the same thing
        // would push the first real turn below the fold.
        break;

      case "progress":
      case "delta": {
        const tokens = env.kind === "progress" ? env.thinking_tokens : undefined;
        if (streaming) {
          streaming.frames += 1;
          streaming.durationMs = env.at_ms - streaming.at;
          streaming.summary = streamLine(streaming.frames, tokens ?? lastTokens(streaming));
          if (tokens != null) streaming.detail = `${tokens} thinking tokens so far`;
        } else {
          streaming = {
            id: `row:${env.seq}`,
            kind: "stream",
            turn,
            at: env.at_ms,
            seq: env.seq,
            badge: BADGES.stream,
            summary: streamLine(1, tokens),
            detail: tokens != null ? `${tokens} thinking tokens so far` : null,
            toolName: null,
            durationMs: 0,
            isError: false,
            open: true,
            frames: 1,
          };
          rows.push(streaming);
        }
        break;
      }

      case "thinking":
      case "message": {
        // Whatever was streaming has now arrived complete.
        if (streaming) streaming.open = false;
        closeStream();
        const kind: RowKind = env.kind === "thinking" ? "thinking" : "assistant";
        rows.push({
          id: `row:${env.seq}`,
          kind,
          turn,
          at: env.at_ms,
          seq: env.seq,
          badge: BADGES[kind],
          // An empty block is a real event and not a rendering failure: recent
          // models emit thinking blocks with the reasoning text withheld, and a
          // blank row reads as a bug in this view rather than as what happened.
          summary: env.text.trim() === "" ? emptyBlock(kind) : truncate(env.text, 160),
          detail: env.text.trim() === "" ? null : env.text,
          toolName: null,
          durationMs: null,
          isError: false,
          open: false,
          frames: 0,
        });
        break;
      }

      case "tool_call": {
        if (streaming) streaming.open = false;
        closeStream();
        toolCalls += 1;
        const row: TrajectoryRow = {
          id: `row:${env.seq}`,
          kind: "tool",
          turn,
          at: env.at_ms,
          seq: env.seq,
          badge: BADGES.tool,
          summary: `${env.name}(${previewArgs(env.input)})`,
          detail: env.input == null ? null : pretty(env.input),
          toolName: env.name,
          durationMs: null,
          isError: false,
          open: true,
          frames: 0,
        };
        rows.push(row);
        openCalls.push(row);
        break;
      }

      case "tool_result": {
        if (streaming) streaming.open = false;
        closeStream();
        // Newest matching open call first — a turn that fired three `Read`s
        // gets its results paired in the order they were issued.
        const idx = lastIndexWhere(openCalls, (r) => r.toolName === env.name);
        const call = idx >= 0 ? openCalls.splice(idx, 1)[0] : null;
        if (call) {
          call.open = false;
          call.durationMs = env.at_ms - call.at;
          call.isError = env.is_error;
          if (env.summary) {
            call.detail = call.detail
              ? `${call.detail}\n\n→ ${env.summary}`
              : `→ ${env.summary}`;
          }
        } else {
          // A result with no call in the retained window — normal at the front
          // of a capped transcript, and it still says a tool ran.
          rows.push({
            id: `row:${env.seq}`,
            kind: "tool",
            turn,
            at: env.at_ms,
            seq: env.seq,
            badge: BADGES.tool,
            summary: `${env.name} → ${env.summary ?? (env.is_error ? "error" : "ok")}`,
            detail: env.summary ?? null,
            toolName: env.name,
            durationMs: null,
            isError: env.is_error,
            open: false,
            frames: 0,
          });
        }
        awaitingModel = true;
        break;
      }

      case "raw":
        if (streaming) streaming.open = false;
        closeStream();
        rows.push({
          id: `row:${env.seq}`,
          kind: "raw",
          turn,
          at: env.at_ms,
          seq: env.seq,
          badge: BADGES.raw,
          summary: truncate(env.line, 160),
          detail: env.line,
          toolName: null,
          durationMs: null,
          isError: false,
          open: false,
          frames: 0,
        });
        break;

      case "session_lost":
      case "error": {
        if (streaming) streaming.open = false;
        closeStream();
        const text =
          env.kind === "error"
            ? env.message
            : `the harness no longer holds session ${env.session_id}`;
        rows.push({
          id: `row:${env.seq}`,
          kind: "error",
          turn,
          at: env.at_ms,
          seq: env.seq,
          badge: BADGES.error,
          summary: truncate(text, 160),
          detail: text,
          toolName: null,
          durationMs: null,
          isError: true,
          open: false,
          frames: 0,
        });
        break;
      }

      case "finished": {
        if (streaming) streaming.open = false;
        closeStream();
        const text = env.text ?? (env.is_error ? "run failed" : "run completed");
        rows.push({
          id: `row:${env.seq}`,
          kind: "finish",
          turn,
          at: env.at_ms,
          seq: env.seq,
          badge: BADGES.finish,
          summary: truncate(text, 160),
          detail: finishDetail(env),
          toolName: null,
          durationMs: null,
          isError: env.is_error,
          open: false,
          frames: 0,
        });
        break;
      }
    }
  }

  // A call still open when the stream ran out is in flight on a live run and
  // abandoned on a dead one. Both are worth seeing; neither gets a duration.
  for (const call of openCalls) call.open = live;

  return {
    rows,
    band: buildBand(rows, startedAt, endedAt, opts.scale ?? "duration"),
    turns: turn,
    toolCalls,
    startedAt,
    endedAt,
    durationMs: Math.max(0, endedAt - startedAt),
    dropped: events.length > 0 ? events[0].seq : 0,
    complete: node.eventsComplete,
  };
}

/**
 * The band across the top: where each turn's time went.
 *
 * A row's block runs from when it happened to when the next thing did, because
 * that gap *is* its duration — the stream reports when a think started, never
 * when it stopped. The exception is a tool call, which reports both, and whose
 * measured span is used in place of the gap so a call that returned in 200ms
 * inside a 30-second turn does not claim the whole turn.
 */
export function buildBand(
  rows: TrajectoryRow[],
  startedAt: number,
  endedAt: number,
  scale: BandScale,
): BandSegment[] {
  const timed = rows.filter((r) => laneFor(r.kind) !== null && r.seq >= 0);
  if (timed.length === 0) return [];

  const segments: BandSegment[] = [];
  for (let i = 0; i < timed.length; i++) {
    const row = timed[i];
    const lane = laneFor(row.kind);
    if (lane === null) continue;
    const nextAt = i + 1 < timed.length ? timed[i + 1].at : endedAt;
    const measured = row.durationMs != null && row.durationMs > 0 ? row.durationMs : null;
    const span = measured ?? Math.max(0, nextAt - row.at);
    segments.push({
      lane,
      from: row.at,
      to: row.at + span,
      label: row.toolName ?? row.badge,
      turn: row.turn,
      isError: row.isError,
    });
  }

  return rescale(segments, startedAt, endedAt, scale);
}

/**
 * Re-space the blocks onto the chosen axis, 0..1.
 *
 * `duration` keeps real proportions. The other two throw the wall-clock away on
 * purpose: under `calls` every block is the same width, which is the only way
 * to read the shape of a loop whose turns differ by three orders of magnitude.
 */
function rescale(
  segments: BandSegment[],
  startedAt: number,
  endedAt: number,
  scale: BandScale,
): BandSegment[] {
  if (segments.length === 0) return [];

  if (scale === "duration") {
    const total = Math.max(1, endedAt - startedAt);
    return segments.map((s) => ({
      ...s,
      from: clamp01((s.from - startedAt) / total),
      to: clamp01((s.to - startedAt) / total),
    }));
  }

  if (scale === "calls") {
    const step = 1 / segments.length;
    return segments.map((s, i) => ({ ...s, from: i * step, to: (i + 1) * step }));
  }

  // `turns`: every turn gets an equal slice, and its blocks divide that slice.
  const order: number[] = [];
  for (const s of segments) if (!order.includes(s.turn)) order.push(s.turn);
  const slice = 1 / order.length;
  return segments.map((s) => {
    const within = segments.filter((o) => o.turn === s.turn);
    const i = within.indexOf(s);
    const step = slice / within.length;
    const base = order.indexOf(s.turn) * slice;
    return { ...s, from: base + i * step, to: base + (i + 1) * step };
  });
}

/**
 * Rows matching a query, with their turn's shape kept.
 *
 * Matches the detail as well as the summary: the summary is truncated to one
 * line, so searching only it would fail to find text the row is *displaying*
 * the moment someone expands it.
 */
export function filterRows(rows: TrajectoryRow[], query: string): TrajectoryRow[] {
  const q = query.trim().toLowerCase();
  if (q === "") return rows;
  return rows.filter(
    (r) =>
      r.summary.toLowerCase().includes(q) ||
      (r.detail?.toLowerCase().includes(q) ?? false) ||
      r.badge.toLowerCase().includes(q) ||
      (r.toolName?.toLowerCase().includes(q) ?? false),
  );
}

/** Rows grouped into their turns, in order, for a view that draws turn rules. */
export function byTurn(rows: TrajectoryRow[]): { turn: number; rows: TrajectoryRow[] }[] {
  const out: { turn: number; rows: TrajectoryRow[] }[] = [];
  for (const row of rows) {
    const last = out[out.length - 1];
    if (last && last.turn === row.turn) last.rows.push(row);
    else out.push({ turn: row.turn, rows: [row] });
  }
  return out;
}

// ─── row text ────────────────────────────────────────────────────────────────

function sessionLine(node: AgentNode): string {
  const s = node.summary;
  return `${s.harness_label} · ${s.model ?? "default model"} · session ${s.session_id ?? "pending"}`;
}

/**
 * What Jod actually knows about how this session was set up.
 *
 * Not the harness's system prompt: no harness reports it, and inventing a
 * plausible one would be the single most misleading thing this view could do.
 * These are the settings the run was launched under, which is the question the
 * row is really being asked.
 */
function sessionDetail(node: AgentNode): string {
  const s = node.summary;
  return [
    `harness      ${s.harness_label} (${s.harness})`,
    `model        ${s.model ?? "chosen by the harness"}`,
    `session      ${s.session_id ?? "not yet reported"}`,
    `cwd          ${s.cwd}`,
    `permission   ${s.permission}`,
    `process      ${s.pgid == null ? "not launched" : `pgid ${s.pgid} · ${s.process_alive ? "alive" : "gone"}`}`,
    `watch        ${s.watch_command}`,
    "",
    "Jod's stream carries no harness system prompt — no harness reports one.",
    "These are the settings this session was launched under.",
  ].join("\n");
}

/**
 * What to show for a block the harness delivered with no text in it.
 *
 * Names the shape of the event rather than guessing at a cause: the block was
 * emitted, it carried nothing, and that is all this view can honestly say.
 */
function emptyBlock(kind: RowKind): string {
  return kind === "thinking"
    ? "reasoning block with no text"
    : "message block with no text";
}

function streamLine(frames: number, tokens: number | undefined | null): string {
  const base = frames === 1 ? "working…" : `working… ${frames} frames`;
  return tokens != null ? `${base} · ${group(tokens)} thinking tokens` : base;
}

/**
 * Thousands separators that do not depend on the machine.
 *
 * `toLocaleString` would render `4,000` here and `4 000` on a host with a
 * different default locale, which makes the same run read differently on two
 * screens and makes any assertion about it a coin flip off CI.
 */
function group(n: number): string {
  return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** Recover the count a stream row last displayed, so a tick without one keeps it. */
function lastTokens(row: TrajectoryRow): number | null {
  const m = row.summary.match(/·\s([\d,]+)\sthinking tokens/);
  return m ? Number(m[1].replace(/,/g, "")) : null;
}

function finishDetail(env: Extract<AgentEnvelope, { kind: "finished" }>): string {
  const u = env.usage;
  const lines = [env.text ?? (env.is_error ? "run failed" : "run completed"), ""];
  if (env.exit_code != null) lines.push(`exit code    ${env.exit_code}`);
  if (u.input_tokens != null) lines.push(`input        ${group(u.input_tokens)}`);
  if (u.output_tokens != null) lines.push(`output       ${group(u.output_tokens)}`);
  if (u.cache_read_tokens != null) lines.push(`cache read   ${group(u.cache_read_tokens)}`);
  if (u.cache_write_tokens != null) lines.push(`cache write  ${group(u.cache_write_tokens)}`);
  if (u.cost_usd != null) lines.push(`cost         $${u.cost_usd.toFixed(4)}`);
  return lines.join("\n");
}

/** The argument worth showing beside a tool's name, matching `jod watch`. */
function previewArgs(input: unknown): string {
  if (input == null) return "";
  if (typeof input === "string") return truncate(input, 72);
  if (typeof input === "object") {
    const rec = input as Record<string, unknown>;
    const key = ["command", "file_path", "pattern", "query", "url", "prompt"].find(
      (k) => typeof rec[k] === "string",
    );
    if (key) return truncate(String(rec[key]), 72);
    return truncate(JSON.stringify(input), 72);
  }
  return truncate(String(input), 72);
}

function pretty(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function lastIndexWhere<T>(list: T[], pred: (item: T) => boolean): number {
  for (let i = list.length - 1; i >= 0; i--) if (pred(list[i])) return i;
  return -1;
}

const clamp01 = (v: number) => (v < 0 ? 0 : v > 1 ? 1 : v);
