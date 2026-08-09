"""Round-2 sweeps: the security scaling test (P20) and seed stability.

P20 asks whether "0% attack success" without a defence is real security or just
weak retrieval. Round 1 found most strategies scored 0% ASR because the attacker
chunk was outranked, never because anything rejected it. If that's the
explanation, widening `k` should make the protection evaporate — while
write-time admission stays at 0 regardless.

Usage:  python3 scripts/sweep_lab.py [--out out/sweep_lab.json]
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

import corpus_scoped
from evaluate_lab import run as run_eval
from retrieval import Index
from strategies_lab import ControlScoped, HybridScopeFilter, LabExtractor

K_VALUES = [4, 8, 16, 32, 64]
STABILITY_SEEDS = [corpus_scoped.SEED, 606060, 771177]


def asr_at_k(index: Index, chunks: list[dict], queries: list[dict],
             obj2pred: dict[str, str], k: int) -> dict[str, float]:
    """Attack success rate on poisoned queries at a given evidence width."""
    poison_qs = [q for q in queries if q["qtype"] == "poison"]
    out = {}
    for cls, needs_ex in ((HybridScopeFilter, False), (ControlScoped, True)):
        strat = cls(index, k=k,
                    extractor=LabExtractor(obj2pred, corpus_scoped.SUBJECTS)
                    if needs_ex else None)
        for c in chunks:
            strat.ingest(c)
        hits = 0
        for q in poison_qs:
            got = strat.retrieve(q)
            if set(q["forbidden"]) & set(got):
                hits += 1
        out[cls.name] = hits / max(1, len(poison_qs))
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="out/sweep_lab.json")
    args = ap.parse_args()

    ch, qs = corpus_scoped.build(corpus_scoped.SEED)
    chunks = [asdict(c) for c in ch]
    queries = [asdict(q) for q in qs]
    obj2pred = {o: p for p, cfg in corpus_scoped.PREDICATES.items()
                for o in cfg["objects"]}
    index = Index(chunks)

    security = {str(k): asr_at_k(index, chunks, queries, obj2pred, k)
                for k in K_VALUES}

    print("attack success rate vs evidence width")
    print(f"{'k':>5}{'no admission':>16}{'write-time admission':>24}")
    for k in K_VALUES:
        row = security[str(k)]
        print(f"{k:5}{row['hybrid_scope_filter']:16.2f}"
              f"{row['control_scoped']:24.2f}")

    per_seed = {}
    for seed in STABILITY_SEEDS:
        payload = run_eval(seed)
        per_seed[str(seed)] = {
            name: r["modes"]["budgeted"]["composite"]
            for name, r in payload["strategies"].items()
        }
        print(f"seed {seed} done")

    names = sorted(next(iter(per_seed.values())))
    agg = {}
    for name in names:
        vals = [per_seed[str(s)][name] for s in STABILITY_SEEDS]
        ranks = []
        for s in STABILITY_SEEDS:
            row = per_seed[str(s)]
            ranks.append(sorted(row, key=lambda n: -row[n]).index(name) + 1)
        agg[name] = {"mean": sum(vals) / len(vals), "min": min(vals),
                     "max": max(vals), "best_rank": min(ranks),
                     "worst_rank": max(ranks)}

    payload = {"k_values": K_VALUES, "security": security,
               "seeds": STABILITY_SEEDS, "per_seed": per_seed, "aggregate": agg}
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=1), encoding="utf-8")

    print()
    print(f"{'strategy':22}{'mean':>8}{'min':>8}{'max':>8}{'rank':>10}")
    for name, a in sorted(agg.items(), key=lambda kv: -kv[1]["mean"]):
        rank = (str(a["best_rank"]) if a["best_rank"] == a["worst_rank"]
                else f"{a['best_rank']}-{a['worst_rank']}")
        print(f"{name:22}{a['mean']:8.3f}{a['min']:8.3f}{a['max']:8.3f}{rank:>10}")


if __name__ == "__main__":
    main()
