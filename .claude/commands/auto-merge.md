---
description: Triage a PR into categories and merge it only if it is safe to merge unread.
argument-hint: "[PR number, defaults to the PR for the current branch]"
---

Decide whether a pull request can merge without a human reading it, using
the **auto-merge** skill at `.agents/skills/auto-merge/SKILL.md`. Read that
skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/auto-merge` — the skill's own directory in this repo.

Target PR: $ARGUMENTS (if empty, the PR for the current branch — `gh pr view
--json number -q .number`).

Steps:
1. Triage it: `scripts/pr_triage.sh origin/<base>...<head-sha>`. On an open
   PR, CI has already posted this as the `merge:auto` / `merge:human` label
   and the triage comment — read that instead of recomputing by hand.
2. Dry-run the merge: `scripts/merge_pr.sh <pr> --dry-run`. This prints every
   precondition — open, not draft, no requested changes, all checks green,
   not behind base, verdict `auto-merge`.
3. If it exits 0, merge: `scripts/merge_pr.sh <pr>` (squash, linear history,
   branch deleted).
4. If it exits 1, **stop and report the reasons verbatim**. Do not run
   `gh pr merge`, do not edit the classifier, do not re-run hoping for a
   different answer. A refusal is a correct outcome.
5. If the only reason is that the branch is behind base, run
   `scripts/merge_pr.sh <pr> --update-branch`, then wait for CI to go green
   on the new head before re-running step 2.
6. Report: the categories, the verdict, and either the merge commit or the
   specific reason a human now owns it.
