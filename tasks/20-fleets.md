# Fleets and the fleet tree — discovery

How this was tested: the built `target/debug/jod` binary against an isolated
`JOD_HOME=/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-fleet` (never
`~/.jod`). Two kinds of evidence:

- **Real runs** — `jod run --detach [--watch]` against a throwaway git repo at
  `/home/reljod/.claude/jobs/cd76af0f/tmp/fleetwork/scratch-repo`, then
  inspected with `jod ls --json`, `jod work show --json`, `jod kill`, `jod
  work delete`, and by reading the sqlite file back with `python3 -c "import
  sqlite3…"` (no `sqlite3` binary on this box).
- **Synthetic state** — rows inserted directly into `JOD_HOME/jod.db` with
  `python3` (sanctioned in the brief for testing the tree/renderer without
  spawning real agents), seeded by
  `/home/reljod/.claude/jobs/cd76af0f/tmp/fleetwork/seed.py`. Every finding
  below says which kind it is.

I could not build anything — `cargo build`/`cargo test`/`cargo run --example
screens` were all out of bounds per the brief (this box OOMs under parallel
cargo, other agents were building). That ruled out `cli/examples/screens.rs`,
the actual TUI renderer, and the compiled test binaries. Where a finding
depends on how something is drawn in a terminal, I read the drawing code
(`cli/src/tui/ui.rs`) directly and, for tree *shape* questions, reimplemented
`Store::forest_of`'s SQL and walk in Python
(`/home/reljod/.claude/jobs/cd76af0f/tmp/fleetwork/simulate_forest.py`) against
the same database, line for line against `core/src/tree.rs`. That is a
faithful re-derivation of the query, not the compiled function — I say so
again wherever a finding rests only on it.

I read `docs/spec-ceo-and-managers.md` and `tasks/00-launch-and-roots.md`
first, as instructed. Findings the spec already covers are cited by change
number rather than refiled.

---

## F1. Once any work exists, every delegated run disappears from the fleet screen
Status: **verified fixed — check run against main, passes** · Severity was: high

The check was executed, not inferred from the merge.
`the_fleet_still_shows_a_run_that_belongs_to_no_work`
(`cli/src/tui/ui.rs`) seeds one work with a session and one loose delegated run
off a real store, renders at 150x30, and asserts the run's id and name are both
on screen. It first asserts the loose run has **no** node in the forest, so it
cannot pass because the run quietly acquired one — a guard my own check did not
ask for. Ran green against main.

A `delegate`d run (or any session started before works existed) belongs to no
work — `conversations.work_id IS NULL`. `Store::forest_of` only ever looks at
sessions with `work_id IS NOT NULL` (`core/src/tree.rs:197-204`), so such a
session has no node in the forest. That is by design and is fine *by itself* —
`App::has_tree()` (`cli/src/tui/app.rs:1075-1077`) is documented to fall back
to the older flat list precisely for this case:

> A session that belongs to no work has no node in the forest, so the flat
> list is not legacy — it is what the screen shows when there is no tree to
> show.

The bug is that the fallback only fires when **no work exists at all**.
`draw_fleet` (`cli/src/tui/ui.rs:3136-3146`) reads:

```rust
fn draw_fleet(f: &mut Frame, app: &App, area: Rect) {
    // The tree the moment there is one. Not a replacement for the flat list
    // below but the other half of the same screen: a session belonging to no
    // work has no node in the forest, and the list is what shows it.
    if app.has_tree() {
        let (left, right) = split(area);
        draw_tree(f, app, left);
        if let Some(right) = right {
            draw_tree_detail(f, app, right);
        }
        return;   // <-- the flat list below never runs
    }
    let (left, right) = split(area);
    let rows = app.fleet_rows();
    ...
```

The comment says the flat list is "the other half of the same screen" — but
the `return` means it is drawn only when the tree is *completely* empty. The
moment a single work exists anywhere on the box, `has_tree()` is true forever
after, and every loose delegated run — past, present, future — vanishes from
the fleet screen. There is a test for the empty-tree case
(`the_fleet_falls_back_to_its_list_when_there_are_no_works`,
`cli/src/tui/ui.rs:9374-9388`) but none for "a work exists *and* a loose run
exists", which is exactly the gap.

Evidence, both real and synthetic:
- Real: `jod run --detach --watch -n hello-agent -c <scratch-repo> "say potato"`
  created conversation `1f0a0b6b-…` with `work_id = NULL` (confirmed by
  reading `conversations` back). It shows in `jod ls --json`.
- Synthetic: after seeding eight works with sessions (`work-C-…` through
  `work-J-…`), that same delegated conversation still has `work_id = NULL`,
  so `has_tree()` is true and, per `forest_of`'s own `WHERE work_id IS NOT
  NULL`, the row can never appear in the tree. Reading `draw_fleet` confirms
  the flat list that would otherwise show it is now unreachable.

This directly hits the scenario the brief calls out: "a `delegate`d run, which
belongs to no work and should stay loose." It does stay loose in the data —
it just becomes invisible the moment the fleet has any structure at all,
which on a box that has ever opened one work is always.

Fix: draw both halves whenever there is anything to show for either — the
tree when `has_tree()`, and a "loose" section (`app.fleet_rows()` filtered to
agents with no matching tree node) whenever any exist, not only when the tree
is completely empty.

Check: seed one work with a session, and one `delegate`d run outside any
work. Render the fleet screen (once `cli/examples/screens.rs` or a render
test can run) and assert the delegated run's id appears somewhere on it.

---

## F2. A run's tree node cannot say finished, failed, or killed — only "running" or not
Status: **verified fixed — check run against main, passes** · Severity was: high

The check was executed, not inferred.
`a_finished_a_failed_and_a_killed_run_each_read_differently`
(`cli/src/tui/ui.rs`) seeds completed, failed and killed under one session,
asserts each row's glyph and each state line names the right word, and then puts
the three state lines in a `HashSet` and asserts they are distinct. That last
assertion is this task's "assert each renders a different state line" literally
rather than approximately. Ran green against main.

`Node` (`core/src/tree.rs:59-80`) carries a single `running: bool` for a run,
set by `RawRun.running = status == "running"` (`core/src/tree.rs:132-133,
244`). A `Run` node's own status — `completed`, `failed`, `killed` — is
discarded; only "was it `running`" survives into the tree at all.

The renderer inherits that flatness. In `draw_tree`
(`cli/src/tui/ui.rs:2955-3003`), the only thing distinguishing a run node is:
a spinner if `node.running`, a card badge if it has cards, and its `summary`
line (the last thing it said). A completed run, a failed run and a killed run
all draw identically — no colour, no glyph, nothing — and are told apart only
if the agent's own last message happened to say so in words ("all done" vs.
"blew up" vs. "stopped"). The detail pane is the same:
`draw_tree_detail` (`cli/src/tui/ui.rs:3084-3087`) prints `state: running` or
`state: idle` — two values for four statuses.

Confirmed with synthetic data: work `work-E-statuses` seeds one session with
three runs, `status = completed | failed | killed`. Reimplementing
`forest_of`'s query against that seed shows all three as sibling leaf nodes
with `running=False` and no other distinguishing field — see
`run-E-fin`, `run-E-fail`, `run-E-kill` in the simulated output, all
identical apart from their (agent-chosen) name and summary.

This sits right next to the problem the SPEC opens with — "you cannot tell a
finished session from a stuck one" — but it is a distinct gap: even without
any stall involved, the tree already cannot tell a *normal* finish from a
*failure* from a *kill*. SPEC Change 1c only proposes a `stalled` badge; it
does not touch this.

Fix: carry the run's actual status onto `Node` (or at minimum thread it
through to the label/detail pane), and give `draw_tree`/`draw_tree_detail` a
visual for failed and killed distinct from a clean finish.

Check: three runs under one session with `status` completed/failed/killed;
render the tree (or its detail pane) and assert each renders a different
`state` line.

---

## F3. `cut()` truncates by Unicode scalar count, not terminal width — unicode rows can overflow their box
Status: **verified fixed — merged as #149, check run against main, passes** · Severity was: medium

Its status read `open · needs confirming` until now — the fix had landed and
nobody flipped it, which is the state this whole verification pass exists to
catch.

`cut()` (`cli/src/tui/ui.rs`) now measures in columns via `Span::raw(s).width()`
and walks the string, dropping a character that would straddle the last column
rather than slicing by scalar count. Two tests cover the check:

```
test tui::ui::tests::cutting_counts_columns_not_characters ... ok
test tui::ui::tests::a_japanese_row_stops_where_an_english_one_stops ... ok
```

The second is the check as this task states it — a wide-char-heavy label
rendered at a fixed width, asserted to stop where an English one does.

`fn cut` (`cli/src/tui/ui.rs:4766-4774`) truncates with `s.chars().count()`.
Every "how much room is left" computation in `draw_tree` builds on the same
assumption — `room -= glyph.chars().count()`, `room -= badge.chars().count()`,
then `cut(&node.summary, room.saturating_sub(2))`
(`cli/src/tui/ui.rs:2974-3000`). That assumption is "1 char = 1 terminal
column", which is false for CJK text and most emoji: they are one `char` in
Rust's sense but render as **two** terminal columns (East Asian Wide /
emoji-wide). A row whose label or summary leans on either will be
under-truncated relative to the box's actual width — confirmed in Python:

```
s = '🚀 超長いタイトルです'
len(s) == 11   # chars()
# but 10 of the 11 are unicodedata.east_asian_width == 'W' (2 columns each)
# so the string is really ~21 terminal columns wide, not 11
```

I seeded exactly this (`work-H-unicode`, title `"🚀 超長いタイトルです…"` ×20,
plus a session title and run name mixing emoji and Japanese) to make sure the
case is real and not hypothetical — `simulate_forest.py` shows the row
loading and flattening fine, so the data layer is unaffected; this is purely
a rendering-budget bug. I could not render the actual box to see the overflow
(no terminal, no build), so this is read from the arithmetic rather than
observed on screen — flagging it `needs confirming` for that reason, but the
mismatch between `chars().count()` and display width is unambiguous.

Fix: use a display-width function (e.g. the `unicode-width` crate) everywhere
`cut()` and its callers currently use `.chars().count()`.

Check: a node whose label is wide-char-heavy (CJK/emoji), rendered at a fixed
terminal width; assert the row's rendered width does not exceed the box.

---

## F4. `list_agents` silently truncates at 20 with no signal there is more
Status: **verified fixed — merged as #143, check run against main, passes** · Severity was: high

```
test mcp::tests::a_listing_says_how_many_agents_the_limit_left_out ... ok
```

> **This finding was wrong in both directions, and the fix says so.** Read the
> correction below before the original text, which is kept for the record.

`list_agents`'s own description (`core/src/mcp.rs:164-168`) says:

> Every agent Jod knows about, running or finished, each with its last
> message.

The implementation (`core/src/mcp.rs:967-1000`) takes `limit` (default 20,
`opt_usize(args, "limit")?.unwrap_or(20)`), sorts running-first-then-newest,
and does `.take(limit)` — with no total count, no "N more" field, nothing in
the JSON that says the page was cut. Contrast the CLI's own `jod ls`, which
explicitly prints `"{hidden} older hidden — jod ls --all"`
(`cli/src/render.rs:391-403`, wired from `cli/src/main.rs:1754-1770`) — the
same underlying cap, but the CLI says so and the MCP tool does not.

This matters specifically for the problem the SPEC is about. The whole
router-picks-a-stuck-agent failure mode depends on the router being able to
see every candidate agent for a project before deciding to spawn a new one.
Past 20 agents on the box — not a large number after a few busy days — an
older, wedged agent silently falls off the page with no hint to the caller
that it should ask again with a higher `limit`. **That sentence is wrong; see
the correction.** SPEC Change 2 widens
`AgentView` with project/work/stalled/busy fields and adds a `project` filter,
which would shrink typical result sets, but it does not add a total/truncated
signal, and neither exists today.

Fix: mirror what `jod ls` already does — return (or otherwise signal) how
many agents exist versus how many were returned.

### The correction, measured by the agent that fixed it

The trigger was wrong in both directions, and neither of the two people who
read this task caught the second half.

**Smaller than filed.** "Past 20 agents on the box" does not lose a running
agent. The sort puts every running agent ahead of every finished one, so the
cut always lands in the finished tier first. Seeded with 100 agents whose three
*oldest* were the running ones, all three still led the page. A running agent
is only lost to the 20-cap when **more than twenty are running at once**,
confirmed at 21.

**Bigger than filed, and this is the case that actually loses a wedged agent.**
Before paging, `list_agents` reads runs out of SQLite with a fixed cap of 200
(`REHYDRATE`), newest first. An agent older than the newest 200 runs never
enters memory at all. With 205 agents seeded, the three oldest running:

```
default            -> 20 rows, no live-* at all
limit: 1000        -> 200 rows, contains live-000: FALSE
running_only: true -> []      # while three agents were running
```

`running_only: true` returning an empty array while three agents are running is
the real bug. **No value of `limit` reached it** — so the remedy this task
assumed existed did not exist, and signalling the cut while leaving the escape
hatch broken would have been the worse half of a fix. The fix is
`rehydrate(REHYDRATE.max(limit))`, the line `jod ls` already had.

So the router can miss a wedged agent, just not for the reason written down
here. The lesson is the same one this file keeps learning: a trigger reasoned
from the code reads as precise and can be wrong in both directions at once.

Check: seed more than `limit` agents, call `list_agents` with the default
limit, assert the response says how many were omitted.

---

## F5. "Stop a running agent and everything it started" does not stop what it started
Status: **in flight** · Severity: high — misleading given the tool's own wording

`stop_agent`/`jod kill`'s docs (`core/src/mcp.rs:223`, `cli/src/main.rs` kill
help) both say **"Stop an agent and everything it started."**
`Jod::kill_agent` (`core/src/service.rs:1163-1213`) implements this by
signalling the run's OS process group:

> The signal goes to the whole process group, so a harness that spawned
> children does not leave them behind — the same reach `tmux kill-session`
> had.

That is true for OS-level subprocesses the harness forks directly (a `Bash`
tool call, say). It is not true for a session that "started" another Jod
agent via `delegate` or `open_work` — a nested session in the SPEC's and this
task's sense (`tasks/00-launch-and-roots.md`'s scenario 6: "a session that
starts a session"). Each spawned run gets its own fresh session/process group
via `setsid` — `core/src/runner.rs:175`:

> `setsid` made the supervisor a session and group leader

— so a child agent's pgid is never a member of its parent's group. Killing
the parent's pgid cannot reach it. Confirmed live: I spawned a real run
(`jod run --detach -n killme … "count to 1000000"`), then `jod kill
<run-id>`; the run correctly transitions to `killed` with `process_alive:
false` (see the "killed run" row in the scenario table — this half works
correctly). I did not additionally spin up a second, delegated child agent
under it to watch it survive the kill — that would cost a second real agent
run for a result the code already makes clear — but the mechanism
(`terminate_group(pgid)` against a pgid that a truly nested agent, by
construction via `setsid`, is never a member of) is unambiguous by reading
alone, so I'm listing this as a confirmed-by-code finding rather than
`needs confirming`.

Fix: either narrow the tool's description to what it actually does ("stops
this agent's own process tree; a session it delegated to keeps running and
must be stopped separately"), or make `kill_agent` walk `parent_conversation_id`
and stop descendants too, if that is the intended behaviour.

Check: a parent run that calls `delegate` for a child run; kill the parent;
assert the child's `runs.status` is unaffected (documenting today's actual
behaviour) — or, if the fix is to cascade, assert the child is also stopped.

---

## F6. Deleting a work's last conversation leaves its runs as permanent, contextless ghosts
Status: **verified fixed — merged as #137, check run against main, passes** · Severity was: medium

```
deleted the parser — 1 session(s), 1 transcript(s), 0 unanswered card(s)
1 run(s) kept, with the transcripts that explained them now gone —
`jod history` still lists them by id
```

The check asked for either the run row gone or the summary mentioning it. The
summary mentions it and names where to find it.

**This check failed the first time I ran it, and the fault was mine.** My seed
inserted a work, a conversation and a run — exactly what the check's wording
says — and the delete reported no runs kept while the run row survived. That
looks precisely like the bug. It was not.

`runs_losing_their_last_transcript` (`core/src/works.rs:1569`) counts runs
through the **`messages`** table: a run is orphaned when the last transcript
explaining it disappears. My seeded run had no messages, so there was nothing
for it to lose, and zero was the right answer.

The check's wording is what allowed it: "seed a work with one session and one
completed run" never says the run must have a transcript, and the fix's whole
subject is transcripts. **Corrected check:** seed the run *with at least one
message*, then delete. Without that clause this check produces a false failure
against working code.

**A second reason not to cascade, found while fixing it.** `events.run_id` has
no foreign key to `runs`, so deleting the run row would strand its events and
its recorded cost with nothing left listing them at all — trading visible
orphans for invisible ones. That is why the fix reports what a delete leaves
behind rather than deleting more.

`jod work delete` cascades: `messages.conversation_id` is `REFERENCES
conversations(id) ON DELETE CASCADE` (`core/src/store.rs:430`), so deleting a
work's conversations deletes their messages too. But `runs` rows are not
scoped to a conversation by a foreign key at all — the *only* link
(`forest_of`'s join, `core/src/tree.rs:230-236`) is through the `messages`
table. Delete the messages and the run is unreachable from the tree forever,
while the row itself is never removed.

Confirmed live: seeded work `work-J-orphan` with one session and one
`completed` run (`run-J-1`), then ran the real CLI:

```
$ jod work delete work-J-orphan
deleted J: orphan run — 1 session(s), 1 transcript(s), 0 unanswered card(s)
```

Afterwards: `works` and `conversations` rows for it are gone, `messages` for
that conversation are gone, but `SELECT id, status FROM runs WHERE
id='run-J-1'` still returns `('run-J-1', 'completed')`. That run has no
messages left anywhere, so no session/work will ever show it in the tree
again, no `conversation_for_run` lookup will resolve it, and nothing in the
deletion's own summary line mentions it. It is only reachable via `jod ls
--all`/`jod history`, as a bare id with a name and a cost, no context, no
transcript, forever.

This is the concrete case behind the brief's "a run whose conversation was
deleted" edge case, produced through a real deletion rather than by directly
deleting a conversations row by hand.

Fix: either cascade-delete a work's runs too when their only conversation
goes with it (losing the row entirely, matching the "transcripts... removed"
framing of the delete message), or say so in the delete summary ("N run(s)
now orphaned"), so the loss is visible rather than silent.

Check: seed a work with one session and one completed run, `jod work delete`
it, assert either the run row is gone or the delete summary mentions it.

---

## F7. A stalled run has no representation in the fleet tree at all — confirms SPEC Change 1c, not new
Status: covered by SPEC Change 1c · Owner: — · Severity: n/a (citing, not filing)

Seeded a synthetic heartbeat for `run-F-stalled` — `status='running'`,
`last_progress_ms` 30 minutes old against a 20-minute `stall_ms`, `pgid` set
to this Python process's own live pid so a real liveness probe would read
`alive=true`. Read `core/src/tree.rs` end to end: `forest_of` never queries
`heartbeats` at all. Read `cli/src/tui/{fleet,ui,data}.rs`: no reference to
`heartbeats`, `stalled`, or a heartbeat-derived state anywhere in the fleet
rendering path — the only `Node` liveness signal is `runs.status == "running"`,
which a stalled run still satisfies. `AgentStatus`
(`core/src/service.rs:47-52`) also has no `Stalled` variant, matching the
SPEC's own explicit decision that it should not. This is exactly the gap
Change 1c describes ("the fleet tree shows a `stalled` badge on a run node")
and it is unbuilt — the tree today would show my seeded stalled run exactly
as if it were healthy and working, spinner and all. Filed here only to
confirm it reproduces; the fix is already scoped in the SPEC.

---

## F8. No `jod daemon` running: confirms SPEC gaps 1a/1d, not new
Status: covered by SPEC Changes 1a, 1d · Owner: — · Severity: n/a (citing, not filing)

Real evidence: I never ran `jod daemon` in this JOD_HOME. `jod run --watch`
armed a heartbeat and printed the SPEC-anticipated warning verbatim:

```
♥ watching 1f0a0b6b-… — stopped if silent for 20 minutes (needs `jod daemon`)
```

That run finished normally (`status: completed`) a few seconds later, but its
`heartbeats` row was still present afterward — nothing ever swept it, because
nothing but `jod daemon`'s tick calls `tick_heartbeats`
(`core/src/ticker.rs:761`). This matches the module's own documented design
(`core/src/heartbeat.rs:44-65`: cleanup on a normal finish is the sweep's job,
not automatic) rather than being a bug — flagging only because it is exactly
the daemon-not-running case the brief asked to test, and it behaves as SPEC
1a/1d describe: nothing is watched, nothing is marked, and (SPEC 1d, unbuilt)
the fleet does not yet say once that the daemon is missing.

---

## F9. `screens.rs` shows an empty fleet whatever the database holds
Status: **fix open as #132** · Severity: medium

`cli/examples/screens.rs` is the one tool that lets someone render a TUI screen
without a terminal, and it renders every workspace off a real database — except
the fleet. It populates `app.memory`, `app.graph_size`, `app.schedules`,
`app.goals`, `app.hooks`, `app.activity`, `app.board`, `app.team`,
`app.members` and `app.tasks` (`cli/examples/screens.rs:115-127`), and **never
loads `app.agents` or `app.forest`**. Verified by grep: neither field is
assigned anywhere in the file.

So its fleet screen is empty no matter what it is pointed at.

**The harm is that it answers "nothing here" when it means "I did not look".**
That is worse than erroring, because the answer is plausible and a reader takes
it. The fleet discovery agent for this file reached for exactly this example,
could not use it, and re-derived `forest_of` in Python instead — which is why
several findings here rest on a re-derivation rather than compiled code. And
all the while a genuine high-severity bug, F1, lived in precisely the code the
example claims to render.

This finding is the joint product of that difficulty and F1's fix (#121), which
left behind the pattern the example should follow: seed a real `Store`, read
back through the compiled `Store::forest()`, render through the real `draw()`,
assert against the buffer.

Fix: load the agents and the forest the way every other workspace is loaded.
Scope it to the example — `cli/src/tui/ui.rs` and `core/src/tree.rs` are being
worked on separately.

Check: point the example at a database holding one work, one session and one
loose delegated run, and assert the fleet screen is not empty.

## Scenarios run

| # | Scenario | Method | Expected | Actual | Result |
|---|---|---|---|---|---|
| 1 | Empty fleet | real, fresh `JOD_HOME` | `jod ls`/`jod work ls` empty | both `[]` | pass |
| 2 | One running agent | real (`jod run --detach --watch`) | shows `running`, heartbeat armed | showed running, then `completed`; heartbeat row created and correctly left un-swept (no daemon) | pass |
| 3 | Many agents at once — anything cap/truncate? | code reading + real | `forest_of`/tree: no cap (by design, windowed at render); `jod ls`: capped at 20 with a spoken hint; `list_agents` (MCP): capped at 20 with **no** hint | tree confirmed uncapped; `jod ls`'s hint confirmed working (see caveat below); `list_agents` confirmed silent | **F4** (list_agents), tree/`jod ls` pass |
| 4 | Work with several sessions — tree shape | synthetic (`work-C`) | lead session, its run, two worker siblings, one worker's own run, all nested correctly | simulated `forest_of` walk produced exactly that shape and order | pass (via SQL re-simulation, not the real renderer — see note in `simulate_forest.py`) |
| 5 | A `delegate`d run, belongs to no work | real + synthetic | stays loose, and is still visible somewhere on the fleet screen | stays loose in the data; becomes **invisible** once any work exists | **F1**, high |
| 6 | Nested tree deeper than two levels | synthetic (`work-D`, 3 levels) | each session nests under its starter, depth increases correctly | simulated walk: depth 1→2→3→(run at 4), correct | pass (simulated) |
| 7 | Finished, failed, killed runs | synthetic (`work-E`) + real kill | tree distinguishes the three | tree cannot distinguish them at all — only `running: bool` | **F2**, high |
| 7b | A killed run (real) | real (`jod kill` on a live run) | `runs.status` → `killed`, `process_alive: false` | exactly that | pass |
| 8 | A stalled run (live process, quiet) | synthetic heartbeat | fleet shows it distinctly from running | no distinction exists anywhere in the fleet path | **F7** — cites SPEC 1c, not new |
| 9 | No `jod daemon` running | real | heartbeat sweep never runs, nothing is marked | confirmed; matches documented design | **F8** — cites SPEC 1a/1d, not new |
| 10 | Killing an agent — do children die? | real (single-agent kill) + code reading | a delegated child should plausibly also stop, per the tool's own wording | single-agent kill works correctly; a delegated child would **not** be reached — `setsid` gives it a separate process group | **F5**, high; single-kill case itself: pass |
| 11 | Edge: run whose conversation was deleted | real (`jod work delete`) | either cleaned up or clearly surfaced | `runs` row survives forever, unreachable from the tree, unmentioned in the delete summary | **F6**, medium |
| 12 | Edge: a work with an empty board | synthetic (`work-G-empty`) | shows as a work node with no children, no crash | exactly that — leaf work node, no expand marker | pass |
| 13 | Edge: very long / unicode titles and summaries | synthetic (`work-H`, emoji + CJK, 200+ chars) | renders without corrupting layout | data layer fine; truncation arithmetic (`cut()`) assumes 1 char = 1 column, which is false for CJK/emoji | **F3**, medium, needs confirming (no terminal) |
| 14 | Edge: a run holding a worktree lease, cwd = worktree | synthetic (`work-I`, lease row + run/session `cwd` set to a worktree path) | some way to tell cwd differs from the checkout | the fleet tree/detail pane shows no `cwd` anywhere for any node — this is the visual side of SPEC gap 3 (`AgentView.cwd`); not refiling, just noting the UI has the same blind spot | needs confirming / overlaps SPEC gap 3 |

### Caveat on scenario 3's `jod ls` cap

While seeding, ten synthetic runs had a placeholder `summary = "{}"`, which
does not deserialize as `AgentSummary`
(`core/src/service.rs::rehydrate`, silent `continue` on failure — a real,
if narrow, edge case in its own right: a row that fails to deserialize is
dropped from history with only an `eprintln!` to stderr, and is also excluded
from the "N older hidden" count's denominator source in one of the two ways
that number is computed, so a corrupt row and a merely-capped row look
identical to the count). That happened to make `jod ls --all`'s hint —
`"10 older hidden — jod ls --all"` — appear even though `--all` had already
been passed, which is confusing but is an artifact of my synthetic seed
having invalid `summary` JSON (something the real binary would never write),
not a bug I can attribute to normal operation. Noting it rather than filing
it: worth a look only if `AgentSummary`'s shape ever changes in a way that
makes old rows undeserializable in practice.
