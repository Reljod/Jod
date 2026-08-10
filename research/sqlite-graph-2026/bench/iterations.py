#!/usr/bin/env python3
"""Ten schema/query designs for the same memory graph, each benchmarked.

The rubric is fixed in ../RUBRIC.md *before* this runs. Each iteration starts
from the same base database (entities, a bare `relations` heap, facts, FTS5),
applies exactly one design change, and answers the same six query classes.

Iterations that make things worse are kept and reported — a regression is a
result, and iterations 1, 2 and 8 exist to produce one.

Usage:
  iterations.py BASE_SOURCE.db --out ../out/iterations-100k.json
"""

import argparse
import json
import os
import random
import shutil
import sqlite3
import statistics
import sys
import tempfile
import threading
import time

NOW_MS = 1786000000000
ASOF_MS = NOW_MS - 400 * 24 * 3600 * 1000
CAP_S = 10.0

# --------------------------------------------------------------------------
# Declared (non-measured) rubric scores. Each is stated here, next to the
# design it judges, so a reader can disagree with one number without having to
# re-derive the whole table. Order: build, one_file, multiproc, maint, simple.
# --------------------------------------------------------------------------
DECLARED = {
    #                    build one_file multiproc maint simple
    "I1_noindex_unionall": (5, 5, 5, 5, 5),
    "I2_noindex_union":    (5, 5, 5, 5, 5),
    "I3_index_src":        (5, 5, 5, 5, 5),
    "I4_covering_src_dst": (5, 5, 5, 5, 5),
    "I5_plus_reverse":     (5, 5, 5, 5, 5),
    "I6_both_direction_rows": (5, 5, 5, 5, 3),
    "I7_scope_temporal_covering": (5, 5, 5, 5, 5),
    "I8_temporal_postfilter": (5, 5, 5, 5, 5),
    "I9_2hop_closure":     (5, 5, 4, 5, 1),
    "I10_json_adjacency":  (5, 5, 4, 5, 2),
}

DECLARED_WHY = {
    "I6_both_direction_rows":
        "simplicity 3: every edge exists as two rows, so every write and "
        "every supersession has to touch both, and 'is this row the edge or "
        "its mirror' becomes a thing callers can get wrong.",
    "I9_2hop_closure":
        "simplicity 1 and multiproc 4: a derived table that must be "
        "invalidated on every edge write. Under concurrent writers the "
        "closure is stale between the write and the rebuild, so a reader can "
        "see a neighbour that no longer exists.",
    "I10_json_adjacency":
        "simplicity 2 and multiproc 4: an adjacency blob is a read-modify-"
        "write on every edge insert — exactly the pattern the store's own "
        "research says never to use for contended state.",
}
# Everything scored 5,5,5,5,5 is plain SQLite core: nothing to build, one
# file, WAL, no third-party code, one table plus indexes.

BASE_SCHEMA = """
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE entities (
  id   INTEGER PRIMARY KEY,
  scope TEXT NOT NULL DEFAULT 'default',
  kind  TEXT NOT NULL,
  name  TEXT NOT NULL
);

-- A bare heap. Every iteration adds its own structure on top of exactly this.
CREATE TABLE relations (
  id             INTEGER PRIMARY KEY,
  scope          TEXT NOT NULL DEFAULT 'default',
  src            INTEGER NOT NULL,
  dst            INTEGER NOT NULL,
  predicate      TEXT NOT NULL,
  weight         REAL NOT NULL DEFAULT 1.0,
  fact_id        INTEGER NOT NULL,
  valid_from_ms  INTEGER,
  valid_to_ms    INTEGER,
  recorded_at_ms INTEGER NOT NULL
);
"""


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))],
                 3)


def connect(path):
    db = sqlite3.connect(path, isolation_level=None)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA busy_timeout = 5000")
    db.execute("PRAGMA synchronous = NORMAL")
    return db


def timed(db, fn, samples, cap_s=CAP_S):
    lat, timeouts = [], 0
    for s in samples:
        t = threading.Timer(cap_s, db.interrupt)
        t.start()
        t0 = time.perf_counter()
        try:
            fn(s)
            lat.append((time.perf_counter() - t0) * 1000.0)
        except sqlite3.Error as exc:
            if "interrupted" not in str(exc):
                raise
            timeouts += 1
            lat.append(cap_s * 1000.0)
        finally:
            t.cancel()
    out = {"n": len(lat), "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95)}
    if timeouts:
        out["timeouts"] = timeouts
    return out


def build_base(src, dest):
    """One canonical base: entities, a bare `relations` heap, facts + FTS5."""
    if os.path.exists(dest):
        os.remove(dest)
    db = connect(dest)
    db.executescript(BASE_SCHEMA)
    db.execute("ATTACH DATABASE ? AS s", (src,))
    db.execute("BEGIN IMMEDIATE")
    db.execute("INSERT INTO entities SELECT id, scope, kind, name FROM s.nodes")
    db.execute(
        "INSERT INTO relations (id, scope, src, dst, predicate, weight, "
        "  fact_id, valid_from_ms, valid_to_ms, recorded_at_ms) "
        "SELECT id, scope, src, dst, predicate, weight, fact_id, "
        "       valid_from_ms, valid_to_ms, recorded_at_ms FROM s.edges")
    db.execute("COMMIT")
    # facts + FTS5, so Q6 runs against a real index rather than a stand-in.
    db.execute("CREATE TABLE facts AS SELECT * FROM s.facts")
    db.execute("CREATE INDEX ix_facts_subject ON facts(scope, subject)")
    db.execute("CREATE VIRTUAL TABLE facts_fts USING fts5("
               "subject, predicate, object, content='facts', "
               "content_rowid='id')")
    db.execute("INSERT INTO facts_fts(facts_fts) VALUES ('rebuild')")
    db.execute("DETACH DATABASE s")
    db.execute("ANALYZE")
    db.close()


# --------------------------------------------------------------------------
# The ten designs. Each is (setup SQL or callable, dict of query builders).
# A query class absent from the dict is one the design cannot answer.
# --------------------------------------------------------------------------

# Shared query text, parameterised by the relation source each design offers.
def khop_directed(rel="relations", extra=""):
    return """
    WITH RECURSIVE reach(node, depth) AS (
      SELECT ?1, 0
      UNION
      SELECT e.dst, r.depth + 1 FROM reach r JOIN %s e ON e.src = r.node %s
       WHERE r.depth < ?2
    )
    SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
    """ % (rel, extra)


KHOP_DIRECTED_UNIONALL = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION ALL
  SELECT e.dst, r.depth + 1 FROM reach r JOIN relations e ON e.src = r.node
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

KHOP_UNDIRECTED_TWO_TERMS = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1 FROM reach r JOIN relations e ON e.src = r.node
   WHERE r.depth < ?2
  UNION
  SELECT e.src, r.depth + 1 FROM reach r JOIN relations e ON e.dst = r.node
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

KHOP_UNDIRECTED_OR = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT CASE WHEN e.src = r.node THEN e.dst ELSE e.src END, r.depth + 1
    FROM reach r JOIN relations e ON (e.src = r.node OR e.dst = r.node)
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

KHOP_UNDIRECTED_MIRRORED = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT u.b, r.depth + 1 FROM reach r JOIN relations_u u ON u.a = r.node
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

# Validity pushed *into* the recursive step: an edge that was not true then is
# never expanded.
KHOP_ASOF_PUSHDOWN = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN relations e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
     AND (e.valid_to_ms IS NULL OR e.valid_to_ms > ?4)
     AND (e.valid_from_ms IS NULL OR e.valid_from_ms <= ?4)
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

# The same answer, filtered *after* the traversal. Iteration 8 exists to show
# what this costs.
KHOP_ASOF_POSTFILTER = """
WITH RECURSIVE reach(node, depth, rel) AS (
  SELECT ?1, 0, NULL
  UNION
  SELECT e.dst, r.depth + 1, e.id
    FROM reach r JOIN relations e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
)
SELECT reach.node, MIN(reach.depth) FROM reach
  LEFT JOIN relations e ON e.id = reach.rel
 WHERE reach.node <> ?1
   AND (reach.rel IS NULL
        OR ((e.valid_to_ms IS NULL OR e.valid_to_ms > ?4)
            AND (e.valid_from_ms IS NULL OR e.valid_from_ms <= ?4)))
 GROUP BY reach.node
"""

SP_DISTANCE = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1 FROM reach r JOIN relations e ON e.src = r.node
   WHERE r.depth < ?3
  UNION
  SELECT e.src, r.depth + 1 FROM reach r JOIN relations e ON e.dst = r.node
   WHERE r.depth < ?3
)
SELECT MIN(depth) FROM reach WHERE node = ?2
"""

HYBRID = """
WITH seeds AS (
  SELECT f.id AS fact_id, r.src AS node, bm25(facts_fts) AS rank
    FROM facts_fts
    JOIN facts f     ON f.id = facts_fts.rowid
    JOIN relations r ON r.fact_id = f.id
   WHERE facts_fts MATCH ?1 AND f.scope = ?2
   ORDER BY rank LIMIT 20
),
reach(node, depth, rank) AS (
  SELECT node, 0, rank FROM seeds
  UNION
  SELECT r.dst, x.depth + 1, x.rank
    FROM reach x JOIN relations r ON r.src = x.node AND r.scope = ?2
   WHERE x.depth < ?3 AND r.valid_to_ms IS NULL
)
SELECT e.id, e.name, MIN(reach.depth),
       MIN(reach.rank) / (1.0 + MIN(reach.depth)) AS score
  FROM reach JOIN entities e ON e.id = reach.node
 GROUP BY e.id ORDER BY score LIMIT 25
"""

KHOP_CLOSURE_3 = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT c.dst, 2 FROM closure2 c WHERE c.src = ?1
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN relations e ON e.src = r.node
   WHERE r.depth >= 2 AND r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""

KHOP_JSON_ADJ = """
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT CAST(j.value AS INTEGER), r.depth + 1
    FROM reach r JOIN adj a ON a.node = r.node,
         json_each(a.out_json) j
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) FROM reach WHERE node <> ?1 GROUP BY node
"""


def design_setup(name, db):
    """Apply exactly one design change to the base. Returns build seconds."""
    t0 = time.time()
    if name == "I1_noindex_unionall":
        pass                                    # nothing at all
    elif name == "I2_noindex_union":
        pass                                    # same storage, better query
    elif name == "I3_index_src":
        db.execute("CREATE INDEX ix_rel_src ON relations(src)")
    elif name == "I4_covering_src_dst":
        db.execute("CREATE INDEX ix_rel_out ON relations(src, dst)")
    elif name == "I5_plus_reverse":
        db.execute("CREATE INDEX ix_rel_out ON relations(src, dst)")
        db.execute("CREATE INDEX ix_rel_in  ON relations(dst, src)")
    elif name == "I6_both_direction_rows":
        db.execute("CREATE TABLE relations_u(a INTEGER, b INTEGER, "
                   "rel INTEGER)")
        db.execute("BEGIN IMMEDIATE")
        db.execute("INSERT INTO relations_u SELECT src, dst, id FROM relations")
        db.execute("INSERT INTO relations_u SELECT dst, src, id FROM relations")
        db.execute("COMMIT")
        db.execute("CREATE INDEX ix_relu ON relations_u(a, b)")
        db.execute("CREATE INDEX ix_rel_out ON relations(src, dst)")
    elif name in ("I7_scope_temporal_covering", "I8_temporal_postfilter"):
        db.execute("CREATE INDEX ix_rel_out ON relations"
                   "(scope, src, valid_to_ms, valid_from_ms, dst)")
        db.execute("CREATE INDEX ix_rel_in  ON relations"
                   "(scope, dst, valid_to_ms, valid_from_ms, src)")
        db.execute("CREATE INDEX ix_rel_fact ON relations(fact_id)")
    elif name == "I9_2hop_closure":
        db.execute("CREATE INDEX ix_rel_out ON relations(src, dst)")
        db.execute("CREATE INDEX ix_rel_in  ON relations(dst, src)")
        db.execute("CREATE TABLE closure2(src INTEGER, dst INTEGER)")
        db.execute("BEGIN IMMEDIATE")
        db.execute("INSERT INTO closure2 SELECT DISTINCT a.src, b.dst "
                   "FROM relations a JOIN relations b ON b.src = a.dst")
        db.execute("COMMIT")
        db.execute("CREATE INDEX ix_c2 ON closure2(src, dst)")
    elif name == "I10_json_adjacency":
        db.execute("CREATE TABLE adj(node INTEGER PRIMARY KEY, "
                   "out_json TEXT NOT NULL)")
        db.execute("BEGIN IMMEDIATE")
        db.execute("INSERT INTO adj SELECT src, json_group_array(dst) "
                   "FROM relations GROUP BY src")
        db.execute("COMMIT")
    else:
        raise ValueError(name)
    db.execute("ANALYZE")
    return round(time.time() - t0, 2)


def queries_for(name):
    """Which of the six classes this design can answer, and with what SQL."""
    q = {}
    if name == "I1_noindex_unionall":
        q["Q1"] = ("khop", KHOP_DIRECTED_UNIONALL, 1)
        q["Q2"] = ("khop", KHOP_DIRECTED_UNIONALL, 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_OR, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name == "I2_noindex_union":
        q["Q1"] = ("khop", khop_directed(), 1)
        q["Q2"] = ("khop", khop_directed(), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_OR, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name in ("I3_index_src", "I4_covering_src_dst"):
        q["Q1"] = ("khop", khop_directed(), 1)
        q["Q2"] = ("khop", khop_directed(), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_OR, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name == "I5_plus_reverse":
        q["Q1"] = ("khop", khop_directed(), 1)
        q["Q2"] = ("khop", khop_directed(), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_TWO_TERMS, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name == "I6_both_direction_rows":
        q["Q1"] = ("khop", khop_directed(), 1)
        q["Q2"] = ("khop", khop_directed(), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_MIRRORED, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name == "I7_scope_temporal_covering":
        q["Q1"] = ("khop", khop_directed(extra="AND e.scope = 'default'"), 1)
        q["Q2"] = ("khop", khop_directed(extra="AND e.scope = 'default'"), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_TWO_TERMS, 3)
        q["Q4"] = ("asof", KHOP_ASOF_PUSHDOWN, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
        q["Q6"] = ("hybrid", HYBRID)
    elif name == "I8_temporal_postfilter":
        q["Q1"] = ("khop", khop_directed(extra="AND e.scope = 'default'"), 1)
        q["Q2"] = ("khop", khop_directed(extra="AND e.scope = 'default'"), 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_TWO_TERMS, 3)
        q["Q4"] = ("asof", KHOP_ASOF_POSTFILTER, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
        q["Q6"] = ("hybrid", HYBRID)
    elif name == "I9_2hop_closure":
        q["Q1"] = ("khop", khop_directed(), 1)
        q["Q2"] = ("khop", KHOP_CLOSURE_3, 3)
        q["Q3"] = ("khop", KHOP_UNDIRECTED_TWO_TERMS, 3)
        q["Q5"] = ("sp", SP_DISTANCE)
    elif name == "I10_json_adjacency":
        q["Q1"] = ("khop", KHOP_JSON_ADJ, 1)
        q["Q2"] = ("khop", KHOP_JSON_ADJ, 3)
        # No reverse adjacency and no per-edge columns: undirected traversal,
        # bitemporal filtering and the FTS5 seed join are all unavailable
        # without a second blob and a second denormalisation.
    return q


ORDER = ["I1_noindex_unionall", "I2_noindex_union", "I3_index_src",
         "I4_covering_src_dst", "I5_plus_reverse", "I6_both_direction_rows",
         "I7_scope_temporal_covering", "I8_temporal_postfilter",
         "I9_2hop_closure", "I10_json_adjacency"]

CHANGED = {
    "I1_noindex_unionall":
        "Baseline. A bare heap table, and a recursive CTE with `UNION ALL` "
        "and a depth cap but no deduplication.",
    "I2_noindex_union":
        "Same storage. `UNION` instead of `UNION ALL`, so the recursion "
        "deduplicates `(node, depth)` and a cycle terminates.",
    "I3_index_src":
        "Add `relations(src)`. The recursive step can seek instead of scan, "
        "but still reads the table for `dst`.",
    "I4_covering_src_dst":
        "Widen it to `relations(src, dst)`. The step is now covering: the "
        "table is never touched.",
    "I5_plus_reverse":
        "Add `relations(dst, src)` and rewrite undirected traversal as two "
        "recursive terms, one per index, replacing the `OR` join.",
    "I6_both_direction_rows":
        "Materialise every edge twice in `relations_u(a, b)` so undirected "
        "traversal is a single recursive term over one index.",
    "I7_scope_temporal_covering":
        "Scope-first covering indexes "
        "`(scope, src, valid_to_ms, valid_from_ms, dst)` and the mirror, plus "
        "`relations(fact_id)`. Validity is pushed into the recursive step.",
    "I8_temporal_postfilter":
        "Identical storage to I7. The validity predicate is applied *after* "
        "the traversal instead of inside it — the regression probe.",
    "I9_2hop_closure":
        "Precompute every directed 2-hop pair into `closure2`, and answer "
        "3-hop as closure ⋈ relations.",
    "I10_json_adjacency":
        "Denormalise: one JSON array of out-neighbours per entity, expanded "
        "with `json_each`.",
}


def score(res):
    """Rubric from ../RUBRIC.md. Two computed criteria, five declared."""
    core = [res["queries"][k]["p95_ms"] for k in ("Q2", "Q3", "Q4")
            if k in res["queries"] and res["queries"][k]["p95_ms"] is not None]
    worst = max(core) if core else None
    if worst is None:
        lat = 0
    elif worst <= 1:
        lat = 5
    elif worst <= 5:
        lat = 4
    elif worst <= 25:
        lat = 3
    elif worst <= 100:
        lat = 2
    elif worst <= 1000:
        lat = 1
    else:
        lat = 0
    power = round(5.0 * len(res["queries"]) / 6.0, 2)
    build, one_file, multiproc, maint, simple = DECLARED[res["name"]]
    total = (0.25 * lat + 0.20 * power + 0.15 * build + 0.10 * one_file
             + 0.10 * multiproc + 0.10 * maint + 0.10 * simple)
    return {
        "worst_core_p95_ms": worst,
        "latency": lat, "query_power": power, "build": build,
        "one_file": one_file, "multiproc": multiproc, "maint": maint,
        "simple": simple, "total": round(total, 3),
    }


def run_one(name, base, work, seeds, pairs, terms, slow):
    path = os.path.join(work, name + ".db")
    shutil.copy(base, path)
    db = connect(path)
    res = {"name": name, "changed": CHANGED[name]}
    res["build_seconds"] = design_setup(name, db)

    qs = queries_for(name)
    out = {}
    for key in ("Q1", "Q2", "Q3", "Q4", "Q5", "Q6"):
        if key not in qs:
            out_key = None
            continue
        spec = qs[key]
        # Fewer samples, and a shorter ceiling, for the designs already known
        # to be pathological: five timeouts establish "unusable" as well as
        # sixty do, and the sweep has to finish.
        heavy = slow and key in ("Q2", "Q3", "Q5")
        n = 5 if heavy else 60
        cap = 5.0 if heavy else CAP_S
        if spec[0] == "khop":
            _, sql, k = spec
            out[key] = timed(db, lambda s, sql=sql, k=k: db.execute(
                sql, (s, k)).fetchall(), seeds[:n], cap)
        elif spec[0] == "asof":
            _, sql, k = spec
            out[key] = timed(db, lambda s, sql=sql, k=k: db.execute(
                sql, (s, k, "default", ASOF_MS)).fetchall(), seeds[:n], cap)
        elif spec[0] == "sp":
            _, sql = spec
            out[key] = timed(db, lambda p, sql=sql: db.execute(
                sql, (p[0], p[1], 5)).fetchall(), pairs[:min(n, 20)], cap)
        elif spec[0] == "hybrid":
            _, sql = spec
            out[key] = timed(db, lambda t, sql=sql: db.execute(
                sql, (t, "default", 2)).fetchall(), terms, cap)
    res["queries"] = out
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    res["bytes"] = os.path.getsize(path)
    res["score"] = score(res)
    if name in DECLARED_WHY:
        res["declared_why"] = DECLARED_WHY[name]
    os.remove(path)
    for s in ("-wal", "-shm"):
        if os.path.exists(path + s):
            os.remove(path + s)
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("source", help="a graph generated by gen.py")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="jod-iter-")
    base = os.path.join(work, "base.db")
    print("building base…", flush=True)
    build_base(a.source, base)

    db = connect(base)
    ids = [r[0] for r in db.execute("SELECT id FROM entities").fetchall()]
    n_edges = db.execute("SELECT count(*) FROM relations").fetchone()[0]
    db.close()
    rng = random.Random(11)
    seeds = [rng.choice(ids) for _ in range(60)]
    pairs = [(rng.choice(ids), rng.choice(ids)) for _ in range(20)]
    terms = ["jod", "memory", "graph", "sqlite", "deploy", "harness",
             "traversal", "linear", "vps", "index"] * 3

    results = {"source": a.source, "edges": n_edges, "entities": len(ids),
               "sqlite_version": sqlite3.sqlite_version,
               "base_bytes": os.path.getsize(base), "iterations": []}
    for i, name in enumerate(ORDER, start=1):
        # Every design up to I4 answers Q3 through the `OR` join, which no
        # index can serve — at 100k that is a full scan per recursion step,
        # so those get five samples and a five-second ceiling.
        slow = name in ("I1_noindex_unionall", "I2_noindex_union",
                        "I3_index_src", "I4_covering_src_dst")
        print("== %d/%d %s" % (i, len(ORDER), name), flush=True)
        r = run_one(name, base, work, seeds, pairs, terms, slow)
        r["iteration"] = i
        results["iterations"].append(r)
        print("   score %.3f  worst core p95 %s ms  %.1f MB"
              % (r["score"]["total"], r["score"]["worst_core_p95_ms"],
                 r["bytes"] / 1e6), flush=True)
        with open(a.out, "w") as fh:
            json.dump(results, fh, indent=2)

    results["ranking"] = sorted(
        [{"iteration": r["iteration"], "name": r["name"],
          "total": r["score"]["total"]} for r in results["iterations"]],
        key=lambda x: -x["total"])
    with open(a.out, "w") as fh:
        json.dump(results, fh, indent=2)
    shutil.rmtree(work, ignore_errors=True)
    print("\nwinner:", results["ranking"][0], flush=True)


if __name__ == "__main__":
    main()
