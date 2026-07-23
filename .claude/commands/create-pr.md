---
description: Build a visual-first, easily-digestible PR description for the current change.
argument-hint: "[base ref, defaults to the merge-base with the default branch]"
---

Create a pull request for the current branch using the **create-pr** skill
at `.agents/skills/create-pr/SKILL.md`. Read that skill and follow it.

Base ref for the diff: $ARGUMENTS (if empty, use the merge-base between this
branch and the repo's default branch).

Steps:
1. Classify the diff with `scripts/categorize_diff.sh <base>...<head>`.
2. Capture the right visual per category (screenshots/GIFs for UI, mermaid
   for API/architecture/infra, terminal output for CLI). Show, don't tell.
3. Scaffold the body with `scripts/pr_body_skeleton.sh <base>...<head>`,
   then fill it in — visuals first, then What changed, Test plan, Checks.
4. Open the PR as a **draft**, honoring this repo's standing PR rules
   (template detection, draft-by-default) and the attribution footer.
5. Report back which visual strategy you used per category.
