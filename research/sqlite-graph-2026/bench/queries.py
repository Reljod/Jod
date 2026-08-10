"""The candidate query texts, in one place.

Everything the report claims is fast is measured *from this file*, so the SQL
printed in the report is the SQL that was timed. Nothing is simplified for the
benchmark.
"""

# --- k-hop neighbourhood, directed (follow out-edges only) ----------------
# `UNION` (not `UNION ALL`) is the whole trick: it deduplicates rows, so a
# cycle terminates without a visited table. Rows are (node, depth), so a node
# reachable at two depths appears twice — bounded by k, not by the graph.
KHOP_DIRECTED = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r
    JOIN edges e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) AS hops FROM reach WHERE node <> ?1 GROUP BY node
"""

# --- k-hop, undirected: two recursive terms, one per index ----------------
# `ON (e.src = r.node OR e.dst = r.node)` would defeat both indexes. Two
# compound recursive SELECTs keep each side on its own covering index.
KHOP_UNDIRECTED = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN edges e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
  UNION
  SELECT e.src, r.depth + 1
    FROM reach r JOIN edges e ON e.dst = r.node AND e.scope = ?3
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) AS hops FROM reach WHERE node <> ?1 GROUP BY node
"""

# --- k-hop, as-of a point in time (bitemporal) ---------------------------
# The validity predicate sits inside the recursive step, so an edge that was
# not true at ?4 is never expanded — not filtered out afterwards.
KHOP_ASOF = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r
    JOIN edges e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
     AND (e.valid_to_ms IS NULL OR e.valid_to_ms > ?4)
     AND (e.valid_from_ms IS NULL OR e.valid_from_ms <= ?4)
)
SELECT node, MIN(depth) AS hops FROM reach WHERE node <> ?1 GROUP BY node
"""

# --- shortest path: distance only ----------------------------------------
# Same dedup trick. Returns the hop count, not the route.
SP_DISTANCE = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN edges e ON e.src = r.node
   WHERE r.depth < ?3
  UNION
  SELECT e.src, r.depth + 1
    FROM reach r JOIN edges e ON e.dst = r.node
   WHERE r.depth < ?3
)
SELECT MIN(depth) FROM reach WHERE node = ?2
"""

# --- shortest path: carrying the route -----------------------------------
# `ORDER BY 2` makes SQLite's recursive queue breadth-first, so the first row
# reaching the target is a shortest one. The path string is the visited set;
# it is per-branch, not global, which is why this is the expensive variant.
SP_PATH = """
WITH RECURSIVE sp(node, depth, path) AS (
  SELECT ?1, 0, ',' || ?1 || ','
  UNION ALL
  SELECT e.dst, s.depth + 1, s.path || e.dst || ','
    FROM sp s JOIN edges e ON e.src = s.node
   WHERE s.depth < ?3 AND instr(s.path, ',' || e.dst || ',') = 0
  UNION ALL
  SELECT e.src, s.depth + 1, s.path || e.src || ','
    FROM sp s JOIN edges e ON e.dst = s.node
   WHERE s.depth < ?3 AND instr(s.path, ',' || e.src || ',') = 0
  ORDER BY 2
)
SELECT depth, path FROM sp WHERE node = ?2 LIMIT 1
"""

# One breadth-first level, for the application-side bidirectional BFS. The
# frontier lives in a temp table because a hub's neighbourhood exceeds
# SQLite's 32,766 bound-parameter ceiling.
BFS_LEVEL_TEMP = """
SELECT e.dst FROM frontier f JOIN edges e ON e.src = f.node
UNION
SELECT e.src FROM frontier f JOIN edges e ON e.dst = f.node
"""

# --- hybrid retrieval: FTS5 seeds, then graph expansion, then rank -------
# One statement. The FTS5 match picks seed entities by text; the graph gives
# them neighbours; the score is BM25 decayed by hop distance and edge weight,
# with the existing hard scope partition applied before any of it.
HYBRID = """
WITH seeds AS (
  SELECT f.id AS fact_id, e.src AS node, bm25(facts_fts) AS rank
    FROM facts_fts
    JOIN facts f ON f.id = facts_fts.rowid
    JOIN edges e ON e.fact_id = f.id
   WHERE facts_fts MATCH ?1
     AND f.scope = ?2
   ORDER BY rank
   LIMIT 20
),
reach(node, depth, seed, rank) AS (
  SELECT node, 0, node, rank FROM seeds
  UNION
  SELECT e.dst, r.depth + 1, r.seed, r.rank
    FROM reach r JOIN edges e ON e.src = r.node AND e.scope = ?2
   WHERE r.depth < ?3
     AND (e.valid_to_ms IS NULL)
)
SELECT n.id, n.name, MIN(r.depth) AS hops,
       MIN(r.rank) / (1.0 + MIN(r.depth)) AS score
  FROM reach r JOIN nodes n ON n.id = r.node
 GROUP BY n.id
 ORDER BY score
 LIMIT 25
"""

# --- one label-propagation iteration (community detection) ---------------
# Every node adopts the most common label among its neighbours. Run to
# convergence this is a clustering; the question is whether one pass is
# cheap enough to be a query rather than a nightly job.
LABEL_PROP_STEP = """
WITH nbr AS (
  SELECT e.src AS node, l.label AS label, count(*) AS c
    FROM edges e JOIN labels l ON l.node = e.dst
   GROUP BY e.src, l.label
  UNION ALL
  SELECT e.dst, l.label, count(*)
    FROM edges e JOIN labels l ON l.node = e.src
   GROUP BY e.dst, l.label
),
best AS (
  SELECT node, label, sum(c) AS c,
         row_number() OVER (PARTITION BY node ORDER BY sum(c) DESC, label) AS rn
    FROM nbr GROUP BY node, label
)
UPDATE labels SET label = (
  SELECT b.label FROM best b WHERE b.node = labels.node AND b.rn = 1
)
WHERE EXISTS (SELECT 1 FROM best b WHERE b.node = labels.node AND b.rn = 1)
"""
