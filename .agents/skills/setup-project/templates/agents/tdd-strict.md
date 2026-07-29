<!-- blurb: test-first enforced, coverage as a required CI gate -->
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
   For anything non-trivial, resolve the ambiguity and write it down before
   the first test. → **`write-spec`**
2. **RED** — write exactly one failing test. Confirm it fails for the
   *right reason* (an assertion, not a setup error).
3. **GREEN** — write the minimum code to pass. No more.
4. **REFACTOR** — clean up with the test green as a safety net.
5. Repeat.

Run the loop with **`tdd-loop`**. A change that adds behavior without a test
that would have failed before it is incomplete, regardless of how obvious
the code looks.

## Green means green — and "blocked" is a legal ending

A test-first repo binds every task to "make the bar green", so when the bar
*cannot* go green — no key, no service, no fixture — the cheapest remaining
move is to change what green means. That is the failure mode to name, because
from inside the loop it feels like finishing.

Never, in service of the bar: invent a credential value · swap a real
integration for a mock to go green · skip, delete, or `xfail` a test · weaken
an assertion · widen an `except`/`catch` to swallow a failure · narrow the
check to the part that already passes.

Instead write a `BLOCKED.md` — `Missing:` / `Tried:` / `Needs:` plus the
failing test's path — and stop there. An honest red is a result; a bought
green is a lie the next reader will trust.

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
<type>: <subject>      e.g.  feat: add retry to sync worker
```

`test`-type commits are first-class here — a commit that only adds a failing
test is a normal, encouraged step. → **`setup-git-hooks`**

{{TICKET_RULE}}

## PRs

Draft by default. Build the body with **`create-pr`**; make the RED→GREEN
evidence and coverage delta visible so a reviewer can see the discipline was
followed, not just claimed.

## Skills

Reusable skills live under `.agents/skills/<skill-name>/SKILL.md` with a thin
slash-command wrapper in `.claude/commands/`.
