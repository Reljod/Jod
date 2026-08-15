# Goals — standing objectives that run until they are met

A goal is meant to stop on its own: satisfied when its check passes, stalled
when nothing it does changes what the check sees, exhausted when it runs out of
iterations or money. The three findings below are all in that stopping
machinery. Two were seen on a live goal on this box; the third is read off the
code and says so.

The goal used throughout is `satisfied-first`, which was still on the hourly
cron in the local store while this was written.

---

## G1. A goal's memory is keyed on its name, so a new goal inherits a dead one's record
Status: open · Owner: — · Severity: high

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

Fix: read scoped. Add a scoped reader beside `facts_about` — subject *and*
scope — and give it the goal's `memory_scope()` at all three call sites. That is
smaller and more honest than deleting facts on `rm`, because it also fixes the
case where the old goal still exists. Deleting the goal's scope in `delete_goal`
is worth doing as well, so a removed goal does not leave rows behind for ever,
but it is the second fix, not the first.

Check: add a goal, write an `ended` fact for it, remove it, add another goal
with the same name, and assert `goal log` on the new one shows nothing from the
first.

### Observed, not argued

`jod goal ls` showed `satisfied-first` running, on iteration 2, with
`done_when` set to `false` — a check that cannot ever pass. `jod goal log
satisfied-first` printed:

```
pursuing  totally different second objective
ended     satisfied
  2: Nothing changed. I made no edits this iteration, and here is why…
  1: Hello.
```

It reports a goal that has ended satisfied and, underneath, the two iterations
it ran after ending. Both lines are true of a different goal that held the name
before this one.

---

## G2. The iteration that satisfies a goal is never recorded
Status: open · Owner: — · Severity: medium

In `tick_goals` the satisfied branch (`core/src/ticker.rs:936`) writes the
`ended` fact, sets the state and `continue`s. The `iteration` fact is written
further down, after that branch has already left. So the last iteration of a
goal — the one that did the work that made the check pass — is missing from
`jod goal log`, and a goal satisfied on its first run has no iteration history
at all.

That is also what made G1 visible: the goal that held the name before this one
left an `ended satisfied` fact and no iteration line, so the two records merged
into something that looked almost plausible.

Fix: record the iteration before the satisfied branch returns. The outcome text
and the cost are both already in hand at that point.

Check: run a goal whose check passes on the first iteration and assert
`goal log` shows one iteration and the `ended satisfied` line, not just the
line.

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
happened. The intended signal — the check's exit status changing from failing to
passing — is the one thing the fingerprint deliberately throws away.

Fix: fingerprint the status alongside the output, so a check that goes from
failing to passing counts as progress even when it says nothing; and when the
output is empty on both runs, fall back to something the agent controls rather
than scoring it as no progress. Whatever the second half turns out to be, it
needs to be stated in `docs/decisions.md`, because "no measurable change means
stop" is a deliberate rule and this is an exception to it.

Check: a goal with `done_when: test -f DONE` whose agent creates the file
partway through should end satisfied, not stalled.

### Observed, not argued

`satisfied-first` runs `false` as its check. It has completed two iterations and
carries `no_progress: 1` — the counter rose on the second one, which is the
first with a previous fingerprint to compare against. Its iteration 1 wrote
"Hello." and its iteration 2 wrote a paragraph explaining that nothing changed,
so in that particular case stalling is the right answer; but the counter did not
rise because of anything either iteration did. It rose because `false` prints
nothing twice, and it would have risen identically had both iterations shipped
real work.
