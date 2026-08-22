# SPEC — the manager plans, engineers only execute, and the manager is the one who says it is done

A manager today routes an instruction to whichever engineer is free and gets out
of the way. That is dispatch, not management. This spec makes the manager do the
thinking an engineer should not have to do: break the instruction into tasks,
decide where each one is allowed to write, hand each engineer a set of files
nobody else owns, and hold the answer until the whole job is finished.

An engineer stops being a small orchestrator and becomes what its name says. It
takes one task, does it, reports to its manager, and stops.

## The problem, in Reljod's words

> For project related stuff, I want the project manager to think about the
> things the engineer needs to do like creating tasks, planning how to breakdown
> tasks for parallel engineers to run. The engineer only job is to take task
> from the project manager, do the task, then report back to manager. Manager
> should be the one telling main if job is already finished.

And the placement rule, which is the other half:

> Manager should decide whether we direct in main, create worktree or re-use a
> worktree. Just make sure we plan which file each engineer should only handle
> to avoid conflicts if working on the same worktree. Use main only if it's a
> new project (fresh) and it's the first iteration. Use worktree approach for
> already existing projects. There should be 1 worktree created ONLY if it
> writes to the project. Engineers can be also used for exploratory and read
> only stuff. Engineers can share a worktree as well. If a project exists in git
> remote, then it should create PR per worktree and manager should ensure that
> each PRs are stacked to each other.

## How this sits with the unblock-main spec

The sibling spec
[`spec-unblock-main-and-roles`](../spec-unblock-main-and-roles/SPEC.md) inserts
an **assistant** between main and the managers, and makes main hand over and
return. Nothing here contradicts it, and this spec does not touch main's
preamble, `ask_assistant`, the scratch lane or the roles panel.

The one seam the two share: that spec says main answers nothing itself and the
assistant routes anything touching a repository to `ask_manager`. This spec
picks the story up at `ask_manager` and describes what the manager does with the
instruction once it arrives. The chain after both ship is

```
main → assistant → manager → engineers
```

and this spec owns only the last arrow. Written so it can land before, after or
alongside the other one.

## What is already true — do not rebuild it

Checked by reading the code in this worktree. Six of these are easy to
duplicate by accident.

- **The manager layer shipped.** `docs/spec-ceo-and-managers.md`. A manager is a
  conversation on `projects.manager_conversation_id`, reached by `ask_manager`,
  running at `ToolAccess::Delegate` with `manager_preamble`
  (`core/src/orchestrator.rs:401`). `open_work` is already refused from main.
- **A work already has a board.** `Store::add_work_task`, `Store::work_tasks`
  and `Store::complete_work_task` (`core/src/works.rs:1193`, `1222`, `1245`) all
  exist and work, and completing the last task already closes the work and
  returns a `Closing`.
- **The board already has an owner column.** `tasks.owner`, surfaced as
  `TeamTask.owner` (`core/src/team.rs:127`).
- **Worktrees are already shared between siblings in one work.**
  `Store::claim_lease` (`core/src/leases.rs:324`) returns `Claim::Reused` when a
  sibling in the same work already holds a lease on that repository, and a
  partial unique index enforces one live lease per work and repository.
- **An engineer already claims its own worktree only when it needs to write.**
  `claim_worktree` (`core/src/mcp.rs:615`) is the explicit step, and the worker
  preamble tells the session its roots start read-only. **"One worktree only if
  it writes" is already the behaviour.** What is missing is the manager being
  able to say up front that an engineer is read-only.
- **Auto-PR already exists as an instruction, not as a `gh` call.**
  `prs::auto_pr_instruction` (`core/src/prs.rs:1273`) asks the session to run the
  `create-pr` skill, deliberately, because Jod shelling out to `gh pr create`
  would open exactly the evidence-free PR the charter forbids.
- **`gh stack` is installed on this box** (`github/gh-stack v0.1.0`), and
  `gh stack link` creates a stack on GitHub **from existing PRs without local
  tracking state**. That is the one subcommand that fits: every other one
  assumes a single checkout driving a chain of branches, and our branches live
  in separate worktrees.

## The five real gaps

Each change below closes exactly one.

1. **A manager has no way to write a plan down.** `add_work_task` has no MCP
   tool. There is no tool named in the whole catalogue
   (`core/src/mcp.rs:163–803`) that puts a task on a board. So a manager asked
   to break work down has nowhere to put the breakdown, and the instruction in
   its preamble would be unfollowable — the same trap `claim_lease` fell into
   before `claim_worktree` named it.
2. **A task cannot say which files it owns.** `tasks` has `owner` and no path
   column, so "one owner per path" is a sentence in `docs/teamwork.md` with
   nothing behind it. Two engineers sharing a worktree collide and neither is
   told.
3. **The manager cannot choose where an engineer writes.** `open_work` always
   starts the session on the read-only checkout and leaves the claim to the
   engineer. There is no way to say *this one is exploratory*, *this one gets
   its own branch*, or *this one joins the worktree that other work already
   holds*.
4. **Every engineer already reports to main, whether or not the manager wants
   it to.** Cards cascade **upward through the whole ancestor chain** —
   `Store::cards_in` walks the subtree (`core/src/cards.rs:699`) — and migration
   `0024` deliberately hung managers under main so their cards would arrive. The
   side effect nobody chose is that an engineer's card arrives on main's rail
   too, three links up. Reljod's model says the manager is the only voice main
   hears about a project.
5. **Nothing stacks the pull requests.** Each worktree opens its own PR against
   the base branch. Three engineers on one job produce three PRs that all claim
   to change the same file from the same starting point, and whoever merges
   first breaks the other two.

## Decisions made here

Reljod's words settle most of these. Where they did not, the choice is recorded
with its reason so it can be reversed on purpose.

- **The manager writes the plan before it hands anything out.** One tool call,
  the whole breakdown at once, not a task at a time. A plan written in one call
  is a plan that can be checked for overlapping files before any of it is
  handed out; a plan accumulated task by task cannot be.
- **Path ownership is enforced, not advised.** `plan_work` refuses a plan whose
  tasks claim overlapping paths and names both sides. Prose in a preamble is not
  enforcement — the charter's own rule, applied here.
- **An engineer reports to its manager and to nobody above it.** A completion
  report is a delivery into the manager's conversation, not a card. Cards keep
  cascading; that machinery is right and is not touched. What changes is that
  the routine "I finished my task" no longer travels as a card.
- **`ask_question` and `request_secret` still cascade to main.** An engineer
  that is blocked on something only Reljod can answer must not be muffled by a
  manager that may not run again for an hour. The manager owns *reporting*, not
  *escalation*.
- **Writing straight into Reljod's checkout is the rarest case, and it is gated
  on facts rather than on judgement.** See D3 for the three conditions. A model
  deciding "this feels like a fresh project" is exactly how something writes
  into a repository somebody is editing.
- **A remote means a pull request.** Reljod tied those together and they stay
  tied: the same condition that forbids writing directly is the one that
  requires the PR.
- **Stacking is `gh stack link`, run by the manager, over PRs the engineers
  already opened.** Not `gh stack init`/`submit`, which want one checkout and
  local tracking state that separate worktrees cannot share.

---

# D1 — the board learns who owns which files

## D1.1 One migration

Next free number is **`0031`**, and the way that was established is worth
reading before anyone changes it.

`core/src/store.rs` on `origin/main` ends at `0028`, so `0029` looks free and is
not. The sibling session working in
`.claude/worktrees/spec-unblock-main-and-roles` has already written
`0029_a_scratch_conversation_tidies_itself_away` and
`0030_a_role_says_what_to_spawn_it_on` **in its own worktree, uncommitted**.
Neither `git log` nor `git fetch` shows them. Reading the sibling worktree is
the only thing that does.

So the rule the last two specs wrote down — do not derive the number by counting
— is necessary and was not sufficient. The version that holds: **read every
worktree's `store.rs`, not just the one you are standing in.** Append after
`0030` and renumber nothing.

```sql
ALTER TABLE tasks ADD COLUMN paths TEXT;
```

A JSON array of repository-relative path prefixes, or null. Null means the task
claims no files, which is the honest state for every task that already exists
and for every exploratory one.

`settings` needs no migration; it is key and value.

## D1.2 `TeamTask` carries it

`core/src/team.rs:127`. Add:

```rust
#[serde(default)]
pub paths: Vec<String>,
```

`serde(default)` for the reason the `created_at_ms` field above it has one: a
payload written by an older build must still deserialise, and it must
deserialise to "claims nothing" rather than to a panic.

## D1.3 Overlap is a fact, not an opinion

A free function in `core/src/works.rs`, so it is testable without a database:

```rust
pub fn overlapping(a: &[String], b: &[String]) -> Option<(String, String)>
```

Two path prefixes overlap when either is a prefix of the other **on a path
component boundary**. `core/src/store.rs` overlaps `core/src`; `core/src`
overlaps `core/src/store.rs`; `core/src` does **not** overlap `core/srcfile.rs`,
and getting that wrong by comparing raw strings is the whole reason this is its
own function with its own tests.

Normalise before comparing: trim, strip a leading `./`, strip a trailing `/`,
collapse internal `//` and `/./`, and reject an absolute path or one containing
`..` with a plain error naming the offending string. A task that owns
`/Users/reljod/...` owns nothing the next machine can check.

**Four spellings defeat a naive implementation, and an audit found all four
accepted after this shipped green.** They are the specification, not a footnote:

| one task claims | the other claims | why they are one place |
|---|---|---|
| `.` | `core/src` | `.` is the repository root |
| `Core/Src` | `core/src` | macOS's default filesystem is case-insensitive |
| `core//src` | `core/src` | an internal `//` is not collapsed by trimming |
| `core/./src` | `core/src` | nor is an internal `/./` |

- **`.` is the root, and the root covers everything.** Do not reject it — "this
  engineer owns the whole repository" is a reasonable thing for a manager to
  say. Normalise it to the empty prefix and let it collide with every other
  path, so `.` alone is a valid plan and `.` beside anything else is refused.
  Rejecting it outright would be safe; accepting it as colliding with nothing —
  which is what shipped — silently disables enforcement for the entire plan, and
  `.` is the most natural way for a model to write "everywhere".
- **Compare case-insensitively.** This over-refuses on Linux, where `Core/Src`
  and `core/src` really are two directories. Take that trade on purpose: a
  wrongly refused plan costs a manager one retry, a wrongly allowed one costs two
  engineers a merge conflict neither can see coming. `README.md` against
  `readme.md` is the same fault with no typo in it.

**Test this as a table of spellings, not as three examples.** The first
implementation asserted exactly the three examples written above and never probed
the space around them, which is why all four survived. Include the two that pass
by luck rather than design — `core/src/.` and `./` — so a later change to
normalisation cannot quietly break them.

## D1.4 Writing a plan

```rust
pub struct PlannedTask {
    pub title: String,
    pub paths: Vec<String>,
}

pub struct Plan {
    pub tasks: Vec<PlannedTask>,
}

impl Store {
    pub fn plan_work(&self, work_id: &str, plan: &Plan) -> Result<Vec<TeamTask>>;
}
```

- Refuses an empty plan, a task with an empty title, and any two tasks in the
  plan whose paths overlap — naming both titles and the two paths. The refusal
  is the feature.
- Checks the new tasks against the **open** tasks already on the board too, not
  only against each other. A second `plan_work` on a work that is half done must
  not hand out a file somebody is holding.
- Writes every task in one transaction. Half a plan is worse than none: the
  manager would believe it handed out five tasks and two engineers would be idle
  with no work and no error.
- Returns the board as written, so the caller does not have to read it back.

`Store::assign_work_task(task_id, owner)` sets `tasks.owner`. It already has a
column and no writer outside `handoff`.

---

# D2 — the manager can write a plan and read a board

Three tools in `core/src/mcp.rs`. Follow the shape of the ones already there:
name, description written for the model, `needs`, `schema`.

## D2.1 `plan_work`

```
plan_work {
  work_id: string,
  tasks: [ { title: string, paths: [string] } ]
}
```

`ToolAccess::Delegate`. It writes to a board and the board decides when a work
closes.

The description has to carry the rule, because this is where the manager reads
it: one task per engineer, each task naming the files only that engineer will
touch, and the call is refused if two tasks claim the same file. Say that
refusal out loud in the description — a manager that learns the constraint from
an error has already spent a turn.

## D2.2 `work_board`

```
work_board { work_id }
```

`ToolAccess::ReadOnly`. Every task with its id, title, owner, status and paths.
This is what the manager reads to decide whether the job is finished, and it is
the answer to "is it done yet" that does not require asking an engineer.

## D2.3 `complete_task`

```
complete_task { task_id: string, report: string }
```

`ToolAccess::ReadOnly` — it writes to Jod's own database and starts no agent,
which is the line `record_decision` sits on. An engineer must be able to call it
at any access level it can be spawned with, because an engineer that cannot
report is an engineer whose work is invisible.

What it does, in order:

1. Marks the task done through `Store::complete_work_task`, which already closes
   the work when it was the last one and returns a `Closing`.
2. Delivers `report` into the **manager's conversation** — the conversation
   found by walking `parent_conversation_id` up from the caller to the first one
   that is a `projects.manager_conversation_id`. This starts the manager's next
   turn, the same way a delegated run's `send_message` to `main` starts main's.
3. Tells the caller plainly whether it was the last task, so the engineer's last
   line is not a guess about whether anybody else is still working.

**When no manager is above the caller** — an engineer started by main directly,
or by a test — the report goes to the parent conversation, whatever it is. Never
nowhere. A report with no addressee is the failure mode this whole change
exists to remove.

---

# D3 — the manager decides where the work happens

## D3.1 Four placements

In `core/src/leases.rs`, beside `Claim`:

```rust
pub enum Placement {
    /// Read-only. No branch, no worktree, no pull request.
    Explore,
    /// A branch and worktree of this engineer's own.
    Worktree,
    /// Join the worktree another work already holds on this repository.
    Share { work_id: String },
    /// Write in Reljod's real checkout. Gated — see `direct_is_allowed`.
    Direct,
}
```

`Explore` is the one the ask names that has no representation today: an engineer
sent to read, search or review, which must never cut a branch. It is also the
default for a work opened with no placement, because reading is the reversible
one.

## D3.2 `Share` crosses works, and that is the change

`claim_lease` already shares within one work. `Placement::Share { work_id }`
lets a manager put a second engineer, on a **different** work, into the worktree
the first one holds — which is what "add instruction to existing agent worktree
or create a new session/engineer and use same/existing worktree" asks for.

```rust
pub fn share_lease(
    &self,
    work_id: &str,
    conversation_id: &str,
    lender_work_id: &str,
    repo_path: &Path,
) -> Result<Claim>
```

- Finds the lender's held lease on that repository. No held lease is a plain
  refusal naming the lender, never a silent fall back to cutting a new one.
  Falling back would give two engineers separate branches while the manager
  believed they were sharing, and the plan's path ownership would be protecting
  nothing.
- Binds the borrower's roots to the same worktree, writable, with the real
  checkout beside it read-only — `bind_lease_roots` already does exactly this.
- **Does not** create a second `leases` row. One worktree is one lease; the
  partial unique index is on `(work_id, repo_path)` and a second row for the
  same directory would make `release_worktree` remove a tree somebody else is
  standing in. Record the borrower in a new table instead:

```sql
CREATE TABLE lease_sharers (
  lease_id        INTEGER NOT NULL REFERENCES leases(id) ON DELETE CASCADE,
  conversation_id TEXT    NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  work_id         TEXT,
  shared_at_ms    INTEGER NOT NULL,
  PRIMARY KEY (lease_id, conversation_id)
);
```

  That is a second statement in migration `0029`.

- `release_lease` refuses while a sharer other than the releaser is still
  attached, and says who. The existing refusal already prints a reason; this is
  one more reason, in the same voice.

## D3.3 `Direct` is gated on three facts

```rust
pub struct DirectVerdict {
    pub allowed: bool,
    /// Every condition that failed, in the words the refusal prints.
    pub because: Vec<String>,
}

pub fn direct_is_allowed(store: &Store, project_id: &str, repo: &Path)
    -> Result<DirectVerdict>
```

Allowed only when **all three** hold:

1. **No git remote.** `git remote` in the checkout prints nothing. Reljod tied
   the remote to the pull request rule, so the same fact decides both: a
   repository with a remote gets a branch and a PR, always.
2. **No other work on this project.** `SELECT COUNT(*) FROM works WHERE
   project_id = ?` counting rows other than this one is zero. This is "the first
   iteration", read from the database rather than judged.
3. **The checkout is clean.** `git status --porcelain` is empty. Writing into a
   tree Reljod has uncommitted changes in is the accident the whole lease system
   was built to prevent, and a fresh project is not an exemption from it.

Every failing condition is reported, not just the first. A manager told only
"there is a remote" fixes that and gets told about the dirty tree on the next
turn, having spent two turns to learn two facts that were both true at once.

**This is a refusal at the tool boundary, not advice in a preamble.** `open_work`
called with `placement: "direct"` on a project that fails any condition is
refused, the failures are named, and `worktree` is named as what to call
instead.

## D3.4 `open_work` takes a placement

Add to the schema in `core/src/mcp.rs:666`:

```
"placement": one_of(
    "Where this engineer works. explore = read-only, no branch, for a look or a \
     review. worktree = its own branch and worktree, for anything that writes. \
     share = join the worktree another work already holds, named in `share_with`. \
     direct = write in Reljod's checkout, allowed only on a fresh project with no \
     remote and nothing uncommitted. Default explore.",
    &PLACEMENT_IDS),
"share_with": text("The work id whose worktree to join. Required when placement is share."),
"paths": array of text("The files this engineer owns, and the only ones it may change."),
```

- `explore` spawns the session with **no writable root** and says so in its
  brief. It keeps `claim_worktree` in its toolbox only if its access level would
  have given it one; the brief tells it plainly that this session was opened to
  look, and that needing to write means saying so and stopping.
- `worktree` claims the lease **at spawn** rather than leaving it to the
  engineer. When the manager has already decided this engineer writes, making it
  discover that for itself costs a turn and sometimes gets skipped.
- `share` calls `share_lease`.
- `direct` runs `direct_is_allowed` and refuses with every reason when it fails.
- `paths` is recorded on the work's first task, so the engineer's brief can name
  the files it owns.
- **The engineer's conversation records the task it was spawned onto**, in the
  new `conversations.task_id`. This is the write side of the column D5.2 reads,
  and without it every stack silently falls back to finish order. A column that
  is only ever read is the failure this bullet exists to prevent — the same
  shape as `claim_lease` having no caller until `claim_worktree` named it.
  `open_work` writes it when the placement carries a task; `continue_agent`
  leaves it alone, because continuing an engineer does not move it to a new task.

**Correction — `Explore` is not the default, and saying it was is wrong.** This
spec originally said `Placement::Explore` was the default and that this "keeps
every existing call site behaving exactly as it does today". That is false, and
the engineer building it caught it.

Today's brief tells a session to call `claim_worktree` when it needs to write.
D4.2 says an `explore` session must never be told that. Both cannot hold if an
absent placement renders as `Explore` — check 13's byte-identical `SpawnRequest`
fails, and so does the existing
`a_worker_is_told_which_roots_it_may_write_to_and_that_it_must_claim_first`.

So the field is `Option<Placement>` and the two states are genuinely different:

- **`None` — nobody placed this session.** The checkout arrives read-only,
  nothing is cut, and the session claims a worktree itself when it needs one.
  Today's text, byte for byte. This is what `continue_agent`, `delegate` and
  every unplanned `open_work` pass, and those really are unplaced rather than
  "sent to look".
- **`Some(Explore)` — a manager decided this engineer must not write.** The brief
  states that as a prohibition and never names the verb.

Collapsing the two would have made the brief lie to most of the sessions in the
fleet, which is a bigger fault than the one the default was trying to avoid.
Held by `an_unplaced_session_is_told_to_claim_and_a_placed_one_is_not`.

`None` as the default is what lets this land without touching the tests that
already pass.

**Where each placement is acted on**, because it is split on purpose.
`prepare_work` honours `Worktree`, `Share` and `Direct` — it has to, because
`claim_lease` and `share_lease` bind roots to a conversation that does not exist
until `prepare_work` creates it, so there is no earlier seam. But
`direct_is_allowed` stays at the **tool boundary**, since that is the only place
the refusal can name every failing condition at once and point at `worktree`.

---

# D4 — the engineer's brief shrinks, and the manager's grows

## D4.1 The engineer is told it is an engineer

`Brief` in `core/src/orchestrator.rs:498` gains two fields:

```rust
/// The task this session exists to do, and the files it owns.
pub assignment: Option<Assignment>,
/// How the manager placed it.
pub placement: Placement,
```

```rust
pub struct Assignment {
    pub task_id: String,
    pub title: String,
    pub paths: Vec<String>,
    /// What to call to report. Always `complete_task`.
    pub manager: String,
}
```

`preamble_lines` gains a section, and it must be tagged `PreambleLine::shared`
so the existing test that holds the body identical across harnesses keeps
passing. What it says:

- **You have one task.** Its title, and the files you own. Nothing else in this
  repository is yours to change, even when you can see it needs changing —
  somebody else may hold it right now, and a change outside your paths is a
  merge conflict with a colleague you cannot see.
- **Something outside your paths needs changing?** Say so in your report and
  stop. Do not do it, and do not ask an engineer to do it. Your manager is the
  one who can widen the plan; you are not.
- **One carve-out, and it is narrow: mechanical fallout from your own change.**
  Adding a field to a shared struct breaks every literal that constructs it,
  often in files nobody planned for. Fixing those is allowed and expected — you
  caused them, they are not judgement calls, and leaving the tree uncompilable
  blocks every other engineer on the job. It stops at the mechanical: adding the
  field, updating a signature's callers, fixing an import. The moment a fix
  requires deciding what the right *value* or the right *behaviour* is, it is a
  change, it is outside your paths, and it goes in your report instead.

  **This was found by running the spec, not by reading it.** The first engineer
  to add a field to `TeamTask` had to touch two `cli/src/tui/` files nobody
  owned, because their test literals stopped compiling. A rule with no carve-out
  would have told it to stop and report with the workspace broken, which is
  worse for everyone than the twelve lines it actually wrote. `plan_work` cannot
  see this coming either — the paths it refuses on are the ones the manager
  named, and the fallout of a struct change is not knowable at plan time.
- **Report with `complete_task` when you are done, and only then.** Your report
  reaches your manager and nobody above it. Reljod is not reading this
  transcript and does not see your prose — the report is the whole of what he
  will be told you did.
- **You are still blocked the ordinary way.** `ask_question` and
  `request_secret` still go to Reljod, because a manager that is not running
  cannot answer them. Blocked is still a successful ending.

When `assignment` is `None` — every existing caller — the section is absent and
the preamble is byte-identical to today's. Hold that with a test.

## D4.2 Placement shows up in `roots_lines`

`roots_lines` already prints each root and whether it is writable, then the
paragraph telling the session to call `claim_worktree`. Split that paragraph by
placement:

- `Explore` — say that this session was opened to read and holds no writable
  root by design. Needing to write is a thing to report, not a thing to fix with
  `claim_worktree`.
- `Worktree` — the worktree is already claimed and already writable. Do not call
  `claim_worktree`; you have one.
- `Share` — the worktree is writable **and somebody else is in it**. Name the
  other engineer's paths. Read before you write, and never rebase or reset a
  branch you are sharing.
- `Direct` — you are in Reljod's real checkout. There is no branch between you
  and his working tree. Commit nothing he did not ask for.

## D4.3 The manager's preamble

`manager_preamble` (`core/src/orchestrator.rs:401`) keeps everything it says
about `list_agents`, free engineers and stalled ones. That reasoning is sound and
this spec does not reopen it. Four things are added, and one is removed.

**Added — plan before you delegate.** An instruction that is one task for one
engineer is still one task: call `plan_work` with a single task and hand it out.
An instruction that splits is where the manager earns its keep. Write the whole
breakdown in one `plan_work` call, give every task the files it owns, and expect
the call to refuse if two of them collide.

**Added — decide the placement for each engineer, and say why.** The four
placements, the rule that `explore` is right for anything that only reads, and
the fact that `direct` is gated on three conditions the manager does not get to
overrule.

**Added — you are the one who tells main.** Read `work_board`. While any task is
open the job is not finished, and a card saying it is would be false. When the
board is empty, raise one card with what the whole job produced, not a relay of
each engineer's report.

**Added — stacking.** D5.

**Removed — nothing.** The `list_agents`-first rule stays exactly as it is,
because it is upstream of all of this: the manager still decides who is free
before it decides what to give them.

## D4.4 An engineer's routine report stops cascading

The narrow change: `complete_task` delivers into the manager's conversation
rather than raising a card. `record_decision`, `ask_question` and
`request_secret` are untouched and keep cascading to main.

This is the whole of gap 4, and it is deliberately small. The alternative —
teaching `Store::cards_in` to stop at a manager — would silence a blocked
engineer's question, which is the one thing that must always get through.

---

# D5 — a pull request per worktree, stacked

## D5.1 When a PR is opened at all

`prs::auto_pr_instruction` already writes the instruction. It gains the two
facts it is missing:

```rust
pub fn auto_pr_instruction(branch: &str, base: &str, stacked_on: Option<&str>) -> String
```

- `stacked_on: None` — today's text, unchanged.
- `stacked_on: Some(parent_branch)` — the same text, with the base being the
  other engineer's branch rather than `main`, and a sentence saying so: this
  pull request sits on top of another one that is not merged yet, so its diff is
  only the part this engineer added.

A placement of `Explore` opens no pull request at all. There is no branch, so
there is nothing to open one from, and the existing auto-PR poller already only
looks at held leases (`leases_to_ask`) — so this falls out for free rather than
needing a guard. Say so in a test rather than trusting it.

## D5.2 The manager links the stack

A new tool, because the manager needs a verb:

```
stack_pull_requests { work_id }
```

`ToolAccess::Delegate` — it pushes branches and writes to GitHub.

What it returns is **an instruction and the ordered list**, not a `gh` call Jod
makes itself. The same reasoning `auto_pr_instruction` is built on: the session
has the context and the skill, and Jod's process never saw the work happen.

The tool's job is the part a model should not be guessing at:

1. Read the work's pull requests (`Store::work_pull_requests`, already there).
2. Order them by the dependency the plan implies — the order the tasks were
   written in `plan_work`, which is the manager's own stated order and the only
   ordering anybody has a reason to trust.

   **This needs a column that did not exist, and the reason is worth keeping.**
   Nothing in the schema joined a pull request back to its task: `pull_requests`
   carries `work_id`, `conversation_id` and `lease_id`; `tasks.owner` is a
   free-text agent name, never a conversation id; and neither `conversations`
   nor `leases` carried a task. The tempting fallback — order by
   `detected_at_ms` — is **finish order, not plan order**, so an engineer who
   finishes task 3 first lands at the bottom of the stack and every base beneath
   it is wrong. That is a broken stack that looks fine.

   So `0031` adds `conversations.task_id`. Rank a pull request by its opener's
   task position in `tasks` ordered by `created_at_ms, id` — the ordering
   `work_tasks` already uses and the one `plan_work` writes in. Fall back to
   `detected_at_ms` when `task_id` is null, which is every pull request that
   exists today.

   `conversations.task_id` and not `leases.task_id`, because `Placement::Share`
   puts two engineers on one lease, so a lease maps to many tasks while one
   engineer conversation maps to exactly one. Where a lease is shared, rank its
   single pull request by the **earliest** task among its sharers.
3. Return the `gh stack link` command with the PR numbers in that order, bottom
   to top, and the warning that goes with it: linking rewrites each PR's base
   branch, so a PR whose branch is already merged must be left out.

Refuse when the work has fewer than two pull requests, naming how many it found.
A stack of one is a pull request, and running `gh stack link` on it churns a base
branch for nothing.

## D5.3 What is deliberately not built

Jod does not run `gh stack merge`, does not rebase a stack, and does not
reorder one. Merging is `merge_pr.sh` and a person — the charter's rule, and a
stack does not exempt anyone from it.

---

# D6 — the manager plans for parallelism, within a budget

Breaking work down is only worth doing if the pieces can actually run at the
same time, and running them at the same time is only safe up to a number. D6 is
both halves.

## D6.1 How many engineers a project may have at once

A setting, not a role row. The sibling spec's `roles` table
(`role, harness, model, thinking, permission`) says **what a layer is spawned
on** — one row per role, describing a single spawn. How many of a role may exist
at once is a different kind of fact and does not fit a column there.

It goes where that spec's own two knobs go, in `settings`, which is key and value
and needs no migration:

```
max_engineers_per_project   default 3
```

```rust
impl Store {
    pub fn max_engineers_per_project(&self) -> Result<usize>;
    pub fn set_max_engineers_per_project(&self, n: usize) -> Result<()>;
}
```

Follow `Store::auto_pr` / `Store::set_auto_pr` (`core/src/prs.rs:1077`) exactly —
that is the established shape for a settings-backed knob, and a second shape for
the same job is how two of them drift.

- **Default 3.** Each engineer is a whole harness process with its own checkout
  of a Rust repository. Three is what a laptop runs without the build cache
  thrashing, and it is the number a manager can still describe in one sentence.
- **`0` means no cap**, matching how the sibling spec reads `0` in
  `scratch_retention_days` and `scratch_reuse_window_minutes`: the escape hatch
  is spelled the same way in all three.

## D6.2 Where the cap is enforced, and what it deliberately does not cover

At the tool boundary in `open_work`, because that is the call that creates an
engineer. When the project already holds `max_engineers_per_project` live
engineer sessions, `open_work` is refused, and the refusal says how many are
running and names them.

**`continue_agent` is not capped, and that is the point.** Reusing a free
engineer adds no process, so the way around the cap is exactly the behaviour
`manager_preamble` already asks for first. The limit and the existing
"reuse before you spawn" rule push the same direction rather than fighting.

A **stalled** engineer counts against the cap — it is still a live process
holding a worktree — but the refusal names it separately and says it can be
stopped. A manager that hits the cap because of a wedged session must be able to
see that from the refusal alone. Reporting only the number would leave it stuck
with no path forward, which is worse than no cap at all.

## D6.3 The manager is told to look for parallelism, and told when not to

`manager_preamble` gains this, and it is the part that makes the manager a
manager rather than a dispatcher. It takes the cap as an argument —
`manager_preamble(project, max_engineers)` — because a preamble that says "a few"
is one every manager interprets differently.

What it says:

- **Ask first whether this splits at all.** Most instructions are one task for
  one engineer, and the answer is a one-task plan. Splitting something indivisible
  costs a cold session and buys nothing.
- **Two tasks can run at once when neither needs the other's output and they
  touch no file in common.** Both halves are required. Two engineers on disjoint
  files whose work is sequential still have to be sequenced, and two engineers on
  the same file are refused by `plan_work` before they start.
- **Where one task's output is another's input, write them in that order and
  hand out only the first.** The plan is the whole breakdown; handing it out is
  separate and is paced by what has finished. This is why `plan_work` takes the
  whole plan at once and `open_work` is one engineer at a time.
- **You may run up to `max_engineers` at once on this project.** Count what is
  already live before you plan, not after — `list_agents` scoped to the project
  is the same first call the preamble already demands, so this costs nothing new.
- **Under the cap is not a reason to reach it.** Two engineers who each spend
  their first turn reading the same three files have cost more than one engineer
  reading them once. Split when the pieces are genuinely independent and each is
  worth a session, not to use the budget up.

**One thing that falls out and should be said out loud in the preamble:** the
order the manager writes tasks in is the order `stack_pull_requests` uses to
stack the pull requests (D5.2). Writing the plan in dependency order is
therefore not bookkeeping — it is what makes the stack come out right. A manager
that writes the plan in the order it happened to think of things gets a stack
whose bases are wrong.

## Migrations

One, `0031_a_task_owns_its_files`, in `core/src/store.rs`. Two statements:

```sql
ALTER TABLE tasks ADD COLUMN paths TEXT;

CREATE TABLE lease_sharers (
  lease_id        INTEGER NOT NULL REFERENCES leases(id) ON DELETE CASCADE,
  conversation_id TEXT    NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  work_id         TEXT,
  shared_at_ms    INTEGER NOT NULL,
  PRIMARY KEY (lease_id, conversation_id)
);

ALTER TABLE conversations ADD COLUMN task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL;
```

Existing tasks get null paths and existing conversations get a null task, both
of which are true: they claim no files and they were not spawned onto a task.

The third statement is what makes D5.2's ordering real rather than a guess at
finish order — see D5.2 for why the obvious fallback is wrong.

**Take the next free number by reading every worktree, not just this one.** See
D1.1: `origin/main` ends at `0028`, the sibling worktree holds an uncommitted
`0029` and `0030`, and nothing in git shows them. This is `0031`.

## Files this touches, and who owns what

One owner per path. This is the same rule the spec is about, applied to the spec.

| Owner | Files | What changes |
|---|---|---|
| **A — the board** | `core/src/works.rs`, `core/src/team.rs`, the `0031` migration in `core/src/store.rs` | `paths` on the board, `overlapping`, `Plan`, `plan_work`, `assign_work_task`, `max_engineers_per_project` |
| **B — placement** | `core/src/leases.rs` | `Placement`, `share_lease`, `lease_sharers`, `direct_is_allowed`, the release refusal |
| **C — stacking** | `core/src/prs.rs` | `auto_pr_instruction`'s third argument, the ordered stack list |
| **D — the tools** | `core/src/mcp.rs` | `plan_work`, `work_board`, `complete_task`, `stack_pull_requests`, `open_work`'s new arguments |
| **E — the briefs** | `core/src/orchestrator.rs` | `Assignment`, the engineer section, `roots_lines` by placement, `manager_preamble` |

D depends on A, B and C. E depends on B. A, B and C are independent of each
other and go first.

## Checks

`cargo test -p jod-core`. Every one of these must run and pass.

**D1 — the board**

1. `overlapping` says `core/src` and `core/src/store.rs` overlap, in both
   argument orders.
2. `overlapping` says `core/src` and `core/srcfile.rs` do **not** overlap. This
   is the component-boundary guard and it is the one a string prefix gets wrong.
3. `plan_work` with two tasks claiming the same path is refused, and the refusal
   names both titles.
4. `plan_work` with two tasks claiming disjoint paths writes both, and
   `work_tasks` returns them in the order written.
5. A refused `plan_work` writes **no** tasks. The transaction guard.
6. `plan_work` is refused when a new task's path overlaps an **open** task
   already on the board, and allowed when it overlaps only a `done` one.
7. A `TeamTask` payload written without `paths` deserialises with an empty list.

**D2 — the tools**

8. `plan_work` appears in the tool catalogue at `ToolAccess::Delegate` and
   `work_board` at `ToolAccess::ReadOnly`.
9. `complete_task` marks the task done and reports `last: true` only when it was
   the last open one.
10. `complete_task` from an engineer under a manager delivers the report into the
    manager's conversation and raises **no** card.
11. `complete_task` from a session with no manager above it delivers to its
    parent conversation rather than failing.
12. The existing test that every tool the preamble names exists still passes —
    `every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists`. The new
    preamble text names `plan_work`, `work_board`, `complete_task` and
    `stack_pull_requests`, and this test is what proves all four are real.

**D3 — placement**

13. `open_work` with no `placement` produces a `SpawnRequest` byte-identical to
    today's. The regression guard on the default.
14. `placement: "worktree"` claims a lease before the session starts, and the
    session's roots include it as writable.
15. `placement: "explore"` claims nothing, and the session has no writable root.
16. `share_lease` binds the borrower to the lender's worktree and writes one
    `lease_sharers` row, leaving `leases` with exactly one row for that
    directory.
17. `share_lease` against a work holding no lease is refused and names the work.
    It does **not** cut a new worktree.
18. `release_lease` on a lease with another sharer attached is refused and names
    the sharer; releasing the last sharer lets it through.
19. `direct_is_allowed` returns false with **all three** reasons for a project
    that has a remote, a sibling work and a dirty tree — not just the first.
20. `direct_is_allowed` returns true for a repository with no remote, no other
    work and a clean tree.
21. `open_work` with `placement: "direct"` on a project that fails any condition
    is refused, and the refusal names `worktree`.

**D4 — the briefs**

22. A `Brief` with `assignment: None` renders a preamble byte-identical to
    today's.
23. A `Brief` with an assignment names the task title, every path it owns, and
    `complete_task`.
24. The engineer section is `PreambleLine::shared`, so
    `the_body_of_the_preamble_is_identical_on_every_harness` still passes.
25. `roots_lines` under `Placement::Explore` does not tell the session to call
    `claim_worktree`; under `Placement::Worktree` it says one is already held.
26. `manager_preamble` names `plan_work`, `work_board` and
    `stack_pull_requests`, and still names `list_agents` first.

**D5 — stacking**

27. `auto_pr_instruction` with `stacked_on: None` is byte-identical to today's
    output.
28. `auto_pr_instruction` with `stacked_on: Some("jod/first")` bases the PR on
    that branch and says the diff is only this engineer's part.
29. `stack_pull_requests` on a work with one PR is refused and says it found one.
30. Three tasks are planned, a conversation is attached to each, and their pull
    requests are opened **out of plan order** — task 3's first, task 1's last.
    `stack_for_work` returns them in *plan* order regardless.

    Opening them out of order is the whole test. Opening them in order passes
    against a timestamp sort too, so it would prove the ordering exists without
    proving it comes from the plan, which is the thing that matters.
30b. A pull request whose conversation has a null `task_id` still comes back in
    `detected_at_ms` order rather than an arbitrary one. That is the state of
    every pull request already in a database on this box.
30c. **The board under test is built by calling `plan_work`, not by inserting
    tasks with hand-picked timestamps.**

    This is not a style preference. `plan_work` calls `now_ms()` once, outside
    its insert loop, and writes the whole plan in one transaction — so every
    task in a plan carries an *identical* `created_at_ms` and the tiebreaker is
    the only thing ordering them. A test that stamps distinct timestamps by hand
    exercises a state production cannot produce, passes, and leaves the state
    production always produces untested.

    Both orderings therefore tie-break on **`rowid`, never on `id`**.
    `tasks.id` is a uuid, so a uuid tiebreaker over one shared millisecond is a
    random shuffle. `Store::work_tasks` and the stack's window function must use
    the same expression, because the design rests on them agreeing.

    Found by one engineer reading another's query, after both were green. Worth
    remembering: this feature has now produced the same class of fault twice —
    an ordering that looks right, is wrong, and whose tests agree with it.

**D6 — parallelism and the cap**

32. `max_engineers_per_project` returns 3 when the key is absent, and the value
    when it is set. The absent case is the one every existing machine is in.
33. `open_work` is refused when the project already holds the cap in live
    engineers, and the refusal states the count.
34. The refusal names a **stalled** engineer separately as one that can be
    stopped. Without this a manager at the cap because of a wedged session has
    no way to read a path forward out of the error.
35. `continue_agent` succeeds at the cap. This is the regression guard on the
    rule that reuse is the way around the limit.
36. `max_engineers_per_project = 0` refuses nothing, however many are running.
37. `manager_preamble` states the cap as a number, and names both conditions for
    running two engineers at once — no shared files, and neither waiting on the
    other's output.
38. `manager_preamble` says the order tasks are planned in is the order the pull
    requests stack in.

**End to end, on this box**

31. Open a work with two tasks on disjoint paths, place the second engineer with
    `share`, and assert both sessions have the same writable root and that
    `git worktree list` shows one worktree, not two.

## Out of scope

Say so rather than doing them.

- **Main's preamble, `ask_assistant`, the scratch lane and the roles panel.**
  That is the sibling spec. This one starts at `ask_manager`.
- **Teaching `cards_in` to stop cascading at a manager.** It would silence a
  blocked engineer's question, which is the one message that must always reach
  Reljod. D4.4 solves the reporting problem without touching the cascade.
- **Jod running `gh stack merge`, `rebase` or `modify`.** Merging is
  `merge_pr.sh` and a person.
- **Automatic conflict detection between engineers.** Path ownership is declared
  and enforced at plan time. Watching the filesystem for a write outside an
  engineer's paths is a different mechanism — the read-only root watcher already
  exists and is the place it would belong, not here.
- **Backfilling `paths` onto existing tasks.** They claim nothing, which is true.
- **A manager for `domains/`.** Managers stay per project, as
  `docs/spec-ceo-and-managers.md` decided.
</content>
</invoke>
