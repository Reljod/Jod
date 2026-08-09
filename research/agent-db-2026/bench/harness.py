"""Multi-process benchmark driver.

Spawns real OS processes, not threads, because the thing under test is whether
several independent agent processes can share one store. Threads inside one
interpreter would share a connection pool and quietly hide the contention that
matters.

Workloads
  append   N writers append agent events for a fixed duration
  rmw      N writers each perform a fixed number of read-modify-write +1
           operations over a small hot key set; correctness = no lost updates
  mixed    N writers appending while M readers query recent events
  vector   single process: bulk load, index build, top-k search, recall@10

Usage
  python harness.py --db sqlite --workload append --writers 8 --duration 10
  python harness.py --db postgres --workload vector --vectors 30000
"""

import argparse
import json
import multiprocessing as mp
import os
import statistics
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from adapters import EVENT_PAYLOAD, REGISTRY  # noqa: E402

MAX_SAMPLES = 40_000


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    i = min(len(xs) - 1, int(round((p / 100) * (len(xs) - 1))))
    return round(xs[i] * 1000, 3)  # ms


def _worker(db, cfg, workload, role, worker_id, duration, ops, n_keys):
    """Runs in a fresh process. Returns a stats dict; never raises."""
    try:
        adapter = REGISTRY[db](cfg)
        adapter.connect()
    except Exception as e:  # connection itself is a legitimate failure mode
        return {
            "worker": worker_id,
            "role": role,
            "fatal": f"{type(e).__name__}: {e}",
            "ok": 0,
            "errors": {"connect": 1},
            "lat": [],
        }

    lat, errors, ok = [], {}, 0
    run_id = f"run-{worker_id}"
    deadline = time.perf_counter() + duration
    seq = 0
    # measured from the first op, not from process start: spawning 16 Python
    # interpreters takes ~0.5s and would otherwise be charged to the database
    t_first = None

    def record(fn, *a):
        nonlocal ok, t_first
        t0 = time.perf_counter()
        if t_first is None:
            t_first = t0
        try:
            fn(*a)
            lat.append(time.perf_counter() - t0)
            ok += 1
        except Exception as e:
            key = type(e).__name__
            msg = str(e)[:60]
            errors[f"{key}: {msg}"] = errors.get(f"{key}: {msg}", 0) + 1

    try:
        if workload == "rmw":
            for i in range(ops):
                record(adapter.rmw, f"task-{i % n_keys}")
        elif workload == "append":
            while time.perf_counter() < deadline:
                seq += 1
                record(adapter.append, run_id, seq, EVENT_PAYLOAD)
        elif workload == "mixed":
            if role == "writer":
                while time.perf_counter() < deadline:
                    seq += 1
                    record(adapter.append, run_id, seq, EVENT_PAYLOAD)
            else:
                target = f"run-{worker_id % max(1, cfg['n_writers'])}"
                while time.perf_counter() < deadline:
                    record(adapter.read, target, 20)
    finally:
        adapter.close()

    if len(lat) > MAX_SAMPLES:  # keep the payload small; sample evenly
        step = len(lat) / MAX_SAMPLES
        lat = [lat[int(i * step)] for i in range(MAX_SAMPLES)]

    elapsed = (time.perf_counter() - t_first) if t_first is not None else 0.0
    return {
        "worker": worker_id,
        "role": role,
        "ok": ok,
        "errors": errors,
        "lat": lat,
        "elapsed": elapsed,
    }


def run_concurrent(db, cfg, workload, writers, readers, duration, ops):
    adapter_cls = REGISTRY[db]
    adapter = adapter_cls(cfg)

    setup_err = None
    try:
        adapter.setup()
    except Exception as e:
        setup_err = f"{type(e).__name__}: {e}"
        return {"db": db, "workload": workload, "setup_error": setup_err}

    procs = []
    if workload == "mixed":
        procs = [("writer", i) for i in range(writers)] + [("reader", i) for i in range(readers)]
    else:
        procs = [("writer", i) for i in range(writers)]

    ctx = mp.get_context("spawn")  # fork + multithreaded native libs = grief
    t0 = time.perf_counter()
    with ctx.Pool(len(procs)) as pool:
        results = pool.starmap(
            _worker,
            [
                (db, cfg, workload, role, i, duration, ops, cfg["n_keys"])
                for role, i in procs
            ],
        )
    wall = time.perf_counter() - t0

    writer_res = [r for r in results if r["role"] == "writer"]
    reader_res = [r for r in results if r["role"] == "reader"]

    def agg(rs):
        lat = [x for r in rs for x in r["lat"]]
        ok = sum(r["ok"] for r in rs)
        errs = {}
        for r in rs:
            for k, v in r["errors"].items():
                errs[k] = errs.get(k, 0) + v
        n_err = sum(errs.values())
        # the slowest worker's own busy window, excluding interpreter startup
        busy = max([r.get("elapsed", 0.0) for r in rs] + [1e-9])
        return {
            "ops_ok": ok,
            "ops_err": n_err,
            "error_rate_pct": round(100 * n_err / max(1, ok + n_err), 3),
            "busy_s": round(busy, 3),
            "throughput_ops_s": round(ok / busy, 1),
            "p50_ms": pct(lat, 50),
            "p95_ms": pct(lat, 95),
            "p99_ms": pct(lat, 99),
            "max_ms": pct(lat, 100),
            "errors": dict(sorted(errs.items(), key=lambda kv: -kv[1])[:5]),
        }

    out = {
        "db": db,
        "workload": workload,
        "writers": writers,
        "readers": readers,
        "wall_s": round(wall, 2),
        "write": agg(writer_res),
    }
    if reader_res:
        out["read"] = agg(reader_res)

    # correctness: did the store actually keep everything it acknowledged?
    try:
        verifier = adapter_cls(cfg)
        verifier.connect()
        if workload == "rmw":
            expected = out["write"]["ops_ok"]
            actual = verifier.counter_sum()
            out["correctness"] = {
                "expected": expected,
                "actual": actual,
                "lost_updates": expected - actual,
                "verdict": "CORRECT" if actual == expected else "LOST UPDATES",
            }
        elif workload in ("append", "mixed"):
            expected = out["write"]["ops_ok"]
            actual = verifier.total_events()
            out["correctness"] = {
                "expected": expected,
                "actual": actual,
                "lost_writes": expected - actual,
                "verdict": "CORRECT" if actual == expected else "LOST WRITES",
            }
        verifier.close()
    except Exception as e:
        out["correctness"] = {"verdict": f"unverifiable: {type(e).__name__}: {e}"}

    return out


def run_vector(db, cfg, n_vectors, n_queries=200, k=10):
    import numpy as np

    adapter = REGISTRY[db](cfg)
    if not adapter.supports_vector:
        return {"db": db, "workload": "vector", "skipped": "no vector support"}

    # Clustered, not uniform. In 384 dimensions i.i.d. Gaussian points are all
    # very nearly equidistant, so the "true" top-10 is a set of arbitrary ties
    # and every ANN index scores near-zero recall against it — an artifact of
    # the data, not a property of the engine. Real sentence embeddings sit in
    # clusters, so the neighbourhood structure an ANN index exploits is
    # actually there. This generates that structure explicitly.
    rng = np.random.default_rng(42)
    n_clusters = 300
    centers = rng.standard_normal((n_clusters, 384), dtype=np.float32)
    centers /= np.linalg.norm(centers, axis=1, keepdims=True)
    assign = rng.integers(0, n_clusters, n_vectors)
    vecs = centers[assign] + 0.35 * rng.standard_normal((n_vectors, 384), dtype=np.float32)
    vecs /= np.linalg.norm(vecs, axis=1, keepdims=True)

    qidx = rng.choice(n_vectors, n_queries, replace=False)
    queries = vecs[qidx] + 0.10 * rng.standard_normal((n_queries, 384), dtype=np.float32)
    queries /= np.linalg.norm(queries, axis=1, keepdims=True)

    # ground truth by exact cosine
    sims = queries @ vecs.T
    order = np.argsort(-sims, axis=1)
    truth = [set(order[i, :k].tolist()) for i in range(n_queries)]
    truth1 = [int(order[i, 0]) for i in range(n_queries)]

    out = {"db": db, "workload": "vector", "n_vectors": n_vectors, "dim": 384, "k": k}
    try:
        adapter.vec_setup()
        t0 = time.perf_counter()
        batch = 1000
        for s in range(0, n_vectors, batch):
            adapter.vec_add(np.arange(s, min(s + batch, n_vectors)), vecs[s : s + batch])
        out["load_s"] = round(time.perf_counter() - t0, 2)
        out["load_vectors_s"] = round(n_vectors / (time.perf_counter() - t0), 1)

        t0 = time.perf_counter()
        out["index"] = adapter.vec_index()
        out["index_build_s"] = round(time.perf_counter() - t0, 2)

        lat, hits, top1 = [], 0, 0
        for i, q in enumerate(queries):
            t0 = time.perf_counter()
            got = [int(g) for g in adapter.vec_search(q, k)]
            lat.append(time.perf_counter() - t0)
            hits += len(truth[i] & set(got))
            if got and got[0] == truth1[i]:
                top1 += 1
        out["p50_ms"] = pct(lat, 50)
        out["p95_ms"] = pct(lat, 95)
        out["p99_ms"] = pct(lat, 99)
        out["qps_single_thread"] = round(1 / statistics.mean(lat), 1)
        out["recall_at_10_pct"] = round(100 * hits / (n_queries * k), 2)
        out["recall_at_1_pct"] = round(100 * top1 / n_queries, 2)
        out["data"] = "clustered (300 centers, sigma=0.35)"
    except Exception as e:
        out["error"] = f"{type(e).__name__}: {e}"
        out["traceback"] = traceback.format_exc()[-600:]
    finally:
        adapter.close()
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--workload", required=True, choices=["append", "rmw", "mixed", "vector"])
    ap.add_argument("--writers", type=int, default=8)
    ap.add_argument("--readers", type=int, default=4)
    ap.add_argument("--duration", type=float, default=10)
    ap.add_argument("--ops", type=int, default=200, help="rmw ops per worker")
    ap.add_argument("--n-keys", type=int, default=4, help="hot keys contended in rmw")
    ap.add_argument("--vectors", type=int, default=30_000)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    cfg = {
        "data_dir": os.environ.get("BENCH_DATA", "/data"),
        "pg_dsn": os.environ.get("PG_DSN", "postgresql://jod:jod@pg:5432/jod"),
        "redis_host": os.environ.get("REDIS_HOST", "redis"),
        "qdrant_host": os.environ.get("QDRANT_HOST", "qdrant"),
        "n_keys": args.n_keys,
        "n_writers": args.writers,
    }
    os.makedirs(cfg["data_dir"], exist_ok=True)

    if args.workload == "vector":
        res = run_vector(args.db, cfg, args.vectors)
    else:
        res = run_concurrent(
            args.db, cfg, args.workload, args.writers, args.readers, args.duration, args.ops
        )

    line = json.dumps(res)
    print(line)
    if args.out:
        with open(args.out, "a") as f:
            f.write(line + "\n")


if __name__ == "__main__":
    main()
