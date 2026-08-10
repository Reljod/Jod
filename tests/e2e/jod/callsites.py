#!/usr/bin/env python3
"""Classify every call site of a name as production or test.

`wiring.py` finds candidates; this says whether the only callers are tests,
which is the shape that matters — a function with passing unit tests and no
production caller looks healthy in CI and does nothing in the product.

    callsites.py <source-root> <name> [name ...]
"""
import os
import re
import sys

root = sys.argv[1]
names = sys.argv[2:]


def test_line(path):
    """First line number inside the file's test module, or None."""
    src = open(path, encoding="utf-8").read()
    m = re.search(r"^#\[cfg\(test\)\]", src, re.M)
    return src[: m.start()].count("\n") + 1 if m else None


files = []
for d in ("core/src", "cli/src", "api/src", "supervisor/src"):
    for base, _, fs in os.walk(os.path.join(root, d)):
        files += [os.path.join(base, f) for f in fs if f.endswith(".rs")]

for name in names:
    prod, test, defined = [], [], []
    for p in files:
        cut = test_line(p)
        rel = os.path.relpath(p, root)
        for i, line in enumerate(open(p, encoding="utf-8"), 1):
            if not re.search(r"\b" + re.escape(name) + r"\b", line):
                continue
            if re.match(r"\s*(pub )?(async )?fn " + re.escape(name) + r"\b", line):
                defined.append(f"{rel}:{i}")
                continue
            if line.lstrip().startswith("//"):
                continue  # doc comment, not a call
            (test if cut and i > cut else prod).append(f"{rel}:{i}")
    verdict = "WIRED" if prod else ("TESTS ONLY" if test else "no references")
    print(f"{name:24} {verdict:12} production={len(prod):3} tests={len(test):3}")
    for hit in prod[:4]:
        print(f"    prod: {hit}")
