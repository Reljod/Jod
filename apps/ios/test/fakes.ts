/**
 * Stand-ins for the two things the browser provides and Node does not.
 *
 * These are deliberately dumb — they record what was asked for and replay what
 * they were told to. Anything clever here would be a second implementation of
 * the daemon, and tests would start passing against a fiction.
 */

import type { AgentEnvelope, AgentSummary } from "../src/contract";
import type { EventSourceLike } from "../src/client";

export interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: unknown;
}

export interface Reply {
  status?: number;
  body?: unknown;
  /** Sent instead of a JSON body, to exercise the non-JSON error path. */
  raw?: string;
  statusText?: string;
}

/**
 * A `fetch` that answers from a queue, keyed by `METHOD /path` or by order.
 *
 * Unmatched requests throw rather than returning a default, because a test that
 * silently got a 200 it never set up is a test that proves nothing.
 */
export class FakeFetch {
  readonly calls: Call[] = [];
  private routes = new Map<string, Reply[]>();

  /** Queue one reply for `METHOD /path`. Repeatable; consumed in order. */
  on(route: string, reply: Reply): this {
    const queue = this.routes.get(route) ?? [];
    queue.push(reply);
    this.routes.set(route, queue);
    return this;
  }

  get fetch(): typeof fetch {
    return (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = (init?.method ?? "GET").toUpperCase();
      this.calls.push({
        url,
        method,
        headers: normaliseHeaders(init?.headers),
        body: init?.body ? JSON.parse(String(init.body)) : undefined,
      });

      // Match on the path without its query string, then fall back to the
      // full URL so a test can pin an exact query when that is the point.
      const path = url.split("?")[0];
      const reply =
        this.routes.get(`${method} ${url}`)?.shift() ??
        this.routes.get(`${method} ${path}`)?.shift();

      if (!reply) {
        throw new Error(`FakeFetch: nothing queued for ${method} ${url}`);
      }

      const status = reply.status ?? 200;
      return {
        ok: status >= 200 && status < 300,
        status,
        statusText: reply.statusText ?? "",
        json: async () => {
          if (reply.raw !== undefined) throw new SyntaxError("not json");
          return reply.body;
        },
      } as unknown as Response;
    }) as typeof fetch;
  }

  calledOnce(route: string): boolean {
    return this.callsTo(route).length === 1;
  }

  callsTo(route: string): Call[] {
    const [method, path] = route.split(" ");
    return this.calls.filter(
      (c) => c.method === method && c.url.split("?")[0] === path,
    );
  }
}

function normaliseHeaders(headers: HeadersInit | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  if (!headers) return out;
  if (Array.isArray(headers)) {
    for (const [k, v] of headers) out[k.toLowerCase()] = v;
  } else if (headers instanceof Headers) {
    headers.forEach((v, k) => (out[k.toLowerCase()] = v));
  } else {
    for (const [k, v] of Object.entries(headers)) out[k.toLowerCase()] = String(v);
  }
  return out;
}

/** An `EventSource` a test can push frames into. */
export class FakeEventSource implements EventSourceLike {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readyState = FakeEventSource.OPEN;
  onerror: ((event: Event) => void) | null = null;
  closed = false;

  private listeners = new Map<string, ((event: MessageEvent) => void)[]>();

  constructor(readonly url: string) {}

  addEventListener(type: string, listener: (event: MessageEvent) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }

  close(): void {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }

  /** Deliver one `event:`/`data:` frame. */
  emit(type: string, data: string): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ data } as MessageEvent);
    }
  }

  /** Deliver an envelope on the `agent` channel, as the daemon does. */
  send(envelope: Partial<AgentEnvelope>): void {
    this.emit("agent", JSON.stringify(envelope));
  }

  /** Fail the stream. `unauthorized` mirrors a real HTTP error, not a blip. */
  fail(unauthorized: boolean): void {
    this.readyState = unauthorized
      ? FakeEventSource.CLOSED
      : FakeEventSource.CONNECTING;
    this.onerror?.(new Event("error"));
  }
}

/** Hands out `FakeEventSource`s and remembers every one it made. */
export class EventSourceSpy {
  readonly created: FakeEventSource[] = [];

  readonly factory = (url: string): EventSourceLike => {
    const source = new FakeEventSource(url);
    this.created.push(source);
    return source;
  };

  get last(): FakeEventSource {
    const source = this.created.at(-1);
    if (!source) throw new Error("no EventSource was created");
    return source;
  }
}

/** A plausible `AgentSummary`; override whatever a test cares about. */
export function agent(over: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "agent-1",
    name: "ship it",
    harness: "claude_code",
    harness_label: "Claude Code",
    status: "running",
    cwd: "/srv/work",
    model: null,
    permission: "ask",
    tmux_session: "jod-agent-1",
    attach_command: "tmux attach -t jod-agent-1",
    switch_command: "tmux switch-client -t jod-agent-1",
    session_closed: false,
    created_at_ms: 1_700_000_000_000,
    session_id: null,
    usage: {},
    event_count: 0,
    last_message: null,
    stream_path: "/root/.jod/runs/agent-1/stream.jsonl",
    ...over,
  };
}

/** Let queued promises settle — the store's fire-and-forget work. */
export async function settle(times = 3): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}
