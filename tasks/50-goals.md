# Goals — standing objectives that run until they are met

How this was tested: the built `target/debug/jod` binary on `PATH`, against an
isolated `JOD_HOME=/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-goal`
(never `~/.jod`). Real goals were added and advanced one tick at a time with
`jod daemon --once`, which lets a goal be pushed forward with `jod goal run
<name>` first rather than waiting on the cron or a live minute. A handful of
scenarios (the wedge, and one budget-exhaustion boundary) used direct SQL
against this same `jod.db` to plant synthetic runs/heartbeats/facts rather than
paying for repeated real iterations to reach the same state — each of those
says so explicitly. Every real iteration spawned a genuine Claude Code agent
and cost real, if small, money; total spend across this pass was under $1.

Two goals (`satisfied-first`, `wedge-test`, `pause-test`) were carried across
several ticks to watch state machinery rather than single calls, which is why
some findings below cite the same goal more than once.

---

## G1. A goal's memory is keyed on its name, so a new goal inherits a dead one's record
Status: **fix open as #126, waiting on a human** · Severity: high

Every fact a goal writes is filed under the subject `goal/<name>`
(`core/src/ticker.rs:908`), and every read is `facts_about("goal/<name>")` —
`goal log` at `cli/src/main.rs:4218`, `last_fingerprint` at
`core/src/ticker.rs:1709`, `current_run` at `core/src/ticker.rs:1718`.
`facts_about` (`core/src/store.rs:2311`) matches on subject alone and ignores
scope entirely.

The scope is the part that would have made this safe. `memory_scope()` is
`goal:<id>` (`core/src/schedule.rs:428`), so every goal already writes into its
own partition — the read path just never looks at it. And `delete_goal`
(`core/src/store.rs:2812`) is a single `DELETE FROM goals`; it leaves every fact
behind. So removing a goal and adding another with the same name gives the new
goal the old one's memory: its `ended` verdict, its `done-when` fingerprint, and
its `current-run` pointer.

The comment above the `goal log` read states the name-keying as a feature —
"keyed on the subject, which is derived from the name, precisely so this does
not need the id the scope is keyed on". That comment is where the bug comes
from, and it should go with the fix.

This is worse than a cosmetic mixup. Because `current_run` is also read this
way, a fresh goal's very first tick can pick up a *stale `current-run` pointer*
from the deleted goal of the same name. Watched live: after deleting
`satisfied-first` (done-when `true`, ended satisfied, one real iteration run
`c2214254…`) and adding a new goal of the same name (done-when `false`), the
new goal's first tick found the old goal's completed run, ran the *new* goal's
`done-when` against it, wrote `iteration: 1: Hello.` — the old run's message,
attributed to an iteration the new goal never ran — and only then spawned the
new goal's actual first iteration, which became iteration 2. If the stale
pointer had named a run that was still genuinely running (not yet finished),
the new goal would instead have sat `held` forever, believing its own first
iteration was perpetually in flight, and never spawned anything.

Fix: read scoped. Add a scoped reader beside `facts_about` — subject *and*
scope — and give it the goal's `memory_scope()` at all three call sites. That is
smaller and more honest than deleting facts on `rm`, because it also fixes the
case where the old goal still exists. Deleting the goal's scope in `delete_goal`
is worth doing as well, so a removed goal does not leave rows behind for ever,
but it is the second fix, not the first.

Check: add a goal, write an `ended` fact for it, remove it, add another goal
with the same name, and assert `goal log` on the new one shows nothing from the
first, and that its first tick spawns a real iteration rather than reading the
old one's `current-run`.

---

## G2. The iteration that satisfies a goal is never recorded
Status: **fixed — merged as #147** · Severity was: medium

In `tick_goals` the satisfied branch (`core/src/ticker.rs:936`) writes the
`ended` fact, sets the state, and `continue`s. The `iteration` fact and the
`advance_goal` call — which is what bumps the `iteration` counter and adds to
`spent_usd` — are both further down, after that branch has already left. So the
last iteration of a goal, the one whose work made the check pass, is invisible
in three places at once: `jod goal log` shows no iteration line for it, `jod
goal ls` still reads `iter 0` (or whatever it was before), and its cost never
lands in `spent_usd` even though a real agent turn happened and may have cost
real money.

Watched live: a goal with `done_when: true` ran one real iteration (agent said
"Hello."), was found satisfied on the next tick, and `jod goal ls` afterwards
still showed `iter 0 · $0.00` — a bill nobody would ever ask the goal about,
because the goal's own record says nothing happened.

That is also what made G1's symptoms readable as almost-plausible: the goal
that held the name before it left an `ended satisfied` fact and no iteration
line, so the two unrelated records merged into something that looked like one
goal's coherent history.

Fix: record the iteration — and call `advance_goal` with the real cost — before
the satisfied branch returns, not after.

Check: run a goal whose check passes on the first iteration and assert `goal
log` shows one iteration line and the `ended satisfied` line together, and that
`spent_usd`/`iteration` reflect the run that satisfied it.

---

## G3. A quiet done-check can never show progress, so the goal always stalls
Status: open · Owner: — · Severity: medium

Progress is a change in the check's *output*, fingerprinted by
`seen.digest()` (`core/src/ticker.rs:1704`), and the comment there explains why:
a check whose output is identical run after run is a goal going nowhere,
however busy the agent looks. That reasoning holds for a check that prints
something. It fails for a check that prints nothing.

Most done-checks are quiet by nature — `test -f DONE`, `grep -q`, a test runner
under `-q`, and `false` itself. All of them produce identical empty output on
every run whether the agent did excellent work or sat still, so `progressed` is
false every time, `no_progress` climbs on every iteration, and the goal is
stopped as stalled after `stall_after` iterations no matter what actually
happened. The one signal a person would expect to count as progress — the
check going from failing to passing — reaches the goal through the *separate*
`satisfied` path (`seen.status == 0`) and stops the goal outright; the
fingerprint only ever sees the in-between iterations, where the check keeps
failing quietly while real work may well be happening.

Fix: when the check's output is empty on both runs, fall back to something the
agent controls rather than silently scoring it as no progress — or fingerprint
the exit status alongside the output, so at minimum a failing check that has
never changed its exit code some other way is distinguished from one that
briefly succeeded and reverted. Whatever the second half turns out to be, it
needs a line in `docs/decisions.md`, because "no measurable change means stop"
is a deliberate rule and this is a real exception to it.

Check: a goal with `done_when: test -f DONE` whose agent writes real,
observable progress every iteration (e.g. appending to a log file) but does not
create `DONE` until the third iteration should not stall before it gets there.

---

## G4. A goal that is already exhausted at creation loops forever claiming and releasing itself, still reporting "running"
Status: **verified fixed — merged as #153, check run against main, passes** · Severity was: high

Both clauses of the check were executed:

```
jod goal add x "do nothing" --max-iterations 0
jod goal run x
jod daemon --once   -> claimed 1 · started 0   state: exhausted
jod daemon --once   -> claimed 0 · started 0   state: exhausted
```

State is `exhausted` rather than `running`, the second tick does not claim it
again, and no run was ever spawned.

**One residual the check does not cover, and it is not a failure.** The goal
still reports `running` between creation and the first tick — `jod goal add`
answers "x is running" and the row reads `running` until a tick flips it. In
practice a tick follows quickly and nothing is spawned meanwhile, so the harm
is cosmetic. Noted rather than filed: the check as written asks for the state
after the tick, and after the tick it is right.

`Goal::should_stop` (`core/src/schedule.rs:436`) is checked in two different
places in `tick_goals`, and only one of them acts on what it finds.

- After an iteration settles, `advance_goal` (`core/src/store.rs:2716`)
  re-reads the goal and, if `should_stop()` is `Some`, calls
  `set_goal_state` — this path works correctly (see the pass in the scenario
  table below).
- Before a fresh iteration is spawned, `tick_goals` checks it again itself
  (`core/src/ticker.rs:1014`):

  ```rust
  if !goal.state.is_live() || goal.should_stop().is_some() {
      store.release_goal(&goal.id)?;
      continue;
  }
  ```

  This branch releases the goal and moves on — it never calls
  `set_goal_state`, never writes an `ended` fact. The goal's `state` column
  stays `running`.

This only matters when `should_stop()` is already true the very first time a
goal is ever ticked, before it has completed a single iteration — which
happens for any goal created with `max_iterations <= 0` (0 is a legal CLI
value, not rejected) or `budget_usd <= 0` (also legal, and a negative budget
was accepted too — see the edge cases below). Nothing about creating such a
goal is refused, and nothing about ticking it ever marks it `exhausted`.
Instead, every due tick claims it, finds it already past its own stop
condition, and releases it — for ever. `jod goal ls` reports it `running` and
`iter 0` indefinitely; nothing distinguishes it from a goal genuinely making
progress except that it never does anything.

Reproduced, `JOD_HOME` above, three separate ways:

```
$ jod goal add zero-iter "…" --max-iterations 0
$ jod goal run zero-iter && jod daemon --once   # claimed 1 · started 0
$ jod daemon --once                              # claimed 1 · started 0, again
$ jod daemon --once                              # claimed 1 · started 0, again — forever
$ jod goal ls
◎ zero-iter    iter 0   $0.00
  next Aug 15 17:56 (7s ago)
```

The same happened for `--budget 0` and for `--budget=-5` (accepted with no
validation; displayed as `$0.00 of $-5.00`).

This is the exact failure the goal design exists to prevent — "a loop that
keeps running while nothing changes looks exactly like a loop doing useful
work" — except here it is not doing anything at all, and the state column
actively says `running` rather than being silent about it. It costs no money
(nothing is ever spawned), which is the only reason this is "high" and not the
literal worst case; the worst case is one keystroke away — see G5's note on
`no_progress` reset.

Fix: the pre-spawn check needs the same treatment as the post-iteration one —
call `set_goal_state` (and write the `ended` fact) the first time
`should_stop()` comes back `Some`, whether or not an iteration has ever run.
Simplest: have `tick_goals` call `advance_goal`-style bookkeeping (or a shared
helper) from both call sites, so there is exactly one place a goal transitions
out of `running`.

Check: `jod goal add x "…" --max-iterations 0`, then `jod goal run x && jod
daemon --once`; assert the goal's state is `exhausted`, not `running`, and that
a second `daemon --once` does not claim it again.

---

## G5. A goal that never becomes satisfied, with a done-check that always changes, has no cap at all
Status: needs confirming (reasoned from code and the mechanics behind G4;
not run to completion — this is deliberately the one finding this pass did not
pay to reproduce for real, since reproducing it means letting a goal run
without a cap by construction)

`should_stop()` (`core/src/schedule.rs:436`) only ever returns `Some` from
three conditions: iteration count, budget, or `no_progress >= stall_after`.
Neither `max_iterations` nor `budget_usd` is required by `jod goal add` — both
default to unset, and CLI validation does not require one or the other (see
G4). That leaves `no_progress` as the *only* possible ceiling for a goal with
neither set.

`no_progress` only climbs when `progressed` is false, and `progressed` is
computed from the done-check's fingerprint (`core/src/ticker.rs:961`):
identical fingerprint twice in a row means no progress; a *changed*
fingerprint — for any reason, including one that has nothing to do with real
progress — resets `no_progress` to zero (`core/src/store.rs:2737`,
`no_progress = CASE WHEN ?3 THEN 0 ELSE no_progress + 1 END`).

A `done_when` command whose output is not deterministic — one that includes a
timestamp, a random value, a line count that changes for reasons unrelated to
the objective, or simply flaky output — will fingerprint differently on every
run. Combined with no `max_iterations` and no `budget_usd`, such a goal has
*no* stopping condition that can ever fire: it is not satisfied (exit code
stays nonzero), it does not stall (`progressed` is always true), and nothing
else bounds it. Per the design's own words, this is "the worst bug in this
area" — a goal that spawns a real, billed agent run every cron tick, for ever,
because its done-check happens to be noisy rather than because anything useful
is happening.

This is not a synthetic edge case reached only by malice: `--done-when` is
free text run through a shell, and any check that shells out to something with
a clock, a PID, an ordering-unstable listing, or network jitter in it produces
exactly this. Nothing in `jod goal add --help` warns that a `done_when` command
must be deterministic in its output, and nothing requires at least one of
`--max-iterations` / `--budget` to be set.

Fix: two independent guards, either of which closes this —
1. Require `jod goal add` to set at least one of `--max-iterations` or
   `--budget` unless `--done-when` is also set and its exit code alone is
   trusted (which still does not bound *runs*, only *satisfaction* — this
   still wants a hard iteration ceiling as a backstop regardless).
2. A hard-coded absolute ceiling on iterations (independent of
   `stall_after`/`max_iterations`) that no goal can exceed regardless of
   configuration, so a misconfigured or adversarial `done_when` cannot produce
   an unbounded loop.

Check: `jod goal add x "…" --done-when "echo $RANDOM; exit 1"` with no budget
and no max-iterations; simulate enough ticks (via `jod daemon --once` against
directly-advanced timestamps, to avoid paying for dozens of real iterations)
to show the goal is still `running` after, say, 50 simulated iterations, and
assert this is refused at `add` time instead once the fix lands.

---

## G6. A goal's first iteration is never checked against its own done-when before it is spawned
Status: **fixed — merged as #166** · Severity was: medium

`check_done` (`core/src/ticker.rs:1688`) is only ever called inside the block
that settles a *previous* run (`core/src/ticker.rs:912`, `if let Some(run) =
self.current_run(...)`). A goal's very first tick has no `current-run` fact
yet, so that whole block is skipped and `tick_goals` goes straight to
`spawn_iteration` (`core/src/ticker.rs:1019`) without ever asking whether the
objective is already met.

Practically: a goal added with a `done_when` that is *already true the moment
it is created* — the objective was already satisfied, or the check was written
loosely enough to pass trivially — still always spawns and pays for at least
one real iteration before the next tick notices and stops it. There is no way
to add a goal and have it recognise on tick one that there is nothing to do.

Watched live: a goal added with `--done-when true` (unconditionally satisfied)
spawned a real agent on its first due tick; only the *second* tick, settling
that first run, found it satisfied and stopped. One iteration, and its cost,
was unavoidable regardless of what the objective actually required.

Fix: call `check_done` once, before `spawn_iteration`, on a goal with no
`current-run` yet. If already satisfied, take the same satisfied branch
without ever spawning — with the fix for G2 applied, so the "free" check is
still recorded honestly as having cost nothing rather than looking like a
skipped iteration.

Check: a goal added with `--done-when true` should show `ended satisfied` and
zero spawned runs after its first tick, not one.

---

## G7. A goal's iteration is a conversation with no roots
Status: open · Owner: — · Severity: medium (goal-specific instance of the
already-filed root bug in `tasks/00-launch-and-roots.md`; noted here because
it is a distinct, goal-specific code path and the assignment asked for it
explicitly — not a re-file of L1/L4)

`spawn_iteration` (`core/src/ticker.rs:1727`) calls `self.jod.spawn_agent`,
which defaults to `RunConversation::New` (`core/src/service.rs:877`).
`open_conversation` (`core/src/service.rs:284`) creates that conversation from
`req.cwd` alone and never calls `add_root` or `ensure_inherited_root`. Every
single goal iteration, for ever, gets a fresh conversation with zero roots.

Confirmed on the real `satisfied-first` run above:

```
$ python3 -c "... select id,title,cwd from conversations ..."
{'id': 'f26d2bf6…', 'title': 'Standing objective: say hello…', 'cwd': '/home/…/fix+orchestrator-routing-and-roots'}
$ python3 -c "... select * from conversation_roots ..."
(nothing)
```

`cwd` is a real, valid, absolute directory the whole time — this is not the
relative-path case `settle_cwd` guards against. The conversation simply never
gets a root at all. Per the known root bug, any tool the goal's agent calls
that requires a root (`open_work`, and by extension anything that routes
through it) will refuse inside every single goal iteration, silently limiting
what an unattended goal can ever do to what needs no root — which for a
coding objective is very little.

Fix: same fix as L1/L3 — call `ensure_inherited_root` (or `add_root` directly,
since the cwd is already known and absolute here) right after
`open_conversation` returns for `RunConversation::New`, or fold it into
`open_conversation` itself so every fresh conversation gets one rather than
requiring each caller to remember.

Check: run one real goal iteration and assert `jod root ls` for that
iteration's conversation reports its `cwd`, not nothing.

---

## G8. Deleting a goal does not stop its in-flight iteration
Status: open · Owner: — · Severity: medium

`delete_goal` (`core/src/store.rs:2812`) is a plain `DELETE FROM goals`. It
does not look at, let alone stop, whatever run the goal's `current-run` fact
points to.

Reproduced: added a goal, forced an iteration to start (`jod goal run` +
`jod daemon --once`, confirmed `running` in `jod ls`), then `jod goal rm`'d it
immediately while the agent was still working. The run kept going to
completion regardless:

```
$ jod goal rm delete-midflight
delete-midflight forgotten
$ jod ls | head -1
3d5d3053   running   Claude Code  delete-midflight
$ sleep 15 && jod ls | grep 3d5d3053
3d5d3053   done      Claude Code  delete-midflight
```

The run finished, was billed, and is now an orphan: it has a conversation and
a `runs` row (`jod ls` can still find it), but no goal to attribute it to —
`jod goal log` has nothing, because the goal is gone. This degrades gracefully
rather than crashing (`Ticker::note`, `core/src/ticker.rs:844`, falls back to a
`run/<id>` scope when `goal_named` comes back empty), but the objective that
justified spending the money no longer exists anywhere a person would look for
it.

Fix: `delete_goal` should stop the in-flight run (the same `kill_agent` call
`carry_out`'s `Replace` branch already uses for schedules,
`core/src/ticker.rs:696`) before — or as part of — deleting the row, at least
when a `current-run` fact names something still running.

Check: start a goal's iteration, delete the goal mid-run, assert the run is
stopped rather than left to finish unattended.

---

## G9. A wedged goal iteration is correctly killed and marked failed — the spec's own worst case works
Status: verified (real code path, synthetic run — see below) · Severity: n/a (pass)

This is a pass, recorded because the spec explicitly calls out Change 1b — a
goal iteration that wedges must be terminated and failed, unlike a plain
session — as a regression risk. It holds.

`Ticker::tick_heartbeats` (`core/src/ticker.rs:761`) runs before
`tick_goals` in the composite tick, specifically so a wedged run's status has
already become `failed` by the time the goal asks about it
(`core/src/daemon.rs:72`). Verified by planting a synthetic `running` run and
heartbeat row (`pgid` pointing at a PID that does not exist, `last_progress_ms`
25 minutes stale — past `DEFAULT_STALL_MS`, 20 minutes) tied to a real goal via
a `current-run` fact, then ticking:

```
before: runs.status = running, heartbeats row present
$ jod daemon --once
after:  runs.status = failed, heartbeats row gone
```

`heartbeat::decide` correctly classified it `Stalled`, which both `terminates()`
and `fails_the_run()` (`core/src/heartbeat.rs:251`, `:260`). The goal then
picked up the failed run and moved forward rather than waiting on it for ever
— in this case straight to `exhausted`, because the test goal was capped at
`--max-iterations 1` specifically so proving the mechanism did not also require
paying for a second real spawn.

One adjacent gap, found while setting this up rather than being the point of
it: the synthetic run's read-back went through `tick_goals`'s `Err(_)` branch
(`core/src/ticker.rs:1002`, "the run is gone from the store entirely") rather
than the ordinary `Ok(agent)` branch — meaning `self.jod.agent(&run)` could not
be built into an `AgentSummary` for a hand-inserted row missing whatever a real
supervised run always has. That branch calls `advance_goal` with `progressed:
false` but — unlike the `Ok(agent)` branch — never writes an `iteration` fact,
so `jod goal log` for this goal shows no iteration line despite the counter
having advanced. Whether a *real* run can ever land in this `Err` branch was left open here.
**It is now answered — yes — and filed as G13 below.**

---

## G13. A real run reaches `tick_goals`'s `Err(_)` branch, and the iteration is lost
Status: open · Owner: — · Severity: medium

Answered while fixing G8, and recorded here because it lived only in a merged
pull request body. A finding that exists in a diff description and nowhere else
has to be established a second time by whoever needs it next — the same waste
the negative results in this file were written down to avoid. It is also easy to
lose: it was found while doing something else, it is not what that pull request
fixed, and G8's own note called it unresolved.

**Two demonstrated routes for a real run to reach the branch:**

1. It falls outside the daemon's **200-run rehydrate window**.
2. `rehydrate` silently skips it because its stored summary was written by a
   build with a different `AgentSummary` shape.

**What the branch then does:** the goal's counter advances, no `iteration` fact
is written, the iteration's cost never reaches `spent_usd` because the branch
passes `0.0`, and `progressed: false` pushes the goal toward a false stall.

So a goal can burn a real, paid-for iteration, record nothing about it, and be
marked as making no progress — the same shape as G2, which is fixed.

**Cross-reference: that 200-run window is the same constant F4 found capping
`list_agents`** (`REHYDRATE`, in `tasks/20-fleets.md`). The two entries describe
it independently and a reader of either would not know the other exists. Anyone
changing it should read both — it bounds what the router can see *and* what the
goal loop can settle.

Check: drive a goal iteration whose run is outside the rehydrate window; assert
an `iteration` fact is written and `spent_usd` includes its cost.

---

## G10. `jod goal ls`, `pause`, `resume` and `rm` carry no help text
Status: open · Owner: — · Severity: low

`jod goal --help` shows blank descriptions for `ls`, `pause`, `resume` and
`rm`, and `jod goal rm --help` / `jod goal ls --help` etc. print only the bare
usage line — no explanation of what they do, unlike `add`, `run`, and `log`,
which all carry doc comments. `pause`/`resume` in particular are worth a line
saying what happens to an in-flight iteration when the goal they belong to is
paused (see G-observations under "pause/resume" in the scenario table — it
keeps running to completion, unattended, and is picked up on the next tick
after `resume`, which is not obvious from the command name alone).

Fix: add a one-line doc comment to each variant in `GoalCommand`
(`cli/src/main.rs:1261`), matching the style already used for `add`/`run`/`log`.

Check: `jod goal ls --help`, `jod goal pause --help`, `jod goal resume --help`,
`jod goal rm --help` each print more than a bare usage line.

---

## G11. `jod goal add ""` creates a goal with an empty name
Status: **fixed — merged as #168** · Severity was: low

Fixed with S6 at the store level, so the MCP tools are covered too — a CLI-only
fix would have looked complete and passed a CLI check.

```
$ jod goal add "" "empty name test"
 is running
$ jod goal ls
◎                       iter 0    $0.00
  empty name test
```

Nothing rejects an empty `<NAME>`. It can be listed, paused, and removed by
passing `""` again, so it is not unrecoverable, but it renders as a blank line
in every listing and is easy to create by accident (an empty positional arg
from a script, a stray quoting mistake) with no feedback that anything odd
happened.

Fix: reject an empty (or whitespace-only) name in `goal_command`'s `Add` arm
before calling `store.add_goal`.

Check: `jod goal add "" "x"` exits non-zero with a clear message instead of
succeeding.

---

## G12. A duplicate goal name surfaces a raw SQLite error
Status: **fixed — merged as #159** · Severity was: low

Fixed together with S5 in one pull request: one defect on two surfaces.

**A negative result from the same work, worth keeping.** All seventeen `UNIQUE`
constraints in the schema were traced to their inserts and to whether an
ordinary mistake can reach them. Only the two filed here could leak a raw
message. Three candidates suggested for checking turned out not to exist at all
— `works` has no unique title, `projects.name` is not unique, and
`team_members` upserts. So there is no follow-up, and nobody should audit the
seventeen a second time.

```
$ jod goal add zero-budget "duplicate objective text" --budget 5
Error: database: UNIQUE constraint failed: goals.name

Caused by:
    0: UNIQUE constraint failed: goals.name
    1: Error code 2067: A UNIQUE constraint failed
```

Correctly refused, but the message names a SQL table and constraint rather
than saying "a goal named `zero-budget` already exists" — the same shape of
issue `jod goal log` guards against with its own `bail!("no goal {name}")`
right above this code (`cli/src/main.rs:4213`).

Fix: check `store.goal_named(&name)?.is_some()` before `add_goal` and bail with
a plain message, the same way `Log` already does for the opposite case.

Check: adding a goal with a name already in use prints a plain "already
exists" message, not a database error.

---

## Scenarios run

| Scenario | Expected | Actual | Pass/fail |
|---|---|---|---|
| Set a goal, read it back (`add` → `ls`/`ls --json`) | Fields round-trip exactly | They do | pass |
| Run one iteration by hand (`jod goal run` + `daemon --once`) | Spawns exactly one real agent run | Confirmed, `started 1` | pass |
| Goal satisfied on the first iteration | Stops after that iteration | Stops, but always pays for the iteration first (G6) and the iteration/spend go unrecorded (G2) | fail (G2, G6) |
| Goal that never becomes satisfied — what stops it | `stall_after` (no-progress) or a set cap | Works if `no_progress` can ever rise (see next row); has **no** cap at all if the check's output is non-deterministic | fail (G5, needs confirming) |
| No-progress detector: does it notice, how many iterations burn first | Stalls after `stall_after` (default 6) genuinely-unchanging iterations | Correct when the check's output actually repeats — but a *quiet* check (no stdout) is scored as "no progress" even when the agent is doing real work, and a *noisy* check is scored as "progress" even when it isn't (G3, G5) | fail (G3) |
| Budget exhaustion, crossed during normal operation | Goal stops, state → `exhausted` | Confirmed: `spent_usd` crossed `budget_usd` mid-run, next settle correctly flipped state | pass |
| Budget/iterations pre-exhausted at creation (`--max-iterations 0`, `--budget 0`, negative budget) | Same — refused at creation, or marked `exhausted` immediately | Neither: state stays `running` forever, silently claimed and released every tick (G4) | fail (G4) |
| Pausing mid-flight | In-flight run is left to finish; goal is not reclaimed while paused; not settled until resumed | Confirmed on both counts | pass |
| Resuming | Picks up the already-finished run and settles it | Confirmed — but only at the next cron-due tick, not immediately on resume; for an hourly goal that can be up to an hour of stale `iter`/cost display (not filed as its own bug — `jod goal run` works around it — but worth knowing) | pass, with a caveat |
| Goal whose iteration wedges (stalls) | Killed and marked failed, unlike a plain session, per spec Change 1b | Confirmed via synthetic heartbeat/run rows | pass (G9) |
| Goal whose iteration fails outright, repeatedly | `no_progress` climbs each failure, eventually stalls | Reasoned from code (`advance_goal`'s `progressed=false` path) and exercised via G9's synthetic `Err` branch; not run to a real repeated-failure stall for cost reasons | needs confirming |
| Goal spawning work with no roots | Roots inherited from `cwd` like any other conversation | Confirmed absent — every goal iteration is rootless (G7) | fail (G7) |
| Interaction with schedules firing at the same tick | Independent claims, no interference | `tick_heartbeats` → schedules → goals run in a fixed order every pass on separate claim tables (`core/src/daemon.rs:63`); no shared state found | pass (read from code, not raced under load) |
| Duplicate goal name | Refused | Refused, but with a raw DB error (G12) | fail (G12, cosmetic) |
| Empty name | Refused or handled cleanly | Silently accepted, renders as a blank row (G11) | fail (G11) |
| Unicode name (`目標-🎯`, `日本語`) | Works | Works — added, listed, removed cleanly | pass |
| Zero iterations (`--max-iterations 0`) | Refused or marked exhausted immediately | Stuck running forever (G4) | fail (G4) |
| Negative budget / negative max-iterations | Refused, or `--flag -5` at least parses | `--budget -5` is rejected by clap as an unknown flag (needs `--budget=-5`); `--budget=-5` is then accepted with no validation and reproduces G4 | fail (G4; clap UX noted, not filed separately) |
| Goal with an unreachable success condition | Eventually stalls or exhausts, never runs forever | True only if `max_iterations`/`budget` is set or the check's output is genuinely stable (G5) | needs confirming / fail (G5) |
| Deleting a goal mid-iteration | Either stops the run or clearly documents it does not | Run continues unattended to completion, now orphaned (G8) | fail (G8) |
| `jod goal ls`/`pause`/`resume`/`rm` help text | Present | Absent (G10) | fail (G10) |
