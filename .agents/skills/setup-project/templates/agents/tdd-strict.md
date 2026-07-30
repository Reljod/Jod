<!-- blurb: test-first enforced, coverage as a required CI gate -->
# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent working in this repo (Claude Code, Claude Agent
SDK, any AGENTS.md-compatible tool). `CLAUDE.md` symlinks here. Reasoning goes
in `docs/decisions.md`; procedure in the skill that owns it.

## What this repo is

{{PROJECT_DESC}}

## Test-first, always — the rule that governs everything

No production code is written except to make a failing test pass. Not a
suggestion; it is how work happens here. → **`tdd-loop`**

1. **Spec** — the next behavior in one sentence (input → output). Ambiguous
   requirements get resolved before the first test. → **`write-spec`**
2. **RED** — exactly one failing test. Confirm it fails for the *right reason*
   (an assertion, not a setup error).
3. **GREEN** — the minimum code to pass. No more.
4. **REFACTOR** — clean up with the test green as a safety net.

A change that adds behavior without a test that would have failed before it is
incomplete, however obvious the code looks.

## Green means green — and blocked is a legal ending

When the bar *cannot* go green — no key, no service, no fixture — the cheapest
move is to change what green means. From inside the loop that feels like
finishing. It isn't.

- **Never**, in service of the bar: invent a credential value · swap a real
  integration for a mock to go green · skip, delete or `xfail` a test · weaken
  an assertion · widen an `except`/`catch` to swallow a failure · narrow the
  check to the part that already passes.
- **Instead** write `BLOCKED.md` — `Missing:` / `Tried:` / `Needs:` + the
  failing test's path. An honest red is a result; a bought green is a lie the
  next reader will trust.

## Coverage is a required gate, not a report

- Enforced in CI with a threshold that only ratchets up.
- A PR that lowers coverage does not merge. New code arrives with its tests.
- Prefer behavior coverage over line coverage: test the contract, so refactors
  stay green.
- Don't chase the number — coverage is a byproduct of the loop, and
  coverage-that-asserts-nothing is what mutation testing exists to catch.

## Principles

1. **Reversible by default.** Reading, drafting, editing are free. Confirm
   anything hard to reverse or visible to others.
2. **Fast feedback beats thorough-but-slow locally.** Keep the inner loop (one
   focused test) sub-second; the full suite is CI's job.

## Conventions

- **Branches:** `{{BRANCH_PREFIX}}/<short-description>-<id>`, never `main`.
- **Commits:** `<type>: <subject>`, imperative, ≤72 chars. `test`-type commits
  are first-class — a commit that only adds a failing test is a normal step.
  → **`setup-git-hooks`**
- {{TICKET_RULE}}
- **PRs:** draft by default. Build the body with **`create-pr`**; make the
  RED→GREEN evidence and the coverage delta visible, so a reviewer sees the
  discipline was followed rather than claimed.
