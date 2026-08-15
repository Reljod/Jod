# JOD // TACTICAL

The web layer for Jod — a live tactical display of the agent fleet.

Jod delegates work to agent harnesses and normalises everything they emit into
one `AgentEvent` stream. This app is the operator's window onto that stream: not
a list of rows, but a force-directed graph where **an agent's position, colour,
motion and size all encode something true about what it is doing right now**.

```
npm install
npm run dev          # http://localhost:5173
npm run check        # tsc --noEmit && vitest run
```

With no orchestrator running it starts on a **simulated fleet** so the whole
display is exercisable offline. Force it either way with `?feed=sim` /
`?feed=live`.

## Reading the display

The layout is the data. Nothing here is decoration:

| What you see | What it means |
|---|---|
| **Distance from the core** | Disengagement. A hot agent is pulled in tight; one that has gone quiet drifts to the rim. |
| **Hue** | Which harness — cyan Claude Code, mint OpenCode, violet AGY. |
| **Outer ring spin rate** | Events per second. **A stalled ring is a hung run** — the fastest way to spot one. |
| **Arc gauge** | Cumulative token burn. |
| **Node size** | Mass, from total spend. Expensive long-lived agents anchor the layout. |
| **Orbiting dots** | Recent tool calls. The in-flight one is bright and pulses; finished ones fade. |
| **Travelling packets** | Real events moving the tether — outbound is a tool call, inbound a result. |
| **Red dashed arc** | **Two live agents share a working directory.** |
| **Clustering** | Same harness. |

That red arc is the one to care about. The charter's rule is *teammates share one
checkout, so one owner per path* — two live agents in the same `cwd` is the
collision that rule exists to prevent, so it is drawn hot and labelled with the
path rather than shown as neutral structure.

`status` and `process_alive` are surfaced **separately**, because they answer
different questions: a run marked `running` with nothing alive behind it never
reported how it ended, and that gap is the thing worth seeing. The dossier shows
the process group beside the status, and offers `jod watch` whatever the state —
watching a finished run replays it.

Controls: drag a node to reposition · drag the background to pan · scroll to zoom
· double-click to clear selection · `⌘K` for the command palette.

The camera **auto-fits the whole fleet** as agents spawn and drift; panning or
zooming hands you manual control, and RECENTRE gives it back.

## The second view: TIMELINE

The graph answers *what is happening now*. It cannot answer *what did this run
actually do*, because an agent that has moved on leaves no trace. TIMELINE is one
swimlane per agent with time running left to right and now at the right edge:

- **bars** are tool calls, harness-coloured, red when the call errored, dashed
  while still in flight;
- **dots** below each lane are messages, faults and finishes;
- a span that started *before* the window is clipped, never dropped — a
  `cargo test` that has been running for two minutes is exactly what you want to
  see, and dropping it would make the busiest agent look idle.

A long unbroken bar is the thing to look for: that is where the wall-clock went.

## The third view: TRAJECTORY

Both views above are about the *fleet*. Neither can answer the question you have
about one run — **what was this session asked to do, and what did it actually
say and call** — and until now that answer existed only in `jod watch`, which
needs a terminal. TRAJECTORY is one session read end to end. Reach it from the
view switch, or with **READ SESSION** in the dossier.

Down the page, one row per thing that happened, grouped into the **turns** of
the agentic loop. A turn boundary is a *model invocation*, not a message: the
model producing anything after a tool result came back is the harness invoking
it again, which is the only turn structure the event stream actually states.

Across the top, three lanes — **Input**, **Model**, **Tools** — showing where a
turn's time went. The axis is switchable, and that is the point:

| Scale | What it shows |
|---|---|
| **DURATION** | Real proportions. The truth about cost, and the one where a 5ms call is an invisible sliver. |
| **TURNS** | Every turn the same width — the shape of the loop. |
| **CALLS** | Every block the same width. The only way to read a run whose steps differ by three orders of magnitude. |

Four things this view refuses to fake:

1. **There is no system-prompt row.** No harness reports its system prompt, so
   the SYSTEM row carries the settings the session was *launched* under — model,
   cwd, permission, session id — and says so when expanded. A plausible-looking
   invented prompt would be the most misleading thing here.
2. **The USER row is a join, not a guess.** A run's prompt is never emitted as
   an event; it lives in the transcript store as a `user` message. `session_id`
   finds candidate conversations and `run_id` *confirms* the message — a
   conversation spans every run that continued it, so a session match alone
   would eventually show one run's ask above another's answers. No exact match,
   no row.
3. **`progress` and `delta` frames are collapsed, never dropped.** They are the
   only thing on the wire through a nine-minute think or a long `Write`, so a
   transcript without them jumps from a tool result to a message with nothing in
   between — which is exactly what a hung run looks like. They fold into one
   WORKING row carrying the frame count and the thinking-token total.
4. **Truncation is stated.** History the retention cap dropped is counted in the
   header rather than silently beginning the transcript at turn six.

## Architecture

```
transport/     one interface, three drivers   ← swap live/sim without touching the UI
  http.ts        REST + SSE against jod-api
  sim.ts         seeded synthetic fleet
state/world.ts   folds AgentEnvelopes into the world model
state/prompt.ts  the transcript join that recovers a run's opening ask
graph/           physics.ts (forces) + model.ts (what to draw)
                 timeline.ts (swimlanes) + trajectory.ts (one session)
render/          hand-rolled canvas; no graph library
components/      the DOM chrome around the canvas
```

**React is deliberately not in the hot path.** Event bursts arrive faster than a
paint, so `WorldStore` absorbs them and publishes to the panels 10×/second, while
the canvas reads `store.world` directly on every frame. A hundred events cost one
frame of physics, not a hundred component renders.

**No graph or charting library.** Every mark needs to be bound to a real field of
`AgentSummary` or the event stream; a general-purpose library would draw pretty
circles that mean nothing. The whole bundle is 272 kB (88 kB gzipped).

**The store keeps each agent's own events, capped.** A trajectory needs the
structured event, not the feed line, and the global feed is a ring across the
whole fleet where one chatty agent evicts another's history. Retaining per agent
is also what makes the third view tail live *without polling*: every envelope
already passes through `ingest`, so the only fetch is the one-shot backfill of
whatever happened before the page loaded.

**Nothing calls `Math.random`.** The simulation, the starfield and per-node jitter
are all seeded, so a reload reproduces the same scene and the physics tests can
assert on settled positions.

## The API contract

Owned by the `api/` crate (`jod-api`). Types are mirrored in `src/types.ts` from
`core/src/{event,service,store}.rs` and `core/src/harness/mod.rs` — **not** from
`apps/desktop/src/types.ts`, which is an unmaintained mirror since the desktop app
left the workspace.

- Base: same-origin, all routes under `/v1`. The daemon binds loopback and is
  reached remotely over Tailscale, so paths are relative on purpose.
- Live feed: **SSE** at `/v1/events`, `event: agent`, each `data:` one *flattened*
  `AgentEnvelope` (`kind` sits beside the event's own fields).
- `GET /v1/agents` roster · `GET /v1/report` counts · `POST /v1/agents` spawn
  (with `Idempotency-Key`) · `DELETE /v1/agents/{id}` kill ·
  `GET /v1/agents/{id}/events?after_seq=` backfill ·
  `GET /v1/conversations` + `/v1/conversations/{id}/messages` for the transcript
  join behind the TRAJECTORY view's USER row.
- `GET /v1/agents/{id}/events` answers with an **`EventsPage`** — `{events,
  last_seq}` — not a bare array. Reading it as an array does not fail loudly:
  `for…of` on the page object throws inside the per-agent `catch` that exists so
  one bad agent cannot abort a backfill, so the whole lag-recovery path went
  quiet instead of visibly breaking. This driver unwraps it and still accepts
  either shape.
- Errors are RFC 9457 `application/problem+json`; `detail` is rendered.
- `resume` is externally tagged: `"fresh"` | `"last"` | `{"session":"<id>"}`, and
  is a first-class control in the palette — threading is far cheaper to design in
  than to retrofit.

**All eleven event kinds are rendered**, including `raw`. Core emits `raw` for
anything a harness said that it could not classify, which makes it the debugging
seam for a harness upgrade. It is collapsed by default in the feed, never dropped
— hiding it would turn *"we did not understand this"* into *"this never
happened"*.

Eleven, and it used to say eight. `progress`, `delta` and `session_lost` were on
the wire long before they were in `types.ts`, and an event kind TypeScript does
not know about is not merely undrawn — every exhaustive `switch` over `kind`
silently returns `undefined` for it. `heatFor` was one, so `heat + undefined`
made an agent's heat `NaN`, and a `NaN` heat is a `NaN` radius: the node
disappeared. A *healthy* streaming run was the one that vanished. The union is
now the whole enum and a test folds every kind through the store asserting heat
stays finite.

**This client does not poll.** The roster is refreshed only on `started` /
`finished` / `error` and after spawn/kill, debounced ~400 ms — and reading a
session is one backfill per run, not a tick.

### `seq` starts at 0, and the cursor is exclusive

`after_seq` is exclusive, so `?after_seq=0` means *"everything after event 0"* and
**skips `started`** — the event carrying `session_id` and `model`. Two consequences
this client gets right, both covered by tests:

- a first load **omits** `after_seq` entirely rather than passing `0`;
- the dedupe map defaults to `-1`, not `0`, or every agent's `started` would be
  silently swallowed.

The simulation driver numbers from 0 for the same reason: seeding from 1 would
mean never exercising the off-by-one.

### Three things this client defends against

1. **Duplicate replays.** `/v1/events` is documented **live-only and issues no
   `id:`** — deliberately, so the browser cannot send a meaningless
   `Last-Event-ID`. Client-side dedupe on `(agent_id, seq)` is therefore the
   contract, not a workaround, and per-agent backfill is the recovery path.
2. **Dropped broadcasts.** The server emits `event: lagged` with `{"missed": N}`
   when its channel overflows. That is handled by re-reading every agent from its
   last rendered `seq` — the difference between a HUD that knows it is stale and
   one that confidently shows wrong state.
3. **Rehydration floods.** A restarted daemon comes back with its whole run
   history, so the graph plots a ranked budget of 48 (live → faults → recency) and
   states `+N not plotted` rather than truncating silently.

### Auth

Cookie session, chosen over bearer-in-browser because `EventSource` cannot set an
`Authorization` header and hand-rolling SSE would discard the automatic
reconnect that motivated SSE in the first place.

`POST /v1/session` with `Authorization: Bearer <token>` returns `{"scope":…}` and
sets an `HttpOnly; Secure; SameSite=Strict` cookie. **The token is never stored by
this page** — the cookie is the credential from then on. A 401 is a real HTTP 401
on the SSE route, so `EventSource` goes to `CLOSED` without retrying and the HUD
shows a re-auth gate instead of looking connected while frozen.

Write actions follow the returned scope: a `read` session disables spawn, kill and
delegate rather than firing a request that 403s. Anything other than an explicit
`"write"` — absent field, lost link, pending probe — is treated as read.

## Tests

111 tests, no browser required — the physics, the world reducer, the transcript
join and both time derivations are all pure.

```
npm run check                      # this app: tsc + vite build
cd ../../packages/hud && npm run check   # the HUD: tsc + vitest
```

The ones worth knowing about assert the *semantics*, not the rendering: that an
engaged agent settles closer to the core than a disengaged one, that two finished
agents in a shared directory produce **no** contention link, that a long
`cargo test` does not get mis-marked idle, and that `rankForDisplay` never hides
a node without counting it.

The trajectory's are the same kind of claim: that a turn counts a model
*invocation* rather than a message, that a result pairs onto the call it
answered even when three of the same tool are open, that a call which never
returned reads as in-flight on a live run and abandoned on a dead one, that 40
tick frames collapse to one row instead of vanishing, and — the one with a bug
behind it — that folding every kind core can emit leaves an agent's heat finite.
The join has its own: a `session_id` match with the wrong `run_id` yields
**no** prompt rather than the neighbouring run's ask.
