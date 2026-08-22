# SPEC — main only delegates, an assistant does the routing, and scratch work cleans up after itself

> **Shipped.** Kept because one code comment cites `SPEC B4` and because the
> numbered checks below are the names the tests still carry. It is a record of
> what was built, not work to pick up.
>
> It arrived here from the branch `feat/unblock-main-and-roles`, where it sat as
> a root `SPEC.md`. Everything below is as written **except** the four places
> building it proved the spec wrong, each corrected in place and marked: C3 (the
> whole section — the thinking spike overturned its premises), B4 (the loose pane
> is runs, not tree nodes), check 18 (the stalled rule rests on run status, not
> on the mark surviving), and check 29. Two checks are **not run**: 31 and 32
> need a person at a terminal.

Three changes that share one story. Main stops holding its own conversation
while work happens, non-project work gets a lane of its own that tidies itself
away, and which harness and model each layer of the chain uses becomes something
you configure on a screen instead of something a model picks in the moment.

## The problem, in Reljod's words

You type a second instruction into main and get back `queued — sends when this
turn ends (2 waiting)`. Main is waiting on something, and you cannot use it while
it waits. **Main should be free from any blockage.**

Two things follow from that.

- Non-project work should go to an **ephemeral scratch session** rather than
  being done inside main's turn. It spins up under main, shows in the fleet
  view, and archives itself when it finishes — unless you tell it to stay, or it
  gets stuck.
- Delegation should be able to use **a different harness** per layer, configured
  in **its own panel** beside the fleet, laid out as the chain of command with
  the harness, model and thinking level on each row.

## Where the block actually comes from

Not a hang. `cli/src/tui/mod.rs:5790`:

```rust
if app.busy {
    app.queue(prompt);
    app.push(Entry::Notice(format!(
        "queued — sends when this turn ends ({} waiting)", app.queued.len())));
    return None;
}
```

`app.busy` is true for as long as main's turn is in flight, and main's turn today
*is* the routing decision. `hand_to_orchestrator` (`core/src/orchestrator.rs:936`)
spawns a run on main's own pinned conversation carrying
`orchestrator_preamble()` and `ToolAccess::Orchestrate`. That run reads the
fleet, decides, hands over, and only then does the box free up.

When it goes wrong it goes wrong badly. `tasks/01-routing.md` R4 records main
sitting in a shell loop waiting for its own child:

```
Bash · until [ "$(jod agents --json | grep -c a2e4f620)" = "0" ]; do sleep 5; done
Bash · sleep 45; echo waited
mcp__jod__list_agents · {"limit":2}      (repeatedly)
```

Forty-two seconds and $0.39, mostly asleep, and it still ended without the
answer. R1 is the same hole from the other side: main has no branch that says
"just answer this", so a one-line question sometimes costs a spawned agent and
sometimes does not, depending on the model's mood that day.

## What is already true — do not rebuild it

- **The CEO and manager layers shipped.** `docs/spec-ceo-and-managers.md`.
  Managers are pinned-per-project conversations reached through `ask_manager`,
  `open_work` is refused from main, and a stalled run is marked rather than
  killed. None of that changes here.
- **`delegate` already takes a harness and a model.** `core/src/mcp.rs:1274`.
  It defaults to `HarnessKind::ClaudeCode` and nothing configures it ahead of
  time. The plumbing exists; the configuration surface does not.
- **`SpawnRequest` already carries `env`, `model`, `harness` and `permission`.**
  `core/src/harness/mod.rs:207`. A per-role default has somewhere real to land.
- **The TUI already has a family of panels.** `Workspace` in
  `cli/src/tui/workspace.rs:11` is `Chat | Fleet | Memory | MemoryGraph |
  Schedules | Goals | Hooks | Tasks | Activity | Team | …`. A roles panel is a
  new member of that family, not a new kind of thing.
- **Ephemeral conversations already exist, once.** The titler
  (`Store::open_titler`, `core/src/works.rs:945`) is created, answers, and is
  deleted, and a crash-orphaned one is recognised by
  `conversations.origin = 'titler'`. The compaction run is hidden from the fleet
  the same way (`works::is_housekeeping_run`, `core/src/works.rs:431`).
- **Nothing called `ephemeral`, `held`, `archived_at_ms` or `roles` exists in
  the schema.** Checked, so none of Epic B or C collides with a name already in
  use.
- **The fleet's loose pane already holds runs belonging to no work.** That is
  where scratch sessions belong. Nothing new needs drawing for them to appear.

## Decisions Reljod made

Do not reopen these.

- **The CEO is main.** The `ceo (jod)` node in the diagram is the pinned main
  conversation. The `assistant` node is a **separate layer below it**, not
  another name for main's own turn.
- **Main only delegates.** It hands the instruction to an assistant and returns.
  It does not route, it does not choose a project, and it does not answer.
- **The assistant runs on a small model.** Routing is a cheap decision and
  paying Opus prices for it is the thing this fixes.
- **The roles panel is a TUI panel shaped like a tree**, not a free-form canvas.
  Nesting carries the edges.
- **A finished scratch session is hidden and then deleted** after a retention
  window, rather than kept forever or destroyed on the spot.
- **The assistant chooses whether to reuse a scratch session or start a new
  one**, and only recent ones count. The choice is the assistant's judgement, not
  a rule in the code — the code only decides which sessions it is allowed to
  choose from.
- **The no-blocking rule is enforced at the tool boundary**, not left to
  preamble wording. A rule the model can talk its way past is not a rule.

## One thing this spec assumes, stated so it can be corrected

The diagram says the CEO does `only delegation`, so **main answers nothing
itself** — even "what does A2A stand for" goes to the assistant, which answers it
directly rather than spawning anything. This is what closes R1: the branch that
decides whether to answer or hand over now lives in exactly one place, on a
cheap model, instead of being main's judgement call on the day.

The cost is a hop on trivial questions. It is worth it because the hop is
cheap and the current behaviour is a coin flip.

---

# Epic A — main hands over and returns

## A1. The assistant conversation is fresh every time

A manager is a *standing* conversation, resumed for each instruction, because a
manager's value is the project context it accumulates. An assistant is the
opposite: **a fresh, ephemeral run per instruction, with no memory of its own.**

That is the whole point. A standing assistant would serialise — instruction two
would wait for instruction one, and the block would move down a layer instead of
going away. A fresh assistant per instruction means several are in flight at
once, and main's turn is over as soon as the hand-over starts.

An assistant needs no memory because the state that matters is not in its
transcript. It is in `list_agents`, the project catalog, and the fleet. It reads
those.

- The assistant conversation is created with `origin = 'assistant'`,
  `ephemeral = 1` (Epic B), and `parent_conversation_id` set to main.
- It is not pinned and it is never resumed.

**The assistant is never reused. The scratch sessions underneath it are** —
that is A6. The two are easy to conflate and they pull in opposite directions:
a standing assistant would serialise every instruction, while a standing scratch
session is how a follow-up keeps its context.

## A2. `ask_assistant`

New MCP tool, main's only way to make anything happen:

```
ask_assistant { instruction: string }
```

- Creates the assistant conversation, spawns it with `assistant_preamble()`,
  and **returns as soon as the run has started**. It never waits for a result.
- Returns the run id and the assistant's name, so the row is findable in the
  fleet.
- `ToolAccess::Delegate`. It starts an agent, and the power to start agents is
  the thing an unattended run should hold least of.
- The instruction is passed through verbatim. Main does not summarise it,
  rewrite it, or resolve the project — `settle_project` already runs on the raw
  instruction before the turn, so the project is settled before anybody reads
  anything.

## A3. Two preambles change

**Main's** (`orchestrator_preamble`, `core/src/orchestrator.rs:178`) shrinks to
intake. Its toolbox becomes:

`ask_assistant`, `schedule_create`, `goal_create`, `recall`, `related`,
`remember`, `record_decision`, `ask_question`, `list_agents`, `stop_agent`.

`ask_manager`, `delegate` and `open_work` all leave main and move to the
assistant. Schedules and goals **stay with main** — that was decided in
`docs/spec-ceo-and-managers.md` (open question 4: arming a schedule spends money
at 2am with nobody watching) and nothing here changes the reasoning.

Keep the paragraph about Taglish and dictated speech. It is still main that
reads what Reljod actually typed.

**The assistant's** (`assistant_preamble()`, new) owns the routing decision that
used to be main's:

- Answer directly when the instruction needs no repository, no tool beyond
  memory, and nothing that outlasts this turn. Say the answer and stop.
- `ask_manager` for anything touching a repository.
- `delegate` for a one-shot that needs a tool but no repository — a lookup, a
  fetch, a calculation. This is what starts a scratch session.
- `continue_agent` instead, when a recent scratch session was working on this
  same thing and has finished. A6 has the rule and its two traps.
- Report back in one or two sentences. The answer reaches Reljod as a card on
  main's rail, which is the return leg that already exists.
- **You never wait.** Hand over and return. The answer arrives later as its own
  event.

## A4. Refusing to block, at the boundary

Prose is not enforcement. Two mechanisms, and they cover different ground.

**At the MCP boundary — repeated polling.** `list_agents` called a second time
by the same run inside one turn is refused, naming the reason: the answer
arrives as a card, return now. Applies to any run holding
`ToolAccess::Orchestrate` or `ToolAccess::Delegate`. One call is a legitimate
look at the fleet; the second is a poll loop starting.

**Through the harness's own hook — sleep-shaped commands.** `sleep`, `until`
loops and `while` loops in a Bash call are refused for those runs.

This part is **Claude Code only, and the spec says so rather than pretending
otherwise.** Bash is the harness's tool, not Jod's, so Jod's MCP server never
sees the call. Jod already hands runs in `ask` and `edits` modes a `--settings`
document carrying a `PreToolUse` hook (`jod approve-hook`,
`docs/harness-config.md`). The change is to emit that document for an
orchestrating or delegating run **in every mode including `auto`**, carrying
only the deny rule. OpenCode and AGY have no equivalent hook, so for them this
stays preamble wording. Main runs on Claude Code in practice, which is why this
is worth doing anyway.

## A5. What the queue looks like afterwards

The queue stays. It is correct behaviour, and it is not the bug — a queue that
drains in a second is not a block. What changes is that main's turn is one tool
call, so it drains in a second.

## A6. The assistant picks up a recent scratch session, or starts a fresh one

Reljod: *"The assistant will determine if it reuses existing ephemeral session or
new ones. It only considers those that are recent else, it creates new ones."*

**Most of this already exists and must not be rebuilt.** `continue_agent`
(`core/src/mcp.rs:229`) sends a follow-up to a run that already reported a
session id, and it already sits at `ToolAccess::Delegate` — the access the
assistant holds. `list_agents` already works out which runs are free
(`core/src/mcp.rs:1163`: status `completed`, has a `session_id`, is not a router)
and already writes a sentence telling the caller to reuse one. What is missing is
the recency bound and a rule that fits scratch rather than engineers.

**Why reuse is worth having at all.** A follow-up — *"what did you find?"*, *"try
the other spelling"* — sent to a fresh session is answered by something that has
never seen the question it is following up on. Reuse is also the cheaper call: no
cold start, no re-reading.

**A candidate is a scratch session that is all four of:**

1. `ephemeral = 1` and descended from main — a scratch row, not an engineer.
2. Free in the sense `list_agents` already means: its latest run is `completed`
   and reported a `session_id`.
3. Last active within `scratch_reuse_window_minutes` — a new key in `settings`,
   default 60. `0` disables reuse and makes every instruction fresh, which is the
   way back to A1-only behaviour if this turns out badly.
4. Not deleted by the retention sweep. Archived is fine — see B2.

**Two rules that are load-bearing, and both are easy to get backwards.**

**Never wait for one.** A *running* scratch session is not a candidate, ever. If
the only session on the right subject is busy, the assistant starts a new one
beside it. Reuse that waits for a session to free up is the original bug rebuilt
one layer down, which is the whole thing this spec exists to prevent.

**Same thread only — the opposite of the engineer rule.** The reuse sentence
`list_agents` writes today says to prefer a free agent *"for any instruction
here, including one on a different subject"* (`core/src/mcp.rs:1226`). That is
right for an engineer, whose value is a warm checkout that any instruction in
that repository benefits from. It is wrong for scratch, which has no checkout —
the only thing a scratch session carries is the subject it was talking about, so
reusing one across subjects buys nothing and pollutes the context.

So scratch needs its own sentence, saying the opposite: reuse this one only if
the new instruction continues what it was doing, otherwise open a new one. The
existing `last_message` field on `AgentView` is what the assistant judges that
from; it is already populated and no new field is needed for it.

**One bug this creates if it is done carelessly.** `is_free` matches a completed
scratch session just as happily as an engineer, so a scratch row would land in
the `idle` list and get advertised as a warm checkout by the engineer sentence.
`AgentView` gains `scratch: bool`, scratch rows are excluded from the `idle` list
that feeds the engineer hint, and the scratch hint is computed separately over
just those rows.

**The assistant's preamble** (A3) gets the rule in these terms: look once at what
is already running under you; if a recent session was working on this same thing
and has finished, continue it with `continue_agent`; otherwise `delegate` a new
one. Never wait for a busy one to free up.

This fits inside A4's single-call budget on purpose. The assistant gets one
`list_agents`, and that one response has to carry both the fleet picture and the
reuse candidates — which it does, because they are the same list.

# Epic B — the scratch lane

## B1. Two columns on `conversations`

- `ephemeral INTEGER NOT NULL DEFAULT 0` — this conversation is scratch. Set on
  assistant conversations and on anything `delegate` starts.
- `held INTEGER NOT NULL DEFAULT 0` — Reljod asked for this one to stay. Never
  auto-archived, never swept.
- `archived_at_ms INTEGER` — when it left the fleet. Null means it has not.

## B2. When a scratch row leaves the fleet

A scratch conversation is archived when **all three** are true:

1. Its latest run's status is `completed`. Not `failed`, not `killed`.
2. It has no `queued` rows in `pending_deliveries` — its report has actually
   reached main. Archiving before the answer lands would hide the row and lose
   the reply, which is R3b happening again by a different route.
3. `held = 0`.

It **stays visible** when it failed, was killed, is marked stalled by the
heartbeat sweep (`heartbeats.stalled_since_ms`), or is held. That is the
"or stuck" half of what Reljod asked for: a wedged scratch session must not
tidy itself away, because the whole reason to see it is that it went wrong.

Archiving sets `archived_at_ms` and nothing else. Nothing is deleted at this
point and the transcript stays readable.

**Archiving is not final, because of A6.** A scratch session can be archived and
then picked up again by `continue_agent` inside the reuse window. Starting a run
on a scratch conversation clears `archived_at_ms` back to null, and it re-archives
under the same three conditions when that run finishes. Archived therefore means
"finished and out of the way", not "closed" — the row comes back to the loose
pane for as long as it is working again.

The two windows have to stay in that order: the reuse window is minutes and the
retention window is days, so the sweep never deletes something the assistant was
about to continue. If anybody sets retention shorter than reuse, the worst case
is a `continue_agent` naming a run that has just been deleted, which already
fails with a plain error rather than doing damage.

## B3. Deleting, later

A sweep in `Ticker` deletes scratch conversations whose `archived_at_ms` is
older than the retention window, along with their messages and events. Default
seven days, stored in `settings` under `scratch_retention_days`. Zero means
never delete, which is the escape hatch for anyone who wants the old behaviour.

Held conversations are never swept, whatever their age.

## B4. In the fleet

Scratch rows appear in the **loose pane** — the pane below the tree that already
holds runs belonging to no work. No new drawing.

**Correction, found while building this.** An earlier draft of this section said
to hang `held` and `archived_at_ms` off `tree::Node`. That does not work.
`Store::forest_of` reads only conversations with `work_id IS NOT NULL`
(`core/src/tree.rs:595`), so a scratch conversation never becomes a `Node` at
all, and the loose pane is not built from the forest — it is `App::loose_rows`
(`cli/src/tui/app.rs:2637`), which filters the flat agent list by run id against
`Condensed::runs`. **A loose row is a run, not a tree node.** So the scratch
facts are exposed as `Store::scratch_lane()`, keyed by run id, which is the key
the rows actually hold. The fleet and anything else join against it.

- `k` toggles hold on the selected scratch row. A held row shows a marker and
  the key bar reads `k keep`. Releasing hold on a row that already satisfies B2
  archives it there and then.
- `z` currently reveals closed works. It also reveals archived scratch rows, so
  there is a way back to something you wanted to re-read before the sweep takes
  it.
- A held row on a finished run says so in its summary, so it is obvious why it
  is still there.

## B5. What this replaces

Nothing is removed. `delegate` behaves as it does now; what changes is that what
it starts is marked ephemeral and therefore cleans up. The titler and the
compaction run keep their existing hiding mechanism
(`works::is_housekeeping_run`) rather than being migrated onto this one — they
are deleted immediately and never wanted a fleet row at all.

---

# Epic C — the roles panel

## C1. What a role is

Six of them, and they are the chain of command:

| Role | What it spawns |
|---|---|
| `main` | main's own turn — `hand_to_orchestrator` |
| `assistant` | `ask_assistant` |
| `manager` | `ask_manager` |
| `engineer` | `open_work`, `continue_agent` |
| `scratch` | `delegate` |
| `housekeeping` | the titler, the compaction run |

`housekeeping` is on the list because it is the cheapest possible win for "use
smaller models". Summarising a transcript and naming a work do not need a
frontier model and currently get one.

## C2. The table

```sql
CREATE TABLE roles (
  role       TEXT PRIMARY KEY,
  harness    TEXT,
  model      TEXT,
  thinking   TEXT,
  permission TEXT
);
```

Every column is nullable and null means *inherit*. An empty table must behave
exactly like today, so that a machine that never opens the panel sees no change.

**Precedence, highest first:** an argument named in the tool call (`delegate`
with an explicit `model`) → the conversation's own `/harness` or `/model` →
the role's row → the harness's own default.

## C3. Thinking, honestly — **the spike ran, and this section was wrong**

The spike this section demanded has now happened, and it overturned the
premises. What follows replaces them.

**There is no environment variable and no token budget.** Two of the three
harnesses take a plain `--effort` flag, and the third takes a flag in the same
slot. `MAX_THINKING_TOKENS` appears nowhere in the codebase — the only hits in
the repo were inside this spec.

| Harness | Flag | Accepts |
|---|---|---|
| Claude Code | `--effort <level>` | `low, medium, high, xhigh, max` (v2.1.220) |
| AGY | `--effort <level>` | `low, medium, high` |
| OpenCode | `--variant <string>` | provider-specific — the help names `high, max, minimal` |

Three corrections to what this section used to say:

- *"Claude Code has no flag"* was true once and is false against the installed
  version, which is 2.1.220.
- *"AGY is unknown"* was never true. The repo already documented it at
  `docs/harness-config.md:202-205`, including that AGY models can also encode
  effort in the name (`gemini-3.6-flash-high`), so AGY has **two channels for one
  setting** and a role that sets both has two sources of truth.
- Only the OpenCode paragraph survives. `--thinking` really is a boolean that
  asks for reasoning *parts* to be emitted, it is passed unconditionally at
  `core/src/harness/opencode.rs:46`, and it is **not** the effort control.
  `--variant` is. Leave `--thinking` exactly as it is.

**Levels are `low | medium | high`.** `none` is dropped: no harness has a
spelling for it, and the honest way to say "don't set this" is to leave the
column null, which passes no flag and gets the harness's own default. That also
keeps the promise in C2 that an empty table behaves exactly like today.

`xhigh` and `max` are reachable on Claude Code only. The panel may offer them
when the row's harness is Claude Code and must not offer them otherwise.

**The one honest caveat, and it belongs to OpenCode.** `--variant` values are
chosen by whichever provider the model comes from, not by OpenCode, so
`low`/`medium` are not knowable to be valid from Jod's side. Therefore: the flag
is passed **only when a value is explicitly set**, and it is passed verbatim. A
null row sends no flag and cannot break a spawn. The panel says on the row that
the value goes straight through to the provider.

Every construction site of `SpawnRequest` currently passes an empty `env`, and
this epic leaves it that way. `SpawnRequest.env` does reach the child process
(`core/src/runner.rs:158` → `supervisor/src/main.rs:193`) — it is simply not the
mechanism any more.

Also stale and worth fixing while nearby: `docs/jod-system.md:330` lists
reasoning-effort cycling as blocked because *"each harness spells it
differently"*. Two of the three now spell it identically.

## C4. The panel

`Workspace::Roles`, reached by `/roles`. Same shape as `Memory` and `Schedules`.

```
┌ roles ───────────────────────────────────┐
│ ● main            claude · haiku  ·  —   │
│ └ ● assistant     claude · haiku  · low  │
│   ├ ○ scratch     opencode · gpt  ·  —   │
│   └ ○ manager     claude · sonnet · med  │
│     └ ○ engineer  claude · opus   · high │
│ ○ housekeeping    claude · haiku  ·  —   │
└──────────────────────────────────────────┘
  ↑↓ move  ⏎ edit  h harness  m model  t think  p perm  r reset
```

- Nesting is the edge. `main` → `assistant` → `{scratch, manager}` →
  `engineer`. `housekeeping` hangs off the root because nothing delegates to it.
- A row showing `—` is inheriting. A row showing a value has one set.
- `h`, `m`, `t` and `p` open the same pickers `/harness`, `/model`, `/mode`
  already use, so there is one list of model names in the codebase and not two.
- `r` clears the row back to inherit.
- Editing a role changes **what is spawned next**. Runs in flight are untouched,
  and the panel says so, because a settings screen that silently does nothing to
  what you are looking at is worse than one that explains itself.

---

## Migrations

Four, in `core/src/store.rs`, in this order. **Take the next free numbers by
reading the file — do not derive them by counting.** The last spec predicted
`0018` and got `0020` because main moved three migrations underneath it.

1. `ALTER TABLE conversations ADD COLUMN ephemeral INTEGER NOT NULL DEFAULT 0;`
2. `ALTER TABLE conversations ADD COLUMN held INTEGER NOT NULL DEFAULT 0;`
3. `ALTER TABLE conversations ADD COLUMN archived_at_ms INTEGER;`
4. `CREATE TABLE roles (...)` as in C2.

Existing rows get `0`, `0` and null, which is the honest starting state: nothing
that already exists is scratch.

Both settings keys — `scratch_retention_days` and `scratch_reuse_window_minutes`
— need no migration. `settings` is key and value (`core/src/store.rs:727`), so an
absent key means the default.

## Files this touches

| File | What changes |
|---|---|
| `core/src/store.rs` | four migrations, `roles` accessors, the archive and sweep queries |
| `core/src/orchestrator.rs` | main's preamble shrinks, `assistant_preamble`, `hand_to_assistant` |
| `core/src/mcp.rs` | `ask_assistant`, `ask_manager`/`delegate`/`open_work` refused from main, the `list_agents` second-call refusal, `AgentView.scratch` and the scratch reuse hint |
| `core/src/service.rs` | `SpawnRequest.role`, resolving a role to harness/model/env at the one spawn seam |
| `core/src/ticker.rs` | the archive and retention sweep |
| `core/src/daemon.rs` | wiring that sweep into `Tick::tick`, where every sweep is actually driven from |
| `core/src/tree.rs` | `scratch_lane()` — held and archived facts keyed by run id |
| `core/src/harness/*.rs` | the thinking mapping, per harness |
| `cli/src/tui/workspace.rs` | `Workspace::Roles` |
| `cli/src/tui/app.rs` | its list state, the roles rows |
| `cli/src/tui/ui.rs` | drawing the roles panel, the held marker |
| `cli/src/tui/mod.rs` | `/roles`, the `k` key, `z` revealing archived scratch |
| `cli/src/tui/keys.rs` | the roles key bar, `k` on the fleet bar |
| `cli/src/main.rs` | the deny-rule settings document for orchestrating runs |

## Checks

`cargo test -p jod-core -p jod-cli`. Every one of these must run and pass.

**Epic A**

1. `ask_assistant` creates a conversation with `origin = 'assistant'`,
   `ephemeral = 1` and `parent_conversation_id` set to main.
2. Two `ask_assistant` calls make two different conversations. This is the
   regression guard on "fresh every time" — a standing assistant would return
   the same id and reintroduce the block.
3. `ask_assistant` returns before the assistant run has produced any event.
4. `ask_manager` called by main's run is refused, and the refusal names
   `ask_assistant`.
5. `delegate` called by main's run is refused the same way.
6. `ask_manager` called by an assistant's run succeeds.
7. A second `list_agents` from one run inside one turn is refused; the first
   succeeds.
8. A run at `ToolAccess::Orchestrate` in `auto` mode is spawned with a settings
   document containing the deny rule. A run at `ToolAccess::Read` is not.
9. `jod approve-hook` denies `sleep 45` and `until [ ... ]; do sleep 5; done`
   for an orchestrating run, and allows `ls`.

**Epic A, reuse (A6)**

10. `list_agents` from an assistant run offers a completed scratch session whose
    last activity is inside `scratch_reuse_window_minutes`, and does not offer
    the same session once it is outside it.
11. A *running* scratch session is never offered for reuse. This is the
    regression guard on rebuilding the block one layer down.
12. A completed scratch session is absent from the engineer `idle` list and from
    the engineer reuse sentence. This is the cross-talk guard from A6 — without
    it a scratch row gets advertised as a warm checkout.
13. `scratch_reuse_window_minutes = 0` offers nothing for reuse, and the
    assistant's every instruction opens a new session.
14. The scratch reuse sentence tells the caller to continue only on the same
    subject, where the engineer one says any subject will do.

**Epic B**

15. A scratch conversation whose run completed and whose deliveries are all
    delivered is archived by the sweep.
16. The same conversation with one `queued` delivery is **not** archived.
17. A scratch conversation whose run `failed` is not archived.
18. A scratch conversation whose run is still **wedged** is not archived.
    **Corrected while building.** This check used to say "marked stalled is not
    archived", and that is false through the daemon and true only of the sweep
    in isolation. `tick_heartbeats` runs first and *retires* the mark: it sees a
    run that has ended, calls `unwatch_run`, and deletes the `heartbeats` row
    and `stalled_since_ms` with it, so by the time the scratch sweep looks there
    is no mark left. The promise therefore rests on the run's **status**, not on
    the mark surviving — a genuinely wedged run is `running` or gets reaped to
    `failed`, and neither is `completed`, so the archive query refuses it either
    way. A session that stalled and then delivered its answer **is** archived,
    which is correct: B2 keeps a row visible because it is *stuck*, present
    tense, and one that recovered is not stuck. Archiving only hides it, and `z`
    brings it back for the whole retention window.
19. `held = 1` survives every one of the above.
20. The sweep deletes an archived scratch conversation past the window and
    leaves one inside it.
21. `scratch_retention_days = 0` deletes nothing.
22. `k` on a finished, unheld scratch row archives it immediately; `k` again on a
    held row releases it.
23. An archived scratch row is hidden from the loose pane and shown under `z`.
24. `continue_agent` on an archived scratch conversation clears
    `archived_at_ms`, and the row is back in the loose pane while it runs.

**Epic C**

25. An empty `roles` table changes no spawn — assert the `SpawnRequest` built for
    a `delegate` is byte-identical to today's.
26. A `roles` row for `scratch` naming a harness and model reaches the
    `SpawnRequest` that `delegate` builds.
27. An explicit `model` argument on `delegate` beats the role's row.
28. The conversation's own `/model` beats the role's row.
29. `thinking = 'high'` on a Claude Code role puts `--effort high` in the argv,
    and on an AGY role too. On OpenCode it puts `--variant high`. A null
    `thinking` puts **no flag at all** on any of the three — that is the check
    that keeps an empty `roles` table behaving exactly like today.
29b. `SpawnRequest.env` is still empty at every construction site. The effort
    level must not have been routed through the environment.
30. The roles panel lists all six roles with `main` at the root and `engineer`
    at depth three.

**End to end, on this box**

31. Open `jod tui`, type three instructions in quick succession, and assert none
    of them queue for more than two seconds. This is the check the whole spec
    exists for, and it is the one that cannot be faked in a unit test.
32. Ask a scratch-shaped question, wait for the answer, then ask a follow-up that
    only makes sense as a continuation. Assert the second one lands on the same
    session as the first.

## Out of scope

Say so rather than doing them.

- **Concurrent turns on one conversation.** Letting two runs write to main at
  once would race `conversations.head_id` and fork the transcript. The fix here
  is a turn short enough that the queue drains, not a queue that does not exist.
- **A draggable graph canvas.** Reljod chose the tree. If the topology ever gets
  edges that nesting cannot express, that is when to revisit it — in the desktop
  app, not the terminal.
- **Making managers ephemeral.** A manager's context is the reason it exists.
- **Migrating existing conversations to `ephemeral`.** Everything that exists
  now stays non-scratch. That is true rather than convenient.
- **Per-role permission ceilings that exceed the console's.** The mode on the
  status bar stays the ceiling for everything below it. A roles row may ask for
  less and never for more.
- **Retiring `orchestrator_preamble`'s name.** It shrinks; renaming it churns
  every test that references it for no gain.
