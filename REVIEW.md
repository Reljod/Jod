# REVIEW.md

Instructions for the automated PR review that runs on every pull request
(`.github/workflows/claude-code-review.yml`). It is the **first pass**, not
the gate: required checks in CI are still the authority, and a human still
reads anything on the high-tier list below.

Read `AGENTS.md` for this repo's conventions before reviewing.

## Review in this order

1. **The diff against its stated intent.** If the branch carries a `SPEC.md`,
   check every requirement is implemented, every listed edge case is tested,
   and nothing out of scope was changed. Otherwise use the PR description.
2. **Substitutions.** Flag any of these unless the PR says explicitly why it
   was deliberate:
   - a credential, key, token, or endpoint value invented rather than read
     from the environment
   - a real integration swapped for a mock or stub in shipped (non-test) code
   - a test skipped, deleted, or `xfail`-ed
   - an assertion weakened or removed, or an `except`/`catch` widened to
     swallow a failure
   - test files or CI config edited by a change that was meant to be
     implementation only
   - a check narrowed to the subset that already passed

   A `BLOCKED.md` naming the missing capability, what was tried, and what is
   needed is the **correct** alternative to all of these. Treat it as a valid
   outcome, never as an incomplete PR.
3. **Correctness in the changed code** — the failure case, not the style.
4. **Evidence.** The body should carry the check's real output, not a ticked
   box. If it claims a check passed and shows nothing, say so.

## Severity

- **high** — auth, permissions, secrets, money, data migrations, deletion, a
  changed public contract (route, response shape, exported type, CLI flag,
  config key), or anything from the substitutions list above.
- **medium** — a correctness bug in the changed code with a nameable failing
  input.
- **low** — everything else. If you cannot name a case where it breaks, it is
  a preference: label it as one or leave it out.

## Don't comment on

- Style, naming, and formatting the linter doesn't already enforce.
- Missing abstraction, defensive branches, or tests for states the code makes
  unreachable. Asking for these produces over-engineering, which costs more
  than the gap it closes.
- Anything the PR declared out of scope.
- Pre-existing issues the diff didn't touch.

Report gaps, not impressions. One resolved finding with a concrete failing
case beats five speculative ones.
