# The Jod API

Roadmap item 7 — *"headless daemon for a VPS: the same `jod-core` behind an
authenticated API"* — and the thing every non-desktop client is blocked on. This
document is the contract, and the argument for why it is shaped this way.

Reasoning that generalises beyond this crate belongs in
[`decisions.md`](decisions.md); this file carries the API's own.

## What it is

`jod-api` is a thin HTTP shell over [`service::Jod`](jod-system.md#pillar-3--jod-the-orchestrator).
It adds no orchestration logic of its own — every route is a method call on the
same struct the CLI and the desktop app already drive. That is deliberate: the
moment the API grows its own idea of what an agent is, three clients start
disagreeing about it.

```
   iOS / Android ─┐
   web app       ─┼─ HTTPS (tailnet) ─► jod-api ─► service::Jod ─► tmux ─► harness
   curl          ─┘                     (127.0.0.1)
```

## The one thing to understand first

**A credential for this API is arbitrary code execution on the box.**

Not "access to some data" — the API's whole purpose is to spawn agent harnesses,
and an agent harness runs shell commands. `POST /v1/agents` with a `bypass`
permission policy is a remote shell with extra steps.

Every decision below follows from taking that sentence literally. It is also why
this API is not, and should not be, on the public internet in its current form.
→ [why](#the-api-is-not-on-the-public-internet)

## Transport: REST + SSE, not WebSocket

The traffic here is lopsided. A client sends a handful of small commands — spawn,
kill — and receives a firehose of events. That is the exact shape server-sent
events were designed for, and the exact shape WebSocket's bidirectional framing
is overkill for.

| | SSE | WebSocket |
|---|---|---|
| Reconnect | automatic, in the client | hand-rolled |
| Resume after a drop | `Last-Event-ID`, in the protocol | hand-rolled |
| Proxies / TLS termination | plain HTTP, works everywhere | upgrade must be preserved |
| Auth | ordinary request headers | header support is uneven in browsers |
| Cost of the rare command | one small POST | already-open socket |

Mobile networks drop connections constantly, so resumability is not a nicety —
it is the feature. A phone that backgrounds for ninety seconds and comes back
must not lose an agent's output, and must not re-download an hour of it either.

The standard objection to resumable SSE is
[write amplification](https://zknill.io/posts/everyone-said-sse-token-streaming-was-easy/):
to replay a stream you must persist every token, which is ruinous when an event
is a five-character delta wrapped in 125 characters of metadata. **That
objection does not apply here.** Jod's events are already coarse — one per tool
call, one per message, not one per token — and `core/src/store.rs` already
persists them with a `UNIQUE(run_id, seq)` index. Resumability is a `WHERE seq >
?` over rows that exist regardless. We get the expensive part for free because
the orchestrator was already durable.

Should Jod ever stream token-level deltas, revisit this.

### How resume actually works

`AgentEnvelope.seq` is a monotonic per-agent sequence number that already exists
in `jod-core`. The SSE `id:` field carries it verbatim.

On connect — and this ordering is load-bearing:

1. **Subscribe to the live broadcast first.** Not second. The CLI does the same
   thing before spawning, for the same reason: anything that subscribes after
   reading history drops every event that lands in between.
2. Read history from the store, keeping only `seq > after_seq`.
3. Emit history, remembering the highest `seq` sent.
4. Switch to the live subscription, discarding anything already emitted.

Steps 1 and 4 are what make the seam invisible. Without the dedupe in 4 a client
sees duplicates; without the subscribe in 1 it sees a hole. A hole is worse: a
duplicate is a rendering bug, a hole is a lost tool call.

Two ways in, same semantics:

- `Last-Event-ID: 41` — sent automatically by any `EventSource` on reconnect.
- `?after_seq=41` — explicit, for clients that persisted their position across a
  cold start. A native app that was killed by the OS has no `EventSource` state
  to reconnect with, only a number it wrote down.

`Last-Event-ID` wins when both are present, because it is the one the runtime
sets and the one that reflects what was actually received.

#### The cursor is absent, never zero

`seq` starts at **0**, and core's `events_since` is strictly exclusive
(`seq > after`). So "I have seen nothing" cannot be spelled `0` — that means "I
have seen event 0", and silently swallows the first event of every run. The
first event is `started`, which carries `session_id` and `model`, so the bug
presents as a client rendering a run that never began.

The cursor is therefore `Option<u64>` end to end. Omitted means everything, and
`?after_seq=0` genuinely means "after event 0". A regression test pins it
(`a_fresh_connection_does_not_suppress_seq_zero`), because the failure is quiet:
everything still works, one event just goes missing.

#### `/v1/events` is deliberately not resumable

`seq` is monotonic *per agent*, so a single cursor across all agents is
ambiguous: `42` means something different for every agent, and any one-cursor
replay would both double-send and skip. Rather than promise a resume that cannot
be honoured, the all-agents stream **omits the `id:` field entirely** — which
also stops a browser sending a meaningless `Last-Event-ID`. After a drop, a
client backfills per agent via `/v1/agents/{id}/events?after_seq=`.

When the channel drops messages, that stream emits an `event: lagged` frame
carrying the count, so a client learns it has a hole rather than believing it has
seen everything.

Giving `/v1/events` a real global cursor would mean surfacing the store's rowid —
which *is* globally monotonic — through `AgentEnvelope`. That is a `jod-core`
change, worth making only when a client actually needs it.

## Endpoints

All JSON, all under `/v1`. Errors are
[RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) `application/problem+json`.

| Method | Path | Scope | What |
|---|---|---|---|
| `GET` | `/v1/health` | none | Liveness. Deliberately says nothing else. |
| `GET` | `/v1/harnesses` | `read` | Which harnesses are installed, and where. |
| `GET` | `/v1/agents` | `read` | Every agent this daemon knows about. |
| `POST` | `/v1/agents` | `write` | Delegate a prompt. Returns the agent. |
| `GET` | `/v1/agents/{id}` | `read` | One agent. |
| `DELETE` | `/v1/agents/{id}` | `write` | Kill it, close its tmux session. |
| `GET` | `/v1/agents/{id}/events` | `read` | History, `?after_seq=&limit=`. |
| `GET` | `/v1/agents/{id}/stream` | `read` | SSE: that agent, live, resumable. |
| `GET` | `/v1/events` | `read` | SSE: every agent, for a dashboard. |
| `GET` | `/v1/report` | `read` | Counts and total spend. |
| `POST` | `/v1/session` | bearer | Trade a token for a browser cookie. |
| `DELETE` | `/v1/session` | any | Sign this browser out. |

`/v1/health` is unauthenticated on purpose and returns `{"status":"ok"}` and
nothing more — no version, no agent count, no hostname. A health check that
leaks inventory is a reconnaissance endpoint.

### Spawning

```http
POST /v1/agents
Authorization: Bearer <token>
Idempotency-Key: 9f2c1e04-...

{
  "prompt": "summarise today's PRs",
  "harness": "claude_code",
  "name": "pr digest",
  "cwd": "/home/jod/work/repo",
  "model": null,
  "permission": "ask",
  "resume": {"mode": "fresh"}
}
```

**`Idempotency-Key` is not decoration.** A phone on a flaky connection retries a
POST it never saw a response to. Without a key, the retry spawns a *second*
agent: double the work, double the spend, two agents editing one worktree. With
one, the retry returns the original agent. The key is remembered for 24 hours,
scoped to the token that used it.

The response is an `AgentSummary` — the same struct the CLI prints — plus
`Location: /v1/agents/{id}`.

### Killing

`DELETE /v1/agents/{id}` is idempotent by nature: killing a finished agent
reclaims its tmux session and is not an error. This mirrors `jod kill`, and it
matters because [an agent's tmux session outlives the
agent](decisions.md#an-agents-tmux-session-outlives-the-agent) — "the run is
over" and "the session is gone" are different questions, and the API answers
both via `status` and `session_closed`.

There is no `PATCH`. An agent is not editable; it is spawned, watched, and
stopped.

## Security

### The API is not on the public internet

Given that a credential here is remote code execution, the cheapest and largest
security win available is to remove the internet from the threat model
entirely. So:

**The daemon binds `127.0.0.1`. It does not listen on a public interface.**

Remote access is [Tailscale](https://tailscale.com/docs/features/tailscale-serve)
— a WireGuard tailnet where every device is individually authorised, traffic is
end-to-end encrypted, and the VPS exposes no inbound port to the world.
`tailscale serve` terminates TLS with a real `*.ts.net` certificate, so there is
no certificate to manage, renew, or get wrong.

This is the same instinct behind Anthropic's own Remote Control feature, which
[opens no listening port on the developer's
machine](https://www.helpnetsecurity.com/2026/02/25/anthropic-remote-control-claude-code-feature/)
and polls outbound instead. We can do better than polling because the VPS is
already a reachable server — but only within the tailnet.

The cost is honest: every client device must run Tailscale, and on Android the
VPN has a
[real battery cost](https://www.xda-developers.com/enabled-https-secure-self-hosted-apps-tailscale/).
That is the price of not having an RCE endpoint on the public internet, and it
is worth paying. Revisit only with a hardened public edge — and with the
`bypass` policy refused remotely, which is the default here anyway.

### The network is not the authentication

Being on the tailnet is necessary, never sufficient. A tailnet contains a laptop
that can be stolen and a phone that can be unlocked, and a flat "if you can
reach it you can use it" model turns one compromised device into full control of
the box.

So every route except `/v1/health` requires a bearer token:

- **Opaque and random**, 256 bits from the OS CSPRNG. Not a JWT — there is one
  issuer and one verifier on one machine, so JWT buys nothing and costs a
  signature-verification footgun.
- **Stored hashed.** The daemon keeps SHA-256 digests in
  `~/.jod/api-tokens.json`, never the token. Reading that file off a backup does
  not yield a credential.
- **Compared in constant time**, so response latency cannot be used to guess a
  token byte by byte.
- **Never logged.** Not on success, not on failure, not in an error body.
- **Scoped** `read` or `write`. A phone that only watches agents gets a `read`
  token and cannot spawn anything. This is the single highest-leverage control
  here: it means the credential most likely to be carried into a coffee shop is
  not the credential that can execute code.

Tokens are minted with `jod-api token issue --scope read` and printed **once**.

#### Browser sessions, and why they exist

`EventSource` cannot set an `Authorization` header. That is the one real
constraint SSE imposes, and there are only two ways past it: re-implement SSE
over `fetch` — handing back the automatic reconnect and `Last-Event-ID` resume
that SSE was chosen *for* — or put the credential somewhere the browser sends by
itself.

So `POST /v1/session` trades a bearer token for a cookie:

```
Set-Cookie: jod_session=…; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=604800
```

`HttpOnly` keeps it away from any script, including an injected one.
`SameSite=Strict` is the CSRF defence. `Secure` is set unconditionally —
production is HTTPS via Tailscale, and browsers treat `http://localhost` as a
secure context for development.

Four properties worth stating, each pinned by a test:

- **Bearer keeps working everywhere.** curl, the CLI and native mobile clients
  never touch a cookie. The cookie is additive, not a replacement.
- **A session inherits the scope of the token that made it.** A `read` token
  cannot be laundered into a `write` session.
- **A session cannot mint a session.** Only a bearer can, so a stolen cookie
  cannot renew itself indefinitely.
- **A presented-but-invalid bearer is a refusal, not a fallback** to whatever
  cookie happened to ride along.

The response body carries `{"scope": …, "expires_at_ms": …}` so a client can grey
out actions it cannot perform rather than offering a form that will eat a 403.

Sessions live in memory only, so a restart signs every browser out. For a
credential that can execute code, that is a feature.

A 401 on an SSE stream is a real HTTP 401, which drives `EventSource` to
`CLOSED` rather than an endless reconnect loop — so an expired session shows as
"re-authenticate" instead of a HUD that looks connected and is quietly frozen.

#### Why not the Tailscale identity header

`tailscale serve` injects `Tailscale-User-Login`, which is tempting: free
identity, no tokens. It is also
[a documented footgun](https://github.com/denoland/clawpatrol/issues/316) — the
header is an ordinary HTTP header, and anything that can reach the service
directly can simply set it. Trusting it is safe *only* when the service is bound
to localhost and the header is verified against `tailscale whois` for the peer
address.

We bind to localhost anyway, so the header is trustworthy here — but it is used
only as an *audit* signal ("which device did this"), never as the authorisation
decision. Identity that the client can assert is not authorisation.

### Bounding the blast radius

Authentication decides *who*. These decide *how much damage a valid credential
can do* — which is the part that matters once you accept that credentials leak.

**A permission ceiling.** `PermissionPolicy::Bypass` auto-approves every tool
call. Over an API, on a server, that is the most dangerous value in the system.
The daemon takes a `max_permission` (default `accept_edits`) and **refuses** any
request above it with `403`. Raising the ceiling to `bypass` is a deliberate,
local, config-file act — never something a remote request can do to itself.

**A working-directory allowlist.** Without one, `cwd` is a free parameter and an
agent can be pointed at `~/.ssh`, `/etc`, or the Jod checkout itself. The daemon
takes a list of permitted roots and rejects anything outside them. Paths are
canonicalised **before** the check, so `/allowed/../../etc` is caught rather than
string-matched — the classic traversal bug.

**A concurrency cap.** Each agent is a tmux session, a harness process, and real
money. A looping client should hit `429`, not fork-bomb the VPS or empty an
account. Default 8.

**A prompt size limit.** Bodies are capped (256 KiB default) so a large POST
cannot exhaust memory.

**An audit log.** Every mutating request appends one line to `~/.jod/audit.jsonl`:
timestamp, route, token label (not the token), tailnet identity if present,
agent id, outcome. Append-only, greppable, in keeping with
[plain files at the boundaries](jod-system.md#design-rules). When something goes
wrong the question is always "what ran, and who asked for it".

### What this design does not defend against

Worth stating plainly, because a security section that claims completeness is
lying:

- **A compromised harness.** Once an agent runs, it does what agents do. The
  permission ceiling narrows this; it does not close it.
- **Prompt injection.** A malicious repository can induce an agent to do things
  the operator did not intend. This is a live problem across the whole industry
  and the API layer is the wrong place to solve it.
- **A stolen `write` token**, until it is revoked. Mitigations are scope,
  revocation, and the audit log — not prevention.
- **Anyone with shell on the box.** They do not need the API.

## Configuration

`~/.jod/api.toml`, every value overridable by environment variable so a systemd
unit can set them without a file.

```toml
bind = "127.0.0.1:8787"        # JOD_API_BIND
max_permission = "accept_edits" # JOD_API_MAX_PERMISSION — "bypass" is opt-in
max_concurrent_agents = 8       # JOD_API_MAX_AGENTS
allowed_cwd = ["/home/jod/work"] # JOD_API_ALLOWED_CWD (colon-separated)
max_body_bytes = 262144         # JOD_API_MAX_BODY
session_ttl_hours = 168         # JOD_API_SESSION_TTL_HOURS
```

An empty `allowed_cwd` means **deny every spawn**, not "allow everything".
Failing closed on an unset security control is the only safe default; the
opposite turns a forgotten config line into an open shell.

## Deployment

The daemon runs under systemd as a dedicated unprivileged user, hardened with
the usual namespacing (`ProtectSystem=strict`, `NoNewPrivileges`,
`PrivateTmp`), with `~/.jod` as its only writable path. `tailscale serve`
publishes it to the tailnet over TLS. See [`deploy/README.md`](../deploy/README.md).

The unit is *not* installed by this repo's installer — putting an RCE endpoint
on a machine should be an explicit act, never a side effect of installing a CLI.

## Sources

- [Resume tokens and last-event IDs for LLM streaming](https://ably.com/blog/resume-tokens-last-event-id-llm-streaming-reconnection) — Ably
- [Everyone said SSE token streaming was easy](https://zknill.io/posts/everyone-said-sse-token-streaming-was-easy/) — the write-amplification critique
- [Streaming patterns in 2026 — SSE, WebSocket, gRPC](https://blog.rajpoot.dev/posts/backend/streaming-patterns-2026/)
- [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve) and [Tailscale identity](https://tailscale.com/docs/concepts/tailscale-identity)
- [Do not trust `Tailscale-User-Login` from arbitrary loopback proxies](https://github.com/denoland/clawpatrol/issues/316)
- [Anthropic's Remote Control opens no inbound port](https://www.helpnetsecurity.com/2026/02/25/anthropic-remote-control-claude-code-feature/)
- [RFC 9457 — Problem Details for HTTP APIs](https://www.rfc-editor.org/rfc/rfc9457)
- [Securely exposing self-hosted services with mTLS](https://julianmeisel.dev/2026/01/securely-exposing-selfhosted-services-without-tailscale/) — the alternative considered
