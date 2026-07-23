---
description: Exhaustively test a unit — enumerate every scenario and edge case, assert each, drive to green.
argument-hint: "[what to test — a script, function, command, or module]"
---

Test the target exhaustively using the **test-scenarios** skill at
`.agents/skills/test-scenarios/SKILL.md`. Read that skill and follow it.

Target: $ARGUMENTS

Steps:
1. **Enumerate** the scenarios that apply by walking
   `.agents/skills/test-scenarios/references/scenario-checklist.md` — happy
   path (every variant), input boundaries (empty/one/many/whitespace/special
   chars), invalid & hostile input (unknown options, missing args, path
   traversal, injection), state & idempotency (re-run, pre-existing output,
   partial failure), environment (missing dirs, permissions, defaults), and
   the output contract (exit codes, no leftover placeholders). Write them
   down as a short spec.
2. **Assert** — one deterministic assertion per scenario, sourcing
   `.agents/skills/test-scenarios/scripts/assert.sh` for the pass/fail
   plumbing. Cover *both* sides of every guard and assert failures leave no
   mess.
3. **Drive to green** — run the suite; fix the code or the test until every
   assertion passes and the summary exits 0. Call out explicitly any scenario
   you're deliberately leaving out of scope.
4. Keep it deterministic (no network/clock/randomness) so the same run works
   as a pre-push hook and a required CI check.
