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

`jod tui --team <name>` adds `Ctrl-G`: the team's members, their harnesses and
statuses, and the task board. It is read from the store on every refresh rather
than kept in memory, because teammates run in their own processes and no
in-memory copy could be authoritative.

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

**Built: agent teams.** `core/src/team.rs`, with the state in the same SQLite
file as everything else. A team has members, a message bus, and a shared task
board; `jod team` drives it and `Ctrl-G` in the TUI shows it.

```sh
jod team join crew lead   --harness claude   --role coordinator
jod team join crew scout  --harness agy      --role research
jod team join crew builder --harness opencode --role implement
jod team task crew t1 port the parser
jod team claim t1 scout        # exits non-zero if someone else already has it
jod team msg crew --from lead stand up please
jod team inbox crew scout      # drains, so a message is never replayed
```

Every harness is growing a team feature of its own, and each one can only ever
contain that harness. Jod owns the bus instead, which buys the thing none of
them can do alone: **one team whose lead runs on Claude Code and whose teammates
run on AGY and OpenCode.** The command above is that, and it works today.

Two operations are contended and both are single statements rather than
read-then-write, for the reason the rest of the store already documents:

- **Claiming a task** reuses the existing `claim_task`, whose `owner IS NULL`
  guard makes two agents racing produce one winner.
- **Draining an inbox** selects and marks delivered in one transaction, so the
  same instruction is never injected into two turns.

Delivery is deliberately dumb: a message becomes a synthetic user turn in the
recipient's next prompt. Because every harness resumes a session by id, that
works on all three without any harness knowing teams exist.

**Still to build:** auto-wake — a message to an idle member should resume it
rather than waiting for something else to. Today a member picks up its inbox on
its next turn. An `jod-mcp` server exposing `send_message` / `read_inbox` to
agents directly is the natural next step after that.

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
8. HTTP API for mobile and web clients
9. Brain nodes and connections

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
