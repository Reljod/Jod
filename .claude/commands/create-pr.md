---
description: Build a visual-first, easily-digestible PR description for the current change.
argument-hint: "[base ref, defaults to the merge-base with the default branch]"
---

Create a pull request for the current branch using the **create-pr** skill
at `.agents/skills/create-pr/SKILL.md`. Read that skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/create-pr` — the skill's own directory in this repo.

Base ref for the diff: $ARGUMENTS (if empty, use the merge-base between this
branch and the repo's default branch).

Steps:
1. Classify the diff with `scripts/categorize_diff.sh <base>...<head>`.
2. Capture the right visual per category (screenshots/GIFs for UI, mermaid
   for API/architecture/infra, terminal output for CLI). Show, don't tell.
3. Generate the deltas with `scripts/evidence_bundle.sh <base>...<head>`
   (add `--spec <path>` if the work had a spec). Paste it in whole — do not
   drop lines it flagged.
4. Scaffold the body with `scripts/pr_body_skeleton.sh <base>...<head>`,
   then fill it in — visuals first, then What changed, Verification (the
   command and its **real output**), Evidence, Decisions.
5. Open the PR as a **draft**, honoring this repo's standing PR rules
   (template detection, draft-by-default) and the attribution footer.
6. Once CI has **finished**, close it out yourself if the gate allows it:
   `.agents/skills/auto-merge/scripts/merge_pr.sh <pr> --ready`. That script
   decides — it merges only on an `auto-merge` verdict with every check green
   and the branch not behind base, and otherwise exits 1 with reasons. Report
   those reasons verbatim and leave the PR open; never fall back to
   `gh pr merge`. Only for work you carried end to end.
7. Report back which visual strategy you used per category, anything the
   substitutions scan flagged, and whether the PR merged or is waiting on a
   human (and why).
