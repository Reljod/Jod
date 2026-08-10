#!/usr/bin/env python3
"""Prototype and benchmark for the Jod conversation-tree schema.

Three ways to store a branching conversation are built side by side on the same
synthetic tree and measured on the operation that matters most — materialising
the full transcript at a leaf, which every resume, fork and harness-switch has
to do before it can send anything.

  A  message-DAG      every message carries `parent_id`; walk it with a
                      recursive CTE from the leaf back to the root.
  B  conversation-    a fork is a new `conversations` row carrying
     chain            `parent_id` + `fork_at_seq`; walk the (short) chain of
                      conversations and range-scan each segment's messages.
  C  copy-on-fork     a fork physically copies its ancestors' messages, so a
                      read is one indexed scan and no recursion at all.

Run: python3 prototype.py [--messages 10000] [--forks 40]
"""

import argparse
import json
import os
import random
import sqlite3
import statistics
import time

HERE = os.path.dirname(os.path.abspath(__file__))

# --------------------------------------------------------------------------
# Schema
# --------------------------------------------------------------------------

DDL_A = """
CREATE TABLE a_messages (
  id       INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES a_messages(id),
  role     TEXT NOT NULL,
  kind     TEXT NOT NULL,
  body     TEXT NOT NULL,
  at_ms    INTEGER NOT NULL
);
CREATE INDEX ix_a_parent ON a_messages(parent_id);
CREATE TABLE a_refs (
  name    TEXT PRIMARY KEY,
  leaf_id INTEGER NOT NULL REFERENCES a_messages(id)
);
"""

DDL_B = """
CREATE TABLE b_conversations (
  id            TEXT PRIMARY KEY,
  title         TEXT NOT NULL,
  parent_id     TEXT REFERENCES b_conversations(id),
  fork_at_seq   INTEGER,
  head_seq      INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX ix_b_parent ON b_conversations(parent_id);
CREATE TABLE b_messages (
  id              INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES b_conversations(id),
  seq             INTEGER NOT NULL,
  role            TEXT NOT NULL,
  kind            TEXT NOT NULL,
  body            TEXT NOT NULL,
  at_ms           INTEGER NOT NULL,
  UNIQUE(conversation_id, seq)
);
"""

DDL_C = """
CREATE TABLE c_conversations (
  id            TEXT PRIMARY KEY,
  title         TEXT NOT NULL,
  parent_id     TEXT REFERENCES c_conversations(id),
  fork_at_seq   INTEGER,
  head_seq      INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE TABLE c_messages (
  id              INTEGER PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES c_conversations(id),
  seq             INTEGER NOT NULL,
  role            TEXT NOT NULL,
  kind            TEXT NOT NULL,
  body            TEXT NOT NULL,
  at_ms           INTEGER NOT NULL,
  UNIQUE(conversation_id, seq)
);
"""

# The read every operation depends on: the whole transcript at a leaf, in order.

SQL_A_MATERIALISE = """
WITH RECURSIVE walk(id, parent_id, role, kind, body, depth) AS (
  SELECT id, parent_id, role, kind, body, 0 FROM a_messages WHERE id = ?
  UNION ALL
  SELECT m.id, m.parent_id, m.role, m.kind, m.body, walk.depth + 1
    FROM a_messages m JOIN walk ON m.id = walk.parent_id
)
SELECT id, role, kind, body FROM walk ORDER BY depth DESC;
"""

# `seq` continues across a fork rather than restarting, so a fork's own
# messages can never collide with the range it inherited and one `ORDER BY seq`
# sorts the whole chain. That is the single trick that keeps this query flat.
#
# `MIN(c.fork_at_seq, chain.upto)` is the part that is easy to get wrong and
# silently returns a transcript that never happened. Forking B off A at seq 500
# when A itself forked off the root at seq 1200 must cap the root at 500, not
# at 1200 — otherwise 700 messages the branch never saw appear in the middle of
# it. The cap has to be monotonic all the way up the chain.
SQL_B_MATERIALISE = """
WITH RECURSIVE chain(id, upto) AS (
  SELECT id, head_seq FROM b_conversations WHERE id = ?
  UNION ALL
  SELECT c.parent_id, MIN(c.fork_at_seq, chain.upto)
    FROM b_conversations c JOIN chain ON c.id = chain.id
   WHERE c.parent_id IS NOT NULL
)
SELECT m.id, m.role, m.kind, m.body
  FROM b_messages m JOIN chain ON m.conversation_id = chain.id
 WHERE m.seq <= chain.upto
 ORDER BY m.seq;
"""

SQL_C_MATERIALISE = """
SELECT id, role, kind, body FROM c_messages
 WHERE conversation_id = ? ORDER BY seq;
"""

# Tree presentation: every descendant of a conversation, with its depth.
SQL_B_DESCENDANTS = """
WITH RECURSIVE sub(id, depth) AS (
  SELECT id, 0 FROM b_conversations WHERE id = ?
  UNION ALL
  SELECT c.id, sub.depth + 1
    FROM b_conversations c JOIN sub ON c.parent_id = sub.id
)
SELECT sub.id, sub.depth, c.title, c.head_seq
  FROM sub JOIN b_conversations c ON c.id = sub.id
 ORDER BY sub.depth, c.created_at_ms;
"""

# Listing: every conversation with its own length and its total materialised
# length, newest first — the `jod conversations` screen.
SQL_B_LIST = """
WITH RECURSIVE chain(root, id, upto) AS (
  SELECT id, id, head_seq FROM b_conversations
  UNION ALL
  SELECT chain.root, c.parent_id, MIN(c.fork_at_seq, chain.upto)
    FROM b_conversations c JOIN chain ON c.id = chain.id
   WHERE c.parent_id IS NOT NULL
)
SELECT c.id, c.title, c.head_seq,
       (SELECT COUNT(*) FROM b_messages m JOIN chain ON m.conversation_id = chain.id
         WHERE chain.root = c.id AND m.seq <= chain.upto) AS total
  FROM b_conversations c
 ORDER BY c.created_at_ms DESC;
"""


def body(i, kind):
    """A message body of roughly realistic size for its kind."""
    if kind == "tool_result":
        return json.dumps({"output": "x" * random.randint(200, 3000), "n": i})
    if kind == "tool_call":
        return json.dumps({"tool": "Bash", "input": {"command": "cargo test -q"}, "n": i})
    return json.dumps({"text": "lorem ipsum dolor sit amet " * random.randint(3, 40)})


KINDS = ["text", "thinking", "tool_call", "tool_result", "text"]


def build(conn, n_messages, n_forks, seed=7, spine=False):
    random.seed(seed)
    cur = conn.cursor()
    cur.executescript(DDL_A + DDL_B + DDL_C)

    # ---- shape: one trunk plus `n_forks` branches taken off random points ----
    trunk_len = n_messages // 2
    per_fork = max(1, (n_messages - trunk_len) // max(1, n_forks))

    t0 = time.perf_counter()

    # ---- B: conversation-chain -------------------------------------------
    conn.execute(
        "INSERT INTO b_conversations (id, title, parent_id, fork_at_seq, head_seq, created_at_ms)"
        " VALUES ('cnv_root', 'root', NULL, NULL, ?, 0)",
        (trunk_len,),
    )
    rows = [
        ("cnv_root", i, "user" if i % 6 == 0 else "assistant", KINDS[i % 5], body(i, KINDS[i % 5]), i)
        for i in range(1, trunk_len + 1)
    ]
    conn.executemany(
        "INSERT INTO b_messages (conversation_id, seq, role, kind, body, at_ms)"
        " VALUES (?,?,?,?,?,?)",
        rows,
    )

    leaves_b = []
    parent = "cnv_root"
    parent_head = trunk_len
    for f in range(n_forks):
        cid = f"cnv_{f:04d}"
        # `spine` chains every fork off the previous one, so the chain depth
        # equals the fork count and B's recursion is actually stressed. The
        # default hangs two thirds off the trunk, which is what real use looks
        # like: a handful of alternatives explored from the same point.
        if spine or f % 3 == 2:
            p, p_head = parent, parent_head
        else:
            p, p_head = "cnv_root", trunk_len
        at = random.randint(max(1, p_head - per_fork), p_head) if spine else random.randint(1, p_head)
        head = at + per_fork
        conn.execute(
            "INSERT INTO b_conversations (id, title, parent_id, fork_at_seq, head_seq, created_at_ms)"
            " VALUES (?,?,?,?,?,?)",
            (cid, f"fork {f}", p, at, head, f + 1),
        )
        conn.executemany(
            "INSERT INTO b_messages (conversation_id, seq, role, kind, body, at_ms)"
            " VALUES (?,?,?,?,?,?)",
            [
                (cid, s, "user" if s % 6 == 0 else "assistant", KINDS[s % 5], body(s, KINDS[s % 5]), s)
                for s in range(at + 1, head + 1)
            ],
        )
        leaves_b.append(cid)
        parent, parent_head = cid, head
    build_b = time.perf_counter() - t0

    # ---- A: message-DAG, same shape --------------------------------------
    t0 = time.perf_counter()
    # Parents must be inserted before their children, and `leaves_b` is already
    # in creation order, so every fork's parent is either the root or an
    # earlier entry. Ordering by conversation id instead put `cnv_root` last
    # and silently gave every fork a null parent — a 40-message "transcript"
    # that benchmarked beautifully and meant nothing.
    b_rows = []
    for cid in ["cnv_root"] + leaves_b:
        b_rows.extend(
            conn.execute(
                "SELECT conversation_id, seq, role, kind, body, at_ms FROM b_messages"
                " WHERE conversation_id = ? ORDER BY seq",
                (cid,),
            ).fetchall()
        )
    # Map (conversation, seq) -> a_messages.id, resolving each message's parent
    # through the same fork chain B uses.
    convs = {
        r[0]: (r[1], r[2], r[3])
        for r in conn.execute(
            "SELECT id, parent_id, fork_at_seq, head_seq FROM b_conversations"
        )
    }
    ident = {}

    def a_parent(cid, seq):
        """The id of the message immediately before (cid, seq) in the chain."""
        want = seq - 1
        cur_cid = cid
        while cur_cid is not None:
            if (cur_cid, want) in ident:
                return ident[(cur_cid, want)]
            pid, fork_at, _ = convs[cur_cid]
            if pid is None:
                return None
            # Anything at or below the fork point lives in the ancestor.
            cur_cid = pid
            if want > fork_at:
                want = fork_at
        return None

    next_id = 1
    for cid, seq, role, kind, bd, at_ms in b_rows:
        pid = a_parent(cid, seq)
        conn.execute(
            "INSERT INTO a_messages (id, parent_id, role, kind, body, at_ms) VALUES (?,?,?,?,?,?)",
            (next_id, pid, role, kind, bd, at_ms),
        )
        ident[(cid, seq)] = next_id
        next_id += 1
    for cid in ["cnv_root"] + leaves_b:
        head = convs[cid][2]
        conn.execute(
            "INSERT INTO a_refs (name, leaf_id) VALUES (?,?)", (cid, ident[(cid, head)])
        )
    build_a = time.perf_counter() - t0

    # ---- C: copy-on-fork --------------------------------------------------
    t0 = time.perf_counter()
    for cid in ["cnv_root"] + leaves_b:
        pid, fork_at, head = convs[cid]
        conn.execute(
            "INSERT INTO c_conversations (id, title, parent_id, fork_at_seq, head_seq, created_at_ms)"
            " VALUES (?,?,?,?,?,0)",
            (cid, cid, pid, fork_at, head),
        )
        rows = conn.execute(SQL_B_MATERIALISE, (cid,)).fetchall()
        conn.executemany(
            "INSERT INTO c_messages (conversation_id, seq, role, kind, body, at_ms)"
            " VALUES (?,?,?,?,?,?)",
            [(cid, i + 1, r[1], r[2], r[3], i) for i, r in enumerate(rows)],
        )
    build_c = time.perf_counter() - t0
    conn.commit()
    return leaves_b, {"build_a_s": build_a, "build_b_s": build_b, "build_c_s": build_c}


def bench(conn, sql, args_list, reps=5):
    times = []
    rows = 0
    for _ in range(reps):
        for a in args_list:
            t0 = time.perf_counter()
            rows = len(conn.execute(sql, (a,)).fetchall())
            times.append((time.perf_counter() - t0) * 1000)
    return {
        "p50_ms": round(statistics.median(times), 3),
        "p95_ms": round(sorted(times)[int(len(times) * 0.95) - 1], 3),
        "max_ms": round(max(times), 3),
        "last_rows": rows,
    }


def table_bytes(conn, name):
    try:
        return conn.execute(
            f"SELECT SUM(pgsize) FROM dbstat WHERE name = '{name}'"
        ).fetchone()[0] or 0
    except sqlite3.OperationalError:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--messages", type=int, default=10000)
    ap.add_argument("--forks", type=int, default=40)
    ap.add_argument("--out", default=os.path.join(HERE, "bench.json"))
    ap.add_argument(
        "--spine",
        action="store_true",
        help="chain every fork off the previous one, so chain depth == fork count",
    )
    args = ap.parse_args()

    db = os.path.join(HERE, "scratch.db")
    if os.path.exists(db):
        os.remove(db)
    conn = sqlite3.connect(db)
    conn.executescript(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;"
    )

    leaves, build_times = build(conn, args.messages, args.forks, spine=args.spine)
    conn.execute("ANALYZE")

    a_leaves = [r[0] for r in conn.execute("SELECT leaf_id FROM a_refs")]

    # The three layouts must agree on what the transcript *is*, or the numbers
    # below compare different amounts of work.
    for cid in leaves:
        leaf = conn.execute("SELECT leaf_id FROM a_refs WHERE name = ?", (cid,)).fetchone()[0]
        n_a = len(conn.execute(SQL_A_MATERIALISE, (leaf,)).fetchall())
        n_b = len(conn.execute(SQL_B_MATERIALISE, (cid,)).fetchall())
        n_c = len(conn.execute(SQL_C_MATERIALISE, (cid,)).fetchall())
        assert n_a == n_b == n_c, f"{cid}: A={n_a} B={n_b} C={n_c} disagree"
    counts = {
        "a_messages": conn.execute("SELECT COUNT(*) FROM a_messages").fetchone()[0],
        "b_messages": conn.execute("SELECT COUNT(*) FROM b_messages").fetchone()[0],
        "c_messages": conn.execute("SELECT COUNT(*) FROM c_messages").fetchone()[0],
        "conversations": conn.execute("SELECT COUNT(*) FROM b_conversations").fetchone()[0],
    }

    result = {
        "config": {"messages": args.messages, "forks": args.forks},
        "counts": counts,
        "build_seconds": {k: round(v, 3) for k, v in build_times.items()},
        "materialise": {
            "A_message_dag": bench(conn, SQL_A_MATERIALISE, a_leaves),
            "B_conversation_chain": bench(conn, SQL_B_MATERIALISE, leaves),
            "C_copy_on_fork": bench(conn, SQL_C_MATERIALISE, leaves),
        },
        "tree_queries": {
            "B_descendants": bench(conn, SQL_B_DESCENDANTS, ["cnv_root"], reps=20),
        },
        "bytes": {
            n: table_bytes(conn, n)
            for n in ["a_messages", "b_messages", "c_messages", "b_conversations"]
        },
    }

    t0 = time.perf_counter()
    listed = conn.execute(SQL_B_LIST).fetchall()
    result["tree_queries"]["B_list_all_with_totals"] = {
        "ms": round((time.perf_counter() - t0) * 1000, 3),
        "rows": len(listed),
    }

    # Fork cost: B writes one row; C copies the whole materialised transcript.
    deepest = max(leaves, key=lambda c: len(conn.execute(SQL_B_MATERIALISE, (c,)).fetchall()))
    t0 = time.perf_counter()
    conn.execute(
        "INSERT INTO b_conversations (id, title, parent_id, fork_at_seq, head_seq, created_at_ms)"
        " VALUES ('cnv_forkbench', 'fork', ?, (SELECT head_seq FROM b_conversations WHERE id = ?), "
        "(SELECT head_seq FROM b_conversations WHERE id = ?), 0)",
        (deepest, deepest, deepest),
    )
    fork_b_ms = (time.perf_counter() - t0) * 1000
    conn.execute(
        "INSERT INTO c_conversations (id, title, parent_id, fork_at_seq, head_seq, created_at_ms)"
        " VALUES ('cnv_forkbench', 'fork', ?, 0, 0, 0)",
        (deepest,),
    )
    t0 = time.perf_counter()
    rows = conn.execute(SQL_C_MATERIALISE, (deepest,)).fetchall()
    conn.executemany(
        "INSERT INTO c_messages (conversation_id, seq, role, kind, body, at_ms) VALUES (?,?,?,?,?,?)",
        [("cnv_forkbench", i + 1, r[1], r[2], r[3], i) for i, r in enumerate(rows)],
    )
    fork_c_ms = (time.perf_counter() - t0) * 1000
    result["fork_cost"] = {
        "deepest_transcript_messages": len(rows),
        "B_fork_ms": round(fork_b_ms, 3),
        "C_fork_ms": round(fork_c_ms, 3),
    }
    conn.rollback()

    # How deep the fork chain actually gets — B's recursion depth.
    depths = []
    convs = dict(conn.execute("SELECT id, parent_id FROM b_conversations"))
    for c in leaves:
        d, cur = 0, c
        while convs.get(cur):
            cur = convs[cur]
            d += 1
        depths.append(d)
    result["fork_chain_depth"] = {"max": max(depths), "p50": statistics.median(depths)}

    conn.close()
    with open(args.out, "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
