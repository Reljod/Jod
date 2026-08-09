"""Re-run the whole comparison across several corpus seeds.

One seed produces a ranking; several show whether that ranking is a property of
the architectures or of the draw. Reports mean, spread, and best/worst rank per
strategy.

Usage:  python3 scripts/stability.py [--seeds 20260809 1 2 3] [--out out/stability.json]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import corpus as corpus_mod
from evaluate import run

DEFAULT_SEEDS = [corpus_mod.SEED, 424242, 909090, 5150]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, nargs="+", default=DEFAULT_SEEDS)
    ap.add_argument("--out", default="out/stability.json")
    args = ap.parse_args()

    per_seed = {}
    for seed in args.seeds:
        payload = run(seed)
        per_seed[str(seed)] = {
            name: r["modes"]["budgeted"]["composite"]
            for name, r in payload["strategies"].items()
        }
        print(f"seed {seed} done")

    names = sorted(next(iter(per_seed.values())))
    agg = {}
    for name in names:
        vals = [per_seed[str(s)][name] for s in args.seeds]
        ranks = []
        for s in args.seeds:
            row = per_seed[str(s)]
            order = sorted(row, key=lambda n: -row[n])
            ranks.append(order.index(name) + 1)
        agg[name] = {
            "mean": sum(vals) / len(vals),
            "min": min(vals),
            "max": max(vals),
            "best_rank": min(ranks),
            "worst_rank": max(ranks),
        }

    payload = {"seeds": args.seeds, "per_seed": per_seed, "aggregate": agg}
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=1), encoding="utf-8")

    print()
    print(f"{'strategy':22}{'mean':>8}{'min':>8}{'max':>8}{'rank':>10}")
    for name, a in sorted(agg.items(), key=lambda kv: -kv[1]["mean"]):
        rng = (f"{a['best_rank']}" if a["best_rank"] == a["worst_rank"]
               else f"{a['best_rank']}-{a['worst_rank']}")
        print(f"{name:22}{a['mean']:8.3f}{a['min']:8.3f}{a['max']:8.3f}{rng:>10}")


if __name__ == "__main__":
    main()
