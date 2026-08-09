#!/usr/bin/env python3
"""Apply hard filters, then weighted scoring, then a sensitivity check.

    python3 scripts/score.py                  # every profile
    python3 scripts/score.py --profile simplicity
    python3 scripts/score.py --show-eliminated

Standard library only.
"""

import argparse
import csv
import json
import os
import random

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DATA = os.path.join(ROOT, "data")
OUT = os.path.join(ROOT, "out")

GATE_KEYS = {
    "mpw": "multi_process_write",
    "mnt": "maintained",
    "shs": "self_hostable",
    "fit": "fits_small_vps",
    "sor": "store_of_record",
}


def load(name):
    with open(os.path.join(DATA, name)) as f:
        return json.load(f)


def survivors(candidates, gates):
    kept, dropped = [], []
    for c in candidates:
        g = gates.get(c["id"])
        if g is None:
            dropped.append((c, "no gate verdict recorded"))
            continue
        failed = [GATE_KEYS[k] for k in GATE_KEYS if not g.get(k, True)]
        if failed:
            dropped.append((c, "fails " + ", ".join(failed) + " — " + g.get("reason", "")))
        else:
            kept.append(c)
    return kept, dropped


def weighted(scores, weights):
    num = sum(scores.get(k, 0) * w for k, w in weights.items())
    den = sum(weights.values())
    return num / den if den else 0.0


def rank(kept, scores, weights):
    rows = []
    for c in kept:
        s = scores.get(c["id"])
        if not s:
            continue
        rows.append({"id": c["id"], "name": c["name"], "score": weighted(s["criteria"], weights), "criteria": s["criteria"]})
    rows.sort(key=lambda r: -r["score"])
    return rows


def sensitivity(kept, scores, weights, trials=2000, seed=7):
    """How often does each option land in the top 3 when the weights and the
    judged scores are perturbed? A winner that only wins under one exact
    weighting is an artifact, not a recommendation."""
    rng = random.Random(seed)
    top3 = {}
    for _ in range(trials):
        w = {k: max(0.0, v + rng.uniform(-1, 1)) for k, v in weights.items()}
        rows = []
        for c in kept:
            s = scores.get(c["id"])
            if not s:
                continue
            measured = set(s.get("measured", []))
            jitter = {
                k: v + (0 if k in measured else rng.uniform(-0.5, 0.5))
                for k, v in s["criteria"].items()
            }
            rows.append((weighted(jitter, w), c["id"]))
        rows.sort(reverse=True)
        for _, cid in rows[:3]:
            top3[cid] = top3.get(cid, 0) + 1
    return {k: round(100 * v / trials, 1) for k, v in top3.items()}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile")
    ap.add_argument("--show-eliminated", action="store_true")
    args = ap.parse_args()

    candidates = load("candidates.json")["candidates"]
    gates = load("gates.json")["gates"]
    profiles = load("profiles.json")
    scores = load("scores.json")["scores"]

    kept, dropped = survivors(candidates, gates)
    print(f"{len(candidates)} candidates -> {len(kept)} survive the hard filters, {len(dropped)} eliminated\n")

    if args.show_eliminated:
        for c, why in dropped:
            print(f"  ✗ {c['name']:<44} {why[:110]}")
        print()

    unscored = [c["id"] for c in kept if c["id"] not in scores]
    if unscored:
        print(f"  ! survivors with no score record: {', '.join(unscored)}\n")

    os.makedirs(OUT, exist_ok=True)
    names = [args.profile] if args.profile else list(profiles["profiles"])
    for pname in names:
        prof = profiles["profiles"][pname]
        rows = rank(kept, scores, prof["weights"])
        stab = sensitivity(kept, scores, prof["weights"])
        print(f"== {pname}: {prof['label']}")
        print(f"   {prof['description']}")
        for i, r in enumerate(rows, 1):
            print(f"   {i:>2}. {r['name']:<42} {r['score']:.2f}   top-3 in {stab.get(r['id'], 0.0):>5.1f}% of perturbations")
        print()

        with open(os.path.join(OUT, f"scores-{pname}.csv"), "w", newline="") as f:
            crit = list(profiles["criteria"])
            wr = csv.writer(f)
            wr.writerow(["rank", "id", "name", "score", "top3_stability_pct"] + crit)
            for i, r in enumerate(rows, 1):
                wr.writerow(
                    [i, r["id"], r["name"], f"{r['score']:.3f}", stab.get(r["id"], 0.0)]
                    + [r["criteria"].get(c, "") for c in crit]
                )


if __name__ == "__main__":
    main()
