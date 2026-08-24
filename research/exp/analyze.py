#!/usr/bin/env python3
"""Aggregate experiment results into per-hypothesis tables."""

import argparse
import json
import math
import re
import statistics as st
from collections import defaultdict


def load(path):
    out = []
    for line in open(path):
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return out


def is_format_fail(r):
    """An answer that contains data but not in the required shape.

    This is worth separating from a wrong answer: the agent may have done the
    work correctly and then failed to write it down in the agreed form. Treating
    the two as the same failure hides which one you actually need to fix.
    """
    a = r.get("answer", "") or ""
    if r.get("score", 0) > 0:
        return False
    if r.get("task") in ("t1_scan", "t2_join"):
        # numbers present, but no CODE=NUM pair parsed
        has_pairs = re.search(r"[A-Z]{2}-\d{4}\s*=\s*\d+", a)
        has_numbers = len(re.findall(r"=\s*\d+", a)) >= 3
        return bool(has_numbers and not has_pairs)
    if r.get("task") == "t5_classify":
        return bool(re.search(r"note_\d", a) is None and "note" in a.lower())
    return False


def agg(rows, key):
    vals = [r[key] for r in rows if isinstance(r.get(key), (int, float))]
    if not vals:
        return 0.0, 0.0
    m = st.mean(vals)
    s = st.stdev(vals) if len(vals) > 1 else 0.0
    return m, s


def fmt_group(rows):
    sc, ss = agg(rows, "score")
    co, _ = agg(rows, "cost_usd")
    wa, _ = agg(rows, "wall_s")
    tu, _ = agg(rows, "turns")
    ff = sum(1 for r in rows if is_format_fail(r))
    ex = sum(1 for r in rows if r.get("exact"))
    dup = [r["dup_rate"] for r in rows if "dup_rate" in r]
    return {
        "n": len(rows), "score": sc, "sd": ss, "exact": ex,
        "cost": co, "wall": wa, "turns": tu, "fmt_fail": ff,
        "dup": st.mean(dup) if dup else None,
        "cost_per_point": (co / sc) if sc > 0.01 else float("inf"),
        "spawned": st.mean([r.get("spawned", 0) for r in rows]),
    }


def table(title, groups, order=None, note=""):
    print()
    print("### " + title)
    if note:
        print(note)
    print()
    hdr = ("%-30s %3s %7s %6s %6s %9s %9s %7s %6s"
           % ("config", "n", "score", "sd", "exact", "cost$", "$/point",
              "wall_s", "fmtF"))
    print(hdr)
    print("-" * len(hdr))
    keys = order or sorted(groups)
    for k in keys:
        if k not in groups:
            continue
        g = groups[k]
        cpp = ("%9.4f" % g["cost_per_point"]
               if g["cost_per_point"] != float("inf") else "        -")
        dup = ("  dup=%.2f" % g["dup"]) if g["dup"] is not None else ""
        spw = ("  spawned=%.1f" % g["spawned"]) if g.get("spawned") else ""
        print("%-30s %3d %7.3f %6.3f %6d %9.4f %s %7.0f %6d%s%s"
              % (k, g["n"], g["score"], g["sd"], g["exact"], g["cost"],
                 cpp, g["wall"], g["fmt_fail"], dup, spw))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--judges", default="")
    ap.add_argument("--no-rescore", action="store_true")
    a = ap.parse_args()
    rows = load(a.results)
    rows = [r for r in rows if not r.get("failed")]
    print("loaded %d successful runs" % len(rows))

    # Rescore from the stored answer text using the current parser, so a parser
    # fix applies to runs already collected instead of forcing a costly rerun.
    if not a.no_rescore:
        import sys as _s
        import os as _o
        _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
        import tasks as _T
        grounds = {}
        for name, p in (("small", "/home/reljod/.claude/jobs/95056dbd/tmp/"
                                  "bench/ground_truth.json"),
                        ("huge", "/home/reljod/.claude/jobs/95056dbd/tmp/"
                                 "bench_huge/ground_truth.json")):
            try:
                grounds[name] = json.load(open(p))
            except OSError:
                pass
        changed = 0
        for r in rows:
            g = grounds.get(r.get("bench", "small"))
            if not g or not r.get("answer"):
                continue
            try:
                new = _T.score(r["task"], "ANSWER: " + r["answer"], g)
            except (KeyError, TypeError):
                continue
            if abs(new["score"] - r.get("score", 0)) > 1e-6:
                changed += 1
            r.update(new)
        if changed:
            print("rescored %d runs whose stored answer parses differently "
                  "under the current parser" % changed)
    tot_cost = sum(r.get("cost_usd", 0) for r in rows)
    print("total spend on runs: $%.2f" % tot_cost)

    by = lambda f: _group(rows, f)  # noqa: E731

    def _group(rs, fn):
        d = defaultdict(list)
        for r in rs:
            k = fn(r)
            if k is not None:
                d[k].append(r)
        return {k: fmt_group(v) for k, v in d.items()}

    # ---- E1: task shape x mode
    e1 = [r for r in rows if r.get("exp") == "E1_shape"]
    for task in ["t1_scan", "t2_join", "t3_chain", "t4_sum", "t5_classify"]:
        sub = [r for r in e1 if r["task"] == task]
        if sub:
            table("E1 %s (%s)" % (task, sub[0].get("shape", "")),
                  _group(sub, lambda r: r["mode"]),
                  order=["solo", "deleg", "fanout"])

    # ---- E2: over-window
    e2 = [r for r in rows if r.get("exp") == "E2_overwindow"]
    for task in sorted({r["task"] for r in e2}):
        sub = [r for r in e2 if r["task"] == task]
        table("E2 over-window %s (400-file corpus, ~316k tokens)" % task,
              _group(sub, lambda r: r["mode"] + (
                  "-w%d" % r["workers"] if r["mode"] == "fanout" else "")))

    # ---- E3: worker count
    e3 = [r for r in rows if r.get("exp") == "E3_workers"]
    if e3:
        table("E3 worker count (t5_classify, small corpus)",
              _group(e3, lambda r: "fanout-w%d" % r["workers"]),
              order=["fanout-w1", "fanout-w2", "fanout-w4", "fanout-w8"],
              note="H5 predicts an early knee; H6 predicts superlinear cost.")

    # ---- E4: brief quality
    e4 = [r for r in rows if r.get("exp") == "E4_brief"]
    for task in sorted({r["task"] for r in e4}):
        sub = [r for r in e4 if r["task"] == task]
        table("E4 worker brief quality (%s)" % task,
              _group(sub, lambda r: "brief=" + r["brief"]),
              note="H13/H14: does an explicit boundary change accuracy and overlap?")

    # ---- E5: return size
    e5 = [r for r in rows if r.get("exp") == "E5_return"]
    if e5:
        table("E5 worker return size (t5_classify)",
              _group(e5, lambda r: "ret=" + r["ret"]),
              note="H15/H16: does returning reasoning instead of an answer help?")

    # ---- E6: verification
    e6 = [r for r in rows if r.get("exp") == "E6_verify"]
    solo5 = [r for r in rows if r.get("exp") == "E1_shape"
             and r["task"] == "t5_classify" and r["mode"] == "solo"]
    if e6:
        g = _group(e6, lambda r: "verify-p%d-%s" % (
            r["passes"], "clean" if str(r["clean"]) == "1" else "shared"))
        if solo5:
            g["solo (no verify)"] = fmt_group(solo5)
        table("E6 verification (t5_classify)", g,
              order=["solo (no verify)", "verify-p1-clean", "verify-p2-clean",
                     "verify-p1-shared", "verify-p2-shared"],
              note="H33-H36: clean-context critic vs shared-context self-review.")

    # ---- cost structure (H6, H7, H29, H30)
    print()
    print("### Token structure by mode (H6, H7, H23, H29, H30)")
    print()
    hdr = ("%-26s %3s %10s %10s %10s %10s %8s"
           % ("mode", "n", "out_tok", "in_tok", "cache_rd", "cache_cr",
              "calls"))
    print(hdr)
    print("-" * len(hdr))
    d = defaultdict(list)
    for r in rows:
        k = r["mode"] + ("-w%d" % r["workers"] if r["mode"] == "fanout" else "")
        d[k].append(r)
    for k in sorted(d):
        v = d[k]
        print("%-26s %3d %10.0f %10.0f %10.0f %10.0f %8.1f"
              % (k, len(v),
                 st.mean([x.get("out_tokens", 0) for x in v]),
                 st.mean([x.get("in_tokens", 0) for x in v]),
                 st.mean([x.get("cache_read", 0) for x in v]),
                 st.mean([x.get("cache_create", 0) for x in v]),
                 st.mean([x.get("n_calls", 1) for x in v])))

    # ---- judges
    if a.judges:
        judge_report(a.judges)


def spearman(xs, ys):
    def rank(v):
        order = sorted(range(len(v)), key=lambda i: v[i])
        r = [0.0] * len(v)
        i = 0
        while i < len(order):
            j = i
            while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
                j += 1
            avg = (i + j) / 2.0 + 1
            for k in range(i, j + 1):
                r[order[k]] = avg
            i = j + 1
        return r
    if len(xs) < 3:
        return float("nan")
    rx, ry = rank(xs), rank(ys)
    mx, my = st.mean(rx), st.mean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    den = math.sqrt(sum((a - mx) ** 2 for a in rx)
                    * sum((b - my) ** 2 for b in ry))
    return num / den if den else float("nan")


def judge_report(path):
    rows = load(path)
    print()
    print("### Judge panel: how well does each judge track the objective score?")
    print()
    print("Every answer already has a true score from generated ground truth,")
    print("so the judges are what is being measured here, not the agents.")
    print()
    hdr = ("%-18s %4s %9s %9s %9s %7s"
           % ("condition", "n", "spearman", "mean_abs", "judge_mu", "unparsed"))
    print(hdr)
    print("-" * len(hdr))
    by = defaultdict(list)
    for r in rows:
        by[r["condition"]].append(r)
    per_item = defaultdict(dict)
    for cond in sorted(by):
        v = [r for r in by[cond] if r.get("judge_score") is not None]
        bad = len(by[cond]) - len(v)
        if not v:
            print("%-18s %4d  (no parseable verdicts)" % (cond, len(by[cond])))
            continue
        xs = [r["true_score"] * 100 for r in v]
        ys = [r["judge_score"] for r in v]
        mae = st.mean([abs(x - y) for x, y in zip(xs, ys)])
        print("%-18s %4d %9.3f %9.1f %9.1f %7d"
              % (cond, len(v), spearman(xs, ys), mae, st.mean(ys), bad))
        for r in v:
            per_item[r["label"]][cond] = r["judge_score"]

    conds = sorted(by)
    print()
    print("Pairwise judge agreement (mean absolute difference in points):")
    for i in range(len(conds)):
        for j in range(i + 1, len(conds)):
            a_, b_ = conds[i], conds[j]
            both = [(v[a_], v[b_]) for v in per_item.values()
                    if a_ in v and b_ in v]
            if len(both) >= 3:
                print("  %-18s vs %-18s n=%2d  mad=%5.1f"
                      % (a_, b_, len(both),
                         st.mean([abs(x - y) for x, y in both])))


if __name__ == "__main__":
    main()
