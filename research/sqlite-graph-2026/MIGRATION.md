# The migration, as it would be written

Drop-in for `MIGRATIONS` in [`core/src/store.rs`](../../core/src/store.rs),
after `0003_process_supervision`. Nothing here needs a new dependency: it is
`rusqlite` with the `bundled` feature Jod already links, and SQLite 3.50.2's
recursive CTEs.

Every measurement behind these choices is in [`REPORT.md`](REPORT.md).

---

## 1. The tables

```sql
(
    "0004_memory_graph",
    r#"
    -- The graph is a *derived index over `facts`*, not a second source of
    -- truth. Both tables can be dropped and rebuilt by rescanning `facts`,
    -- which is what keeps markdown and `facts` authoritative — the same
    -- property `facts_fts` already has.

    -- An entity is a thing facts talk about. `name` is the fact's subject or
    -- object text, normalised once at index time so traversal compares
    -- integers rather than strings.
    CREATE TABLE entities (
      id            INTEGER PRIMARY KEY,
      -- The same hard partition `facts` uses. Applied *before* traversal,
      -- never as a ranking signal: measured leaking 79% cross-domain as a
      -- boost, 0% as a filter.
      scope         TEXT NOT NULL DEFAULT 'default',
      kind          TEXT NOT NULL DEFAULT 'thing',
      name          TEXT NOT NULL,
      first_seen_ms INTEGER NOT NULL,
      last_seen_ms  INTEGER NOT NULL,
      UNIQUE(scope, kind, name)
    );

    -- One edge per fact. `subject --predicate--> object`, which is the shape
    -- `facts` already stores; this table only makes it traversable.
    CREATE TABLE relations (
      id             INTEGER PRIMARY KEY,
      scope          TEXT NOT NULL DEFAULT 'default',
      src            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      dst            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      predicate      TEXT NOT NULL,
      weight         REAL NOT NULL DEFAULT 1.0,
      -- The fact this edge came from. `ON DELETE CASCADE` is what makes
      -- `forget` reach the graph: destroying every version of a fact
      -- destroys its edges too, so a forgotten thing is not still
      -- traversable. Without it, "Jod forgot that" and "Jod says it forgot
      -- that" stop being the same thing.
      fact_id        INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
      -- Milliseconds, not the ISO text `facts` keeps. A derived index may
      -- pick its own representation, and an integer comparison inside a
      -- recursive step is the difference between pruning an edge and
      -- parsing it.
      valid_from_ms  INTEGER,
      valid_to_ms    INTEGER,
      recorded_at_ms INTEGER NOT NULL
    );

    -- Both indexes are *covering* for one traversal step: the recursive term
    -- reads only these five columns, so k-hop never touches the table. The
    -- column order is the query's order — scope partitions first, then the
    -- endpoint, then validity, then the far end.
    CREATE INDEX ix_relations_out
      ON relations(scope, src, valid_to_ms, valid_from_ms, dst);
    CREATE INDEX ix_relations_in
      ON relations(scope, dst, valid_to_ms, valid_from_ms, src);

    -- Not optional, and not obvious. Without it the hybrid query's
    -- FTS5-seed join degenerates to a full scan of `relations` per seed
    -- row: measured 533 ms against 6.5 ms at 10k edges, an 82x difference
    -- from one index.
    CREATE INDEX ix_relations_fact ON relations(fact_id);

    -- Communities are recomputed by a periodic job, never at query time:
    -- one label-propagation pass over 100k edges measured 46.8 s.
    CREATE TABLE entity_community (
      entity_id      INTEGER PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE,
      community      INTEGER NOT NULL,
      computed_at_ms INTEGER NOT NULL
    );
    CREATE INDEX ix_entity_community ON entity_community(community);
    "#,
),
```

**Why not `nodes` and `edges`.** Those are the two table names GraphQLite
creates unconditionally when its extension is loaded — verified by loading the
shipped `.so` and reading `sqlite_master`. Naming Jod's tables `entities` and
`relations` keeps the option of ever loading that extension from being a schema
collision in `~/.jod/jod.db`.

## 2. k-hop neighbourhood

`UNION` rather than `UNION ALL` is the whole trick: it deduplicates rows, so a
cycle terminates without a visited table. The rows are `(node, depth)`, so a
node reachable at two depths appears at most `k` times — bounded by the depth
limit, never by the size of the graph.

```sql
-- Directed: follow out-edges. p50 0.37 ms at 1M edges.
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT r2.dst, r.depth + 1
    FROM reach r
    JOIN relations r2 ON r2.src = r.node AND r2.scope = ?3
   WHERE r.depth < ?2
     AND (r2.valid_to_ms IS NULL OR r2.valid_to_ms > ?4)
     AND (r2.valid_from_ms IS NULL OR r2.valid_from_ms <= ?4)
)
SELECT e.id, e.name, MIN(reach.depth) AS hops
  FROM reach JOIN entities e ON e.id = reach.node
 WHERE reach.node <> ?1
 GROUP BY e.id
 ORDER BY hops;
```

Pass `?4` = now for "what is true", or any past instant for "what did Jod
believe then". The predicate sits *inside* the recursive step, so an edge that
was not valid then is never expanded — and because it prunes ~30% of edges, the
filtered traversal measured **faster** than the unfiltered one (0.151 ms vs
0.326 ms at 100k).

Undirected — the case that actually matters for "what is related to X" — needs
two recursive terms, one per index. A single `ON (src = node OR dst = node)`
defeats both:

```sql
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT r2.dst, r.depth + 1
    FROM reach r JOIN relations r2 ON r2.src = r.node AND r2.scope = ?3
   WHERE r.depth < ?2 AND (r2.valid_to_ms IS NULL OR r2.valid_to_ms > ?4)
  UNION
  SELECT r2.src, r.depth + 1
    FROM reach r JOIN relations r2 ON r2.dst = r.node AND r2.scope = ?3
   WHERE r.depth < ?2 AND (r2.valid_to_ms IS NULL OR r2.valid_to_ms > ?4)
)
SELECT e.id, e.name, MIN(reach.depth) AS hops
  FROM reach JOIN entities e ON e.id = reach.node
 WHERE reach.node <> ?1
 GROUP BY e.id
 ORDER BY hops;
```

**Default the depth to 2, cap it at 3.** Not for speed — undirected 3-hop is
22 ms at a million edges. Because of what comes back: 4 undirected hops from a
well-connected entity returned **20% of every node in the database**. A
neighbourhood that size is not a retrieval result, it is the graph.

## 3. Shortest path

Two entities, "how are these connected". Distance only, in one statement:

```sql
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT r2.dst, r.depth + 1
    FROM reach r JOIN relations r2 ON r2.src = r.node
   WHERE r.depth < ?3
  UNION
  SELECT r2.src, r.depth + 1
    FROM reach r JOIN relations r2 ON r2.dst = r.node
   WHERE r.depth < ?3
)
SELECT MIN(depth) FROM reach WHERE node = ?2;
```

**Do not carry the route in SQL.** The obvious version threads a path string
through the recursion and uses it as a per-branch visited set; that is
exponential, and it measured 7,983 ms p50 at 100k edges with 6 of 20 queries
hitting a 10 s ceiling — against 81 ms for a bidirectional BFS in Rust issuing
one indexed query per level. **98x.** The route belongs in application code:

```rust
// Expand whichever side is smaller, one level at a time, until the
// frontiers meet. The frontier goes through a temp table, not an
// `IN (?,?,…)` list — SQLite's bound-parameter ceiling is 32,766 and a
// hub's neighbourhood exceeds it.
const BFS_LEVEL: &str = "
  SELECT r.dst FROM frontier f JOIN relations r ON r.src = f.node
  UNION
  SELECT r.src FROM frontier f JOIN relations r ON r.dst = f.node";
```

## 4. Hybrid retrieval — FTS5 seeds, graph expansion, one ranking

The query the whole thing exists for. BM25 picks the entities the words point
at; the graph supplies the second hop; scope partitions before any of it.

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
SELECT e.id, e.name, MIN(reach.depth) AS hops,
       MIN(reach.rank) / (1.0 + MIN(reach.depth)) AS score
  FROM reach JOIN entities e ON e.id = reach.node
 GROUP BY e.id
 ORDER BY score
 LIMIT 25;
```

27 ms p50 at 100k facts — 1.3x the cost of the FTS5 query alone, for a second
hop the prior retrieval research measured as worth **0.00 → 0.42** on
multi-hop questions for about fourteen extra tokens.

## 5. Keeping it in sync

The builder is a fold over `facts`, and it must be re-runnable from empty:

```
DELETE FROM relations; DELETE FROM entities;
for each fact:  upsert entities(scope, kind, subject/object)
                insert relations(... fact_id, valid_*_ms)
```

Write it inside one `BEGIN IMMEDIATE` per batch, never one per fact, and never
hold the transaction across anything slow — the same rule the rest of the store
already follows.

Incrementally, three hooks:

| when | do |
|---|---|
| `remember` inserts a fact | upsert the two entities, insert one relation |
| a fact is superseded (`valid_to` set) | set `valid_to_ms` on its relation |
| `forget` destroys a fact | nothing — the `ON DELETE CASCADE` did it |

A `PRAGMA integrity_check`-style equivalent for the index is simply: rebuild
into a temp table and compare counts. Cheap, and it makes "the graph drifted"
a detectable state rather than a suspicion.
