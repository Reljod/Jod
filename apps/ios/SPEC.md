# SPEC — bring `apps/ios` up to the nine-workspace TUI

Status: ready to execute. Owner: the `apps/ios` lane. Scope: `apps/ios/**` only.

## Why this exists

`apps/ios` was last touched on 2026-08-10 (`26d32f8`) against a TUI that was one
conversation. Two days of TUI work later, `jod tui` is a nine-workspace console
(`cli/src/tui/workspace.rs`) with a fleet, a pinned main chat, memory and its
local graph, schedules, goals, webhooks, tasks, activity and team — and roughly
thirty slash commands where the phone has twelve.

The phone is therefore not "slightly behind". It is a different product.

Two things also turned out to be **broken rather than merely dated**, both
confirmed from source, and both are fixed here:

1. The documented setup cannot authenticate at all (§4).
2. The packaged app cannot complete a single request (§4).

## 1. Ownership, settled with the other live lanes

Confirmed by direct exchange, not assumed:

| Path | Owner |
|---|---|
| `apps/ios/**` | **this lane** |
| `api/**` | `web app ui parity` |
| `packages/hud/**`, `apps/desktop/**` | `desktop app visualization refactor` |

Both peers confirmed they will not touch `apps/ios/**`. This lane touches
nothing outside it — in particular it adds **no API routes** and does **not**
add a `CorsLayer` (§4 explains why that would be the wrong fix).

`SPECS.md` on `feat/harness-spec` maps three lanes and **none of them covers
`apps/ios/`**. Flagged for Reljod: this lane should be recognised there.

## 2. The nav model: mirror the TUI, do not invent one

No client has nine-workspace nav yet, so there is nothing to copy from web or
desktop. All three lanes agreed to mirror the TUI's own model, which already has
the names, the order and the muscle memory — `cli/src/tui/workspace.rs`:

```
MENU order (which-key letters and digits both follow it):
  1/c chat · 2/f fleet · 3/m memory · 4/s schedules · 5/g goals
  6/h hooks · 7/t tasks · 8/a activity · 9/w team
```

Rules carried across:

- **Chat is home.** Every other screen is somewhere you went *from* chat and
  come back to. On the phone: chat is the root; workspaces are pushed over it,
  and the back gesture is `Esc`.
- **`MemoryGraph` is not a destination.** It is memory's second level, reached
  from a focused node, because it means nothing without one. It gets no tab and
  no digit — a drill-down only.
- **`Esc` unwinds one thing at a time** — an open filter first, then the screen.
- Each list keeps its own cursor, filter and sort (`ListState`), and the
  selection is an **id, never a row index**, because the fleet re-sorts under
  the cursor and an index silently moves it onto a different run.
- Each screen's `S` sort orders are ported verbatim from `Workspace::sorts()`.

### Phone-native substitutions

A keystroke whose *mechanism* has no meaning on a phone gets its **rule** ported
rather than the key — the precedent the README already set for `Ctrl-W`:

| TUI | Phone |
|---|---|
| digits / which-key `Alt-K` | a tab bar for the 9, in MENU order |
| `↑↓` + `⏎` on a list | tap the row |
| `/` filter line | a search field on the list |
| `S` sort cycle | a sort control showing the current order's name |
| `⏎` re-centre, `Backspace` pop (graph visit stack) | **push navigation** — iOS's own nav stack *is* the visit stack |

That last row is the happy one: `cli/src/tui/graph.rs` argues there is no layout
algorithm, no zoom, no edge crossings — one focus node, in-edges above,
out-edges below, and a visit stack so you can walk back out. A phone's
navigation controller is that model natively. **No force-directed graph on the
phone**; the ~20-node argument is stronger at 393pt than in a terminal.

## 3. The data, and the one real gap

Seven workspaces are servable **today** against routes verified in
`feat/api-workspaces` (`82e5603`, `api/src/workspaces.rs`, all ten registered at
`api/src/lib.rs:135-144`, all `GET`, all `Scope::Read`):

```
/v1/memory?scope=&limit=            → { nodes, node_count, edge_count }
/v1/memory/{id}?limit=              → node + { in_edges, out_edges }
/v1/memory/{id}/graph?depth=&limit= → { root_id, nodes, edges }
/v1/schedules · /v1/schedules/{name}?limit= → Schedule[] · Schedule + { fires }
/v1/goals · /v1/goals/{name}
/v1/hooks?limit=                    → (Rule + { deliveries })[]
/v1/tasks?team=                     → TeamTask[]
/v1/activity?limit=&needs_you=      → ActivityItem[]
```

Fleet and team need no new routes: `/v1/agents`, `/v1/report`, `/v1/events`,
`/v1/teams`, `/v1/teams/{team}` already exist.

Semantics to honour rather than rediscover:

- **The API sends core types, not `data.rs` rows.** No `gloss`, no
  `"✓ verified 2m ago"`, no sparkline — a cron gloss is an English sentence and a
  relative timestamp is true for one second. **This app writes its own gloss**
  from `cron` + `timezone` + `next_fire_at_ms`. Port the wording from
  `data::gloss` so the two agree.
- `limit=0` returns zero rows and is honoured, while `/v1/memory`'s counts still
  describe the whole graph — a cheap counts-only call.
- Errors are RFC 9457, as the existing routes: `401` carries no detail, `404`
  does.
- `/v1/tasks` with no `?team=` picks the first team that has a member — same
  rule as `/v1/teams`.
- `ActivityItem.jump_to` is a two-element tuple `["schedules"|"goals", name]`.
  It must actually navigate; an activity row that names a schedule and cannot
  reach it is the TUI feature without the point of it.
- `needs_you` is why the activity screen exists. Surface it, and default the
  screen to it.

### The gap: the pinned main chat is not on the wire

`core/src/conversation.rs` holds the whole model — threads, forks, reverts,
compaction, handoff — and **the API exposes none of it**. In the TUI the fleet's
top row *is* the pinned main chat, and `⏎` there enters it.

The `api/` owner scoped their commit to the six read-only tables deliberately:
the main chat is a thing you *send to*, so it is a write surface with
audit-trail obligations, and they want Reljod's decision before designing it.
That is the right call and this lane does not route around it.

**Consequence, stated plainly:** the fleet ships with its pinned top row
**present but not enterable**, saying why. Everything else in the fleet works.
This is the one place the phone knowingly falls short of the TUI, and it unblocks
the moment a conversations route exists.

## 4. Reaching the daemon — the part that was broken

Three findings, each verified from source on `main`:

1. `api/Cargo.toml:25` enables tower-http's `cors` feature, but **`CorsLayer` is
   never constructed** — `grep -rn 'CorsLayer|allow_origin|Access-Control'
   api/src/` returns nothing. No CORS header is ever sent. (Independently
   confirmed by the `api/` owner in their own worktree.)
2. `api/src/session.rs:110` sets `HttpOnly; Secure; SameSite=Strict`. A `Secure`
   cookie is **not stored over plain `http://`** to a non-localhost host, and a
   `SameSite=Strict` cookie is **never sent cross-site**.
3. The packaged app's page origin is `tauri://localhost`
   (`apps/ios/src/origin.ts`) and it uses plain webview `fetch` with
   `credentials: "include"` (`apps/ios/src/client.ts:148`). There is **no
   `tauri-plugin-http`** in `apps/ios/src-tauri/Cargo.toml`.

So: the README's advice — point the app at `http://jod-cloud:8787` and add an ATS
cleartext exception — describes a setup whose auth flow **cannot complete**, and
the packaged app is cross-origin against an API that sends no CORS headers, so it
**cannot complete a single call** either. The `Secure` flag's own comment says
the author always assumed *"HTTPS via Tailscale"*.

**The fix is TLS plus the right auth per shell — not CORS.** Adding `CorsLayer` +
`SameSite=None` would trade a real CSRF defence for something the PWA gets free
by being same-origin. Both peers agree; the `api/` owner is explicitly not
adding it.

Precisely, because it is easy to overstate: **same-origin fixes the PWA only.**
It cannot fix the packaged app — assets loaded from `tauri://localhost` are
cross-origin to `https://<host>.ts.net` no matter what the daemon mounts, unless
the shell loads its content remotely, at which point it is a PWA in a wrapper.
Two shells, two answers.

### Install path A — PWA, primary

`tailscale serve` terminates TLS with a real `*.ts.net` certificate. Serve the
built bundle and the API from **one origin**, add to home screen.

- same-origin ⇒ no CORS, and `SameSite=Strict` is satisfied
- secure context ⇒ the `Secure` cookie is stored, `EventSource` works — the
  automatic reconnect and `Last-Event-ID` resume come free
- **no Mac, no Xcode, no 7-day free-provisioning expiry**

**Do not mount the naive path split.** `/` → bundle, `/v1` → daemon looks right
and fails twice: `tailscale serve --set-path` **strips the mount prefix before
proxying**, so `/v1/health` arrives at the backend as `/health` and every route
in `api/src/lib.rs` — all registered with a literal `/v1/` — 404s; and a mount at
`/` **overrides all other paths** on precedence. Two independent failures.

One origin therefore means one of:

- **A1 — the daemon serves the bundle.** Single mount, `tailscale serve --bg
  8787`, nothing stripped. `api/` would gain a `ServeDir` fallback at `/` and
  tower-http's `fs` feature (`api/Cargo.toml:25` is `["limit","cors","trace"]`
  today; no `ServeDir` anywhere in `api/src/`). Makes same-origin *structural* —
  a property of the binary, not of whoever last ran `tailscale serve`.
  **`api/`'s call, not this lane's.**
- **A2 — Caddy on loopback as multiplexer.** `tailscale serve --bg 8080` → Caddy
  on `127.0.0.1:8080`, `/v1/*` → `:8787`, everything else → the static dir. No
  code change, unblocks today. Caddy on loopback behind the tailnet opens no
  port and is a different role from Caddy as a public `:443` ingress, which the
  exposure lane recommends against.

Still needs the Tailscale VPN profile on the phone — the PWA does not remove
that. `tailscale funnel` is ruled out (`deploy/README.md`: identity headers are
not injected), and public exposure is wrong on principle: a credential for this
API is arbitrary code execution on the box.

### Install path B — packaged Tauri app, native

Keep the shell, move it off the cookie: **bearer auth via `tauri-plugin-http`**,
so requests go through Rust and never meet the webview's CORS check.
`api/src/session.rs:9-11` already says native mobile clients never need a
cookie. Requires no API change. Note that a `CorsLayer` would *not* rescue the
cookie here either — `SameSite=Strict` blocks it cross-site regardless — so
bearer is the only clean route.

**The cost, priced honestly:** `EventSource` cannot set an `Authorization`
header, which is the whole reason the cookie exists, so a bearer client must
hand-roll SSE over `fetch` — giving back the reconnect and resume that
`EventSource` provides for free.

`packages/hud`'s `HttpTransport` **already does this**, so the work is reuse
rather than invention: `new HttpTransport(base, token)` switches to
`Authorization:` plus a `fetch` stream, `scheduleRetry` reconnects with
exponential backoff capped at 15s, and recovery goes through the **`after_seq`
REST backfill plus `(agent_id, seq)` dedupe** rather than the `Last-Event-ID`
header. For this API that is arguably the better cursor, since `seq` is
per-agent.

But it is **thinner than it looks**: `packages/hud/test/sse.test.ts` covers only
the frame *parser* — eight cases on `data:` joining, comments, the one-space
rule. **The reconnect and backfill path has no test**, and the desktop lane
warns its live HTTP path has never run against a real daemon. So path B's
streaming is code that exists and is unproven, which is a real argument for
making the PWA primary independent of the Mac/Xcode convenience.

### Auth rules to get right

- **`401` means "re-`POST /v1/session`", not "your token is bad."** Sessions are
  in-memory, so a daemon restart signs every browser out. The app must not send
  the user back to the token gate on a restart.
- Mint **per-device** tokens, and default the phone to
  `--scope read`. A read token cannot execute code if the phone is lost — the
  highest-leverage control available here.
- Keep the existing rule that the token is never stored; only the scope is.

### Honesty requirement for the README

The VPS **has no Tailscale installed and no `jod-api` systemd units** — measured
on the box by the exposure-research lane. `deploy/README.md` describes an
intended end state. The current README reads as though someone had done it.
Install steps ship **marked unverified**, naming what has not been run.

Specifically unverified, and each a ten-minute check once Tailscale is installed:

- that `tailscale serve` strips the mount prefix, and that a `/` mount takes
  precedence (both read from Tailscale's docs, not observed)
- that the `Secure` cookie is in fact stored over `https://<host>.ts.net` and
  dropped over `http://`
- that `EventSource` survives iOS backgrounding Safari and resumes

Do these before the install steps lose their "unverified" marking. The README
must not claim otherwise in the meantime.

## 5. Work order

Each step keeps `npm run check` green; the suite is the runnable check.

1. **Nav shell** — `Workspace` model ported from `workspace.rs`, tab bar in MENU
   order, chat as root, `ListState` (id-keyed selection, filter, sort) as a pure
   module. Tests mirror `workspace.rs`'s own, case for case.
2. **Fleet** — off `/v1/agents` + `/v1/report` + `/v1/events`. Pinned top row
   present, not enterable, says why. Sorts: running first · newest · name · spend.
3. **Slash commands** — close the gap to `command.rs`'s set, including `/open`
   for each workspace so the terminal habit works.
4. **Activity** — `needs_you` first, `jump_to` actually navigates.
5. **Schedules · Goals · Hooks · Tasks** — lists plus detail, own gloss ported
   from `data::gloss`.
6. **Memory + local graph** — list, then push-navigation drill-down as the visit
   stack. No node-link drawing.
7. **README rewrite** — install paths A and B, marked unverified.

Deferred, not forgotten: entering the pinned main chat, which needs a
conversations route (§3).

## 6. Definition of done

- `npm run check` green — `tsc --noEmit` plus vitest, with new suites for the
  nav model, each workspace's data mapping, and the gloss.
- Nine workspaces reachable in MENU order; `MemoryGraph` reachable only from a
  focused node.
- Every list: id-keyed cursor, filter, and a sort cycle naming its current order.
- No `apps/ios` file imports from a path outside `apps/ios/**` except the shared
  contract re-export.
- README states the working install path and marks it unverified.
- No new API route, no `CorsLayer`, no weakened cookie.

## 7. Known coordination item

`desktop app visualization refactor` moved `apps/web/src/types.ts` →
`packages/hud/src/types.ts` in `92b05ad` on the unmerged `feat/desktop-hud`.
`apps/ios/src/contract.ts:40,50` and `apps/ios/tsconfig.json:23` point at the
old path.

**On `main` today the old path still exists and `packages/` does not**, so
repointing now would break this build immediately. The fix lands *when* that
branch merges, as `@jod/hud` via a `paths` alias — the form web and desktop both
use. That lane will ping this one at merge.
