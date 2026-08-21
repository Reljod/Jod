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
                     │              ▼
                     └────►  jod-core :: service::Jod
                                    │ setsid + spawn
                     ┌──────────────┼──────────────┐
                     ▼              ▼              ▼
                  jod-run        jod-run        jod-run     ← one per run,
                     │              │              │          detached
               claude -p …    opencode run…   agy --print …
                     │              │              │
                     └──────────────┴──────────────┘
                          stdout ─► parse ─► AgentEvent
                                    │
                                    ▼
                          ~/.jod/jod.db  (SQLite, WAL)
                                    │ poll (WAL: readers never block)
                                    ▼
                        every client — TUI, API, phone
```

Note which way the arrows run. Nothing reads a run *through* the process that
started it, so "watch this agent" is a query rather than a privilege held by one
process.

## The pillars

| # | Pillar | What it is | Status |
|---|--------|-----------|--------|
| 1 | **Brain nodes** | Flat markdown notes in Open Knowledge Format | Planned |
| 2 | **Brain connections** | A graph over those notes | **Built** — `entities` + `relations` over `facts` |
| 3 | **Jod** | The orchestrator that delegates and reports | **Built** |
| 4 | **Agents + A2A** | Harness-run agents that talk to each other | **Built (single-agent)** |
| 5 | **Memory** | What Jod knows, and what it did | **Built** |
| 6 | **Conversations** | A transcript Jod owns: list, fork, revert, compact | **Built** |
| 7 | **Time** | Schedules that fire, goals that persist, heartbeats that reap what wedges | **Built** |
| 8 | **Inbound** | GitHub events, and Telegram messages that are turns in the main chat | **Built** |

Pillar 2 arrived early and by a different route than planned. It is a graph over
*facts* rather than over markdown notes — a derived, rebuildable index in the
same `jod.db`, answering "what is related to this" and "how are these two
connected", which a list of facts cannot answer at all. There is no SQLite graph
extension worth having: none is simultaneously maintained, permissively
licensed and statically linkable into one binary, and plain tables with a
recursive CTE walk a million edges fast enough that one would buy nothing.
→ [why](decisions.md#the-graph-is-an-index-and-the-extension-was-not-worth-buying)

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

### How a run is supervised

Every agent runs under its own **`jod-run` supervisor**: a small process that
Jod `setsid`s into a session of its own, hands one file — the run's
`spawn.json` — and then forgets about. The supervisor spawns the harness with
its output piped, parses each line through the harness adapter, and appends the
resulting events straight into `jod.db`.

Four properties fall out, and they are the same four tmux used to provide:

- **Observability** — `jod watch <id>` follows a live agent. It reads the store,
  so unlike `tmux attach` it needs no shell on the box: the web client and the
  phone show the same run through the API.
- **A kill switch that works** — the supervisor leads its own process group, so
  its pid *is* its pgid. `kill(-pgid, SIGTERM)` stops the agent and every
  process still in that group, from any process, whether or not Jod is running.
  A run started by delegation leads a session of its own and is outside that
  group, so Jod walks the fleet down to it and stops it separately: stopping a
  manager stops its workers, and theirs, to the bottom. The main chat is the
  exception and stops alone, because it hands work out rather than owning any.
  Continuing a stopped agent starts its workers again. → `Jod::kill_agent`,
  `Jod::resume_cascade`.
- **Survivability** — `setsid` gives the run no controlling terminal, so closing
  an SSH connection cannot `SIGHUP` it. On a VPS this is the difference between
  an assistant and a foreground script.
- **One transport for every client** — one SQLite file, which in WAL mode lets
  every reader poll while the supervisor writes.

**There is no shell in the path.** The plan is `execve`'d directly, so a prompt
containing quotes or `$(...)` is simply an argument — the whole class of quoting
bug the old generated launcher had to defend against cannot arise.

**The supervisor is a separate binary, and that is not incidental.** A thread
inside `jod` could not hold the harness's stdout pipe past its own process's
death, so the agent would die with the terminal that launched it —
worse than tmux, not better. → [why](decisions.md#a-run-is-a-detached-process-group-and-the-database-is-its-only-transport)

**A run reports how it ended, always.** The supervisor writes a terminal event
and a status for a clean exit, a non-zero exit, a signal, and a harness that
never started at all. `SIGTERM` before `SIGKILL` exists for exactly this: a
supervisor killed outright records nothing, and the run would sit marked
running for ever.

### The harness seam

```rust
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;
    fn takes_system_prompt(&self) -> bool { false }
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}
```

`takes_system_prompt` defaults to `false` because that is both the safe answer
and the true one for most CLIs. A harness that never learned the flag would
otherwise drop the framing on the floor, and framing that vanishes silently is
worse than framing in the wrong place, so the runner folds it into the prompt
for everyone who answers `false`.

Adding a harness means one file. Nothing above the seam changes, because every
harness is normalised into one vocabulary:

`Started · Thinking · Progress · Delta · Message · ToolCall · ToolResult ·
Finished · Raw · SessionLost · Error`

Three of those exist because a transcript that goes quiet is indistinguishable
from a process that died. `Progress` is a bare tick carrying at most a
reasoning-token count, for a turn that thinks for nine minutes and emits nothing
else. `Delta` is a fragment of a block the harness has not finished writing,
which is what covers a long *write* rather than a long think — seven `Write`
calls in a row, each streaming a whole file. `SessionLost` is how a harness
says the conversation it was asked to resume is gone.

`Raw` matters more than it looks: an unrecognised line is *surfaced*, never
dropped. All three harnesses print human-readable prose onto the same stream as
their JSON, and `Raw` is why that prose reaches you instead of vanishing.

**The runner owns "the run is over", not the harness.** Completion is detected
from the process exit marker, and each adapter reports its accumulated answer
and cost in `finalize`.

### The three harnesses

| Harness | Invocation | Resume | Cost reported |
|---|---|---|---|
| **Claude Code** | `claude -p … --output-format stream-json --verbose --include-partial-messages` | `--continue` / `--resume <id>` | yes |
| **OpenCode** | `opencode run --format json …` | `--continue` / `--session <id>` | yes |
| **AGY** (Google Antigravity) | `agy --print … --output-format stream-json` | `--continue` / `--conversation <id>` | no |

Session resume is normalised behind one `Resume` field — `Fresh`, `Last`, or
`Session(id)`. Each harness spells it differently; the seam hides that. This is
what makes `jod chat` a conversation rather than a series of unrelated one-shot
tasks.

**This page used to add "and it is why Jod needs no memory of the transcript:
the harness owns it." That is no longer true, and the reversal is deliberate.**
It held while a conversation was a line you could only continue. It fails the
moment you want to fork, revert, or move a thread to a different harness,
because a session id issued by Claude Code means nothing to OpenCode. Probing
the three binaries directly:

| | Claude Code | OpenCode | AGY |
|---|---|---|---|
| fork a session | `--fork-session` | `--fork` | **none** |
| assign a session id | `--session-id <uuid>` | none | none |
| export a transcript | — | `opencode export` | none |
| accept one back | `--input-format stream-json` | `opencode import` | none |

Two of the three can fork themselves, and Jod delegates that rather than
reimplementing it. **None of them can hand a thread to another**, and AGY can do
none of it — so cross-harness handoff has no owner unless Jod is the owner. Jod
now keeps the transcript as a DAG with a moving head pointer, which is the shape
git, ChatGPT and LangGraph converged on and the harnesses did not.
→ [why](decisions.md#jod-owns-the-transcript-now)

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

`jod tui` is a full-screen interface built on ratatui. It is no longer a chat
window with a panel bolted on: it is **eleven workspaces**, and chat is one of
them.

| Digit | Workspace | What it is |
|---|---|---|
| 1 | Chat | the transcript, the input box, the status bar |
| 2 | Fleet | every run, as a tree over the conversations behind them |
| 3 | Memory | what Jod knows |
| 4 | Schedules | cron-triggered runs |
| 5 | Goals | looping objectives |
| 6 | Hooks | webhook rules |
| 7 | Tasks | the board |
| 8 | Activity | what happened while you were away |
| 9 | Team | the roster and its board |
| — | MemoryGraph | one memory node with its neighbours, reached with `g` |
| — | Traffic | one work's agent-to-agent messages, threaded, reached with `T` |

The last two have no digit on purpose. Neither means anything without a node or
a work already picked, so there is nowhere for a digit to land.

Nine of the eleven answer a digit, and all of them answer the leader key. The
fleet still shows runs from earlier processes, because `rehydrate` puts them
back.

**The keymap is Ctrl throughout, and that is a decision with a cost.** The
verbs were briefly on Alt to stay clear of a multiplexer that eats Ctrl chords
before this process sees them. That fixed the wrong half of the problem: on
macOS, Option does not send Alt unless the terminal is specially configured, so
the chords could not be typed at all. A binding nobody can press is worse than
one tmux eats, because the second at least has a workaround.

So the verbs are Ctrl, minus what something else already holds — tmux's prefix
and pane keys, the terminal's `Ctrl-Q`/`Ctrl-Z`, `Ctrl-I`/`Ctrl-M` for Tab and
Enter, and readline's `Ctrl-C/D/E/U/W`. Eleven letters survive: **B F G N O P R
T V X Y**. There are eighteen verbs.

They do not fit, so what keeps a letter is decided by what a chord is *for*. The
chat box turns every bare key into text, so a chord buys exactly one thing: a
verb you can reach without stopping the sentence you are typing or looking away
from the run you are watching.

| Chord | What it does |
|---|---|
| `Ctrl-B` | delegate the typed line |
| `Ctrl-X` | stop the run being watched |
| `Ctrl-Y` | copy the last reply |
| `Ctrl-T` · `Ctrl-O` | show or hide reasoning · what tools returned |
| `Ctrl-V` | dictate — a switch, not a button |
| `Ctrl-R` · `Ctrl-N` | show or hide the decision rail · its next card |
| `Ctrl-P` | add a directory this session may work in |
| `Shift-Tab` | show or hide the side panel |
| `Ctrl-↑↓` | scroll the transcript |

Everything else is a *destination*, and destinations go behind the leader:
**`Ctrl-G` opens a menu, and one more letter lands you anywhere.** The
workspaces take `c f m s g h t a w`, and the verbs that could not afford a chord
take the rest — `n` new, `e` compose in `$EDITOR`, `j` background shells, `u`
the oldest unread, `l` clear the screen, `d` the project catalog. The letters
are free of the workspaces by construction, and a test stops a new screen
quietly taking one back.

`Ctrl-F` is the one destination that kept a chord, because the fleet is where a
delegated run goes and `Ctrl-B` `Ctrl-F` is a single thought.

**The eleven letters are now spent**, which makes the next verb a decision
rather than a default: either it is a destination and goes behind the leader, or
something already holding a letter is demoted. The menu is the pressure valve,
and it has letters left.

What did not move is the handful of chords the terminal itself taught everyone.
`Ctrl-C`/`Ctrl-D` quit, `Ctrl-U` clears the line, `Ctrl-W` deletes a word, and
`Ctrl-A`/`Ctrl-E` go to the ends of it. Moving those would break forty years of
muscle memory to solve a problem nobody has.

Two tables generate both the always-on keybar and the `?` overlay, so the two
cannot disagree. The keybar reserves the way out *first* and spends what is left
on verbs, dropping whole chords rather than half of one — half a chord teaches a
key that does not exist — and saying `? more` when it has dropped any.

**One of its conversations is the main chat, and you can now be in it.** The
pinned conversation was reachable only by *sending* to it — `jod main "…"`,
`/main <instruction>` — and readable only as a static dump, so the one
conversation that never ends was the one nobody could sit in. It is now a
destination: the fleet's first row, `⏎` to enter, `/main` with no argument as
the keyboard route, `/new` to leave. Inside it a typed line goes to the
orchestrator, because that is what being in it means; in every other
conversation the chat box behaves exactly as it always did.

`jod tui --team <name>` fills the team workspace — `Ctrl-G w`, or digit `9` —
with the team's members, their harnesses and statuses, and the task board. It is
read from the store on every refresh rather than kept in memory, because
teammates run in their own processes and no in-memory copy could be
authoritative.

### Running several agents at once

The UI is built for unattended work, which means three things a chat window does
not need.

**Delegating.** `Ctrl-B` — or `/delegate` — sends the typed line to an agent that
never takes over the screen. It always starts a fresh conversation: a background
job that silently continued the conversation on screen would inherit context
nobody gave it, and two agents writing into one session is not a conversation.
When it ends, a notice says which one, how it went and how long it took, because
the whole point of delegating is that you were not watching.

**The fleet as a control surface.** `Ctrl-F` is a cursor over every run, with
the main chat as a permanent first row — `⏎` there goes into the chat, and the
run verbs say why they do not apply rather than doing nothing. The chat's own
runs, one per instruction, are collapsed into that row instead of filling the
list with copies of itself. The cursor starts on the first *agent*: managing the
work is what opening this list means, and the chat is one `k` away.

It is the widest screen in the program, because it is the only one that is both
a list of runs and a handle on the conversation graph behind them. `s r d a` act
on the run under the cursor — stop, resume, delegate, and the `jod watch` line
for following it from another terminal. `c b u U g f t` act on its thread: list conversations, open the branches
of one, undo and redo, go to a numbered branch, fork it, retry the run. `T`
opens that work's agent-to-agent traffic, which is the only way to answer what a
group of agents are saying to each other rather than merely watching them spend
money. `→←`, `space` and `E` move around the tree itself.

Resuming carries the harness with it, since a session id belongs to the harness
that issued it. The same verbs reach the keyboard as `/watch`, `/stop` and
`/attach`, where an id prefix is enough and an ambiguous one is refused rather
than guessed: stopping the wrong agent is not undoable.

**Never being made to wait.** A prompt typed mid-turn is queued and sent when the
turn ends, rather than refused — the old behaviour left it in a blocked box,
which made sitting still the only thing to do while an agent worked. `↑`/`↓`
recall what was sent, as every shell does, so scrolling the transcript moved to
`PageUp`/`PageDown`, the mouse, and `Ctrl` with an arrow. `Ctrl-X` stops the run
being watched, because `Ctrl-C` is quit and otherwise the only way to interrupt
an agent is to leave. Quitting warns about *every* running agent, not just the
one on screen.

The panels are modal while open: their letters are commands, not text. A panel
you can only look at makes you leave the UI to act on what you saw.

A 250ms tick drives a spinner and an elapsed counter, and re-reads the fleet once
a second. Without it a ten-minute run and a hung one look identical, and a panel
that only refreshed when the watched agent finished showed a fleet that stopped
moving minutes ago.

### Slash commands

Typing `/` opens a completion popup — `Tab` completes, `↑↓` choose — and
arguments are completed too, so `/harness ` offers the three spellings rather
than expecting them to be remembered.

There are forty-seven of them, and one table in `cli/src/tui/command.rs` is both
the help text and the completion list, so the two cannot disagree.

**The conversation itself**

| | |
|---|---|
| `/harness <name>` | claude, opencode or agy — takes effect next turn |
| `/login [name]` | sign in to a harness; no argument means the one this conversation is on |
| `/model <name>` | set the model; no argument restores the harness default |
| `/mode [name]` | plan, ask, edits or auto; no argument cycles, which is what Tab does |
| `/thinking` · `/details` | show or hide reasoning, and what tools returned |
| `/config [key] [value]` | preferences that outlive the session |
| `/new [kind]` | a fresh conversation, or a new schedule, goal, hook or task |
| `/sessions` · `/resume <id>` | conversations you can pick up, and picking one up |
| `/main` · `/main <instruction>` | go into the main chat · send it one instruction and stay where you are |
| `/clear` | empty the screen and start the next message with no context behind it |
| `/help` · `/exit` | |

**Where the work happens**

| | |
|---|---|
| `/add-dir [where]` | pick a folder this session can work in and `@` |
| `/root [add\|rm]` | the directories this session works in |
| `/project [add]` | the repositories an instruction with no path resolves against |

**Agents**

| | |
|---|---|
| `/delegate <prompt>` | run it in the background, same as `Ctrl-B` |
| `/agents` | the fleet |
| `/watch <id>` · `/stop <id>` · `/attach <id>` | act on one agent, by id prefix |
| `/heartbeat <id> [off]` | reap it if it goes silent — for runs left alone for hours |

**Standing work**

| | |
|---|---|
| `/schedules` · `/schedule <name>` | cron-triggered runs, and one of them |
| `/goals` · `/goal <name>` | looping objectives, and one of them |
| `/hooks` · `/hook <name>` | webhook rules, and one of them |
| `/run <name>` · `/pause <name>` · `/unpause <name>` | fire one now · stop it firing · arm it again |

**Memory, board and housekeeping**

| | |
|---|---|
| `/memory [query]` · `/remember <s> \| <p> \| <o>` · `/forget <name>` | what Jod knows, one new fact, one dropped node |
| `/tasks` · `/todo <title>` · `/done <task-id>` | the board, one task on it, one finished |
| `/team` · `/activity` | the roster · what happened while you were away |
| `/jobs` · `/reload` | background shells · restart the console into the jod now on disk |
| `/update` · `/upgrade` | rebuild and install the newest patch · download the newest release |

Two rules keep the set honest. **A command exists only if Jod can do it**: there
is no `/compact`, `/undo`, `/share` or `/themes`, because a command that
silently does nothing is worse than one that is absent — an unknown `/word` is
named back rather than sent to the agent as a prompt, and a test names those
four and fails if any appears. And **switching harness starts a fresh
conversation**, because a session id belongs to the harness that issued it;
carrying it across would try to resume a conversation the new harness has never
heard of.

Arguments are completed too, and the lists come from live state rather than a
constant: `/stop` offers only agents that could actually be stopped, `/model`
offers what this harness said it accepts, `/schedule` and `/goal` offer what
exists by name. Retyping a name off the screen above is not a user interface.

Parsing and completion are pure functions over a string, so the whole of "what
did the user ask for" is tested without a terminal.

#### What OpenCode has that this does not

Written down so the gap is a decision rather than an oversight. Nothing here is
stubbed: a command Jod cannot honour is absent, and an unknown `/word` is named
back rather than quietly accepted.

**Since closed.** Four rows of this table have been built and are listed here
rather than deleted, because a gap that closed is the evidence that the rest are
work rather than excuses:

| Was missing | Where it landed |
|---|---|
| `@file` references | `cli/src/tui/mention.rs` — an inline picker, ranked and highlighted live, over the session's roots rather than the process's working directory. |
| Compose in `$EDITOR` | `Ctrl-G e`, not a slash command. It needs the terminal, so only the main loop can lend it. |
| Leader keys | `Ctrl-G` opens the menu and one more letter lands anywhere. It covers all nine numbered workspaces plus six verbs. |
| Model listing | `opencode models` and `agy models` are asked and parsed; Claude Code has no such subcommand, so its list is the static catalogue in `core/src/harness/models.rs`. The list is an aid, not a gate — an unlisted name still passes through. |

**Still buildable — nothing is in the way but the work:**

| Missing | What it needs |
|---|---|
| `/export` | The transcript is already in SQLite; this is a formatter. |
| A general command palette | The completion popup generalised past `/`. `Ctrl-P` is no longer free — it adds a directory — so this needs a key as well as the work. |
| `/themes` | Colours are deliberately the terminal's own eight, so that Jod reads on someone else's box. A theme system means giving that up first. |

**Half-built:** `/sessions` opens the fleet and tells you to `/resume <id>`; it
is not yet a picker you can arrow through. The id it shows does work — `/resume`
takes a prefix of either the agent id on screen or the harness's own
conversation id and resolves it. It refuses an ambiguous prefix, and refuses an
agent that has never reported a conversation, because resuming that would
silently start a fresh one instead of continuing anything.

**Blocked on something Jod does not have:**

| Missing | Why |
|---|---|
| `/undo`, `/redo` | Reverting file changes needs snapshots of the working tree. Jod keeps a transcript, not a filesystem history — and git already does this properly. |
| `/compact` | No harness exposes conversation compaction through its headless interface. It is the harness's own context to manage, which is the seam working as intended. |
| `/share`, `/unshare` | OpenCode-specific hosted sessions; there is nothing for Jod to share *to*. |
| `/connect` | Provider credentials belong to the harness. Jod never holds an API key, by design. |
| Reasoning-effort cycling | Each harness spells it differently — AGY `--effort`, OpenCode `--variant`, Claude model-side — so there is no uniform control to expose yet. |

The last two rows of that table are the harness seam's cost showing up in the
UI, and are the expected price of [delegating rather than owning the
loop](decisions.md).

### Watching the work

The transcript shows what the harness is *doing*, not just what it concluded:
a tool call carries the most useful field of its arguments — `Bash · cargo
test`, not a bare `Bash` — and what the tool gave back is shown underneath it,
trimmed to a few lines. `/details` turns the output off for a quieter view; a
*failed* tool is shown either way, because it is the reason the answer is about
to be wrong.

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

Around forty tables now, migrated forward in place. The ones worth knowing by
name, grouped by what they answer:

**What ran**

- **`events`** — every agent event, append-only, unique on `(run_id, seq)` so a
  replayed stream cannot duplicate history.
- **`runs`** — one row per delegation, so a restarted process still knows what
  it launched.
- **`works`** · **`delegations`** · **`cascaded_stops`** — the tree above a run:
  which work it belongs to, who delegated it, and what stopping a manager
  reached.
- **`conversations`** · **`conversation_roots`** · **`compactions`** — the
  transcript DAG, the directories each thread may work in, and what was folded
  away.

**What Jod knows**

- **`facts`** — subject, predicate, object, bitemporal and scoped.
- **`entities`** · **`relations`** · **`entity_community`** — the graph over
  those facts. Derived and rebuildable, which is why deleting it costs nothing.
- **`tombstones`** — proof that a deletion happened, after the fact is gone.

**What is contended**

- **`tasks`** — claimed with a single guarded `UPDATE`. Zero rows changed means
  you lost the race. Never a read then a write.
- **`leases`** — worktrees an agent has claimed, so two agents do not edit one
  checkout.
- **`team_members`** · **`team_messages`** — the roster and the bus.
- **`delivery_ledger`** · **`pending_deliveries`** — what has been handed to an
  agent, so nothing is delivered twice or lost on a failed spawn.

**What fires on its own**

- **`schedules`** · **`schedule_fires`** — cron rules and every firing.
- **`goals`** — looping objectives and their iterations.
- **`webhook_rules`** · **`webhook_deliveries`** — inbound GitHub events.
- **`monitors`** · **`monitor_checks`** — one row per schedule, answering
  whether a firing should become an agent run at all.
- **`heartbeats`** — what reaps a run that has gone silent.

**Everything else**

- **`cards`** — questions waiting for a human, which is what the rail draws.
- **`grants`** — standing permission, so a tool approved once is not asked
  about again.
- **`secrets`** · **`settings`** — values an agent may request by name, and
  preferences that outlive a session.
- **`projects`** · **`project_resolutions`** · **`pull_requests`** — the
  repository catalog, how a pathless instruction was resolved, and PR state.
- **`discovered_commands`** — slash commands and skills found under a root or in
  the user's own config, so the palette offers what a repo already defines.
- **`channel_sessions`** — a Telegram chat's place in a conversation, which has
  to outlive the process that answered the last message.
- **`migrations`** — which of the above have been applied.

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
run still marked running whose process group is gone did not report a result and
becomes *failed*, rather than running forever. Only a run that still claims to
be running is probed at all — pids are recycled, and asking about a finished
run's long-dead pgid is how a stranger's process gets mistaken for an agent.

A run that *is* still alive is picked back up: a follower starts on it, so its
remaining events reach this process's clients as they arrive. The old file
tailer could never do that, because it had to belong to whoever spawned the
agent.

`Jod::events_since(id, after_seq)` serves a reconnecting client only the tail it
missed, from memory when this process owns the agent and from the database when
it does not.

---

## Pillar 4 — Agents and A2A

**Built:** agents run under all three harnesses, each in its own supervised
process group, each managing its own context.

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

### Auto-wake

`jod team wake <team>` delivers waiting mail by resuming every idle member that
has some, which is what makes a team react rather than sit there.

```sh
jod team start crew scout "read the parser and report"   # first turn
jod team msg   crew --from lead --to scout "what did you find?"
jod team wake  crew                                      # resumes that session
```

The decision of *whether* to wake is a pure function, `team::wake_order`, kept
separate from the spawning so the judgement in it can be tested without a
supervisor or a harness. It declines in four cases, each on purpose:

- **Nothing waiting** — waking an agent to tell it nothing burns a turn.
- **Not idle** — a busy member reads its inbox on its next turn anyway, and
  resuming a conversation that is mid-turn would fork it.
- **Shutting down or failed** — waking it would undo the request.
- **No session id** — the important one. Spawning without one starts a *fresh*
  context, so the member would answer having forgotten everything. Staying
  asleep holding visible unread mail is better than answering with amnesia.

That last rule is why `jod team start` exists: a member has no conversation
until it has run once, and `start` is the first turn that creates one.

**Both commands wait for their runs by default.** This used to be *forced*: the
task that recorded a run's events belonged to the process that spawned it, so a
command that returned early took the tailer with it — no `Finished` event would
ever be written, and the member stayed marked busy for ever, never eligible to
be woken again.

That constraint is gone. The supervisor records the run whether or not anything
is watching, so an early return now loses nothing. Waiting remains the default
because it is what lets the command mark the member idle before it exits, but it
is a choice again rather than a requirement, and `--detach` no longer trades
correctness for speed. Making `wake` non-blocking is follow-up work, not a
missing piece.

Mail is drained only *after* a spawn succeeds, so a failure leaves it waiting
rather than losing it.

**Agents reach the bus themselves.** Jod's MCP server exposes `send_message`,
`read_messages`, `roster`, `ask`, `reply` and `handoff`, so a teammate messages
another from inside a run with no human and no script between turns. Measured
across harnesses: an asker on Claude Code and an answerer on OpenCode exchange a
question and a reply, sharing one thread id, with the reply one hop deeper than
what it answers.

Those six are one part of a larger surface. `jod mcp` serves thirty-two tools in
all — delegating and stopping agents, reading and writing memory, creating
schedules and goals, claiming and releasing worktrees, switching project,
raising a card for a human and requesting a secret by name. Access is
fail-closed: `--access` decides how much of Jod the agent on the other end may
reach, and an unset flag gets the read-only set rather than the full one.

**The sender is the run, never an argument.** Jod's MCP server resolves its own
process group against `runs.pgid` — the server is a child of the harness, which
sits in the run's group — so identity is something the model cannot argue its
way into. A per-run config names the run as well, and the two are only ever
allowed to *agree*: a disagreement refuses the call and names both answers
rather than picking a winner, because quietly choosing is how a wrong answer
becomes a permanent one. An agent that passes `from`, `sender` or `as` has all
three ignored.

**Mail delivers itself.** The ticker consults `wake_order` for every member
holding waiting mail and resumes the idle ones; `wake_order` gained a caller,
not a rewrite. A member is resumed at most once per interval, so ten messages
arriving together become one turn carrying ten rather than ten turns — a cost
control and a coherence one, since an agent reading everything at once answers
better than one woken per line.

**Every conversation is bounded, and hitting a bound raises a card rather than
killing anything.** Depth in a thread, messages per work, and a deadline on any
wait. Two agents in a polite loop are a way to spend money at machine speed, and
the failure is invisible because every individual message looks reasonable.
Waiting for a reply is always bounded: an agent that can hang waiting for a peer
is an agent that can hang for ever, because the peer might be dead.

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

1. ~~Core service: harness seam, process supervision, event normalisation~~
   **done**
2. ~~Third harness (AGY) and normalised session resume~~ **done**
3. ~~Durable runs, transcripts and memory in SQLite~~ **done**
4. ~~CLI: delegate, watch, list, report, remember, recall, chat~~ **done**
   ~~plus `jod tui`, the full-screen interface~~ **done**
5. ~~Browser access for agents — Camoufox, headless, verified~~ **done**
   (awaiting Webshare ISP credentials to fix the egress IP →
   [`browser.md`](browser.md))
6. ~~A2A inbox/outbox + MCP server~~ **done** — `core/src/mcp.rs`, served by
   `jod mcp`, which exposes thirty-two tools across delegation, memory,
   schedules, projects, worktrees and the message bus. Access is fail-closed:
   an unset `--access` flag gets the read-only set.
7. ~~Scheduled work: recurring delegations~~ **done** — `schedules` fire on
   cron, `goals` loop until they are met, and the ticker drives both.
   `monitors` decide whether a firing should become a run at all. There is no
   digest yet; that is the part of this line still outstanding.
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

- **An iPhone cannot host an agent.** No `claude` binary, no shell to run one
  in, no way to keep a process group alive. So this client does not embed `jod-core` the way `apps/desktop`
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
completion list, the live tool output, the agents and team panels, and the
status line are the same behaviour — ported from
`cli/src/tui/{app,command,mod}.rs` and held there by tests that assert what the
Rust ones assert, case for case.

It is not the same *surface*, and the gap has widened. The phone carries twelve
slash commands against the console's forty-seven, and it is the twelve that read
or steer one conversation: harness, model, thinking, details, new, sessions,
resume, agents, team, clear, help, exit. Everything the console grew since —
schedules, goals, hooks, grants, roots, projects, dictation, the rail — either
needs a process group on the box or needs a keyboard, and a phone has neither.
A phone watches the work and answers it; it does not administer it.

What is deliberately *not* ported is the machinery that only makes sense on a
terminal, and in each case the *rule* crossed over while the *mechanism* did
not: byte-cursor editing (iOS has a real caret), a line-counted scrollback (a
real scroll view, but **new output still never yanks a reader back down**), and
a highlighted suggestion moved with `Tab` and the arrows (the finger goes
straight to the row). `/exit` cannot quit an iOS app, so it does what the TUI's
`/exit` actually achieves: stop watching, leave the agent running.

**Teams needed the daemon to grow two routes.** `Ctrl-G` shows a cross-harness
team, which the TUI reads straight out of SQLite — impossible from a phone. So
`jod-api` now serves `GET /v1/teams` and `GET /v1/teams/{team}`, both read-scope,
returning the roster and the board in one answer. Nothing else was added:
joining, claiming and messaging are how a *teammate* participates, and a
teammate is an agent on the box with a process group. A phone watches the board;
it does not play on it.

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
- **Plain files at the boundaries.** What was asked (`prompt.txt`) and what was
  run (`spawn.json`) stay readable with `cat` when Jod is not running. A run's
  *transcript* is the exception, and deliberately so: it is contended state that
  several processes append to and read, which is the one job a file cannot do.
- **Unknown input is surfaced, not swallowed.** `Raw` over silent drops.
- **A failed run must never look like a successful one.** Exit codes, empty
  answers and lost sessions are all checked, because every harness has at least
  one way of failing quietly.
- **The core has no UI.** If a client needs logic, it belongs in `jod-core`.
