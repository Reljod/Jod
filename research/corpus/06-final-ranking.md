# Phase 5 — The final top five

The pre-experiment ranking was written down before any runs, so it can be
compared with what the evidence produced.

| | before experiments | after experiments |
|---|---|---|
| 1 | B4 sub-agent context isolation | **Gate delegation on task shape** |
| 2 | C2 durable execution | **Make every worker return self-describing** |
| 3 | A5 single-writer, multi-reader | **Context isolation, with worker count as the cost dial** |
| 4 | C4 agent inbox | **Verify by a different method, not a second opinion** |
| 5 | E2 clean-context reviewer | **Durable execution with an inbox** |

Two entries changed materially. **E2, the clean-context reviewer, was dropped**:
it measured *worse* than a shared-context critic, and neither caught the study's
one systematic error. **Self-describing worker returns was promoted from a minor
entry (H20) to second place** on the largest effect measured anywhere in the
study. Those are the two places where running the experiment changed the answer.

---

## 1. Gate delegation on task shape, not on corpus size

**The rule.** Delegate when the task requires reading more than fits in one
context window. Do not delegate because the corpus is large, because the task
seems big, or because delegation looks diligent.

**The evidence.** Below the crossover, delegation is strictly dominated: equal
accuracy at 4× the cost on breadth tasks, and **7.4× the cost with 6× the
latency** on a sequential task. On a 316,000-token corpus — five times the
worker's context window — a *searchable* task still favoured a single agent by
**36× on cost** (0.874 at $0.07 against 0.995 at $2.57), because the agent
searched instead of reading; what the extra spend bought was consistency, not
capability. Only when the criteria were paraphrased so that search could not
find them, forcing every file to be read, did the single agent collapse from
0.986 to 0.118 — and delegation gave 2.4× the accuracy.

**Why it matters most.** This is the single decision that determines whether the
architecture pays for itself, and it is the one the published record gets
muddled. Anthropic's 90.2% gain and Cognition's "don't build multi-agents" are
both correct; they sit on opposite sides of this line. Breadth-first research
that must consult many sources crosses it. A codebase you can navigate by search
does not.

**The trap.** "My data is bigger than the context window" is not the trigger.
Search makes most large corpora fit. The trigger is that the work itself cannot
be narrowed before reading.

---

## 2. Make every worker return say what it is

**The rule.** A worker's reply must state its own meaning, not just carry a
value. `confirmed matches in my slice = …`, never a bare list.

**The evidence.** The largest effect in the study, and the cheapest.

| worker return | score | cost |
|---|---|---|
| bare answer | **0.000** (4/4 failed) | $0.18 |
| answer + one clause of self-description | **0.978** (4/4) | $0.23 |

The failure was not obvious and was found by accident. Fan-out scored zero on a
task where the workers were demonstrably right — between them they found every
target file. The merging agent discarded their work and asked for reports it had
already been given, because on that task the answer type (file paths) matched
the input type (file paths), making a bare answer indistinguishable from the
worker's own assignment. Returning full reasoning also fixed it (1.000), but at
five times the extra cost, because verbosity is an accidental way of achieving
self-description.

**Why it matters.** It converts a total, intermittent, silent failure into a
non-issue for one clause of prompt text. Nothing else measured here has that
ratio. It also generalises: the moment a worker's output could be mistaken for
its input, merging becomes a coin flip.

---

## 3. Context isolation is the mechanism — and worker count is the cost dial

**The rule.** The point of a sub-agent is that it burns its own context and
returns a summary. Use the fewest workers that cover the work.

**The evidence.** Isolation, not parallelism, is what buys anything: the single
agent's collapse on the over-window task was a context problem, and delegation
recovered it. But each worker starts a fresh prefix and pays for it:

| mode | output tokens | cache created | calls |
|---|---|---|---|
| solo | 4,928 | 17,228 | 1 |
| fan-out ×4 | 18,943 | 117,525 | 5 |
| fan-out ×8 | 43,438 | **350,332** | 9 |

Cache creation is **20× higher** at eight workers, and per-call cache creation
rises too (17k → 23k → 39k). Delegation and prompt-cache efficiency pull against
each other, which is rarely stated. For short delegated calls the fixed prologue
dominates the actual work, so **worker count drives cost far more than worker
effort does**.

**The health metric.** Watch the ratio of tokens a worker burns to tokens it
returns. Near 1 means the isolation is doing nothing and you are paying for a
round trip.

---

## 4. Verify by a different method, not by a second opinion

**The rule.** A check is only worth its cost if it can fail independently.
Prefer an assertion, a recomputation by another route, or ground truth. Treat a
same-model critic as nearly worthless against systematic error.

**The evidence.** Every configuration got the aggregation task wrong in exactly
the same way, returning the sum of all four topics instead of one. Fifteen runs
across five configurations — solo, fan-out, clean-context critic, shared-context
critic, two critic passes — produced **zero catches**. The critic re-reads the
task and misreads it identically. The LLM judges scored that same wrong answer
50–75 out of 100.

Verification also has a headroom condition. On a task already at 0.986, adding a
critic *lowered* the score to 0.752 and the clean-context critic did worst,
because a critic that cannot improve a correct answer can only damage it. The
published gain curve (about 62% to 70%) was measured where there was a lot to
fix.

And the judges themselves are weak instruments: no judging condition exceeded
0.67 rank correlation with objective truth, letting a judge read the source did
not help (0.563 against 0.628 blind), and **21% of objectively worthless answers
scored above 70**. One answer that contained no real data at all — the literal
template `CODE=84,CODE=134,…` — was rated 68–90 by all five judges.

**The one cheap win.** A fixed checklist rubric cut cross-family judge
disagreement from **34.2 points to 13.2**. If you must use a judge, give it a
taxonomy rather than an open question, and read agreement as a measure of what
is obvious rather than of what is true.

---

## 5. Durable execution with an inbox

**The rule.** For an agent that runs unattended for hours, persist the run so it
can be resumed, and put a queue in front of it so a human can interrupt,
redirect, or approve without losing work.

**The evidence, and its limits.** This is the one entry in the five that this
study did **not** measure. It is here on two other grounds. The independent
panel of five model families was unanimous: durable execution appeared on
**15 of 15 ballots** with the most first-place votes, and the agent inbox topped
the tally at 57 Borda points, also on 15 of 15. And it follows directly from the
requirement: the moment a task outlives a process, resumability stops being an
optimisation and becomes the difference between a system and a demonstration.

It is stated separately from the measured findings because it is not one.

---

## Runners-up

**State the task before the data.** At 122,000 tokens, stating the objective
before the document rather than after was worth about 11 points of recall
(0.961 against 0.853, n=6, p≈0.03). Instruction *adherence* never degraded in
any condition, so the drift that recitation targets did not appear — what
degraded was retrieval. An agent that knows what it is looking for before it
reads finds more. Restating the objective a second time at the end added
nothing.

**Single-writer, multi-reader.** The strongest structural claim in the
literature, and untestable here because every task in this benchmark was
read-only. It is kept as a runner-up on the strength of the published record
rather than promoted on evidence this study did not gather.

**Scalar answers are the dangerous shape.** A set-valued answer degrades
visibly and earns partial credit; a single number is either right or
catastrophically wrong with no symptom. Every silent failure in this study was
a scalar.

---

## The one-paragraph answer

The pattern is called orchestrator-worker, or a supervisor, and when it is
packaged it is called an agent harness. It works, but only above a specific
line: delegate when the task cannot be narrowed by search before reading, and
not otherwise, because below that line it costs 4–36× for little or no accuracy.
When you do delegate, the failures that remain are not reasoning failures but
communication failures — the merge step is the weak point, and the fix is that
every worker states what its answer means. Verify with something that can fail
independently, because a second instance of the same model shares its blind
spots exactly. And if the thing is to run for hours unattended, durability and
an inbox are what make it a system rather than a demonstration.
