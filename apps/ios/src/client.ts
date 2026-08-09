/**
 * Talks to `jod-api` over REST + SSE.
 *
 * The contract is owned by the `api/` crate. The points this client leans on,
 * all of them load-bearing:
 *
 * - every route is `/v1`, JSON, and **relative** — the daemon binds loopback
 *   and is reached over Tailscale, so hard-coding an origin would break the
 *   only deployment that exists;
 * - auth is a cookie session minted by `POST /v1/session` from a bearer token,
 *   because `EventSource` cannot set an `Authorization` header;
 * - errors are RFC 9457 `application/problem+json`, and `detail` is the part a
 *   human should read;
 * - `GET /v1/agents/{id}/events` returns `{events, last_seq}`, not a bare array.
 *
 * ## Why the per-agent stream, not `/v1/events`
 *
 * The HUD in `apps/web` watches the whole fleet, so it subscribes to the global
 * feed. A conversation watches exactly one delegation, and the per-agent stream
 * is the one the API made resumable: it **replays history first, then goes
 * live**, and it emits `id:` so the browser's own `Last-Event-ID` resume works
 * across the tunnel drops a phone gets constantly — walking out of wifi, or the
 * screen locking. Subscribing to the global feed and filtering client-side
 * would throw that away and re-introduce the gap between "read history" and
 * "start listening" that `sse.rs` is explicitly written to avoid.
 *
 * That gap is why this client does **not** backfill before connecting. The
 * stream does it, atomically. `events()` exists for one narrower job: catching
 * up a conversation whose stream was closed while the app was backgrounded.
 *
 * ## Testability
 *
 * `fetch` and the `EventSource` constructor are injected rather than reached
 * for globally, so every path here — including the failure paths that matter
 * most, a 401 mid-stream and a `lagged` frame — is exercised headless in
 * `test/client.test.ts`. iOS Safari is where this *runs*; it is not where it
 * has to be *tested*.
 */

import type {
  AgentEnvelope,
  AgentSummary,
  HarnessInfo,
  Report,
  Resume,
  TeamView,
} from "./contract";
import type { HarnessKind, PermissionPolicy } from "./contract";

/** A token's authority. An absent scope is treated as `read` — fail safe. */
export type Scope = "read" | "write";

export interface SessionInfo {
  scope: Scope;
  expires_at_ms: number;
}

/** What `POST /v1/agents` accepts. Everything but `prompt` has a server default. */
export interface SpawnBody {
  prompt: string;
  harness?: HarnessKind;
  name?: string;
  /** Omitted means the daemon's first allowed root — the one-project phone case. */
  cwd?: string;
  model?: string;
  permission?: PermissionPolicy;
  resume?: Resume;
}

/** `GET /v1/agents/{id}/events` — a page, plus the cursor for the next one. */
export interface EventsPage {
  events: AgentEnvelope[];
  last_seq: number | null;
}

/** Raised for 401/403 so the UI can show the re-auth gate instead of an error. */
export class UnauthorizedError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "UnauthorizedError";
  }
}

/**
 * The subset of `EventSource` this client uses, so a fake can stand in.
 *
 * `onerror` is typed with the DOM's own `Event` rather than something looser:
 * it is a mutable property, so TypeScript checks it invariantly and a widened
 * parameter would make the real `EventSource` fail to satisfy this interface.
 */
export interface EventSourceLike {
  addEventListener(type: string, listener: (event: MessageEvent) => void): void;
  close(): void;
  onerror: ((event: Event) => void) | null;
  readonly readyState: number;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

export interface ClientOptions {
  /** Prefix for every route. Empty means same-origin, which is the deployment. */
  base?: string;
  fetch?: typeof fetch;
  eventSource?: EventSourceFactory;
  /** Injected so idempotency keys are deterministic under test. */
  newKey?: () => string;
}

/** `EventSource.CLOSED`. Named rather than magic, and not read off the global. */
const CLOSED = 2;

export class JodClient {
  /**
   * Prefix for every route. Empty means same-origin.
   *
   * Mutable because the packaged app cannot know it at construction: the shell
   * serves its assets from `tauri://localhost`, so the daemon's address is a
   * setting the user supplies. See `origin.ts`.
   */
  private base: string;
  private readonly doFetch: typeof fetch;
  private readonly makeEventSource: EventSourceFactory;
  private readonly newKey: () => string;

  constructor(options: ClientOptions = {}) {
    this.base = options.base ?? "";
    this.doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.makeEventSource =
      options.eventSource ??
      ((url: string) => new EventSource(url, { withCredentials: true }));
    this.newKey = options.newKey ?? defaultKey;
  }

  /** Point this client at a daemon. `""` means same-origin. */
  setBase(base: string): void {
    this.base = base;
  }

  // ─── REST ────────────────────────────────────────────────────────────────

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await this.doFetch(`${this.base}${path}`, {
      // The credential is a cookie, so every call must carry it. On iOS this
      // also has to survive the app being resumed from the background.
      credentials: "include",
      ...init,
      headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
    });

    if (res.status === 401 || res.status === 403) {
      throw new UnauthorizedError(await problemDetail(res), res.status);
    }
    if (!res.ok) throw new Error(await problemDetail(res));
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }

  /**
   * Exchange a bearer token for a session cookie.
   *
   * The token is handed over once and **never stored by this app**. From here
   * the cookie is the credential, which is the whole reason to do this dance:
   * a token in `localStorage` on a phone is a token in every script that ever
   * runs on the page.
   */
  async authenticate(token: string): Promise<SessionInfo> {
    const info = await this.json<SessionInfo>("/v1/session", {
      method: "POST",
      headers: { authorization: `Bearer ${token.trim()}` },
    });
    return { ...info, scope: normaliseScope(info?.scope) };
  }

  async endSession(): Promise<void> {
    await this.json<void>("/v1/session", { method: "DELETE" });
  }

  async harnesses(): Promise<HarnessInfo[]> {
    return this.json<HarnessInfo[]>("/v1/harnesses");
  }

  async agents(): Promise<AgentSummary[]> {
    return this.json<AgentSummary[]>("/v1/agents");
  }

  async report(): Promise<Report> {
    return this.json<Report>("/v1/report");
  }

  /** Every team that has a member. Read scope is enough. */
  async teams(): Promise<string[]> {
    return this.json<string[]>("/v1/teams");
  }

  /**
   * One team's roster and board, in one answer.
   *
   * Deliberately one request rather than two: the sheet draws both together,
   * and a board from one moment against a roster from another is a screen that
   * was never true.
   */
  async team(name: string): Promise<TeamView> {
    return this.json<TeamView>(`/v1/teams/${encodeURIComponent(name)}`);
  }

  /**
   * Delegate a prompt.
   *
   * Carries an `Idempotency-Key` because this is the expensive, side-effecting
   * verb and a phone is the client most likely to retry one: a tap on a flaky
   * link, or iOS resuming a suspended request. Without the key that is two
   * agents running the same task in the same directory — the exact collision
   * the charter's one-owner-per-path rule exists to prevent.
   */
  async spawn(body: SpawnBody, key = this.newKey()): Promise<AgentSummary> {
    return this.json<AgentSummary>("/v1/agents", {
      method: "POST",
      headers: { "idempotency-key": key },
      body: JSON.stringify(body),
    });
  }

  async kill(id: string): Promise<void> {
    await this.json<void>(`/v1/agents/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
  }

  /**
   * Backfill one agent's events.
   *
   * `afterSeq` is **exclusive** and `seq` starts at 0, so "I have seen nothing"
   * must omit the parameter entirely. Passing 0 would skip `started` — the
   * event carrying `session_id` and `model`, and therefore the one that makes
   * the next turn continue this conversation instead of starting a new one.
   */
  async events(id: string, afterSeq?: number): Promise<EventsPage> {
    const q =
      afterSeq === undefined || afterSeq < 0
        ? ""
        : `?after_seq=${encodeURIComponent(String(afterSeq))}`;
    const page = await this.json<EventsPage | AgentEnvelope[]>(
      `/v1/agents/${encodeURIComponent(id)}/events${q}`,
    );
    // Tolerate a bare array, which is what an older daemon returns.
    if (Array.isArray(page)) {
      return { events: page, last_seq: page.at(-1)?.seq ?? null };
    }
    return { events: page.events ?? [], last_seq: page.last_seq ?? null };
  }

  // ─── SSE ─────────────────────────────────────────────────────────────────

  /**
   * Follow one delegation: history, then live, on one connection.
   *
   * Returns a function that closes the stream. Handlers are all optional so a
   * caller that only wants envelopes does not have to write empty stubs.
   *
   * `afterSeq` resumes: the server accepts it as a query parameter and, on the
   * browser's own automatic reconnect, as `Last-Event-ID`.
   */
  stream(
    id: string,
    handlers: {
      onEnvelope?: (envelope: AgentEnvelope) => void;
      /** The server's channel overflowed and dropped `missed` events. */
      onLagged?: (missed: number) => void;
      /** The stream died. `unauthorized` means the session expired. */
      onError?: (reason: { unauthorized: boolean }) => void;
    },
    afterSeq?: number,
  ): () => void {
    const q =
      afterSeq === undefined || afterSeq < 0
        ? ""
        : `?after_seq=${encodeURIComponent(String(afterSeq))}`;
    const source = this.makeEventSource(
      `${this.base}/v1/agents/${encodeURIComponent(id)}/stream${q}`,
    );

    source.addEventListener("agent", (event: MessageEvent) => {
      const envelope = parseEnvelope(event.data);
      if (envelope) handlers.onEnvelope?.(envelope);
    });

    source.addEventListener("lagged", (event: MessageEvent) => {
      const missed = parseMissed(event.data);
      if (missed > 0) handlers.onLagged?.(missed);
    });

    source.onerror = () => {
      // `EventSource` retries on its own and only reaches CLOSED when the
      // server answered with a real HTTP error — which, for this API, means
      // the session is gone. Anything else is a transient drop it will fix,
      // and reporting it would make a phone in a lift look broken.
      handlers.onError?.({ unauthorized: source.readyState === CLOSED });
    };

    return () => source.close();
  }
}

/** Anything that is not explicitly `"write"` is read-only. Fail safe. */
export function normaliseScope(scope: unknown): Scope {
  return scope === "write" ? "write" : "read";
}

/**
 * The human half of an RFC 9457 problem document.
 *
 * A body that is not the expected shape must still produce something readable —
 * a bare status code beats "undefined" on a screen the user cannot debug from.
 */
export async function problemDetail(res: {
  status: number;
  statusText?: string;
  json: () => Promise<unknown>;
}): Promise<string> {
  try {
    const body = (await res.json()) as Record<string, unknown> | null;
    for (const field of ["detail", "title", "message", "error"] as const) {
      const value = body?.[field];
      if (typeof value === "string" && value.trim() !== "") return value;
    }
  } catch {
    // Not JSON, or the body was already consumed. Fall through.
  }
  return res.statusText ? `${res.status} ${res.statusText}` : `HTTP ${res.status}`;
}

/**
 * One `data:` payload as an envelope, or `null` if it is not one.
 *
 * A frame that cannot be parsed is dropped rather than thrown, because one
 * malformed line must not take down a live conversation.
 */
export function parseEnvelope(data: unknown): AgentEnvelope | null {
  if (typeof data !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const candidate = parsed as Partial<AgentEnvelope>;
  // The envelope is flattened: `kind` sits beside the event's own fields.
  if (typeof candidate.kind !== "string") return null;
  if (typeof candidate.seq !== "number") return null;
  return candidate as AgentEnvelope;
}

/** `{"missed":12}` → 12. Anything unreadable counts as zero. */
export function parseMissed(data: unknown): number {
  if (typeof data !== "string") return 0;
  try {
    const parsed = JSON.parse(data) as { missed?: unknown };
    return typeof parsed?.missed === "number" ? parsed.missed : 0;
  } catch {
    return 0;
  }
}

function defaultKey(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  // `randomUUID` needs a secure context. A phone on plain http over the tailnet
  // is not one, and a spawn that throws there would be worse than a weaker key.
  const bytes = new Uint8Array(16);
  if (c && typeof c.getRandomValues === "function") c.getRandomValues(bytes);
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}
