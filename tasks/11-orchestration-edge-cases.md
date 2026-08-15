# Orchestration — edge cases and bad input

Written from my own testing rather than by the orchestration discovery agent,
which had not delivered when this was pushed. It deliberately does **not** use
the filename `10-orchestration.md`, so that agent's file can land beside this
one without either overwriting the other. If both exist, read both; they cover
different ground.

How this was tested: the binary built from this branch, driven from the CLI
against a throwaway `JOD_HOME`
(`/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-orch2`) from inside a scratch
git repository. Everything below was run. The two scenarios that need a live
model are marked `needs confirming` and were skipped on purpose — the box was
at load 8 with no free memory, and the honest answer is that they were not run.

The routing findings themselves — the orchestrator refusing to answer anything,
busy-waiting on its own child, reaching outside Jod's tool set — are in
[`01-routing.md`](01-routing.md) as R1 and R4 to R6. This file is the edge-case
sweep around them.

---

## E1. `jod run` starts in `$HOME` too, not just `jod main`
Status: open · Owner: — · Severity: medium · **folded into
[`00-launch-and-roots.md`](00-launch-and-roots.md) L3**

Filed here because this is where it was found. Running
`jod run --detach -n probe "hi"` from inside a scratch repository produced a
conversation with `cwd = /home/reljod` and no roots at all.

That widened L3 from "`jod main` gets this wrong" to "every entry point except
the TUI gets this wrong". `console_cwd` — the function that means "here" — has
exactly one caller in the whole CLI, and it is the TUI. See L3 for the full list
of the seven sites and the fix.

Do not fix this separately; it is the same change.

---

## Everything that behaved well

Worth recording, because these are the cases most likely to be broken and none
of them were. Nobody should spend budget re-testing them.

- Bad input into the main chat is handled gracefully. An empty instruction, a
  whitespace-only instruction, and reading the chat before anything has been
  said all produce the same clear line — "the main chat is empty —
  `jod main "<instruction>"` starts it" — rather than an error or an empty
  spawn. Notably, a whitespace-only instruction does **not** cost a model turn.
- Unknown ids fail cleanly and identically. `jod kill no-such-run` and
  `jod watch no-such-run` both answer "no agent with id `no-such-run`" and exit
  non-zero, with no stack trace and no partial state written.
- `jod run --continue` with no previous conversation to continue starts a fresh
  run rather than failing. That is the right call — the flag is a preference,
  not an assertion — and it is the behaviour `Resume::Last` documents.
- A `delegate`d run creates no work, correctly. `works` stayed empty while the
  run existed, which is the documented design: a delegated run belongs to no
  work and is deliberately not a node in the tree.
- Cost and token accounting are reported and are plausible: `1957 out · $0.3864
  · 42s` for the turn described in R1.

---

## Scenarios run

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 1 | Empty instruction | refused or ignored, no spawn | clear empty-chat line, no spawn | pass |
| 2 | Whitespace-only instruction | same | same, and no model turn paid for | pass |
| 3 | Read the chat before anything was said | clear empty state | clear empty state | pass |
| 4 | `jod kill` an unknown id | clean error, non-zero exit | "no agent with id …" | pass |
| 5 | `jod watch` an unknown id | clean error | same message, consistent with kill | pass |
| 6 | `jod run --continue` with nothing to continue | start fresh, do not fail | started fresh | pass |
| 7 | `jod run` from inside a repository | conversation cwd is that repository | `$HOME`, and no roots | **fail — E1 / L3** |
| 8 | A trivial factual question | answered directly | agent spawned, no answer, $0.39 | **fail — R1** |
| 9 | That turn's tool calls | hand over and return | `sleep 45` plus a poll loop | **fail — R4** |
| 10 | That turn's tool set | Jod's verbs only | also `ToolSearch select:Monitor` | **fail — R5** |
| 11 | A `delegate`d run creates no work | no work row | none, correct by design | pass |
| 12 | Cost and token accounting | reported | reported and plausible | pass |
| 13 | An instruction that says *when* (schedule-shaped) | `schedule_create` | not run — box under load | **needs confirming** |
| 14 | An instruction that says *keep/until* (goal-shaped) | `goal_create` | not run — box under load | **needs confirming** |

Scenarios 13 and 14 are the two most valuable remaining ones, because they are
the routing verbs nothing has yet exercised end to end. Whoever picks them up
should run them from a console that already has a root, so a failure means the
router chose wrongly rather than `open_work` refusing for the reason in L4.
