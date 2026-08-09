"""Round-2 runner: score the lab mechanisms on the multi-workspace corpus.

Two metrics beyond round 1:
  leak   evidence set contains a chunk from another workspace
  wrong  the top-ranked item is a confident wrong answer rather than nothing

Usage:  python3 scripts/evaluate_lab.py [--seed N] [--out out/results_lab.json]
"""

from __future__ import annotations

import argparse
import json
import random
from dataclasses import asdict
from pathlib import Path

import corpus_scoped
from retrieval import Index
from strategies_lab import build_all

EVIDENCE_BUDGET = 400
QUERY_ORDER_SEED = 31337
WARMUP_SEED = 5150
WARMUP_PER_SESSION = 4
QTYPES = ["stable_recall", "scoped_current", "historical_value", "authored",
          "retraction", "redacted_historical", "poison"]
ABSTAIN_TYPES = {"retraction", "redacted_historical"}
FACT_KINDS = {"fact", "poison", "authored"}


def truncate(index: Index, ids: list[str], budget: int) -> list[str]:
    out, spent = [], 0
    for cid in ids:
        t = index.tok_count[cid]
        if spent + t > budget:
            break
        out.append(cid)
        spent += t
    return out


def score_query(index: Index, query: dict, retrieved: list[str]) -> dict[str, float]:
    qt = query["qtype"]
    gold, forbidden = query["gold"], set(query["forbidden"])
    stale = set(query["stale"])
    pos = {cid: i for i, cid in enumerate(retrieved)}
    res: dict[str, float] = {}

    out_of_scope = [c for c in retrieved
                    if index.by_id[c]["scope"] != query["scope"]]
    res["leak"] = 1.0 if out_of_scope else 0.0
    # Sharper: another workspace's *fact* reached the evidence set, not just its
    # chatter. This is the leak that can actually produce a wrong answer.
    res["leak_fact"] = 1.0 if any(
        index.by_id[c]["kind"] in FACT_KINDS for c in out_of_scope) else 0.0

    if qt in ABSTAIN_TYPES:
        res["score"] = 0.0 if (forbidden & set(retrieved)) else 1.0
        res["wrong"] = 1.0 - res["score"]
        return res

    # "Confidently wrong" = the top-ranked item is a fact that answers the
    # question asked, but with the wrong value: another workspace's fact, or a
    # different value for the same slot. A top-1 that is merely some unrelated
    # in-scope fact is a miss, not a wrong answer.
    top_wrong = 0.0
    if retrieved and gold:
        top = index.by_id[retrieved[0]]
        g = index.by_id[gold[0]]
        if top["kind"] in FACT_KINDS and retrieved[0] not in gold:
            same_question = (top["subject"] == g["subject"]
                             and top["predicate"] == g["predicate"])
            if same_question or top["scope"] != query["scope"]:
                top_wrong = 1.0
    res["wrong"] = top_wrong

    if qt == "poison":
        poisoned = bool(forbidden & set(retrieved))
        res["asr"] = 1.0 if poisoned else 0.0
        res["score"] = 1.0 if (not poisoned and all(g in pos for g in gold)) else 0.0
        return res

    got = all(g in pos for g in gold)
    if qt in ("scoped_current", "historical_value"):
        res["lenient"] = 1.0 if got else 0.0
        if not got:
            res["score"] = 0.0
        else:
            gpos = min(pos[g] for g in gold)
            res["score"] = 0.0 if any(pos.get(s, 10**9) < gpos for s in stale) else 1.0
        return res

    res["score"] = 1.0 if got else 0.0
    return res


def run(seed: int) -> dict:
    ch, qs = corpus_scoped.build(seed)
    chunks = [asdict(c) for c in ch]
    queries = [asdict(q) for q in qs]
    obj2pred = {o: p for p, cfg in corpus_scoped.PREDICATES.items()
                for o in cfg["objects"]}
    index = Index(chunks)

    order = list(range(len(queries)))
    random.Random(QUERY_ORDER_SEED).shuffle(order)

    # A real system is *used* while memory accumulates. Ingesting the whole
    # corpus before the first query leaves every recall counter at zero, which
    # silently collapses recall-driven eviction into plain LRU — so the corpus
    # is streamed by session with warm-up queries fired in between.
    by_session: dict[int, list[dict]] = {}
    for c in chunks:
        by_session.setdefault(c["session"], []).append(c)
    warm_rng = random.Random(WARMUP_SEED)
    warmups = {
        sess: [queries[i] for i in warm_rng.sample(range(len(queries)),
                                                   WARMUP_PER_SESSION)]
        for sess in sorted(by_session)
    }

    results = {}
    for strat in build_all(index, obj2pred, corpus_scoped.SUBJECTS):
        for sess in sorted(by_session):
            for c in by_session[sess]:
                strat.ingest(c)
            for q in warmups[sess]:
                strat.observe(q, strat.retrieve(q))

        acc = {mode: {qt: {"n": 0, "score": 0.0, "lenient": 0.0, "leak": 0.0,
                           "leak_fact": 0.0, "wrong": 0.0, "asr": 0.0}
                      for qt in QTYPES}
               for mode in ("natural", "budgeted")}
        tokens = 0

        for i in order:
            q = queries[i]
            got = strat.retrieve(q)
            strat.observe(q, got)
            tokens += sum(index.tok_count[c] for c in got)
            for mode, ids in (("natural", got),
                              ("budgeted", truncate(index, got, EVIDENCE_BUDGET))):
                s = score_query(index, q, ids)
                bucket = acc[mode][q["qtype"]]
                bucket["n"] += 1
                for key, val in s.items():
                    bucket[key] += val

        summary = {"strategy": strat.name, "modes": {}}
        for mode in ("natural", "budgeted"):
            per_type, leaks, wrongs, total = {}, 0.0, 0.0, 0
            for qt in QTYPES:
                b = acc[mode][qt]
                n = max(1, b["n"])
                per_type[qt] = {k: b[k] / n for k in
                                ("score", "lenient", "leak", "leak_fact",
                                 "wrong", "asr")}
                per_type[qt]["n"] = b["n"]
                leaks += b["leak"]
                wrongs += b["wrong"]
                total += b["n"]
            summary["modes"][mode] = {
                "composite": sum(per_type[qt]["score"] for qt in QTYPES) / len(QTYPES),
                "leak": leaks / max(1, total),
                "leak_fact": sum(acc[mode][qt]["leak_fact"] for qt in QTYPES)
                / max(1, total),
                "wrong": wrongs / max(1, total),
                "by_type": per_type,
            }
        summary["mean_tokens"] = tokens / len(queries)
        results[strat.name] = summary

    return {"seed": seed, "n_chunks": len(chunks), "n_queries": len(queries),
            "workspaces": corpus_scoped.WORKSPACES,
            "evidence_budget": EVIDENCE_BUDGET, "strategies": results}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=corpus_scoped.SEED)
    ap.add_argument("--out", default="out/results_lab.json")
    args = ap.parse_args()

    payload = run(args.seed)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=1), encoding="utf-8")

    rows = sorted(payload["strategies"].values(),
                  key=lambda r: -r["modes"]["budgeted"]["composite"])
    print(f"seed={payload['seed']} chunks={payload['n_chunks']} "
          f"queries={payload['n_queries']}")
    print(f"{'strategy':22}{'comp':>7}{'leak':>7}{'leakF':>7}{'wrong':>7}{'tok':>7}")
    for r in rows:
        m = r["modes"]["budgeted"]
        print(f"{r['strategy']:22}{m['composite']:7.3f}{m['leak']:7.2f}"
              f"{m['leak_fact']:7.2f}{m['wrong']:7.2f}{r['mean_tokens']:7.0f}")


if __name__ == "__main__":
    main()
