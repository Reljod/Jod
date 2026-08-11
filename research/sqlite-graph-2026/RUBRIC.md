# The rubric

Fixed **before** the iteration loop ran, so the winner is picked by score
rather than by whichever design happened to be measured last.

Every criterion is scored **0–5**. Two are computed from measurements; five are
declared judgements, and the declaration is in
[`bench/iterations.py`](bench/iterations.py) next to the design it scores, so a
reader can disagree with a number without having to re-derive it.

| # | Criterion | Weight | How it is scored |
|---|-----------|--------|------------------|
| 1 | **Measured p95 latency** at the target scale (100k edges) | **0.25** | *Computed.* The **worst** p95 across the core query classes the design can answer (Q2 3-hop directed, Q3 3-hop undirected, Q4 3-hop as-of). Worst, not mean: a memory system is judged by the query that makes you wait. `5` ≤1 ms · `4` ≤5 ms · `3` ≤25 ms · `2` ≤100 ms · `1` ≤1000 ms · `0` above |
| 2 | **Query power** | 0.20 | *Computed.* Fraction of the six query classes the design can answer at all, × 5. A design that cannot express bitemporal traversal scores below one that can, however fast it is |
| 3 | **Build / deploy cost** | 0.15 | *Declared.* `5` = nothing to build, already in the binary. Deductions for: a loadable `.so`, a non-`cargo` build step, added binary size, a system package, a platform-specific artefact |
| 4 | **One-file fit** | 0.10 | *Declared.* `5` = lives in `~/.jod/jod.db` alongside `facts` and `events`. `0` = a second file or a directory |
| 5 | **Multi-process safety** | 0.10 | *Declared.* `5` = WAL, several supervisors writing while the TUI and API read. `0` = one process may hold it |
| 6 | **Maintenance risk** | 0.10 | *Declared.* `5` = SQLite core features only. Deductions for third-party code, single-maintainer projects, alpha status, and anything a rebuild has to be kept in sync with |
| 7 | **Schema simplicity** | 0.10 | *Declared.* `5` = one table plus indexes, no derived structure to invalidate. Deductions per extra table, per denormalisation, and heavily for anything needing a rebuild on every write |

Weights sum to 1.00. Score = Σ(weight × criterion).

## Why these weights

Latency and query power carry 0.45 together because they are the only two
things a graph is *for*. Build cost is third at 0.15 rather than fourth,
because Jod's charter makes "a plain `cargo build` on a bare VPS" a property of
the product and not a preference — a design that is 2x faster and needs a
`.so` on disk has not won anything.

The three 0.10 operational criteria are where a good benchmark result goes to
die: an engine that is quick in a single process and cannot be opened by a
second one scores 0 on criterion 5 no matter what criterion 1 says.

## The six query classes

| | class | why it is in the set |
|---|---|---|
| **Q1** | 1-hop out-neighbours | the floor; every design must do this |
| **Q2** | 3-hop directed neighbourhood | "what follows from X" |
| **Q3** | 3-hop undirected neighbourhood | "what is related to X" — the one recall actually uses |
| **Q4** | 3-hop as-of a past instant | bitemporal validity is already in `facts`; a graph that cannot honour it re-introduces the bug supersession was built to fix |
| **Q5** | shortest-path distance between two entities | "how are these two connected" |
| **Q6** | FTS5 seed → 2-hop expansion → rank | hybrid retrieval — the reason to have a graph at all |
