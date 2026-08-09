"""Fusion sweeps, run on a tuning seed that the headline table never uses.

Two questions:

  1. Is 0.7 / 0.3 the right vector:text split, or is it a constant that only
     holds for the embedder it was tuned against?
  2. How much does the linear-vs-RRF scale mismatch actually cost?

Tuning happens on TUNE_SEED. evaluate.py reports on corpus.SEED. Keeping those
apart is the difference between measuring an architecture and fitting one.

Usage:  python3 scripts/sweep.py [--out out/sweep.json]
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

import corpus as corpus_mod
from evaluate import QTYPES, EVIDENCE_BUDGET, score_query, truncate
from retrieval import Index
from strategies import Hybrid

TUNE_SEED = 1234
WEIGHTS = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]


class WeightedHybrid(Hybrid):
    def __init__(self, index: Index, vector_weight: float, mode: str) -> None:
        super().__init__(index)
        self.vw = vector_weight
        self.fusion = mode
        self.name = f"{mode}_vw{vector_weight:.1f}"

    def _scored(self, query):
        from retrieval import fuse, importance_multiplier
        eligible = set(self.seen)
        ranked = fuse(self.index, query["id"], query["text"], eligible,
                      vector_weight=self.vw, text_weight=1.0 - self.vw,
                      mode=self.fusion)
        out = []
        for cid, score in ranked:
            c = self.index.by_id[cid]
            out.append((cid, score * importance_multiplier(c.get("importance"))))
        out.sort(key=lambda x: (-x[1], x[0]))
        return out


def composite_for(index: Index, queries: list[dict], chunks: list[dict],
                  vw: float, mode: str) -> float:
    strat = WeightedHybrid(index, vw, mode)
    for c in chunks:
        strat.ingest(c)
    acc = {qt: [0, 0.0] for qt in QTYPES}
    for q in queries:
        ids = truncate(index, strat.retrieve(q), EVIDENCE_BUDGET)
        s = score_query(q, ids)
        acc[q["qtype"]][0] += 1
        acc[q["qtype"]][1] += s.get("score", 0.0)
    return sum(v[1] / max(1, v[0]) for v in acc.values()) / len(QTYPES)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="out/sweep.json")
    args = ap.parse_args()

    ch, qs = corpus_mod.build(TUNE_SEED)
    chunks = [asdict(c) for c in ch]
    queries = [asdict(q) for q in qs]
    index = Index(chunks)

    table = {}
    for mode in ("linear", "rrf"):
        row = {}
        for vw in WEIGHTS:
            row[f"{vw:.1f}"] = composite_for(index, queries, chunks, vw, mode)
        table[mode] = row

    best = {
        mode: max(row.items(), key=lambda kv: kv[1])
        for mode, row in table.items()
    }
    payload = {"tune_seed": TUNE_SEED, "table": table,
               "best": {m: {"vector_weight": float(k), "composite": v}
                        for m, (k, v) in best.items()}}
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=1), encoding="utf-8")

    print(f"tuning seed={TUNE_SEED}  (headline table uses seed={corpus_mod.SEED})")
    print(f"{'vec weight':>11} {'linear':>9} {'rrf':>9}")
    for vw in WEIGHTS:
        k = f"{vw:.1f}"
        print(f"{k:>11} {table['linear'][k]:9.3f} {table['rrf'][k]:9.3f}")
    for mode, (k, v) in best.items():
        print(f"best {mode:7} vector_weight={k} composite={v:.3f}")


if __name__ == "__main__":
    main()
