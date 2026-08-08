#!/usr/bin/env python3
"""Score and rank VPS providers against a weighted profile.

Two things make this more than a spreadsheet:

1. Hard filters run *before* scoring. A provider that cannot run Docker is not
   a low-scoring option, it is not an option. Weighted models love to let a
   great price paper over a disqualifying flaw; filters stop that.

2. A Monte Carlo pass perturbs both the weights *and* the prices, then re-ranks.
   Price perturbation is scaled by each row's `confidence`, so a cheap provider
   whose price I could not verify has to survive being wrong about the price
   before it is allowed to win. The stability number in the report is the
   fraction of trials where a provider held a top-5 slot.

Stdlib only. No install step.

Usage:
    python3 score.py --profile jod
    python3 score.py --profile cheapest --trials 20000
    python3 score.py --profile jod --json out/scores.json
"""

import argparse
import csv
import json
import math
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DATA = os.path.join(ROOT, "data")

# How far a price is allowed to be wrong, by how much I trust the row.
# These feed the Monte Carlo; they are the honest cost of unverified data.
CONFIDENCE_PRICE_ERROR = {"high": 0.04, "medium": 0.12, "low": 0.25}

DMCA_SCORE = {"ignored": 100.0, "forwarded": 75.0, "responsive": 40.0, "strict": 10.0}
DISK_SCORE = {"NVMe": 100.0, "SSD": 70.0, "HDD": 20.0}

UNMETERED_TB = 9999


# ---------------------------------------------------------------- helpers


def clamp(x, lo=0.0, hi=100.0):
    return max(lo, min(hi, x))


def scale_1_5(v, invert=False):
    """Map a 1-5 rating onto 0-100. invert=True means 1 is best."""
    v = max(1, min(5, v))
    frac = (5 - v) / 4.0 if invert else (v - 1) / 4.0
    return frac * 100.0


def log_norm(value, lo, hi):
    """Normalize onto 0-100 on a log scale. Compresses long tails so that a
    40-location provider does not make a 15-location provider look like zero."""
    value = max(value, lo)
    if hi <= lo:
        return 100.0
    return clamp(100.0 * (math.log(value) - math.log(lo)) / (math.log(hi) - math.log(lo)))


def log_norm_inverse(value, lo, hi):
    """Same, but lower is better (used for price)."""
    return 100.0 - log_norm(value, lo, hi)


def usd_price(p, fx):
    rate = fx.get(p["currency"])
    if rate is None:
        raise SystemExit(f"{p['id']}: unknown currency {p['currency']!r}")
    return p["price"] * rate


# ---------------------------------------------------------------- filters


def passes_filters(p, f):
    """Return (ok, reason). Reason is the first disqualifier hit."""
    if p["ram_gb"] < f.get("min_ram_gb", 0):
        return False, f"below RAM floor ({p['ram_gb']} GB)"
    if p["vcpu"] < f.get("min_vcpu", 0):
        return False, f"below vCPU floor ({p['vcpu']})"
    if p["disk_gb"] < f.get("min_disk_gb", 0):
        return False, f"below disk floor ({p['disk_gb']} GB)"

    allowed_virt = f.get("require_virt")
    if allowed_virt and p["virt"] not in allowed_virt:
        return False, f"virtualization {p['virt']} cannot run the workload"

    if f.get("require_docker") and not p.get("docker_ok"):
        return False, "no Docker support"

    for prefix in f.get("exclude_flags", []):
        for flag in p.get("flags", []):
            if flag.startswith(prefix):
                return False, f"flagged {flag}"

    return True, ""


# ---------------------------------------------------------------- criteria


def score_cost(p, fx, price_override=None):
    """Blend absolute monthly price with price-per-GB-RAM.

    Absolute price answers 'what leaves my account'. Price-per-GB answers 'what
    do I get for it' — without that term, a provider handing you 8 GB for $5
    looks identical to one handing you 4 GB for $5.
    """
    usd = price_override if price_override is not None else usd_price(p, fx)
    usd = max(usd, 0.5)  # floor keeps log finite for free tiers

    absolute = log_norm_inverse(usd, 2.0, 120.0)
    per_gb = log_norm_inverse(usd / max(p["ram_gb"], 1), 0.3, 10.0)
    return 0.6 * absolute + 0.4 * per_gb


def score_permissiveness(p):
    """'Free to do anything.' The AUP is what actually gets you suspended, so it
    carries the most weight; KYC is what stops you signing up at all."""
    aup = scale_1_5(p["aup_strict"], invert=True)
    kyc = scale_1_5(p["kyc"], invert=True)
    dmca = DMCA_SCORE.get(p["dmca"], 40.0)
    crypto = 100.0 if p.get("crypto") else 0.0
    return 0.45 * aup + 0.25 * kyc + 0.20 * dmca + 0.10 * crypto


def score_availability(p):
    """SLA is a promise; reputation is the observed truth. Reputation outweighs
    the contract, because SLA credits do not bring a dead orchestrator back."""
    sla = p.get("sla_pct", 0.0)
    if sla <= 0:
        sla_score = 0.0
    elif sla >= 99.9999:
        sla_score = 100.0
    else:
        nines = -math.log10(1.0 - sla / 100.0)
        sla_score = clamp(nines / 5.0 * 100.0)

    rep = scale_1_5(p["uptime_rep"])
    steal = scale_1_5(p["steal_risk"], invert=True)
    return 0.35 * sla_score + 0.45 * rep + 0.20 * steal


def score_network(p):
    """IP reputation carries the largest share here.

    For an agent that browses the web, a blocklisted IP range is a harder
    failure than a slow port: the box is up, the bandwidth is there, and every
    request still returns a CAPTCHA. Raw throughput cannot fix that.
    """
    ip = scale_1_5(p["ip_rep"], invert=True)
    quality = scale_1_5(p["net_quality"])
    tb = p.get("traffic_tb", 0)
    traffic = 100.0 if tb >= UNMETERED_TB else log_norm(max(tb, 0.05), 0.05, 40.0)
    port = log_norm(p.get("port_gbps", 1), 0.1, 10.0)
    return 0.35 * ip + 0.30 * quality + 0.20 * traffic + 0.15 * port


def score_locations(p):
    """Count on a log scale, plus continent breadth. Twenty datacenters across
    one continent is worse global reach than eight spread over four."""
    count = log_norm(max(p.get("loc_count", 1), 1), 1, 40)
    breadth = len(set(p.get("regions", []))) / 7.0 * 100.0
    return 0.6 * count + 0.4 * clamp(breadth)


def score_performance(p):
    cpu = scale_1_5(p["cpu_perf"])
    disk = DISK_SCORE.get(p.get("disk_type"), 50.0)
    steal = scale_1_5(p["steal_risk"], invert=True)
    return 0.5 * cpu + 0.3 * disk + 0.2 * steal


def score_ops(p):
    bits = [
        (p.get("api"), 0.30),
        (p.get("snapshots"), 0.25),
        (p.get("hourly"), 0.15),
        (p.get("ipv4_incl"), 0.20),
        (p.get("ipv6"), 0.10),
    ]
    return sum(100.0 * w for ok, w in bits if ok)


CRITERIA = {
    "cost": score_cost,
    "permissiveness": score_permissiveness,
    "availability": score_availability,
    "network": score_network,
    "locations": score_locations,
    "performance": score_performance,
    "ops": score_ops,
}


def subscores(p, fx, price_override=None):
    out = {}
    for name, fn in CRITERIA.items():
        out[name] = fn(p, fx, price_override) if name == "cost" else fn(p)
    return out


def weighted_total(subs, weights):
    total_w = sum(weights.values())
    if total_w <= 0:
        return 0.0
    return sum(subs[k] * w for k, w in weights.items() if k in subs) / total_w


# ---------------------------------------------------------------- monte carlo


def monte_carlo(candidates, fx, weights, trials, seed, top_n=5):
    """Perturb weights and prices; count how often each provider holds a top-N slot.

    A provider that only wins under one exact set of weights is not a
    recommendation, it is an artifact of my weighting. This separates the two.
    """
    rng = random.Random(seed)
    ids = [p["id"] for p in candidates]
    top_hits = {i: 0 for i in ids}
    rank_sum = {i: 0 for i in ids}
    win_hits = {i: 0 for i in ids}

    base_prices = {p["id"]: usd_price(p, fx) for p in candidates}
    errs = {
        p["id"]: CONFIDENCE_PRICE_ERROR.get(p.get("confidence", "low"), 0.25)
        for p in candidates
    }

    for _ in range(trials):
        w = {k: v * rng.uniform(0.65, 1.35) for k, v in weights.items()}

        scored = []
        for p in candidates:
            err = errs[p["id"]]
            price = base_prices[p["id"]] * rng.uniform(1 - err, 1 + err)
            subs = subscores(p, fx, price_override=price)
            scored.append((weighted_total(subs, w), p["id"]))

        scored.sort(reverse=True)
        for rank, (_, pid) in enumerate(scored, start=1):
            rank_sum[pid] += rank
            if rank <= top_n:
                top_hits[pid] += 1
            if rank == 1:
                win_hits[pid] += 1

    return {
        pid: {
            "top_n_pct": 100.0 * top_hits[pid] / trials,
            "win_pct": 100.0 * win_hits[pid] / trials,
            "mean_rank": rank_sum[pid] / trials,
        }
        for pid in ids
    }


# ---------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description="Rank VPS providers against a weighted profile.")
    ap.add_argument("--profile", default="jod")
    ap.add_argument("--providers", default=os.path.join(DATA, "providers.json"))
    ap.add_argument("--profiles", default=os.path.join(DATA, "profiles.json"))
    ap.add_argument("--trials", type=int, default=5000)
    ap.add_argument("--seed", type=int, default=20260808)
    ap.add_argument("--top-n", type=int, default=5)
    ap.add_argument("--json", default=None, help="write full results here")
    ap.add_argument("--csv", default=None, help="write the ranked table here")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    with open(args.providers) as fh:
        providers = json.load(fh)["providers"]
    with open(args.profiles) as fh:
        pdata = json.load(fh)

    if args.profile not in pdata["profiles"]:
        raise SystemExit(
            f"unknown profile {args.profile!r}; have: {', '.join(pdata['profiles'])}"
        )

    profile = pdata["profiles"][args.profile]
    fx = {k: v for k, v in pdata["fx_to_usd"].items() if not k.startswith("_")}
    weights = profile["weights"]
    filters = profile.get("filters", {})

    candidates, rejected = [], []
    for p in providers:
        ok, why = passes_filters(p, filters)
        (candidates if ok else rejected).append(p if ok else (p, why))

    if not candidates:
        raise SystemExit("every provider was filtered out — check the profile filters")

    rows = []
    for p in candidates:
        subs = subscores(p, fx)
        rows.append(
            {
                "id": p["id"],
                "name": p["name"],
                "category": p["category"],
                "usd_mo": round(usd_price(p, fx), 2),
                "plan": p["plan_name"],
                "spec": f"{p['vcpu']}vCPU/{p['ram_gb']}GB/{p['disk_gb']}GB",
                "locations": p["loc_count"],
                "confidence": p.get("confidence", "low"),
                "flags": p.get("flags", []),
                "subscores": {k: round(v, 1) for k, v in subs.items()},
                "total": round(weighted_total(subs, weights), 2),
            }
        )

    rows.sort(key=lambda r: r["total"], reverse=True)

    mc = monte_carlo(candidates, fx, weights, args.trials, args.seed, args.top_n)
    for r in rows:
        r["stability"] = {k: round(v, 1) for k, v in mc[r["id"]].items()}

    result = {
        "profile": args.profile,
        "profile_label": profile["label"],
        "weights": weights,
        "filters": filters,
        "fx_to_usd": fx,
        "trials": args.trials,
        "seed": args.seed,
        "counts": {
            "total": len(providers),
            "eligible": len(candidates),
            "rejected": len(rejected),
        },
        "rejected": [{"id": p["id"], "name": p["name"], "reason": why} for p, why in rejected],
        "ranking": rows,
    }

    if args.json:
        os.makedirs(os.path.dirname(args.json), exist_ok=True)
        with open(args.json, "w") as fh:
            json.dump(result, fh, indent=2)

    if args.csv:
        os.makedirs(os.path.dirname(args.csv), exist_ok=True)
        with open(args.csv, "w", newline="") as fh:
            w = csv.writer(fh)
            w.writerow(
                ["rank", "id", "name", "usd_mo", "total"]
                + list(CRITERIA)
                + ["top5_pct", "mean_rank", "confidence"]
            )
            for i, r in enumerate(rows, 1):
                w.writerow(
                    [i, r["id"], r["name"], r["usd_mo"], r["total"]]
                    + [r["subscores"][c] for c in CRITERIA]
                    + [r["stability"]["top_n_pct"], r["stability"]["mean_rank"], r["confidence"]]
                )

    if not args.quiet:
        print(f"profile: {args.profile}  ({profile['label']})")
        print(f"eligible: {len(candidates)} of {len(providers)}   trials: {args.trials}\n")
        print(f"{'#':>3}  {'provider':<24} {'$/mo':>7} {'score':>6} {'top5%':>6}  conf")
        print("-" * 62)
        for i, r in enumerate(rows[:15], 1):
            print(
                f"{i:>3}  {r['name']:<24} {r['usd_mo']:>7.2f} {r['total']:>6.1f} "
                f"{r['stability']['top_n_pct']:>5.1f}%  {r['confidence']}"
            )
        if rejected:
            print(f"\nfiltered out ({len(rejected)}):")
            for p, why in rejected:
                print(f"  - {p['name']}: {why}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
