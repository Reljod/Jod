#!/usr/bin/env python3
"""Validate providers.json before anything scores it.

This is the runnable check for the dataset. A mistyped rating (say `ip_rep: 6`)
would silently distort every ranking downstream, so the pipeline refuses to run
until the data is well-formed. Exits non-zero on any error.

Usage:
    python3 validate.py
"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(os.path.dirname(HERE), "data")

REQUIRED = [
    "id", "name", "hq", "jurisdiction", "category", "plan_name", "vcpu", "ram_gb",
    "disk_gb", "disk_type", "traffic_tb", "port_gbps", "price", "currency", "term",
    "virt", "loc_count", "regions", "ipv6", "ipv4_incl", "net_quality", "ip_rep",
    "sla_pct", "uptime_rep", "steal_risk", "cpu_perf", "aup_strict", "kyc", "dmca",
    "crypto", "api", "snapshots", "hourly", "docker_ok", "compliance", "confidence",
    "sources", "notes", "flags",
]

RATING_FIELDS = ["net_quality", "ip_rep", "uptime_rep", "steal_risk", "cpu_perf", "aup_strict", "kyc"]
VALID_REGIONS = {"EU", "NA", "SA", "APAC", "ME", "AF", "OC"}
VALID_DMCA = {"ignored", "forwarded", "responsive", "strict"}
VALID_CONF = {"high", "medium", "low"}
VALID_DISK = {"NVMe", "SSD", "HDD"}
VALID_CURRENCY = {"USD", "EUR", "GBP"}
VALID_CATEGORY = {"mainstream", "eu-budget", "lowend", "offshore", "hyperscaler", "paas"}


def main():
    with open(os.path.join(DATA, "providers.json")) as fh:
        providers = json.load(fh)["providers"]
    with open(os.path.join(DATA, "profiles.json")) as fh:
        fx = json.load(fh)["fx_to_usd"]

    errors, warnings = [], []
    seen = set()

    for p in providers:
        pid = p.get("id", "<missing id>")

        for field in REQUIRED:
            if field not in p:
                errors.append(f"{pid}: missing required field {field!r}")

        if pid in seen:
            errors.append(f"{pid}: duplicate id")
        seen.add(pid)

        for f in RATING_FIELDS:
            v = p.get(f)
            if isinstance(v, int) and not (1 <= v <= 5):
                errors.append(f"{pid}: {f}={v} outside the 1-5 scale")

        if p.get("dmca") not in VALID_DMCA:
            errors.append(f"{pid}: dmca={p.get('dmca')!r} not one of {sorted(VALID_DMCA)}")
        if p.get("confidence") not in VALID_CONF:
            errors.append(f"{pid}: confidence={p.get('confidence')!r} invalid")
        if p.get("disk_type") not in VALID_DISK:
            errors.append(f"{pid}: disk_type={p.get('disk_type')!r} invalid")
        if p.get("currency") not in VALID_CURRENCY:
            errors.append(f"{pid}: currency={p.get('currency')!r} has no FX rate")
        if p.get("currency") not in fx:
            errors.append(f"{pid}: currency {p.get('currency')!r} missing from fx_to_usd")
        if p.get("category") not in VALID_CATEGORY:
            errors.append(f"{pid}: category={p.get('category')!r} invalid")

        for r in p.get("regions", []):
            if r not in VALID_REGIONS:
                errors.append(f"{pid}: region {r!r} not in {sorted(VALID_REGIONS)}")

        sla = p.get("sla_pct", 0)
        if not (0 <= sla <= 100):
            errors.append(f"{pid}: sla_pct={sla} out of range")

        for num in ("vcpu", "ram_gb", "disk_gb", "price", "loc_count"):
            v = p.get(num)
            if isinstance(v, (int, float)) and v < 0:
                errors.append(f"{pid}: {num}={v} is negative")

        if not p.get("sources"):
            warnings.append(f"{pid}: no sources cited")
        if p.get("confidence") == "low":
            warnings.append(f"{pid}: low confidence — price unverified against a live page")

    print(f"validated {len(providers)} providers")

    if warnings:
        print(f"\n{len(warnings)} warning(s):")
        for w in warnings:
            print(f"  ! {w}")

    if errors:
        print(f"\n{len(errors)} ERROR(s):")
        for e in errors:
            print(f"  x {e}")
        return 1

    print("\nno errors")
    return 0


if __name__ == "__main__":
    sys.exit(main())
