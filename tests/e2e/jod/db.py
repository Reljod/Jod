#!/usr/bin/env python3
"""Read-only peek into a Jod store.

There is no `sqlite3` binary on this box, and some claims — "forget destroys
every version", "forget cascades to edges" — are only provable by looking at the
rows rather than at what the CLI prints. Usage:

    db.py <path-to-jod.db> "<sql>"
"""
import sys
import sqlite3

db, sql = sys.argv[1], sys.argv[2]
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
cur = con.execute(sql)
cols = [d[0] for d in cur.description] if cur.description else []
if cols:
    print(" | ".join(cols))
    print("-+-".join("-" * len(c) for c in cols))
rows = cur.fetchall()
for r in rows:
    print(" | ".join("" if v is None else str(v) for v in r))
print(f"({len(rows)} rows)")
