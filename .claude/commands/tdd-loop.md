---
description: Build a feature or fix a bug test-first with a tight red-green-refactor loop.
argument-hint: "[what to build/fix, and/or a focused test command]"
---

Drive the work test-first using the **tdd-loop** skill at
`.agents/skills/tdd-loop/SKILL.md`. Read that skill and follow it.

Task / focus: $ARGUMENTS

Steps:
1. **Spec** — restate the next behavior in one sentence (input → output).
   For a non-trivial feature, list the behaviors first and take them one
   at a time.
2. **RED** — write exactly one failing test. Run it with
   `.agents/skills/tdd-loop/scripts/tdd-loop.sh -- <focused test>` and
   confirm it fails for the *right reason* (an assertion, not a setup
   error).
3. **GREEN** — write the minimum code to pass; re-run the one-shot runner
   until you see the GREEN banner.
4. **REFACTOR** — clean up with the test green as a safety net; re-run.
5. Repeat for the next behavior until the behavior list is done.

Use one-shot runs (read the RED/GREEN result each turn) while you are the
one in the loop; reserve `--watch` for a human at the keyboard. Keep the
loop focused and fast — the full suite is CI's job.
