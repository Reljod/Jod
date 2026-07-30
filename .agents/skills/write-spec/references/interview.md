# The spec interview — question bank

The interview exists to buy back review time: a decision made here costs the
user one click, and the same decision made wrong costs a re-implementation.
So the bar for asking is not "would this be nice to know" — it is **"do I
know what to write without it?"**

## Before you ask anything

Read the code first. Grep for the module, open the neighbours, check the
tests and the existing conventions. Every question whose answer is already
in the repo spends the user's attention on something you could have found,
and it trains them to stop reading your questions carefully.

## How to ask

- **2-4 questions per round**, batched in one `AskUserQuestion`. 2-3 rounds
  total is normal. Needing five rounds means this is several features and
  should be several specs.
- **Options, not blanks.** Give 2-4 concrete choices with the trade-off
  named. Put your recommendation first and say why in the description — the
  user is confirming a judgment, not designing from nothing.
- **Multi-select** when the answers aren't exclusive (which surfaces to
  cover, which cases to handle).
- **Stop when a round changes nothing.** If the answers wouldn't have
  changed a line of the diff, the interview is over.
- Anything still ambiguous when you stop goes in **Escalate on** — a
  declared stop is honest; a guess is not.

## The axes

Pick the axes that apply. Most features need three or four of these, not
all nine.

**1. Outcome** — What is observably true after this that isn't now? Who
sees it? What is the *smallest* version that would count as done?

**2. Scope edges** — Name the tempting adjacent work and ask whether it's
in or out: the neighbouring refactor, the second call site, the other
platform, the backfill. This is the question that most often prevents a
30-file diff.

**3. Code boundaries** — Extend the existing thing or add a new one? Which
existing pattern should this follow? Is there a file it must not touch
(generated code, vendored, another team's)?

**4. Contract** — Is any public surface changing: API route, response
shape, exported type, CLI flag, config key, event name, DB column? Must old
callers keep working, and for how long? Contract changes are also the ones
that belong on the escalation list.

**5. Data & state** — Is anything persisted? What happens to rows that
already exist — default, backfill, or nullable? Is a migration in scope,
and is it reversible?

**6. Failure behavior** — When the thing it depends on is down or the input
is bad: fail loudly, retry, degrade, or skip? Partial success — commit what
worked or roll it all back? This is where unattended runs go wrong, and
it's rarely in the original request.

**7. Verification** — *Always ask this one.* What command would convince
you it works? What input or fixture should it run against, and what output
proves it? If the user says "just make the tests pass", push once: which
test, and what does it assert that would have failed before?

**8. Dependencies present?** — Does the runnable check need a key, a
service, a seeded database, a fixture file, network access? Ask whether
each is available in this environment *now*. A missing dependency
discovered at implementation time is the single most common cause of an
invented fake; discovered here, it's a five-minute setup or a deliberate
sanctioned fake.

**9. Sanctioned fakes** — If something genuinely can't be reached, ask
which stand-in is acceptable and where it should live. Naming one fake
prevents six improvised ones.

**10. Non-functional** — Ask only when it changes the design: expected
scale, latency budget, whether the data is sensitive, whether this path is
authenticated.

## Don't ask about

- Anything discoverable in the repo (test runner, lint config, file layout).
- Style and naming you can infer from the surrounding code.
- **Plan approval.** "Here are my six steps, look OK?" re-creates the
  synchronous gate the spec is meant to replace. Ask about the *decisions*,
  not the *sequence*.
- Questions with one sensible answer. Make the call, write it in the spec as
  an assumption, and move on.

## One worked round

For "add rate limiting to the public API":

| Axis | Question | Options offered |
|---|---|---|
| Contract | What does a throttled caller get back? | `429` + `Retry-After` (recommended — standard, clients already handle it) · `429`, no header · silent queue-and-delay |
| Scope edges | What's the limit keyed on? | API key (recommended) · IP · both, key first |
| Dependencies | Where does the counter live? | Redis, already running in dev · in-process, resets on deploy · Postgres table |
| Verification | What proves it works? | integration test hammering the endpoint until it 429s (recommended) · unit test on the limiter · manual curl loop |

Four questions, one round, and the resulting spec has a testable contract,
a named store, and a check a fresh session can run. The version without the
interview picks each of those silently and gets caught at review.
