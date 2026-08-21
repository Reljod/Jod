# TASKS — index

Open work found by testing the orchestrator end to end: launching real consoles,
sending real instructions, firing real schedules and goals, and reading the
result. **This file is an index only.** Every finding lives in the per-area file
below, so several people can file at once without touching the same file.

One owner per task. Claim one by putting your name in its `Owner:` line before
you start. Add findings to the area file, never to this index.

Counts below were taken by reading each file's `Status:` lines on 16 August
2026. They go stale the moment someone files or closes a finding, so trust the
area file over this table and correct the table when you notice it drifting.

| Area | File | Findings | Done | Open | Owner |
|---|---|---|---|---|---|
| Launch, roots and the console's directory | [`tasks/00-launch-and-roots.md`](tasks/00-launch-and-roots.md) | L1–L10 | 5 | 4 + 1 blocked | — |
| Routing: answer it yourself or hand it over | [`tasks/01-routing.md`](tasks/01-routing.md) | R1–R7 | 4 | 3 | — |
| The TUI itself | [`tasks/02-tui.md`](tasks/02-tui.md) | T1–T3 | 1 + 17 shipped | 2, both decisions | — |
| Orchestration scenarios | [`tasks/10-orchestration.md`](tasks/10-orchestration.md) | O1–O10, **one critical** | 2 | 8 | — |
| Orchestration edge cases and bad input | [`tasks/11-orchestration-edge-cases.md`](tasks/11-orchestration-edge-cases.md) | E1 + 14 scenarios | 1 | 0 | — |
| Fleets and the tree | [`tasks/20-fleets.md`](tasks/20-fleets.md) | F1–F9 | 5 | 4 | — |
| Project managers | [`tasks/30-project-managers.md`](tasks/30-project-managers.md) | P1–P5; T1–T7 **shipped** | 4 + T1–T7 | 1 | — |
| Scheduling | [`tasks/40-scheduling.md`](tasks/40-scheduling.md) | S1–S6 | 5 | 1 | — |
| Goals | [`tasks/50-goals.md`](tasks/50-goals.md) | G1–G14 | 9 | 6 | — |

That is **36 findings closed, 27 open and one blocked**. The seven
project-manager tasks T1–T7 have since shipped, so the largest pile of open work
is orchestration. What is left under project managers is P4, a decision about
`State::Paused` that is Reljod's to make rather than anybody's to build.

Every file ends with a "Scenarios run" table listing what was tried, what was
expected, and what happened — passes included. A clean pass is worth recording;
it is what stops the same ground being covered twice.

## Running a check is itself something to check

**On one of them I was one keystroke from filing a regression against working
code.** That is the cost worth naming: not a wasted check, but a wasted fix, and
a correct implementation changed to satisfy a wrong test — worse than the bug
the check was written for.

Verifying the tasks below produced four near-misses **in the verification**,
two in each direction:

- **`cargo test -- a b c` silently ran only one filter.** It reported "ok. 43
  passed; 0 failed" while two of the three checks never executed. Caught only by
  grepping the output for the named tests instead of reading the summary line.
- **An O2 run told the model to use no tools**, leaving the
  `ToolCall`/`ToolResult` half of that check unexercised while the result looked
  like a pass.
- **An unfaithful fixture produced a false failure.** F6's check says "seed a
  work with one session and one completed run". Seeded exactly that, the delete
  reported no runs kept while the run row survived — which looks precisely like
  the bug. It was not: the code counts runs through the `messages` table, and a
  run with no transcript has nothing to lose.
- **A check was arithmetically wrong.** S1's said "tick five times, assert five
  failures". At five ticks the count is four, because a tick starts a run and
  only the *next* tick sees it fail — the asynchrony the bug was about. It would
  have condemned a working fix.

So: a green summary is not evidence the thing you meant to run ran, and a red
one is not evidence the code is broken. **When a check fails, suspect the check
first** — it was written at diagnosis time by someone who did not yet know what
the fix would look like. Always confirm the named test appears in the output.

## What a second reader is for

Two sessions worked this list, and each reviewed the other's work only where it
had no stake — the reviewer had neither filed the finding nor written the brief
the agent reasoned from. That constraint mattered more than the reviewing did:
the person who commissioned work cannot judge whether their own agent talked
itself past a rule, because they wrote the rule it would be talking past.

**The clearest case it caught would have failed no test.** A change made
settling a goal execute its `done-when` command. Correct in an ordinary tick.
But another task, four minutes into its own work, was about to route *paused*
goals through that same path — which would have meant a paused goal shelling out
to run a command, arriving as an unnamed side effect of a sensible-looking
reuse. Nothing would have gone red either way. Flagged before both landed, the
agent decided it deliberately and wrote down why; flagged after, it would have
been a behaviour nobody chose.

So the arrangement earns its keep on interactions rather than on defects: a
diff shows what a change alters, and a reader who knows the neighbouring work
sees what it *enables*.

## Merged does not mean recorded, and that has two causes

The statuses in these files drift, and twice now a sweep has found a batch of
tasks reading `open` whose fixes had merged. Both causes are structural rather
than anybody forgetting, which is why writing them down is worth more than
flipping the lines again.

**One: the person who knows is not the person who can act.** Agents fixing these
tasks are told not to edit files under `tasks/`, because several sessions read
them and one owner per path is the rule. They obey, and reference their task id
in the pull request body instead. So the flips accumulate on one side and nobody
else can make them. That is the right trade — a conflict in a shared file costs
more than a stale line — but the stale line is its price, and it should be a
known cost rather than a surprise.

**Two: branching from `origin/main` orphans unmerged work.** Five verifications
I had recorded — L4, G4, O3, F3, F4 — were missing from main, because I started
each new sweep with `git checkout -B <new> origin/main` while the previous
branch's pull request had not yet merged. The commits survived and were
recovered by cherry-pick, but for a while the file said `open` about tasks I had
personally verified, which is the worst version of this: a record contradicted
by its own author's work.

**It is invisible while it happens**, which is why it ran five times before
anyone noticed. The new branch is clean, CI passes, the pull request merges —
nothing anywhere reports a problem. It surfaces only later, as a file that has
quietly reverted.

**The remedy is one line: branch from the previous branch, not from `main`,
until the previous one has landed.** Anyone running several pull requests in
sequence against the same file has this waiting for them.

**The rate is roughly one stale line per merge**, and there have been about
fifty merges. Three sweeps in one hour each found a fresh batch. That is not a
series of oversights, it is the measured cost of the arrangement above, and it
is worth knowing as a rate rather than rediscovering as an incident.

**So treat the `Status:` lines as lagging merges by design.** They are corrected
in batches, not continuously, and between batches they will name work that has
landed.

**The authority is not this file.** Every merged pull request names its task id
in its body, so `gh pr list --state merged --json number,body` is what to check
before concluding a task is still yours to do. Deriving the statuses from that would stop the drift
recurring. Nobody has built it, and until somebody does, the manual sweep is a
known cost — not an oversight to blame anyone for.

## Which tasks can be verified at all

The rule below depends on tasks having a `Check:` line, so here is how many do.
Of 70 entries, **24 carry none** — but most of those are exempt by kind rather
than incomplete:

- **O4–O10, G9, F7, F8** record a *pass* or cite the spec rather than asking for
  work. There is nothing to verify; they are evidence, not tasks.
- **T1–T7** are the project-manager spec's work, and each names which of that
  spec's own seventeen numbered checks proves it. Their checks exist, by
  reference.
- **E1** is folded into L3 and says so.

That leaves **six real tasks that were filed without a check**: L5, L6, R2, R3,
S5, S6. Four are since fixed or closed, and L6's was written by the agent that
fixed it — in the form the other tasks use, asserting both spellings, which is
the right instinct.

**The point worth keeping: a task with no check cannot be verified, so
"verified" is silently unavailable for it.** That absence looks identical to
nobody having got round to it. If you file a task, give it a check; if you fix
one that has none, write the check you ran.

## A merge closes a pull request. The `Check:` line closes the task.

Learned the hard way, twice in one afternoon, and worth more than any single
finding here.

Every task below carries a `Check:` line — the runnable thing that proves it is
done. We closed tasks on *merge* instead, and a merge only says a pull request
finished. Those are different claims:

- **L3** was marked fixed when one of its seven sites had been fixed. The pull
  request was honest and did exactly what its brief asked; the brief had
  narrowed silently from the task. L3's check says assert `jod main` **and**
  `jod run` both root themselves — it would have failed on `jod run` in
  seconds, and nobody ran it.
- **F4** was marked done against a trigger that measurement showed was wrong in
  both directions at once.

So: a pull request merging is not a task changing status. Run the task's own
check, and if the check has two halves, both halves must pass. If you
deliberately fix only part of a task, say so and leave it open — a partly-fixed
task marked done is worse than an open one, because nobody looks at it again.

## Start here

**Read `10-orchestration.md` O1 first. It is the most serious thing in this
list and it is not what Reljod reported.** A work session cannot write
anything. `open_work` starts a session with the checkout read-only and expects
it to call `claim_worktree` when it needs to write — but the harness turns
roots into `--add-dir` flags once, at process launch
(`core/src/harness/claude.rs:74`), and `claim_worktree` only returns a path
string. Nothing widens a running process's sandbox, so the worktree it just
claimed is outside what it may touch. Observed end to end, twice: the session
claimed a worktree, tried to write, was refused, and reported the block
honestly. Every `open_work` session that needs to write deadlocks the same way
— which is the entire reason `open_work` exists rather than `delegate`.

After that, the reported bug. It is three separate faults that happen to
produce one symptom.

1. **`00-launch-and-roots.md` L1** — the console Reljod uses starts at boot from
   `jod-tui.service`, which sets no `WorkingDirectory`, so it runs in `$HOME`.
   `$HOME` is not a repository. The root-seeding code is correct and is simply
   handed the wrong directory once, for ever.
2. **`00-launch-and-roots.md` L2** — new work defaults to `roots.first()`, and
   root order is append-only, so the oldest root wins permanently. Adding the
   right repository later cannot help. This is what turns L1 into the symptom.
3. **`01-routing.md` R1 and R4** — asked one factual question, the orchestrator
   spawned an agent, sat in a shell loop waiting for it, and after 42 seconds
   and $0.39 returned no answer at all.

## Everything rated high or critical, in one place

Buried in the area files otherwise, and several of these are worse than the bug
that prompted the sweep.

**The `State` column is a snapshot and it will rot.** This list came out of one
afternoon's testing; it is not a live tracker, and nothing updates it when a
pull request merges. It went stale twice while its own correcting pull request
was open. So treat the column as "true when written" and check the pull request
before concluding a task is still yours to do — `gh pr list --state all` costs
one command and is the only authority. A status that says `open` about finished
work is the same failure this list keeps documenting.

| | What | Where | State |
|---|---|---|---|
| **critical** | A work session cannot write anything — `claim_worktree` cuts a worktree the running process may not touch | `10-orchestration.md` O1 | **needs Reljod** |
| high | Every tool call waits 60s for an approval hook with nobody there. Four file reads take 4m14s under `ask`, 7s under `auto` | filed separately as #138 | **needs Reljod** |
| high | The console is rooted at `$HOME`, which is not a repository | `00-launch-and-roots.md` L1 | **needs Reljod** |
| high | A goal's memory is name-keyed, so a new goal inherits a dead one's record | `50-goals.md` G1 | fix open, #126 |
| high | New work defaults to the oldest root for ever | `00-launch-and-roots.md` L2 | blocked on L1 |
| high | Re-adding a root you already hold silently revokes write access | `00-launch-and-roots.md` L7 | open |
| high | A goal whose stop condition is already true never stops — claimed and released every tick, for ever | `50-goals.md` G4 | open |
| high | A goal with no cap and a non-deterministic check has no ceiling at all | `50-goals.md` G5 | open |
| high | It busy-waits on its own child with `sleep`, which the design forbids | `01-routing.md` R4 | open, now small |
| high | The orchestrator will not answer, or will — it is a coin flip nothing decides | `01-routing.md` R1 | in flight |
| high | "Stop an agent and everything it started" does not stop what it started | `20-fleets.md` F5 | in flight |
| high | A cron expression that can never fire is armed anyway | `40-scheduling.md` S2 | in flight |
| high | `list_agents` hid running agents entirely past 200 runs — `running_only` returned nothing while three ran | `20-fleets.md` F4 | **fixed, #143** |
| high | It reaches outside Jod's tool set on *every* run | `01-routing.md` R5 | **fixed, #127** |
| high | The main chat can permanently lose a run's reply | `10-orchestration.md` O2 | **fixed, #133** |
| high | The schedule circuit breaker never trips for the ordinary failure | `40-scheduling.md` S1 | **fixed, #135** |
| high | Two projects sharing a name resolve non-deterministically | `30-project-managers.md` P3 | **fixed, #131** |
| high | Re-cataloguing a project wipes its aliases and notes | `30-project-managers.md` P1 | **fixed, #123** |
| high | Once any work exists, every loose run vanishes from the fleet screen | `20-fleets.md` F1 | **fixed, #121** |
| high | A run's tree node cannot say finished, failed or killed | `20-fleets.md` F2 | **fixed, #130** |

## A proposed charter change — Reljod's call, deliberately not made

**Not applied. Nobody should apply it without Reljod.** Two agent sessions
independently think it is right, which is not the same as authority to edit the
charter, and a charter that agents amend among themselves is not a charter.

The charter says "Every task needs one runnable check. Without one, 'looks
done' is the only stop signal and you are the loop." That justifies the check
by whether the work is *finished*. This sweep hit two failures that wording
does not cover, and they are different from each other:

1. **A bug that was not real.** The first finding in this list was filed from
   code plus the live database and was wrong — the behaviour had already been
   fixed and the old rows were recording old behaviour. The defence is
   *reproduce before you fix*.
2. **A fix whose mechanism was wrong.** Two readers told the P1 agent to guard
   the columns with `COALESCE`, copying a correct pattern one line away in the
   same file. It cannot work there, and it would have shipped with tests
   passing. The defence is *see the test red before the fix, not merely green
   after*.

The second is the uncomfortable one. Reasoning from a correct neighbouring
pattern is what a careful reviewer would also do, so review would not have
caught it. Only running it would.

Proposed: extend that charter line so the check is justified by whether the work
is *correct*, not only whether it is done — and say that a fix's check must be
observed failing first. Both practices are already in use by both sessions; the
question is only whether they belong in the charter.

## What this sweep corrected

An earlier version of this list claimed `jod tui` does not add the directory you
launch it in. **That was wrong**, and it was wrong because it was written from
the code and the database without launching a console. `ensure_launch_root`
(`cli/src/tui/mod.rs:5509`) already works, on both this branch and the installed
release, and adds every launch directory.

The lesson is written into the files: a finding taken from code plus old rows
tells you what the code says and what the data became, and neither tells you
what happens when the thing runs. Old rows are the worst of it — they record
behaviour that has since been fixed. Anything below marked `needs confirming`
has not been run, and should be run before it is built against.

## Context worth reading before you design anything

**The manager spec has shipped.** It is
[`docs/spec-ceo-and-managers.md`](docs/spec-ceo-and-managers.md), marked shipped,
with a section at the end recording what was built, how its four corrections were
applied, and how its seven open questions were answered. Read it as a record
rather than as work to pick up.

What landed: every session is watched and a stalled one is marked rather than
killed; `works.project_id` and a wider `AgentView` carrying project, work,
`stalled_for_ms` and `busy`; a manager conversation per project, reached by
`ask_manager`; `open_work` and repository-pointed `delegate` refused from main at
the tool boundary; two preambles instead of one; and project and manager levels
in the fleet tree.

Two things from the review that anyone working near this still needs:

- **A manager must not use `pinned = 1`.** `Store::pinned_conversation` is a
  `query_row` with no `LIMIT` and no ordering, so a second pinned row makes
  "which conversation is main" depend on SQLite's row order — and Reljod's
  instructions would start landing in a manager's transcript. Managers live on
  `projects.manager_conversation_id`, and
  `creating_a_manager_does_not_disturb_the_main_chat` holds it.
- **Routing to a manager is already deterministic.** `settle_project` runs on
  the raw instruction before the model turn, so `ask_manager` is wiring, not
  reasoning. Anything here treating the choice of manager as a judgement call is
  overbuilt.

The catalog findings P1–P5 below are separate from the manager work and are
tracked on their own.
