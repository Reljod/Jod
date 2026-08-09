"""Render out/RANKINGS.md from the JSON artefacts. Generated file — do not edit.

Interpretation lives in ../FINDINGS.md; this only tabulates what was measured.

Usage:  python3 scripts/report.py
"""

from __future__ import annotations

import json
from pathlib import Path

OUT = Path("out")
QTYPES = ["stable_recall", "current_value", "historical_value",
          "multihop", "retraction", "poison"]
LABELS = {
    "stable_recall": "stable",
    "current_value": "current",
    "historical_value": "historic",
    "multihop": "multihop",
    "retraction": "retract",
    "poison": "poison",
}


def load(name: str):
    p = OUT / name
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else None


def main() -> None:
    results = load("results.json")
    if results is None:
        raise SystemExit("run scripts/evaluate.py first")
    sweep = load("sweep.json")
    stability = load("stability.json")
    sens = load("sensitivity.json")

    rows = sorted(results["strategies"].values(),
                  key=lambda r: -r["modes"]["budgeted"]["composite"])
    best_tokens = min(r["mean_tokens"] for r in rows)

    L: list[str] = []
    L.append("# Rankings — generated, do not edit")
    L.append("")
    L.append(f"Seed `{results['seed']}` · {results['n_chunks']} chunks · "
             f"{results['n_queries']} queries · evidence budget "
             f"{results['evidence_budget']} tokens.")
    L.append("")
    L.append("Regenerate with `bash scripts/run_all.sh`. "
             "Interpretation lives in [`../FINDINGS.md`](../FINDINGS.md).")
    L.append("")

    L.append("## Headline")
    L.append("")
    L.append("`composite` is the unweighted mean of the six query-type scores. "
             "`budgeted` truncates every strategy to the same evidence budget; "
             "`natural` lets each spend what it wants.")
    L.append("")
    L.append("| # | strategy | budgeted | natural | tokens/query | vs cheapest |")
    L.append("|---|---|---:|---:|---:|---:|")
    for i, r in enumerate(rows, 1):
        L.append(f"| {i} | `{r['strategy']}` | "
                 f"**{r['modes']['budgeted']['composite']:.3f}** | "
                 f"{r['modes']['natural']['composite']:.3f} | "
                 f"{r['mean_tokens']:.0f} | "
                 f"{r['mean_tokens'] / best_tokens:.1f}x |")
    L.append("")

    for mode in ("budgeted", "natural"):
        L.append(f"## Per query type — {mode}")
        L.append("")
        L.append("| strategy | " + " | ".join(LABELS[q] for q in QTYPES)
                 + " | composite |")
        L.append("|---" * (len(QTYPES) + 2) + "|")
        ordered = sorted(results["strategies"].values(),
                         key=lambda r: -r["modes"][mode]["composite"])
        for r in ordered:
            bt = r["modes"][mode]["by_type"]
            cells = " | ".join(f"{bt[q]['score']:.2f}" for q in QTYPES)
            L.append(f"| `{r['strategy']}` | {cells} | "
                     f"{r['modes'][mode]['composite']:.3f} |")
        L.append("")

    L.append("## The stale trap")
    L.append("")
    L.append("`strict` requires the current version to be retrieved with no "
             "superseded version ranked above it. `lenient` only requires it to "
             "be present. `stale_above` is how often an outdated version "
             "outranks the true one — the gap between the two columns is "
             "exactly what a control plane buys.")
    L.append("")
    L.append("| strategy | strict | lenient | stale_above |")
    L.append("|---|---:|---:|---:|")
    for r in sorted(results["strategies"].values(),
                    key=lambda r: -r["modes"]["budgeted"]["by_type"]["current_value"]["score"]):
        cv = r["modes"]["budgeted"]["by_type"]["current_value"]
        L.append(f"| `{r['strategy']}` | {cv['score']:.2f} | "
                 f"{cv['lenient']:.2f} | {cv['stale_above']:.2f} |")
    L.append("")

    L.append("## Memory poisoning")
    L.append("")
    L.append("`asr` is attack success rate: how often the untrusted chunk "
             "reached the evidence set. Lower is better.")
    L.append("")
    L.append("| strategy | asr | poison score |")
    L.append("|---|---:|---:|")
    for r in sorted(results["strategies"].values(),
                    key=lambda r: r["modes"]["natural"]["by_type"]["poison"]["asr"]):
        p = r["modes"]["natural"]["by_type"]["poison"]
        L.append(f"| `{r['strategy']}` | {p['asr']:.2f} | {p['score']:.2f} |")
    L.append("")

    if sweep:
        L.append("## Fusion weight sweep")
        L.append("")
        L.append(f"Tuned on seed `{sweep['tune_seed']}`, which the headline "
                 "table never uses. `vector weight` is the dense channel's "
                 "share; the rest goes to BM25.")
        L.append("")
        L.append("| vector weight | linear | rrf |")
        L.append("|---|---:|---:|")
        for k in sorted(sweep["table"]["linear"], key=float):
            L.append(f"| {k} | {sweep['table']['linear'][k]:.3f} | "
                     f"{sweep['table']['rrf'][k]:.3f} |")
        L.append("")
        for mode, b in sweep["best"].items():
            L.append(f"- best **{mode}**: vector weight "
                     f"`{b['vector_weight']:.1f}` → {b['composite']:.3f}")
        L.append("")

    if sens:
        L.append("## minScore sensitivity")
        L.append("")
        L.append("Cells are `composite / mean results returned`. A floor is an "
                 "absolute number compared against a score whose scale depends "
                 "on both the embedder and the fusion formula.")
        L.append("")
        floors = [f"{f:.2f}" for f in sens["floors"]]
        L.append("| strategy | " + " | ".join(floors) + " |")
        L.append("|---" * (len(floors) + 1) + "|")
        for name, row in sens["table"].items():
            cells = " | ".join(
                f"{row[f]['composite']:.3f} / {row[f]['mean_results']:.1f}"
                for f in floors)
            L.append(f"| `{name}` | {cells} |")
        L.append("")

    if stability:
        L.append("## Seed stability")
        L.append("")
        L.append(f"Seeds: {', '.join(str(s) for s in stability['seeds'])}. "
                 "`rank` is best-worst placement across seeds.")
        L.append("")
        L.append("| strategy | mean | min | max | rank |")
        L.append("|---|---:|---:|---:|---:|")
        for name, a in sorted(stability["aggregate"].items(),
                              key=lambda kv: -kv[1]["mean"]):
            rank = (str(a["best_rank"]) if a["best_rank"] == a["worst_rank"]
                    else f"{a['best_rank']}-{a['worst_rank']}")
            L.append(f"| `{name}` | {a['mean']:.3f} | {a['min']:.3f} | "
                     f"{a['max']:.3f} | {rank} |")
        L.append("")

    (OUT / "RANKINGS.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    print(f"wrote {OUT / 'RANKINGS.md'} ({len(L)} lines)")


if __name__ == "__main__":
    main()
