# Scenario & edge-case checklist

Enumerate against this list before declaring something tested. Not every row
applies to every unit — but each row is a question you must *answer*, and a
row you can't rule out needs an assertion, not an assumption. "Probably fine"
is an untested scenario.

## 1. Happy path

- [ ] The nominal case, with realistic inputs, produces the expected output.
- [ ] Every documented flag / option / mode is exercised at least once.
- [ ] All discrete variants (each preset, each subcommand, each enum value) —
      not just the first one. Bugs hide in the variant you didn't render.

## 2. Input boundaries

- [ ] **Empty** — empty string, empty list, empty file, zero items.
- [ ] **One** — single-element list (off-by-one lives here).
- [ ] **Many** — large input; does it still terminate and stay correct?
- [ ] **Whitespace** — leading/trailing/internal spaces, tabs, blank lines.
- [ ] **Special characters** — `& | / \ < > ' " $ * ? { }`, unicode, emoji.
      Especially where the value flows into a shell, a regex, SQL, or markup.
- [ ] **Very long** — names/paths near length limits.

## 3. Invalid & hostile input

- [ ] Unknown option / unknown enum value → clear error, non-zero exit.
- [ ] Missing required argument → clear error, not a stack trace.
- [ ] Wrong type / malformed value → rejected, nothing half-written.
- [ ] **Path traversal** — `../`, absolute paths, symlinks pointing outside
      the intended tree. A name that should be one path segment must not be
      able to read or write elsewhere.
- [ ] Injection — values that look like flags (`--foo`), delimiters, or code.

## 4. State & idempotency

- [ ] **Re-run** — running twice does not double-apply or corrupt (idempotent
      where it claims to be).
- [ ] **Pre-existing output** — target already exists: does it refuse, merge,
      or overwrite? Is that the *intended* one, and is it tested both ways
      (guard refuses; `--force`/equivalent overwrites)?
- [ ] **Partial failure** — if step 3 of 5 fails, is the result left clean, or
      half-written? Nothing should be created when validation fails.
- [ ] Order independence where claimed; correct ordering where required.

## 5. Environment & resources

- [ ] Missing target dir / file / dependency → handled, not a crash.
- [ ] Permissions: read-only location, non-writable target.
- [ ] Defaults: every value that has a default is correct when omitted
      (e.g. target defaults to cwd, name defaults to the dir basename).
- [ ] Works from a different working directory than the source tree.

## 6. Output contract

- [ ] Exit code is 0 on success, non-zero on every failure path — so the unit
      composes into a hook or CI gate.
- [ ] No leftover template tokens / placeholders / TODO markers in generated
      output.
- [ ] Idempotent output: same input → byte-identical output (no timestamps or
      randomness leaking in).

## How to use it

1. Walk the list; write down the scenarios that apply as a short spec.
2. Turn each into **one deterministic assertion** (see `assert.sh`).
3. Run; drive failures to green. A scenario without an assertion is not
   "handled" — it's unobserved.
