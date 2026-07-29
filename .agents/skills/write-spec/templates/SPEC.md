# SPEC — <feature name>

<!-- Written after the interview, before any code. A fresh session with no
     memory of that conversation must be able to execute this file alone.
     Validate with: .agents/skills/write-spec/scripts/check-spec.sh SPEC.md -->

## Goal

<!-- What behavior exists after this that doesn't now, and why it's wanted.
     2-4 sentences. State the user-visible change, not the implementation. -->

## Files & interfaces

<!-- Real paths and real names. A reader who has to go looking will look in
     the wrong place. -->

| Path | What changes |
|---|---|
| `` | |

Interfaces involved (functions, types, routes, tables, CLI flags, env vars):

-

## Out of scope

<!-- Explicit non-goals. This is what keeps a reasonable agent from
     "helpfully" refactoring next door. Name the tempting adjacent work. -->

-

## Verification

The check that proves this works, end to end:

```
<!-- one command -->
```

Expected: <!-- the observable result — exit 0 plus what output, what row in
the database, what pixels on screen. Real output, not "tests pass". -->

**Done means one of exactly two things:**

- the check above passes, and its **real output** is included as evidence; or
- a `BLOCKED.md` exists naming the missing capability, what was tried, and
  what is needed to unblock. Blocked is a legitimate, successful ending.

Because "make the check pass" is the goal, these are never acceptable ways
to reach it — take the blocked exit instead:

- inventing a credential, key, token, or endpoint value
- swapping a real integration for a mock to go green
- skipping, deleting, or `xfail`-ing a test
- weakening an assertion, or widening an `except`/`catch` to swallow it
- editing test files or CI config during an implementation task
- narrowing the check to the subset that already passes

## Sanctioned fakes

<!-- Which fake, where it lives, and when it may be used — or the single
     word None. Anything undefined here gets invented under pressure. -->

None

## Escalate on

Stop and ask when the work touches any of these; decide everything else and
log it below.

- irreversible or externally-visible actions
- data migrations, deletion, money
- auth, permissions, secrets
- public API / schema / config contracts
- <!-- anything this spec had to guess at — name it here -->
- a capability or dependency that isn't present in the environment

## Decision log

Filled in during execution, not now. One line per decision made without
asking, with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| | | |
