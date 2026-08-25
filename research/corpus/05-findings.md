# Phase 4 — What the experiments found

147 agent runs on a generated benchmark with objective ground truth, plus 100
judge verdicts and 15 independent ranking ballots. Total spend on model calls,
about $64.

Everything below is scored against generated ground truth, not by a model.
Where a number rests on few trials, the n is given and the claim is weakened to
match.

---

## 1. Delegation is strictly dominated until the task stops fitting

This was the central question, and it has a sharper answer than expected.

**On tasks that fit in one context window, delegation costs more for nothing.**

| task | shape | solo | delegation offered | fan-out ×4 |
|---|---|---|---|---|
| t1_scan | breadth, easy | 0.991 @ $0.06 | 0.667 @ $0.05 | 1.000 @ **$0.23** |
| t2_join | breadth, join | 1.000 @ $0.07 | 0.933 @ $0.07 | 1.000 @ **$0.19** |
| t3_chain | **depth** | 1.000 @ $0.04, 16s | 1.000 @ $0.04, 16s | 1.000 @ **$0.30, 98s** |
| t5_classify | breadth, semantic | 0.986 @ $0.20 | 0.912 @ $0.15 | 0.000 @ $0.22 |

The depth task is the cleanest case in the study: identical accuracy, **7.4×
the cost and 6× the latency**. There is nothing to parallelise when each step's
input is the previous step's output, and the orchestration overhead is pure loss.

**But the crossover is real, and it is not where people say it is.**

On a 400-file corpus of roughly 316,000 tokens — well past the worker model's
200k window — the two tasks behave completely differently:

| task on the 316k corpus (n=5) | solo | delegation offered | fan-out ×4 |
|---|---|---|---|
| t2_join (searchable) | 0.874 @ **$0.07** | 0.869 @ $0.09 | 0.995 @ **$2.57** |
| t5_classify (must read everything) | **0.118** @ $0.22 | 0.259 @ $0.24 | **0.284** @ $2.54 |

On the searchable task, a corpus five times the context window causes almost no
trouble: the agent greps, never loads the corpus, and reaches 0.874 for seven
cents where fan-out pays **36× more** for 0.995. What fan-out actually buys
there is *consistency* rather than capability — solo is usually perfect but
occasionally fails outright (sd 0.282) where fan-out is steady (sd 0.010).
Paying 36× for that is still a poor trade.

On the semantic task, where the paraphrased criteria cannot be found by search
and every file must be read, solo collapses from 0.986 to 0.118 and delegation
gives **2.4×** the accuracy for 11× the cost.

Note that the fan-out figures on the semantic row are a **lower bound**: those
runs used the bare worker return that section 2 shows fails about half the time
on this task. With a self-describing return the gap would be wider.

**So the crossover is not "the corpus is bigger than the context window." It is
"the task requires reading more than fits."** Those are different conditions,
and conflating them is why the published record looks contradictory. A research
question that must consult many sources exceeds the window in the way that
matters; a large repository that can be navigated by search does not.

This reconciles the two positions in the literature without either being wrong.

*(n=2–5 per cell on the over-window rows, with high variance; the direction is
consistent but the exact magnitudes are soft.)*

---

## 2. The largest effect measured was one clause of prompt text

The accidental discovery of the study, and then a controlled test of it.

Fan-out failed completely on the semantic task — 0.000, repeatedly. The workers
were **correct**: between them they found all 12 target files. The merging agent
threw their answers away and replied *"Could you provide the workers' reports?"*

The cause: on that task a worker's answer is a list of file paths, and its
assignment was also a list of file paths. A bare answer is indistinguishable
from the input. On the other tasks, answers were `CODE=NUM` pairs and inputs
were paths, so no ambiguity arose and merging worked perfectly.

Two independent conditions confirm it:

| worker return | score | cost |
|---|---|---|
| bare answer only | **0.000** (4/4 failed) | $0.18 |
| answer + one clause saying what the list means | **0.978** (4/4) | $0.23 |
| full reasoning, then the answer | 1.000 (3/3) | $0.27 |

The fix is not verbosity. Full reasoning works because it is *self-evidently* a
set of findings, and one clause of self-description achieves the same thing at
**a fifth of the extra cost**. This refutes H15, which predicted that terse
returns would beat verbose ones because they keep the merger's context small.
Size was the wrong variable; ambiguity was the right one.

**A worker's return must state what it is, not just what it found.**

---

## 3. A shared blind spot defeats both delegation and verification

Every configuration got the aggregation task wrong, and wrong *identically*.
Asked to sum only the `alpha` facts, agents returned 19,209 — which is exactly
the sum of **all four topics**. The correct answer was 4,777.

When five independent runs agree on a wrong number, the benchmark is the first
suspect, so the ground truth was re-derived from the files on disk. It was
correct. The agents were consistently making the same over-broad match.

What is striking is what did not fix it:

| config | score |
|---|---|
| solo | 0.000 |
| fan-out ×4 | 0.000 |
| verify ×1, clean-context critic | 0.000 |
| verify ×1, shared-context critic | 0.000 |
| verify ×2, clean-context critic | 0.000 |

Fifteen runs, five configurations, **zero catches**. The critic re-derives the
answer and makes the same mistake, because the mistake is in how the task is
read, not in the arithmetic. The LLM judges missed it too, scoring that answer
50–75 out of 100.

This refutes H40. A second look is not worth more than a second agent when both
are blind in the same way. Only a check by a *different method* — an
independent computation, a ground-truth assertion, a test — catches this class
of error.

It also explains why a **scalar** answer is the dangerous shape. A set-valued
answer shows partial credit and the damage is visible; a single number is
either right or catastrophically wrong with no symptom at all.

---

## 4. Verification pays in proportion to headroom, and can be negative

| config on the semantic task | score | cost |
|---|---|---|
| solo, no verification | **0.986** | $0.20 |
| verify ×1, clean context | 0.752 | $0.29 |
| verify ×2, clean context | 0.912 | $0.51 |
| verify ×1, shared context | 0.970 | $0.43 |
| verify ×2, shared context | 1.000 | $0.35 |

Verification did not help, and the clean-context critic did worst — the
opposite of the published advice and of H33.

The reconciliation is headroom. The literature's gain curve (about 62% rising
to 70% on the first pass) was measured where there was a great deal to fix. At
a 0.986 baseline there is nothing to fix, so a critic can only do damage, and a
critic that cannot see the original reasoning re-derives from scratch and
sometimes overwrites a correct answer.

*(n=3, sd up to 0.31. The defensible claim is "no gain at 1.4–2.5× the cost",
not a confident reversal of the literature.)*

---

## 5. An agent offered delegation mostly declines it

Delegation-enabled runs scored and cost almost exactly like solo runs. The
reason turned out to be that the model **did not delegate**: `subagent_stats`
showed `spawned: 0` on tasks that fit in context. It took the offer up only on
the over-window semantic task, and even then rarely.

This refutes H8, which predicted agents would over-delegate because delegation
looks like diligence. On this evidence the calibration is better than expected —
though "declines to delegate" is also why delegation-enabled runs showed no
benefit where a benefit was theoretically available.

Recording the spawn count is what made this falsifiable. Without it, "delegation
allowed" and "delegation used" are indistinguishable, and every conclusion drawn
from that condition would have been wrong.

---

## 6. Delegation and prompt caching are in direct tension

| mode | output tokens | cache **created** | calls |
|---|---|---|---|
| solo | 4,928 | 17,228 | 1 |
| fan-out ×1 | 9,567 | 43,042 | 2 |
| fan-out ×4 | 18,943 | 117,525 | 5 |
| fan-out ×8 | 43,438 | **350,332** | 9 |

Every worker starts a fresh context and therefore pays a fresh prefix. Cache
creation is **20× higher** at eight workers than at none, and per-call cache
creation grows too (17k → 23k → 39k). H6, H7 and H30 are all supported: the
fixed prologue, not the work, dominates the cost of a small delegated call.

The practical consequence is that **worker count is the cost dial**, and it
should be set as low as the task allows.

---

## 7. Instruction position changes retrieval, not obedience

At about 122,000 tokens of inline document, with a required output constraint:

| where the task is stated | adherence | recall |
|---|---|---|
| before the document | 1.000 | **0.961** |
| stated twice | 1.000 | 0.912 |
| no constraint (control) | — | 0.882 |
| after the document | 1.000 | **0.853** |

**Adherence never degraded** — 1.000 in every condition. The objective drift
that recitation is meant to prevent did not appear at all. What degraded was
recall, and stating the task *before* the data beat stating it after by about
11 points (n=6 each, roughly p≈0.03). Restating it a second time at the end was
no better than stating it once at the start.

The mechanism is not primacy versus recency. An agent that knows what it is
looking for before it reads retrieves better than one that reads first and
learns the task afterwards. That refines the popular advice: recitation is an
addition to stating the task up front, never a substitute for it.

---

## 8. Grading the judges

Every answer already had an objective score, so the judges could be measured
rather than trusted.

| judge condition | Spearman vs truth | mean abs error |
|---|---|---|
| claude, open rubric | 0.669 | 40.6 |
| agy (Gemini), open rubric | 0.628 | 16.5 |
| claude, fixed taxonomy | 0.581 | 27.4 |
| agy, **allowed to read the corpus** | 0.563 | 23.0 |
| agy, fixed taxonomy | 0.552 | 24.6 |

Three things stand out.

**No judge exceeded 0.67.** Judges separate good from bad on average — mean 72.1
on perfect answers against 25.4 on worthless ones — but the tail is what
matters: **29% of objectively worthless answers scored above 50, and 21% scored
above 70.** The clearest single case was an answer that emitted the literal
template `CODE=84,CODE=134,…` with no real codes in it. True score 0.00. All
five judges rated it 68–90, including the one that could read the files.

**Letting a judge check the source did not help** (0.563 grounded against 0.628
blind). Access to the evidence is not the same as using it.

**Model family drives the verdict more than the rubric does.** Cross-family
disagreement was 34.2 points against 14.5 within a family. But a fixed taxonomy
rubric cut cross-family disagreement from **34.2 to 13.2** — H38 supported, and
the one cheap intervention that clearly improves judging.

---

## 9. Scheduling is a second-order concern

Simulated with the measured distribution of real call durations (mean 41.2s,
cv 0.29), comparing barrier-synchronised stages against pipelining:

| worker pool | barrier | pipeline | speed-up |
|---|---|---|---|
| 2 | 754s | 750s | 1.01× |
| 4 | 390s | 389s | 1.00× |
| 8 | 237s | 208s | 1.14× |
| 12 (≥ items) | 161s | 151s | 1.07× |

Pipelining only helps when the worker pool is *not* the bottleneck, and the gain
grows with duration variance (1.00× at zero variance, 1.03× at triple). H41 is
supported but small. When the pool is saturated, total work dominates and no
scheduling choice can recover anything.

*(The first version of this simulation reported pipelining as 0.63× — worse than
a barrier — because it assigned whole items to slots up front and booked future
slot time nothing needed. It was rewritten as an event-driven scheduler.)*

---

## Hypothesis scorecard

Of the 52 hypotheses, 21 were tested against live runs or harness measurement.

**Supported:** H1 (delegation dominated below crossover), H2/H3 (shape decides,
not size), H4 (depth tasks gain nothing), H6/H7 (fixed prologue dominates cost),
H10 (errors become boundary/communication errors), H20 (returns need
provenance), H22 (isolation, not parallelism, is the mechanism), H23
(burn-to-return ratio is the health metric), H29/H30 (caching versus
delegation), H36 (over-correction is real), H38 (taxonomy improves inter-judge
agreement), H41 (pipelining helps, modestly), H42 (start-up dominates short
calls).

**Refuted:** H8 (agents over-delegate — they declined), H13 (rich brief beats
vague — no difference; the dominant failure was elsewhere), H15 (thin beats fat
— the opposite, and for a reason neither predicted), H33 (clean-context critic
beats shared — worse here), H40 (a second look beats a second agent — neither
caught the shared blind spot).

**Not resolved:** H5 (worker-count knee — obscured by the merge failure), H14
(duplication from vague briefs — `dup_rate` was 0.00 everywhere, so no boundary
violation ever occurred and the hypothesis went untested).

The remaining 31 are graded on published evidence and marked as such; they are
not claimed as results of this study.

---

## Threats to validity

- **Small n.** Most cells are n=3, the over-window cells n=5. Cost and latency
  differences are large enough (4–36×) to survive this easily; accuracy
  differences of a few points are not, and are reported as inconclusive.
- **One worker model.** Everything ran on a small fast model. A larger model
  would likely raise every baseline and shrink the headroom where verification
  helps.
- **Harness overhead is in the cost figures.** Each `claude -p` call pays a
  fixed system prompt, which inflates fan-out cost relative to a bare API
  orchestrator. The direction of every comparison is unaffected; the magnitudes
  are an upper bound.
- **Two harness failures were excluded** after being separated from model
  failures by their stored stdout. Keeping them would have penalised whichever
  configuration happened to hit a flaky call.
- **Generated, not natural, tasks.** The benchmark controls difficulty precisely
  at the cost of realism.
