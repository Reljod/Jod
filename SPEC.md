# SPEC — an engineer is reused on its own subject only, and one that is finished is put away

A manager hands every instruction to whichever engineer is free, whatever the
instruction is about. That is what Jod tells it to do today, in three separate
places and on purpose. The cost has turned out to be the thing the placement
rules exist to prevent: the free engineer is often one that was started on the
real checkout, `continue_agent` resumes it exactly where it sat, and the new
task is written straight into Reljod's working copy with no branch and no
worktree. Nobody decided that. It fell out of "reuse whoever is free".

This spec inverts the reuse rule, makes "finished" a state Jod can read rather
than a thing a manager remembers, and adds one sweep that puts finished and
stranded engineers away.

## Goal

A manager reuses an engineer only for an instruction that carries on what that
engineer was already doing. Anything on a different subject opens a new engineer
with a worktree of its own, so a task that writes can no longer land in Reljod's
real checkout by accident. An engineer stranded on the checkout is archived after
an hour, and one whose pull request has merged stops being offered immediately
and is archived a day later.

Done when the thirty checks under **Verification** pass and the three passages
named under **Where the behaviour actually comes from** no longer say what they
say today.

## The problem, in Reljod's words

> it seems like we still don't follow the rule where manager should ask engineer
> to create new worktree if working on an existing ones. Maybe because it
> re-uses an engineer and since that engineer is already working on main, it
> didn't create a new one.

And the rule he wants instead:

> an engineer should only be re-used if what it's working is truly related to
> the task OR if it's not working on main and it already past 1 hr. If already
> past 1 hr and the engineer is in main, then we need to archive that, and
> create new engineer and worktree. Once a worktree is finished (already created
> PR and merged), work is considered done and we should always not re-use that
> engineer unless directly stated. Engineer that are done should be auto-archive
> in 1 day.

Two parts of that were ambiguous and were settled by asking. Both answers are
recorded under **Decisions settled by interview** below, and they narrow the
work: the one-hour window governs **archiving only**, never reuse, and
relatedness is the **manager's judgement** rather than something Jod computes.

## Where the behaviour actually comes from

This is not a bug in the sense of a mistake. Three passages say plainly that a
free engineer takes anything, and they will keep saying it until they are
changed.

1. **`core/src/mcp.rs`, in `list_agents`** — the `reuse` sentence, built around
   line 1591:

   > `` `{run_id}` `` is free. Continue it with `continue_agent` — it already
   > holds this checkout, so it starts where a new session would have to start
   > over. **Prefer it for any instruction here, including one on a different
   > subject.**

2. **`core/src/orchestrator.rs`, in the manager's brief**, from line 662. Three
   passages, each stronger than the last:

   > Then hand the instruction to an engineer who is free, **whatever it is
   > about**.

   > An engineer of this project who is not busy is your answer for *any*
   > instruction about {project}, not only for one that carries on what it was
   > last doing.

   > **Do not open a second session beside a free one because the new
   > instruction looks like a different subject. Different subject, same
   > repository, same engineer.**

3. **`docs/decisions.md`**, under *"A scratch session is reused on the same
   subject only, which is the opposite of the engineer rule"*, which states the
   contrast as settled reasoning. That decision is not wrong about scratch. It
   is wrong about engineers, and this spec is the correction.

Nothing else has to change for the symptom to stop. The placement machinery is
already correct; it is simply never reached, because reuse always wins first.

## What is already true — do not rebuild it

Every fact this spec needs is already in the database. There is no new
bookkeeping to invent, only queries to write and sentences to rewrite.

- **`conversations` already carries `held`, `archived_at_ms` and `ephemeral`**
  (`core/src/store.rs`, migration `0029`). Archiving is a column, not a new
  table. `held = 1` is the manual override that beats every automatic rule, and
  it must keep beating them here.
- **`Store::archive_conversation` and `Store::unarchive_conversation` exist**
  and are exactly what is wanted. `continue_agent` already calls the second one
  unconditionally on every resume (`core/src/mcp.rs`, around line 2026), so an
  archived engineer that a manager deliberately continues comes back to the
  fleet by itself. That is the whole of *"unless directly stated"* — no new
  path is needed for it.
- **`leases` links a conversation to a worktree** — `conversation_id`,
  `worktree_path`, `branch`, `state` of `held | released | removed`, with a
  unique index on one live lease per work and repository. An engineer with no
  `held` lease is an engineer that is not in a worktree.
- **`pull_requests` links a conversation, a lease and a work to a URL and a
  state** of `draft | open | merged | closed | unknown`, reconciled by
  `Ticker::tick_pull_requests`. `state = 'merged'` is already authoritative and
  already discovered without anybody watching, including for a pull request
  merged long after the session ended.
- **`Ticker::tick_scratch` is the sweep to copy** (`core/src/ticker.rs`, line
  1871). It archives what is ready and deletes what is past retention, it is
  best-effort per row, and it reports `claimed` / `started` / `failed`. The new
  sweep is its sibling and should read like it.
- **`Store::scratch_ready_to_archive` is the query to copy**
  (`core/src/store.rs`, line 2697). Note what it refuses to archive: a row whose
  latest run is not `completed`, one with a queued delivery, and one whose run
  is marked stalled. All three exclusions matter here for the same reasons.
- **The roster's `idle` and `reuse` are computed over everything the filter
  matched, not over the page.** Keep that. A finished engineer that fell off the
  end of `limit` must still be excluded.

## Decisions settled by interview

Two readings of the rule were possible and they led to materially different
work. Reljod chose:

**The one-hour window governs archiving, never reuse.** An idle engineer whose
old task is unrelated is not offered for reuse at any age. It is not made
reusable by going stale. The alternative — treat an hour-old worktree engineer
as fair game for anything — was rejected because reusing it means the new,
unrelated change lands on a branch that may already have a pull request open on
it, which is worse than the problem being fixed.

**The manager judges relatedness; Jod stops pushing.** Jod does not compute
whether a task carries on a subject. It reports what each engineer was last
doing and states the rule, and the manager decides. The alternative — gate reuse
on the work id — was rejected because a genuinely related follow-up filed as new
work would be locked out, and because a manager that cannot exercise judgement
is a dispatcher again.

## D1 — the reuse sentence says the opposite of what it says today

`core/src/mcp.rs`, `list_agents`.

The sentence keeps its shape: one plain-English sentence naming one run, because
the answer is a choice between two tool calls and the reason for the choice is
the half that keeps getting lost. What changes is what it says.

It must now do three things it does not do today.

**Name the subject.** The manager cannot judge relatedness without being told
what the engineer was doing. The row already carries `last_message`; the
sentence must quote or summarise it rather than leaving the manager to go
looking. One line, trimmed — this is a preamble paid on every routing decision.

**State the rule, in the direction scratch already states it.** Continue this
engineer only if the instruction carries on that subject. A different subject
gets a new engineer with a worktree of its own.

**Say where the engineer is, when it is on the checkout.** An engineer sitting
on the real checkout is the case that produced this spec. When the head of
`idle` is one of those, the sentence has to say so and say what follows: writing
here means writing in Reljod's working copy, so anything that writes needs
`open_work` with `placement: "worktree"` instead.

Draft wording, to be tightened in review rather than copied blindly:

> `run-7` is free. It was last working on *"tighten the lease sweep"*. Continue
> it with `continue_agent` **only if this instruction carries on that subject** —
> it holds that work in its head and starts where a cold session would have to
> start over. A different subject is a different job: open a new engineer with
> `open_work` and a worktree of its own, so the two changes do not land on one
> branch.

And, when that engineer is on the checkout rather than in a worktree, a second
sentence:

> It is working in the checkout itself, not in a worktree, so anything it
> writes lands in Reljod's working copy. Continue it only for reading. Anything
> that writes gets `open_work` with `placement: "worktree"`.

The empty cases keep their present wording, except that "nothing free" now has a
new reason to be true — see D4 — and the sentence should not claim there is
nothing when there is something that has been withheld. Where an engineer was
excluded because it is finished, say so, so the manager is not told a lie about
an empty fleet.

**The scratch sentence does not change.** It was right, and after this change
the two sentences agree rather than contradicting each other. The paragraph of
comment above `scratch_reuse` that explains why the two say opposite things is
now wrong and must be rewritten, not left.

## D2 — the manager's brief says the opposite too

`core/src/orchestrator.rs`, the manager brief, from line 662.

Rewriting the `reuse` sentence alone is not enough. The brief is prepended to
every one of the manager's turns and it currently instructs the manager to
ignore exactly the distinction this spec introduces. Left as it is, the manager
reads a sentence saying "same subject only" inside a brief saying "different
subject, same engineer" and the brief will win, because it is framing and the
other is data.

Three passages change:

- *"hand the instruction to an engineer who is free, whatever it is about"* →
  hand it to a free engineer **that was working on this subject**.
- *"is your answer for any instruction about {project}, not only for one that
  carries on what it was last doing"* → is your answer for an instruction that
  **does** carry on what it was last doing. The paragraph's reasoning — it holds
  the repository in its head, a cold session buys that again and buys it wrong —
  stays, because it is true and it is why reuse is worth having at all. Only its
  scope narrows.
- *"Do not open a second session beside a free one because the new instruction
  looks like a different subject. Different subject, same repository, same
  engineer."* → **deleted, and replaced by its opposite.** A different subject
  is a different job. Open a new engineer and give it a worktree.

The list of "the three cases where you open something new instead" gains two
more, and stops being three:

- **Every free engineer is on a different subject.** The commonest case after
  this change, and the one the spec exists for.
- **The only free engineer has finished** — its pull request is merged, its job
  is over. See D4.

And one line in the tools list is now false and must go. Under
`continue_agent` the brief says reuse *"is also how you get around the engineer
cap honestly: reusing a free engineer adds no process, so it is never refused."*
After this change that is an invitation to reuse an engineer on the wrong
subject in order to dodge the cap. Delete it. The cap consequence is real and is
addressed under **What this costs** below.

## D3 — "on the checkout" is a fact Jod reads, not a guess

An engineer is in a worktree when its conversation holds a lease in state
`held`. Otherwise it is on the checkout. That is the whole test, and it is one
query for the fleet rather than one per row, in the shape `run_contexts` and
`scratch_runs` already use.

    Store::runs_in_worktrees() -> HashSet<String>

Every run whose conversation holds a live lease, joined `messages.run_id` →
`conversations.id` → `leases.conversation_id` where `leases.state = 'held'`. A
run not in the set is on the checkout.

**Do not use `cwd` for this.** It is the obvious approach and it is the one the
codebase has already been bitten by: the comment on `AgentView::project` records
that grouping by directory split one project's agents into two groups, because a
session holding a lease has the worktree as its cwd. Comparing `cwd` against a
project root would work until a worktree is nested somewhere unexpected, and
then fail quietly. The lease is the record; read the record.

Carry the answer onto the row as a field, so the manager and the sweep read the
same fact:

    /// Whether this agent writes in a worktree of its own.
    ///
    /// False means it is sitting in the real checkout, where anything it writes
    /// lands in Reljod's working copy. That is the case this fleet keeps
    /// falling into by accident — a free engineer started on the checkout is
    /// resumed for a task that writes, and no worktree is ever cut, because
    /// nobody asked for one.
    worktree: bool,

## D4 — done means a merged pull request

An engineer is **done** when a pull request whose `conversation_id` is that
engineer's conversation has reached state `merged`. The branch is in, the work
that engineer was for is over, and its context is now a description of code that
has already landed.

    Store::finished_engineer_runs() -> HashMap<String, i64>

Run id to the instant its pull request was reconciled as merged, joined the same
way as D3 through `pull_requests.conversation_id`. The instant is what the
one-day archive window in D5 is measured from, so it has to come back with the
answer rather than being looked up again.

Where an engineer has several merged pull requests, the **newest** merge is the
one that counts. That is the instant it stopped having anything to do.

What follows from being done:

- **It never appears in `idle`, and `reuse` never names it.** Not "deprioritised
  — excluded, the way scratch is excluded today, and for a stronger reason: a
  scratch session at least still knows something, whereas this one knows a
  branch that no longer exists as a branch.
- **It is still on the roster**, with a field saying why it is not offered:

      /// Its pull request has merged, so the job it was opened for is over.
      /// Excluded from `idle` and never named by `reuse`. A manager that
      /// genuinely wants it back continues it by run id, which unarchives it.
      finished: bool,

- **`continue_agent` still works on it.** No refusal, no override argument. The
  manager naming a run id **is** the "directly stated" case, and a refusal with
  an escape hatch would be a second thing to learn for a case that is already
  unambiguous. The tool should say what it noticed, though: where the run being
  continued is finished, prepend one line to the result reporting that its pull
  request merged and that its worktree may be gone, so a manager that reached
  for it out of habit finds out immediately rather than three turns later.

**Merged, not opened.** A draft pull request means the engineer is waiting for
review and may well have more to do — review comments are the commonest reason
to continue an engineer, and locking that out would be a worse bug than the one
being fixed. Only `merged` ends it. `closed` deliberately does not: a pull
request closed without merging usually means the work is being redone, and the
engineer that has it in its head is the one to redo it.

## D5 — two archive rules, one sweep

A new `Ticker::tick_engineers`, a sibling of `tick_scratch`, called from the
same place in `Ticker::tick` and reporting the same counters.

**Rule one — stranded on the checkout.** An engineer that is idle, holds no
live lease, and has not been active for longer than
`engineer_checkout_idle_minutes` is archived. It is the case Reljod named: the
engineer that should never have been the answer to the next instruction, sitting
on the real checkout, waiting to be picked. Archiving it removes it from `idle`
and from `reuse`, so the next instruction opens a fresh engineer with a worktree
— which is what should have happened in the first place.

**Rule two — finished for a day.** An engineer that is done under D4 and whose
newest merge was more than `engineer_done_retention_days` ago is archived. It is
already excluded from reuse the moment its pull request merges; this is only
about the roster getting long. A day is a deliberate delay rather than an
immediate archive, because the hours right after a merge are exactly when
somebody notices something and wants the session that did it.

**An engineer in a worktree that is not done is never archived by age.** It
holds a branch, and possibly an open pull request, and a roster that quietly
loses it loses the only pointer to that branch. It is excluded from reuse for
being on a different subject, which is enough. Do not add a third rule here.

**What the sweep must refuse to touch**, copied from
`Store::scratch_ready_to_archive` and for the same reasons:

- `held = 1`. The manual override wins over both rules, always.
- A conversation whose latest run is not `completed`. Archiving a working
  engineer is archiving something mid-turn.
- A conversation with a queued delivery. Mail in flight to a row that has left
  the fleet is mail nobody reads.
- A run marked stalled. A stall is a thing for a person to look at, and hiding
  it is the opposite of raising it.

Add to those:

- **Any conversation that is not an engineer.** The query must be narrow:
  `work_id IS NOT NULL AND ephemeral = 0`. A main chat, a manager's conversation
  and the assistant all have completed runs and would all match a careless
  query. `Store::router_run_ids` already knows which conversations are routers,
  and the sweep has to exclude them — archiving a project's manager would take
  the project off the air.

## D6 — what archiving an engineer does, and what it must not do

Archiving sets `archived_at_ms` and nothing else. It is not deletion and not a
final state.

- The transcript stays on disk and stays searchable.
- The lease stays. The branch stays. The worktree stays. Nothing about a
  worktree's lifecycle changes in this spec, and an archived engineer's branch
  is exactly as recoverable as it was before.
- `continue_agent` unarchives, already, today, with no change.

**The scratch retention sweep must not start deleting engineers.** It deletes
archived rows past `scratch_retention_days` and it is scoped `WHERE ephemeral =
1`, which is what keeps engineers out of it. Nothing in this spec widens that
scope, and widening it would turn a roster-tidying change into one that destroys
transcripts. There is no deletion half to this spec at all: engineers archive
and stay.

The fleet pane and `list_agents` both have to leave archived engineers out, the
way they leave archived scratch rows out. Check both.

## D7 — the settings

Two keys, spelled and defaulted like the two the scratch lane already has, so
all four read alike.

| Key | Default | `0` means |
| --- | --- | --- |
| `engineer_checkout_idle_minutes` | `60` | never archive for sitting on the checkout |
| `engineer_done_retention_days` | `1` | never archive for being finished |

`0` is off in both, matching `scratch_retention_days` and
`scratch_reuse_window_minutes`. A value that will not parse falls back to the
default rather than failing the tool, for the reason
`Store::max_engineers_per_project` gives about its own broken values: a typo in
a settings row must cost the setting, never the sweep.

Negative values must behave as `0`, not as "archive everything". `tick_scratch`
already makes this mistake impossible by returning early on `days <= 0` and by
using saturating arithmetic on the cutoff. Copy both.

## Migration

One migration. **`0032` is the next free number as of writing** — the highest
across the checkout and all seven sibling worktrees is
`0031_a_task_owns_its_files` — but the charter's rule stands: take the number by
reading every worktree at the moment you write it, not by trusting this line.

    0032_an_engineer_is_put_away_when_it_is_finished

It adds no columns. `archived_at_ms`, `held` and `ephemeral` are already there
and are what this uses. What it adds is the partial index the two new queries
need, the sibling of `ix_conversations_scratch`:

    CREATE INDEX IF NOT EXISTS ix_conversations_engineer
      ON conversations(archived_at_ms, held)
      WHERE work_id IS NOT NULL AND ephemeral = 0;

Same column order and the same reasoning: `archived_at_ms` leads because it is
what every reader asks about first, and `held` follows so held rows drop out of
the index rather than sending the query back to the table.

Apply it one statement at a time, skipping what is already there — that is this
repo's established migration rule and it is written down in
`docs/decisions.md`.

## Files & interfaces

One engineer per row. No two rows share a file.

| Files | What changes |
| --- | --- |
| `core/src/store.rs` | migration `0032`; `runs_in_worktrees`; `finished_engineer_runs`; `engineers_ready_to_archive`; the two settings readers and writers |
| `core/src/ticker.rs` | `tick_engineers`, and its call from `tick` |
| `core/src/mcp.rs` | the `reuse` sentence; `worktree` and `finished` on `AgentView`; excluding finished and archived engineers from `idle`; the note on `continue_agent`; the now-wrong comment above `scratch_reuse` |
| `core/src/orchestrator.rs` | the manager's brief — the three passages and the tools list |
| `docs/decisions.md` | the reversal, written as a decision |
| `docs/jod-system.md`, `docs/harness-config.md` | the two new settings keys, wherever the scratch pair is documented |

`core/src/leases.rs` and `core/src/prs.rs` are **read from and not changed**.
Both already record what this needs. A change proposed in either of them during
this work is a sign the query went the wrong way round; go back to D3 and D4.

## Verification

Every one of these is a test, and each names the decision it comes from. All of
them run together:

```
cargo test -p jod-core
```

Confirm the named tests actually appear in the output rather than reading the
summary line. `cargo test -- a b c` silently runs only the first filter, and
this repo has already lost two hours to a green summary over checks that never
executed — see `TASKS.md`, *"Running a check is itself something to check"*.

**Blocked is a legal ending.** If any of these cannot be made to pass without
faking something — see **Sanctioned fakes** for the short list of what may be
faked — write `BLOCKED.md` with `Missing:` / `Tried:` / `Needs:` and every
failing suite path, and stop. Do not weaken an assertion, widen a `catch`, or
narrow a check to the part that already passes to get to green.

**D1 — the sentence**

1. A free engineer produces a `reuse` sentence that contains its subject, drawn
   from `last_message`.
2. That sentence tells the caller to continue **only** on the same subject, and
   contains no instruction to prefer it for a different one. Assert the absence
   as well as the presence — the old wording is what regressions look like here.
3. A free engineer with no live lease produces the extra sentence naming
   `placement: "worktree"`.
4. A free engineer in a worktree does not produce that extra sentence.
5. The scratch sentence is unchanged and still names its own run.

**D3 — where an engineer is**

6. An engineer whose conversation holds a `held` lease comes back `worktree:
   true`.
7. The same engineer after its lease is released comes back `worktree: false`.
8. An engineer that never claimed one comes back `worktree: false`.

**D4 — done**

9. An engineer with a merged pull request is absent from `idle` and unnamed by
   `reuse`, while still appearing in `agents` with `finished: true`.
10. An engineer with a **draft** pull request is still offered for reuse. This
    is the check that stops the rule being written against the wrong state.
11. An engineer with a **closed, unmerged** pull request is still offered.
12. `continue_agent` on a finished engineer succeeds, and its result carries the
    line saying the pull request merged.

**D5 — the sweep**

13. An idle engineer on the checkout, older than the window, is archived.
14. The same engineer inside the window is not.
15. An idle engineer in a worktree, older than the window and not done, is
    **not** archived.
16. A done engineer is archived once its merge is older than the retention
    window, and not before.
17. `held = 1` survives both rules.
18. A running engineer survives both rules.
19. An engineer with a queued delivery survives both rules.
20. A stalled engineer survives both rules.
21. A manager's conversation and the main chat survive the sweep. Build the
    fixture so both have a completed run and would match a query that forgot to
    exclude routers.
22. A scratch conversation is untouched by `tick_engineers`, and an engineer is
    untouched by `tick_scratch`.
23. Both windows at `0` archive nothing.
24. A negative window archives nothing.

**D6 — archiving is reversible and destroys nothing**

25. An archived engineer is absent from `list_agents`.
26. `continue_agent` on it clears `archived_at_ms` and it returns to the roster.
27. Its lease is still `held` and its worktree path still recorded after
    archiving.
28. The scratch retention sweep, run against a database of archived engineers,
    deletes none of them.

**D2 — the brief.** The brief is a string, and the check is a string check —
worth having anyway, because the passages being removed are the exact thing that
will get reintroduced by somebody fixing something else.

29. The manager's brief contains no sentence telling it to reuse an engineer for
    a different subject. Assert against the removed phrases by name.
30. It does contain the instruction to open a new engineer with a worktree for a
    different subject.

## What this costs, and why it is still right

**Managers will hit the engineer cap more often.** `DEFAULT_MAX_ENGINEERS` is 3
per project. Today a manager routes ten unrelated instructions to one engineer
and never comes near the cap. After this change those are ten engineers, and the
fourth is refused.

This is a real consequence and it should not be smoothed over by quietly raising
the default. Three things make it survivable, in this order:

1. **The sweep is what makes room.** An engineer stranded on the checkout goes
   after an hour; a finished one goes a day after its merge. The cap is on
   engineers that are *live*, and after this change far fewer of them are.
2. **`explore` placements do not need to be engineers at all.** The brief
   already offers `delegate` for a one-shot that needs no board — a lookup, a
   check, a script — and much of what currently gets routed to a warm engineer
   is exactly that. Reuse was hiding how much.
3. **If the cap still bites, it is a settings change and a visible one.**
   `max_engineers_per_project` exists and is a row in `settings`. Raising it is
   a decision somebody makes and can see, which is the opposite of what is
   happening today, where the cap is dodged by reuse and nobody notices.

Watch for the cap after this ships. If it is refusing managers regularly on real
work, that is evidence for a higher default, and it should be argued from the
evidence rather than pre-empted here.

## Sanctioned fakes

**Nothing in production code.** Every fact this spec needs is already recorded in
SQLite, and no part of it calls out to a network, a forge or a clock that a test
cannot set. If you find yourself reaching for a mock in `core/src/`, the query
went the wrong way round — go back to D3 and D4 and read the record instead.

In tests, exactly three things are built rather than produced:

- **Rows written straight into the store.** Leases in `held`, pull requests in
  `merged`, conversations with an `archived_at_ms`. This is the existing
  convention in `core/src/store.rs` and `core/src/mcp.rs` tests and it is how
  the sweep's twelve cases get their fixtures at all.
- **The clock.** Both sweeps take `now_ms` as an argument, the way
  `tick_scratch` does. Pass an instant; never sleep, and never assert on wall
  time. This repo has one test that flakes on a busy box for exactly that
  reason.
- **`gh` is not called.** `pull_requests.state` is read from the table, never
  reconciled during a test. `Ticker::tick_pull_requests` is what writes it and
  it is not part of this work.

Note the standing warning in `tasks/30-project-managers.md` M4: fifteen
git-dependent tests in `core/src/leases.rs` return silently when git is absent,
so "green" there does not distinguish *ran and passed* from *never ran*. The
lease fixtures in D3's checks inherit that. Confirm those three tests appear by
name in the output.

## Escalate on

Stop and ask rather than deciding these alone.

- **The cap starts refusing managers on real work.** The spec says explicitly
  not to pre-empt this by raising `max_engineers_per_project`. If it bites
  during execution, that is evidence worth reporting, not a default to change.
- **Any of the three passages in D1 and D2 turns out to be load-bearing
  somewhere this spec did not look.** The brief is prepended to every manager
  turn and something else may be reading it.
- **A check contradicts a decision.** When a check fails, suspect the check
  first — it was written before the fix existed. But if the check is right and
  D1–D7 are wrong about how the code actually behaves, that is a spec error and
  it should be corrected in place and marked, the way this repo's other shipped
  specs record their corrections.
- **The migration number has moved.** Expected, and handled by re-reading every
  worktree. Escalate only if two branches have genuinely taken the same number.

## Decision log

<!-- Written during execution. One line per decision the spec did not settle:
     what was chosen, and why. Corrections to D1-D7 belong here too, marked in
     place in the decision they correct. -->

## Out of scope

- **Deleting engineer transcripts.** This spec archives and stops. Retention for
  engineers is a separate decision with a separate blast radius.
- **Removing worktrees when an engineer is archived.** Archiving is a roster
  change. `release_worktree` and the lease sweep own worktree lifecycle and are
  untouched. Reljod has an existing rule that worktrees are swept by `locked`
  state rather than by idle time; nothing here should be read as a reason to
  revisit it.
- **Computing relatedness.** Settled by interview: the manager judges. Do not
  add a similarity score, a shared-file heuristic, or a work-id gate.
- **Changing the scratch reuse rule.** It was right. After this change the two
  rules agree, which is the only thing about scratch that this touches.
- **Raising `max_engineers_per_project`.** See above — argue it from evidence
  after this ships, not as part of it.
