<!-- blurb: lean identity + a few principles; grow it as needs get real -->
# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent working in this repo (Claude Code, Claude Agent
SDK, any AGENTS.md-compatible tool). `CLAUDE.md` symlinks here.

## What this repo is

{{PROJECT_DESC}}

## Principles

1. **Act decisively where the call is clearly yours;** escalate the ambiguous
   ones instead of guessing.
2. **Reversible by default.** Reading, drafting, editing need no check-in.
   Confirm anything hard to reverse or visible to others first — pushing to
   shared branches, sending messages, deleting things.
3. **Leave the repo better than you found it.** Match the surrounding style,
   keep changes focused, write down anything worth reusing.

## Never work around a blocked check

- **Never**, to make a check pass: invent a credential value · swap a real
  integration for a mock · skip, delete or weaken a test · widen an
  `except`/`catch` to swallow a failure.
- **Instead** write `BLOCKED.md` — what's `Missing:`, what you `Tried:`, what it
  `Needs:` — and stop. Blocked is a successful ending; a hollow green isn't.

## Branching

Feature branches, not `main`. Suggested: `{{BRANCH_PREFIX}}/<short-description>`.

## Grow this file

Deliberately lean. Add sections — commit convention, quality gates, PR habits —
as the needs become real, as bullets, with the reasoning in `docs/decisions.md`.
Don't pre-build process for hypothetical futures.
