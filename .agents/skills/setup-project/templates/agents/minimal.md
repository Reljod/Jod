# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent working in this repository (Claude Code,
Claude Agent SDK, or any AGENTS.md-compatible tool). `CLAUDE.md` is a
symlink to this file.

## What this repo is

{{PROJECT_DESC}}

## Principles

1. **Act decisively where the call is clearly yours; escalate the ambiguous
   ones** instead of guessing.
2. **Reversible by default.** Reading, drafting, and editing don't need a
   check-in. Confirm anything hard to reverse or visible to others (pushing
   to shared branches, sending messages, deleting things) first.
3. **Leave the repo better than you found it** — match the surrounding
   style, keep changes focused, and write down anything worth reusing.

## Branching

Work on feature branches, not directly on `main`. Suggested naming:
`{{BRANCH_PREFIX}}/<short-description>`.

## Grow this file

This is a deliberately lean starting point. Add sections (commit
convention, test/quality gates, PR habits, reusable skills under
`.agents/skills/`) as the project's needs become real — don't pre-build
process for hypothetical futures.
