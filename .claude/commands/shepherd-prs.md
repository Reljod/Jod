---
description: Sweep the open PRs, review the ready ones with agents, and merge what survives.
argument-hint: "[--pr N] [--ready] [--dry-run] (default: sweep every open PR)"
---

Sweep the open pull requests and close out the finished ones, using the
**shepherd-prs** skill at `.agents/skills/shepherd-prs/SKILL.md`. Read that
skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/shepherd-prs` — the skill's own directory in this repo. Its
sibling `${CLAUDE_SKILL_DIR}/../auto-merge` is `.agents/skills/auto-merge`.

Arguments: $ARGUMENTS — pass `--pr N` through to shepherd a single PR,
`--ready` to allow publishing your own drafts, `--dry-run` to stop after the
verdicts without merging anything.

Steps:
1. Sweep: `.agents/skills/shepherd-prs/scripts/pr_sweep.sh --format md`
   (add `--pr N` / `--ready` if given). Report `blocked` and `skipped` PRs and
   do not investigate them further.
2. For each `ready` PR, spawn **`merge-checker`** and **`reviewer`** in
   parallel in a single message. Both are read-only; both end with
   `VERDICT: CLEAR` or `VERDICT: BLOCK — <reason>`. Give `merge-checker` the
   path `.agents/skills/auto-merge/scripts/merge_pr.sh`.
3. Fail closed: anything that is not exactly `VERDICT: CLEAR` from **every**
   agent — a block, a hedge, a malformed line, a crashed agent — blocks that
   PR. A near-clear is not a clear.
4. Merge survivors one at a time:
   `.agents/skills/auto-merge/scripts/merge_pr.sh <pr> --ready`. The gate runs
   again and it is the one that counts. Re-sweep after each merge — every merge
   puts the other branches one commit behind, which is itself a refusal.
5. On any refusal, report the reasons **verbatim** and leave the PR open. Do
   not run `gh pr merge`, do not edit the classifier, do not retry hoping for
   a different answer.
6. Report per PR: merged (with the commit) or not merged (with the specific
   reason, in the gate's or the agent's own words).
