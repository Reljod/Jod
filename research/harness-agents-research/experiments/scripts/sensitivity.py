"""What does OpenClaw's minScore=0.35 floor cost when the embedder disagrees?

The floor is a fixed number compared against a fused score whose scale depends
entirely on the embedder's cosine distribution. Neural embedders sit higher than
Random Indexing, so the same constant prunes far more aggressively here. This
quantifies that, and is why the main run sets the floor to 0.

Usage:  python3 scripts/sensitivity.py [--out out/sensitivity.json]
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

import corpus as corpus_mod
from evaluate import EVIDENCE_BUDGET, QTYPES, score_query, truncate
from retrieval import Index
from strategies import Hybrid, HybridRRF, HybridTuned

FLOORS = [0.0, 0.1, 0.2, 0.35, 0.5]


def measure(index: Index, chunks: list[dict], queries: list[dict],
            cls, floor: float) -> tuple[float, float]:
    strat = cls(index)
    strat.min_score = floor
    for c in chunks:
        strat.ingest(c)
    acc = {qt: [0, 0.0] for qt in QTYPES}
    returned = 0
    for q in queries:
        ids = strat.retrieve(q)
        returned += len(ids)
        s = score_query(q, truncate(index, ids, EVIDENCE_BUDGET))
        acc[q["qtype"]][0] += 1
        acc[q["qtype"]][1] += s.get("score", 0.0)
    composite = sum(v[1] / max(1, v[0]) for v in acc.values()) / len(QTYPES)
    return composite, returned / len(queries)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="out/sensitivity.json")
    args = ap.parse_args()

    ch, qs = corpus_mod.build(corpus_mod.SEED)
    chunks = [asdict(c) for c in ch]
    queries = [asdict(q) for q in qs]
    index = Index(chunks)

    table = {}
    for cls in (Hybrid, HybridTuned, HybridRRF):
        row = {}
        for floor in FLOORS:
            comp, mean_n = measure(index, chunks, queries, cls, floor)
            row[f"{floor:.2f}"] = {"composite": comp, "mean_results": mean_n}
        table[cls.name] = row

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"floors": FLOORS, "table": table}, indent=1),
                   encoding="utf-8")

    print(f"{'strategy':18}" + "".join(f"{f:>16.2f}" for f in FLOORS))
    for name, row in table.items():
        cells = "".join(
            f"{row[f'{f:.2f}']['composite']:8.3f}/{row[f'{f:.2f}']['mean_results']:6.1f}"
            for f in FLOORS)
        print(f"{name:18}{cells}")
    print("cells are composite / mean results returned per query")


if __name__ == "__main__":
    main()
