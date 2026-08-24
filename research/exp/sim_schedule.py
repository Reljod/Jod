#!/usr/bin/env python3
"""Barrier vs pipeline scheduling, using measured call durations.

H41 says pipelining beats barrier-synchronised stages whenever per-item
duration varies, and that the gap widens with variance. That is a scheduling
claim, not a model-quality claim, so simulating it is legitimate - but the
duration distribution must be real. We take it from the observed wall times of
single-call (solo) runs in this study rather than inventing a distribution.

Barrier:   every item must finish stage k before any item starts stage k+1.
Pipeline:  each item flows through all stages on its own; only the worker pool
           is shared.
"""

import argparse
import json
import random
import statistics as st


def load_durations(path):
    ds = []
    for line in open(path):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if r.get("failed"):
            continue
        # A solo run is exactly one model call, so its wall time is a clean
        # sample of single-call duration.
        if r.get("mode") == "solo" and r.get("n_calls") == 1:
            ds.append(float(r["wall_s"]))
    return ds


def simulate(durations, n_items, n_stages, pool, mode, rng):
    """Return makespan in seconds."""
    draw = lambda: rng.choice(durations)  # noqa: E731

    if mode == "barrier":
        total = 0.0
        for _ in range(n_stages):
            # One wave: all items run this stage, limited by the pool. The
            # stage ends only when its slowest item ends.
            times = sorted((draw() for _ in range(n_items)), reverse=True)
            free = [0.0] * pool
            for t in times:
                i = min(range(pool), key=lambda j: free[j])
                free[i] += t
            total += max(free)
        return total

    # Pipeline, scheduled properly: a task (item, stage) becomes ready when the
    # same item's previous stage finishes, and any free slot may pick up any
    # ready task. Assigning whole items to slots up front - the obvious but
    # wrong approach - books future slot time that nothing needs yet and makes
    # pipelining look worse than a barrier.
    ready = [(0.0, 0) for _ in range(n_items)]   # (ready_time, next_stage)
    slot_free = [0.0] * pool
    done = 0
    makespan = 0.0
    while done < n_items:
        # Pick the task that can start earliest on the slot that frees earliest.
        s = min(range(pool), key=lambda j: slot_free[j])
        cand = [i for i in range(n_items) if ready[i][1] < n_stages]
        if not cand:
            break
        i = min(cand, key=lambda k: ready[k][0])
        start = max(slot_free[s], ready[i][0])
        dur = draw()
        end = start + dur
        slot_free[s] = end
        ready[i] = (end, ready[i][1] + 1)
        if ready[i][1] == n_stages:
            done += 1
            makespan = max(makespan, end)
    return makespan


def spread(durations, factor, rng):
    """Rescale a sample around its mean to change variance without changing
    the mean, so the effect of variance alone can be isolated."""
    m = st.mean(durations)
    return [max(0.5, m + (d - m) * factor) for d in durations]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--items", type=int, default=12)
    ap.add_argument("--stages", type=int, default=3)
    ap.add_argument("--pool", type=int, default=4)
    ap.add_argument("--reps", type=int, default=2000)
    a = ap.parse_args()

    base = load_durations(a.results)
    if len(base) < 3:
        print("not enough solo runs yet to seed the simulation (have %d)"
              % len(base))
        return
    print("seeded from %d measured single-call durations: "
          "mean=%.1fs sd=%.1fs min=%.1f max=%.1f"
          % (len(base), st.mean(base),
             st.stdev(base) if len(base) > 1 else 0.0, min(base), max(base)))
    print("items=%d stages=%d pool=%d reps=%d"
          % (a.items, a.stages, a.pool, a.reps))
    print()
    hdr = "%-10s %10s %10s %10s %9s" % ("variance", "barrier_s", "pipeline_s",
                                        "saved_s", "speedup")
    print(hdr)
    print("-" * len(hdr))
    rng = random.Random(11)
    for factor, name in [(0.0, "none"), (0.5, "half"), (1.0, "measured"),
                         (2.0, "double"), (3.0, "triple")]:
        ds = spread(base, factor, rng)
        b = st.mean([simulate(ds, a.items, a.stages, a.pool, "barrier", rng)
                     for _ in range(a.reps)])
        p = st.mean([simulate(ds, a.items, a.stages, a.pool, "pipeline", rng)
                     for _ in range(a.reps)])
        print("%-10s %10.1f %10.1f %10.1f %8.2fx"
              % (name, b, p, b - p, b / p if p else 0))
    print()
    print("cv of measured sample = %.2f"
          % (st.stdev(base) / st.mean(base) if len(base) > 1 else 0))

    # The variance sweep alone is misleading, because when the worker pool is
    # the bottleneck the total work dominates and no scheduling choice can
    # help. Sweep the pool to show where pipelining actually pays.
    print()
    hdr2 = "%-10s %10s %10s %10s %9s" % ("pool", "barrier_s", "pipeline_s",
                                         "saved_s", "speedup")
    print(hdr2)
    print("-" * len(hdr2))
    ds = spread(base, 1.0, rng)
    for pool in [2, 4, 6, 8, 12, 24]:
        b = st.mean([simulate(ds, a.items, a.stages, pool, "barrier", rng)
                     for _ in range(a.reps)])
        p = st.mean([simulate(ds, a.items, a.stages, pool, "pipeline", rng)
                     for _ in range(a.reps)])
        tag = "%d%s" % (pool, "  (>= items)" if pool >= a.items else "")
        print("%-10s %10.1f %10.1f %10.1f %8.2fx"
              % (tag, b, p, b - p, b / p if p else 0))


if __name__ == "__main__":
    main()
