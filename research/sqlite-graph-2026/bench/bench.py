#!/usr/bin/env python3
"""Measure the graph queries against a generated memory graph.

Every hypothesis in ../HYPOTHESES.md is decided by a number this prints.
Output is one JSON document per scale, into ../out/.

Usage:  bench.py DB.db --label 100k --out ../out/sqlite-100k.json
"""

import argparse
import json
import os
import random
import sqlite3
import statistics
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import queries as Q  # noqa: E402

NOW_MS = 1786000000000
ASOF_MS = NOW_MS - 400 * 24 * 3600 * 1000   # a little over a year ago


def connect(path, cache_kb=None):
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA busy_timeout = 5000")
    db.execute("PRAGMA synchronous = NORMAL")
    if cache_kb is not None:
        db.execute("PRAGMA cache_size = -%d" % cache_kb)
    return db


def save(res, path):
    """Write partial results as we go; a late crash must not cost the run."""
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as fh:
        json.dump(res, fh, indent=2)


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return round(xs[k], 3)


def timed(fn, samples, db=None, cap_s=10.0):
    """Run fn once per sample; latencies in ms, plus result sizes.

    A query that exceeds `cap_s` is interrupted and counted as a timeout
    rather than being allowed to stall the sweep — an unbounded traversal is
    a finding, not a measurement to wait for.
    """
    lat, sizes, timeouts = [], [], 0
    for s in samples:
        timer = None
        if db is not None:
            timer = threading.Timer(cap_s, db.interrupt)
            timer.start()
        t0 = time.perf_counter()
        try:
            n = fn(s)
            lat.append((time.perf_counter() - t0) * 1000.0)
            sizes.append(n)
        except sqlite3.OperationalError as exc:
            if "interrupted" not in str(exc):
                raise
            timeouts += 1
            lat.append(cap_s * 1000.0)
        finally:
            if timer is not None:
                timer.cancel()
    out = {
        "n": len(lat),
        "p50_ms": pct(lat, 50),
        "p95_ms": pct(lat, 95),
        "max_ms": pct(lat, 100),
        "mean_rows": round(statistics.mean(sizes), 1) if sizes else 0,
        "max_rows": max(sizes) if sizes else 0,
    }
    if timeouts:
        out["timeouts"] = timeouts
    return out


def pick_seeds(db, n_random=120, n_hub=30):
    """Random nodes, and the highest-degree nodes — the hard case."""
    rng = random.Random(11)
    ids = [r[0] for r in db.execute("SELECT id FROM nodes").fetchall()]
    rand = [rng.choice(ids) for _ in range(n_random)]
    hubs = [r[0] for r in db.execute(
        "SELECT node, count(*) c FROM ("
        "  SELECT src AS node FROM edges UNION ALL SELECT dst FROM edges"
        ") GROUP BY node ORDER BY c DESC LIMIT ?", (n_hub,)).fetchall()]
    return ids, rand, hubs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--label", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--cache-kb", type=int, default=None)
    ap.add_argument("--max-depth", type=int, default=4)
    a = ap.parse_args()

    db = connect(a.db, a.cache_kb)
    res = {"label": a.label, "db": a.db, "cache_kb": a.cache_kb,
           "sqlite_version": sqlite3.sqlite_version}

    res["nodes"] = db.execute("SELECT count(*) FROM nodes").fetchone()[0]
    res["edges"] = db.execute("SELECT count(*) FROM edges").fetchone()[0]
    res["facts"] = db.execute("SELECT count(*) FROM facts").fetchone()[0]
    res["bytes"] = sum(os.path.getsize(a.db + s) for s in ("", "-wal")
                       if os.path.exists(a.db + s))

    # Size breakdown, so H8 is answered with real page counts.
    try:
        rows = db.execute(
            "SELECT name, sum(pgsize) FROM dbstat GROUP BY name "
            "ORDER BY 2 DESC").fetchall()
        res["size_by_object"] = {r[0]: r[1] for r in rows}
    except sqlite3.OperationalError:
        res["size_by_object"] = None

    # Fewer samples as the graph grows: a 4-hop undirected walk over a
    # million edges is seconds, not microseconds, and 120 of them prove
    # nothing 20 do not.
    big = res["edges"] >= 500_000
    ids, rand_seeds, hubs = pick_seeds(
        db, n_random=40 if big else 120, n_hub=10 if big else 30)
    res["hub_degrees"] = db.execute(
        "SELECT max(c), min(c), avg(c) FROM ("
        "  SELECT count(*) c FROM ("
        "    SELECT src AS node FROM edges UNION ALL SELECT dst FROM edges"
        "  ) GROUP BY node)").fetchone()

    # ---- H1/H2/H3/H4: k-hop -------------------------------------------
    khop = {}
    for k in range(1, a.max_depth + 1):
        for name, sql in (("directed", Q.KHOP_DIRECTED),
                          ("undirected", Q.KHOP_UNDIRECTED)):
            for who, seeds in (("random", rand_seeds), ("hub", hubs)):
                # A 4-hop undirected walk from a hub can return the whole
                # graph; cap the sample count so the sweep still finishes.
                s = seeds if k <= 3 else seeds[:20]
                key = "%s.k%d.%s" % (name, k, who)
                khop[key] = timed(
                    lambda seed, sql=sql, k=k: len(
                        db.execute(sql, (seed, k, "default")).fetchall()),
                    s, db)
    res["khop"] = khop
    res["node_count_for_coverage"] = res["nodes"]
    save(res, a.out)

    # ---- H6: temporal traversal ----------------------------------------
    tmp = {}
    for who, seeds in (("random", rand_seeds), ("hub", hubs)):
        tmp["asof.k3.%s" % who] = timed(
            lambda seed: len(db.execute(
                Q.KHOP_ASOF, (seed, 3, "default", ASOF_MS)).fetchall()),
            seeds, db)
    res["temporal"] = tmp
    save(res, a.out)

    # ---- H5: shortest path ---------------------------------------------
    rng = random.Random(23)
    pairs = [(rng.choice(ids), rng.choice(ids))
             for _ in range(20 if big else 60)]
    sp = {}
    sp["cte_distance_d5"] = timed(
        lambda p: len(db.execute(Q.SP_DISTANCE, (p[0], p[1], 5)).fetchall()),
        pairs, db)
    sp["cte_path_d5"] = timed(
        lambda p: len(db.execute(Q.SP_PATH, (p[0], p[1], 5)).fetchall()),
        pairs[:20], db)

    # The frontier goes through a temp table rather than an `IN (?,?,...)`
    # list: a hub's neighbourhood is tens of thousands of ids, and SQLite's
    # bound-parameter ceiling is 32,766. This is also what the Rust version
    # would have to do.
    db.execute("CREATE TEMP TABLE IF NOT EXISTS frontier(node INTEGER "
               "PRIMARY KEY)")

    def bidi(p, maxd=6):
        """Application-side bidirectional BFS: one indexed query per level."""
        a_, b_ = p
        if a_ == b_:
            return 1
        fa, fb = {a_}, {b_}
        seen_a, seen_b = {a_}, {b_}
        for d in range(1, maxd + 1):
            side = fa if len(fa) <= len(fb) else fb
            seen = seen_a if side is fa else seen_b
            other = seen_b if side is fa else seen_a
            db.execute("DELETE FROM frontier")
            db.executemany("INSERT INTO frontier VALUES (?)",
                           [(n,) for n in side])
            nxt = {r[0] for r in db.execute(Q.BFS_LEVEL_TEMP).fetchall()}
            nxt -= seen
            if nxt & other:
                return d
            seen |= nxt
            if side is fa:
                fa = nxt
            else:
                fb = nxt
            if not nxt:
                return 0
        return 0
    sp["bidi_bfs_d6"] = timed(bidi, pairs, db)
    res["shortest_path"] = sp
    save(res, a.out)

    # ---- H7: hybrid FTS5 + graph ---------------------------------------
    terms = ["jod", "memory", "graph", "sqlite", "deploy", "harness",
             "traversal", "linear", "vps", "index"]
    res["hybrid"] = timed(
        lambda t: len(db.execute(Q.HYBRID, (t, "default", 2)).fetchall()),
        terms * 6, db)
    res["fts_only"] = timed(
        lambda t: len(db.execute(
            "SELECT f.id FROM facts_fts JOIN facts f ON f.id=facts_fts.rowid "
            "WHERE facts_fts MATCH ? AND f.scope='default' "
            "ORDER BY bm25(facts_fts) LIMIT 25", (t,)).fetchall()),
        terms * 6, db)
    save(res, a.out)

    # ---- H12: community detection --------------------------------------
    db.execute("DROP TABLE IF EXISTS labels")
    db.execute("CREATE TEMP TABLE labels(node INTEGER PRIMARY KEY, "
               "label INTEGER)")
    db.execute("INSERT INTO labels SELECT id, id FROM nodes")
    lp, lp_timeouts = [], 0
    for _ in range(3):
        timer = threading.Timer(120.0, db.interrupt)
        timer.start()
        t0 = time.perf_counter()
        try:
            db.execute("BEGIN IMMEDIATE")
            db.execute(Q.LABEL_PROP_STEP)
            db.execute("COMMIT")
            lp.append((time.perf_counter() - t0) * 1000.0)
        except sqlite3.OperationalError as exc:
            if "interrupted" not in str(exc):
                raise
            # An interrupt can land after SQLite already unwound the
            # transaction, in which case ROLLBACK is itself an error.
            try:
                db.execute("ROLLBACK")
            except sqlite3.OperationalError:
                pass
            lp.append(120_000.0)
            lp_timeouts += 1
        finally:
            timer.cancel()
    res["label_propagation"] = {
        "iterations_timed": len(lp),
        "timeouts": lp_timeouts,
        "p50_ms": pct(lp, 50),
        "max_ms": pct(lp, 100),
        "distinct_labels_after_3": db.execute(
            "SELECT count(DISTINCT label) FROM labels").fetchone()[0],
    }

    db.close()
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print(json.dumps(res, indent=2)[:2000])


if __name__ == "__main__":
    main()
