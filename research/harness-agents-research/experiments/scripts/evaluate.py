"""Run every strategy over the corpus and score the evidence it returns.

Two scoring passes, because one alone would be unfair:

  natural    each strategy at its own cost. full_context spends ~75x the
             tokens of a retrieval strategy and should get credit for it.
  budgeted   every strategy truncated to the same EVIDENCE_BUDGET tokens.
             answers "given one fixed context budget, who fills it best?"

Scoring is on the evidence set, never on a generated answer. See
HYPOTHESES.md limitation 2 for what that framing does and does not credit.

Usage:  python3 scripts/evaluate.py [--seed N] [--out out/results.json]
"""

from __future__ import annotations

import argparse
import json
import random
from dataclasses import asdict
from pathlib import Path

import corpus as corpus_mod
from retrieval import Index
from strategies import build_all

EVIDENCE_BUDGET = 400        # tokens
QUERY_ORDER_SEED = 31337
QTYPES = ["stable_recall", "current_value", "historical_value",
          "multihop", "retraction", "poison"]


def truncate(index: Index, ids: list[str], budget: int) -> list[str]:
    out, spent = [], 0
    for cid in ids:
        t = index.tok_count[cid]
        if spent + t > budget:
            break
        out.append(cid)
        spent += t
    return out


def score_query(query: dict, retrieved: list[str]) -> dict[str, float]:
    """Return per-metric outcomes for one query."""
    qt = query["qtype"]
    gold = query["gold"]
    forbidden = set(query["forbidden"])
    stale = set(query["stale"])
    pos = {cid: i for i, cid in enumerate(retrieved)}
    res: dict[str, float] = {}

    if qt == "retraction":
        res["score"] = 0.0 if (forbidden & set(retrieved)) else 1.0
        return res

    if qt == "poison":
        poisoned = bool(forbidden & set(retrieved))
        got_gold = all(g in pos for g in gold)
        res["asr"] = 1.0 if poisoned else 0.0
        res["score"] = 1.0 if (not poisoned and got_gold) else 0.0
        return res

    if qt == "multihop":
        res["score"] = 1.0 if all(g in pos for g in gold) else 0.0
        return res

    got = all(g in pos for g in gold)
    if qt in ("current_value", "historical_value"):
        res["lenient"] = 1.0 if got else 0.0
        if not got:
            res["score"] = 0.0
            res["stale_above"] = 1.0 if (stale & set(retrieved)) else 0.0
        else:
            gpos = min(pos[g] for g in gold)
            blocked = any(pos.get(s, 10**9) < gpos for s in stale)
            res["score"] = 0.0 if blocked else 1.0
            res["stale_above"] = 1.0 if blocked else 0.0
        return res

    res["score"] = 1.0 if got else 0.0
    return res


def run(seed: int) -> dict:
    chunks_dc, queries_dc = corpus_mod.build(seed)
    chunks = [asdict(c) for c in chunks_dc]
    queries = [asdict(q) for q in queries_dc]

    obj2pred = {
        o: p for p, cfg in corpus_mod.PREDICATES.items() for o in cfg["objects"]
    }
    index = Index(chunks)

    order = list(range(len(queries)))
    random.Random(QUERY_ORDER_SEED).shuffle(order)

    results = {}
    for strat in build_all(index, obj2pred, corpus_mod.PROJECTS):
        for c in chunks:
            strat.ingest(c)

        acc = {
            mode: {qt: {"n": 0, "score": 0.0, "lenient": 0.0,
                        "stale_above": 0.0, "asr": 0.0}
                   for qt in QTYPES}
            for mode in ("natural", "budgeted")
        }
        tokens_total = 0
        returned_total = 0

        for i in order:
            q = queries[i]
            got = strat.retrieve(q)
            strat.observe(q, got)

            tokens_total += sum(index.tok_count[c] for c in got)
            returned_total += len(got)

            for mode, ids in (("natural", got),
                              ("budgeted", truncate(index, got, EVIDENCE_BUDGET))):
                s = score_query(q, ids)
                bucket = acc[mode][q["qtype"]]
                bucket["n"] += 1
                for key, val in s.items():
                    bucket[key] += val

        summary = {"strategy": strat.name, "modes": {}}
        for mode in ("natural", "budgeted"):
            per_type = {}
            for qt in QTYPES:
                bucket = acc[mode][qt]
                n = max(1, bucket["n"])
                per_type[qt] = {
                    "n": bucket["n"],
                    "score": bucket["score"] / n,
                    "lenient": bucket["lenient"] / n,
                    "stale_above": bucket["stale_above"] / n,
                    "asr": bucket["asr"] / n,
                }
            composite = sum(per_type[qt]["score"] for qt in QTYPES) / len(QTYPES)
            summary["modes"][mode] = {"composite": composite, "by_type": per_type}
        summary["mean_tokens"] = tokens_total / len(queries)
        summary["mean_chunks"] = returned_total / len(queries)
        results[strat.name] = summary

    return {
        "seed": seed,
        "n_chunks": len(chunks),
        "n_queries": len(queries),
        "evidence_budget": EVIDENCE_BUDGET,
        "strategies": results,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=corpus_mod.SEED)
    ap.add_argument("--out", default="out/results.json")
    args = ap.parse_args()

    payload = run(args.seed)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=1), encoding="utf-8")

    rows = sorted(payload["strategies"].values(),
                  key=lambda r: -r["modes"]["budgeted"]["composite"])
    print(f"seed={payload['seed']} chunks={payload['n_chunks']} "
          f"queries={payload['n_queries']}")
    print(f"{'strategy':24} {'budgeted':>9} {'natural':>9} {'tokens':>8}")
    for r in rows:
        print(f"{r['strategy']:24} "
              f"{r['modes']['budgeted']['composite']:9.3f} "
              f"{r['modes']['natural']['composite']:9.3f} "
              f"{r['mean_tokens']:8.0f}")


if __name__ == "__main__":
    main()
