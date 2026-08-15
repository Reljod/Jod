# TASKS — index

Open work found by testing the orchestrator end to end: launching real consoles,
sending real instructions, firing real schedules and goals, and reading the
result. **This file is an index only.** Every finding lives in the per-area file
below, so several people can file at once without touching the same file.

One owner per task. Claim one by putting your name in its `Owner:` line before
you start. Add findings to the area file, never to this index.

| Area | File | Findings | Owner |
|---|---|---|---|
| Launch, roots and the console's directory | [`tasks/00-launch-and-roots.md`](tasks/00-launch-and-roots.md) | L1–L6 | — |
| Routing: answer it yourself or hand it over | [`tasks/01-routing.md`](tasks/01-routing.md) | R1–R6 | — |
| The TUI itself | [`tasks/02-tui.md`](tasks/02-tui.md) | T1 | — |
| Orchestration scenarios | [`tasks/10-orchestration.md`](tasks/10-orchestration.md) | O1–O10, **one critical** | — |
| Orchestration edge cases and bad input | [`tasks/11-orchestration-edge-cases.md`](tasks/11-orchestration-edge-cases.md) | E1 + 14 scenarios | — |
| Fleets and the tree | [`tasks/20-fleets.md`](tasks/20-fleets.md) | F1–F8 | — |
| Project managers | [`tasks/30-project-managers.md`](tasks/30-project-managers.md) | P-series + spec tasks | — |
| Scheduling | [`tasks/40-scheduling.md`](tasks/40-scheduling.md) | S1–S6 | — |
| Goals | [`tasks/50-goals.md`](tasks/50-goals.md) | G1–G12 | — |

Every file ends with a "Scenarios run" table listing what was tried, what was
expected, and what happened — passes included. A clean pass is worth recording;
it is what stops the same ground being covered twice.

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

`docs/spec-ceo-and-managers` holds a `SPEC.md` designing most of what Reljod
asked for: main behaves like a CEO, every project gets a manager conversation,
main loses `open_work` and gains `ask_manager`, and a stalled session is marked
rather than killed.

Pull request #120 reviewed that spec claim by claim against `origin/main` and
found it executable, with four corrections. Two of them matter to anyone working
here:

- **A manager must not use `pinned = 1`.** `Store::pinned_conversation`
  (`core/src/orchestrator.rs:1312`) is a `query_row` with no `LIMIT` and no
  ordering, so a second pinned row makes "which conversation is main" depend on
  SQLite's row order — and Reljod's instructions would start landing in a
  manager's transcript.
- **Routing to a manager is already deterministic.** `settle_project` runs on
  the raw instruction before the model turn
  (`core/src/orchestrator.rs:875`), so `ask_manager` is wiring, not reasoning.

So most of the manager work is "execute the spec", not "work out what to do".
