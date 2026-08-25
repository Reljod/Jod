# Phase 3 — How the hypotheses were tested

## The problem with testing this

Most published comparisons of agent architectures are scored by another model.
That is a weak instrument for this particular question, because the thing being
compared — how an agent manages context and delegation — is exactly the thing a
judge model is also bad at assessing. A judge that cannot check an answer can
only rate how confident it sounds.

So the benchmark here is synthetic and generated, which means every answer has
an objective score that no model was involved in producing. Judges are then
pointed at the judging problem itself rather than used as the primary
instrument.

## The corpus

`gen_corpus.py` writes a tree of markdown notes containing planted lines whose
positions and values are recorded as it writes them. Two corpora were used:

| corpus | files | approx tokens | purpose |
|---|---|---|---|
| small | 60 | ~50k | fits comfortably in one context window |
| huge | 400 | ~316k | exceeds the worker model's 200k window |

Two design decisions matter for the validity of the results.

**The planted lines are scattered, not blocked.** An early version put all the
planted lines together, which meant one search hit handed the agent everything
nearby. They are now interleaved through filler text.

**The semantic task uses paraphrases.** Restart-safety is decided by a
combination of two properties, and each property is written as one of six
paraphrases. A single pattern match cannot answer it, and an agent cannot know
the paraphrase set in advance. Without this the "semantic" task collapses into
a search task and every configuration scores the same.

## The tasks

Chosen so that task *shape* varies independently of task *size*, because the
central question is whether shape decides when delegation pays.

| id | shape | why it is here |
|---|---|---|
| t1_scan | breadth, easy | control: trivially parallel and trivially searchable, so it shows what delegation costs when it was not needed |
| t2_join | breadth, needs per-file reasoning | a join between two lines in the same file; no single search answers it |
| t3_chain | depth | each step's input is the previous step's output, so there is nothing to parallelise |
| t4_sum | breadth then reduce | a scalar answer, which turns out to matter a great deal |
| t5_classify | breadth, semantic | per-file judgement over every file |

Every task ends with a required `ANSWER:` line, so parsing is deterministic and
a formatting slip is never silently scored as a reasoning failure. The two are
counted separately.

## The configurations

| mode | what it is |
|---|---|
| solo | one agent, no delegation tool available |
| deleg | one agent, delegation tool available and encouraged — the agent decides |
| fanout | the script partitions the files across k workers, then a synthesis call merges their answers — the orchestrator decides |
| verify | solo, then n critic passes, either clean-context or shared-context |

`fanout` additionally varies the worker brief (rich versus deliberately vague)
and the worker return (answer only versus full reasoning).

## Measurement

Every model call goes through `claude -p --output-format json`, so token counts,
cost and wall time are read from the harness rather than estimated. Recorded per
run: objective score, exact-match flag, cost, wall time, turns, input/output
tokens, cache reads and creates, cross-worker duplication, and whether the agent
actually spawned a subagent.

That last field was added after a check showed the delegation-enabled
configuration was scoring like the solo one. The reason turned out to be that
the model was declining to delegate, not that delegation made no difference.
Without recording it, the condition would have been unfalsifiable.

## Three things that were checked rather than assumed

**The ground truth was verified against the corpus on disk.** `verify_gt.py`
re-derives the answers by reading the written files instead of trusting what the
generator intended to write. This mattered: when every agent returned the same
wrong total, the first suspicion was a broken benchmark. It was not — the
agents were making the same mistake.

**The scheduling simulation was corrected after it produced a backwards
result.** The first version assigned whole items to worker slots up front,
which books future slot time nothing needs yet and made pipelining look worse
than a barrier. Rewritten as an event-driven scheduler, it gives a sensible and
more interesting answer.

**The judges are graded, not trusted.** Because every answer already has an
objective score, `judge.py` measures how well each judging condition tracks it,
across model family, rubric style, and whether the judge can inspect the corpus.

## Independent evaluation

Two separate uses of other models, both through `agy` so that no part of the
evaluation comes from the same family that produced the work:

- **Judge panel** — Gemini and Claude judges score the same answers under open
  and fixed-checklist rubrics, with and without the ability to check the source.
- **Ranking panel** — five model families rank the surveyed techniques for this
  specific system, with the list shuffled per ballot so list position cannot
  drive the result, tallied by Borda count.
