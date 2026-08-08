"""Scoring for the IP-blocking study.

The reference workload, fixed before any row was collected:

    10,000 successful fetches/month of Cloudflare-protected pages
    ~500 KB each  ->  ~5 GB egress
    20% needing a persistent session
    per-IP options costed at 10 IPs, browser-hour options at 25 hours

Every pricing model in this market is different — per GB, per 1,000 requests,
per IP per month, per browser-hour, flat — and vendors naturally quote the one
that flatters them. `derive` collapses all of them onto one monthly figure for
that single workload, which is the only way the rows are comparable.
"""

from comparelib import log_norm_inverse, scale_1_5, clamp

# --- the reference workload -------------------------------------------------
FETCHES = 10_000
GB = 5
IPS = 10
BROWSER_HOURS = 25

# Cloudflare chains six checks; a solution is scored on how many it addresses.
ALL_LAYERS = ["tls", "http2", "js", "behavior", "ip_rep", "captcha"]


def derive(c, ctx):
    """Collapse every pricing model onto one monthly USD figure."""
    model = c.get("pricing_model")
    unit = c.get("unit_price", 0) or 0

    if model == "per_gb":
        cost = unit * GB
    elif model == "per_1k_req":
        cost = unit * (FETCHES / 1000.0)
    elif model == "per_ip_mo":
        cost = unit * IPS
    elif model == "per_hour":
        cost = unit * BROWSER_HOURS
    elif model == "flat_mo":
        cost = unit
    else:  # free
        cost = 0.0

    # A plan floor is real money whether or not you use the quota. Several
    # vendors' headline per-request rates are unreachable at this volume
    # because the monthly minimum dominates.
    c["monthly_usd"] = round(max(cost, c.get("plan_min_mo", 0) or 0), 2)
    c["raw_usage_usd"] = round(cost, 2)
    return c


def score_effectiveness(c):
    """Measured or claimed pass rate against Cloudflare-class protection.

    Used directly — it is already a 0-100 quantity, and it is the one number
    that decides whether the spend accomplishes anything.
    """
    return clamp(c.get("success_pct", 0))


def score_cost(c):
    """Monthly cost for the reference workload, log-scaled.

    Log because the spread is enormous — $0 to ~$70 — and the difference
    between $3 and $30 matters far more than between $60 and $90.
    """
    return log_norm_inverse(max(c.get("monthly_usd", 0), 1.0), 1.0, 200.0)


def score_completeness(c):
    """How many of the six detection layers the solution actually addresses.

    This is the finding the whole study turns on. A residential proxy fixes
    exactly one layer (ip_rep) and leaves the TLS handshake — checked first,
    at the edge, before the IP is scored — untouched. Two products at the same
    price can differ 6x here.
    """
    fixed = set(c.get("layers_fixed", []))
    return 100.0 * len(fixed & set(ALL_LAYERS)) / len(ALL_LAYERS)


def score_integration(c):
    """Effort to wire into Jod. 1 = an env var, 5 = hardware in a rack."""
    return scale_1_5(c.get("integration", 3), invert=True)


def score_ethics(c):
    """Whether the IPs were obtained with the owner's informed consent.

    Not decoration. Routing Jod's traffic through someone's Smart TV without
    their understanding is both a real harm and a real liability, and the
    2026 reporting on bundled SDKs made the distinction concrete.
    """
    return scale_1_5(c.get("ethics", 3), invert=True)


def score_ops(c):
    """Maintenance burden, plus the operational features an agent needs."""
    burden = scale_1_5(c.get("ops_burden", 3), invert=True)
    sticky = 100.0 if c.get("sticky_sessions") else 0.0
    geo = 100.0 if c.get("geo_control") else 0.0
    return 0.6 * burden + 0.25 * sticky + 0.15 * geo


CRITERIA = {
    "effectiveness": score_effectiveness,
    "cost": score_cost,
    "completeness": score_completeness,
    "integration": score_integration,
    "ethics": score_ethics,
    "ops": score_ops,
}
