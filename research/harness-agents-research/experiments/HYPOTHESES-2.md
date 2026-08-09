# Round 2 — pre-registered hypotheses (lab mechanisms)

**Date written:** 2026-08-09 · **Status at time of writing:** round-2 corpus not
yet generated, no strategy run. All eleven predictions committed before any
result; [`FINDINGS-2.md`](FINDINGS-2.md) scores them honestly.

Round 1 ([`HYPOTHESES.md`](HYPOTHESES.md) → [`FINDINGS.md`](FINDINGS.md)) tested
the open-source designs — Hermes' cap, OpenClaw's hybrid recall, graph
expansion, earned promotion. [`BIG-LAB-MEMORY.md`](../BIG-LAB-MEMORY.md) then
found Anthropic, OpenAI and Google shipping eight mechanisms round 1 never
touched. This round tests those.

Round 1 code is untouched and still reproduces; round 2 adds its own corpus,
strategies and runner beside it.

---

## What's new in the corpus

Round 1 had one implicit tenant and no authored tier. Both are now real:

- **Two workspaces sharing the same project names.** Every workspace has a
  project called `atlas`; each one's `atlas` has different facts. A question
  asked in workspace A has a lexically near-identical decoy in workspace B —
  the hardest form of cross-tenant leak, where the decoy differs from the answer
  only by a metadata field.
- **An authored layer** — stated conventions per workspace, never superseded,
  never retracted. The `AGENTS.md` / Cursor-rules tier every lab keeps in a
  separate file from learned memory.
- **Withdrawn-fact history.** A retracted slot now also gets an "as of \<date
  before the retraction\>" question. The retraction said the fact was *wrong*,
  so the correct answer is to abstain — even about the past.

Carried over: supersession, retraction, paraphrase drift between statement and
question, filler distractors, untrusted poison claims.

**New metrics:** `leak` (evidence set contains a chunk from another workspace)
and `wrong` (top-ranked fact is a confident wrong answer rather than nothing).

## The mechanisms under test

| # | Lab mechanism | Source | A/B |
|---|---|---|---|
| **L1** | Scope as a hard partition applied *before* ranking | Memory Bank filters by identity then runs similarity; Claude isolates per project | `hybrid_noscope` · `hybrid_scope_boost` · `hybrid_scope_filter` |
| **L2** | Grep recall — bounded summary + literal search, no embeddings | Codex CLI reads `memory_summary.md` whole, greps `MEMORY.md` | `codex_grep` · `control_scoped` |
| **L3** | Recall-driven **eviction** under a hard cap | Codex prunes memories unrecalled for 30 days | `bounded_lru` · `bounded_unrecalled` · `bounded_importance` |
| **L4** | Destructive rewrite vs append-only supersession | ChatGPT "dreaming" rewrites when the world moves; Mem0's 2026 algorithm is ADD-only | `rewrite_inplace` · `append_supersede` |
| **L5** | A separate always-resident authored tier | `AGENTS.md`, Cursor rules, `CLAUDE.md` | `control_scoped` · `authored_core` · `authored_merged` |
| **L6** | Abstain rather than answer when the evidence conflicts | Claude Code treats recalled memory as non-authoritative until verified | `control_scoped` · `abstain_ambiguous` |
| **L7** | Redaction purges history, not just current state | Anthropic's memory-version redaction endpoint | `control_scoped` · `redact_history` |
| **L8** | Write-time admission vs being too weak to retrieve the attack | Codex redacts secrets pre-disk; OpenClaw's origin classes | k-sweep on poisoned queries, with and without admission |

---

## Predictions

| # | Prediction | Rationale |
|---|---|---|
| **P10** | **Hard scope filtering is the single largest effect in round 2** — larger than any retrieval change available | Every lab treats scope as a partition rather than a feature. If that's right it should dominate everything else here |
| **P11** | Scope-as-a-ranking-signal leaks on **>15%** of queries; the hard filter leaks **0%** | A boost is a thumb on the scale, and a near-identical cross-tenant decoy will sometimes outweigh it. This is the specific claim the labs' design implies |
| **P12** | `codex_grep` is competitive where question and statement share vocabulary, **clearly worse where they don't**, and cheaper in tokens | Grep cannot bridge paraphrase, and the corpus deliberately drifts statement vocabulary away from question vocabulary |
| **P13** | Recall-driven eviction beats LRU on long-tail recall | Round 1's LRU cap scored **0.00** on stable recall — everything unreferenced was evicted. "Nothing has recalled this" is a better signal than "nothing touched this recently" |
| **P14** | Importance-based eviction lands **between** LRU and recall-driven | Write-time importance is a weak, noisy prior — better than pure recency, worse than observed usage |
| **P15** | In-place rewrite wins current-value slightly and scores **~0.00** on historical-value; append-only wins the composite | Rewriting destroys the record. The stale trap vanishes because the stale versions are gone — along with any ability to answer about the past |
| **P16** | The authored tier wins its own query type outright at negligible token cost, and changes nothing else | ~8 short always-resident chunks. If this doesn't pay, the whole authored/learned split is questionable |
| **P17** | Routing authored facts through the same extraction pipeline as learned memory **measurably corrupts them** | Extraction runs at 90% recall / 5% false positives. Conventions that should be permanent will get missed or clobbered by misread filler — the mechanical reason labs keep the two in separate files |
| **P18** | Abstaining on conflicting evidence cuts the confident-wrong rate by **more than it costs in score** | Returning nothing is a better failure than returning another tenant's fact — and the metric should show that trade is favourable, not just principled |
| **P19** | A control plane that tombstones only *current* state still **leaks the withdrawn fact on historical queries**; purging every version fixes it | Round 1's retraction test only ever asked "what is it now?". Anthropic ships a redaction endpoint precisely because deleting the head is not deleting the record |
| **P20** | **Attack success rate rises with retrieval strength** for strategies without write-time admission, and stays at 0 with it | Round 1's warning, now tested directly: most strategies scored 0% ASR because the attacker chunk was outranked, not defended against. Widen `k` and that protection should evaporate |

### The prediction I most expect to be wrong

**P13.** Round 1 taught me recall-driven *promotion* did nothing, and I am now
predicting recall-driven *eviction* works. The distinction is real — evidence
for deleting ("nothing has needed this in 30 days") is much stronger than
evidence for ranking ("this was retrieved often") — but I may be rationalising a
mechanism because a lab ships it. If `bounded_unrecalled` ties `bounded_lru`,
the honest read is that eviction policy doesn't matter and the cap is the whole
story.

---

## Declared limitations

Carried from [`HYPOTHESES.md`](HYPOTHESES.md): the dense channel is Random
Indexing rather than a neural embedder, the corpus is synthetic, extraction is
simulated at 90% recall / 5% false positives, and scoring is on the evidence
layer rather than a generated answer. Three round-2 additions:

1. **Scope is a clean metadata field here.** Real systems must *infer* which
   project or user a request belongs to, and that inference is its own failure
   surface this experiment does not model. The leak figures are a lower bound.
2. **`codex_grep` is a stand-in, not Codex.** It reproduces the published shape
   — inject a bounded summary, then literal-match the long-form store — not
   OpenAI's implementation, which I have not read.
3. **No LLM summarizer.** Anthropic's context-editing-versus-compaction
   distinction (prune vs summarize) is therefore *not* tested here: a faked
   summary would measure my summarizer, not the mechanism. It stays an open
   question.
