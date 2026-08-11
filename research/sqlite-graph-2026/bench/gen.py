#!/usr/bin/env python3
"""Generate a synthetic Jod memory graph in a SQLite file.

The shape is deliberately *not* uniform random. A personal memory graph has
hubs (`reljod`, `jod`, `linear`) and clusters (one per life-domain), and both
decide whether recursive-CTE traversal is viable. So: preferential attachment
inside each of C communities, plus a few percent of cross-community links.

Every edge carries the bitemporal columns the real `facts` table already has,
and every edge has a matching `facts` row indexed by FTS5, so the hybrid
retrieval measurement is over a real FTS5 index rather than a stand-in.

Usage:  gen.py OUT.db --edges 100000
"""

import argparse
import os
import random
import sqlite3
import time

# The candidate schema, verbatim — this is what the report proposes adding to
# `core/src/store.rs`. It is benchmarked as written, not as a simplification.
SCHEMA = """
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE nodes (
  id      INTEGER PRIMARY KEY,
  scope   TEXT NOT NULL DEFAULT 'default',
  kind    TEXT NOT NULL,
  name    TEXT NOT NULL,
  UNIQUE(scope, kind, name)
);

CREATE TABLE edges (
  id             INTEGER PRIMARY KEY,
  src            INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  dst            INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  predicate      TEXT NOT NULL,
  scope          TEXT NOT NULL DEFAULT 'default',
  weight         REAL NOT NULL DEFAULT 1.0,
  fact_id        INTEGER,
  valid_from_ms  INTEGER,
  valid_to_ms    INTEGER,
  recorded_at_ms INTEGER NOT NULL
);

-- Both indexes are covering for a traversal step: the recursive term reads
-- only these columns, so a k-hop never touches the table itself.
CREATE INDEX ix_edges_out ON edges(scope, src, valid_to_ms, valid_from_ms, dst);
CREATE INDEX ix_edges_in  ON edges(scope, dst, valid_to_ms, valid_from_ms, src);
-- Found by measurement, not by design: without this the hybrid query's
-- FTS5-seed join degenerates to a full scan of `edges` per seed row —
-- 533 ms at 10k edges, 0.5 ms with it.
CREATE INDEX ix_edges_fact ON edges(fact_id);

-- The existing facts table, copied from core/src/store.rs so the hybrid
-- measurement runs against the real thing.
CREATE TABLE facts (
  id             INTEGER PRIMARY KEY,
  scope          TEXT NOT NULL DEFAULT 'default',
  subject        TEXT NOT NULL,
  predicate      TEXT NOT NULL,
  object         TEXT NOT NULL,
  origin         TEXT NOT NULL DEFAULT 'agent',
  source         TEXT,
  valid_from     TEXT,
  valid_to       TEXT,
  recorded_at_ms INTEGER NOT NULL,
  state          TEXT NOT NULL DEFAULT 'accepted',
  invalidated_by INTEGER REFERENCES facts(id)
);
CREATE INDEX ix_facts_subject ON facts(scope, subject);

CREATE VIRTUAL TABLE facts_fts USING fts5(
  subject, predicate, object,
  content='facts', content_rowid='id'
);

-- Events, so the concurrency measurement writes what supervisors really write.
CREATE TABLE events (
  id      INTEGER PRIMARY KEY,
  run_id  TEXT NOT NULL,
  seq     INTEGER NOT NULL,
  kind    TEXT NOT NULL,
  at_ms   INTEGER NOT NULL,
  payload TEXT NOT NULL,
  UNIQUE(run_id, seq)
);
"""

KINDS = ["person", "project", "repo", "concept", "run", "doc", "org", "tool"]
PREDICATES = [
    "works_on", "owns", "depends_on", "mentions", "authored", "blocks",
    "part_of", "related_to", "reviewed", "deployed_to", "learned_from",
    "assigned_to",
]
WORDS = [
    "jod", "reljod", "linear", "notion", "supervisor", "harness", "claude",
    "opencode", "agy", "sqlite", "rust", "memory", "graph", "traversal",
    "vps", "deploy", "hetzner", "tui", "ratatui", "api", "webhook", "cron",
    "digest", "fact", "recall", "scope", "origin", "tombstone", "wal",
    "migration", "index", "benchmark", "latency", "cluster", "embedding",
]

YEAR_MS = 365 * 24 * 3600 * 1000
NOW_MS = 1786000000000  # ~ mid-2026, fixed so runs are reproducible


def build(path: str, n_edges: int, seed: int = 7) -> dict:
    rng = random.Random(seed)
    if os.path.exists(path):
        for suffix in ("", "-wal", "-shm"):
            if os.path.exists(path + suffix):
                os.remove(path + suffix)

    # Average degree 16 (out-degree 8) — a memory graph is denser than a
    # citation network and sparser than a social one.
    m = 8
    n_nodes = max(n_edges // m, 64)
    n_comm = max(n_nodes // 2000, 1)

    t0 = time.time()
    db = sqlite3.connect(path, isolation_level=None)
    db.executescript(SCHEMA)

    # --- nodes -----------------------------------------------------------
    nodes = []
    for i in range(1, n_nodes + 1):
        kind = KINDS[i % len(KINDS)]
        name = "%s-%s-%d" % (
            rng.choice(WORDS), rng.choice(WORDS), i)
        nodes.append((i, "default" if i % 4 else "work", kind, name))
    db.executemany("INSERT INTO nodes VALUES (?,?,?,?)", nodes)

    # --- edges: preferential attachment inside communities ---------------
    # Each community owns a contiguous id range. Within it, a new node picks
    # m existing endpoints proportional to their current degree, which is
    # what produces hubs. Then 3% of edges are rewired across communities.
    comm_of = lambda nid: (nid - 1) * n_comm // n_nodes
    bounds = []
    for c in range(n_comm):
        lo = c * n_nodes // n_comm + 1
        hi = (c + 1) * n_nodes // n_comm
        bounds.append((lo, hi))

    edges = []
    facts = []
    eid = 0
    for lo, hi in bounds:
        repeat = []           # degree-proportional draw pool
        for nid in range(lo, min(lo + m, hi) + 1):
            repeat.append(nid)
        for nid in range(lo + m, hi + 1):
            targets = set()
            while len(targets) < min(m, len(repeat)):
                targets.add(rng.choice(repeat))
            for t in targets:
                eid += 1
                edges.append((nid, t))
                repeat.append(nid)
                repeat.append(t)

    # cross-community links, 3%
    n_cross = int(len(edges) * 0.03)
    for _ in range(n_cross):
        a = rng.randint(1, n_nodes)
        b = rng.randint(1, n_nodes)
        if comm_of(a) != comm_of(b):
            edges.append((a, b))

    rng.shuffle(edges)
    edges = edges[:n_edges]

    rows = []
    frows = []
    name_of = {n[0]: n[3] for n in nodes}
    scope_of = {n[0]: n[1] for n in nodes}
    for i, (a, b) in enumerate(edges, start=1):
        pred = PREDICATES[i % len(PREDICATES)]
        scope = scope_of[a]
        recorded = NOW_MS - rng.randint(0, 3 * YEAR_MS)
        vfrom = recorded - rng.randint(0, YEAR_MS)
        # 30% of edges are historical: superseded, no longer valid now.
        vto = None if rng.random() < 0.70 else vfrom + rng.randint(1, YEAR_MS)
        rows.append((i, a, b, pred, scope, rng.random(), i, vfrom, vto, recorded))
        frows.append((
            i, scope, name_of[a], pred,
            "%s %s" % (name_of[b], rng.choice(WORDS)),
            "agent", None, None, None, recorded, "accepted", None,
        ))

    db.execute("BEGIN IMMEDIATE")
    db.executemany("INSERT INTO edges VALUES (?,?,?,?,?,?,?,?,?,?)", rows)
    db.executemany("INSERT INTO facts VALUES (?,?,?,?,?,?,?,?,?,?,?,?)", frows)
    db.commit()

    db.execute("INSERT INTO facts_fts(facts_fts) VALUES ('rebuild')")
    db.commit()
    db.execute("ANALYZE")
    db.commit()

    n_e = db.execute("SELECT count(*) FROM edges").fetchone()[0]
    db.close()

    size = sum(
        os.path.getsize(path + s)
        for s in ("", "-wal") if os.path.exists(path + s)
    )
    return {
        "path": path,
        "nodes": n_nodes,
        "edges": n_e,
        "communities": n_comm,
        "bytes": size,
        "gen_seconds": round(time.time() - t0, 2),
    }


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--edges", type=int, required=True)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    print(build(a.out, a.edges, a.seed))
