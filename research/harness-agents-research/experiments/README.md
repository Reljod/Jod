# Memory architecture experiment

Seventeen memory strategies, one adversarial multi-session corpus, scored on the
evidence they put in front of the model.

```bash
bash scripts/run_all.sh      # ~29s, pure stdlib Python 3, no network, no API key
```

Outputs land in `out/`. Start with [`FINDINGS.md`](FINDINGS.md).

## Layout

| Path | What |
|---|---|
| [`HYPOTHESES.md`](HYPOTHESES.md) | Pre-registered predictions and declared limitations — written before the first run |
| [`FINDINGS.md`](FINDINGS.md) | Analysis, prediction scorecard, recommendations |
| `scripts/corpus.py` | Deterministic adversarial corpus generator |
| `scripts/retrieval.py` | BM25, Random Indexing dense channel, fusion, MMR, expansion |
| `scripts/strategies.py` | The seventeen architectures |
| `scripts/evaluate.py` | Scoring, both budgeted and natural |
| `scripts/sweep.py` | Fusion weight sweep on a held-out tuning seed |
| `scripts/sensitivity.py` | What `minScore` costs at different fusion scales |
| `scripts/stability.py` | Same comparison across four corpus seeds |
| `scripts/report.py` | Renders `out/RANKINGS.md` |
| `out/RANKINGS.md` | Generated tables — do not edit |
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

## Known limitations

The dense channel is Random Indexing rather than a neural embedder (no key, no
numpy available) — genuinely distributional but weaker than
`text-embedding-3-small`, so fusion-weight findings are soft while control-plane
findings are firm. The corpus is synthetic. Extraction cost is simulated, not
billed. Full detail in [`HYPOTHESES.md`](HYPOTHESES.md).
