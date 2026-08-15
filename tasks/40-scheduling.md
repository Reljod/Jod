# 40. Scheduling

How this was tested: the built binary at `target/debug/jod`, on `PATH`, against
an isolated `JOD_HOME=/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-sched`
(never `~/.jod`). Schedules were armed with `jod schedule add`, fired with
`jod schedule run` + `jod daemon --once` (which drives one real tick without a
resident process), and read back with `jod schedule ls --json` / `jod schedule
log` and direct `python3 -c "import sqlite3..."` reads of `jodhome-sched/jod.db`.
Some scenarios needed time to have passed that hadn't — those are marked
**(synthetic)** and say exactly what was edited in the database (typically
`next_fire_at_ms` or `last_fire_at_ms`) before the next real tick ran. One
scenario (overlap) needed a genuinely alive process group to test honestly; a
`setsid sleep 300 &` stood in for a long-running agent, and its real pid was
written into `runs.pgid` so the rehydrate-and-liveness-probe path was exercised
for real, not faked past. No daemon was left running at the end; all spawned
processes finished or were killed.

Nothing here is about missing roots on the *main chat* — that's `tasks/00`.
This file only covers what's specific to scheduling.

---

## S1. The circuit breaker never trips for the ordinary way a schedule fails
Status: open · Owner: — · Severity: high

`BREAK_AFTER_FAILURES = 5` and the backoff curve exist, by the module's own
account, because a schedule whose every run fails made **288 spawn attempts in
24 hours** with nothing counting it (`core/src/schedule.rs:1-17`). I reproduced
that exact scenario and the breaker did not fire.

Reproduction: armed a schedule pointed at a directory, then deleted the
directory (`rmdir`) before it ever fired — the "schedule pointing at a deleted
project or directory" case, and also the ordinary way a schedule breaks in
practice (a repo gets moved, a worktree gets cleaned up, a project gets
archived). Forced it due every minute and ticked seven times in a row:

```
=== tick 1 ===  claimed 1 · started 1 · held 0 · failed 0
state= armed failures= 0
=== tick 2 ===  claimed 1 · started 1 · held 0 · failed 0
state= armed failures= 0
... (identical through tick 7)

$ jod schedule log doomed-dir -l 15
✗ ran, then failed
✗ ran, then failed
✗ ran, then failed
✗ ran, then failed
✗ ran, then failed
✗ ran, then failed
✗ ran, then failed
```

Seven consecutive real failures (`jod ls` shows all seven runs as `failed`),
`consecutive_failures` stayed `0` the whole time, `state` never left `armed`,
and `next_fire_at_ms` never backed off — it kept firing on the ordinary
once-a-minute cadence. Left alone, this schedule fires and fails for ever.

Cause: `core/src/ticker.rs:388-511` (`Ticker::tick`). The `failed` local
(`ticker.rs:416`) is set to `true` only when `self.carry_out(...)` returns
`Err` (`ticker.rs:426`) — i.e. only when *starting the process* fails
synchronously (missing harness binary, DB write error). It is never set when
`carry_out` returns `Ok` because the process launched fine and the harness
*inside* it failed afterward — a bad `cwd`, a crashing agent, an
unreachable-model error. That is reported later, asynchronously, by the
supervisor updating the run's own `status` to `failed`. Nothing in the tick
ever looks back at a previous fire's eventual outcome before calling
`store.release_schedule(&s.id, now_ms, failed)` at `ticker.rs:503`, which is
the only caller of `release_schedule` (`core/src/store.rs:2502`) anywhere in
the codebase — confirmed by grep, so there is no other path that could catch
this after the fact either.

So `FireOutcome::SpawnFailed` (which does count) covers a fairly rare failure
mode, and the overwhelmingly more common one — the run started, then the
harness immediately errored out — is filed as an ordinary `Ran` and never
revisited.

Fix: after a `Decision::Run`/`Decision::Replace` starts a process, the tick (or
a later tick, via `running_run`'s same lookup) needs to check whether that run
*actually succeeded* before computing `failed` for `release_schedule` — at
minimum, checking the previous fire's run status the way `running_run` already
does, and feeding that into the next tick's failure count.

Check: point a schedule at a directory that doesn't exist, `jod daemon --once`
it five times, assert `consecutive_failures >= 5` and `state == "broken"`.

---

## S2. A cron expression that can never fire is armed anyway
Status: open · Owner: — · Severity: high

The module's own comment on `validate` says the reason it exists: "a schedule
that can never fire is refused when it is written rather than discovered as
silence weeks later" (`core/src/schedule.rs:281-287`). It does not do that.

```
$ jod schedule add --cron "0 0 31 2 *" feb31 "test"
feb31 armed
```

No error, and the listing shows it as armed with nothing to wait for:

```
$ jod schedule ls
● feb31    0 0 31 2 *    —
```

```json
{"name": "feb31", "state": "armed", "next_fire_at_ms": null, ...}
```

This schedule is indistinguishable at a glance from one that fires far in the
future — `jod schedule ls` just shows `—` for both a genuinely-armed schedule
whose owner is asleep and one that is dead on arrival. Someone arms "the 31st
of every month" or "the last Friday" written wrong, sees "armed", and finds out
never.

Cause: `validate` (`core/src/schedule.rs:285-287`) calls `next_fire` and maps
`Ok(_) => Ok(())` — it only checks that the *call* didn't error, not that it
found an occurrence. `next_fire` (`core/src/schedule.rs:258-279`) returns
`Ok(None)` for a syntactically valid pattern that matches no real date (croner
searches forward and gives up rather than erroring), and `Ok(None)` passes
`validate` exactly like `Ok(Some(_))` does.

Fix: `validate` should reject `Ok(None)` the same as `Err(_)` — a cron
expression that resolves to no next occurrence is exactly the case this
function exists to catch.

Check: `assert!(schedule::validate("0 0 31 2 *", "UTC").is_err())`. (The
existing test `a_nonsense_expression_is_refused_when_it_is_written` in
`core/src/schedule.rs:542-546` checks a syntactically broken expression and an
unknown timezone, but not this case — a syntactically valid, semantically
impossible one.)

---

## S3. A scheduled run's conversation has no roots of its own
Status: open · Owner: — · Severity: medium

Related to the main-chat root bug in `tasks/00`, but a separate instance with
a separate cause worth tracking on its own: it is not that the same code path
is reused, it is that the scheduling path has *never* called the seeding
function either.

```
$ jod schedule run nightly-triage && jod daemon --once
claimed 1 · started 1 · held 0 · failed 0
```

```
--conversations--
{'id': 'dd268...', 'cwd': '/home/.../scratch-repo', 'title': 'Triage the inbox', ...}
--conversation_roots--
(empty)
```

The run's process `cwd` is correct — `settle_cwd`
(`core/src/service.rs:199-220`) is a no-op here because a schedule's `cwd` is
always stored as an absolute path (`req.cwd.is_absolute()` short-circuits at
`service.rs:200`), so the launch-time root check that gates ordinary spawns
never even runs for a schedule. But the *conversation* Jod opens for the run —
`open_conversation` → `store.new_conversation(...)` at `service.rs:284-294` —
never calls `Store::ensure_inherited_root` (`core/src/roots.rs:309`), same as
the main chat. Confirmed by grep: `ensure_inherited_root` has no caller outside
its own tests anywhere in the codebase.

Current practical impact is limited: `ToolAccess::unattended()`
(`core/src/harness/mod.rs:420-422`) caps every scheduled run to `ReadOnly`, and
there is no per-schedule override to raise that yet (grepped for one; none
exists), so the write-gated, root-gated MCP tools (`open_work`, `delegate`,
etc.) aren't reachable from a scheduled run today regardless. But `list_roots`
(`core/src/mcp.rs:1777-1795`) *is* a read tool available at every access level,
and it will tell a scheduled agent it has zero directories to work in — a
9pm/2am agent asking Jod's own tool "where can I work" gets told "nowhere",
even though its actual process `cwd` is fine and its filesystem tools work.
That's a self-contradictory answer for the agent to reason about, and the day
a per-schedule `Orchestrate` override ships (the docstring at
`harness/mod.rs:418-419` says it's "worth having"), any schedule using it will
hit the exact `open_work`-refuses-with-no-roots failure from `tasks/00` — at
2am, unattended, for a schedule that has a perfectly good `cwd` and just was
never given a root to match it.

Fix: the schedule spawn path (`Ticker::spawn`, `core/src/ticker.rs:1837-1866`)
or `open_conversation` itself should call `ensure_inherited_root` for the
conversation it just opened, the same fix `tasks/00`'s L1 proposes for the main
chat — ideally the same change covers both call sites.

Check: fresh `JOD_HOME`, fire a schedule, assert `jod root ls` (or
`store.roots(conversation_id)`) for the resulting conversation is non-empty and
origin `inherited`.

---

## S4. `fire_once` (the default misfire policy) leaves no trace of what it dropped
Status: open · Owner: — · Severity: low

Compare two live runs, both after 3-6 hours of simulated downtime **(synthetic:
`last_fire_at_ms` set into the past)**:

`misfire: skip` (`--misfire skip`), 3h down on a 15-min cron, one real tick:

```
$ jod schedule log skip-test -l 30
✓ ran
○ skipped_misfire  missed while Jod was not running
○ skipped_misfire  missed while Jod was not running
... (12 of these)
```

`misfire: fire_once` (the default), 6h down on a 15-min cron, one real tick:

```
$ jod schedule log catchup-test -l 30
✓ ran
```

Nothing else — 23 other missed instants leave no row anywhere. This isn't
wrong by the design intent (`decide`'s `FireOnce` arm,
`core/src/ticker.rs:218-220`, deliberately returns a single `Decision::Run` and
nothing else, matching the test `a_long_outage_produces_exactly_one_run_by_default`),
but it sits oddly next to the stated design principle right above it on
`FireOutcome`: "every outcome is written down... a skip nobody recorded is a
silent failure" (`core/src/schedule.rs:180-182`). For the *default* policy,
every skip *is* unrecorded — a person looking at `jod schedule log` after an
outage sees one ordinary-looking `ran` and has no way to tell "that instant was
6 hours late and 23 others were dropped" from "that ran right on time."

Fix (worth considering, not clearly a bug): have `FireOnce` also emit
`Decision::Hold { outcome: SkippedMisfire }` for the discarded instants, the
way `Skip` already does — it's one branch away from `Skip`'s own code
(`ticker.rs:221-236`) and would cost one row per instant rather than the
silence.

Check: after an outage with `fire_once`, assert `schedule_fires` contains one
`ran` and N-1 `skipped_misfire` rows for N missed instants, matching `Skip`'s
contract today.

---

## S5. Duplicate-name error leaks a raw SQL message
Status: open · Owner: — · Severity: low

```
$ jod schedule add --cron "0 3 * * *" nightly-triage "dup test"
Error: database: UNIQUE constraint failed: schedules.name

Caused by:
    0: UNIQUE constraint failed: schedules.name
    1: Error code 2067: A UNIQUE constraint failed
```

Every other validation error in this area (`schedule.rs`'s `Misfire`/`Overlap`
parsers, `validate`) returns a plain English `JodError::Invalid` message. This
one comes straight from rusqlite with no translation, three lines of internal
detail a person didn't ask for, for what is a completely ordinary mistake
(re-arming a name that's already taken). Not filing a `Fix:`/`Check:` beyond:
catch the constraint violation in `add_schedule` and return
`JodError::Invalid(format!("a schedule named `{name}` already exists"))`.

---

## S6. Empty and unicode schedule names both work, but empty ones can't be told apart in a list
Status: open · Owner: — · Severity: low

```
$ jod schedule add --cron "0 3 * * *" "" "empty name test"
 armed — next Aug 16 03:00 (in 9h03m)
$ jod schedule add --cron "0 4 * * *" "夜間トリアージ🌙" "unicode test"
夜間トリアージ🌙 armed — next Aug 16 04:00 (in 10h03m)
```

Unicode names are handled correctly end to end (armed, listed, fired,
addressable by `jod schedule pause "夜間トリアージ🌙"` — not separately shown
above but consistent with the json round-trip). The empty name is accepted
without complaint, and `jod schedule ls` prints a blank name column for it —
indistinguishable from terminal padding or a rendering glitch:

```
$ jod schedule ls
●                        0 3 * * *       Aug 16 03:00 (in 9h03m)
```

If two empty-named schedules were ever created, `pause`/`resume`/`rm <name>`
(which key on `name`) would become ambiguous. Fix: reject a blank/whitespace
name the same way an invalid cron is rejected, at `add` time.

---

## Confirmed correct (worth recording as passes, not just gaps)

- **S1 originally miscalled**: my first two attempts to prove overlap=skip
  broken were artifacts of faking a "running" row in the database without a
  genuinely alive process behind it — `rehydrate` (`core/src/service.rs:670-769`)
  correctly downgrades a stale `running` row to `Failed` once it can't find a
  live process group, so those attempts were re-testing the crash-recovery
  path, not overlap. Once retested with a *real* long-running process
  (`setsid sleep 300` standing in, its pid wired into `runs.pgid`) and tight
  timing so the fire genuinely landed while it was still running, `overlap:
  skip` (the default) correctly held the second fire and recorded
  `skipped_overlap`:
  ```
  claimed 1 · started 0 · held 1 · failed 0
  ○ skipped_overlap   ...is still running
  ✓ ran
  ```
- **`jod schedule resume` does clear the failure count**, as the CLI help
  claims. **(synthetic: `consecutive_failures` and `state` set directly)**
  `broken, 7` → `resume` → `armed, 0`, confirmed against
  `Store::set_schedule_state` (`core/src/store.rs:2536-2568`), which zeroes
  `consecutive_failures` in the same statement that arms it.
- **Deleting a schedule mid-flight doesn't touch its in-progress run.** Fired a
  schedule, caught it genuinely `running` (polled `jod ls` in a tight loop,
  caught on the 2nd poll), ran `jod schedule rm` — it succeeded immediately,
  the run kept going untouched, and the schedule's row and history were gone.
  Reasonable behaviour: a schedule is a trigger, not the run's owner.
- **A badly-overdue schedule is visible, not silent, in `jod schedule ls`.**
  Forced `next_fire_at_ms` 10 days into the past **(synthetic)**: the listing
  shows `Aug 05 18:01 (240h00m ago)` rather than hiding it or showing the
  original armed time. A person checking with no daemon running would notice.
- **Invalid cron and unknown timezone are both rejected at `add` time**, with
  plain messages (`not a cron is not a cron expression: ...`, `unknown
  timezone Mars/Olympus`).
- **A very long prompt (50,000 chars) is accepted and round-trips intact**
  through `add` and `ls --json`.
- **DST handling** (spring-forward gap runs at 03:00 rather than vanishing;
  fall-back's repeated local hour fires once, not twice) is exercised by the
  repository's own unit tests in `core/src/schedule.rs:508-539`, which I read
  and did not need to re-derive; not independently re-run against a live clock
  given the time budget — **needs confirming** if an end-to-end check against
  `jod daemon` itself (rather than the pure `next_fire` function the tests
  already cover) is wanted.

## Not run / needs confirming

- **True clock skew** (system clock jumping backward mid-run) — not
  reproducible without changing the sandbox's wall clock. Code has explicit
  guards for it in two places (`due_to_poll`, `core/src/ticker.rs:125-135`;
  `trim_ledger`, `ticker.rs:544-558`, both treat `at > now_ms` as "due" so a
  clock rewind can't lock a feature out indefinitely), but nothing analogous
  was seen for `claim_due_schedules`'s own due-check — **needs confirming**
  against `core/src/store.rs`.
- **`jod daemon` with genuinely no process ever started** — trivially true
  from every scenario above (nothing fired until a real tick ran), and stated
  outright in the daemon's own `--help` text ("`jod schedule ls` describes work
  that will never happen"). Not filed as a separate finding since it's
  documented, working-as-designed behaviour, not a bug.
- **`jitter_ms`** — confirmed hardcoded to `0` at every schedule-creation call
  site (`cli/src/main.rs:4076`, `core/src/mcp.rs:1284`); there is no `--jitter`
  flag anywhere. This matches the module doc's own conclusion that jitter
  measured worse and defaults to off, so not filed as a bug — flagging only
  because the same doc describes a refusal-when-exceeding-grace-window path
  that has no way to be reached today.

## Scenarios run

| # | Scenario | Expected | Actual | Pass/Fail |
|---|---|---|---|---|
| 1 | Arm a schedule, read back next fire | `ls` shows correct next instant | Matched cron exactly | Pass |
| 2 | `jod schedule run` then ordinary tick fires it | Fires through the tick, not directly | `run` sets `next_fire_at_ms=now`; only `daemon --once` actually spawns | Pass |
| 3 | Fire via real tick, check conversation created | Conversation + fire row, both correct `cwd` | Correct, but zero roots (S3) | Fail (S3) |
| 4 | `misfire: fire_once` after 6h down | One run, no other trace required by design | One run, but genuinely zero trace of the other 23 (S4) | Partial (S4) |
| 5 | `misfire: skip` after 3h down | One run + Hold rows for every skipped instant | Exactly that: 1 `ran` + 12 `skipped_misfire` | Pass |
| 6 | `overlap: skip` while a real process is genuinely still running | Second fire held, `skipped_overlap` recorded | Held correctly | Pass |
| 7 | `overlap: skip` against a *stale* DB row with no live process | Should NOT be treated as running | Correctly downgraded to failed/not-alive by rehydrate | Pass |
| 8 | Repeated real failures (schedule pointed at a deleted directory) | Backs off, breaks after 5 | Never backed off, never broke, 7/7 silent misses of the breaker | **Fail (S1, high)** |
| 9 | `jod schedule resume` clears failure count | `consecutive_failures` → 0, state → armed | Confirmed | Pass |
| 10 | Invalid cron expression | Refused at `add` | Refused, clear message | Pass |
| 11 | Unknown timezone | Refused at `add` | Refused, clear message | Pass |
| 12 | Valid but never-firing cron (Feb 31) | Refused at `add` | **Armed, `next_fire_at_ms: null`, fires never** | **Fail (S2, high)** |
| 13 | Deleting a schedule mid-flight | Run unaffected, schedule + history gone | Confirmed | Pass |
| 14 | Duplicate name | Refused | Refused, but with a raw SQL error (S5) | Partial (S5) |
| 15 | Empty name | Reasonable to refuse or accept | Accepted; unlabelled in listing (S6) | Partial (S6) |
| 16 | Unicode name | Works end to end | Confirmed | Pass |
| 17 | Very long prompt (50k chars) | Accepted, intact | Confirmed | Pass |
| 18 | No daemon running at all | Nothing fires, and this is visible | Confirmed, documented in `--help`, overdue schedules show clearly | Pass |
| 19 | Badly overdue schedule display | Shown as overdue, not hidden | `240h00m ago` shown | Pass |
| 20 | DST — spring-forward gap / fall-back double hour | Correct per repo's own tests | Not independently re-run live | Needs confirming |
| 21 | Clock skew | Should not wedge scheduling | Partial guards found, not fully verified | Needs confirming |
