#!/usr/bin/env python3
"""H14 — does a graph + embedding hybrid need an ANN index or a second engine?

`sqlite-vec` brute force over N vectors in the same file, measured at the
scale the graph benchmark uses. The prior benchmark measured 19 ms over 30k
vectors; this asks what 100k and 300k cost, since that is where a memory
graph with an embedding per fact ends up.

Usage:  vectors.py --out ../out/vectors.json [--n 100000] [--dim 384]
"""

import argparse
import json
import os
import random
import sqlite3
import struct
import tempfile
import time


def pct(xs, p):
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return round(xs[k], 3)


def run(n, dim, queries=30):
    import sqlite_vec
    path = os.path.join(tempfile.mkdtemp(prefix="jod-vec-"), "vec.db")
    db = sqlite3.connect(path, isolation_level=None)
    db.enable_load_extension(True)
    sqlite_vec.load(db)
    db.enable_load_extension(False)
    ver = db.execute("SELECT vec_version()").fetchone()[0]

    db.execute("CREATE VIRTUAL TABLE v USING vec0("
               "fact_id INTEGER PRIMARY KEY, emb FLOAT[%d])" % dim)
    rng = random.Random(3)
    t0 = time.time()
    db.execute("BEGIN IMMEDIATE")
    for i in range(1, n + 1):
        blob = struct.pack("%df" % dim,
                           *[rng.uniform(-1, 1) for _ in range(dim)])
        db.execute("INSERT INTO v(fact_id, emb) VALUES (?,?)", (i, blob))
    db.execute("COMMIT")
    build_s = time.time() - t0

    lat = []
    for _ in range(queries):
        q = struct.pack("%df" % dim,
                        *[rng.uniform(-1, 1) for _ in range(dim)])
        t0 = time.perf_counter()
        db.execute("SELECT fact_id, distance FROM v "
                   "WHERE emb MATCH ? AND k = 10 ORDER BY distance",
                   (q,)).fetchall()
        lat.append((time.perf_counter() - t0) * 1000.0)

    size = os.path.getsize(path)
    db.close()
    return {"vec_version": ver, "n": n, "dim": dim,
            "build_seconds": round(build_s, 2),
            "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95),
            "bytes": size}


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--dim", type=int, default=384)
    a = ap.parse_args()
    res = {"scales": []}
    for n in (30_000, 100_000, 300_000):
        res["scales"].append(run(n, a.dim))
        print(res["scales"][-1])
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
