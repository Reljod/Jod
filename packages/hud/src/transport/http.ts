import type {
  AgentEnvelope,
  AgentSummary,
  ConversationSummary,
  FleetNode,
  HarnessInfo,
  Message,
  Report,
  SpawnRequest,
  StoredRun,
} from "../types";
import { fleetKey } from "../types";
import { NO_FLEET } from "./index";
import type { Fleet, Scope, Transport, TransportHandlers, WorkDeletion } from "./index";

/** How many events one backfill request asks for. The route caps its own page. */
export const EVENT_PAGE = 500;

/** `/v1/fleet`'s body, exactly as `api::workspaces::FleetPage` serialises it. */
interface FleetPage {
  nodes?: FleetNode[];
  run_of?: Record<string, string>;
}

/**
 * Talks to `jod-api` over REST + SSE.
 *
 * Contract owner: the sibling session building the `api/` crate. Frozen points
 * this driver depends on —
 *
 *   · every route is `/v1`, JSON, same-origin (the daemon binds loopback and is
 *     reached remotely over Tailscale, so paths here are relative on purpose)
 *   · the live feed is SSE at `/v1/events`, `event: agent`, and each `data:` is
 *     one *flattened* `AgentEnvelope` — `kind` sits beside the event's own
 *     fields, not nested under a payload key
 *   · errors are RFC 9457 `application/problem+json`; `detail` is the human part
 *
 * Two deliberate defences against the contract moving:
 *
 *   1. `dispatch` still accepts a wrapped `{type,data}` frame, because that
 *      costs nothing and an unrecognised frame otherwise renders as nothing.
 *   2. Every envelope is deduped on `(agent_id, seq)`. The global stream tags
 *      frames with a *per-agent* `seq`, so `Last-Event-ID` resume can legally
 *      replay events this client has already drawn. Deduping locally means a
 *      replay is idempotent no matter how the server resolves that.
 */
export class HttpTransport implements Transport {
  readonly label = "HTTP";

  private handlers: TransportHandlers | null = null;
  private sse: EventSource | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private reconcileTimer: ReturnType<typeof setTimeout> | null = null;
  private stopped = false;
  private attempt = 0;
  /**
   * Highest seq already rendered per agent — the dedupe key.
   *
   * `seq` starts at **0**, so "nothing seen yet" must be -1. Defaulting to 0
   * would silently swallow every agent's `started` event, and with it the
   * `session_id` and `model` it carries.
   */
  private lastSeq = new Map<string, number>();
  private scope: Scope = "read";
  /** Aborts the fetch-based stream. Only used on the bearer-token path. */
  private streaming: AbortController | null = null;

  /**
   * `token`, when given, switches this driver from cookie auth to `Authorization:
   * Bearer` on every request — including the event stream, which then runs on
   * `fetch` instead of `EventSource`.
   *
   * The desktop shell needs this. It is served from the API's own origin, but
   * the session cookie is marked `Secure`, and whether a webview honours a
   * `Secure` cookie over `http://127.0.0.1` differs between WebKitGTK, WKWebView
   * and Chromium. Depending on that would be depending on the platform. A header
   * is carried identically everywhere.
   */
  constructor(
    private readonly base = "",
    private readonly token?: string,
  ) {}

  /** Auth that travels on every request, or nothing on the cookie path. */
  private authHeaders(): Record<string, string> {
    return this.token ? { Authorization: `Bearer ${this.token}` } : {};
  }

  start(handlers: TransportHandlers): void {
    this.handlers = handlers;
    this.stopped = false;
    handlers.onLink({ phase: "probing" });
    void this.bootstrap();
  }

  stop(): void {
    this.stopped = true;
    this.sse?.close();
    this.sse = null;
    this.streaming?.abort();
    this.streaming = null;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    if (this.reconcileTimer) clearTimeout(this.reconcileTimer);
    this.retryTimer = null;
    this.reconcileTimer = null;
  }

  // ─── REST ────────────────────────────────────────────────────────────────

  private url(path: string): string {
    return `${this.base}${path}`;
  }

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await fetch(this.url(path), {
      // Auth is a cookie session, so every call must carry credentials.
      credentials: "include",
      ...init,
      // After the spread, so a caller's headers extend these rather than
      // replacing them — dropping the bearer token here would 401 every write.
      headers: {
        "content-type": "application/json",
        ...this.authHeaders(),
        ...(init?.headers as Record<string, string> | undefined),
      },
    });
    if (res.status === 401 || res.status === 403) {
      throw new UnauthorizedError(await problemDetail(res, path), res.status);
    }
    if (!res.ok) throw new Error(await problemDetail(res, path));
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }

  /**
   * `/v1/agents` returns the roster and `/v1/report` the counts and spend, so
   * the `Report` the HUD wants is assembled from both.
   */
  async report(): Promise<Report> {
    const [agents, counts] = await Promise.all([
      this.json<AgentSummary[] | Report>("/v1/agents"),
      this.json<Omit<Report, "agents">>("/v1/report").catch(() => null),
    ]);

    // Tolerate `/v1/agents` returning a full Report instead of a bare array.
    if (!Array.isArray(agents)) return agents;

    return {
      running: counts?.running ?? agents.filter((a) => a.status === "running").length,
      completed: counts?.completed ?? agents.filter((a) => a.status === "completed").length,
      failed: counts?.failed ?? agents.filter((a) => a.status === "failed").length,
      killed: counts?.killed ?? agents.filter((a) => a.status === "killed").length,
      total_cost_usd:
        counts?.total_cost_usd ??
        agents.reduce((n, a) => n + (a.usage?.cost_usd ?? 0), 0),
      agents,
    };
  }

  async harnesses(): Promise<HarnessInfo[]> {
    return this.json<HarnessInfo[]>("/v1/harnesses");
  }

  /**
   * The fleet tree, straight from `Store::fleet` — the same rows the TUI
   * renders, folded by the same function, with no second implementation on
   * either side of the wire.
   *
   * Both payload shapes are accepted. The route answers with
   * `{nodes, run_of}` now; it used to answer with a bare array of rows, and a
   * daemon older than this build still does. Reading the array as an object
   * would not fail loudly — `body.nodes` is simply `undefined` — so the fleet
   * would go quietly empty against a server that was answering perfectly well.
   *
   * A failure is an empty fleet rather than a thrown error. This panel sits
   * beside the live stream and must not be able to take the HUD down with it;
   * an older daemon without the route at all is exactly the case that would.
   */
  async fleet(): Promise<Fleet> {
    try {
      const page = await this.json<FleetNode[] | FleetPage>("/v1/fleet");
      // An unfolded forest still carries its runs as rows, so the map it did
      // not send can be read off them: a run row stands for itself. Built here
      // rather than allowed for in the panel, because normalising the wire into
      // the shape the app expects is exactly this layer's job — and a panel
      // that has to know which kind of server answered is a panel that will
      // eventually get it wrong.
      if (Array.isArray(page)) {
        return {
          nodes: page,
          runOf: new Map(
            page.filter((n) => n.kind === "run").map((n) => [fleetKey(n.id), n.id.id]),
          ),
        };
      }
      return {
        nodes: page?.nodes ?? [],
        runOf: new Map(Object.entries(page?.run_of ?? {})),
      };
    } catch {
      return NO_FLEET;
    }
  }

  /** Not in the v1 contract yet; the store exists in core, so ask and shrug. */
  async history(limit: number): Promise<StoredRun[]> {
    try {
      return await this.json<StoredRun[]>(`/v1/history?limit=${limit}`);
    } catch {
      return [];
    }
  }

  /**
   * `after_seq` is an *exclusive* cursor over a sequence that starts at 0, so
   * `?after_seq=0` means "everything after event 0" and skips `started`.
   * Omitting the parameter entirely is what returns seq 0 onward.
   *
   * The route answers with an `EventsPage` — `{events, last_seq}` — not a bare
   * array. Unwrapped here, and both shapes accepted, because reading it as an
   * array does not fail loudly: `for…of` on the page object throws inside the
   * per-agent `catch` that exists so one bad agent cannot abort a backfill, so
   * the whole lag-recovery path went quiet instead of visibly breaking.
   */
  async events(agentId: string, sinceSeq?: number): Promise<AgentEnvelope[]> {
    const path = `/v1/agents/${encodeURIComponent(agentId)}/events`;
    const query =
      sinceSeq === undefined
        ? `?limit=${EVENT_PAGE}`
        : `?after_seq=${sinceSeq}&limit=${EVENT_PAGE}`;
    const page = await this.json<AgentEnvelope[] | { events?: AgentEnvelope[] }>(path + query);
    if (Array.isArray(page)) return page;
    return page?.events ?? [];
  }

  /**
   * Recent conversations. Absent on a daemon with no store, which is a state to
   * render as "no transcript" rather than an error to raise.
   */
  async conversations(limit: number): Promise<ConversationSummary[]> {
    return this.json<ConversationSummary[]>(`/v1/conversations?limit=${limit}`);
  }

  async messages(conversationId: string): Promise<Message[]> {
    return this.json<Message[]>(
      `/v1/conversations/${encodeURIComponent(conversationId)}/messages`,
    );
  }

  /**
   * Exchange a bearer token for an `HttpOnly` session cookie.
   *
   * `EventSource` cannot set an `Authorization` header, which is the whole
   * reason this endpoint exists. The token is never stored client-side — the
   * cookie is the credential from here on.
   */
  async authenticate(token: string): Promise<Scope> {
    const res = await fetch(this.url("/v1/session"), {
      method: "POST",
      credentials: "include",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) throw new Error(await problemDetail(res, "/v1/session"));
    const body = (await res.json().catch(() => ({}))) as { scope?: Scope };
    // An absent scope is treated as read — fail safe, never fail open.
    this.scope = body.scope === "write" ? "write" : "read";
    this.attempt = 0;
    void this.bootstrap();
    return this.scope;
  }

  async spawn(request: SpawnRequest): Promise<AgentSummary | null> {
    // `resume` is externally tagged and matches core's serde exactly, so the
    // request goes on the wire unchanged. Omitted entirely it defaults to
    // "fresh", which is what a one-shot task wants.
    const body: SpawnRequest = { ...request };
    if (!body.resume || body.resume === "fresh") delete body.resume;

    const created = await this.json<AgentSummary>("/v1/agents", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        // Retrying a spawn must never launch a second agent.
        "Idempotency-Key": idempotencyKey(),
      },
      body: JSON.stringify(body),
    });
    void this.refreshReport();
    return created;
  }

  async kill(agentId: string): Promise<void> {
    await this.json<void>(`/v1/agents/${encodeURIComponent(agentId)}`, {
      method: "DELETE",
    });
    void this.refreshReport();
  }

  /**
   * `/v1/runs/{id}`, not `/v1/agents/{id}` — the second is the kill.
   *
   * No report refresh: the roster is rebuilt from the daemon's memory, which
   * this call has just removed the run from, and the caller has already dropped
   * it locally. Refreshing would be a round trip to confirm an absence.
   */
  async deleteRun(agentId: string): Promise<void> {
    await this.json<void>(`/v1/runs/${encodeURIComponent(agentId)}`, {
      method: "DELETE",
    });
  }

  async deleteConversation(conversationId: string): Promise<void> {
    await this.json<void>(`/v1/conversations/${encodeURIComponent(conversationId)}`, {
      method: "DELETE",
    });
  }

  /**
   * The one call whose refusal is a *body*, not an error.
   *
   * A work holding worktrees answers 409 with everything the delete would take,
   * which is what a confirmation dialog is made of. `json` throws on any
   * non-2xx, so this reads the response itself and only falls back to that
   * behaviour for a status it cannot interpret — a 403, a 404, a proxy's 502.
   */
  async deleteWork(workId: string): Promise<WorkDeletion> {
    const path = `/v1/works/${encodeURIComponent(workId)}`;
    const res = await fetch(this.url(path), {
      method: "DELETE",
      credentials: "include",
      headers: { "content-type": "application/json", ...this.authHeaders() },
    });
    if (res.status === 401 || res.status === 403) {
      throw new UnauthorizedError(await problemDetail(res, path), res.status);
    }
    if (res.ok || res.status === 409) {
      return (await res.json()) as WorkDeletion;
    }
    throw new Error(await problemDetail(res, path));
  }

  // ─── live feed ───────────────────────────────────────────────────────────

  private async bootstrap(): Promise<void> {
    if (this.stopped) return;
    try {
      // Before the first authorised call, so the roster is fetched with a scope
      // already known and the UI never briefly offers writes it cannot make.
      await this.learnScope();
      this.handlers?.onReport(await this.report());
      this.attempt = 0;
      this.handlers?.onLink({
        phase: "live",
        origin: this.base || location.origin,
        scope: this.scope,
      });
      this.openStream();
    } catch (err) {
      // A 401 is not a transport failure to retry into — it needs a human to
      // present a token, so surface it instead of reconnecting forever.
      if (err instanceof UnauthorizedError) {
        this.handlers?.onLink({ phase: "auth", reason: err.message });
        return;
      }
      this.scheduleRetry(err instanceof Error ? err.message : String(err));
    }
  }

  private async refreshReport(): Promise<void> {
    try {
      this.handlers?.onReport(await this.report());
    } catch {
      /* the stream keeps the HUD alive; a failed refresh is not fatal */
    }
  }

  /**
   * The roster carries state the event stream cannot reconstruct — most
   * importantly `process_alive`, which is a live probe of the run's process
   * group rather than anything the event stream carries.
   * Rather than poll, refresh it only when the stream says a lifecycle
   * boundary was crossed, debounced so a burst costs one request.
   */
  private scheduleReconcile(): void {
    if (this.reconcileTimer || this.stopped) return;
    this.reconcileTimer = setTimeout(() => {
      this.reconcileTimer = null;
      void this.refreshReport();
    }, 400);
  }

  /**
   * What a bearer token is allowed to do.
   *
   * There is no "describe this token" route, but `POST /v1/session` answers with
   * the scope as a side effect of minting a cookie. The cookie may or may not
   * survive (`Secure`, over loopback http) — it does not matter, because every
   * request carries the header anyway. The scope is what we came for, and the
   * HUD disables its write controls without it.
   */
  private async learnScope(): Promise<void> {
    if (!this.token) return;
    try {
      const res = await fetch(this.url("/v1/session"), {
        method: "POST",
        credentials: "include",
        headers: this.authHeaders(),
      });
      if (!res.ok) return;
      const body = (await res.json().catch(() => ({}))) as { scope?: Scope };
      this.scope = body.scope === "write" ? "write" : "read";
    } catch {
      /* fail safe: stays `read`, and the UI offers no write it cannot make */
    }
  }

  /**
   * The event stream, for the bearer-token path.
   *
   * `EventSource` cannot set an `Authorization` header — the single fact the
   * whole cookie exchange exists to work around. Reading the same `text/event-
   * stream` off `fetch` can, at the cost of hand-rolling the framing and the
   * reconnect that `EventSource` gives away.
   *
   * Frames are separated by a blank line and `data:` may repeat within one, so
   * the buffer is split on the boundary rather than by line.
   */
  private async openFetchStream(): Promise<void> {
    if (this.stopped || this.streaming) return;
    const ac = new AbortController();
    this.streaming = ac;

    try {
      const res = await fetch(this.url("/v1/events"), {
        headers: { ...this.authHeaders(), accept: "text/event-stream" },
        signal: ac.signal,
      });
      if (res.status === 401 || res.status === 403) {
        this.handlers?.onLink({
          phase: "auth",
          reason: await problemDetail(res, "/v1/events"),
        });
        return;
      }
      if (!res.ok || !res.body) throw new Error(await problemDetail(res, "/v1/events"));

      this.attempt = 0;
      this.handlers?.onLink({
        phase: "live",
        origin: this.base || location.origin,
        scope: this.scope,
      });

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";

      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        let boundary = buffer.indexOf("\n\n");
        while (boundary !== -1) {
          this.handleFrame(buffer.slice(0, boundary));
          buffer = buffer.slice(boundary + 2);
          boundary = buffer.indexOf("\n\n");
        }
      }
      // A clean end of body is still a lost stream: the server closed it.
      if (!this.stopped) {
        this.streaming = null;
        this.scheduleRetry("event stream ended");
      }
    } catch (err) {
      this.streaming = null;
      // An abort is us calling `stop()`, not a failure to report.
      if (ac.signal.aborted || this.stopped) return;
      this.scheduleRetry(err instanceof Error ? err.message : String(err));
    }
  }

  /** One `event:`/`data:` frame, already split off at the blank line. */
  private handleFrame(frame: string): void {
    const parsed = parseSseFrame(frame);
    if (parsed === null) return;

    if (parsed.event === "lagged") {
      this.onLagged(parsed.data);
      return;
    }
    this.ingest(parsed.data);
  }

  /** The server dropped broadcasts — admit the hole and backfill it. */
  private onLagged(payload: string): void {
    let missed = 0;
    try {
      missed = Number(JSON.parse(payload)?.missed ?? 0);
    } catch {
      /* the count is advisory; the backfill is the point */
    }
    this.handlers?.onLink({
      phase: "lost",
      reason: `stream lagged — ${missed} event(s) missed, backfilling`,
      retryInMs: 0,
    });
    void this.backfillAll();
  }

  private openStream(): void {
    if (this.stopped || this.sse) return;
    // A bearer token cannot ride on `EventSource`; take the fetch path.
    if (this.token) {
      void this.openFetchStream();
      return;
    }
    try {
      // `withCredentials` carries the session cookie; EventSource cannot set an
      // Authorization header, which is why the cookie exchange exists at all.
      const sse = new EventSource(this.url("/v1/events"), { withCredentials: true });
      this.sse = sse;

      // The server tags frames `event: agent`, so a bare `onmessage` would
      // never fire. Listen for both, and treat any unnamed frame as an event.
      sse.addEventListener("agent", (ev) => this.ingest((ev as MessageEvent).data));
      // The server dropped broadcast messages: this HUD now has a hole, and
      // showing stale state confidently is worse than admitting it. Backfill.
      sse.addEventListener("lagged", (ev) =>
        this.onLagged(String((ev as MessageEvent).data)),
      );
      sse.onmessage = (ev) => this.ingest(ev.data);
      sse.onerror = () => {
        // EventSource reconnects itself while readyState is CONNECTING; only
        // a hard CLOSED needs us to intervene.
        if (sse.readyState === EventSource.CLOSED) {
          this.sse = null;
          this.scheduleRetry("event stream closed");
        } else {
          this.handlers?.onLink({
            phase: "lost",
            reason: "stream interrupted — resuming",
            retryInMs: 0,
          });
        }
      };
      sse.onopen = () => {
        this.attempt = 0;
        this.handlers?.onLink({
          phase: "live",
          origin: this.base || location.origin,
          scope: this.scope,
        });
      };
    } catch (err) {
      this.scheduleRetry(err instanceof Error ? err.message : String(err));
    }
  }

  /**
   * Re-read every known agent from its last rendered seq.
   *
   * `/v1/events` is documented live-only and issues no `id:`, so there is no
   * cursor to resume the aggregate stream with. Per-agent backfill is the
   * documented recovery path, and the `(agent_id, seq)` dedupe makes replaying
   * an overlap harmless.
   */
  private async backfillAll(): Promise<void> {
    try {
      const report = await this.report();
      this.handlers?.onReport(report);

      await Promise.all(
        report.agents.map(async (agent) => {
          const seen = this.lastSeq.get(agent.id);
          try {
            for (const env of await this.events(agent.id, seen)) this.dispatch(env);
          } catch {
            /* one agent failing to backfill must not abort the rest */
          }
        }),
      );

      this.handlers?.onLink({
        phase: "live",
        origin: this.base || location.origin,
        scope: this.scope,
      });
    } catch {
      /* the retry loop will pick this up */
    }
  }

  private ingest(raw: unknown): void {
    if (typeof raw !== "string") return;
    try {
      this.dispatch(JSON.parse(raw));
    } catch {
      /* a malformed frame is dropped, never thrown */
    }
  }

  private dispatch(msg: unknown): void {
    if (Array.isArray(msg)) {
      for (const m of msg) this.dispatch(m);
      return;
    }
    if (!msg || typeof msg !== "object") return;
    const rec = msg as Record<string, unknown>;

    // The contract shape: a flattened AgentEnvelope.
    if (typeof rec.kind === "string" && typeof rec.agent_id === "string") {
      const env = msg as AgentEnvelope;
      // -1, not 0: seq starts at 0, so `?? 0` would drop every `started`.
      const seen = this.lastSeq.get(env.agent_id) ?? -1;
      if (env.seq <= seen) return; // replayed or backfilled — already drawn
      this.lastSeq.set(env.agent_id, env.seq);

      this.handlers?.onEnvelope(env);
      if (env.kind === "finished" || env.kind === "started" || env.kind === "error") {
        this.scheduleReconcile();
      }
      return;
    }

    if (rec.report && typeof rec.report === "object") {
      this.handlers?.onReport(rec.report as Report);
      return;
    }
    const inner = (rec.data ?? rec.payload ?? rec.event ?? rec.envelope) as unknown;
    if (inner) this.dispatch(inner);
  }

  private scheduleRetry(reason: string): void {
    if (this.stopped) return;
    this.attempt += 1;
    const retryInMs = Math.min(15000, 750 * 2 ** Math.min(this.attempt, 5));
    this.handlers?.onLink({ phase: "lost", reason, retryInMs });
    this.retryTimer = setTimeout(() => void this.bootstrap(), retryInMs);
  }
}

/**
 * Parse one server-sent-events frame.
 *
 * Exported because it is the only piece of wire protocol this client hand-rolls.
 * `EventSource` does this internally and is unavailable on the bearer-token
 * path, so these rules are re-implemented here and worth pinning in tests:
 *
 *   · `data:` may repeat within a frame and the parts join with newlines
 *   · a line starting `:` is a comment — servers send them as keep-alives
 *   · exactly one leading space after the colon is framing, not payload, so
 *     `data:  {}` carries a value that begins with a space
 *   · a field with no colon at all is a name with an empty value
 *   · no `data:` line means nothing to dispatch, which is not an error
 *
 * Returns `null` when the frame carries no data.
 */
export function parseSseFrame(frame: string): { event: string; data: string } | null {
  let event = "message";
  const data: string[] = [];

  for (const line of frame.split("\n")) {
    if (line.startsWith(":")) continue;
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    const value = colon === -1 ? "" : line.slice(colon + 1).replace(/^ /, "");
    if (field === "event") event = value;
    else if (field === "data") data.push(value);
  }

  if (!data.length) return null;
  return { event, data: data.join("\n") };
}

/** No valid session, or one without the authority for this call. */
export class UnauthorizedError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "UnauthorizedError";
  }
}

/** RFC 9457 problem details — surface `detail`, which is the human-readable bit. */
async function problemDetail(res: Response, path: string): Promise<string> {
  try {
    const body = (await res.json()) as { detail?: string; title?: string };
    if (body.detail) return body.detail;
    if (body.title) return body.title;
  } catch {
    /* not problem+json */
  }
  return `${res.status} ${res.statusText} — ${path}`;
}

function idempotencyKey(): string {
  return crypto.randomUUID();
}

/** Is an orchestrator reachable at all? `/v1/health` is unauthenticated. */
export async function probeApi(base = "", timeoutMs = 1500): Promise<boolean> {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const res = await fetch(`${base}/v1/health`, { signal: ac.signal });
    return res.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}
