#!/usr/bin/env python3
"""Does the position of an instruction in a long context change obedience?

This tests the recitation technique (B6/H27) directly. The agent is given one
long document inline and one output constraint. The only thing that varies is
where the constraint sits:

  early  stated once, before the document
  late   stated once, after the document, just before the question
  both   stated in both places - the recitation condition
  none   control: no constraint at all, to confirm the constraint is what is
         being measured rather than the task being impossible

The document is passed inline on stdin rather than read from files, because the
whole point is to control the constraint's position in the context window, which
tool-driven reading would not let us do.
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
import concurrent.futures as cf
import threading

CONSTRAINT = ("IMPORTANT OUTPUT RULE: every code you report must be written "
              "with the prefix ZZ- added in front of it. For example the code "
              "AL-1234 must be reported as ZZ-AL-1234. This rule applies to "
              "every single code in your answer.")

QUESTION = ("List every code that appears on a FACT[alpha] line in the document "
            "above.\n"
            "End your reply with exactly one line:\n"
            "ANSWER: <comma-separated codes>")


def build_document(corpus, target_words, seed):
    rng = random.Random(seed)
    paths = []
    for root, _, fns in os.walk(corpus):
        for fn in fns:
            if fn.endswith(".md"):
                paths.append(os.path.join(root, fn))
    rng.shuffle(paths)
    out, words, codes = [], 0, []
    for p in paths:
        body = open(p).read()
        codes += re.findall(r"FACT\[alpha\]:\s*([A-Z]{2}-\d{4})", body)
        out.append("=== %s ===\n%s" % (os.path.relpath(p, corpus), body))
        words += len(body.split())
        if words >= target_words:
            break
    return "\n\n".join(out), sorted(set(codes)), words


def build_prompt(doc, where):
    head = (CONSTRAINT + "\n\n") if where in ("early", "both") else ""
    tail = (CONSTRAINT + "\n\n") if where in ("late", "both") else ""
    return ("%sYou will be shown a long document, then asked a question about "
            "it.\n\n--- BEGIN DOCUMENT ---\n%s\n--- END DOCUMENT ---\n\n%s%s"
            % (head, doc, tail, QUESTION))


def run(prompt, model="haiku", timeout=900):
    t0 = time.time()
    p = subprocess.run(["claude", "-p", "--model", model,
                        "--output-format", "json", "--allowedTools", "Read"],
                       input=prompt, capture_output=True, text=True,
                       timeout=timeout)
    if p.returncode != 0:
        return None, 0.0, time.time() - t0
    try:
        d = json.loads(p.stdout)
    except json.JSONDecodeError:
        return None, 0.0, time.time() - t0
    return (d.get("result", ""), d.get("total_cost_usd", 0.0) or 0.0,
            time.time() - t0)


def score(text, want_codes, expect_prefix):
    m = re.findall(r"^\s*ANSWER:\s*(.*)$", text or "", re.MULTILINE)
    ans = m[-1] if m else (text or "")
    prefixed = re.findall(r"ZZ-([A-Z]{2}-\d{4})", ans)
    bare_all = re.findall(r"([A-Z]{2}-\d{4})", ans)
    # A prefixed hit also matches the bare pattern, so subtract to get the
    # codes that were reported WITHOUT the required prefix.
    bare_only = len(bare_all) - len(prefixed)
    total = len(bare_all)
    found = set(bare_all)
    recall = len(found & set(want_codes)) / len(want_codes) if want_codes else 0
    adherence = (len(prefixed) / total) if total else 0.0
    if not expect_prefix:
        adherence = float("nan")
    return {"n_reported": total, "n_prefixed": len(prefixed),
            "n_unprefixed": bare_only, "adherence": adherence,
            "recall": recall}


_lock = threading.Lock()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--words", type=int, default=30000)
    ap.add_argument("--trials", type=int, default=4)
    ap.add_argument("--concurrency", type=int, default=1)
    a = ap.parse_args()

    doc, codes, words = build_document(a.corpus, a.words, seed=5)
    print("document: %d words (~%d tokens), %d alpha codes planted"
          % (words, int(words * 1.35), len(codes)), flush=True)

    done = set()
    if os.path.exists(a.out):
        for line in open(a.out):
            try:
                d = json.loads(line)
                done.add((d["where"], d["trial"]))
            except (json.JSONDecodeError, KeyError):
                pass

    jobs = [(w, t) for w in ("early", "late", "both", "none")
            for t in range(a.trials) if (w, t) not in done]

    def work(job):
        where, trial = job
        prompt = build_prompt(doc, where)
        text, cost, wall = run(prompt)
        if text is None:
            with _lock:
                print("  %-6s t%d FAILED" % (where, trial), flush=True)
            return
        sc = score(text, codes, where != "none")
        rec = {"where": where, "trial": trial, "words": words,
               "cost_usd": round(cost, 5), "wall_s": round(wall, 1)}
        rec.update(sc)
        with _lock:
            with open(a.out, "a") as fh:
                fh.write(json.dumps(rec) + "\n")
            print("  %-6s t%d adherence=%s recall=%.2f reported=%d"
                  % (where, trial,
                     "n/a" if sc["adherence"] != sc["adherence"]
                     else "%.2f" % sc["adherence"],
                     sc["recall"], sc["n_reported"]), flush=True)

    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        list(ex.map(work, jobs))

    # summary
    import statistics as st
    rows = [json.loads(l) for l in open(a.out)]
    print()
    print("%-8s %3s %11s %9s %10s" % ("where", "n", "adherence", "recall",
                                      "unprefixed"))
    print("-" * 46)
    for w in ("early", "late", "both", "none"):
        v = [r for r in rows if r["where"] == w]
        if not v:
            continue
        ad = [r["adherence"] for r in v if r["adherence"] == r["adherence"]]
        print("%-8s %3d %11s %9.3f %10.1f"
              % (w, len(v), "%.3f" % st.mean(ad) if ad else "n/a",
                 st.mean([r["recall"] for r in v]),
                 st.mean([r["n_unprefixed"] for r in v])))


if __name__ == "__main__":
    main()
