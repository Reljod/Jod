# Scheduling, goals, and the claim that must not race

**Date:** 2026-08-10 · **Analyst:** Jod
**Raw measurements:** [`out/cron-dst.txt`](out/cron-dst.txt) ·
[`out/claim-race.txt`](out/claim-race.txt) ·
[`out/design-sim.txt`](out/design-sim.txt)
**Re-runnable:** [`bench/cron-dst/`](bench/cron-dst) (Rust),
[`bench/claim-race/claim_race.py`](bench/claim-race/claim_race.py),
[`bench/design-sim/sim.py`](bench/design-sim/sim.py)

> **Provenance of this file.** The benchmarks and the design work are a
> subagent's. Its `Write` to this path was refused by a harness hook
> ("subagents should return findings as text"); it reported that rather than
> routing around the check with a shell heredoc, which is the correct reading
> of the charter. This file is written by the lead session from the committed
> raw output. Every number below is read out of `out/`, not restated from
> memory.

---

## 1. The answer

Use **croner**. Store an **IANA zone name**, never an offset. Claim a due
schedule with **one guarded statement** inside `BEGIN IMMEDIATE`, and make that
claim **reap the lease it displaces**. Default misfire to **fire-once**, overlap
to **skip**, and jitter to **zero**. Stop a schedule after five consecutive
failures and a goal when it stops making progress.

Every one of those is a measured result rather than a preference, and one of
them — jitter — is the opposite of what intuition suggests.

---

## 2. The cron crate: measured, not chosen by popularity

Four crates against the same two DST transitions
([`out/cron-dst.txt`](out/cron-dst.txt)): croner 3.0.1, cron 0.17.0,
saffron 0.1.0, cronexpr 1.6.0, with chrono-tz 2025b.

### Syntax

| pattern | croner | cron | saffron | cronexpr |
|---|---|---|---|---|
| `@daily` | yes | yes | **no** | **no** |
| seconds field | yes | yes | **no** | **no** |
| `L` — last day of month | yes | **no** | yes | yes |
| `#` — second Friday | yes | **no** | yes | yes |
| `W` — nearest weekday | yes | **no** | yes | yes |

**croner is the only one that accepts all five.**

### Spring forward — `30 2 * * *`, America/New_York, 2026-03-08

2026-03-08 has no 02:30 in New York. Acceptable: run at 03:00 (the Vixie
behaviour) or skip the day. Unacceptable: panic, or drift the following days.

| crate | what it did |
|---|---|
| **croner** | **2026-03-08 03:00 EDT**, then back to 02:30 on the 9th ✓ |
| cron | skipped straight to 2026-03-09 — **the day vanished** |
| cronexpr | skipped to 2026-03-09 — same |
| saffron | 2026-03-07 **21:30**, then 22:30, then 22:30 — no timezone concept at all |

### Fall back — `30 1 * * *`, America/New_York, 2026-11-01

Two 01:30s exist. A fixed-time daily job must fire **once**.

| crate | what it did |
|---|---|
| **croner** | 11-01 01:30 EDT, then **11-02** ✓ |
| cron | 11-01 01:30 EDT, then **11-01 01:30 EST** — fires twice |
| cronexpr | same double fire |
| saffron | 10-31 21:30, then 20:30 — wrong throughout |

A daily job that runs twice is a duplicated digest, a doubled spend, and — for
anything that writes — a duplicated side effect.

### The Europe/London repro

`3 1 * * *`, Europe/London, across 2026-10-25 (the `zslayton#48` "invalid time"
panic case). **Nothing panicked.** croner fired once and moved on; `cron` and
`cronexpr` double-fired again; saffron was wrong throughout.

**Verdict: croner.** It is the only crate correct on both transitions *and*
complete on syntax.

---

## 3. The claim protocol

16 real OS processes, 4 schedules, 6 seconds per arm
([`out/claim-race.txt`](out/claim-race.txt), Python 3.14.4 / SQLite 3.46.1):

| arm | claims | distinct | duplicates | dup % | claims/s |
|---|---:|---:|---:|---:|---:|
| **cas** | 5,408 | 5,408 | **0** | **0.00%** | 870 |
| immediate | 5,085 | 5,085 | 0 | 0.00% | 817 |
| **naive** (control) | 3,585 | 2,106 | **1,479** | **41.26%** | 569 |

The naive read-then-write arm is the control, and duplicates are the point of
it: **two winners for the same schedule 41% of the time.** The guarded
compare-and-swap is both correct *and* the fastest arm — serialising the poll,
as the `immediate` arm does, costs throughput and buys nothing extra.

### Lease takeover

500 rounds × 16 processes contending at a barrier for one dead claimant's
schedule: **winners per round min=1, max=1; rounds with ≠1 winner: 0;
duplicate claims: 0.**

### The finding the experiment produced, that reading would not have

Crash injection — workers abandon 30% of the claims they win:

| design | claims | dupes | done | reaped | twice | **unaccounted** |
|---|---:|---:|---:|---:|---:|---:|
| lease only | 255 | 0 | 168 | 35 | 0 | **52** |
| lease + reap | 270 | 0 | 182 | 88 | 0 | **0** |

**A lease alone loses one claim in five.** It looks sufficient — the lease
expires, someone takes over, the schedule keeps running — but the next claimant
*overwrites* the lease, and the original claim then exists in no record at all.
The reaper arrives to find nothing to reap.

The fix is that whoever displaces a dead lease is the **last process that can
still see it existed**, so the claim writes the abandonment down *before* taking
it. 52 → 0.

---

## 4. Ten graded design iterations

Rubric fixed in code before the first run
([`bench/design-sim/sim.py`](bench/design-sim/sim.py)): crash/restart correctness
20, race-freedom 20, DST correctness 15, operator predictability 15, goal-loop
safety 15, implementation size 8, observability 7.

Seven scenarios over one fake clock. Key columns: **catch** = runs launched in
the first minute back after a 6h outage · **fall** = fires on 2026-11-01
(2 means it ran twice) · **peak** = concurrent runs for an hourly schedule with
90-minute runs · **jitL** = fires lost because jitter pushed them past
`grace_ms` · **fail** = spawn attempts in 24h for a schedule whose every run
fails · **skipR** = skips written down as rows.

| # | design | catch | fall | peak | jitL | fail | skipR | **total** |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | naive poll | 73 | 1 | 2 | 0 | 288 | 0 | 17.7 |
| 2 | + CAS claim | 73 | 1 | 2 | 0 | 288 | 0 | 36.3 |
| 3 | + lease | 73 | 1 | 1 | 0 | 288 | 0 | 46.7 |
| 4 | + claim-time reap | 73 | 1 | 1 | 0 | 288 | 0 | 58.1 |
| 5 | + IANA timezone | 73 | 1 | 1 | 0 | 288 | 0 | 69.8 |
| 6 | + misfire policy | **0** | 1 | 1 | 0 | 288 | 1 | 76.7 |
| 7 | + overlap policy | 0 | 1 | 1 | 0 | 288 | 1 | 77.1 |
| 8 | + jitter, backoff, breaker | 0 | 1 | 1 | **34** | **5** | 9 | 71.8 |
| 9 | + goals | 0 | 1 | 1 | 34 | 5 | 9 | 85.3 |
| **10** | **trimmed final** | **0** | **1** | **1** | **0** | **5** | 1 | **92.8** |

### Ranking, by score rather than recency

1. **iteration 10 — trimmed final, 92.8** · jitter off by default, `queue_one`
   dropped, goal state in memory
2. iteration 9 — goals, 85.3
3. iteration 7 — overlap policy, 77.1
4. iteration 6 — misfire policy, 76.7
5. **iteration 8 — jitter/backoff/breaker, 71.8** ← *ranks below 6 and 7 despite
   being later and larger*
6. iteration 5 — IANA timezone, 69.8
7. iteration 4 — claim-time reap, 58.1
8. iteration 3 — lease, 46.7
9. iteration 2 — CAS claim, 36.3
10. iteration 1 — naive poll, 17.7

**Iteration 8 ranking below 6 and 7 is the reason for scoring rather than taking
the newest.** It added three things; two were right (backoff and the breaker
took 288 spawn attempts to 5) and one was wrong.

### Regressions the log recorded

- 1→2, 2→3, 3→4, 4→5, 5→6, 6→7: implementation size falls monotonically, 5.0 →
  2.1. Every correctness gain costs code. That is the honest price.
- **7→8: operator predictability 5 → 3** — jitter.
- 8→9: implementation size 1.7 → 0.0 — goals are the largest single addition.

### DST across the iterations

Iterations 1–4 fired the spring-forward job at **03:30 EDT with 60 minutes of
drift on ordinary days**, because they captured an offset. From iteration 5, once
the zone *name* is stored: **03:00 EDT, 0 minutes of drift**. That single change
is worth 15 rubric points and one line of schema.

---

## 5. Jitter: the one addition that measured worse

Spreading fires to avoid a thundering herd is the obvious move. Measured, a
**300 s spread against a 150 s grace window lost 34 of 72 fires** — jitter
pushed them past the point where they still counted as that fire, so they were
dropped rather than delayed — and operator predictability fell from 5 to 3.

**Ships defaulting to zero, and `jitter_ms >= grace_ms` is refused at the
boundary** rather than silently losing fires.

The general rule: *a safety feature that has not been measured against the
failure it claims to prevent is a guess.*

---

## 6. The recommended policy set

| Question | Answer | Evidence |
|---|---|---|
| Cron crate | **croner 3.0.1** | §2 — only crate correct on both transitions and complete on syntax |
| Timezone | **IANA name, never an offset** | §4 — 60 min drift → 0 |
| Misfire | **`fire_once`** default; `skip`; `fire_all` capped at 100 | 6h outage: 73 runs → 0 unwanted. Unbounded replay = 72 instants |
| Overlap | **`skip`** default; `replace`; `allow`. `queue_one` dropped | peak 2 → 1; queueing added a state nobody could predict |
| Claim | **CAS + lease + claim-time reap** | §3 — 41.26% → 0%, and 52 unaccounted → 0 |
| Failure | backoff, breaker at **5** consecutive | 288 spawn attempts in 24h → 5 |
| Jitter | **0**, and refused when ≥ grace | §5 — lost 34 of 72 fires |
| Skips | **always a row** | a skip nobody recorded is a silent failure |
| Where it lives | a tick inside a long-lived process, 60 s | cron's own resolution; faster buys nothing |

---

## 7. Goals

Goals are **memory-backed**, per the owner's requirement, rather than carrying a
private journal:

- **The brief** is a *prospective* fact, superseded each iteration — so
  bitemporal validity answers "what did it think it was doing last month".
- **What happened** each iteration is episodic, written in a **`goal:<id>`
  scope**. Its own partition because an hourly loop writes far more than a
  person does, and scope is a hard filter — so those writes cannot crowd out
  ordinary recall.
- **Conclusions** go to the goal's domain scope, where ordinary recall finds
  them.
- `goal_journal` and `progress_note` were deleted from an earlier iteration:
  they duplicated the fact store.
- **Budgets and counters stay as columns.** The claim reads them on every tick
  and must not depend on a text index.

Safety rules, each with its reason:

| Rule | Why |
|---|---|
| **Stall detection** — count iterations that changed nothing; enough, and the goal stalls itself | The characteristic failure of an autonomous loop. From outside, no progress looks identical to hard work |
| **Iteration cap** and **spend cap**, re-checked *after* recording | So a goal that just spent its last dollar stops rather than spending more proving it |
| **Gates before the judge** | From Hermes' `/goal`: a deterministic check that short-circuits is evidence; a model's opinion is not. Its output *is* what the agent must repair against |
| **Judge fails open** | Also Hermes: a broken judge must not wedge progress; the budget is the backstop |
| Claim goals with the same CAS | Two processes iterating one goal double its spend and corrupt the progress count that decides whether it stalled |

---

## 8. What is not measured

Stated so nothing here is mistaken for a result:

- **The systemd units are written from the man pages and were never installed or
  run.** Everything else in this report is measured.
- The claim benchmark ran on SQLite 3.46.1 via Python; the shipped code uses
  rusqlite's bundled SQLite. The protocol is the same SQL, but the exact
  throughput numbers are not the shipped ones. The *duplicate counts* are the
  load-bearing result and are a property of the SQL, not the binding.
- The design simulation uses a fake clock and a stub spawn. It measures
  scheduling decisions, not what happens when a real harness is slow.
- One machine, one run per arm.
