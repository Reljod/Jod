# The future of memory in LLMs — what's real, what's marketing, what to build

**Date:** 2026-08-09 · **Analyst:** Jod · **Companions:**
[`HERMES.md`](HERMES.md) · [`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md) ·
**Experiment:** [`experiments/FINDINGS.md`](experiments/FINDINGS.md)

> **Question, as asked:** where is LLM memory going, which studies and
> repositories implement it, and are they actually worth it?

Confidence markers: **[src]** read from source · **[paper]** from the paper ·
**[docs]** vendor documentation · **[3p]** third-party, unverified.

---

## The answer

**Retrieval is a solved-enough problem and the field is still optimising it.
The unsolved problem is governance** — knowing which of five contradictory
statements is currently true, deleting something so it stays deleted, and
refusing to believe a fact the user never said. Almost every product in the
category sells better recall. Almost none sells governance, and governance is
where the measured failures are.

Three things follow, and I ran an experiment to check the middle one.

1. **The benchmark numbers in this category are not trustworthy.** Independent
   replication puts Mem0's LongMemEval at **73.8%** against a published claim of
   **93.4%**; the gap traces to benchmark-specific prompt engineering in the
   evaluation harness. **[3p]** Zep's audit of the same benchmark found LoCoMo's
   full-context baseline (**~73%**) *beating* Mem0's best score (**~68%**), plus
   a category with missing ground truth. **[3p]** Buy on architecture, never on
   a leaderboard.
2. **Memory systems lose to long context on accuracy and win on cost, and the
   crossover is about ten turns.** A controlled comparison has long-context
   GPT-5-mini at **92.85%** on LoCoMo where Mem0 scores **57.68%** — a 35-point
   gap caused by compressing ~102k tokens down to ~2,909 retrieved (**35:1**).
   **[paper]** Memory becomes cheaper after ~10 interaction turns at 100k
   context. **[paper]** So today's memory layers are a *cost* optimisation that
   costs you accuracy — unless they add governance, which is the part that
   cannot be recovered by spending more tokens.
3. **The frontier that matters is memory moving into the weights and the KV
   cache**, not better vector search. Cartridges, nested learning, and
   sleep-time compute are the live directions.

**My own experiment** ([17 architectures, 1,702 chunks, 173 scored
queries](experiments/FINDINGS.md)) found the single highest-value component is a
**deterministic control plane** — versioned facts, real deletion, write-time
trust. It lifted current-value accuracy from 0.17 to **0.73** while being the
*cheapest* strategy tested. Raw long context scored 1.00 on recall, 0.00 on
freshness, 0.00 on deletion, and had a **100% poisoning attack success rate**,
at **314×** the tokens.

---

## 1. The state of the field, honestly

### The taxonomy that has actually settled

The December 2025 survey *Memory in the Age of AI Agents* (arXiv:2512.13564)
proposes the three-axis frame the field has converged on, explicitly retiring
"long-term vs short-term" as insufficient: **[paper]**

| Axis | Values |
|---|---|
| **Form** | token-level (text you can read) · **parametric** (weights) · **latent** (hidden states / KV) |
| **Function** | factual · experiential · working |
| **Dynamics** | formation · evolution · retrieval |

Most shipped products occupy exactly one cell: token-level, factual, retrieval.
The interesting research is everywhere else.

### The benchmark credibility problem

This deserves its own section because it invalidates most comparison tables you
will find.

| Claim | Independent measurement |
|---|---|
| Mem0 LongMemEval **93.4%** (published) | **73.8%** observed post-April-2026; **57.5%** before **[3p]** |
| Mem0 SOTA on LoCoMo | Full-context baseline ~**73%** beats Mem0's ~**68%** **[3p]** |
| Zep at 65.99% (in Mem0's paper) | **75.14% ± 0.17** when Zep ran it correctly **[3p]** |

The documented inflation mechanisms are specific: dataset-specific equivalence
rules keyed to benchmark question IDs, hidden chain-of-thought before grading,
"lean toward yes" judge prompts, and one-directional override clauses. **[3p]**
Mem0's evaluation of Zep also used incorrect user modelling, broken timestamp
handling, and sequential rather than parallel search. **[3p]**

The benchmarks themselves are weak too. LoCoMo conversations average
**16k–26k tokens** — inside every modern context window, so they cannot test
long-term memory at all — and have known ground-truth errors. LongMemEval grades
the final answer rather than retrieval, so a system can score well by dumping a
large candidate set and letting the model dig the answer out of noise. **[3p]**

**Practical rule:** treat every published memory benchmark number as a claim
about a vendor's evaluation harness until reproduced. This directly shaped my
experiment — it scores the evidence set, with no judge in the loop.

### What the better benchmarks say

**MemoryAgentBench** decomposes memory into four competencies and finds no
architecture wins all four: RAG leads accurate retrieval, long-context leads
test-time learning and long-range understanding, and **every method fails
selective forgetting — at most 28% on multi-hop deletion.** **[paper]**

**BEAM** (from *Beyond a Million Tokens*) runs 100 conversations and 2,000
validated questions out to **10M tokens**; even 1M-context models degrade as
dialogues lengthen. Its LIGHT framework — episodic + working memory + scratchpad
— adds **3.5–12.69%** over the strongest baselines. **[paper]**

Forgetting is the field's open wound, and it is exactly what a control plane
addresses.

---

## 2. The repositories, and whether they are worth it

Star counts pulled from the GitHub API on 2026-08-09. **[src]**

| Repo | ★ | Approach | Worth it? |
|---|---:|---|---|
| [mem0ai/mem0](https://github.com/mem0ai/mem0) | 62.8k | LLM fact extraction + vector + optional graph. April 2026 rewrite: single-pass hierarchical extraction, ADD-only (never overwrites), entity linking, multi-signal fusion (semantic + BM25 + entity) | **Yes, with eyes open.** Easiest adoption, largest community. Claims 92.5 LoCoMo / 94.4 LongMemEval at <7k tokens/query vs 25k+ for full context **[docs]** — discount those numbers hard. The ADD-only + entity-linking + temporal-rerank direction is genuinely the right one |
| [getzep/graphiti](https://github.com/getzep/graphiti) | 29.7k | Bi-temporal knowledge graph; every node and edge carries `valid_at` / `invalid_at` | **Yes — the most architecturally serious.** Independently measured **63.8%** vs Mem0's **49.0%** on LongMemEval **[3p]**, and the gap is temporal reasoning. This is a control plane, sold as a graph |
| [topoteretes/cognee](https://github.com/topoteretes/cognee) | 29.9k | ECL (extract-cognify-load) pipelines into graph + vector | Maybe. Strong if you already think in data pipelines; more machinery than most agents need |
| [supermemoryai/supermemory](https://github.com/supermemoryai/supermemory) | 28.8k | Hosted semantic + graph ingest, broad integrations | For product speed, not for control. You are buying someone else's opinions about your memory |
| [letta-ai/letta](https://github.com/letta-ai/letta) | 24.2k | MemGPT lineage: core / recall / archival tiers, **sleep-time compute** | **Yes, for the ideas.** The OS metaphor (tiered memory, agent manages its own paging) is the most transferable design in the category, and sleep-time compute is a real advance — background agents refine shared memory blocks off the critical path |
| [EverMind-AI/EverOS](https://github.com/EverMind-AI/EverOS) | 11.9k | Local-first, Markdown-native, self-evolving; EverMemOS reports 92.3 LoCoMo / 82 LongMemEval-S | Watch. Markdown-native and user-owned is the right posture; the benchmark claims need the same discount as everyone's |
| [MemTensor/MemOS](https://github.com/MemTensor/MemOS) | 10.7k | "Memory OS": ultra-persistent memory, hybrid retrieval, cross-task skill reuse, 35.24% token savings | Watch. Skill reuse as memory is the underrated idea here — the same bet Hermes makes |
| [OSU-NLP-Group/HippoRAG](https://github.com/OSU-NLP-Group/HippoRAG) | 3.9k | Neurobiological: neocortex (LLM) + parahippocampal encoder + hippocampal KG; v2 fixes v1's entity-centric context loss | Research-grade, actively maintained. Read it for the ideas, don't deploy it |
| [basicmachines-co/basic-memory](https://github.com/basicmachines-co/basic-memory) | 3.6k | Local Markdown knowledge base over MCP | **Yes, if you want simple.** Closest in spirit to how Jod already works |
| [agiresearch/A-mem](https://github.com/agiresearch/A-mem) | 1.1k | Zettelkasten: atomic notes, LLM-suggested links, periodic consolidation. Claims up to 6× on multi-hop and 85–93% fewer memory-operation tokens **[paper]** | Ideas yes, code no — **last pushed 2025-12-12**, effectively dormant |

**The pattern worth seeing:** the two best-regarded systems (Graphiti, Mem0's
2026 rewrite) both converged on the same thing without calling it that —
**temporal validity as a first-class field, and never overwriting a fact.** That
is a control plane. My experiment measures why it dominates.

---

## 3. Where it is actually going

Four directions, ranked by how much they would change what you build.

### 3.1 Memory in the weights — nested learning and HOPE

Google Research's **Nested Learning** reframes a model as "a system of
interconnected, multi-level learning problems optimised simultaneously," arguing
architecture and optimiser are the same thing at different update frequencies.
The **HOPE** architecture — a self-modifying Titans variant — adds a **Continuum
Memory System**: memory banks updating at different rates, fast ones holding
immediate detail, slow ones consolidating abstractions. Titans prioritised
memories by surprise but supported only two update levels; HOPE supports
unbounded levels of in-context learning and reports lower perplexity plus better
needle-in-a-haystack than Transformers, Titans, Samba, TTT, and Mamba2. **[docs]**

**Worth it?** Not to adopt — there is no production path. Worth it as the
strongest signal about direction: the long-run answer to "how does an agent
remember" is probably *not* a database. Note the Google blog states no
limitations at all, which is itself a caution.

### 3.2 Memory in the KV cache — cartridges

**Cartridges** (arXiv:2506.06266) trains a small KV cache offline per corpus via
"self-study" (synthetic conversations + context distillation), then loads it at
inference. Results: **matches in-context learning at 38.6× less memory and 26.4×
higher throughput**, extends effective context from 128k to **484k** on MTOB,
and — surprisingly — cartridges **compose at inference without retraining**.
**[paper]** Follow-ups scale it to large document collections and derive the
weights analytically instead of by training.

**Worth it? This is the one I would watch hardest.** It attacks the actual
economics: amortise the cost of understanding a corpus once across every query
against it. For a personal agent whose corpus is "everything about you", that is
precisely the right shape. Composability without retraining is the part that
makes it a *system* rather than a trick.

### 3.3 Memory off the critical path — sleep-time compute

Letta's **sleep-time compute** runs a background agent sharing the primary
agent's memory blocks, refining them while the user is idle. **[docs]** OpenClaw
independently shipped the same idea as nightly "dreaming"; OpenAI reportedly
runs a background user-context synthesis; Claude distils long-term-worthy
information roughly every 24 hours. **[3p]**

**Worth it? Yes, and it is cheap.** Four independent teams converged on it. The
insight is that consolidation is not latency-sensitive, so it should never
compete with the user for either tokens or wall-clock. OpenClaw's version is the
most carefully engineered — see the promotion thresholds and the 25%
loss-fraction guard in [`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md#4-writing-and-consolidation).

### 3.4 Memory as an evolving prompt — agentic context engineering

**ACE** (arXiv:2510.04618, ICLR 2026) treats context as an evolving playbook
built by generation → reflection → curation, targeting two named failure modes:
**brevity bias** (summarisation dropping domain detail) and **context collapse**
(iterative rewriting eroding content). Reports **+10.6%** on agents and
**+8.6%** on finance, matching a top production agent on AppWorld with a smaller
open model. **[paper]**

**Worth it? Yes, and it is the most immediately applicable.** Context collapse
is exactly the failure OpenClaw's `maxPriorEntryLossFraction: 0.25` guard
prevents and that Hermes' hard cap induces. If you let an LLM rewrite your
memory file, you need a named defence against it quietly deleting things.

---

## 4. Security is not optional any more

Memory poisoning became **OWASP ASI06** in the 2026 Agentic AI Top 10. Reported
attack success rates: **80%, 95%, 99.8%**; Agent Security Bench averages
**84.30%**; MINJA achieves ~**98%** injection success by getting the agent to
write its own poisoned reasoning traces, later retrieved as few-shot examples
for other users. **[3p]**

The distinction that matters: **prompt injection resets between sessions;
memory poisoning does not.** The attack and its effect are temporally
decoupled — you are attacked on Tuesday and exploited in November.

Defences cluster at four points, and **prompt-only filtering is repeatedly shown
insufficient**: **[3p]**

1. **Write-time admission** — reject by origin before storage
2. **Provenance binding** — trust label stored where content cannot forge it
3. **Retrieval-time filtering**
4. **Post-hoc forensic detection**

OpenClaw does 1 and 2 properly (origin class `owner`/`agent`/`untrusted`/`system`
stored outside the chunk text). Hermes scans for injection patterns but has no
origin class. Most of the repos in §2 do none of it.

**My experiment produced a warning here.** Write-time admission drove attack
success to zero as expected — but so did most plain retrievers, simply because
the attacker's chunk was outranked. That is not security. **"Too weak to
retrieve the attack" fails open the moment your retrieval improves.** Only
`full_context` (ASR 1.00) and `bounded_consolidated` (0.47) failed visibly; the
others were merely lucky, and would score identically to a properly defended
system on any benchmark that only measures outcomes.

---

## 5. So — is it worth it?

By situation, with the evidence behind each call.

| Situation | Verdict |
|---|---|
| Single session, accuracy critical | **No memory layer.** Long context wins by 33–35 points **[paper]** |
| <10 turns against the same context | **No.** Below the cost crossover **[paper]** |
| Persistent assistant, many sessions | **Yes** — but for governance and cost, not accuracy. Expect to *lose* raw accuracy to a 35:1 compression ratio |
| Facts that change over time | **Yes, and this is the strongest case.** Bi-temporal validity is the single highest-value feature measured anywhere, mine included |
| Ingesting anything external | **Yes, mandatory** — provenance and write-time admission, or you have an ASI06 hole |
| Wanting the leaderboard number | **No.** Published numbers do not survive replication |

**The honest summary:** buying a memory *product* today mostly buys cost
reduction and a benchmark claim. Building a memory *control plane* buys
correctness that no amount of context can substitute for. The industry sells the
first and under-invests in the second.

---

## 6. What I would build, and why

From [`experiments/FINDINGS.md`](experiments/FINDINGS.md), ranked by measured
value per unit of complexity:

1. **Deterministic control plane.** Versioned facts with validity intervals,
   real deletion, write-time trust admission. Conflict resolution is
   `max(version)` in code, never a prompt — arXiv:2606.01435 measures **+24 to
   +34.8 pp** from exactly that move, and shows LLM-mediated resolution
   degrading from 75% to 61% as context grows 64K→262K. **[paper]** In my run it
   took current-value from 0.17 to **0.73** as the *cheapest* strategy tested.
2. **A second retrieval hop into reserved slots.** Multi-hop 0.00 → **0.42** for
   ~14 extra tokens. The design rule is load-bearing: reserve slots, never let
   the hop displace round one. Merging instead cost more than it gained.
3. **Hybrid recall over what survived the control plane** — but sweep your own
   weights and use a *relative* score floor. OpenClaw's shipped 0.7/0.3 was 30%
   worse than the swept optimum on my corpus, and its absolute `minScore: 0.35`
   returns **zero results** under a different fusion formula.
4. **Sleep-time consolidation with a blast-radius bound.** Four teams converged
   on background consolidation; OpenClaw's 25% loss-fraction guard is the piece
   most implementations lack.
5. **Skip the earned-promotion tier.** Zero measured gain, +62% tokens — and it
   was the mechanism I most expected to like.
6. **Skip temporal decay for freshness.** It destroyed long-tail recall
   (0.40 → **0.00**) to buy less than the control plane gives free.

### The one thing I would add that I could not test

Control-plane placement (arXiv:2606.15903) evaluates thirteen configurations on
a 385-case adversarial surface and finds deterministic primitives **cannot**
canonicalize — **5%** on identifier obfuscation, **0%** cross-lingual — while an
LLM hook at *mutation* time recovers intent-aware deletion (**78–85%**) and
lifts nearly every category at once (**91.7–93.2%** overall, **$0.17** per
385-case run, 2.3 s/case). **[paper]**

So the mature design is **deterministic by default, LLM at mutation time for
what determinism provably cannot do** — entity canonicalization and
intent-aware deletion ("forget everything about that project"). My experiment
validates the deterministic half and does not test the hybrid; that paper is the
best evidence for the other half, and it is where I would look next.

---

## 7. Predictions

1. **Control planes get productised within a year.** Graphiti and Mem0's 2026
   rewrite both arrived at temporal validity independently; the vocabulary will
   follow the implementations.
2. **The benchmark reckoning is coming.** MemoryAgentBench and BEAM measure
   forgetting and 10M-token behaviour; a category whose systems fail selective
   forgetting at ≤28% cannot keep publishing 90%+ headline numbers.
3. **Cartridges-style KV memory is the sleeper.** Composable, amortised,
   38.6× cheaper in memory — attacking economics rather than recall.
4. **Retrieval quality stops being the differentiator.** Everyone has hybrid
   search. The differentiators become forgetting, provenance, and temporal
   correctness — the three things nobody markets.
5. **Security forces the issue.** ASI06 plus 80–99.8% attack success rates means
   write-time admission becomes table stakes, and it happens to be the same
   mechanism a control plane already needs.

---

## Sources

**Papers** — [Memory in the Age of AI Agents (2512.13564)](https://arxiv.org/abs/2512.13564) ·
[Beyond a Million Tokens / BEAM (2510.27246)](https://arxiv.org/abs/2510.27246) ·
[Evaluating Memory via Incremental Multi-Turn Interactions / MemoryAgentBench (2507.05257)](https://arxiv.org/abs/2507.05257) ·
[Beyond the Context Window: cost-performance of fact memory vs long context (2603.04814)](https://arxiv.org/html/2603.04814v1) ·
[Don't Ask the LLM to Track Freshness (2606.01435)](https://arxiv.org/html/2606.01435v1) ·
[Control-Plane Placement Shapes Forgetting (2606.15903)](https://arxiv.org/abs/2606.15903) ·
[Cartridges (2506.06266)](https://arxiv.org/abs/2506.06266) ·
[Agentic Context Engineering (2510.04618)](https://arxiv.org/abs/2510.04618) ·
[A-MEM](https://openreview.net/forum?id=FiM0M8gcct) ·
[Agent-Memory-Paper-List](https://github.com/Shichun-Liu/Agent-Memory-Paper-List)

**Model-level** — [Nested Learning / HOPE](https://research.google/blog/introducing-nested-learning-a-new-ml-paradigm-for-continual-learning/)

**Independent evaluation** — [The state of AI memory 2026: claimed vs observed](https://www.maximem.ai/blog/state-of-ai-memory-2026-claimed-vs-observed) ·
[Is Mem0 Really SOTA in Agent Memory?](https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/)

**Systems** — [Letta sleep-time compute](https://www.letta.com/blog/sleep-time-compute/) ·
[Letta memory blocks](https://www.letta.com/blog/memory-blocks/) ·
[Mem0 token-efficient algorithm](https://mem0.ai/blog/mem0-the-token-efficient-memory-algorithm)

**Security** — [AI memory poisoning](https://vectorize.io/articles/ai-memory-poisoning) ·
[Memory Poisoning Attack and Defense (2601.05504)](https://arxiv.org/abs/2601.05504) ·
[Survey on Long-Term Memory Security (2604.16548)](https://arxiv.org/abs/2604.16548)
