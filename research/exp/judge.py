#!/usr/bin/env python3
"""Independent judge panel over agent answers whose true score is already known.

Because the benchmark is synthetic, every answer already has an objective score.
That lets us turn the usual question around: instead of using a judge to grade
agents, we grade the judges. A judge is useful only insofar as its score tracks
the objective one.

Conditions
  claude_open      same model family as the author, open rubric
  agy_open         different model family (Gemini), open rubric
  claude_taxonomy  same family, fixed checklist rubric
  agy_taxonomy     different family, fixed checklist rubric
  agy_grounded     different family, and allowed to inspect the corpus itself

The last condition is the control that separates "judging" from "checking".
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import concurrent.futures as cf

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tasks as T  # noqa: E402

OPEN_RUBRIC = """Rate how correct and complete the answer is, from 0 to 100.
100 means every required item is present and every value is right.
0 means the answer is wrong or empty."""

TAXONOMY_RUBRIC = """Score the answer by working through this fixed checklist,
which is the same for every answer you see:

  1. FORMAT     Is there exactly one ANSWER line in the required shape?
  2. COMPLETE   Does the answer look like it covers the whole task, or does its
                size suggest whole sections were missed?
  3. PRECISE    Are there items that look invented, malformed, or duplicated?
  4. CONSISTENT Do the items agree with each other and with the task's rules?

Give each of the four a mark out of 25, then report the total from 0 to 100."""

TAIL = """
Reply with ONLY this line and nothing else:
VERDICT: <integer 0-100>
"""


def build_prompt(rec, task_obj, rubric, grounded):
    where = ("You may read the files in the current directory to check the\n"
             "answer against them before scoring.\n\n" if grounded else
             "You cannot see the source files. Judge from the answer itself.\n\n")
    return (
        "You are grading another agent's answer to a task.\n\n"
        + where +
        "THE TASK THE AGENT WAS GIVEN:\n%s\n\n"
        "THE AGENT'S ANSWER:\n%s\n\n"
        "HOW TO SCORE:\n%s\n%s" % (task_obj, rec.get("answer", "")[:2500],
                                   rubric, TAIL))


def parse_verdict(text):
    m = re.findall(r"VERDICT:\s*(\d{1,3})", text or "")
    if m:
        return max(0, min(100, int(m[-1])))
    m = re.findall(r"\b(\d{1,3})\b", text or "")
    return max(0, min(100, int(m[-1]))) if m else None


def call_claude(prompt, cwd, grounded):
    cmd = ["claude", "-p", prompt, "--model", "haiku",
           "--output-format", "json"]
    cmd += ["--allowedTools"] + (["Read", "Grep", "Glob", "Bash"] if grounded
                                 else ["Read"])
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=600)
    if p.returncode != 0:
        return None, 0.0
    d = json.loads(p.stdout)
    return d.get("result", ""), d.get("total_cost_usd", 0.0) or 0.0


def call_agy(prompt, cwd, grounded):
    cmd = ["agy", "-p", prompt, "--model", "gemini-3.7-flash-low",
           "--output-format", "json"]
    if grounded:
        cmd.append("--dangerously-skip-permissions")
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=600)
    if p.returncode != 0:
        return None, 0.0
    try:
        d = json.loads(p.stdout)
    except json.JSONDecodeError:
        return None, 0.0
    return d.get("response", ""), 0.0


CONDITIONS = {
    "claude_open":     (call_claude, OPEN_RUBRIC, False),
    "agy_open":        (call_agy, OPEN_RUBRIC, False),
    "claude_taxonomy": (call_claude, TAXONOMY_RUBRIC, False),
    "agy_taxonomy":    (call_agy, TAXONOMY_RUBRIC, False),
    "agy_grounded":    (call_agy, TAXONOMY_RUBRIC, True),
}

BENCH = {
    "small": ("/home/reljod/.claude/jobs/95056dbd/tmp/bench/corpus",
              "/home/reljod/.claude/jobs/95056dbd/tmp/bench/ground_truth.json"),
    "huge": ("/home/reljod/.claude/jobs/95056dbd/tmp/bench_huge/corpus",
             "/home/reljod/.claude/jobs/95056dbd/tmp/bench_huge/ground_truth.json"),
}


def sample_records(path, n):
    """Spread the sample across the observed score range, so judges are not
    handed only good answers (which would make any judge look calibrated)."""
    recs = []
    for line in open(path):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if r.get("failed") or not r.get("answer"):
            continue
        recs.append(r)
    recs.sort(key=lambda r: r.get("score", 0))
    if len(recs) <= n:
        return recs
    step = len(recs) / float(n)
    return [recs[int(i * step)] for i in range(n)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--n", type=int, default=24)
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--conditions", default=",".join(CONDITIONS))
    a = ap.parse_args()

    recs = sample_records(a.results, a.n)
    grounds = {k: json.load(open(v[1])) for k, v in BENCH.items()}
    conds = a.conditions.split(",")
    print("judging %d answers x %d conditions" % (len(recs), len(conds)),
          flush=True)

    done = set()
    if os.path.exists(a.out):
        for line in open(a.out):
            try:
                d = json.loads(line)
                done.add((d["label"], d["condition"]))
            except (json.JSONDecodeError, KeyError):
                pass

    jobs = []
    for r in recs:
        for c in conds:
            if (r["label"], c) not in done:
                jobs.append((r, c))

    lock = __import__("threading").Lock()

    def work(job):
        r, cname = job
        fn, rubric, grounded = CONDITIONS[cname]
        bench = r.get("bench", "small")
        cwd = BENCH[bench][0]
        obj = T.prompt_for(r["task"], grounds[bench])
        prompt = build_prompt(r, obj, rubric, grounded)
        t0 = time.time()
        try:
            text, cost = fn(prompt, cwd, grounded)
        except Exception as e:                       # noqa: BLE001
            text, cost = None, 0.0
            print("  judge error %s/%s: %s" % (cname, r["label"], e), flush=True)
        v = parse_verdict(text)
        rec = {"label": r["label"], "task": r["task"], "bench": bench,
               "mode": r.get("mode"), "condition": cname,
               "true_score": round(r.get("score", 0.0), 4),
               "judge_score": v, "wall_s": round(time.time() - t0, 1),
               "cost_usd": round(cost, 5)}
        with lock:
            with open(a.out, "a") as fh:
                fh.write(json.dumps(rec) + "\n")
            print("  %-16s %-46s true=%.2f judge=%s"
                  % (cname, r["label"][:46], rec["true_score"],
                     v if v is not None else "?"), flush=True)

    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        list(ex.map(work, jobs))
    print("judging done", flush=True)


if __name__ == "__main__":
    main()
