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

## Architecture

```
transport/     one interface, three drivers   ← swap live/sim without touching the UI
  http.ts        REST + SSE against jod-api
  sim.ts         seeded synthetic fleet
state/world.ts   folds AgentEnvelopes into the world model
graph/           physics.ts (forces) + model.ts (what to draw)
render/          hand-rolled canvas; no graph library
components/      the DOM chrome around the canvas
```

**React is deliberately not in the hot path.** Event bursts arrive faster than a
paint, so `WorldStore` absorbs them and publishes to the panels 10×/second, while
the canvas reads `store.world` directly on every frame. A hundred events cost one
frame of physics, not a hundred component renders.

**No graph or charting library.** Every mark needs to be bound to a real field of
`AgentSummary` or the event stream; a general-purpose library would draw pretty
circles that mean nothing. The whole bundle is 245 kB (79 kB gzipped).

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
  `GET /v1/agents/{id}/events?after_seq=` backfill.
- Errors are RFC 9457 `application/problem+json`; `detail` is rendered.
- `resume` is externally tagged: `"fresh"` | `"last"` | `{"session":"<id>"}`, and
  is a first-class control in the palette — threading is far cheaper to design in
  than to retrofit.

**All eight event kinds are rendered**, including `raw`. Core emits `raw` for
anything a harness said that it could not classify, which makes it the debugging
seam for a harness upgrade. It is collapsed by default in the feed, never dropped
— hiding it would turn *"we did not understand this"* into *"this never
happened"*.

**This client does not poll.** The roster is refreshed only on `started` /
`finished` / `error` and after spawn/kill, debounced ~400 ms.

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

41 tests, no browser required — the physics, the world reducer and the graph
derivation are all pure.

```
npm run check
```

The ones worth knowing about assert the *semantics*, not the rendering: that an
engaged agent settles closer to the core than a disengaged one, that two finished
agents in a shared directory produce **no** contention link, that a long
`cargo test` does not get mis-marked idle, and that `rankForDisplay` never hides
a node without counting it.
