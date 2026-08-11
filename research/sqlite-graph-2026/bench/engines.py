#!/usr/bin/env python3
"""H9/H10 — is a purpose-built graph engine worth a second file?

Loads exactly the same edge list into KuzuDB and DuckDB, runs the same 3-hop
neighbourhood from the same seeds, and also probes the property Jod cares
about more than speed: can two OS processes open the store at once?

Needs a venv with `kuzu` and `duckdb`; skips whichever is missing.

Usage:  engines.py DB.db --out ../out/engines-100k.json
"""

import argparse
import json
import os
import random
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import queries as Q  # noqa: E402


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return round(xs[k], 3)


def dirsize(path):
    if os.path.isfile(path):
        return os.path.getsize(path)
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            total += os.path.getsize(os.path.join(root, f))
    return total


def load_edges(src_db):
    db = sqlite3.connect(src_db)
    edges = db.execute("SELECT src, dst FROM edges").fetchall()
    nodes = [r[0] for r in db.execute("SELECT id FROM nodes").fetchall()]
    db.close()
    return nodes, edges


def bench_sqlite(src_db, seeds, k=3):
    db = sqlite3.connect(src_db)
    lat = []
    for s in seeds:
        t0 = time.perf_counter()
        db.execute(Q.KHOP_DIRECTED, (s, k, "default")).fetchall()
        lat.append((time.perf_counter() - t0) * 1000.0)
    db.close()
    return {"p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95),
            "bytes": dirsize(src_db)}


def bench_kuzu(nodes, edges, seeds, workdir, k=3):
    import kuzu
    path = os.path.join(workdir, "kuzu")
    t0 = time.time()
    db = kuzu.Database(path)
    conn = kuzu.Connection(db)
    conn.execute("CREATE NODE TABLE N(id INT64, PRIMARY KEY(id))")
    conn.execute("CREATE REL TABLE E(FROM N TO N)")
    ncsv = os.path.join(workdir, "n.csv")
    ecsv = os.path.join(workdir, "e.csv")
    with open(ncsv, "w") as fh:
        for n in nodes:
            fh.write("%d\n" % n)
    with open(ecsv, "w") as fh:
        for a, b in edges:
            fh.write("%d,%d\n" % (a, b))
    conn.execute('COPY N FROM "%s"' % ncsv)
    conn.execute('COPY E FROM "%s"' % ecsv)
    load_s = time.time() - t0

    q = ("MATCH (a:N)-[:E*1..%d]->(b:N) WHERE a.id = $s "
         "RETURN DISTINCT b.id" % k)
    lat = []
    for s in seeds:
        t0 = time.perf_counter()
        r = conn.execute(q, {"s": s})
        n = 0
        while r.has_next():
            r.get_next()
            n += 1
        lat.append((time.perf_counter() - t0) * 1000.0)

    out = {"version": kuzu.__version__, "load_seconds": round(load_s, 2),
           "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95),
           "bytes": dirsize(path)}

    # The property that decides it for Jod: can a second OS process read
    # while this one holds the database open?
    probe = (
        "import kuzu,sys\n"
        "try:\n"
        "    d=kuzu.Database(%r, read_only=True)\n"
        "    c=kuzu.Connection(d)\n"
        "    c.execute('MATCH (n:N) RETURN count(n)')\n"
        "    print('SECOND_PROCESS_OK')\n"
        "except Exception as e:\n"
        "    print('SECOND_PROCESS_FAILED:', type(e).__name__, e)\n"
    ) % path
    p = subprocess.run([sys.executable, "-c", probe],
                       capture_output=True, text=True, timeout=120)
    out["second_process_readonly"] = (p.stdout + p.stderr).strip()[:400]

    probe_w = probe.replace("read_only=True", "read_only=False")
    p = subprocess.run([sys.executable, "-c", probe_w],
                       capture_output=True, text=True, timeout=120)
    out["second_process_readwrite"] = (p.stdout + p.stderr).strip()[:400]

    conn.close()
    db.close()
    return out


def bench_duckdb(nodes, edges, seeds, workdir, k=3):
    import duckdb
    path = os.path.join(workdir, "duck.db")
    t0 = time.time()
    con = duckdb.connect(path)
    con.execute("CREATE TABLE nodes(id BIGINT PRIMARY KEY)")
    con.execute("CREATE TABLE edges(src BIGINT, dst BIGINT)")
    con.executemany("INSERT INTO nodes VALUES (?)", [(n,) for n in nodes])
    con.executemany("INSERT INTO edges VALUES (?,?)", edges)
    con.execute("CREATE INDEX ix_src ON edges(src)")
    con.execute("CREATE INDEX ix_dst ON edges(dst)")
    load_s = time.time() - t0

    pgq = None
    try:
        con.execute("INSTALL duckpgq FROM community")
        con.execute("LOAD duckpgq")
        pgq = "loaded"
    except Exception as exc:
        pgq = "unavailable: %s" % str(exc)[:200]

    q = """
    WITH RECURSIVE reach(node, depth) AS (
      SELECT ?::BIGINT, 0
      UNION
      SELECT e.dst, r.depth + 1 FROM reach r JOIN edges e ON e.src = r.node
       WHERE r.depth < %d
    )
    SELECT node, MIN(depth) FROM reach GROUP BY node
    """ % k
    lat = []
    for s in seeds:
        t0 = time.perf_counter()
        con.execute(q, [s]).fetchall()
        lat.append((time.perf_counter() - t0) * 1000.0)
    con.close()

    # Second process, same file.
    probe = (
        "import duckdb\n"
        "try:\n"
        "    c=duckdb.connect(%r, read_only=True)\n"
        "    c.execute('SELECT count(*) FROM edges').fetchall()\n"
        "    print('SECOND_PROCESS_OK')\n"
        "except Exception as e:\n"
        "    print('SECOND_PROCESS_FAILED:', type(e).__name__, e)\n"
    ) % path
    p = subprocess.run([sys.executable, "-c", probe],
                       capture_output=True, text=True, timeout=120)

    return {"version": duckdb.__version__, "load_seconds": round(load_s, 2),
            "duckpgq": pgq, "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95),
            "bytes": dirsize(path),
            "second_process_readonly": (p.stdout + p.stderr).strip()[:400]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--out", required=True)
    ap.add_argument("--seeds", type=int, default=60)
    a = ap.parse_args()

    nodes, edges = load_edges(a.db)
    rng = random.Random(11)
    seeds = [rng.choice(nodes) for _ in range(a.seeds)]

    res = {"source_db": a.db, "nodes": len(nodes), "edges": len(edges),
           "python": sys.version.split()[0]}
    res["sqlite"] = bench_sqlite(a.db, seeds)

    work = tempfile.mkdtemp(prefix="jod-engines-")
    try:
        for name, fn in (("kuzu", bench_kuzu), ("duckdb", bench_duckdb)):
            try:
                res[name] = fn(nodes, edges, seeds, work)
            except ImportError as exc:
                res[name] = {"skipped": str(exc)}
            except Exception as exc:
                res[name] = {"failed": "%s: %s" % (type(exc).__name__, exc)}
    finally:
        shutil.rmtree(work, ignore_errors=True)

    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
