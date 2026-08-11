#!/usr/bin/env python3
"""Render the iteration log and ranking table into RECOMMENDATION.md.

Substitutes the <!--ITERATIONS--> and <!--RANKING--> markers in place, so the
prose around them is written once and the numbers are never hand-copied.

Usage:  render_iterations.py ../out/iterations-100k.json ../RECOMMENDATION.md
"""

import json
import sys

LABEL = {
    "I1_noindex_unionall": "no index, `UNION ALL`",
    "I2_noindex_union": "no index, `UNION` dedup",
    "I3_index_src": "index on `(src)`",
    "I4_covering_src_dst": "covering `(src, dst)`",
    "I5_plus_reverse": "+ reverse `(dst, src)`, two recursive terms",
    "I6_both_direction_rows": "mirrored rows in `relations_u`",
    "I7_scope_temporal_covering": "scope-first covering + temporal pushdown",
    "I8_temporal_postfilter": "same storage, temporal **post**-filter",
    "I9_2hop_closure": "materialised 2-hop closure",
    "I10_json_adjacency": "denormalised JSON adjacency",
}
CLASSES = ["Q1", "Q2", "Q3", "Q4", "Q5", "Q6"]


def cell(q):
    if q is None:
        return "—"
    mark = " ⏱" if q.get("timeouts") else ""
    return "%s / %s%s" % (q["p50_ms"], q["p95_ms"], mark)


def main(src, dest):
    d = json.load(open(src))
    it = d["iterations"]

    L = []
    p = L.append
    p("Measured at **%s edges / %s entities**, SQLite %s. Each cell is "
      "**p50 / p95 ms**; `—` is a query class the design cannot answer; ⏱ "
      "marks a class where at least one sample hit its ceiling and is "
      "recorded at the ceiling.\n"
      % ("{:,}".format(d["edges"]), "{:,}".format(d["entities"]),
         d["sqlite_version"]))
    p("| # | design | Q1 1-hop | Q2 3-hop dir | Q3 3-hop undir | Q4 as-of | "
      "Q5 path | Q6 hybrid | size | score |")
    p("|---|--------|---|---|---|---|---|---|---|---|")
    for r in it:
        q = r["queries"]
        p("| **%d** | %s | %s | %s | %s | %s | %s | %s | %.1f MB | **%.3f** |"
          % (r["iteration"], LABEL[r["name"]],
             *[cell(q.get(k)) for k in CLASSES],
             r["bytes"] / 1e6, r["score"]["total"]))

    p("\nWhat changed at each step, and what it bought:\n")
    prev = None
    prev_classes = None
    for r in it:
        w = r["score"]["worst_core_p95_ms"]
        classes = set(r["queries"])
        delta = ""
        if prev is not None and prev and w:
            if classes != prev_classes:
                # Comparing a worst-case across designs that answer different
                # question sets is not a comparison. Say so instead of
                # printing a ratio that means nothing.
                delta = (" Not comparable to the previous step: it answers a "
                         "different set of query classes.")
            else:
                ratio = prev / w
                if ratio >= 1.15:
                    fmt = "%.0fx" if ratio >= 10 else "%.1fx"
                    delta = (" **" + fmt % ratio +
                             " faster** than the previous step.")
                elif ratio <= 0.87:
                    delta = " **%.0f%% slower** than the previous step." % (
                        100.0 * (w / prev - 1))
                else:
                    delta = " No material change in the worst core query."
        p("- **%d. %s** — %s Worst core p95 **%s ms**, rubric **%.3f**.%s"
          % (r["iteration"], LABEL[r["name"]], r["changed"], w,
             r["score"]["total"], delta))
        if r.get("declared_why"):
            p("  - Declared score note: %s" % r["declared_why"])
        prev = w
        prev_classes = classes

    iters = "\n".join(L)

    R = []
    p = R.append
    p("Sorted by weighted score, not by iteration order. Criteria and weights "
      "are fixed in [`RUBRIC.md`](RUBRIC.md): latency 0.25, query power 0.20, "
      "build 0.15, one-file 0.10, multi-process 0.10, maintenance 0.10, "
      "simplicity 0.10.\n")
    p("| rank | # | design | latency | power | build | file | proc | maint | "
      "simple | **total** |")
    p("|---|---|---|---|---|---|---|---|---|---|---|")
    ranked = sorted(it, key=lambda r: -r["score"]["total"])
    for i, r in enumerate(ranked, start=1):
        s = r["score"]
        p("| %d | %d | %s | %s | %s | %s | %s | %s | %s | %s | **%.3f** |"
          % (i, r["iteration"], LABEL[r["name"]], s["latency"],
             s["query_power"], s["build"], s["one_file"], s["multiproc"],
             s["maint"], s["simple"], s["total"]))
    win = ranked[0]
    p("\n**Winner: iteration %d — %s**, at %.3f. It is not the fastest cell in "
      "every column, and it is not the last iteration run; it wins because it "
      "is the only design that answers all six query classes while staying "
      "one table plus indexes in one file."
      % (win["iteration"], LABEL[win["name"]], win["score"]["total"]))
    rank = "\n".join(R)

    text = open(dest).read()
    text = text.replace("<!--ITERATIONS-->", iters)
    text = text.replace("<!--RANKING-->", rank)
    open(dest, "w").write(text)
    print("rendered %d iterations; winner %s (%.3f)"
          % (len(it), win["name"], win["score"]["total"]))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
