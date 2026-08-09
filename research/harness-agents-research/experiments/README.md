# Memory architecture experiment

Thirty-one memory strategies across two rounds, scored on the evidence they put
in front of the model rather than on an LLM-judged answer.

**Round 1** tests the open-source designs (Hermes, OpenClaw, graph expansion) on
an adversarial single-tenant corpus. **Round 2** tests what the big labs
actually ship — scope partitioning, Codex-style grep recall, eviction policy,
rewrite-vs-append, an authored tier, abstention, redaction, write-time admission
— on a corpus with two workspaces that share project names.

```bash
bash scripts/run_all.sh      # ~45s, pure stdlib Python 3, no network, no API key
```

Outputs land in `out/`. Start with [`FINDINGS-2.md`](FINDINGS-2.md) — it carries
the combined conclusion from both rounds.

## Layout

| Path | What |
|---|---|
| [`HYPOTHESES.md`](HYPOTHESES.md) | Round 1 — pre-registered predictions and declared limitations, written before the first run |
| [`FINDINGS.md`](FINDINGS.md) | Round 1 — analysis, prediction scorecard, recommendations |
| [`HYPOTHESES-2.md`](HYPOTHESES-2.md) | Round 2 — eleven pre-registered predictions on the mechanisms the big labs ship |
| [`FINDINGS-2.md`](FINDINGS-2.md) | Round 2 — results, scorecard, and the combined conclusion from both rounds |
| `scripts/corpus.py` | Deterministic adversarial corpus generator |
| `scripts/retrieval.py` | BM25, Random Indexing dense channel, fusion, MMR, expansion |
| `scripts/strategies.py` | The seventeen architectures |
| `scripts/evaluate.py` | Scoring, both budgeted and natural |
| `scripts/sweep.py` | Fusion weight sweep on a held-out tuning seed |
| `scripts/sensitivity.py` | What `minScore` costs at different fusion scales |
| `scripts/stability.py` | Same comparison across four corpus seeds |
| `scripts/report.py` | Renders `out/RANKINGS.md` |
| `scripts/corpus_scoped.py` | Round 2 — two workspaces, authored tier, withdrawn-fact history |
| `scripts/strategies_lab.py` | Round 2 — the fourteen lab-mechanism architectures |
| `scripts/evaluate_lab.py` | Round 2 — scoring, plus cross-workspace leak and confident-wrong metrics |
| `scripts/sweep_lab.py` | Round 2 — attack-success-vs-evidence-width sweep and seed stability |
| `scripts/report_lab.py` | Renders `out/RANKINGS-2.md` |
| `out/RANKINGS.md`, `out/RANKINGS-2.md` | Generated tables — do not edit |
| `data/` | Generated corpus, regenerable in one second, git-ignored |

## Design decisions worth knowing

**Scoring is on retrieved evidence, not on generated answers.** No LLM judge, no
API key, fully deterministic. This is also what the 2026 benchmark critiques ask
for: LongMemEval grades downstream answers, which lets a system look good while
its retrieval failed and the model recovered anyway.

**Six query types, each isolating one capability** — long-distance recall,
current value under supersession, historical value at a date, multi-hop,
retraction (where the right answer is *nothing*), and untrusted-source
poisoning. The last one is scored by no mainstream memory benchmark despite
memory poisoning being OWASP ASI06 in the 2026 Agentic AI Top 10.

**Two scoring passes.** `budgeted` truncates everyone to the same 400-token
evidence budget; `natural` lets each spend what it wants. Reporting only the
first would cripple long-context; only the second would ignore that it costs
314× more.

**Structured strategies pay an extraction tax.** Anything maintaining a fact
store parses prose through `Extractor`, simulated at 90% recall and 5% false
positives — so they inherit realistic extraction damage instead of ground truth.
Filler misread as fact really does pollute their stores.

**Tuning and evaluation use different seeds.** `sweep.py` tunes on seed 1234;
every reported number comes from seed 20260809 or the stability sweep.

**Both channels are computed once and shared.** Every strategy sees identical
BM25 and dense rankings, so differences come from architecture, not jitter.

**Byte-reproducible.** Every score sort carries a secondary key on the chunk id.
Without it, equal scores resolved by set-iteration order — which Python
randomises per process — so token counts drifted between otherwise identical
runs. Two consecutive `run_all.sh` invocations now produce byte-identical
`out/*.json`.

## Known limitations

The dense channel is Random Indexing rather than a neural embedder (no key, no
numpy available) — genuinely distributional but weaker than
`text-embedding-3-small`, so fusion-weight findings are soft while control-plane
findings are firm. The corpus is synthetic. Extraction cost is simulated, not
billed. Full detail in [`HYPOTHESES.md`](HYPOTHESES.md).
