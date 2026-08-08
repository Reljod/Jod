---
description: Rebase this branch onto main, resolve conflicts, run the tests, then force-push with a lease.
argument-hint: "[base ref, defaults to the remote's default branch] [--cmd \"<test command>\"]"
---

Rebase the current branch using the **rebase-main** skill at
`.agents/skills/rebase-main/SKILL.md`. Read that skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/rebase-main` — the skill's own directory in this repo.

Base / options: $ARGUMENTS (if empty, the remote's default branch and the
auto-detected test command).

Steps:
1. `scripts/rebase-main.sh preflight` — fetches, saves the pre-rebase HEAD,
   and refuses on a detached HEAD, an in-progress rebase, or the default /
   any protected branch. **Report the ahead/behind counts to the user before
   rewriting anything.**
2. `scripts/rebase-main.sh start` (add `--autostash` only if the dirty files
   are genuinely unrelated). Exit `3` means conflicts.
3. On conflicts: `status` to list them, then resolve **one file at a time** —
   read both sides first, remember `--ours` is *main* during a rebase, and
   regenerate lockfiles rather than hand-merging them. `git add` each file,
   then `continue`. Repeat while it keeps returning `3`.
4. `scripts/rebase-main.sh check` (or `check --cmd "<command>"`) — the
   project's real suite. **RED is a stop.** Decide whether the rebase broke
   it or main was already red by testing the saved pre-rebase SHA; never
   narrow the command or weaken a test to get past it.
5. `scripts/rebase-main.sh push` — `--force-with-lease --force-if-includes`,
   gated on a green `check` at the current HEAD. If the lease rejects the
   push, the remote moved: re-run `preflight` and rebase again. Never
   `--force`.
6. Report back: how many commits replayed, every conflict and the resolution
   you chose (naming any file where you took one side wholesale), the test
   command with its real output, and the pushed ref.

Confirm with the user before pushing if anyone else may have this branch
checked out — a review in progress, or a branch stacked on top. If the rebase
cannot land honestly, `abort`/`restore` and write `BLOCKED.md` instead of
forcing it through; nothing has been pushed at that point.
