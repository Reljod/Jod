# Hypotheses — written before the benchmark was run

Committed 2026-08-09, before any measurement. Each states a falsifiable
prediction with a number. The point of writing them first is that a hypothesis
adjusted after seeing the data explains nothing.

Result column filled in afterwards by `scripts/report.py`; the prose verdicts
live in [`REPORT.md`](REPORT.md).

---

**H1 — The single-writer limit is not the bottleneck at agent scale.**
Agent write transactions are tiny (one row, a few hundred bytes). SQLite in WAL
mode serializes them, but serialization at microsecond cost still clears far
more throughput than a handful of agents can generate.
*Prediction:* SQLite WAL sustains **> 2,000 append txn/s** across 8 writer
processes with **p99 < 50 ms** and a **0% error rate**.

**H2 — SQLite's reputation for "database is locked" is a configuration bug, not
an engine property.**
*Prediction:* with no `busy_timeout` and deferred transactions, the error rate
at 8 writers exceeds **5%**; with `busy_timeout=5000` and `BEGIN IMMEDIATE`,
the same workload errors at **~0%**, at a cost of higher tail latency.

**H3 — Postgres wins on concurrency, loses on latency floor.**
In-process SQLite has no IPC; Postgres pays a socket round-trip per statement.
*Prediction:* Postgres shows **higher p50 latency** than SQLite on single
appends, but **degrades more gracefully** as writer count rises, overtaking
SQLite on total throughput somewhere in the 8–32 writer range.

**H4 — Contended read-modify-write is the discriminator, not append rate.**
Everything can append. Almost nothing gets "claim this task exactly once"
right without being told how.
*Prediction:* SQLite (`BEGIN IMMEDIATE`), Postgres (`SELECT … FOR UPDATE`) and
Redis (`INCR`) all finish with **zero lost updates**; the naive variant of each
— deferred txn, bare `SELECT` then `UPDATE` at READ COMMITTED, `GET` then `SET`
— **loses updates**, and Redis's naive variant loses the most because it is the
fastest and therefore interleaves most often.

**H5 — Optimistic-commit vector stores fall over as multi-writer state stores.**
LanceDB commits a new manifest per write with bounded retries.
*Prediction:* LanceDB at 8 concurrent writer processes shows a **> 20% error
rate** or **> 10x throughput collapse** versus a single writer.

**H6 — DuckDB fails categorically, not gradually.**
*Prediction:* a second process cannot open the same database file read-write at
all. This is a hard disqualification, not a low score.

**H7 — At Jod's actual scale, vector search choice barely matters.**
Jod's brain is thousands of notes, not tens of millions.
*Prediction:* at 30k × 384-dim, **every** engine tested answers top-10 in
**p95 < 50 ms**, and the spread between the fastest and slowest is **smaller
than the cost of running a second daemon**. Brute-force sqlite-vec stays within
**5x** of HNSW engines while giving **100% recall**.

**H8 — Reads stay fast under write load in MVCC/WAL engines, and only there.**
*Prediction:* SQLite (WAL) and Postgres (MVCC) show **< 2x** read-latency
inflation when 4 writers hammer the same table; engines that lock or rebuild
indexes synchronously show more.

---

## What would change the recommendation

Stated in advance so the conclusion cannot quietly move:

- If SQLite's append throughput came in **under ~300 txn/s** or its error rate
  above 1% with correct configuration, the embedded option is dead and the
  answer is Postgres.
- If Postgres p50 came in **above 5 ms** for a single append, the "just use
  Postgres" advice is too slow for a hot agent loop and needs a write buffer.
- If any engine **lost updates even when used correctly**, it is disqualified as
  a state store regardless of speed.
