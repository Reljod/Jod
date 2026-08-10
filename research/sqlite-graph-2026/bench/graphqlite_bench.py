#!/usr/bin/env python3
"""H9/H11 — measure the actual SQLite graph extension against the plain CTE.

GraphQLite is the only maintained, permissively-licensed SQLite graph
extension in 2026, so it is the concrete thing the owner's question names.
This loads its **prebuilt** `.so` — the crate ships binaries, it does not
compile from source on your machine — and asks three questions:

  A. Does it coexist with a database that already has `nodes` and `edges`
     tables of its own? (It creates tables with exactly those names.)
  B. What does importing the same 100k-edge graph cost?
  C. Is a 3-hop neighbourhood in Cypher faster than the recursive CTE, on
     the same host, same graph, same seeds?

Usage:
  graphqlite_bench.py DB.db --ext PATH/graphqlite-linux-x86_64.so \\
      --out ../out/graphqlite-100k.json
"""

import argparse
import json
import os
import random
import shutil
import sqlite3
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import queries as Q  # noqa: E402

ENTRY = "sqlite3_graphqlite_init"


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return round(xs[k], 3)


def load_ext(db, ext):
    db.enable_load_extension(True)
    db.load_extension(ext, entrypoint=ENTRY)
    db.enable_load_extension(False)


def collision_test(src, ext, work):
    """A: what happens in a database that already owns `nodes` and `edges`?"""
    path = os.path.join(work, "collide.db")
    shutil.copy(src, path)
    db = sqlite3.connect(path, isolation_level=None)
    load_ext(db, ext)
    out = {}
    out["jod_edges_schema_before"] = db.execute(
        "SELECT sql FROM sqlite_master WHERE name='edges'").fetchone()[0][:120]
    try:
        db.execute("SELECT cypher(\"CREATE (:N {id: 1})\")")
        out["result"] = "created a node without complaining"
    except sqlite3.Error as exc:
        out["result"] = "%s: %s" % (type(exc).__name__, exc)
    out["jod_edges_schema_after"] = db.execute(
        "SELECT sql FROM sqlite_master WHERE name='edges'").fetchone()[0][:120]
    out["jod_edge_rows_after"] = db.execute(
        "SELECT count(*) FROM edges").fetchone()[0]
    db.close()
    return out


def import_and_query(src, ext, work, seeds_n, hops):
    """B and C: a clean database, the same graph, the same question."""
    sdb = sqlite3.connect(src)
    edges = sdb.execute("SELECT src, dst FROM edges").fetchall()
    nodes = [r[0] for r in sdb.execute("SELECT id FROM nodes").fetchall()]
    sdb.close()

    path = os.path.join(work, "gql.db")
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA synchronous = NORMAL")
    load_ext(db, ext)

    out = {"nodes": len(nodes), "edges": len(edges)}
    t0 = time.time()
    err = None
    try:
        db.execute("BEGIN IMMEDIATE")
        for n in nodes:
            db.execute("SELECT cypher(?)", ("CREATE (:N {id: %d})" % n,))
        db.execute("COMMIT")
        out["node_import_seconds"] = round(time.time() - t0, 2)

        t1 = time.time()
        db.execute("BEGIN IMMEDIATE")
        for s, d in edges:
            db.execute("SELECT cypher(?)", (
                "MATCH (a:N {id: %d}), (b:N {id: %d}) CREATE (a)-[:E]->(b)"
                % (s, d),))
        db.execute("COMMIT")
        out["edge_import_seconds"] = round(time.time() - t1, 2)
    except (sqlite3.Error, KeyboardInterrupt) as exc:
        err = "%s: %s" % (type(exc).__name__, exc)
        out["import_error"] = err
    out["import_seconds_total"] = round(time.time() - t0, 2)
    out["bytes"] = os.path.getsize(path)
    out["graphqlite_tables"] = [
        r[0] for r in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "ORDER BY name").fetchall()]

    if err is None:
        rng = random.Random(11)
        seeds = [rng.choice(nodes) for _ in range(seeds_n)]
        lat = []
        for s in seeds:
            q = ("MATCH (a:N {id: %d})-[:E*1..%d]->(b:N) RETURN DISTINCT b.id"
                 % (s, hops))
            t0 = time.perf_counter()
            try:
                db.execute("SELECT cypher(?)", (q,)).fetchall()
                lat.append((time.perf_counter() - t0) * 1000.0)
            except sqlite3.Error as exc:
                out["query_error"] = str(exc)
                break
        out["cypher_khop"] = {"hops": hops, "n": len(lat),
                              "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95)}
    db.close()
    return out


def cte_baseline(src, seeds_n, hops):
    db = sqlite3.connect(src)
    nodes = [r[0] for r in db.execute("SELECT id FROM nodes").fetchall()]
    rng = random.Random(11)
    seeds = [rng.choice(nodes) for _ in range(seeds_n)]
    lat = []
    for s in seeds:
        t0 = time.perf_counter()
        db.execute(Q.KHOP_DIRECTED, (s, hops, "default")).fetchall()
        lat.append((time.perf_counter() - t0) * 1000.0)
    db.close()
    return {"hops": hops, "n": len(lat), "p50_ms": pct(lat, 50),
            "p95_ms": pct(lat, 95)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--ext", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--seeds", type=int, default=40)
    ap.add_argument("--hops", type=int, default=3)
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="jod-gql-")
    res = {
        "source_db": a.db,
        "ext_bytes": os.path.getsize(a.ext),
        "note": ("The .so is a prebuilt binary shipped inside the crate; "
                 "nothing here compiled it from source."),
    }
    try:
        res["A_name_collision"] = collision_test(a.db, a.ext, work)
        res["B_C_import_and_query"] = import_and_query(
            a.db, a.ext, work, a.seeds, a.hops)
        res["C_cte_baseline"] = cte_baseline(a.db, a.seeds, a.hops)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
