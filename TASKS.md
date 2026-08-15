# TASKS — index

Open work found by testing the orchestrator end to end. **This file is an index
only.** Every finding lives in the per-area file below, so several agents can
file findings at once without touching the same file.

One owner per task. Claim one by putting your name in its `Owner:` line before
you start. Add findings to your area's file, never to this index.

| Area | File | Owner |
|---|---|---|
| Launch, roots and the TUI's working directory | [`tasks/00-launch-and-roots.md`](tasks/00-launch-and-roots.md) | — |
| Routing: answer it yourself or hand it over | [`tasks/01-routing.md`](tasks/01-routing.md) | — |
| Orchestration scenarios | [`tasks/10-orchestration.md`](tasks/10-orchestration.md) | — |
| Fleets and the tree | [`tasks/20-fleets.md`](tasks/20-fleets.md) | — |
| Project managers | [`tasks/30-project-managers.md`](tasks/30-project-managers.md) | — |
| Scheduling | [`tasks/40-scheduling.md`](tasks/40-scheduling.md) | — |
| Goals | [`tasks/50-goals.md`](tasks/50-goals.md) | — |

## How to read a task

Each one names the file and line the behaviour lives at, what was actually
observed, and the check that should go green once it is fixed. A task with no
observed behaviour behind it is a guess and should say so.

## The one piece of context worth reading first

There is an unmerged branch, `docs/spec-ceo-and-managers`, holding a 350-line
`SPEC.md` that already designs most of what Reljod asked for: main behaves like
a CEO, every project gets a manager conversation, main loses `open_work` and
gains `ask_manager`, and a stalled session is marked rather than killed.

**Read it before designing any of this again.** Several tasks below are
"execute the spec", not "work out what to do".

    git show docs/spec-ceo-and-managers:SPEC.md
