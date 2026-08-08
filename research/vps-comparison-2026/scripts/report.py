#!/usr/bin/env python3
"""Generate the final markdown report from scores + measurements.

The split of labour here is deliberate: this script owns every number and
table, so the report cannot drift from the data. The judgement — what the
numbers mean, what to actually buy — is written by hand in REPORT.md's
narrative sections, which this file emits as ANALYSIS blocks.

Usage:
    python3 report.py --out ../out/REPORT.md
"""

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DATA = os.path.join(ROOT, "data")
OUT = os.path.join(ROOT, "out")

PROFILES = ["jod", "verified", "cheapest", "freedom", "production"]


def run_profile(name, trials):
    """Invoke score.py and read back its JSON, so the report and the CLI can
    never disagree about a number."""
    path = os.path.join(OUT, f"scores-{name}.json")
    subprocess.run(
        [
            sys.executable, os.path.join(HERE, "score.py"),
            "--profile", name, "--trials", str(trials),
            "--json", path, "--csv", os.path.join(OUT, f"scores-{name}.csv"),
            "--quiet",
        ],
        check=True,
    )
    with open(path) as fh:
        return json.load(fh)


def load_json(path):
    if not os.path.exists(path):
        return None
    with open(path) as fh:
        return json.load(fh)


def fmt_flags(flags):
    return ", ".join(f"`{f}`" for f in flags) if flags else "—"


def conf_badge(c):
    return {"high": "**high**", "medium": "medium", "low": "_low_"}.get(c, c)


def table_full_ranking(res, providers_by_id, limit=None):
    rows = res["ranking"][:limit] if limit else res["ranking"]
    out = [
        "| # | Provider | $/mo | Score | Perm | Cost | Avail | Net | Loc | Top-5 stability | Confidence |",
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for i, r in enumerate(rows, 1):
        s = r["subscores"]
        out.append(
            f"| {i} | **{r['name']}** | ${r['usd_mo']:.2f} | {r['total']:.1f} | "
            f"{s['permissiveness']:.0f} | {s['cost']:.0f} | {s['availability']:.0f} | "
            f"{s['network']:.0f} | {s['locations']:.0f} | "
            f"{r['stability']['top_n_pct']:.0f}% | {conf_badge(r['confidence'])} |"
        )
    return "\n".join(out)


def table_profile_compare(all_res):
    out = ["| Rank | " + " | ".join(all_res[p]["profile_label"] for p in PROFILES) + " |",
           "|---:|" + "---|" * len(PROFILES)]
    for i in range(5):
        cells = []
        for p in PROFILES:
            rk = all_res[p]["ranking"]
            cells.append(f"{rk[i]['name']} (${rk[i]['usd_mo']:.0f})" if i < len(rk) else "—")
        out.append(f"| {i+1} | " + " | ".join(cells) + " |")
    return "\n".join(out)


def table_netcheck(net, providers_by_id, top_ids):
    if not net:
        return "_No measurement data. Run `python3 scripts/netcheck.py` first._"

    out = [
        "| Provider | Median RTT | Jitter | Endpoint | Reading |",
        "|---|---:|---:|---|---|",
    ]
    rows = [(pid, r) for pid, r in net["results"].items() if pid in top_ids and r.get("ok")]
    for pid, r in sorted(rows, key=lambda kv: kv[1]["median_ms"]):
        name = providers_by_id.get(pid, {}).get("name", pid)
        if r["jitter_ms"] >= 900:
            reading = "packet loss (SYN retransmits)"
        elif r["cdn"]:
            reading = "CDN-fronted — not the metal"
        else:
            reading = "direct"
        out.append(
            f"| {name} | {r['median_ms']:.0f} ms | {r['jitter_ms']:.0f} ms | "
            f"`{r['host']}` | {reading} |"
        )
    return "\n".join(out)


def table_rejected(res):
    if not res["rejected"]:
        return "_Nothing was filtered out._"
    out = ["| Provider | Disqualified because |", "|---|---|"]
    for r in res["rejected"]:
        out.append(f"| {r['name']} | {r['reason']} |")
    return "\n".join(out)


def table_confidence(providers):
    counts = {"high": 0, "medium": 0, "low": 0}
    for p in providers:
        counts[p.get("confidence", "low")] = counts.get(p.get("confidence", "low"), 0) + 1
    total = sum(counts.values())
    out = ["| Confidence | Providers | Share | Meaning |", "|---|---:|---:|---|"]
    meaning = {
        "high": "Price and policy confirmed against the provider's own live page",
        "medium": "Confirmed against a reputable secondary source or a partial live fetch",
        "low": "Plausible market figure, **not** confirmed — verify before purchase",
    }
    for c in ["high", "medium", "low"]:
        out.append(f"| {conf_badge(c)} | {counts[c]} | {100*counts[c]/total:.0f}% | {meaning[c]} |")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trials", type=int, default=20000)
    ap.add_argument("--out", default=os.path.join(OUT, "RANKINGS.md"))
    args = ap.parse_args()

    with open(os.path.join(DATA, "providers.json")) as fh:
        providers = json.load(fh)["providers"]
    by_id = {p["id"]: p for p in providers}

    all_res = {p: run_profile(p, args.trials) for p in PROFILES}
    net = load_json(os.path.join(OUT, "netcheck.json"))

    jod = all_res["jod"]
    top_ids = {r["id"] for r in jod["ranking"][:12]}

    w = jod["weights"]
    weight_lines = "\n".join(
        f"- **{k}** — {v*100:.0f}%" for k, v in sorted(w.items(), key=lambda kv: -kv[1])
    )

    md = f"""<!-- GENERATED by scripts/report.py — do not edit by hand.
     Narrative analysis lives in REPORT.md. This file is the numbers. -->

# VPS rankings — generated tables

Generated from `data/providers.json` at {jod['counts']['total']} candidates.
Monte Carlo: {args.trials:,} trials, seed `{jod['seed']}`.

FX used: {", ".join(f"1 {k} = {v} USD" for k, v in jod['fx_to_usd'].items())}.

## The weighting

{weight_lines}

Hard filters applied before scoring: {json.dumps(jod['filters'])}

**{jod['counts']['eligible']} of {jod['counts']['total']}** candidates survived the filters.

## Full ranking — Jod profile

{table_full_ranking(jod, by_id)}

Columns are 0-100 sub-scores. "Top-5 stability" is the share of {args.trials:,}
Monte Carlo trials in which the provider held a top-5 position while weights were
perturbed +/-35% and prices were perturbed by their confidence-scaled error bar.

## What wins under different priorities

{table_profile_compare(all_res)}

Where a provider tops several columns, the recommendation is robust. Where the
columns disagree entirely, the "best" VPS is genuinely a matter of what you
weight — and the report says so rather than hiding it.

## Measured network latency

Top candidates only. Measured from the machine that ran `netcheck.py`.

{table_netcheck(net, by_id, top_ids)}

Read this cautiously: it is TCP-connect time to each company's public website,
which for most is a CDN edge rather than a datacenter you can buy in. It tells
you a host is reachable and whether the path drops packets. It does not tell you
how fast your VPS will be.

## Filtered out before scoring

{table_rejected(jod)}

## Data confidence

{table_confidence(providers)}

Every figure marked _low_ is a research lead, not a quotation. The Monte Carlo
above already penalises them: low-confidence prices are perturbed +/-25% versus
+/-4% for verified ones, so an unverified bargain must survive being wrong
before it can rank.
"""

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as fh:
        fh.write(md)

    print(f"wrote {args.out}")
    for p in PROFILES:
        top = all_res[p]["ranking"][0]
        print(f"  {p:<12} winner: {top['name']} (${top['usd_mo']:.2f}, {top['total']:.1f})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
