# The Jod system

Jod is a personal AI system that lives on a VPS and stays up. It is a chief of
staff that delegates rather than executes. This document is the architecture for
the whole thing, and marks what is built versus what is planned.

Nothing here requires a cloud service. Every dependency is a local process or a
local file on one box, which is what makes a 24/7 assistant affordable.

## The shape

```
        jod tui ─────┐
        jod run ─────┤
        cron ────────┤        the `jod` command
        HTTP API ────┤              │
        (planned)    │              ▼
                     └────►  jod-core :: service::Jod
                                    │
                     ┌──────────────┼──────────────┐
                     ▼              ▼              ▼
              tmux session   tmux session   tmux session
               claude -p …    opencode run…   agy --print …
                     │              │              │
                     └──────────────┴──────────────┘
                                    │ JSONL via tee
                                    ▼
                       ~/.jod/runs/<id>/stream.jsonl
                                    │ tail + parse
                                    ▼
                               AgentEvent
                                    │
                                    ▼
                          ~/.jod/jod.db  (SQLite, WAL)
```

## The four pillars

| # | Pillar | What it is | Status |
|---|--------|-----------|--------|
| 1 | **Brain nodes** | Flat markdown notes in Open Knowledge Format | Planned |
| 2 | **Brain connections** | A graph over those notes | Planned |
| 3 | **Jod** | The orchestrator that delegates and reports | **Built** |
| 4 | **Agents + A2A** | Harness-run agents that talk to each other | **Built (single-agent)** |
| 5 | **Memory** | What Jod knows, and what it did | **Built** |

Pillars 3, 4 and 5 come first on purpose. They produce value on day one, and
they are what the other two will be *built by* — once Jod can delegate reliably
and remember the result, the knowledge layer can be assembled by agents rather
than by hand.

---

## Pillar 3 — Jod, the orchestrator

**Built.** `core/`.

The single rule: **Jod never does the work.** It has no model client, no prompt
templates and no tools. It owns delegation, observation, memory and reporting;
the thinking happens inside an agent *harness* — a CLI that already solved
context management, tool use and permissions.

This is a CLI on top of other CLIs, and that is the point. Every harness is a
program that already works; Jod's job is to run them uniformly, watch them, and
keep what they produced.

### Why tmux is load-bearing

Every agent runs inside its own `tmux` session, not as a child process of the
app. That single choice buys four things that would otherwise each need code:

- **Observability** — `tmux attach -t jod-<id>` shows the live agent.
- **A kill switch that works** — `tmux kill-session` stops an agent whether or
  not Jod is running.
- **Survivability** — closing your SSH session does not kill running work. On a
  VPS this is the difference between an assistant and a foreground script.
- **One transport for every client** — the CLI, an SSH session, and a future
  HTTP API all watch the same sessions.

The launcher is a generated bash script (`runner::render_script`) that pipes the
harness through `tee`, so the pane a human watches and the JSONL Jod parses are
the same bytes. The prompt is passed via `"$JOD_PROMPT"` read from a file, so a
prompt containing quotes or `$(...)` can never be re-interpreted by the shell.

**The session outlives the agent.** When the harness exits, the pane prints the
status and `exec`s a shell rather than letting the session end — because a
session that destroys itself takes any attached client with it, closing the
terminal of whoever was watching. → [why](decisions.md)

### The harness seam

```rust
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}
```

Adding a harness means one file. Nothing above the seam changes, because every
harness is normalised into one vocabulary:

`Started · Thinking · Message · ToolCall · ToolResult · Finished · Raw · Error`

`Raw` matters more than it looks: an unrecognised line is *surfaced*, never
dropped. All three harnesses print human-readable prose onto the same stream as
their JSON, and `Raw` is why that prose reaches you instead of vanishing.

**The runner owns "the run is over", not the harness.** Completion is detected
from the process exit marker, and each adapter reports its accumulated answer
and cost in `finalize`.

### The three harnesses

| Harness | Invocation | Resume | Cost reported |
|---|---|---|---|
| **Claude Code** | `claude -p … --output-format stream-json --verbose` | `--continue` / `--resume <id>` | yes |
| **OpenCode** | `opencode run --format json …` | `--continue` / `--session <id>` | yes |
| **AGY** (Google Antigravity) | `agy --print … --output-format stream-json` | `--continue` / `--conversation <id>` | no |

Session resume is normalised behind one `Resume` field — `Fresh`, `Last`, or
`Session(id)`. Each harness spells it differently; the seam hides that. This is
what makes `jod chat` a conversation rather than a series of unrelated one-shot
tasks, and it is why Jod needs no memory of the transcript: the harness owns it.

**AGY needs three defences, all found by probing the real CLI.** They are
documented here because each one makes a *failed* run look successful:

1. It auto-denies any tool needing approval in headless mode — then reports
   `status: SUCCESS`, an empty response, and exit code 0. A successful run that
   produced nothing is treated as a failure.
2. An unknown `--conversation` id does not fail. It silently starts a new
   conversation and reports success, so the agent loses every prior turn without
   saying so. The adapter compares the id it asked for against the one AGY
   reports.
3. `--print-timeout` defaults to five minutes and kills the run when it expires,
   which looks like a truncated answer. It is raised explicitly.

---

### The interface

`jod tui` is a full-screen interface built on ratatui: a scrolling transcript,
an input box with line editing, a status bar, and `Ctrl-A` for a panel listing
every delegation. That panel is the reason it is not just a chat window — Jod's
job is watching several agents, and it shows runs from earlier processes too,
because `rehydrate` puts them back.

Two behaviours it takes care over, both easy to get wrong:

- **Scrolling up does not get yanked back down.** New output only follows the
  view if the view was already at the bottom. Reading something while an agent
  keeps talking has to work.
- **The terminal is always restored.** Raw mode and the alternate screen are
  undone on the normal path *and* from a panic hook, because a panic that skips
  the restore leaves a shell that echoes nothing and needs a blind `reset`.

`jod chat` is the same conversation on a plain terminal, for when a full-screen
UI is in the way — over a flaky SSH link, or piped from a script.

---

## Pillar 5 — Memory and durable state

**Built.** `core/src/store.rs`. One SQLite file, `~/.jod/jod.db`, in WAL mode.

The design is not a default; it is the outcome of a benchmark in
[`research/agent-db-2026`](../research/agent-db-2026/REPORT.md) that ran nine
engines with real concurrent OS processes. Three results drive the code:

- **SQLite was both fastest and the only engine that never lost a write.** Under
  contended read-modify-write, Postgres silently discarded 47% of updates on its
  obvious path, LanceDB 51%, Qdrant 46% — every one reporting a 0% error rate.
  SQLite misconfigured *refused* 58% of calls and lost nothing. Failing loudly
  is a feature.
- **`BEGIN IMMEDIATE` is mandatory.** Deferred transactions upgrade their lock
  late and collide: 98% errors in the benchmark, versus 0% correctly configured.
- **Never hold a write transaction across a model call.** The whole argument
  rests on write transactions costing microseconds.

```sql
PRAGMA journal_mode = WAL;      -- readers never block the writer
PRAGMA busy_timeout = 5000;     -- wait for the lock instead of failing
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
```

### What it holds

- **`events`** — every agent event, append-only, unique on `(run_id, seq)` so a
  replayed stream cannot duplicate history.
- **`runs`** — one row per delegation, so a restarted process still knows what
  it launched.
- **`tasks`** — contended state, claimed with a single guarded `UPDATE`. Zero
  rows changed means you lost the race. Never a read then a write.
- **`facts`** — what Jod knows.
- **`tombstones`** — proof that a deletion happened, after the fact is gone.

### How memory behaves

Facts are `subject · predicate · object`, and four properties matter more than
retrieval quality — which the
[retrieval research](../research/harness-agents-research/RECOMMENDATION.md)
measured as the *least* valuable component:

- **Bitemporal.** `valid_from`/`valid_to` describe the world; `recorded_at`
  describes when Jod learned it. A fact is superseded, never edited, so "what
  did Jod believe last month" stays answerable.
- **Scoped.** `scope` is a hard partition applied *before* ranking. Used as a
  ranking signal instead, scope leaked facts across domains 79% of the time;
  as a filter, 0%.
- **Attributed.** `origin` is `owner | agent | untrusted | system`, in its own
  column and never inside the fact text — so a page Jod ingested cannot claim to
  be Reljod by writing "origin: owner" into its content. It is shown at recall,
  because storing the distinction is worthless if it is invisible when read.
- **Really deletable.** `forget` destroys every version and writes a tombstone.
  Closing only the current version leaves the withdrawn fact fully readable to
  any question phrased about the past — measured leaking on 56% of historical
  queries. "Jod forgot that" and "Jod says it forgot that" must be the same
  thing.

Recall is FTS5. **Embeddings are deliberately absent.** Retrieval quality was
worth 0.02–0.07 in the experiments against 0.2–0.5 for the governance work
above, and no ranker answers "what is true now" — a superseded fact is a
near-perfect match for a question about its replacement, outranking the current
one 35–54% of the time. Two frontier coding agents ship no embeddings for
memory. If they are ever needed, `sqlite-vec` brute force measured 100% recall
at 19 ms over 30k vectors and holds to roughly 150k memories.

Free text is escaped into a safe FTS expression before it reaches SQLite:
`what's the plan?` is otherwise a syntax error rather than a search.

### Surviving a restart

`Jod::rehydrate` loads prior runs back into memory. It does not trust the last
status written — a process killed mid-run never recorded how it ended — so it
replays each run's stored events through the same fold a live process uses. A
run still marked running whose tmux session is gone did not report a result and
becomes *failed*, rather than running forever.

`Jod::events_since(id, after_seq)` serves a reconnecting client only the tail it
missed, from memory when this process owns the agent and from the database when
it does not.

---

## Pillar 4 — Agents and A2A

**Built:** agents run under all three harnesses, each in its own tmux session,
each managing its own context.

**Planned:** agent-to-agent communication, following the same local-files
principle, so it needs no broker. An agent sends a message by appending one JSON
line; Jod's tailer already knows how to follow an append-only file. Two paths,
in order of cost:

1. **Prompt-level** — Jod injects pending inbox messages into the next
   delegation. Works with any harness today.
2. **MCP-level** — a small `jod-mcp` server exposing `send_message`,
   `read_inbox`, `list_agents`. All three harnesses support MCP.

**Collaboration on code** uses git rather than messages: each agent gets its own
worktree, and integration is a merge. → [`teamwork.md`](teamwork.md)

---

## Pillars 1 and 2 — The brain

**Planned.** Flat markdown, no hierarchy, one idea per file, following
[Open Knowledge Format](https://github.com/openkf) conventions.

**Markdown stays the source of truth for prose; the database is a derived index
that can be deleted and rebuilt by rescanning.** That keeps the knowledge
portable and greppable, and keeps `grep` working when the system is down. What
markdown cannot be is a *database* — it has no atomic read-modify-write, so two
agents updating one note is last-writer-wins. That is the job SQLite took.

Graph traversal starts as recursive CTEs. GraphQLite ranked well but sits at
v0.3.7 with a single maintainer, and that bus factor is reason enough not to
make it load-bearing before it has to be.

---

## Roadmap

1. ~~Core service: harness seam, tmux runner, event normalisation~~ **done**
2. ~~Third harness (AGY) and normalised session resume~~ **done**
3. ~~Durable runs, transcripts and memory in SQLite~~ **done**
4. ~~CLI: delegate, watch, list, report, remember, recall, chat~~ **done**
   ~~plus `jod tui`, the full-screen interface~~ **done**
5. ~~Browser access for agents — Camoufox, headless, verified~~ **done**
   (awaiting Webshare ISP credentials to fix the egress IP →
   [`browser.md`](browser.md))
6. A2A inbox/outbox + `jod-mcp` server
7. Scheduled work: a digest, and recurring delegations
8. ~~HTTP API for mobile and web clients~~ **done** — `api/`, with the
   tactical HUD in `apps/web` as its first consumer
9. **iOS client: `jod tui` in your pocket** — `apps/ios`, **the current goal**.
   The behaviour is built and tested; shipping it to a device needs a Mac.
   → [the goal](#the-goal-jod-in-your-pocket)
10. Brain nodes and connections

---

## The goal: Jod in your pocket

**In progress.** `apps/ios`.

Jod runs on a box that stays up, which means the work continues whether or not
Reljod is at a desk — and until now, watching it required being at one. The goal
is the `jod tui` conversation on an iPhone, with the desktop client's feel
rather than a terminal emulator's.

Three things fall out of the hardware, and they decide the whole design:

- **An iPhone cannot host an agent.** No tmux, no `claude` binary, no shell to
  run them in. So this client does not embed `jod-core` the way `apps/desktop`
  does; it is a *client of the daemon*, and every capability arrives over HTTP
  from `jod-api`. That is the seam the architecture already had — the core has
  no UI, so clients are interchangeable.
- **The connection is not reliable.** A phone walks out of wifi and locks its
  screen. The per-agent SSE stream replays history and then goes live on one
  connection, so there is no gap between "read what happened" and "start
  listening", and a resume cursor plus `seq` deduplication makes reconnecting
  idempotent. Coming back from the background catches up over REST first.
- **Sending is a physical risk.** The return key is under a thumb, and a
  delegation starts a real process on the box. Enter inserts a newline; sending
  is a deliberate tap; and every spawn carries an `Idempotency-Key` so a retry
  on a flaky link cannot start the same agent twice in the same directory.
- **A packaged app has no origin to fall back on.** The web client is *served
  by* the daemon, so every route can be relative. The iOS bundle loads from
  `tauri://localhost`, where a relative route is not even a valid URL — so the
  daemon's address is a real setting the app asks for once and remembers. This
  was found by running the built app in a simulator, and by nothing before it.

**What "the same as the TUI" means, precisely.** The transcript vocabulary, the
resume cursor that threads turns into one conversation, the busy guard, the
thinking toggle, the agents panel, and the status line are the same behaviour —
ported from `cli/src/tui/app.rs` and held there by tests that assert what the
Rust ones assert. What is deliberately *not* ported is the machinery that only
makes sense on a terminal: byte-cursor editing, and a line-counted scrollback.
iOS supplies a real caret and a real scroll view; the rule worth keeping is that
**new output never yanks a reader back down**, and that survived.

**How it is verified, and what is left.** The behaviour is unit-tested, and the
built bundle is exercised in WebKit — the engine WKWebView uses — at an iPhone
viewport, so the cookie exchange, the SSE handshake, the touch targets and the
no-zoom rule are verified rather than asserted.

The parts no Linux machine can reach happen on a macOS runner instead of being
left as a promise: `.github/workflows/ios.yml` compiles the shell for
`aarch64-apple-ios`, generates the Xcode project, builds an unsigned simulator
app, and launches it. On Linux that compile stops at `objc2-exception-helper`,
whose build script needs the iOS SDK via `xcrun` — a licensing boundary, since
the SDK ships only inside Xcode.

What remains genuinely uncovered is a **device** build, which needs an Apple
developer certificate this repo does not hold. CI is simulator-bound and
unsigned by design. → [`apps/ios/README.md`](../apps/ios/README.md)

## Design rules

- **Jod delegates; harnesses think.** No model client in `jod-core`, ever.
- **Local first.** Every dependency is a local process or a local file.
- **Plain files at the boundaries.** Anything Jod stores stays readable with
  `cat` when Jod is not running — and the database is rebuildable from them.
- **Unknown input is surfaced, not swallowed.** `Raw` over silent drops.
- **A failed run must never look like a successful one.** Exit codes, empty
  answers and lost sessions are all checked, because every harness has at least
  one way of failing quietly.
- **The core has no UI.** If a client needs logic, it belongs in `jod-core`.
