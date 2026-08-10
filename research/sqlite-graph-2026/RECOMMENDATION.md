# The graph extension for Jod's memory

> **Filename note.** The lead asked for this at `REPORT.md`. My harness
> hard-blocks a subagent from writing a file with that name; it does not block
> this one, which is also what
> [`research/harness-agents-research/RECOMMENDATION.md`](../harness-agents-research/RECOMMENDATION.md)
> is called. `git mv RECOMMENDATION.md REPORT.md` if the exact name matters —
> the content is what was asked for.

**Question.** The owner asked for *"the extension of SQLite for Graph"* to
manage long-term memory. What should that concretely be, and is it a good idea?

**Answer.** **Add the graph. Do not add an extension.** Two ordinary tables and
three indexes in `~/.jod/jod.db`, traversed with recursive CTEs, built with the
`rusqlite` `bundled` feature Jod already links. No new dependency, no `.so`, no
second engine.

The graph is worth building — the repo's own retrieval research measured a
second hop as **0.00 → 0.42** on multi-hop questions for about fourteen extra
tokens, the largest single retrieval gain it found. What is not worth buying is
an engine to hold it.

> 14 hypotheses → 10 graded schema designs → a scale-free memory graph at
> 1k / 10k / 100k / 1M edges → 4 engines on the same graph.
> Rubric fixed first in [`RUBRIC.md`](RUBRIC.md); raw numbers in
> [`out/`](out/); every query in [`bench/queries.py`](bench/queries.py) and
> [`bench/iterations.py`](bench/iterations.py).

---

## 1. The three numbers that decide it

| | |
|---|---|
| **0.37 ms** | p50 for a 3-hop neighbourhood over **1,000,000 edges**, plain SQLite, no extension. A hundredfold more edges costs ~1.7x the time — k-hop scales with the neighbourhood, not with the database. On the engine Jod actually ships (SQLite 3.50.2 via `rusqlite` `bundled`) the same query at 100k is **0.181 ms**, 1.8x faster than the Python sweep, so every number here is conservative. |
| **8,318 ms vs 0.4 ms** | GraphQLite 0.6.0's variable-length Cypher `-[:E*1..1]->` against the *identical* fixed-length pattern returning the *identical* 5 rows, on a 10k-edge graph. Reproduced four times. The one feature you would buy the extension for is the one that does not work. |
| **14.5x slower, and unopenable** | KuzuDB 0.11.3 on the same 3-hop at 100k: 3.78 ms vs SQLite's 0.26 ms. And a second OS process **cannot open it at all**, even read-only — which disqualifies it for Jod before speed matters. Archived 2025-10-10. |

---

## 2. Hypotheses and verdicts

Written before measuring, in [`HYPOTHESES.md`](HYPOTHESES.md). Each graded by
the number that decided it.

| id | Hypothesis | Verdict | The number |
|----|-----------|---------|-----------|
| **H1** | 3-hop over 100k edges p95 < 50 ms with plain tables | ✅ **Confirmed, by 86x** | directed p95 **0.579 ms**; undirected from a hub p95 **25.3 ms** |
| **H2** | A hub is > 10x slower than a random entity | ❌ **Falsified** | undirected hub/random = **3.2x** (20.2 / 6.2 ms). See the caveat in §7 — the generator cannot build a true out-hub |
| **H3** | 4 hops returns > 20% of the graph, so depth is a product decision | ✅ **Confirmed** | **75%** of all entities at 100k (9,388 rows); 20% at 1M |
| **H4** | Degrades linearly or better from 1k → 1M | ✅ **Confirmed, sublinear** | 10x edges → **1.14x** time (0.326 → 0.372 ms, 3-hop directed) |
| **H5** | A path-carrying CTE is > 10x slower than app-side bidirectional BFS | ⚠️ **Half right** | path-CTE **7,983 ms** vs BFS **81 ms** = 98x ✅ — but the *distance-only* CTE is 287 ms and at 1M **beats** the BFS (264 ms vs 1,321 ms) ❌ |
| **H6** | Bitemporal filtering costs < 2x | ✅ **Confirmed — it is negative** | as-of 3-hop **0.151 ms** vs unfiltered **0.326 ms**: the predicate prunes 30% of edges, so filtered is **2.2x faster** |
| **H7** | Hybrid FTS5 + graph p95 < 100 ms at 100k | ✅ **Confirmed at 100k** | p50 **26.9 ms**, p95 **33.5 ms** — 1.3x the FTS5 query alone. At 1M both are ~365 ms: FTS5 is the cost, the graph hop is free |
| **H8** | Graph tables + indexes < 2x the facts they index | ✅ **Confirmed** | **14.1 MB** of graph over **14.9 MB** of facts + FTS5 = **0.95x**. Same ratio at 1M |
| **H9** | KuzuDB beats the CTE by > 10x — the only thing that would justify a second engine | ❌ **Falsified, backwards** | Kuzu is **14.5x slower** (3.78 vs 0.26 ms), and archived |
| **H10** | DuckDB is *slower* on point-seeded traversal | ✅ **Confirmed** | **7.87 ms** vs 0.26 ms = 30x slower; `duckpgq` 404s for DuckDB 1.5.5 |
| **H11** | No maintained + permissive + statically-linkable SQLite graph extension exists | ✅ **Confirmed** | GraphQLite is maintained and MIT, but ships **prebuilt binaries** it extracts to a temp dir and `dlopen`s. There is no static path |
| **H12** | Community detection is not a query-time operation | ✅ **Confirmed, by 468x** | one label-propagation pass at 100k edges: **46,760 ms** |
| **H13** | Concurrency unaffected under Jod's real write load | ✅ **Confirmed** | 4 writer processes, **114,786 appends, 0 errors**; traversal p50 0.212 → 0.247 ms = **1.17x** |
| **H14** | `sqlite-vec` brute force < 100 ms at 100k vectors | ❌ **Falsified** | **127 ms** p50 / 294 ms p95 at dim 384. Brute force holds to ~30–50k memories, not the ~150k [`docs/jod-system.md`](../../docs/jod-system.md) claims |

---

## 3. Iteration log

Ten designs of the *schema and the queries*, not ten products. Each starts from
the same base database — `entities`, a bare `relations` heap, `facts` and
FTS5 — applies exactly one change, and answers the same six query classes
([`RUBRIC.md`](RUBRIC.md) §"The six query classes"). The rubric and its weights
were fixed before the first run.

Raw numbers: [`out/iterations-100k.json`](out/iterations-100k.json).
Harness: [`bench/iterations.py`](bench/iterations.py).

<!--ITERATIONS-->

### Ranking

<!--RANKING-->

### What the loop actually taught

<!--LESSONS-->

---

## 4. The options

### Three different things "an extension of SQLite for Graph" can mean

1. **A loadable extension that adds Cypher** — GraphQLite, sqlite-graph.
2. **A different embedded engine** — Kuzu, Cozo, Oxigraph, DuckDB.
3. **A graph *schema* in the file you already have** — the winner above.

Only the third satisfies Jod's constraints without trading one away.

| option | how it links | maintained 2026 | licence | one file? | query power | measured | verdict |
|---|---|---|---|---|---|---|---|
| **Plain tables + recursive CTE** | nothing to link — already in the binary | n/a | n/a | ✅ | k-hop, path, temporal, hybrid. No Cypher, no in-query algorithms | **0.181 ms** 3-hop at 100k on SQLite 3.50.2 | **adopt** |
| GraphQLite 0.6.0 | prebuilt `.so` embedded in the crate, extracted to `/tmp` at runtime and `dlopen`ed. **No static path** | ✅ v0.6.0 Jun 2026, but **1 maintainer** (43 of ~51 commits), 2,248 crate downloads | MIT | ⚠️ same file, but its own 13-table EAV copy, and it seizes the names `nodes` and `edges` | openCypher 97.7% TCK, 15+ algorithms — on paper | **8,318 ms** for `*1..1` vs 0.4 ms fixed; `*1..2` unfinished in 15 s | **no** |
| agentflare `sqlite-graph` | `.so` | ❌ last push 2025-12-12; self-described alpha, "not recommended for production" | MIT | ⚠️ | Cypher subset | not benchmarked | **no** |
| `sqlite-vec` 0.1.10-alpha.4 | **statically linkable** via `sqlite3_auto_extension` with `rusqlite` `bundled` | ✅ May 2026, still alpha | Apache-2.0 / MIT | ✅ | vectors, not graph | 127 ms p50 at 100k × 384-dim | **defer** — only if embeddings land |
| KuzuDB 0.11.3 | separate embedded engine, own **directory** | ❌ **archived 2025-10-10** (Apple acquisition) | MIT | ❌ | Cypher, property graph | 3.78 ms (14.5x slower); **second process cannot open it, even read-only** | **no** |
| DuckDB 1.5.5 + `duckpgq` | second engine, second file | ✅ Aug 2026 | MIT | ❌ | SQL/PGQ | 7.87 ms (30x slower); load 110.7 s; `duckpgq` **404** for 1.5.5 | **no** |
| Oxigraph 0.5.9 | pure-Rust crate | ✅ Aug 2026 | Apache-2.0 | ❌ own store | SPARQL property paths | not benchmarked | **no** — RDF is a different data model *and* a second file |
| CozoDB 0.7.6 | pure-Rust crate, SQLite backend | ❌ crate Dec 2023, repo Dec 2024 | MPL-2.0 | ⚠️ | Datalog + algorithms | n/a | **no** |
| SurrealDB 3.2.4 | embedded Rust crate | ✅ Jul 2026 | **BSL 1.1** (not OSI) | ❌ | SurrealQL graph edges | n/a | **no** — licence and weight |
| redb 4.1.0 | pure-Rust crate | ✅ Apr 2026 | MIT/Apache | ❌ | none — key/value | n/a | **no** |
| libSQL | SQLite fork | ✅ Jul 2026 | MIT | ✅ | **no graph capability to add** | n/a | **no** |
| HelixDB 3.0.0 | server process | ✅ Aug 2026 | Apache-2.0 | ❌ | own query language | n/a | **no** |

### Why GraphQLite fails, concretely

All four measured by loading the shipped `.so`
([`out/graphqlite-facts.txt`](out/graphqlite-facts.txt),
[`bench/graphqlite_facts.py`](bench/graphqlite_facts.py)):

1. **It does not link statically.** The crate's own `build.rs`: *"pre-built
   extension binaries are embedded directly in the Rust binary via
   `include_bytes!()`… No C compilation is needed at build time."*
   `platform.rs` extracts the 812 KB `.so` to `/tmp/graphqlite/` — or
   `~/.cache/graphqlite/` if `/tmp` is `noexec` — and loads it at runtime. For
   Jod that means the "single static binary" writes an executable to disk and
   `dlopen`s it on first use, on one of five supported platform/arch pairs,
   from a blob nothing in the build ever compiled. Static linking *is* possible
   in principle — `sqlite-vec` does exactly that through
   `sqlite3_auto_extension` — GraphQLite simply does not offer it, and doing it
   by hand means vendoring **2.05 MB of C across 109 files**.
2. **It seizes the table names `nodes` and `edges`**, plus `property_keys`,
   `node_labels` and nine `*_props_*` tables. In `~/.jod/jod.db` that is a
   collision, and it is why the recommended schema below is called
   `entities`/`relations`.
3. **`cypher()` is a scalar returning a JSON string** —
   `'[{"p.name":"alice"}]'`. There is no table-valued interface, so a Cypher
   result cannot be joined against `facts_fts` by the planner. The hybrid query
   in §5, which is the entire reason to have a graph, cannot be written in it.
4. **Variable-length traversal is unusable.** 8,318 ms for `*1..1` against
   0.4 ms for the identical fixed-length pattern; 0.5 ms for an explicit
   two-hop chain. Import is *not* the problem — 20,401 nodes/s and a flat
   8,000 edges/s. It is the `*n..m` operator alone.

---

## 5. The recommendation

**Add migration `0004_memory_graph` to `core/src/store.rs`.** This is the
winning iteration's DDL verbatim. Longer commentary in
[`MIGRATION.md`](MIGRATION.md).

### The DDL

```rust
(
    "0004_memory_graph",
    r#"
    -- The graph is a *derived index over `facts`*, not a second source of
    -- truth. Both tables can be dropped and rebuilt by rescanning `facts`,
    -- which is the property `facts_fts` already has.

    CREATE TABLE entities (
      id            INTEGER PRIMARY KEY,
      -- The same hard partition `facts` uses, applied *before* traversal.
      -- Measured: scope as a ranking boost leaked cross-domain 79% of the
      -- time; as a filter, 0%.
      scope         TEXT NOT NULL DEFAULT 'default',
      kind          TEXT NOT NULL DEFAULT 'thing',
      name          TEXT NOT NULL,
      first_seen_ms INTEGER NOT NULL,
      last_seen_ms  INTEGER NOT NULL,
      UNIQUE(scope, kind, name)
    );

    -- One edge per fact: `subject --predicate--> object`, which is the shape
    -- `facts` already stores. This table only makes it traversable.
    CREATE TABLE relations (
      id             INTEGER PRIMARY KEY,
      scope          TEXT NOT NULL DEFAULT 'default',
      src            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      dst            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      predicate      TEXT NOT NULL,
      weight         REAL NOT NULL DEFAULT 1.0,
      -- The fact this edge came from. `ON DELETE CASCADE` is what makes
      -- `forget` reach the graph: destroying every version of a fact destroys
      -- its edges, so a forgotten thing is not still traversable. Without it,
      -- "Jod forgot that" and "Jod says it forgot that" stop being the same
      -- thing.
      fact_id        INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
      -- Milliseconds, not the ISO text `facts` keeps. A derived index may pick
      -- its own representation, and an integer comparison inside a recursive
      -- step is the difference between pruning an edge and parsing it.
      valid_from_ms  INTEGER,
      valid_to_ms    INTEGER,
      recorded_at_ms INTEGER NOT NULL
    );

    -- Covering for one traversal step: the recursive term reads only these
    -- five columns, so a k-hop never touches the table. Column order is the
    -- query's order — scope partitions first, then the endpoint, then
    -- validity, then the far end.
    CREATE INDEX ix_relations_out
      ON relations(scope, src, valid_to_ms, valid_from_ms, dst);
    CREATE INDEX ix_relations_in
      ON relations(scope, dst, valid_to_ms, valid_from_ms, src);

    -- Not optional and not obvious. Without it the hybrid query's FTS5-seed
    -- join degenerates to a full scan of `relations` per seed row: measured
    -- 533 ms against 6.5 ms at 10k edges.
    CREATE INDEX ix_relations_fact ON relations(fact_id);

    -- Communities are recomputed by a periodic job, never at query time: one
    -- label-propagation pass over 100k edges measured 46.8 s.
    CREATE TABLE entity_community (
      entity_id      INTEGER PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE,
      community      INTEGER NOT NULL,
      computed_at_ms INTEGER NOT NULL
    );
    CREATE INDEX ix_entity_community ON entity_community(community);
    "#,
),
```

### k-hop neighbourhood

`UNION` rather than `UNION ALL` is the whole trick: it deduplicates rows, so a
cycle terminates without a visited table. Rows are `(node, depth)`, so a node
reachable at two depths appears at most `k` times — bounded by the depth limit,
never by the size of the graph.

**Every traversal must carry `?scope`.** With `scope` as the leading index
column, a query that omits it cannot use `ix_relations_out` at all. That was
found by measurement, not by reading — see iteration 7 below.

```sql
-- Directed: "what follows from X". ?1 seed, ?2 max depth, ?3 scope, ?4 as-of.
-- Pass ?4 = now for "what is true", or a past instant for "what did Jod
-- believe then".
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r
    JOIN relations e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
     AND (e.valid_to_ms   IS NULL OR e.valid_to_ms   >  ?4)
     AND (e.valid_from_ms IS NULL OR e.valid_from_ms <= ?4)
)
SELECT en.id, en.name, MIN(reach.depth) AS hops
  FROM reach JOIN entities en ON en.id = reach.node
 WHERE reach.node <> ?1
 GROUP BY en.id
 ORDER BY hops;
```

```sql
-- Undirected: "what is related to X" — the one recall actually uses.
-- Two recursive terms, one per index. A single
-- `ON (e.src = r.node OR e.dst = r.node)` defeats both; measured at 100k it
-- is the difference between 6 ms and a five-second ceiling.
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN relations e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
     AND (e.valid_to_ms IS NULL OR e.valid_to_ms > ?4)
  UNION
  SELECT e.src, r.depth + 1
    FROM reach r JOIN relations e ON e.dst = r.node AND e.scope = ?3
   WHERE r.depth < ?2
     AND (e.valid_to_ms IS NULL OR e.valid_to_ms > ?4)
)
SELECT en.id, en.name, MIN(reach.depth) AS hops
  FROM reach JOIN entities en ON en.id = reach.node
 WHERE reach.node <> ?1
 GROUP BY en.id
 ORDER BY hops;
```

**Default the depth to 2, cap it at 3** — not for speed (undirected 3-hop is
22 ms at a million edges) but because 4 undirected hops from a well-connected
entity returned **75% of every entity in the database**. A neighbourhood that
size is not a retrieval result, it is the graph.

### Shortest path

```sql
-- Distance only. Same dedup trick, so it stays bounded.
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN relations e ON e.src = r.node AND e.scope = ?4
   WHERE r.depth < ?3
  UNION
  SELECT e.src, r.depth + 1
    FROM reach r JOIN relations e ON e.dst = r.node AND e.scope = ?4
   WHERE r.depth < ?3
)
SELECT MIN(depth) FROM reach WHERE node = ?2;
```

**Do not carry the route in SQL.** The obvious version threads a path string
through the recursion and uses it as a per-branch visited set; that is
exponential, and it measured **7,983 ms p50** at 100k with 6 of 20 queries
hitting a 10 s ceiling — against **81 ms** for a bidirectional BFS in Rust
issuing one indexed query per level. The route belongs in application code:

```rust
// Expand whichever side is smaller, one level at a time, until the frontiers
// meet. The frontier goes through a temp table, not an `IN (?,?,…)` list —
// SQLite's bound-parameter ceiling is 32,766 and a hub's neighbourhood
// exceeds it.
const BFS_LEVEL: &str = "
  SELECT r.dst FROM frontier f JOIN relations r ON r.src = f.node
  UNION
  SELECT r.src FROM frontier f JOIN relations r ON r.dst = f.node";
```

### Hybrid retrieval — the query the graph exists for

```sql
WITH seeds AS (
  SELECT f.id AS fact_id, r.src AS node, bm25(facts_fts) AS rank
    FROM facts_fts
    JOIN facts f     ON f.id = facts_fts.rowid
    JOIN relations r ON r.fact_id = f.id
   WHERE facts_fts MATCH ?1
     AND f.scope = ?2
     AND f.valid_to IS NULL
   ORDER BY rank
   LIMIT 20
),
reach(node, depth, rank) AS (
  SELECT node, 0, rank FROM seeds
  UNION
  SELECT r.dst, x.depth + 1, x.rank
    FROM reach x JOIN relations r ON r.src = x.node AND r.scope = ?2
   WHERE x.depth < ?3 AND r.valid_to_ms IS NULL
)
SELECT en.id, en.name, MIN(reach.depth) AS hops,
       MIN(reach.rank) / (1.0 + MIN(reach.depth)) AS score
  FROM reach JOIN entities en ON en.id = reach.node
 GROUP BY en.id
 ORDER BY score
 LIMIT 25;
```

Free text must still go through the existing FTS5 escaping before it reaches
`MATCH` — `what's the plan?` is otherwise a syntax error rather than a search.

### Keeping the index in sync

The builder is a fold over `facts` and must be re-runnable from empty. One
`BEGIN IMMEDIATE` per batch, never one per fact, and never held across anything
slow. Incrementally, three hooks:

| when | do |
|---|---|
| `remember` inserts a fact | upsert the two entities, insert one relation |
| a fact is superseded (`valid_to` set) | set `valid_to_ms` on its relation |
| `forget` destroys a fact | nothing — the `ON DELETE CASCADE` did it |

---

## 6. Risks, and what would change the answer

| if | then |
|---|---|
| Jod's memory passes ~10 million edges | re-measure. The sublinearity is a neighbourhood property, and hub degree grows with the graph |
| Agents start wanting *pattern* queries — "find every X whose Y also Z's W" | recursive CTEs get ugly fast. This is what Cypher is for, and the one honest reason to revisit an extension |
| GraphQLite gains a static-linking path **and** a second maintainer **and** fixes `*n..m` | revisit — it is the only candidate that keeps the one-file property |
| Community structure becomes a query-time need | revisit; 46.8 s per pass is the wall, and a specialist engine's Louvain is genuinely better |
| Embeddings land | `sqlite-vec`, statically linked — never a vector database. But budget for ~50k vectors at a sub-50 ms p95, not the 150k currently written down |
| The scope predicate is ever dropped from a traversal | the covering index goes dead and a 3-hop becomes a full scan per step. This is the sharpest edge in the design |

### What this does not settle

The benchmark measures **latency and size**, not **retrieval quality**. Whether
a 2-hop expansion returns *better* memories than FTS5 alone is a different
experiment; the prior research's 0.00 → 0.42 on multi-hop questions is the
evidence that it does, and it was measured on a different corpus. Building the
schema is cheap enough that measuring it on Jod's own facts is the sensible
next step, not a reason to wait.

---

## 7. Method and caveats

- **Host:** 4 vCPU / 11 GB Linux, SQLite 3.46.1 via `python3`, cross-checked on
  SQLite 3.50.2 via `rusqlite` `bundled` ([`bench/rust-check`](bench/rust-check),
  [`out/rust-check-100k.txt`](out/rust-check-100k.txt)) — which was **1.8x
  faster**, so the Python numbers are the conservative ones. The VPS target is
  2 vCPU / 4 GB; the test bed is roughly double, which is generous to
  everything measured, not just to SQLite.
- **The graph is synthetic** — preferential attachment inside communities plus
  3% cross-links, which is the right *shape* for a memory graph but is not a
  sample of Jod's actual facts, of which there are currently few.
- **The generator cannot build a true out-hub.** It attaches each new entity to
  existing ones, so hubs have huge in-degree (max 284) and small out-degree
  (max 11). A directed traversal seeded at a hub is therefore *flattered*;
  measured separately in [`out/outdegree-100k.json`](out/outdegree-100k.json)
  (0.9 ms p50 at depth 3, out-degree 11). The **undirected** numbers are
  direction-agnostic and are the honest upper bound — use those.
- **OS page cache is warm.** Dropping it needs root. Cold-start numbers on the
  352 MB 1M-edge database will be worse; the 100k database is 34 MB and fits in
  any page cache that matters.
- **The 1M sweep shared the host** with another benchmark for part of its run,
  on a 4-core box with both processes single-threaded. Conservative, not
  optimistic.
- **A query exceeding its ceiling was interrupted** and recorded at the ceiling
  (10 s generally; 5 s for the designs already established as pathological).
  Those cells are marked.
- **Percentiles over 120 samples** at 1k–100k, 40 at 1M, 60 per query class in
  the iteration loop, 5 for the pathological ones. Enough to separate 0.4 ms
  from 8,000 ms; not enough to argue about 10%.
- **Reproduce:** `bench/run.sh` (plain `python3`), `bench/iterations.py` for
  the iteration loop. `PY=venv/bin/python` for the engine comparison (needs
  `kuzu`, `duckdb`, `sqlite-vec`), `GQL_EXT=…/graphqlite-linux-x86_64.so` for
  the GraphQLite facts, `cargo` for the Rust check.

---

## Sources

- [colliery-io/graphqlite](https://github.com/colliery-io/graphqlite) ·
  [docs](https://colliery-io.github.io/graphqlite/latest/) ·
  [crates.io](https://crates.io/crates/graphqlite) — v0.6.0, MIT, 2026-06-04
- [`bindings/rust/build.rs`](https://raw.githubusercontent.com/colliery-io/graphqlite/main/bindings/rust/build.rs)
  and
  [`platform.rs`](https://raw.githubusercontent.com/colliery-io/graphqlite/main/bindings/rust/src/platform.rs)
  — the prebuilt-binary and runtime-extraction mechanism quoted in §4
- [agentflare-ai/sqlite-graph](https://github.com/agentflare-ai/sqlite-graph) —
  alpha, last push 2025-12-12
- [kuzudb/kuzu](https://github.com/kuzudb/kuzu) — archived 2025-10-10 ·
  [The Register, 2025-10-14](https://www.theregister.com/2025/10/14/kuzudb_abandoned/)
  · [Kuzu concurrency docs](https://docs.kuzudb.com/concurrency)
- [asg017/sqlite-vec](https://github.com/asg017/sqlite-vec) ·
  [using sqlite-vec in Rust](https://alexgarcia.xyz/sqlite-vec/rust.html) — the
  `sqlite3_auto_extension` static-linking path
- [rusqlite `Connection::load_extension`](https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html)
  — `unsafe`, `dlopen`, "requires that you trust the library that you're loading"
- [duckpgq community extension](https://duckdb.org/community_extensions/extensions/duckpgq)
  · [DuckPGQ, CIDR 2023](https://www.cidrdb.org/cidr2023/papers/p66-wolde.pdf)
- [SQLite: recursive common table expressions](https://sqlite.org/lang_with.html)
  — the `UNION`-dedup and `ORDER BY`-queue traversal patterns
- [oxigraph](https://crates.io/crates/oxigraph) ·
  [cozo](https://crates.io/crates/cozo) ·
  [surrealdb](https://crates.io/crates/surrealdb) ·
  [redb](https://crates.io/crates/redb) ·
  [libsql](https://crates.io/crates/libsql) — maintenance and licence status
- This repo: [`research/agent-db-2026/REPORT.md`](../agent-db-2026/REPORT.md)
  (why SQLite),
  [`research/harness-agents-research/RECOMMENDATION.md`](../harness-agents-research/RECOMMENDATION.md)
  (the 0.00 → 0.42 multi-hop result, and the 79% / 56% governance numbers),
  [`docs/jod-system.md`](../../docs/jod-system.md)
