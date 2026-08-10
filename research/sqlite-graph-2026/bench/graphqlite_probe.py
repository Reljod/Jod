#!/usr/bin/env python3
"""A bounded GraphQLite measurement: where does the time actually go?

The full 10k import did not finish in ten minutes of CPU, so this measures in
stages with a wall-clock budget instead of waiting: import rate per chunk (does
it degrade with graph size?), then Cypher k-hop latency, then the same
question as a recursive CTE over ordinary tables in the same file.

Usage:
  graphqlite_probe.py --ext PATH/graphqlite-linux-x86_64.so \\
      --out ../out/graphqlite-probe.json [--budget 60]
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

ENTRY = "sqlite3_graphqlite_init"


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(round((p / 100) * (len(xs) - 1))))], 2)


def open_gql(path, ext):
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA synchronous = NORMAL")
    db.execute("PRAGMA busy_timeout = 5000")
    db.enable_load_extension(True)
    db.load_extension(ext, entrypoint=ENTRY)
    db.enable_load_extension(False)
    return db


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ext", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--nodes", type=int, default=1250)
    ap.add_argument("--edges", type=int, default=10000)
    ap.add_argument("--chunk", type=int, default=500)
    ap.add_argument("--budget", type=float, default=60.0)
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="jod-gqlp-")
    path = os.path.join(work, "g.db")
    res = {"ext_bytes": os.path.getsize(a.ext), "nodes": a.nodes,
           "edges_requested": a.edges, "budget_seconds": a.budget}

    db = open_gql(path, a.ext)

    t0 = time.time()
    db.execute("BEGIN IMMEDIATE")
    for i in range(1, a.nodes + 1):
        db.execute("SELECT cypher(?)", ("CREATE (:N {id: %d})" % i,))
    db.execute("COMMIT")
    dt = time.time() - t0
    res["node_import"] = {"n": a.nodes, "seconds": round(dt, 2),
                          "per_second": round(a.nodes / dt)}
    print("nodes: %d in %.2fs (%.0f/s)" % (a.nodes, dt, a.nodes / dt),
          flush=True)

    rng = random.Random(7)
    chunks, done = [], 0
    started = time.time()
    while done < a.edges and time.time() - started < a.budget:
        t0 = time.time()
        db.execute("BEGIN IMMEDIATE")
        for _ in range(a.chunk):
            x, y = rng.randint(1, a.nodes), rng.randint(1, a.nodes)
            db.execute("SELECT cypher(?)", (
                "MATCH (a:N {id: %d}), (b:N {id: %d}) CREATE (a)-[:E]->(b)"
                % (x, y),))
        db.execute("COMMIT")
        dt = time.time() - t0
        done += a.chunk
        chunks.append({"edges_so_far": done, "seconds": round(dt, 2),
                       "per_second": round(a.chunk / dt, 1)})
        print("edges %6d  chunk %.2fs  %.1f/s" % (done, dt, a.chunk / dt),
              flush=True)
    res["edge_import_chunks"] = chunks
    res["edges_imported"] = done
    res["bytes"] = os.path.getsize(path)
    res["graphqlite_tables"] = [r[0] for r in db.execute(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
    ).fetchall()]

    # Cypher k-hop, capped so a pathological query is a result, not a wait.
    khop = {}
    for k in (1, 2, 3):
        lat, timeouts = [], 0
        for _ in range(15):
            s = rng.randint(1, a.nodes)
            q = ("MATCH (a:N {id: %d})-[:E*1..%d]->(b:N) RETURN DISTINCT b.id"
                 % (s, k))
            t = threading.Timer(10.0, db.interrupt)
            t.start()
            t0 = time.time()
            try:
                db.execute("SELECT cypher(?)", (q,)).fetchone()
                lat.append((time.time() - t0) * 1000)
            except sqlite3.Error as exc:
                timeouts += 1
                lat.append(10000.0)
                khop.setdefault("error_k%d" % k, str(exc)[:200])
            finally:
                t.cancel()
        khop["cypher.k%d" % k] = {"n": len(lat), "p50_ms": pct(lat, 50),
                                  "p95_ms": pct(lat, 95),
                                  "timeouts": timeouts}
        print("cypher k=%d p50 %s ms p95 %s ms (%d timeouts)"
              % (k, pct(lat, 50), pct(lat, 95), timeouts), flush=True)
    res["cypher_khop"] = khop

    # The same graph, the same file, as ordinary tables + a recursive CTE.
    db.execute("CREATE TABLE plain_edges(src INTEGER, dst INTEGER)")
    db.execute("BEGIN IMMEDIATE")
    db.execute("INSERT INTO plain_edges(src, dst) "
               "SELECT source_id, target_id FROM edges")
    db.execute("COMMIT")
    db.execute("CREATE INDEX ix_pe ON plain_edges(src, dst)")
    cte = """
    WITH RECURSIVE reach(node, depth) AS (
      SELECT ?1, 0
      UNION
      SELECT e.dst, r.depth + 1 FROM reach r
        JOIN plain_edges e ON e.src = r.node WHERE r.depth < ?2
    )
    SELECT node, MIN(depth) FROM reach GROUP BY node"""
    cte_out = {}
    for k in (1, 2, 3):
        lat = []
        for _ in range(15):
            s = rng.randint(1, a.nodes)
            t0 = time.time()
            db.execute(cte, (s, k)).fetchall()
            lat.append((time.time() - t0) * 1000)
        cte_out["cte.k%d" % k] = {"n": len(lat), "p50_ms": pct(lat, 50),
                                  "p95_ms": pct(lat, 95)}
        print("cte    k=%d p50 %s ms p95 %s ms"
              % (k, pct(lat, 50), pct(lat, 95)), flush=True)
    res["cte_khop"] = cte_out
    res["plain_edge_rows"] = db.execute(
        "SELECT count(*) FROM plain_edges").fetchone()[0]

    db.close()
    shutil.rmtree(work, ignore_errors=True)
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print("wrote", a.out, flush=True)


if __name__ == "__main__":
    main()
