# Pre-registered hypotheses

**Date written:** 2026-08-09 · **Status at time of writing:** no experiment has
been run. Predictions below are committed *before* seeing any result, and
[`out/RANKINGS.md`](out/RANKINGS.md) scores them honestly — including the ones
that turn out wrong.

---

## What is being tested

Eleven memory architectures, each implementing the same two-method interface —
`ingest(chunk)` streaming in session order, and `retrieve(query, today)` — over
one adversarial multi-session corpus. Scoring is on **the evidence set each
architecture puts in front of the model**, not on an LLM's answer.

That choice is deliberate. The dominant 2026 critique of LongMemEval is that it
"measures downstream answer quality rather than upstream retrieval accuracy,
allowing a system to score well by returning a large candidate set and relying
on the model to recover the answer from noisy context, even when retrieval
itself failed." Grading the evidence layer removes the judge, removes the
API key, and makes the whole thing deterministic and reproducible.

## The strategies

| # | Strategy | Provenance of the idea |
|---|---|---|
| S1 | `full_context` | The long-context baseline — no memory at all |
| S2 | `recency_window` | Last N chunks; the naive default |
| S3 | `bm25` | Lexical-only retrieval |
| S4 | `dense` | Semantic-only retrieval |
| S5 | `hybrid` | OpenClaw's shipped recipe: `0.7·dense + 0.3·bm25`, importance multiplier, decay off |
| S6 | `hybrid_decay` | S5 with OpenClaw's temporal decay **on** (half-life 30d) |
| S7 | `hybrid_mmr` | S5 with MMR diversity re-ranking, λ=0.7 |
| S8 | `bounded_consolidated` | Hermes' bet: a hard character cap, newest-wins consolidation, whole memory always injected |
| S9 | `versioned_factstore` | Deterministic control plane — supersession, retraction, write-time provenance admission (Zep bi-temporal + arXiv:2606.01435) |
| S10 | `earned_promotion` | OpenClaw "dreaming" signal — promote by retrieval frequency × query diversity |
| S11 | `two_plane` | **Novel synthesis.** Deterministic control plane + hybrid recall + MMR + earned promotion + abstention |

## The corpus

Deterministically generated, seeded. Multi-session, with seven adversarial
properties layered in: superseded facts (the stale trap), explicit retractions,
multi-hop chains, time-scoped questions, heavy near-duplicate distractors,
paraphrase drift between how a fact is *stated* and how it is *asked*, and
**untrusted-source claims** that contradict owner-stated facts.

That last one matters: memory poisoning became OWASP ASI06 in the 2026 Agentic
AI Top 10, with reported attack success rates of 80–99.8%, and **no mainstream
memory benchmark tests it.** Here it is a scored query type.

---

## Predictions

| # | Prediction | Rationale |
|---|---|---|
| **P1** | `full_context` takes top raw recall but at 10–30× the token cost of any retrieval strategy | It is the upper bound by construction; the cost-performance literature puts the crossover near ~10 turns |
| **P2** | `hybrid` beats both `bm25` and `dense` individually, but by less than vendor marketing implies (<10 points) | Fusion gains are real but oversold |
| **P3** | **Every pure-recall strategy (S1–S7, S10) fails badly on current-value queries under strict scoring** | The stale trap: superseded versions are near-perfect lexical *and* semantic matches for the current one. This is the central prediction |
| **P4** | `hybrid_decay` materially improves current-value but *hurts* long-tail stable recall | Decay cannot distinguish "old" from "old and still true" — and OpenClaw ships it off by default |
| **P5** | `versioned_factstore` dominates current-value and historical-value at near-zero extra cost | A deterministic control plane is the right tool; arXiv:2606.01435 reports +24 to +34.8 pp from exactly this move |
| **P6** | `bounded_consolidated` has by far the best cost profile and surprisingly strong current-value, but the worst multi-hop and long-tail recall | Newest-wins-under-a-cap *is* a control plane in disguise. Hermes' bet is right about freshness and wrong about coverage |
| **P7** | `earned_promotion` adds cost for little gain on a uniform query distribution | Promotion-by-usefulness needs skewed, repeated queries to pay off — an honest prediction that it may underperform |
| **P8** | Only write-time provenance admission (S9, S11) resists poisoning; everything else retrieves the attacker chunk at a high rate | Retrieval-time ranking has no notion of trust, and the attacker chunk is written to be maximally relevant |
| **P9** | `two_plane` wins the composite but does **not** win every axis | If it swept everything, the corpus would be rigged |

### The prediction I most expect to be wrong

**P7.** Earned promotion is the mechanism I find most intellectually appealing —
letting retrieval generate the retention signal is the one genuinely clever idea
in OpenClaw's dreaming design — and appeal is exactly when to expect a null
result. If the query set is uniform, there is no skew for it to exploit and it
should be indistinguishable from S5 plus overhead.

---

## Declared limitations

Stated up front because they bound what the numbers mean.

1. **The dense channel is not a neural embedder.** No API key and no numpy are
   available here, so the semantic channel is **Random Indexing** (Kanerva 2000;
   Sahlgren 2005) — distributional term vectors from in-chunk co-occurrence,
   randomly projected to 192 dimensions. It is genuinely distributional and does
   recover paraphrases, but it is weaker than `text-embedding-3-small`. Findings
   that hinge on dense-vs-lexical quality are therefore **soft**; findings about
   the control plane do not depend on the embedder at all and are **firm**.
   Every result is tagged accordingly.
2. **The evidence layer is scored, not the model.** `full_context` is credited
   with containing the answer but not with an LLM's ability to resolve
   conflicting versions itself. Real models *sometimes* resolve recency —
   though arXiv:2606.01435 measures that ability degrading from 75% to 61% as
   context grows from 64K to 262K. **This framing favours control-plane
   strategies on strict current-value scoring**, so both a strict and a lenient
   variant are reported, and the gap between them is itself the finding.
3. **Synthetic corpus.** Generated, not human. It controls the adversarial
   structure precisely, at the cost of realism in phrasing and topic drift.
4. **Token counts are `len(text)/4`**, applied identically to every strategy —
   fine for relative comparison, not a billing estimate.
5. **One corpus, one seed** for the headline table. A seed sweep is run
   separately to confirm the ordering is stable rather than a fluke of the draw.
