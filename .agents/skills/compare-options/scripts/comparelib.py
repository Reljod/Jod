#!/usr/bin/env python3
"""Reusable machinery for scripted option comparisons.

Everything here is domain-agnostic: normalization, hard filters, weighted
totals, and the Monte Carlo that propagates data uncertainty into the ranking.
The domain-specific part — what "cost" or "reliability" means for *your*
candidates — lives in the study's own `criteria.py`.

Stdlib only, so a study runs anywhere with no install step.
"""

import importlib.util
import json
import math
import os
import random

# How far the uncertain field is allowed to be wrong, by how much the row is
# trusted. This is the mechanism that stops an unverified bargain from winning.
CONFIDENCE_ERROR = {"high": 0.04, "medium": 0.12, "low": 0.25}
DEFAULT_ERROR = 0.25


# ---------------------------------------------------------------- normalizing


def clamp(x, lo=0.0, hi=100.0):
    return max(lo, min(hi, x))


def scale_1_5(v, invert=False):
    """Map a 1-5 rating onto 0-100. `invert=True` means 1 is the good end."""
    v = max(1, min(5, v))
    frac = (5 - v) / 4.0 if invert else (v - 1) / 4.0
    return frac * 100.0


def log_norm(value, lo, hi):
    """Normalize onto 0-100 on a log scale.

    Log rather than linear because real-world option sets have long tails: one
    candidate with 40 locations should not make a 15-location candidate look
    like zero.
    """
    value = max(value, lo)
    if hi <= lo:
        return 100.0
    return clamp(100.0 * (math.log(value) - math.log(lo)) / (math.log(hi) - math.log(lo)))


def log_norm_inverse(value, lo, hi):
    """Same, but lower is better — prices, latencies, error rates."""
    return 100.0 - log_norm(value, lo, hi)


def bool_score(pairs):
    """Weighted sum over (flag, weight) pairs, as a 0-100 score."""
    return sum(100.0 * w for ok, w in pairs if ok)


# ---------------------------------------------------------------- study loading


def load_study(study_dir):
    """Read a study directory: dataset, profiles, and its criteria module."""
    data_dir = os.path.join(study_dir, "data")

    with open(os.path.join(data_dir, "dataset.json")) as fh:
        dataset = json.load(fh)
    with open(os.path.join(data_dir, "profiles.json")) as fh:
        profiles = json.load(fh)

    criteria_path = os.path.join(study_dir, "criteria.py")
    if not os.path.exists(criteria_path):
        raise SystemExit(f"missing {criteria_path} — every study defines its own criteria")

    spec = importlib.util.spec_from_file_location("study_criteria", criteria_path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    if not hasattr(mod, "CRITERIA"):
        raise SystemExit(f"{criteria_path} must define CRITERIA = {{name: fn(candidate)}}")

    return dataset, profiles, mod


def context(profiles):
    """Non-candidate inputs the criteria may need (FX rates, constants)."""
    return {k: v for k, v in profiles.items() if not k.startswith("_") and k != "profiles"}


def materialize(candidate, mod, ctx):
    """Apply the study's `derive` hook, if it has one.

    Derived fields (a native price converted to one currency, a total cost of
    ownership) are computed once here so the criteria functions stay pure
    functions of a candidate dict.
    """
    c = dict(candidate)
    if hasattr(mod, "derive"):
        c = mod.derive(c, ctx)
    return c


# ---------------------------------------------------------------- filtering


def passes_filters(candidate, filters):
    """Return (ok, reason). Generic rules, driven entirely by the profile.

    Supported keys:
      min_<field> / max_<field>   numeric bounds
      require_<field>             value must be in the given list
      require_true                list of fields that must be truthy
      exclude_<field>             value must NOT be in the given list
      exclude_flags               flag prefixes that disqualify

    Flags are checked first so that when a candidate fails several rules, the
    reported reason is the meaningful one. "flagged excluded:sanctioned" tells
    the reader why it can never be used; "capacity below minimum" reads like it
    might qualify with a bigger plan.
    """
    for prefix in filters.get("exclude_flags", []):
        for flag in candidate.get("flags", []):
            if flag.startswith(prefix):
                return False, f"flagged {flag}"

    for key, want in filters.items():
        if key == "exclude_flags":
            continue

        if key == "require_true":
            for field in want:
                if not candidate.get(field):
                    return False, f"{field} is not available"

        elif key.startswith("min_"):
            field = key[4:]
            if field in candidate and candidate[field] < want:
                return False, f"{field}={candidate[field]} below minimum {want}"

        elif key.startswith("max_"):
            field = key[4:]
            if field in candidate and candidate[field] > want:
                return False, f"{field}={candidate[field]} above maximum {want}"

        elif key.startswith("require_"):
            field = key[8:]
            if field in candidate and candidate[field] not in want:
                return False, f"{field}={candidate[field]!r} not in {want}"

        elif key.startswith("exclude_"):
            field = key[8:]
            if field in candidate and candidate[field] in want:
                return False, f"{field}={candidate[field]!r} is excluded"

    return True, ""


# ---------------------------------------------------------------- scoring


def subscores(candidate, mod):
    return {name: fn(candidate) for name, fn in mod.CRITERIA.items()}


def weighted_total(subs, weights):
    total_w = sum(weights.values())
    if total_w <= 0:
        return 0.0
    return sum(subs[k] * w for k, w in weights.items() if k in subs) / total_w


def score_all(candidates, mod, ctx, weights):
    rows = []
    for c in candidates:
        m = materialize(c, mod, ctx)
        subs = subscores(m, mod)
        rows.append({"candidate": c, "materialized": m, "subscores": subs,
                     "total": weighted_total(subs, weights)})
    rows.sort(key=lambda r: r["total"], reverse=True)
    return rows


# ---------------------------------------------------------------- monte carlo


def monte_carlo(candidates, mod, ctx, weights, uncertain_field,
                trials=20000, seed=0, top_n=5, weight_jitter=0.35):
    """Perturb weights and the uncertain field, then re-rank, repeatedly.

    Two independent sources of doubt are simulated at once:

      - the weighting, because it is a judgement call, not a measurement
      - the data, because some rows were never verified

    The second is scaled per row by `confidence`, so a candidate whose price
    you could not confirm has to survive being wrong about it before it is
    allowed to rank. Returns per-candidate stability statistics.
    """
    rng = random.Random(seed)
    ids = [c["id"] for c in candidates]
    top_hits = {i: 0 for i in ids}
    win_hits = {i: 0 for i in ids}
    rank_sum = {i: 0 for i in ids}

    lo, hi = 1.0 - weight_jitter, 1.0 + weight_jitter

    for _ in range(trials):
        w = {k: v * rng.uniform(lo, hi) for k, v in weights.items()}

        scored = []
        for c in candidates:
            m = materialize(c, mod, ctx)
            if uncertain_field and uncertain_field in m:
                err = CONFIDENCE_ERROR.get(c.get("confidence"), DEFAULT_ERROR)
                m = dict(m)
                m[uncertain_field] = m[uncertain_field] * rng.uniform(1 - err, 1 + err)
                if hasattr(mod, "derive"):
                    m = mod.derive(m, ctx)
            scored.append((weighted_total(subscores(m, mod), w), c["id"]))

        scored.sort(reverse=True)
        for rank, (_, cid) in enumerate(scored, start=1):
            rank_sum[cid] += rank
            if rank <= top_n:
                top_hits[cid] += 1
            if rank == 1:
                win_hits[cid] += 1

    return {
        cid: {
            "top_n_pct": 100.0 * top_hits[cid] / trials,
            "win_pct": 100.0 * win_hits[cid] / trials,
            "mean_rank": rank_sum[cid] / trials,
        }
        for cid in ids
    }
