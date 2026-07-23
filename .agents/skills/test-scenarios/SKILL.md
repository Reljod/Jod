---
name: test-scenarios
description: >
  Use when the goal is to test something *exhaustively* — enumerate every
  scenario and edge case, write one deterministic assertion per scenario, and
  drive them all to pass. Triggers on "make sure every case is handled",
  "add tests for the edge cases", "test this thoroughly", "cover all the
  scenarios", "harden the tests". Complements tdd-loop (which builds one
  behavior at a time); this audits an existing unit for completeness of
  coverage. Ships a sourceable assert.sh and an edge-case checklist.
---

# test-scenarios

`tdd-loop` grows a feature one failing test at a time. **This skill is the
other half:** given something that already exists, prove that *every*
scenario — happy path, boundaries, invalid input, re-runs, defaults — is
handled and passing. The discipline is simple and non-negotiable:

> A scenario without an assertion is not "handled" — it's unobserved.
> "Probably fine" is an untested case. Enumerate, assert, drive to green.

This belongs to the deterministic tier of the quality layer-model (see
`domains/coding/README.md`): the tests are pure, repeatable, and exit
non-zero on any failure, so the suite doubles as a pre-push / CI gate.

## The method

1. **Enumerate.** Walk [`references/scenario-checklist.md`](references/scenario-checklist.md)
   and write down the scenarios that apply as a short spec. The checklist is
   a taxonomy — happy path (incl. *every* variant, not just the first),
   input boundaries (empty / one / many / whitespace / special chars),
   invalid & hostile input (unknown options, missing args, **path
   traversal**, injection), state & idempotency (re-run, pre-existing output,
   partial failure), environment (missing dirs, permissions, defaults, run
   from elsewhere), and the output contract (exit codes, no leftover
   placeholders, deterministic output).

2. **Assert.** Turn each scenario into exactly one deterministic assertion.
   Source the shared helper so you're not re-inventing pass/fail plumbing:

   ```bash
   source "path/to/.agents/skills/test-scenarios/scripts/assert.sh"

   section "happy path — all variants"
   assert_ok    mytool --mode a
   assert_ok    mytool --mode b
   assert_file  out.txt "produces output"

   section "invalid input"
   assert_fails mytool --mode nope          # unknown value → non-zero exit
   assert_missing out.txt "nothing written on failure"

   section "idempotency"
   assert_ok    mytool --run
   assert_fails mytool --run                # refuses to clobber without --force

   assert_summary; exit                     # exit != 0 if any assertion failed
   ```

   Helpers: `assert_ok` / `assert_fails` (command exit code), `assert_file` /
   `assert_dir` / `assert_missing` / `assert_symlink` / `assert_link_to`,
   `assert_grep` / `assert_no_grep` (fixed-string, so special characters match
   literally), `assert_eq`, and `ok "<expr>" "name"` as the escape hatch.

3. **Drive to green.** Run the suite; for each failure, decide whether the
   *test* is wrong or the *code* is — fix whichever, re-run. Done means every
   assertion passes and the summary exits 0. If a scenario is genuinely out
   of scope, say so explicitly (as a comment in the suite) rather than
   leaving it silently uncovered.

## What good coverage looks like

- **Every variant, not a representative one.** Four presets → test four. The
  bug is in the one you'd have skipped.
- **Both sides of every guard.** If it refuses to overwrite, test that it
  refuses *and* that `--force` overwrites.
- **Failure leaves no mess.** On invalid input, assert both the non-zero exit
  *and* that nothing was written.
- **Deterministic.** No network, no clock, no randomness — same input, same
  result, every run. That's what makes it safe to gate on.

## Boundaries

- This skill audits coverage; it does not replace `tdd-loop` for *building*
  new behavior test-first. Use tdd-loop to grow it, test-scenarios to prove
  it's complete.
- Keep assertions deterministic. Anything non-deterministic (fuzzing,
  property-based, load) is a different, valuable layer — but wrap it in a
  deterministic gate before it can block a merge.
- The suite is a courtesy layer locally; make the same run a *required* check
  in CI for it to actually enforce anything.
