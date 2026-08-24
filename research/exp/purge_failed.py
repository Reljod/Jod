#!/usr/bin/env python3
"""Drop records whose failure was infrastructure, not the strategy under test.

A cell that never produced parseable output tells us nothing about the
orchestration strategy it was meant to measure. Leaving it in as a zero would
be an artefact: it would make whichever configuration happened to hit a flaky
call look worse than it is. Removing it lets the resumable driver re-run that
exact cell.

Records that produced a genuine bad answer are kept - those are results.
"""
import json
import shutil
import sys

path = sys.argv[1]
dry = "--apply" not in sys.argv

keep, drop = [], []
for line in open(path):
    try:
        r = json.loads(line)
    except json.JSONDecodeError:
        continue
    errs = " ".join(str(e) for e in (r.get("errors") or []))
    infra = ("unparseable" in errs or "timeout" in errs
             or errs.startswith("exit") or r.get("failed"))
    # An empty answer alongside an infrastructure error means nothing was
    # measured. An empty answer with no error is a real (bad) result.
    if infra and not r.get("answer"):
        drop.append(r)
    else:
        keep.append(r)

print("keep=%d  drop=%d" % (len(keep), len(drop)))
for r in drop:
    print("  drop %-52s %s" % (r.get("label"), str(r.get("errors"))[:80]))

if dry:
    print("\ndry run; pass --apply to rewrite")
else:
    shutil.copy(path, path + ".bak")
    with open(path, "w") as fh:
        for r in keep:
            fh.write(json.dumps(r) + "\n")
    print("\nrewrote %s (backup at %s.bak); re-run the driver to refill"
          % (path, path))
