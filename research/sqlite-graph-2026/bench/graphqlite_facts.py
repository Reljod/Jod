#!/usr/bin/env python3
"""What GraphQLite actually does, measured rather than read off a README.

Four questions, each answered by loading the prebuilt `.so` the crate ships:

  1. What tables does it create in *your* database?
  2. What shape is a Cypher result — a result set, or a string?
  3. Where does the query time go: DISTINCT, the property filter, or the
     variable-length path operator?
  4. What does the same answer cost as a recursive CTE?

Usage:  graphqlite_facts.py --ext PATH/graphqlite-linux-x86_64.so
"""

import argparse
import os
import random
import sqlite3
import tempfile
import threading
import time

# The filename does not encode the entry point — SQLite would look for
# `sqlite3_graphqlitelinuxx_init` and fail with "undefined symbol".
ENTRY = "sqlite3_graphqlite_init"


def load(db, ext):
    db.enable_load_extension(True)
    db.load_extension(ext, entrypoint=ENTRY)
    db.enable_load_extension(False)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ext", required=True)
    ap.add_argument("--nodes", type=int, default=1250)
    ap.add_argument("--edges", type=int, default=10000)
    a = ap.parse_args()

    path = os.path.join(tempfile.mkdtemp(prefix="jod-gqlf-"), "g.db")
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    load(db, a.ext)

    db.execute("SELECT cypher(\"CREATE (:P {name: 'alice'})\")")
    print("\n-- 1. tables it created in your database --")
    for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' "
                        "ORDER BY name"):
        print("   ", r[0])
    print("   (`nodes` and `edges` are taken unconditionally)")

    print("\n-- 2. what a MATCH returns --")
    r = db.execute("SELECT cypher(\"MATCH (p:P) RETURN p.name\")").fetchone()[0]
    print("   type:", type(r).__name__, " value:", repr(r)[:120])
    print("   (a scalar JSON string — no table-valued interface, so this "
          "cannot be joined against facts_fts by the planner)")

    print("\n-- 3. building the graph --")
    t0 = time.time()
    db.execute("BEGIN IMMEDIATE")
    for i in range(1, a.nodes + 1):
        db.execute("SELECT cypher(?)", ("CREATE (:N {id: %d})" % i,))
    db.execute("COMMIT")
    print("   %d nodes in %.2fs (%.0f/s)"
          % (a.nodes, time.time() - t0, a.nodes / (time.time() - t0)))

    rng = random.Random(7)
    t0 = time.time()
    db.execute("BEGIN IMMEDIATE")
    for _ in range(a.edges):
        x, y = rng.randint(1, a.nodes), rng.randint(1, a.nodes)
        db.execute("SELECT cypher(?)", (
            "MATCH (a:N {id: %d}), (b:N {id: %d}) CREATE (a)-[:E]->(b)"
            % (x, y),))
    db.execute("COMMIT")
    dt = time.time() - t0
    print("   %d edges in %.2fs (%.0f/s) — import is not the problem"
          % (a.edges, dt, a.edges / dt))

    print("\n-- 4. where the query time goes --")
    for label, q in (
        ("fixed 1 hop, no DISTINCT",
         "MATCH (a:N {id: 5})-[:E]->(b:N) RETURN b.id"),
        ("fixed 1 hop, DISTINCT",
         "MATCH (a:N {id: 5})-[:E]->(b:N) RETURN DISTINCT b.id"),
        ("fixed 2 hop, explicit chain",
         "MATCH (a:N {id: 5})-[:E]->()-[:E]->(b:N) RETURN b.id"),
        ("variable length *1..1 (same answer as row 1)",
         "MATCH (a:N {id: 5})-[:E*1..1]->(b:N) RETURN b.id"),
    ):
        t = threading.Timer(20.0, db.interrupt)
        t.start()
        t0 = time.time()
        try:
            res = db.execute("SELECT cypher(?)", (q,)).fetchone()[0]
            print("   %-44s %9.1f ms  rows=%d"
                  % (label, (time.time() - t0) * 1000, res.count("{")))
        except sqlite3.Error as exc:
            print("   %-44s ABORTED after %.1f s (%s)"
                  % (label, time.time() - t0, str(exc)[:50]))
        finally:
            t.cancel()

    print("\n-- 5. the same 1-hop question as a recursive CTE --")
    db.execute("CREATE TABLE plain_edges(src INTEGER, dst INTEGER)")
    db.execute("BEGIN IMMEDIATE")
    db.execute("INSERT INTO plain_edges SELECT source_id, target_id FROM edges")
    db.execute("COMMIT")
    db.execute("CREATE INDEX ix_pe ON plain_edges(src, dst)")
    cte = ("WITH RECURSIVE reach(node, depth) AS ("
           " SELECT ?1, 0 UNION"
           " SELECT e.dst, r.depth+1 FROM reach r"
           " JOIN plain_edges e ON e.src = r.node WHERE r.depth < ?2)"
           " SELECT node, MIN(depth) FROM reach GROUP BY node")
    for k in (1, 2, 3):
        t0 = time.time()
        n = len(db.execute(cte, (5, k)).fetchall())
        print("   recursive CTE, %d hop%s%s %9.3f ms  rows=%d"
              % (k, "s" if k > 1 else " ", " " * 30, (time.time() - t0) * 1000,
                 n))
    db.close()


if __name__ == "__main__":
    main()
