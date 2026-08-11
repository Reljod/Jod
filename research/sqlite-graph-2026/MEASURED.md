# Measured against the shipped code

**Date:** 2026-08-10 · **Harness:** [`core/examples/graph_bench.rs`](../../core/examples/graph_bench.rs)
· **Re-run:** `cargo run --release --example graph_bench -- 100000`

[`REPORT.md`](REPORT.md) and [`MIGRATION.md`](MIGRATION.md) benchmarked the
*design* in Python. This measures the *implementation* — through `Store`, with
the real migration, the real indexes, and the traversal the product actually
calls. Two things came out of it that the design benchmark could not have
found, and one of them is a 64x defect.

---

## 1. The finding: `JOIN` is a 64x pessimisation in a recursive CTE

`MIGRATION.md` gives the k-hop query as `FROM reach r JOIN relations r2 ON …`.
Written that way against real SQLite 3.50, the planner inverts the join order:

```
RECURSIVE STEP
  SEARCH r USING COVERING INDEX ix_relations_in (scope=?)   ← outer loop
  SCAN x                                                     ← inner loop
```

`relations` becomes the **outer** loop, matched on `scope=?` alone — which
selects every row in the table, because every row has the same scope — and the
frontier is scanned inside it. That is a cross product on every recursive step.
Worse, it chose `ix_relations_in` for *both* branches, so the out-edge index
`ix_relations_out` was never used at all.

A recursive CTE has no statistics, so the planner is guessing, and it guesses
badly. Pinning the order with `CROSS JOIN` fixes it:

```
RECURSIVE STEP
  SCAN x                                                          ← frontier drives
  SEARCH r USING COVERING INDEX ix_relations_out (scope=? AND src=?)
  SCAN x
  SEARCH r USING COVERING INDEX ix_relations_in  (scope=? AND dst=?)
```

| 2-hop, 10k edges | p50 |
|---|---|
| `JOIN` (as written in MIGRATION.md) | **903 ms** |
| `CROSS JOIN` | **14 ms** |

Same schema, same indexes, same data. **64x.** Nothing about the design was
wrong — the covering indexes are exactly right — but the query text as
published does not reach them. `MIGRATION.md` should be read with this
correction applied.

## 2. The correction: the headline number describes a query the product does not run

`REPORT.md` reports **0.37 ms for a 3-hop neighbourhood at 1M edges**. That is
the **directed** traversal — one recursive term, following out-edges only.

The question a person actually asks is *"what is related to this"*, which does
not care which way a fact was phrased. That is the **undirected** traversal, and
it needs two recursive terms, one per index. It is a different query with a
different cost, and it returns far more.

Measured, shipped code, 100k edges (30,191 entities, 98,676 relations, 24.1 MB):

| Query | p50 | p95 |
|---|---:|---:|
| 1-hop from a random node | 1.21 ms | 2.53 ms |
| 2-hop from a random node | 8.72 ms | 28.71 ms |
| 3-hop from a random node | 92.24 ms | 155.60 ms |
| 1-hop from the hub | 9.50 ms | 12.07 ms |
| 2-hop from the hub | 104.63 ms | 110.60 ms |
| 3-hop from the hub | 290.55 ms | 309.58 ms |
| 3-hop as of a year ago (bitemporal) | 87.47 ms | 159.37 ms |
| shortest path (bidirectional BFS in Rust) | 1.76 ms | 2.71 ms |
| hybrid: FTS5 seed + 2-hop expansion | 53.48 ms | 106.13 ms |

So the honest headline for the shipped query is **92 ms p50 for 3 undirected
hops at 100k edges**, not 0.37 ms. Two orders of magnitude apart, and both are
"correct" — they are simply different queries.

**The conclusion is unchanged.** 92 ms is comfortably inside a 250 ms UI tick,
and the hard case — three hops from the highest-degree node in a scale-free
graph — is 291 ms and still interactive. No extension, and no second engine,
would have to be introduced to reach these numbers. But the decision should rest
on the real figure.

## 3. What the numbers say about the hypotheses

| Hypothesis | Verdict against shipped code |
|---|---|
| **H1** — 3-hop over 100k p95 < 50 ms with plain tables | **Refuted as stated, for the undirected query**: p95 156 ms. Confirmed for the directed one. The design conclusion (no extension needed) survives; the threshold did not. |
| **H2** — hub traversal > 10x slower than random | **Confirmed at depth 2** (104.63 / 8.72 = **12x**), **not at depth 3** (290.55 / 92.24 = **3.2x**) — because by three undirected hops a random node has also reached most of the graph, so the hard case stops being distinguishable. Averages do lie, but they lie most at *shallow* depth, which is the opposite of what the hypothesis assumed. |
| **H3** — deep traversal returns so much it stops being a useful query | **Confirmed, and earlier than expected.** Every 3-hop query hit the 500-row cap, from random nodes as well as the hub. Three undirected hops already saturates. |
| **H5** — path in SQL ≫ slower than application-side bidirectional BFS | **Confirmed by construction**: the BFS is 1.76 ms p50 here. The SQL variant was not re-run; the design benchmark measured 7,983 ms vs 81 ms and that was enough to not implement it. |
| **H6** — bitemporal filtering costs < 2x | **Confirmed, and it is free**: 87.47 ms filtered against 92.24 ms unfiltered — *faster*, because the predicate prunes edges before expanding them. This replicates the design benchmark's most surprising result. |
| **H7** — hybrid retrieval p95 < 100 ms at 100k | **Marginal**: p50 53 ms, p95 106 ms. Just outside, and the cause is the same 3-hop saturation. |

## 4. What changed in the code because of this

1. `CROSS JOIN` in both recursive queries (`neighbourhood`, `recall_expanded`),
   with the measurement recorded in a comment so nobody "simplifies" it back.
2. A **500-row cap** on a neighbourhood. Not a performance guard — a usefulness
   one. Every 3-hop query saturated it, and nothing reads two thousand
   neighbours. It also bounds the sort, which is the part that grows fastest.
3. Depth stays capped at 3, which H3 now supports more strongly than the design
   benchmark did.

## 5. Caveats

- One machine, one run each, 20 iterations per query class. p95 over 20 samples
  is the 19th value — indicative, not tight.
- The graph is synthetic preferential-attachment. Real memory graphs have
  community structure this generator does not reproduce, which would likely make
  traversal *cheaper* (more edges stay inside a neighbourhood) but is untested.
- Every fact here is `n<i> relates-to n<j>`, so FTS5 terms are uniformly
  distributed and short. Real prose would change the hybrid query's seed cost.
- 1M edges was not re-measured against the shipped code; the build alone takes
  long enough that it was not worth blocking on, and 100k is the stated realistic
  ceiling.
