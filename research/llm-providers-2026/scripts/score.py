#!/usr/bin/env python3
"""Apply the hard filters, then rank the survivors under each weight profile.

The ranking is a function of the weights, and the weights are data. Edit
data/profiles.json and re-run to disagree with the result.

    python3 scripts/score.py                      # every profile
    python3 scripts/score.py --profile charter    # one profile
    python3 scripts/score.py --show-eliminated    # why each of the 50 failed
    python3 scripts/score.py --csv out/           # write per-profile CSVs
"""

import argparse
import csv
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DATA = ROOT / "data"


def load():
    j = lambda n: json.loads((DATA / n).read_text())
    return j("candidates.json"), j("profiles.json"), j("scores.json"), j("gates.json")


def weighted(scores, weights):
    """Weighted mean on the 0-5 scale, normalised to 0-5 so scores stay comparable."""
    num = sum(scores[c]["s"] * w for c, w in weights.items() if c in scores)
    den = sum(w for c, w in weights.items() if c in scores)
    return num / den if den else 0.0


def rank(scores, profile, tier=None):
    rows = []
    for cid, entry in scores["scores"].items():
        if tier and entry.get("tier") != tier:
            continue
        crit = {k: v for k, v in entry.items() if isinstance(v, dict)}
        rows.append((cid, entry.get("tier"), weighted(crit, profile["weights"])))
    rows.sort(key=lambda r: -r[2])
    return rows


def top3_stability(scores, profiles):
    """How often each option lands in the top 3 across all profiles.

    An option that wins under exactly one weighting is an artifact of that
    weighting. One that survives every weighting is a finding.
    """
    hits = {}
    for p in profiles["profiles"].values():
        for cid, _, _ in rank(scores, p)[:3]:
            hits[cid] = hits.get(cid, 0) + 1
    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile")
    ap.add_argument("--show-eliminated", action="store_true")
    ap.add_argument("--tier", choices=["A", "B"])
    ap.add_argument("--csv")
    args = ap.parse_args()

    candidates, profiles, scores, gates = load()

    if args.show_eliminated:
        elim = {k: v for k, v in gates["gates"].items() if v["fails"]}
        print(f"{len(elim)} of {len(gates['gates'])} candidates failed at least one hard filter\n")
        by_filter = {}
        for cid, g in elim.items():
            for f in g["fails"]:
                by_filter.setdefault(f, []).append(cid)
        for f, ids in sorted(by_filter.items(), key=lambda kv: -len(kv[1])):
            print(f"  {f} ({len(ids)}):")
            for cid in ids:
                print(f"      {cid:<26} {gates['gates'][cid]['reason']}")
            print()
        return

    names = [args.profile] if args.profile else list(profiles["profiles"])
    for name in names:
        if name not in profiles["profiles"]:
            sys.exit(f"unknown profile: {name}")
        p = profiles["profiles"][name]
        rows = rank(scores, p, tier=args.tier)
        print(f"\n=== {name} — {p['label']} ===")
        print(f"    {p['description']}\n")
        for i, (cid, tier, sc) in enumerate(rows, 1):
            print(f"  {i:>2}. [{tier}] {cid:<24} {sc:.2f}")

        if args.csv:
            out = pathlib.Path(args.csv)
            out.mkdir(parents=True, exist_ok=True)
            with (out / f"scores-{name}.csv").open("w", newline="") as fh:
                w = csv.writer(fh)
                crits = list(profiles["criteria"])
                w.writerow(["rank", "id", "tier", "weighted"] + crits)
                for i, (cid, tier, sc) in enumerate(rows, 1):
                    e = scores["scores"][cid]
                    w.writerow([i, cid, tier, f"{sc:.3f}"] + [e[c]["s"] for c in crits])

    if not args.profile:
        print("\n=== top-3 stability across all profiles ===")
        hits = top3_stability(scores, profiles)
        n = len(profiles["profiles"])
        for cid, c in sorted(hits.items(), key=lambda kv: -kv[1]):
            print(f"  {cid:<24} {c}/{n} profiles")


if __name__ == "__main__":
    main()
