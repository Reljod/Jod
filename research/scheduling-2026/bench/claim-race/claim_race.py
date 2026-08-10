#!/usr/bin/env python3
"""Can N jod processes share one schedule table without double-firing it?

Jod's charter says a run is expensive and externally visible: a scheduled
"post the weekly summary" that fires twice has posted twice. So the claim has
to be provably exclusive, not probably exclusive.

This harness races real OS processes — not threads, which would share a
connection and a GIL and prove nothing — against one SQLite file, and checks
one invariant: **no two claims ever name the same scheduled instant.**

Three arms, so the recommendation is measured against its alternatives:

  cas       BEGIN IMMEDIATE + compare-and-swap on next_fire_at_ms  (recommended)
  immediate BEGIN IMMEDIATE, then SELECT and UPDATE inside the txn (safe, slower)
  naive     SELECT outside a txn, then UPDATE                      (control)

Plus a fourth scenario: a claimant that dies holding the lease, to check that
exactly one other process takes it over once the lease expires.

Usage: python3 claim_race.py [--workers N] [--seconds S]
"""

from __future__ import annotations

import argparse
import multiprocessing as mp
import os
import sqlite3
import sys
import time

PERIOD_MS = 1  # a schedule that is due again the instant it is claimed

SCHEMA = """
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS schedules (
  id              TEXT PRIMARY KEY,
  state           TEXT NOT NULL DEFAULT 'active',
  next_fire_at_ms INTEGER,
  lease_owner     TEXT,
  lease_until_ms  INTEGER,
  -- Which instant the live lease is for. Without it a reaper can clear a dead
  -- claimant's lease but cannot say which fire was lost, which is the
  -- difference between a recorded failure and a silent one.
  lease_fired_for INTEGER,
  fires           INTEGER NOT NULL DEFAULT 0
);

-- Every successful claim, so duplicates are detectable after the fact rather
-- than only observable live.
CREATE TABLE IF NOT EXISTS claims (
  id           INTEGER PRIMARY KEY,
  schedule_id  TEXT NOT NULL,
  fired_for_ms INTEGER NOT NULL,
  worker       TEXT NOT NULL,
  arm          TEXT NOT NULL
);

-- A claim whose worker lived long enough to finish the run.
CREATE TABLE IF NOT EXISTS completions (
  schedule_id  TEXT NOT NULL,
  fired_for_ms INTEGER NOT NULL,
  worker       TEXT NOT NULL,
  PRIMARY KEY (schedule_id, fired_for_ms)
);

-- A claim whose worker died holding the lease, recorded by whichever reaper
-- won the sweep. Every claim must end up in exactly one of these two tables:
-- that is what "a failed run never looks like a successful one" means when
-- the process that would have reported it is gone.
CREATE TABLE IF NOT EXISTS reaps (
  schedule_id  TEXT NOT NULL,
  fired_for_ms INTEGER NOT NULL,
  dead_worker  TEXT NOT NULL,
  reaper       TEXT NOT NULL,
  PRIMARY KEY (schedule_id, fired_for_ms)
);
"""


def connect(db: str) -> sqlite3.Connection:
    # isolation_level=None: python's sqlite3 otherwise opens its own implicit
    # DEFERRED transactions, which is the very failure mode being tested.
    conn = sqlite3.connect(db, isolation_level=None, timeout=5.0)
    conn.execute("PRAGMA busy_timeout = 5000")
    return conn


def now_ms() -> int:
    return int(time.time() * 1000)


# ---- the three claim implementations ------------------------------------


def claim_cas(conn, sched_id, worker, lease_ms):
    """The recommended protocol.

    Read outside the transaction — cheap, and allowed to be stale. The write
    then re-asserts everything the read assumed, so a stale read cannot win.
    """
    row = conn.execute(
        "SELECT next_fire_at_ms FROM schedules "
        " WHERE id = ? AND state = 'active' AND next_fire_at_ms <= ? "
        "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
        (sched_id, now_ms(), now_ms()),
    ).fetchone()
    if row is None:
        return None
    seen = row[0]
    # Pure function of the pattern and the instant claimed — computed outside
    # the transaction on purpose, because the CAS pins the input it used.
    following = seen + PERIOD_MS
    now = now_ms()

    conn.execute("BEGIN IMMEDIATE")
    try:
        cur = conn.execute(
            "UPDATE schedules "
            "   SET next_fire_at_ms = ?, lease_owner = ?, lease_until_ms = ?, "
            "       lease_fired_for = ?, fires = fires + 1 "
            " WHERE id = ? "
            "   AND state = 'active' "
            "   AND next_fire_at_ms = ? "          # <- the compare-and-swap
            "   AND next_fire_at_ms <= ? "
            "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
            (following, worker, now + lease_ms, seen, sched_id, seen, now, now),
        )
        won = cur.rowcount == 1
        if won:
            conn.execute(
                "INSERT INTO claims (schedule_id, fired_for_ms, worker, arm) "
                "VALUES (?, ?, ?, 'cas')",
                (sched_id, seen, worker),
            )
        conn.execute("COMMIT")
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    return seen if won else None


def claim_immediate(conn, sched_id, worker, lease_ms):
    """Correct, but holds the write lock across the read."""
    conn.execute("BEGIN IMMEDIATE")
    try:
        now = now_ms()
        row = conn.execute(
            "SELECT next_fire_at_ms FROM schedules "
            " WHERE id = ? AND state = 'active' AND next_fire_at_ms <= ? "
            "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
            (sched_id, now, now),
        ).fetchone()
        if row is None:
            conn.execute("COMMIT")
            return None
        seen = row[0]
        conn.execute(
            "UPDATE schedules SET next_fire_at_ms = ?, lease_owner = ?, "
            "       lease_until_ms = ?, fires = fires + 1 WHERE id = ?",
            (seen + PERIOD_MS, worker, now + lease_ms, sched_id),
        )
        conn.execute(
            "INSERT INTO claims (schedule_id, fired_for_ms, worker, arm) "
            "VALUES (?, ?, ?, 'immediate')",
            (sched_id, seen, worker),
        )
        conn.execute("COMMIT")
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    return seen


def claim_naive(conn, sched_id, worker, lease_ms):
    """What an agent writes without thinking: decide, then write."""
    now = now_ms()
    row = conn.execute(
        "SELECT next_fire_at_ms FROM schedules "
        " WHERE id = ? AND state = 'active' AND next_fire_at_ms <= ? "
        "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
        (sched_id, now, now),
    ).fetchone()
    if row is None:
        return None
    seen = row[0]
    conn.execute(
        "UPDATE schedules SET next_fire_at_ms = ?, lease_owner = ?, "
        "       lease_until_ms = ?, fires = fires + 1 WHERE id = ?",
        (seen + PERIOD_MS, worker, now + lease_ms, sched_id),
    )
    conn.execute(
        "INSERT INTO claims (schedule_id, fired_for_ms, worker, arm) "
        "VALUES (?, ?, ?, 'naive')",
        (sched_id, seen, worker),
    )
    return seen


ARMS = {"cas": claim_cas, "immediate": claim_immediate, "naive": claim_naive}


def release(conn, sched_id, worker):
    """The run finished: drop the lease so the next fire may be claimed.

    Guarded by `lease_owner`, so a process whose lease already expired and was
    taken over by someone else cannot release a lease it no longer holds.
    """
    conn.execute("BEGIN IMMEDIATE")
    conn.execute(
        "UPDATE schedules SET lease_owner = NULL, lease_until_ms = NULL, "
        "       lease_fired_for = NULL "
        " WHERE id = ? AND lease_owner = ?",
        (sched_id, worker),
    )
    conn.execute("COMMIT")


# ---- the race -----------------------------------------------------------


def worker_proc(db, arm, worker, deadline, sched_ids, out):
    conn = connect(db)
    claim = ARMS[arm]
    wins = attempts = busy = 0
    while time.time() < deadline:
        for sid in sched_ids:
            attempts += 1
            try:
                if claim(conn, sid, worker, lease_ms=30_000) is not None:
                    wins += 1
                    # The "run" is instantaneous here; releasing makes the
                    # schedule due again so the race can repeat thousands of
                    # times rather than once per lease.
                    release(conn, sid, worker)
            except sqlite3.OperationalError:
                busy += 1
    conn.close()
    out.put((worker, wins, attempts, busy))


def run_arm(db_dir, arm, workers, seconds, schedules=4):
    db = os.path.join(db_dir, f"race-{arm}.db")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db + suffix)
        except FileNotFoundError:
            pass

    conn = connect(db)
    conn.executescript(SCHEMA)
    # Ten minutes of backlog at one simulated tick per millisecond: enough
    # headroom that the schedule stays due for the whole arm.
    base = now_ms() - 600_000
    sched_ids = [f"s{i}" for i in range(schedules)]
    for sid in sched_ids:
        conn.execute(
            "INSERT INTO schedules (id, next_fire_at_ms) VALUES (?, ?)", (sid, base)
        )
    conn.close()

    out = mp.Queue()
    deadline = time.time() + seconds
    procs = [
        mp.Process(
            target=worker_proc,
            args=(db, arm, f"w{i}", deadline, sched_ids, out),
        )
        for i in range(workers)
    ]
    t0 = time.time()
    for p in procs:
        p.start()
    results = [out.get() for _ in procs]
    for p in procs:
        p.join()
    elapsed = time.time() - t0

    conn = connect(db)
    total = conn.execute("SELECT count(*) FROM claims").fetchone()[0]
    distinct = conn.execute(
        "SELECT count(*) FROM (SELECT DISTINCT schedule_id, fired_for_ms FROM claims)"
    ).fetchone()[0]
    # The counter and the ledger must agree, or a fire happened without a row.
    fires = conn.execute("SELECT sum(fires) FROM schedules").fetchone()[0] or 0
    conn.close()

    wins = sum(r[1] for r in results)
    attempts = sum(r[2] for r in results)
    busy = sum(r[3] for r in results)
    dup = total - distinct
    return {
        "arm": arm,
        "workers": workers,
        "elapsed": elapsed,
        "claims": total,
        "distinct": distinct,
        "duplicates": dup,
        "dup_pct": (100.0 * dup / total) if total else 0.0,
        "wins_reported": wins,
        "attempts": attempts,
        "busy_errors": busy,
        "fires_counter": fires,
        "claims_per_s": total / elapsed if elapsed else 0.0,
    }


# ---- lease takeover -----------------------------------------------------


# ---- crash injection ----------------------------------------------------
#
# Design iteration 3's whole claim: a lease turns "the process holding this
# schedule vanished" from a schedule that is stuck for ever into one run that
# is lost and *recorded as lost*. That is testable, so it is tested: workers
# abandon a fraction of the claims they win, and a contested reaper has to
# notice every one of them exactly once.


def reap(conn, reaper, now):
    """Clear expired leases and record which fire each one lost.

    Guarded on `lease_owner` and `lease_until_ms` so that when several
    reapers sweep at once exactly one of them records each orphan — the same
    compare-and-swap argument as the claim itself, one table over.
    """
    rows = conn.execute(
        "SELECT id, lease_owner, lease_fired_for FROM schedules "
        " WHERE lease_owner IS NOT NULL AND lease_until_ms <= ?",
        (now,),
    ).fetchall()
    reaped = 0
    for sched_id, dead, fired_for in rows:
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE schedules SET lease_owner = NULL, lease_until_ms = NULL, "
                "       lease_fired_for = NULL "
                " WHERE id = ? AND lease_owner = ? AND lease_until_ms <= ?",
                (sched_id, dead, now),
            )
            if cur.rowcount == 1 and fired_for is not None:
                conn.execute(
                    "INSERT OR IGNORE INTO reaps "
                    "(schedule_id, fired_for_ms, dead_worker, reaper) VALUES (?,?,?,?)",
                    (sched_id, fired_for, dead, reaper),
                )
                reaped += 1
            conn.execute("COMMIT")
        except BaseException:
            conn.execute("ROLLBACK")
            raise
    return reaped


def claim_cas_reaping(conn, sched_id, worker, lease_ms):
    """Iteration 3's claim, corrected: the claim reaps the lease it displaces.

    Measured defect in the plain version: a claimant that takes over an
    *expired* lease overwrites `lease_fired_for` with its own instant, so the
    orphan the dead worker left is erased before any reaper's sweep gets to
    it. The reaper is racing the next claimant, and over a 4-second run it
    lost that race 26 times out of 170 claims. Recording the displaced lease
    inside the same transaction removes the race rather than shrinking it.
    """
    row = conn.execute(
        "SELECT next_fire_at_ms FROM schedules "
        " WHERE id = ? AND state = 'active' AND next_fire_at_ms <= ? "
        "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
        (sched_id, now_ms(), now_ms()),
    ).fetchone()
    if row is None:
        return None
    seen = row[0]
    following = seen + PERIOD_MS
    now = now_ms()

    conn.execute("BEGIN IMMEDIATE")
    try:
        # Whatever this claim is about to overwrite, write it down first.
        stale = conn.execute(
            "SELECT lease_owner, lease_fired_for FROM schedules "
            " WHERE id = ? AND lease_owner IS NOT NULL AND lease_until_ms <= ?",
            (sched_id, now),
        ).fetchone()
        if stale is not None and stale[1] is not None:
            conn.execute(
                "INSERT OR IGNORE INTO reaps "
                "(schedule_id, fired_for_ms, dead_worker, reaper) VALUES (?,?,?,?)",
                (sched_id, stale[1], stale[0], worker),
            )
        cur = conn.execute(
            "UPDATE schedules "
            "   SET next_fire_at_ms = ?, lease_owner = ?, lease_until_ms = ?, "
            "       lease_fired_for = ?, fires = fires + 1 "
            " WHERE id = ? AND state = 'active' "
            "   AND next_fire_at_ms = ? AND next_fire_at_ms <= ? "
            "   AND (lease_until_ms IS NULL OR lease_until_ms <= ?)",
            (following, worker, now + lease_ms, seen, sched_id, seen, now, now),
        )
        won = cur.rowcount == 1
        if won:
            conn.execute(
                "INSERT INTO claims (schedule_id, fired_for_ms, worker, arm) "
                "VALUES (?, ?, ?, 'cas_reaping')",
                (sched_id, seen, worker),
            )
        conn.execute("COMMIT")
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    return seen if won else None


def crash_worker(db, worker, deadline, sched_ids, crash_pct, lease_ms, seed, out,
                 claim=None):
    import random

    rng = random.Random(seed)
    claim = claim or claim_cas
    conn = connect(db)
    wins = crashed = reaped = busy = 0
    while time.time() < deadline:
        try:
            reaped += reap(conn, worker, now_ms())
            for sid in sched_ids:
                fired_for = claim(conn, sid, worker, lease_ms)
                if fired_for is None:
                    continue
                wins += 1
                if rng.random() < crash_pct:
                    # The supervisor was SIGKILLed: no completion, no release,
                    # and nothing this process will ever do about it.
                    crashed += 1
                    continue
                conn.execute("BEGIN IMMEDIATE")
                conn.execute(
                    "INSERT OR IGNORE INTO completions "
                    "(schedule_id, fired_for_ms, worker) VALUES (?,?,?)",
                    (sid, fired_for, worker),
                )
                conn.execute("COMMIT")
                release(conn, sid, worker)
        except sqlite3.OperationalError:
            busy += 1
            try:
                conn.execute("ROLLBACK")
            except sqlite3.OperationalError:
                pass
    conn.close()
    out.put((wins, crashed, reaped, busy))


def run_crash_race(db_dir, workers=8, seconds=6, crash_pct=0.3, lease_ms=250,
                   schedules=4, claim=None, tag="plain"):
    """Every claim must end as a completion or as a recorded reap. Never neither."""
    db = os.path.join(db_dir, f"race-crash-{tag}.db")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db + suffix)
        except FileNotFoundError:
            pass
    conn = connect(db)
    conn.executescript(SCHEMA)
    base = now_ms() - 600_000
    sched_ids = [f"c{i}" for i in range(schedules)]
    conn.execute("BEGIN IMMEDIATE")
    for sid in sched_ids:
        conn.execute(
            "INSERT INTO schedules (id, next_fire_at_ms) VALUES (?, ?)", (sid, base)
        )
    conn.execute("COMMIT")
    conn.close()

    out = mp.Queue()
    deadline = time.time() + seconds
    procs = [
        mp.Process(
            target=crash_worker,
            args=(db, f"w{i}", deadline, sched_ids, crash_pct, lease_ms, i, out, claim),
        )
        for i in range(workers)
    ]
    for p in procs:
        p.start()
    tallies = [out.get() for _ in procs]
    for p in procs:
        p.join()

    # One last sweep, because the run ended mid-flight and the final crashed
    # claims have not aged past their lease yet.
    conn = connect(db)
    time.sleep(lease_ms / 1000.0 + 0.1)
    reap(conn, "final", now_ms())

    q = lambda sql: conn.execute(sql).fetchone()[0]
    claims = q("SELECT count(*) FROM claims")
    distinct = q(
        "SELECT count(*) FROM (SELECT DISTINCT schedule_id, fired_for_ms FROM claims)"
    )
    completions = q("SELECT count(*) FROM completions")
    reaps = q("SELECT count(*) FROM reaps")
    both = q(
        "SELECT count(*) FROM completions c JOIN reaps r"
        "  ON c.schedule_id = r.schedule_id AND c.fired_for_ms = r.fired_for_ms"
    )
    # The invariant: a claim that is in neither table is a run that vanished
    # with nothing written down anywhere.
    unaccounted = q(
        "SELECT count(*) FROM claims a"
        " WHERE NOT EXISTS (SELECT 1 FROM completions c"
        "                    WHERE c.schedule_id = a.schedule_id"
        "                      AND c.fired_for_ms = a.fired_for_ms)"
        "   AND NOT EXISTS (SELECT 1 FROM reaps r"
        "                    WHERE r.schedule_id = a.schedule_id"
        "                      AND r.fired_for_ms = a.fired_for_ms)"
    )
    conn.close()
    return {
        "workers": workers,
        "crash_pct": crash_pct,
        "lease_ms": lease_ms,
        "claims": claims,
        "duplicates": claims - distinct,
        "completions": completions,
        "reaped": reaps,
        "double_counted": both,
        "unaccounted": unaccounted,
        "crashed_reported": sum(t[1] for t in tallies),
        "busy_errors": sum(t[3] for t in tallies),
    }


def contender(db, worker, rounds, gate, out):
    """Race every round's dead schedule from the same starting gun."""
    conn = connect(db)
    won = []
    for r in range(rounds):
        gate.wait()
        won.append(1 if claim_cas(conn, f"dead{r}", worker, 30_000) is not None else 0)
    conn.close()
    out.put(won)


def run_lease_takeover(db_dir, workers=8, rounds=200):
    """A claimant dies holding the lease. Exactly one other may take over."""
    db = os.path.join(db_dir, "race-lease.db")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db + suffix)
        except FileNotFoundError:
            pass
    conn = connect(db)
    conn.executescript(SCHEMA)
    # Schedules whose owner claimed them and then vanished: the lease is in the
    # past, so each is legally reclaimable exactly once.
    conn.execute("BEGIN IMMEDIATE")
    for r in range(rounds):
        conn.execute(
            "INSERT INTO schedules (id, next_fire_at_ms, lease_owner, lease_until_ms)"
            " VALUES (?, ?, 'ghost', ?)",
            (f"dead{r}", now_ms() - 5000, now_ms() - 1000),
        )
    conn.execute("COMMIT")
    conn.close()

    gate = mp.Barrier(workers)
    out = mp.Queue()
    procs = [
        mp.Process(target=contender, args=(db, f"w{i}", rounds, gate, out))
        for i in range(workers)
    ]
    for p in procs:
        p.start()
    tallies = [out.get() for _ in procs]
    for p in procs:
        p.join()

    per_round = [sum(t[r] for t in tallies) for r in range(rounds)]

    conn = connect(db)
    dup = conn.execute(
        "SELECT count(*) - count(DISTINCT schedule_id || ':' || fired_for_ms) FROM claims"
    ).fetchone()[0]
    conn.close()
    return {
        "rounds": rounds,
        "workers": workers,
        "min_winners": min(per_round),
        "max_winners": max(per_round),
        "rounds_not_exactly_one": sum(1 for w in per_round if w != 1),
        "duplicate_claims": dup,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--seconds", type=float, default=5.0)
    ap.add_argument("--rounds", type=int, default=200)
    ap.add_argument("--dir", default=os.environ.get("TMPDIR", "/tmp"))
    args = ap.parse_args()

    print(f"python {sys.version.split()[0]}  sqlite {sqlite3.sqlite_version}")
    print(f"{args.workers} OS processes, {args.seconds}s per arm, 4 schedules\n")

    print(
        f"{'arm':<11}{'claims':>9}{'distinct':>10}{'dupes':>8}{'dup%':>8}"
        f"{'busy':>7}{'claims/s':>11}"
    )
    for arm in ("cas", "immediate", "naive"):
        r = run_arm(args.dir, arm, args.workers, args.seconds)
        assert r["claims"] == r["fires_counter"], (
            f"{arm}: ledger {r['claims']} != counter {r['fires_counter']}"
        )
        print(
            f"{r['arm']:<11}{r['claims']:>9}{r['distinct']:>10}{r['duplicates']:>8}"
            f"{r['dup_pct']:>7.2f}%{r['busy_errors']:>7}{r['claims_per_s']:>11.0f}"
        )

    print("\nlease takeover — a dead claimant's schedule, contested at a barrier")
    t = run_lease_takeover(args.dir, workers=args.workers, rounds=args.rounds)
    print(
        f"  {t['rounds']} rounds x {t['workers']} processes: "
        f"winners per round min={t['min_winners']} max={t['max_winners']}, "
        f"rounds != 1 winner: {t['rounds_not_exactly_one']}, "
        f"duplicate claims: {t['duplicate_claims']}"
    )

    print("\ncrash injection — workers abandon 30% of the claims they win")
    print(
        f"{'claim':<14}{'claims':>8}{'dupes':>7}{'done':>7}{'reaped':>8}"
        f"{'twice':>7}{'UNACCOUNTED':>13}"
    )
    for tag, fn in (("lease only", claim_cas), ("lease+reap", claim_cas_reaping)):
        c = run_crash_race(
            args.dir, workers=args.workers, seconds=args.seconds,
            claim=fn, tag=tag.replace(" ", "-").replace("+", "-"),
        )
        print(
            f"{tag:<14}{c['claims']:>8}{c['duplicates']:>7}{c['completions']:>7}"
            f"{c['reaped']:>8}{c['double_counted']:>7}{c['unaccounted']:>13}"
        )

    print("\nVERDICT")
    print("  cas       must show 0 duplicates")
    print("  immediate must show 0 duplicates (correct, but serialises the poll)")
    print("  naive     duplicates are the point of the control arm")


if __name__ == "__main__":
    mp.set_start_method("spawn")
    main()
