# Red-green-refactor: reference

Backing detail for `SKILL.md`: the spec/behavior template, per-ecosystem
runners and focusing syntax, and where TDD pays off most.

## Behavior list (the lightweight spec)

Before writing code for a non-trivial feature, write the behaviors down —
this is your spec and your loop's checklist. One line each, input →
expected output. Example for a retry wrapper:

```
- [ ] returns the result unchanged when the call succeeds first try
- [ ] retries up to N times on a transient error, then returns the result
- [ ] raises the original error after N failed attempts
- [ ] does not retry on a non-transient error (raises immediately)
- [ ] waits with backoff between attempts   (assert on injected clock)
```

Take them **one at a time**, top to bottom. Each unchecked box is one
RED → GREEN → REFACTOR turn of the loop. Checking the last box means the
feature is done and every behavior is pinned by a test.

## "Fails for the right reason"

A new RED test must fail on its **assertion**, not on an import error,
missing fixture, or typo. If it errors before reaching the assertion, you
tested your setup, not the behavior — fix the setup first and confirm the
red is a genuine assertion failure. Likewise, a test that passes the
instant you write it told you nothing new; make sure it actually exercises
the unbuilt behavior.

## Runners and focusing syntax

`detect-test-runner.sh` picks the whole-suite command; **focus** it on the
one test you're driving so the loop stays fast.

| Ecosystem | Whole suite | Focus one test |
|---|---|---|
| pytest | `pytest` | `pytest path/test_x.py::test_name` or `-k name` |
| vitest | `npx vitest run` | `npx vitest run x.test.ts -t "name"` |
| jest | `npx jest` | `npx jest x.test.ts -t "name"` |
| go | `go test ./...` | `go test ./pkg/ -run TestName` |
| cargo | `cargo test` | `cargo test test_name` |
| rspec | `bundle exec rspec` | `bundle exec rspec path_spec.rb:LINE` |
| gradle | `./gradlew test` | `./gradlew test --tests '*ClassName.method'` |
| mix | `mix test` | `mix test test/x_test.exs:LINE` |

## Where TDD pays off (and where it doesn't)

**High payoff** — anything with a clear input/output contract and real
branching logic: parsers, serializers, validators, state machines,
pricing/tax/financial rules, data transforms, permission checks. These are
also where the failure modes are subtle enough that example-first design
genuinely shapes the code.

**Once the examples are green, consider property-based testing** for the
same code: instead of more fixed cases, assert the invariant and let the
framework generate inputs — `encode(decode(x)) == x`, "a sorted list is
sorted and a permutation of the input", "total never goes negative".
Tools: Hypothesis (Python), fast-check (JS/TS), proptest (Rust), jqwik
(Java). This catches edge cases the example tests didn't think of.

**Lower payoff / don't force it** — thin glue with no logic (a controller
that just forwards to a service), throwaway spikes, and UI layout, where
the feedback loop is visual rather than assert-based. Test the logic these
call into, not the wiring.

## How this connects to the rest of the stack

The watch loop is the **local, fast** layer — seconds per cycle, focused on
one test. It is not the gate. The same tests must run green in **CI** as a
required status check (that's the mandatory layer), and the whole suite —
not just the focused test — runs there. Mutation testing is the periodic
audit that answers "do these green tests actually assert anything"; run it
occasionally against the modules you care about most, not on every commit.
The cheap local watch loop runs *under* the mandatory CI gate — that layering
is the point: fast feedback locally, the real gate in CI.
