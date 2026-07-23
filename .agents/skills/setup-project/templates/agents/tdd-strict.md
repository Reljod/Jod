# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent working in this repository (Claude Code,
Claude Agent SDK, or any AGENTS.md-compatible tool). `CLAUDE.md` is a
symlink to this file.

## What this repo is

{{PROJECT_DESC}}

## The rule that governs everything here: test-first, always

No production code is written except to make a failing test pass. This is
not a suggestion; it is how work happens in this repo.

1. **Spec** — restate the next behavior in one sentence (input → output).
   For anything non-trivial, list the behaviors first and take them one at a
   time.
2. **RED** — write exactly one failing test. Confirm it fails for the
   *right reason* (an assertion, not a setup error).
3. **GREEN** — write the minimum code to pass. No more.
4. **REFACTOR** — clean up with the test green as a safety net.
5. Repeat.

Run the loop with **`tdd-loop`**. A change that adds behavior without a test
that would have failed before it is incomplete, regardless of how obvious
the code looks.

## Coverage is a required gate, not a report

- Coverage is enforced in CI with a threshold that only ratchets up.
- A PR that lowers coverage does not merge. New code arrives with its tests.
- Beyond line coverage, prefer behavior coverage: test the contract, not the
  implementation, so refactors stay green.

## Principles

1. **Reversible by default.** Reading, drafting, and editing don't need a
   check-in. Confirm anything hard to reverse or visible to others first.
2. **Fast feedback beats thorough-but-slow locally.** Keep the inner loop
   (one focused test) sub-second; the full suite is CI's job.

## Branching & commits

Feature branches only, never on `main`: `{{BRANCH_PREFIX}}/<short-description>-<id>`.

```
<type>: <TICKET> <subject>      e.g.  feat: {{TICKET_PREFIX}}-12 add retry to sync worker
```

`test`-type commits are first-class here — a commit that only adds a failing
test is a normal, encouraged step. → **`setup-git-hooks`**

## PRs

Draft by default. Build the body with **`create-pr`**; make the RED→GREEN
evidence and coverage delta visible so a reviewer can see the discipline was
followed, not just claimed.

## Skills

Reusable skills live under `.agents/skills/<skill-name>/SKILL.md` with a thin
slash-command wrapper in `.claude/commands/`.
