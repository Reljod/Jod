#!/usr/bin/env python3
"""Why did two benchmarks of the same schema disagree by 64x?

`MEASURED.md` found that `FROM reach x JOIN relations r ON …` in a recursive
CTE makes SQLite pick `relations` as the *outer* loop, matched on `scope=?`
alone — a cross product per recursive step — and that `CROSS JOIN` fixes it.
This design benchmark never saw that, because it runs `ANALYZE`.

So: measure both query forms, with and without `sqlite_stat1`, on the same
database. Four cells. If the difference is statistics, the no-stats `JOIN`
cell is the slow one and the other three are fast.

Usage:  join_vs_crossjoin.py SOURCE.db --out ../out/join-vs-crossjoin.json
"""

import argparse
import json
import os
import random
import shutil
import sqlite3
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import iterations as I  # noqa: E402

UNDIRECTED = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, x.depth + 1
    FROM reach x %(j)s relations e ON e.src = x.node AND e.scope = 'default'
   WHERE x.depth < ?2
  UNION
  SELECT e.src, x.depth + 1
    FROM reach x %(j)s relations e ON e.dst = x.node AND e.scope = 'default'
   WHERE x.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""


def pct(xs, p):
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))],
                 3)


def measure(db, sql, seeds, depth, cap_s=20.0):
    lat, timeouts = [], 0
    for s in seeds:
        t = threading.Timer(cap_s, db.interrupt)
        t.start()
        t0 = time.perf_counter()
        try:
            db.execute(sql, (s, depth)).fetchall()
            lat.append((time.perf_counter() - t0) * 1000.0)
        except sqlite3.OperationalError as exc:
            if "interrupted" not in str(exc):
                raise
            timeouts += 1
            lat.append(cap_s * 1000.0)
        finally:
            t.cancel()
    out = {"p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95), "n": len(lat)}
    if timeouts:
        out["timeouts"] = timeouts
    return out


def plan(db, sql, seed, depth):
    return [r[-1] for r in
            db.execute("EXPLAIN QUERY PLAN " + sql, (seed, depth))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("source")
    ap.add_argument("--out", required=True)
    ap.add_argument("--depth", type=int, default=2)
    ap.add_argument("--samples", type=int, default=15)
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="jod-join-")
    base = os.path.join(work, "base.db")
    I.build_base(a.source, base)
    path = os.path.join(work, "t.db")
    shutil.copy(base, path)
    db = I.connect(path)
    I.design_setup("I7_scope_temporal_covering", db)   # this runs ANALYZE

    ids = [r[0] for r in db.execute("SELECT id FROM entities").fetchall()]
    rng = random.Random(11)
    seeds = [rng.choice(ids) for _ in range(a.samples)]

    res = {"source": a.source, "depth": a.depth,
           "entities": len(ids),
           "relations": db.execute(
               "SELECT count(*) FROM relations").fetchone()[0],
           "sqlite_version": sqlite3.sqlite_version,
           "scope_distribution": dict(db.execute(
               "SELECT scope, count(*) FROM relations GROUP BY scope"
           ).fetchall()),
           "cells": {}}

    for stats in ("with_ANALYZE", "without_ANALYZE"):
        if stats == "without_ANALYZE":
            db.execute("ANALYZE sqlite_schema")   # drops to an empty stat1
            db.execute("DELETE FROM sqlite_stat1")
            db.execute("PRAGMA optimize")
            db.close()
            db = I.connect(path)                  # reopen: stats are cached
        for form in ("JOIN", "CROSS JOIN"):
            sql = UNDIRECTED % {"j": form}
            key = "%s / %s" % (form, stats)
            res["cells"][key] = measure(db, sql, seeds, a.depth)
            res["cells"][key]["plan"] = plan(db, sql, seeds[0], a.depth)
            print("%-28s p50 %8s ms  p95 %8s ms"
                  % (key, res["cells"][key]["p50_ms"],
                     res["cells"][key]["p95_ms"]), flush=True)

    db.close()
    shutil.rmtree(work, ignore_errors=True)
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)


if __name__ == "__main__":
    main()
