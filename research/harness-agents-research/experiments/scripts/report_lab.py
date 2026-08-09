"""Render out/RANKINGS-2.md from the round-2 artefacts. Generated — do not edit.

Interpretation lives in ../FINDINGS-2.md; this only tabulates what was measured.
"""

from __future__ import annotations

import json
from pathlib import Path

OUT = Path("out")
QTYPES = ["stable_recall", "scoped_current", "historical_value", "authored",
          "retraction", "redacted_historical", "poison"]
LABELS = {"stable_recall": "stable", "scoped_current": "current",
          "historical_value": "historic", "authored": "authored",
          "retraction": "retract", "redacted_historical": "redacted",
          "poison": "poison"}


def load(name: str):
    p = OUT / name
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else None


def main() -> None:
    res = load("results_lab.json")
    if res is None:
        raise SystemExit("run scripts/evaluate_lab.py first")
    sweep = load("sweep_lab.json")

    rows = sorted(res["strategies"].values(),
                  key=lambda r: -r["modes"]["budgeted"]["composite"])

    L: list[str] = []
    L.append("# Round 2 rankings — generated, do not edit")
    L.append("")
    L.append(f"Seed `{res['seed']}` · {res['n_chunks']} chunks · "
             f"{res['n_queries']} queries · workspaces "
             f"{', '.join(f'`{w}`' for w in res['workspaces'])} · evidence "
             f"budget {res['evidence_budget']} tokens.")
    L.append("")
    L.append("Regenerate with `bash scripts/run_all.sh`. Interpretation lives "
             "in [`../FINDINGS-2.md`](../FINDINGS-2.md).")
    L.append("")

    L.append("## Headline")
    L.append("")
    L.append("`leak` = evidence set contained any chunk from another workspace. "
             "`leakF` = it contained another workspace's *fact*. "
             "`wrong` = the top-ranked item was a confidently wrong answer.")
    L.append("")
    L.append("| # | strategy | composite | leak | leakF | wrong | tokens |")
    L.append("|---|---|---:|---:|---:|---:|---:|")
    for i, r in enumerate(rows, 1):
        m = r["modes"]["budgeted"]
        L.append(f"| {i} | `{r['strategy']}` | **{m['composite']:.3f}** | "
                 f"{m['leak']:.2f} | {m['leak_fact']:.2f} | {m['wrong']:.2f} | "
                 f"{r['mean_tokens']:.0f} |")
    L.append("")

    L.append("## Per query type (budgeted)")
    L.append("")
    L.append("| strategy | " + " | ".join(LABELS[q] for q in QTYPES) + " | composite |")
    L.append("|---" * (len(QTYPES) + 2) + "|")
    for r in rows:
        bt = r["modes"]["budgeted"]["by_type"]
        cells = " | ".join(f"{bt[q]['score']:.2f}" for q in QTYPES)
        L.append(f"| `{r['strategy']}` | {cells} | "
                 f"{r['modes']['budgeted']['composite']:.3f} |")
    L.append("")

    if sweep:
        L.append("## Attack success rate vs evidence width")
        L.append("")
        L.append("Does 0% attack success without a defence survive a wider "
                 "evidence set?")
        L.append("")
        L.append("| k | no admission (`hybrid_scope_filter`) | write-time admission (`control_scoped`) |")
        L.append("|---|---:|---:|")
        for k in sweep["k_values"]:
            row = sweep["security"][str(k)]
            L.append(f"| {k} | {row['hybrid_scope_filter']:.2f} | "
                     f"{row['control_scoped']:.2f} |")
        L.append("")

        L.append("## Seed stability")
        L.append("")
        L.append(f"Seeds: {', '.join(str(s) for s in sweep['seeds'])}. "
                 "`rank` is best-worst placement across seeds.")
        L.append("")
        L.append("| strategy | mean | min | max | rank |")
        L.append("|---|---:|---:|---:|---:|")
        for name, a in sorted(sweep["aggregate"].items(),
                              key=lambda kv: -kv[1]["mean"]):
            rank = (str(a["best_rank"]) if a["best_rank"] == a["worst_rank"]
                    else f"{a['best_rank']}-{a['worst_rank']}")
            L.append(f"| `{name}` | {a['mean']:.3f} | {a['min']:.3f} | "
                     f"{a['max']:.3f} | {rank} |")
        L.append("")

    (OUT / "RANKINGS-2.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    print(f"wrote {OUT / 'RANKINGS-2.md'} ({len(L)} lines)")


if __name__ == "__main__":
    main()
