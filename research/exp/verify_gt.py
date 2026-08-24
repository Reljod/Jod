#!/usr/bin/env python3
"""Recompute ground truth straight from the written corpus.

The generator records what it *intended* to plant. This reads what actually
ended up on disk. If the two disagree, the benchmark is measuring the wrong
thing and any score built on it is meaningless.
"""
import json
import os
import re
import sys

corpus, gt_path = sys.argv[1], sys.argv[2]
gt = json.load(open(gt_path))

alpha, policy, safe = {}, {}, {}
for root, _, fns in os.walk(corpus):
    for fn in sorted(fns):
        if not fn.endswith(".md"):
            continue
        p = os.path.relpath(os.path.join(root, fn), corpus)
        body = open(os.path.join(root, fn)).read()
        m = re.search(r"^POLICY:\s*(\w+)", body, re.MULTILINE)
        policy[p] = (m.group(1) == "enabled") if m else None
        for c, n in re.findall(r"FACT\[alpha\]:\s*([A-Z]{2}-\d{4})=(\d+)", body):
            alpha[c] = (int(n), p)

join = {c: n for c, (n, p) in alpha.items() if policy.get(p)}
observed_sum = sum(join.values())

print("alpha facts on disk      : %d   (ground truth says %d)"
      % (len(alpha), len(gt["facts"]["alpha"])))
print("policy-enabled alpha     : %d   (ground truth says %d)"
      % (len(join), len(gt["join_alpha_enabled"])))
print("sum of those             : %d   (ground truth says %d)"
      % (observed_sum, gt["aggregate_sum"]))
print("sum of ALL alpha facts   : %d" % sum(n for n, _ in alpha.values()))

ok = True
if len(alpha) != len(gt["facts"]["alpha"]):
    print("MISMATCH: fact count"); ok = False
if join != gt["join_alpha_enabled"]:
    print("MISMATCH: join set")
    only_disk = set(join) - set(gt["join_alpha_enabled"])
    only_gt = set(gt["join_alpha_enabled"]) - set(join)
    if only_disk:
        print("  on disk but not in ground truth:", sorted(only_disk)[:10])
    if only_gt:
        print("  in ground truth but not on disk:", sorted(only_gt)[:10])
    ok = False
if observed_sum != gt["aggregate_sum"]:
    print("MISMATCH: aggregate sum"); ok = False
print("VERDICT:", "ground truth matches the corpus" if ok
      else "GROUND TRUTH IS WRONG - fix before trusting any score")
