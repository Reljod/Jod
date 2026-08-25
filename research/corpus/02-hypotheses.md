# Phase 2 — 52 hypotheses derived from the survey

Each hypothesis states something that could be false, names the approaches it
comes from, and says how it is being tested here.

Test codes:
- **LIVE** — tested by running real agents in this study, scored against
  generated ground truth.
- **MEAS** — settled by measuring the harness (token accounting, wall clock),
  not by judging output quality.
- **PANEL** — put to an independent model panel (`agy`, Gemini family) so the
  judgement does not come from the same model that produced the work.
- **LIT** — not testable with this apparatus; graded on the strength of the
  published evidence and recorded as such rather than claimed as a result.

---

## Group 1 — When delegation pays (A1, A2, A4, A5, B4)

**H1.** Delegation is strictly dominated when the working set fits comfortably in
one context window: equal accuracy, higher cost, higher latency. *LIVE (E1)*

**H2.** The crossover where delegation starts to win happens well below the
nominal context limit — nearer 40-60% of it — because context rot degrades the
solo agent before the window is actually full. *LIVE (E2)*

**H3.** Task **shape** predicts the value of delegation better than task **size**.
A small broad task benefits more than a large narrow one. *LIVE (E1, E2)*

**H4.** For strictly sequential tasks (each step's input is the previous step's
output), delegation adds latency and cost with no accuracy gain, because there is
nothing to run in parallel. *LIVE (E1, t3_chain)*

**H5.** Accuracy against worker count is concave with an early knee (k≈2-4).
Past the knee, merge errors grow faster than coverage does. *LIVE (E3)*

**H6.** Cost grows faster than linearly in worker count, because every worker
re-pays a fixed context prologue that the solo agent pays once. *MEAS (E3)*

**H7.** For short delegated tasks the fixed prologue dominates the actual work,
so worker *count* drives cost far more than worker *effort*. *MEAS (E3)*

**H8.** An agent given a delegation tool will over-delegate on tasks that do not
need it, because delegation looks like diligence. *LIVE (E1, deleg vs solo)*

**H9.** Agent-decided delegation is worse calibrated than orchestrator-decided
delegation: the agent cannot see its own remaining context budget. *LIVE (E1)*

**H10.** Delegation converts a context problem into a communication problem. The
errors that survive are boundary errors (gaps and overlaps), not reasoning
errors. *LIVE (E4, dup_rate + recall)*

**H11.** Where solo and fan-out score equally, fan-out still wins on wall-clock
once per-worker slices are large enough for parallelism to beat start-up
overhead. *MEAS (E2)*

**H12.** Two levels of delegation nesting multiply cost without a matching
accuracy gain; the one-level restriction in shipped harnesses is an economic
finding, not an implementation limit. *LIT (A10)*

---

## Group 2 — The worker interface (A2, B4, F4)

**H13.** A rich brief (objective, explicit boundary, output format) improves
fan-out accuracy more than upgrading the worker model would. *LIVE (E4)*

**H14.** A vague brief's damage shows up specifically as boundary violation:
workers reporting items outside their own slice. *LIVE (E4, dup_rate)*

**H15.** Thin returns (answer only) beat fat returns (answer plus full
reasoning) on final accuracy, because the merging agent's own context stays
small. *LIVE (E5)*

**H16.** Fat returns raise cost measurably while leaving accuracy flat or worse
— the clearest case of paying for tokens that actively hurt. *LIVE + MEAS (E5)*

**H17.** A worker asked for a strict output line produces parseable output far
more reliably than one asked for prose, and parse failure is a large share of
observed "reasoning" failure. *LIVE (all modes, via ANSWER-line extraction)*

**H18.** The merge step is the accuracy bottleneck in fan-out, not the workers.
Worker answers are individually better than the merged result. *LIVE (E4/E5,
worker_answers vs final)*

**H19.** Telling a worker what it must **not** do (stay out of other slices)
matters as much as telling it what to do. *LIVE (E4)*

**H20.** Workers should return provenance (which file a fact came from), because
the merger cannot otherwise detect a duplicate from a contradiction. *LIT/LIVE*

**H21.** A small fixed tool surface beats a large one for a worker, since a
worker with fewer choices spends fewer turns choosing. *MEAS (turns per call)*

---

## Group 3 — Context management in the main agent (B1-B13)

**H22.** Sub-agent context isolation, not parallelism, is the mechanism that
makes the main-agent pattern work. A serial worker that returns a summary still
helps the orchestrator. *LIVE (E3, workers=1)*

**H23.** The compression ratio at the worker boundary (tokens burned to tokens
returned) is the single most useful health metric for this architecture; ratios
near 1 mean the isolation is doing nothing. *MEAS (all fan-out cells)*

**H24.** Compaction loses task-relevant detail preferentially, because summaries
keep narrative and drop enumerations — exactly the content agents need. *LIT (B1,
B3)*

**H25.** Compaction fired at a task boundary preserves more usable state than
compaction fired at a token threshold. *LIT (B2)*

**H26.** A compaction should be treated as a claim to verify, not a fact. *LIT
(B3)*

**H27.** Restating the objective at the end of a long context measurably improves
constraint adherence, and the gain grows with context length. *LIT (B6)*

**H28.** Externalising state to files beats holding it in context for any task
that outlives one window, and it is the only technique here that survives a
harness change. *LIT (B5, D4)*

**H29.** Prefix stability is worth more than prompt wording. Cache hit rate is
the dominant cost term in a long agent run. *MEAS (cache_read vs cache_create
across all cells)*

**H30.** Because each delegated call starts a fresh prefix, heavy delegation and
cache efficiency are in direct tension — an under-discussed cost of fan-out.
*MEAS (E3)*

**H31.** Loading identifiers and fetching bodies on demand beats pre-loading, and
the advantage grows with corpus size. *LIVE (E2, solo behaviour on huge corpus)*

**H32.** Clearing stale tool results is safer than summarising them, because
nothing is paraphrased and therefore nothing can be paraphrased wrongly. *LIT
(B9)*

---

## Group 4 — Verification (E1-E5)

**H33.** A clean-context critic catches materially more errors than a
shared-context self-review. *LIVE (E6)*

**H34.** Shared-context self-review can *lower* the score, because the model
agrees with its own earlier reasoning rather than re-deriving. *LIVE (E6)*

**H35.** Verification gains saturate after one pass; a second pass mostly
re-confirms. *LIVE (E6)*

**H36.** A second verification pass carries a real risk of over-correction, in
which a right answer is talked into being wrong. *LIVE (E6)*

**H37.** A judge from a different model family disagrees with the author model
more than a same-family judge does, and that disagreement is signal, not noise.
*PANEL*

**H38.** Judging against a fixed taxonomy produces higher inter-judge agreement
than judging against an open rubric. *PANEL*

**H39.** Veto-only review (reviewers may block, never approve) removes a real
failure mode: a hedged review being read as assent. *LIT (E4)*

**H40.** Verification is worth more than delegation per dollar on tasks that fit
in context — the cheapest accuracy is a second look, not a second agent. *LIVE
(E6 vs E1)*

---

## Group 5 — Asynchrony and scheduling (C1-C10)

**H41.** Pipelining beats barrier-synchronised stages whenever per-item duration
varies, and the gap widens with variance. *MEAS/SIM*

**H42.** The dominant wall-clock term in a short delegated call is process
start-up, not inference — so batching small delegations is worth more than
parallelising them. *MEAS (E3, workers=1 vs 8)*

**H43.** A long-running agent needs durable checkpoints not primarily for crash
recovery but so a human can interrupt it without losing work. *LIT (C2, C5)*

**H44.** Steering delivered at a turn boundary is strictly safer than steering
injected mid-tool-batch, which injection-resistant models may flag as an attack.
*LIT (C6) — this is a concrete, citable operational finding.*

**H45.** For an agent that runs for hours, the right human interface is an inbox
(queue and review) rather than a chat (block and wait). *LIT (C4)*

**H46.** Memory maintenance moved off the hot path (sleep-time compute) improves
responsiveness without hurting quality, because reorganisation is not
latency-critical. *LIT (C7)*

**H47.** A planner that emits a dependency DAG beats step-by-step routing on both
latency and cost, because the model is not re-invoked to decide every next step.
*LIT (C8) — reported at up to 3.7x latency and 6.7x cost.*

**H48.** Supervision (restart a failed worker under a declared policy) is a
better fit for agent failure than prevention, because agent failure is common,
cheap to detect and cheap to retry. *LIT (C10)*

---

## Group 6 — Memory and the long run (D1-D5)

**H49.** Retrieval scored on recency alone surfaces stale facts; scored on
relevance alone it surfaces trivia. The three-term score exists because each
term fails alone. *LIT (D2)*

**H50.** Reflection — periodically synthesising observations into higher-level
claims — is what turns a log into memory, and without it a memory store grows
without becoming more useful. *LIT (D3)*

**H51.** Facts need validity intervals, because an agent that cannot invalidate a
belief accumulates contradictions instead of knowledge. *LIT (D5)*

**H52.** A memory written for a future *reader* (the fact plus why it mattered)
outperforms one written as a transcript excerpt. *LIT (D4)*

---

## What this set is designed to resolve

The survey contains a genuine contradiction: Anthropic reports a 90.2%
improvement from orchestrator-worker, and Cognition reports that multi-agent
systems are fragile and that context engineering matters more. Both are
credible. H1-H4 and H22 are the hypotheses that decide between them, and the
prediction implicit in the corpus is that **neither is wrong** — they are
describing opposite sides of a crossover that nobody has located precisely.
Locating it is the point of the experiment.
