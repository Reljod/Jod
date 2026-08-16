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

`docs/spec-harness.md` (then `SPECS.md` on `feat/harness-spec`) maps three lanes and **none of them covers
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
| digits / which-key `Ctrl-G` | a tab bar for the 9, in MENU order |
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
**PR #61** (`feat/api-workspaces`, `api/src/workspaces.rs`, all ten registered in
`api/src/lib.rs`, all `GET`, all `Scope::Read`):

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

### The main chat — on the wire as of PR #61

This was the one gap, and it is closed. Reljod assigned it to the `api/` lane and
it shipped on `feat/api-workspaces` (PR #61), so the fleet's pinned top row is
**enterable** rather than present-but-inert:

```
GET  /v1/conversations?limit=        read   → ConversationSummary[]
GET  /v1/conversations/main          read   → { conversation, messages }
POST /v1/conversations/main/messages WRITE  → { agent, conversation_id, compaction_due }
GET  /v1/conversations/{id}          read   → Conversation
GET  /v1/conversations/{id}/messages read   → Message[]  (full thread, oldest first)
```

Verified: all five registered in `api/src/lib.rs`.

**Cite the branch and the PR, never a SHA.** `feat/api-workspaces` has been
rebased and force-pushed three times while this spec was being written, and each
time the routes, shapes and field names were unchanged — only the base moved. A
SHA pinned here goes stale by design and reads as "this was checked against
something that no longer exists", which is worse than no citation. Pull it with
`git fetch && git reset --hard origin/feat/api-workspaces`, never a merge, and
expect line numbers to drift.

The merge gate correctly refuses to auto-merge #61 — an API contract change of
1856 lines — so it waits for Reljod's review. Its one `security` finding is a
false positive: triage's regex matches the PR's own prose quoting the TUI's
rendered-secret display string. That is left un-reworded on purpose, since
narrowing a check to make it pass is exactly what the charter forbids.

Five semantics that decide how the row is built:

- **`conversation` is `null` before anyone has spoken** — a state to render, not
  a 404. The pinned row draws from first launch, the way the TUI's does.
- **Reading it does not create it.** The GET path uses `pinned_conversation`
  rather than core's get-or-create `main_conversation`, because a GET that
  creates is a GET a prefetcher can fire.
- **`role` has six values, not two.** Real threads interleave `thinking` turns
  and carry `tool_call`/`tool_result` rows with `tool_name`. This app already
  has the vocabulary for all of it — `session.ts`'s `Entry` renders the same
  eight kinds from the event stream — so the thread reuses those renderers
  rather than inventing a second set.
- **`parent_id` is a real tree and `head_id` is the leaf being talked to.**
  Moving `head_id` *is* switching branches. **Decision: render the thread flat**,
  following `head_id`. Branching is a power-user act with no gesture on a phone,
  and a tree drawn at 393pt is the node-link mistake `graph.rs` already argues
  against. Stated here so it is deliberate rather than accidental.
- **POST body is `{ instruction, harness?, cwd? }`**, `201` on success, and it
  carries an **`Idempotency-Key`** — a replay returns `200` with the original
  rather than starting a second run. This app already sends one on every spawn
  for exactly that reason.

**A `403` naming `accept_edits` must be surfaced verbatim, not swallowed.** The
main chat runs at `accept_edits` by construction, because `ask` is plan mode and
plan mode refuses the MCP calls that are the orchestrator's whole job. The route
checks that against the daemon's `max_permission` before handing over, so a
daemon capped at `ask` answers `403` with a detail naming both the mode and the
setting. The operator needs to know which knob to turn, so the honest UI is the
daemon's own text — the same rule this app already follows for a `403` on spawn.

**Unproven, and to be treated as such:** the `api/` lane exercised every read
against real data but has **not** driven a real instruction through
`POST /v1/conversations/main/messages`, because that spawns an actual
orchestrator run on Reljod's box. Its refusals are tested; the happy path is
covered only insofar as it delegates to `hand_to_orchestrator`, which the CLI,
TUI and Telegram bridge do exercise in production. **The first real send from the
phone is the first real send, full stop** — report what it does.

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
  8787`, nothing stripped. Would need a `ServeDir` fallback in `api/` plus
  tower-http's `fs` feature (`api/Cargo.toml:25` is `["limit","cors","trace"]`;
  no `ServeDir` anywhere in `api/src/`). Makes same-origin *structural* — a
  property of the binary rather than of whoever last ran `tailscale serve`.
- **A2 — Caddy on loopback as multiplexer.** `tailscale serve --bg 8080` → Caddy
  on `127.0.0.1:8080`, `/v1/*` → `:8787`, everything else → the static dir. No
  code change. Caddy on loopback behind the tailnet opens no port and is a
  different role from Caddy as a public `:443` ingress, which the exposure lane
  recommends against.

**Decided: A2.** Put to Reljod alongside the main chat; he chose the main chat
and not the asset-serving, so `jod-api` is not growing a `ServeDir` today and
`api/Cargo.toml` is untouched. A1 stays available if that changes. Document both,
lead with A2.

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

**`tauri-plugin-http` is the load-bearing half, not an optimisation.** Stated
plainly so nobody wires up a token and expects it to be enough: bearer fixes the
*credential*, not the *origin*. A cross-origin `fetch` issued from the webview
still needs `Access-Control-Allow-Origin` on the response and there is none, so
**bearer-alone stays blocked**. Only issuing the request from Rust — where the
browser's CORS check does not apply — makes the combination work.

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

Each step keeps `npm run check` green; the suite is the runnable check. Baseline
was **244** tests; it is **331** now.

**Done — the headless layer.** Everything that can be got wrong is a pure module
with tests, which is how the rest of this app is built (`session.ts` is a
reducer, the components are a projection):

| Module | What it holds | Tests |
|---|---|---|
| `workspaces.ts` | the map — names, letters, digits, titles, sort orders, `ListState` | 24, case for case with `workspace.rs`'s own suite |
| `navigation.ts` | the state machine — back stack, one-at-a-time dismiss, the graph's visit stack | 32 |
| `workspace-contract.ts` | TS mirrors of all fifteen wire types | — (types) |
| `gloss.ts` | the cron gloss, ported from `data::gloss` | 10, including the must-not-guess list |
| `client.ts` | the ten workspace routes + five conversation routes | 21, on the contract's sharp edges |

**Remaining — the React projection.** No new rules live here; these draw the
state above:

1. ~~Nav shell state~~ — done (`navigation.ts`). **Still to draw:** the tab bar
   in MENU order, and the screen frame that titles itself from `title()`.
2. **Fleet** — off `/v1/agents` + `/v1/report` + `/v1/events`. Pinned top row is
   the main chat and **enters it** (§3). Sorts: running first · newest · name ·
   spend, with the pinned row outside the sort and outside the filter.
3. **Slash commands** — close the gap to `command.rs`'s set, including `/open`
   for each workspace so the terminal habit works.
4. **Activity** — `needs_you` first, `jump_to` actually navigates.
5. **Schedules · Goals · Hooks · Tasks** — lists plus detail, own gloss ported
   from `data::gloss`.
6. **Memory + local graph** — list, then push-navigation drill-down as the visit
   stack. No node-link drawing.
7. **Main chat** — the pinned thread, rendered flat along `head_id`, reusing
   `session.ts`'s existing renderers for the six roles. `403` surfaced verbatim.
8. **README rewrite** — install paths A and B, marked unverified.

Nothing is deferred for want of an API route any more. What remains unproven is
listed in §4 and at the end of §3, and is unproven for want of a *running
daemon*, not a missing shape.

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
