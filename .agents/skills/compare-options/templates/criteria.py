"""Domain-specific scoring for this study.

This is the only file you must write per study. Everything generic — filters,
normalization, Monte Carlo, output — lives in the skill's `comparelib.py`.

Contract:

  CRITERIA : dict[str, callable(candidate) -> float in 0..100]
      One entry per criterion named in profiles.json weights.

  derive(candidate, ctx) -> candidate            (optional)
      Runs once before scoring. Compute derived fields here (currency
      conversion, totals) so the criteria stay pure functions of a dict.
      `ctx` holds the non-profile keys of profiles.json, e.g. fx_to_usd.
      It also re-runs inside the Monte Carlo after a field is perturbed, so
      keep it deterministic and side-effect free.

Every criterion must return 0-100 where **higher is always better**, whatever
the underlying field's direction. Use `log_norm_inverse` for
lower-is-better quantities and `scale_1_5(..., invert=True)` for
lower-is-better ratings.
"""

# score.py puts the skill's scripts/ directory on sys.path before loading this
# file, so comparelib imports directly — no path juggling needed here.
from comparelib import log_norm_inverse, scale_1_5, bool_score  # noqa: F401


def derive(c, ctx):
    """Convert the native price to one currency so rows are comparable."""
    fx = ctx.get("fx_to_usd", {})
    rate = fx.get(c.get("currency", "USD"), 1.0)
    c["usd"] = c["price"] * rate
    return c


def score_cost(c):
    """Blend absolute price with price-per-unit-of-capacity.

    Absolute price answers 'what leaves my account'; per-unit answers 'what do
    I get for it'. Without the second term, twice the capacity for the same
    money looks identical to half of it.
    """
    usd = max(c.get("usd", c["price"]), 0.5)
    absolute = log_norm_inverse(usd, 2.0, 120.0)
    per_unit = log_norm_inverse(usd / max(c.get("capacity", 1), 1), 0.3, 10.0)
    return 0.6 * absolute + 0.4 * per_unit


def score_reliability(c):
    return scale_1_5(c.get("reliability", 3))


def score_permissiveness(c):
    # `strictness` is 1=best, so invert it.
    return scale_1_5(c.get("strictness", 3), invert=True)


def score_ops(c):
    return bool_score([
        (c.get("has_api"), 0.6),
        (c.get("capacity", 0) >= 8, 0.4),
    ])


CRITERIA = {
    "cost": score_cost,
    "reliability": score_reliability,
    "permissiveness": score_permissiveness,
    "ops": score_ops,
}
