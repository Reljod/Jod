import type {
  AgentEnvelope,
  AgentSummary,
  HarnessInfo,
  Report,
  SpawnRequest,
  StoredRun,
} from "../types";
import type { Scope, Transport, TransportHandlers } from "./index";

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

  constructor(private readonly base = "") {}

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
      headers: { "content-type": "application/json" },
      ...init,
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
   */
  async events(agentId: string, sinceSeq?: number): Promise<AgentEnvelope[]> {
    const path = `/v1/agents/${encodeURIComponent(agentId)}/events`;
    const query = sinceSeq === undefined ? "?limit=500" : `?after_seq=${sinceSeq}&limit=500`;
    return this.json<AgentEnvelope[]>(path + query);
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

  // ─── live feed ───────────────────────────────────────────────────────────

  private async bootstrap(): Promise<void> {
    if (this.stopped) return;
    try {
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
   * importantly `session_closed`, since a tmux session outlives its agent.
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

  private openStream(): void {
    if (this.stopped || this.sse) return;
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
      sse.addEventListener("lagged", (ev) => {
        const data = (ev as MessageEvent).data;
        let missed = 0;
        try {
          missed = Number(JSON.parse(String(data))?.missed ?? 0);
        } catch {
          /* the count is advisory; the backfill is the point */
        }
        this.handlers?.onLink({
          phase: "lost",
          reason: `stream lagged — ${missed} event(s) missed, backfilling`,
          retryInMs: 0,
        });
        void this.backfillAll();
      });
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
