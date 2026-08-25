#!/usr/bin/env python3
"""Run an experiment plan with bounded concurrency, resumably.

Each plan row is one (experiment, task, mode, knobs, trial) cell. Results append
to a JSONL file; a rerun skips cells already present, so a long run can be
interrupted and resumed without losing or repeating work.
"""

import argparse
import concurrent.futures as cf
import itertools
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
RUNNER = os.path.join(HERE, "runner.py")

BENCH = {
    "small": ("/home/reljod/.claude/jobs/95056dbd/tmp/bench/corpus",
              "/home/reljod/.claude/jobs/95056dbd/tmp/bench/ground_truth.json"),
    "huge": ("/home/reljod/.claude/jobs/95056dbd/tmp/bench_huge/corpus",
             "/home/reljod/.claude/jobs/95056dbd/tmp/bench_huge/ground_truth.json"),
}

_print_lock = threading.Lock()


def cell_key(c):
    return "|".join(str(c.get(k, "")) for k in
                    ("exp", "bench", "task", "mode", "workers", "brief",
                     "ret", "passes", "clean", "model", "trial"))


def build_plan(trials):
    plan = []

    def add(exp, bench, task, mode, trial, **kw):
        c = {"exp": exp, "bench": bench, "task": task, "mode": mode,
             "trial": trial, "workers": kw.get("workers", 4),
             "brief": kw.get("brief", "rich"), "ret": kw.get("ret", "thin"),
             "passes": kw.get("passes", 1), "clean": kw.get("clean", "1"),
             "model": kw.get("model", "haiku")}
        plan.append(c)

    T = range(trials)

    # E1 - does task shape decide whether delegation pays?
    for task, mode, t in itertools.product(
            ["t1_scan", "t2_join", "t3_chain", "t4_sum", "t5_classify"],
            ["solo", "deleg", "fanout"], T):
        add("E1_shape", "small", task, mode, t)

    # E2 - the over-window regime, where the corpus exceeds one context window
    for task, mode, t in itertools.product(
            ["t2_join", "t5_classify"], ["solo", "deleg"], T):
        add("E2_overwindow", "huge", task, mode, t)
    for task, w, t in itertools.product(["t2_join", "t5_classify"], [4, 8], T):
        add("E2_overwindow", "huge", task, "fanout", t, workers=w)

    # E3 - where is the knee in worker count?
    for w, t in itertools.product([1, 2, 4, 8], T):
        add("E3_workers", "small", "t5_classify", "fanout", t, workers=w)

    # E4 - how much does the worker brief matter? (MAST: most failure is spec)
    for task, brief, t in itertools.product(
            ["t2_join", "t5_classify"], ["rich", "vague"], T):
        add("E4_brief", "small", task, "fanout", t, brief=brief)

    # E5 - should a worker return an answer or its reasoning?
    for ret, t in itertools.product(["thin", "fat"], T):
        add("E5_return", "small", "t5_classify", "fanout", t, ret=ret)

    # E6 - verification passes, clean context vs shared context
    for passes, clean, t in itertools.product([1, 2], ["1", "0"], T):
        add("E6_verify", "small", "t5_classify", "verify", t,
            passes=passes, clean=clean)

    # E7 - the silent-failure case. Every configuration got t4_sum wrong in the
    # same way (an over-broad match that swept in the other topics), and a
    # scalar answer gives no partial credit and no visible symptom. The
    # question is whether a second look catches what more agents did not.
    for mode, kw, t in [("verify", {"passes": 1, "clean": "1"}, None),
                        ("verify", {"passes": 1, "clean": "0"}, None),
                        ("verify", {"passes": 2, "clean": "1"}, None),
                        ("fanout", {"workers": 4}, None)]:
        for tt in T:
            add("E7_silent", "small", "t4_sum", mode, tt, **kw)

    # E8 - the ambiguous-return case, found by accident and then made into a
    # controlled comparison. On t5 the worker's answer type (file paths) is the
    # same as its input type (file paths), so a bare list is indistinguishable
    # from the worker's own assignment and the merger discards it. Does making
    # the return self-describing fix it?
    for ret, t in itertools.product(["thin", "labelled"], range(4)):
        add("E8_labelled", "small", "t5_classify", "fanout", t, ret=ret)

    return plan


def done_keys(path):
    seen = set()
    if not os.path.exists(path):
        return seen
    for line in open(path):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "cellkey" in r:
            seen.add(r["cellkey"])
    return seen


def run_cell(c, out, timeout):
    corpus, ground = BENCH[c["bench"]]
    label = "%s/%s/%s" % (c["exp"], c["task"], c["mode"])
    if c["mode"] == "fanout":
        label += "-w%d" % c["workers"]
        if c["brief"] != "rich":
            label += "-" + c["brief"]
        if c["ret"] != "thin":
            label += "-" + c["ret"]
    if c["mode"] == "verify":
        label += "-p%d-%s" % (c["passes"], "clean" if c["clean"] == "1" else "shared")
    label += "[%s,t%d]" % (c["bench"], c["trial"])

    cmd = [sys.executable, RUNNER,
           "--corpus", corpus, "--ground", ground,
           "--task", c["task"], "--mode", c["mode"], "--model", c["model"],
           "--workers", str(c["workers"]), "--brief", c["brief"],
           "--ret", c["ret"], "--passes", str(c["passes"]),
           "--clean", c["clean"], "--trial", str(c["trial"]),
           "--label", label, "--out", out + ".raw"]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        ok = p.returncode == 0
        err = "" if ok else p.stderr[-300:]
    except subprocess.TimeoutExpired:
        ok, err = False, "driver timeout"

    rec = None
    if ok and os.path.exists(out + ".raw"):
        lines = open(out + ".raw").read().strip().splitlines()
        if lines:
            try:
                cand = json.loads(lines[-1])
                if cand.get("label") == label:
                    rec = cand
            except json.JSONDecodeError:
                pass
    if rec is None:
        rec = {"label": label, "score": 0.0, "cost_usd": 0.0,
               "wall_s": round(time.time() - t0, 1), "failed": True,
               "errors": [err or "no record"]}
        rec.update({k: c[k] for k in c})
    rec["cellkey"] = cell_key(c)
    rec["exp"] = c["exp"]
    rec["bench"] = c["bench"]

    with _print_lock:
        with open(out, "a") as fh:
            fh.write(json.dumps(rec) + "\n")
        print("%-58s score=%.3f $%.4f %4.0fs %s"
              % (label, rec.get("score", 0.0), rec.get("cost_usd", 0.0),
                 rec.get("wall_s", 0), "FAIL " + str(rec.get("errors"))
                 if rec.get("failed") or rec.get("errors") else ""),
              flush=True)
    return rec


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--timeout", type=int, default=1800)
    ap.add_argument("--only", default="", help="comma-separated exp prefixes")
    ap.add_argument("--dry", action="store_true")
    a = ap.parse_args()

    plan = build_plan(a.trials)
    if a.only:
        keep = tuple(a.only.split(","))
        plan = [c for c in plan if c["exp"].startswith(keep)]
    seen = done_keys(a.out)
    todo = [c for c in plan if cell_key(c) not in seen]

    print("plan=%d done=%d todo=%d concurrency=%d"
          % (len(plan), len(plan) - len(todo), len(todo), a.concurrency),
          flush=True)
    if a.dry:
        for c in todo:
            print("  ", cell_key(c))
        return

    t0 = time.time()
    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        futs = [ex.submit(run_cell, c, a.out, a.timeout) for c in todo]
        for _ in cf.as_completed(futs):
            pass
    print("ALL DONE in %.0f min" % ((time.time() - t0) / 60), flush=True)


if __name__ == "__main__":
    main()
