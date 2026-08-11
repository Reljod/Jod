# Hypotheses — a graph layer for Jod's long-term memory

Written **before** any measurement. Each is falsifiable by a number the
benchmark in [`bench/`](bench/) produces. Verdicts land in
[`REPORT.md`](REPORT.md); raw numbers in [`out/`](out/).

The question under test: the owner asked for *"the extension of SQLite for
Graph"*. The null hypothesis is that **no extension is needed** — plain tables
plus recursive CTEs, in the file Jod already has, are fast enough that any
extension is a cost with no matching benefit.

## The scale that matters

A personal memory graph is not a social network. The honest ceiling for Jod
after years of use is **~10^5 facts / edges**; 10^6 is the paranoid upper bound.
Every hypothesis below is graded at 100k first and 1M second, because a win at
1M that costs a dependency is irrelevant if 100k is the real world.

The generated graph is **scale-free** (preferential attachment), not uniform
random, because a memory graph has hubs — `reljod`, `jod`, `linear` — and a
traversal from a hub is the case that decides whether SQL is viable.

| id | Hypothesis | Falsified if |
|----|------------|--------------|
| **H1** | A 3-hop neighbourhood from a random entity over **100k edges** completes p95 **< 50 ms** with plain indexed tables + a recursive CTE. | p95 ≥ 50 ms |
| **H2** | The same query from a **hub** (top-1% degree) is the hard case and is **> 10x** slower than from a random node — i.e. average-case numbers lie. | hub/random ratio < 10x |
| **H3** | 4-hop from a hub at 100k edges touches so much of the graph that it is **not a useful query** (> 20% of all nodes returned), so the depth limit is a product decision, not a performance one. | < 20% of nodes returned |
| **H4** | Recursive-CTE traversal **degrades linearly or better** with edge count from 1k → 1M (10x edges ≤ 10x time at fixed depth). | superlinear (> 10x time per 10x edges) |
| **H5** | **Shortest path** between two random entities via a single recursive CTE with a path-string cycle guard is **> 10x slower** than an application-side bidirectional BFS issuing one indexed query per level. | CTE within 10x of bidirectional BFS |
| **H6** | Adding **bitemporal validity filtering** (`valid_from`/`valid_to`) to a traversal costs **< 2x** when the predicate is pushed into the recursive step and a covering index exists. | ≥ 2x |
| **H7** | **Hybrid retrieval** — FTS5 match → seed nodes → 2-hop expansion → rank — completes p95 **< 100 ms** at 100k edges, so graph + text needs no third engine. | p95 ≥ 100 ms |
| **H8** | The graph tables + indexes add **< 2x** to the size of the same edges stored as bare `facts` rows, so keeping the index in `jod.db` costs little disk. | ≥ 2x |
| **H9** | **KuzuDB** beats SQLite recursive CTEs by **> 10x** on 3-hop at 100k edges — the only result that would justify a second engine. | < 10x speedup |
| **H10** | **DuckDB** (with or without PGQ) is **slower than SQLite** on point-seeded traversal, because it is a columnar analytics engine and k-hop is a point-lookup workload. | DuckDB faster |
| **H11** | No SQLite graph extension that is (a) maintained in 2026, (b) permissively licensed and (c) **statically linkable into a Rust binary** exists — so the "extension" the question names is not purchasable at any price. | one exists meeting all three |
| **H12** | **Community detection** (label propagation) is not a query-time operation at any scale — a single iteration over 100k edges alone exceeds 100 ms — so it belongs in a periodic rebuild job, not in recall. | one iteration < 100 ms and converges in-query |
| **H13** | Multi-process concurrency is unaffected: a reader running a 3-hop traversal while 4 supervisor processes append events sees **0 errors** and **< 2x** traversal slowdown under WAL. | errors > 0 or ≥ 2x slowdown |
| **H14** | **`sqlite-vec`** brute force over 100k memory vectors stays **< 100 ms**, so a graph + vector hybrid does not need an ANN index or a second engine. | p95 ≥ 100 ms |

## What each verdict decides

- H1, H4, H7 falsified → an extension or a second engine becomes necessary.
- H2, H3 confirmed → the schema needs a **degree cap / hub guard**, not a faster
  engine.
- H5 confirmed → shortest path lives in Rust as a bidirectional BFS, not in SQL.
- H9, H10 falsified → the one-file property is worth reconsidering.
- H11 falsified → re-evaluate; a static, maintained, permissive extension would
  be the cheapest answer to the question as asked.
