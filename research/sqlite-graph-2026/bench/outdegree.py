#!/usr/bin/env python3
"""H2, honestly: traverse *out* of the entities with the largest out-degree.

The generator attaches each new node to existing ones, so its hubs have huge
in-degree and small out-degree. A directed traversal starting at one of them
therefore looks unnaturally cheap — and in a real memory graph the hub is
`reljod`, the *subject* of thousands of facts, i.e. large out-degree.

So this measures the case the main sweep flatters: k-hop following out-edges
from the top out-degree entities.

Usage:  outdegree.py DB.db --out ../out/outdegree-100k.json
"""

import argparse
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import queries as Q  # noqa: E402
from bench import connect, pct  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--out", required=True)
    ap.add_argument("--top", type=int, default=30)
    a = ap.parse_args()

    db = connect(a.db)
    res = {"db": a.db,
           "edges": db.execute("SELECT count(*) FROM edges").fetchone()[0]}

    rows = db.execute(
        "SELECT src, count(*) c FROM edges GROUP BY src "
        "ORDER BY c DESC LIMIT ?", (a.top,)).fetchall()
    seeds = [r[0] for r in rows]
    res["top_out_degrees"] = [r[1] for r in rows[:5]]
    res["max_in_degree"] = db.execute(
        "SELECT count(*) c FROM edges GROUP BY dst ORDER BY c DESC "
        "LIMIT 1").fetchone()[0]

    out = {}
    for k in (1, 2, 3, 4):
        lat, sizes, timeouts = [], [], 0
        for s in seeds:
            t = threading.Timer(10.0, db.interrupt)
            t.start()
            t0 = time.perf_counter()
            try:
                n = len(db.execute(Q.KHOP_DIRECTED,
                                   (s, k, "default")).fetchall())
                lat.append((time.perf_counter() - t0) * 1000.0)
                sizes.append(n)
            except Exception:
                timeouts += 1
                lat.append(10000.0)
            finally:
                t.cancel()
        out["directed.k%d.out_hub" % k] = {
            "n": len(lat), "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95),
            "mean_rows": round(sum(sizes) / max(len(sizes), 1), 1),
            "timeouts": timeouts,
        }
    res["khop"] = out
    db.close()
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=2)
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
