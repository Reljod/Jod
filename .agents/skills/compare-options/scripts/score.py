#!/usr/bin/env python3
"""Rank a study's candidates against a weighted profile.

Usage:
    score.py <study-dir> --profile <name>
    score.py <study-dir> --profile jod --trials 20000 --json out/scores.json
    score.py <study-dir> --all-profiles
"""

import argparse
import csv
import json
import os
import sys

# Don't leave __pycache__ inside the skill (or the study) directory. A bundled
# skill may live on a read-only path, and the stray .pyc embeds absolute paths
# that make the skill look non-portable to tooling that greps it.
sys.dont_write_bytecode = True

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import comparelib as cl  # noqa: E402


def run_profile(study_dir, profile_name, trials, seed, top_n):
    dataset, profiles, mod = cl.load_study(study_dir)
    candidates = dataset["candidates"]
    ctx = cl.context(profiles)

    if profile_name not in profiles["profiles"]:
        raise SystemExit(
            f"unknown profile {profile_name!r}; have: {', '.join(profiles['profiles'])}"
        )

    profile = profiles["profiles"][profile_name]
    weights = profile["weights"]
    filters = profile.get("filters", {})
    uncertain = profiles.get("uncertain_field")

    eligible, rejected = [], []
    for c in candidates:
        m = cl.materialize(c, mod, ctx)
        ok, why = cl.passes_filters(m, filters)
        if ok:
            eligible.append(c)
        else:
            rejected.append((c, why))

    if not eligible:
        raise SystemExit("every candidate was filtered out — check the profile filters")

    rows = cl.score_all(eligible, mod, ctx, weights)
    stats = cl.monte_carlo(eligible, mod, ctx, weights, uncertain, trials, seed, top_n)

    ranking = []
    for r in rows:
        c = r["candidate"]
        ranking.append({
            "id": c["id"],
            "name": c.get("name", c["id"]),
            "confidence": c.get("confidence", "low"),
            "flags": c.get("flags", []),
            "subscores": {k: round(v, 1) for k, v in r["subscores"].items()},
            "total": round(r["total"], 2),
            "stability": {k: round(v, 1) for k, v in stats[c["id"]].items()},
            "display": {k: r["materialized"].get(k) for k in dataset.get("_display", [])},
        })

    return {
        "profile": profile_name,
        "profile_label": profile.get("label", profile_name),
        "weights": weights,
        "filters": filters,
        "trials": trials,
        "seed": seed,
        "counts": {"total": len(candidates), "eligible": len(eligible),
                   "rejected": len(rejected)},
        "rejected": [{"id": c["id"], "name": c.get("name", c["id"]), "reason": w}
                     for c, w in rejected],
        "ranking": ranking,
    }


def write_csv(result, path, criteria_names):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["rank", "id", "name", "total"] + criteria_names
                   + ["top_n_pct", "mean_rank", "confidence"])
        for i, r in enumerate(result["ranking"], 1):
            w.writerow([i, r["id"], r["name"], r["total"]]
                       + [r["subscores"].get(c, "") for c in criteria_names]
                       + [r["stability"]["top_n_pct"], r["stability"]["mean_rank"],
                          r["confidence"]])


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("study", help="study directory (contains data/ and criteria.py)")
    ap.add_argument("--profile", default=None)
    ap.add_argument("--all-profiles", action="store_true")
    ap.add_argument("--trials", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--top-n", type=int, default=5)
    ap.add_argument("--json", default=None)
    ap.add_argument("--csv", default=None)
    ap.add_argument("--limit", type=int, default=15)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    _, profiles, mod = cl.load_study(args.study)
    criteria_names = list(mod.CRITERIA)

    names = list(profiles["profiles"]) if args.all_profiles else [args.profile or "default"]

    for name in names:
        res = run_profile(args.study, name, args.trials, args.seed, args.top_n)

        if args.json:
            path = args.json if len(names) == 1 else args.json.replace(".json", f"-{name}.json")
            os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
            with open(path, "w") as fh:
                json.dump(res, fh, indent=2)

        if args.csv:
            path = args.csv if len(names) == 1 else args.csv.replace(".csv", f"-{name}.csv")
            write_csv(res, path, criteria_names)

        if args.quiet:
            continue

        print(f"\nprofile: {name}  ({res['profile_label']})")
        print(f"eligible: {res['counts']['eligible']} of {res['counts']['total']}"
              f"   trials: {args.trials:,}\n")
        print(f"{'#':>3}  {'candidate':<28} {'score':>6} {'top' + str(args.top_n) + '%':>7}  conf")
        print("-" * 56)
        for i, r in enumerate(res["ranking"][:args.limit], 1):
            print(f"{i:>3}  {r['name']:<28} {r['total']:>6.1f} "
                  f"{r['stability']['top_n_pct']:>6.1f}%  {r['confidence']}")

        if res["rejected"]:
            print(f"\nfiltered out ({len(res['rejected'])}):")
            for r in res["rejected"]:
                print(f"  - {r['name']}: {r['reason']}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
