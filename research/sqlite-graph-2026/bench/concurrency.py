#!/usr/bin/env python3
"""H13 — does a graph traversal survive Jod's real write load?

Jod's shape is several supervisor processes appending events into one file
while a TUI reads it. So: 4 writer *processes* (not threads) appending events
with `BEGIN IMMEDIATE`, while this process runs 3-hop traversals. Measured
against the same traversals with no writers at all.

Usage:  concurrency.py DB.db --out ../out/concurrency-100k.json
"""

import argparse
import json
import multiprocessing as mp
import os
import random
import sqlite3
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import queries as Q  # noqa: E402
from bench import connect, pct, timed  # noqa: E402


def writer(path, ident, stop_at, out_q):
    """Append events the way a jod-run supervisor does."""
    db = connect(path)
    ok, err, seq = 0, 0, 0
    while time.time() < stop_at:
        seq += 1
        try:
            db.execute("BEGIN IMMEDIATE")
            db.execute(
                "INSERT INTO events(run_id, seq, kind, at_ms, payload) "
                "VALUES (?,?,?,?,?)",
                ("run-%d" % ident, seq, "Message",
                 int(time.time() * 1000), '{"text":"x"}'))
            db.execute("COMMIT")
            ok += 1
        except sqlite3.Error as exc:
            err += 1
            try:
                db.execute("ROLLBACK")
            except sqlite3.Error:
                pass
            if err < 3:
                print("writer %d: %s" % (ident, exc), file=sys.stderr)
    db.close()
    out_q.put({"writer": ident, "ok": ok, "errors": err})


def traversals(db, seeds, seconds):
    lat, errs = [], 0
    end = time.time() + seconds
    i = 0
    while time.time() < end:
        s = seeds[i % len(seeds)]
        i += 1
        t0 = time.perf_counter()
        try:
            db.execute(Q.KHOP_DIRECTED, (s, 3, "default")).fetchall()
            lat.append((time.perf_counter() - t0) * 1000.0)
        except sqlite3.Error:
            errs += 1
    return lat, errs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--out", required=True)
    ap.add_argument("--writers", type=int, default=4)
    ap.add_argument("--seconds", type=float, default=10.0)
    a = ap.parse_args()

    db = connect(a.db)
    rng = random.Random(5)
    ids = [r[0] for r in db.execute("SELECT id FROM nodes").fetchall()]
    seeds = [rng.choice(ids) for _ in range(200)]

    quiet_lat, quiet_err = traversals(db, seeds, a.seconds)

    q = mp.Queue()
    stop_at = time.time() + a.seconds
    procs = [mp.Process(target=writer, args=(a.db, i, stop_at, q))
             for i in range(a.writers)]
    for p in procs:
        p.start()
    busy_lat, busy_err = traversals(db, seeds, a.seconds)
    wstats = [q.get() for _ in procs]
    for p in procs:
        p.join()

    res = {
        "db": a.db,
        "writers": a.writers,
        "seconds_each_phase": a.seconds,
        "quiet": {"n": len(quiet_lat), "p50_ms": pct(quiet_lat, 50),
                  "p95_ms": pct(quiet_lat, 95), "errors": quiet_err},
        "under_write_load": {"n": len(busy_lat), "p50_ms": pct(busy_lat, 50),
                             "p95_ms": pct(busy_lat, 95), "errors": busy_err},
        "writer_processes": wstats,
        "writes_ok": sum(w["ok"] for w in wstats),
        "writes_failed": sum(w["errors"] for w in wstats),
    }
    res["p50_slowdown"] = (
        round(res["under_write_load"]["p50_ms"] / res["quiet"]["p50_ms"], 2)
        if res["quiet"]["p50_ms"] else None)
    db.close()
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
