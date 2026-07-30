---
name: write-spec
description: >
  Use BEFORE implementing anything non-trivial — a feature, a refactor, a
  migration, a bug fix whose cause isn't yet known. Triggers on "build X",
  "add support for Y", "let's implement", "write a spec", "plan this out",
  "spec this feature". Interviews the user with AskUserQuestion until the
  ambiguity is gone, then writes a self-contained SPEC.md that names the
  files, the escalation list, and the one runnable check that proves the
  feature works — so execution can run unattended in a fresh session.
---

# write-spec

Coding is not the bottleneck; verifying is. So the human gate belongs at
the **spec**, not the diff: a decision costs one sentence here and one PR
at review time. The output is a `SPEC.md` precise enough that a fresh
session with no memory of this conversation can execute it, and a reviewer
can check the diff against it without re-deriving intent.

Two things make a spec worth writing. Everything else is optimization:

1. **The interview** — ask until nothing material is still guessed.
2. **One runnable check** — with **blocked** as a legal way to finish.

## 1. Interview until the ambiguity is gone

Use `AskUserQuestion`. Full question bank, grouped by axis, in
[`references/interview.md`](references/interview.md) — read it, don't
improvise the questions.

The rule for what to ask: **ask only what changes the work.** If both
answers lead to the same diff, pick one, note it in the spec, and move on.
If you cannot predict the diff without the answer, ask.

- Batch 2-4 questions per round; 2-3 rounds is normal, more means the
  request is really several features.
- Offer concrete options with trade-offs, not "how would you like this
  done?" — a chooseable option is faster to answer than a blank.
- Recommend one option first and say why. The user is picking, not
  designing from scratch.
- Stop when a round changes nothing about the plan. Ambiguity that
  survives becomes a line in **Escalate on**, not a guess.

Before asking, read the code. A question whose answer is in the repo is a
question you should not be spending the user's attention on.

## 2. Write the spec

Copy [`templates/SPEC.md`](templates/SPEC.md) to the repo root (or
`docs/specs/<slug>.md` for a repo that keeps them) and fill every section.
What makes it self-contained:

- **Files & interfaces** — name the actual paths and the functions, types,
  routes, or tables involved. "The auth layer" is not a spec; a reader who
  has to go find them will find the wrong ones.
- **Out of scope** — the explicit non-goals. This is what stops a
  reasonable agent from "helpfully" refactoring next door.
- **Verification** — one command, its expected output, proving the feature
  works end to end. Not "tests pass" — the command a skeptic would run.
- **Escalate on** — the pre-declared stop list (see below).
- **Sanctioned fakes** — which fake, where it lives, when it's allowed.
  Write `None` if none are. Undefined here means invented at 2am.

Then check it deterministically:

```
${CLAUDE_SKILL_DIR}/scripts/check-spec.sh SPEC.md
```

It fails on a missing section, an unfilled placeholder, or a `TBD` — the
cheap version of "is this actually finished?".

## 3. Pre-declare the escalation list, then let it run

Reviewing every plan before every run is still a synchronous human gate;
it just moves the pain earlier. Instead the spec names what deserves a
stop, and the agent decides everything else and logs it:

> irreversible actions · data migrations · auth and permissions · public
> API or schema contracts · money · deletion · anything the spec forced it
> to guess · a capability it does not have

Everything not on that list: decide it, and add a line to the spec's
**Decision log** with a confidence marker. The user reads the
low-confidence ones, not all forty.

**Gate autonomy on dependency completeness, not task size.** An unattended
run needs its whole dependency set already present — key, service,
fixture, seeded database. Missing one means prepare it first or run the
task attended. This is the check to make *before* starting, not the
discovery that produces a fake at step nine.

## 4. Execute in a fresh session

Hand the spec to a new session (`Implement SPEC.md`) rather than continuing
this one. The interview context is spent, and a fresh reader tests whether
the spec is genuinely self-contained. If the executor has to come back and
ask, that was a gap in the spec — fix the spec, not just the answer.

## Boundaries

- **Skip this for genuinely small work.** A one-line fix, a typo, a rename:
  a spec costs more than it saves. The trigger is *non-trivial*, not *any*.
- The spec is not a plan-approval ritual. If you find yourself listing
  implementation steps for the user to approve, you have built the gate
  this skill exists to remove.
- Don't let the spec grow into a design doc. Every line has to change what
  gets built or what gets checked; if removing it changes neither, remove
  it.
- Update the spec when reality contradicts it mid-run, and say so. A spec
  that quietly diverges from the diff is worse than no spec, because review
  is checking the diff against it.
