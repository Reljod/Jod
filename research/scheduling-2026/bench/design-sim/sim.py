#!/usr/bin/env python3
"""Ten revisions of the scheduler design, each run against the same scenarios.

The claim protocol is tested with real processes next door in
`bench/claim-race/`. Everything else about a scheduler is a question about
*time* — what happens after a six-hour outage, on the morning the clocks move,
when a run outlives its own period, when a goal stops making progress — and
those are not questions you can answer by waiting. So this harness runs a fake
clock and asks each design the same seven questions.

Each design is the previous one plus one change. Scores come from the measured
outcomes, not from an opinion about the design.

Run: python3 sim.py
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

MIN = timedelta(minutes=1)
UTC = timezone.utc


# ---- cron ---------------------------------------------------------------


def _field_matches(spec: str, value: int, lo: int, hi: int) -> bool:
    for part in spec.split(","):
        if part == "*":
            return True
        if part.startswith("*/"):
            if (value - lo) % int(part[2:]) == 0:
                return True
        elif "-" in part:
            a, b = part.split("-")
            if int(a) <= value <= int(b):
                return True
        elif int(part) == value:
            return True
    return False


def matches(pattern: str, naive: datetime) -> bool:
    """Five-field cron against a naive wall-clock minute. POSIX weekdays."""
    m, h, dom, mon, dow = pattern.split()
    return (
        _field_matches(m, naive.minute, 0, 59)
        and _field_matches(h, naive.hour, 0, 23)
        and _field_matches(dom, naive.day, 1, 31)
        and _field_matches(mon, naive.month, 1, 12)
        # POSIX: 0 = Sunday. Python's weekday() is 0 = Monday.
        and _field_matches(dow, (naive.weekday() + 1) % 7, 0, 6)
    )


def resolve_local(naive: datetime, tz: ZoneInfo) -> tuple[datetime, str]:
    """Turn a wall-clock time into a real instant, the way croner does.

    Gap: the wall-clock time does not exist, so fire at the first instant that
    is at or after it — 02:30 on a spring-forward morning becomes 03:00.
    Fold: the wall-clock time happens twice, so a fixed-time job takes the
    first one and the second is not a separate fire.
    """
    aware = naive.replace(tzinfo=tz)
    inst = aware.astimezone(UTC)
    if inst.astimezone(tz).replace(tzinfo=None) != naive:
        # Nonexistent. Walk forward in real time to the transition itself.
        probe = inst - timedelta(hours=3)
        while probe.astimezone(tz).replace(tzinfo=None) < naive:
            probe += MIN
        return probe, "gap_snapped"
    if naive.replace(tzinfo=tz, fold=1).utcoffset() != aware.utcoffset():
        return inst, "fold_first"
    return inst, "normal"


def next_fire(pattern: str, after: datetime, tz_mode: str, zone: str,
              offset_min: int) -> tuple[datetime, str]:
    """The next instant this pattern fires, strictly after `after` (UTC)."""
    if tz_mode == "offset":
        # What a design that stored a fixed UTC offset can do. Correct in
        # whichever half of the year the offset was captured, and an hour out
        # in the other half.
        shift = timedelta(minutes=offset_min)
        naive = (after + shift).replace(second=0, microsecond=0) + MIN
        for _ in range(400_000):
            if matches(pattern, naive):
                return naive - shift, "fixed_offset"
            naive += MIN
        raise RuntimeError("no occurrence")

    tzi = ZoneInfo(zone)
    naive = after.astimezone(tzi).replace(tzinfo=None, second=0, microsecond=0) + MIN
    last = None
    for _ in range(400_000):
        if matches(pattern, naive):
            inst, kind = resolve_local(naive, tzi)
            # A fold makes two wall-clock minutes resolve to the same instant
            # for a fixed-time job; emitting it once is the whole point.
            if inst > after and inst != last:
                return inst, kind
            last = inst
        naive += MIN
    raise RuntimeError("no occurrence")


# ---- designs ------------------------------------------------------------


@dataclass
class Design:
    n: int
    name: str
    change: str
    atomic_claim: bool = False
    lease: bool = False
    lease_reaps_on_claim: bool = False
    tz_mode: str = "offset"
    misfire_policy: bool = False
    grace: bool = False
    overlap: str = "allow"          # allow | lease_skip | policy
    jitter_ms: int = 0
    backoff: bool = False
    breaker: bool = False
    goal: bool = False
    stall: bool = False
    budget: bool = False
    memory_backed: bool = False
    # Every outcome this design can write down, for the observability score.
    records: set = field(default_factory=lambda: {"fired"})
    ddl_columns: int = 4
    claim_sql_lines: int = 3


DESIGNS = [
    Design(1, "naive poll", "SELECT due, then UPDATE next_fire_at",
           records={"fired"}, ddl_columns=4, claim_sql_lines=3),
    Design(2, "+ CAS claim", "BEGIN IMMEDIATE + compare-and-swap on next_fire_at_ms",
           atomic_claim=True, records={"fired"}, ddl_columns=5, claim_sql_lines=9),
    Design(3, "+ lease", "lease_until_ms, swept when a claimant dies",
           atomic_claim=True, lease=True, overlap="lease_skip",
           records={"fired", "skipped_overlap"}, ddl_columns=8, claim_sql_lines=12),
    Design(4, "+ claim-time reap", "the claim records the expired lease it displaces",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           overlap="lease_skip",
           records={"fired", "skipped_overlap", "run_lost"},
           ddl_columns=9, claim_sql_lines=18),
    Design(5, "+ IANA timezone", "store the zone name, not a captured offset",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", overlap="lease_skip",
           records={"fired", "skipped_overlap", "run_lost"},
           ddl_columns=10, claim_sql_lines=18),
    Design(6, "+ misfire policy", "fire_once | skip | fire_all, bounded by grace_ms",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", misfire_policy=True, grace=True, overlap="lease_skip",
           records={"fired", "skipped_overlap", "run_lost", "skipped_late",
                    "caught_up"},
           ddl_columns=12, claim_sql_lines=18),
    Design(7, "+ overlap policy", "skip | queue_one | replace | allow",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", misfire_policy=True, grace=True, overlap="policy",
           records={"fired", "skipped_overlap", "run_lost", "skipped_late",
                    "caught_up", "replaced"},
           ddl_columns=13, claim_sql_lines=18),
    Design(8, "+ jitter, backoff, breaker", "spread fires; back off; pause after 5 failures",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", misfire_policy=True, grace=True, overlap="policy",
           jitter_ms=300_000, backoff=True, breaker=True,
           records={"fired", "skipped_overlap", "run_lost", "skipped_late",
                    "caught_up", "replaced", "backed_off", "paused"},
           ddl_columns=16, claim_sql_lines=18),
    Design(9, "+ goals", "goal state machine, stall detection, budget caps",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", misfire_policy=True, grace=True, overlap="policy",
           jitter_ms=300_000, backoff=True, breaker=True,
           goal=True, stall=True, budget=True,
           records={"fired", "skipped_overlap", "run_lost", "skipped_late",
                    "caught_up", "replaced", "backed_off", "paused",
                    "goal_iteration", "goal_ended"},
           ddl_columns=30, claim_sql_lines=18),
    Design(10, "trimmed final", "jitter off by default, queue_one dropped, goal state in memory",
           atomic_claim=True, lease=True, lease_reaps_on_claim=True,
           tz_mode="iana", misfire_policy=True, grace=True, overlap="policy",
           jitter_ms=0, backoff=True, breaker=True,
           goal=True, stall=True, budget=True, memory_backed=True,
           records={"fired", "skipped_overlap", "run_lost", "skipped_late",
                    "caught_up", "replaced", "backed_off", "paused",
                    "goal_iteration", "goal_ended", "goal_fact"},
           ddl_columns=26, claim_sql_lines=18),
]


# ---- the fire loop, parameterised by design -----------------------------


@dataclass
class Sched:
    pattern: str
    zone: str
    offset_min: int
    next_fire: datetime
    misfire: str = "fire_once"
    grace: timedelta = timedelta(hours=1)
    overlap: str = "skip"
    run_len: timedelta = timedelta(minutes=1)
    always_fails: bool = False
    lease_until: datetime | None = None
    failures: int = 0
    paused: bool = False
    backoff_until: datetime | None = None


def simulate(d: Design, s: Sched, start: datetime, end: datetime,
             gap: tuple[datetime, datetime] | None = None,
             tick: timedelta = MIN):
    """Run one schedule from `start` to `end`. `gap` is scheduler downtime."""
    events = []
    running = []                      # (ends_at, fired_for)
    queued = 0
    now = start
    s.next_fire, _ = next_fire(s.pattern, start - MIN, d.tz_mode, s.zone, s.offset_min)

    while now < end:
        if gap and gap[0] <= now < gap[1]:
            now = gap[1]
            continue
        running = [r for r in running if r[0] > now]

        if s.paused:
            now += tick
            continue

        while s.next_fire <= now:
            scheduled = s.next_fire
            late = now - scheduled

            following, kind = next_fire(
                s.pattern, max(scheduled, now), d.tz_mode, s.zone, s.offset_min
            )

            # --- misfire -------------------------------------------------
            if d.misfire_policy:
                if d.grace and late > s.grace:
                    if s.misfire == "fire_all":
                        # Replay: advance by exactly one period so the next
                        # loop pass picks up the following missed instant.
                        events.append(("caught_up", scheduled, now))
                        s.next_fire, _ = next_fire(
                            s.pattern, scheduled, d.tz_mode, s.zone, s.offset_min
                        )
                        continue
                    events.append(("skipped_late", scheduled, now))
                    s.next_fire = following
                    continue
                if s.misfire == "skip" and late > timedelta(0):
                    events.append(("skipped_late", scheduled, now))
                    s.next_fire = following
                    continue
            else:
                # No policy at all: every missed instant is replayed, because
                # nothing in the loop knows it is late.
                if late > timedelta(0):
                    events.append(("caught_up", scheduled, now))
                    s.next_fire, _ = next_fire(
                        s.pattern, scheduled, d.tz_mode, s.zone, s.offset_min
                    )
                    if s.next_fire <= now:
                        running.append((now + s.run_len, scheduled))
                        continue

            # --- backoff -------------------------------------------------
            if d.backoff and s.backoff_until and now < s.backoff_until:
                s.next_fire = max(following, s.backoff_until)
                events.append(("backed_off", scheduled, now))
                continue

            # --- jitter --------------------------------------------------
            fire_at = now
            if d.jitter_ms:
                # Deterministic "random" so the run reproduces: the hash of
                # the instant stands in for the RNG.
                h = int(hashlib.sha256(str(scheduled).encode()).hexdigest()[:8], 16)
                fire_at = now + timedelta(milliseconds=h % d.jitter_ms)
                if d.grace and (fire_at - scheduled) > s.grace:
                    events.append(("skipped_late", scheduled, fire_at))
                    s.next_fire = following
                    continue

            # --- overlap -------------------------------------------------
            busy = bool(running) if not d.lease else (s.lease_until is not None
                                                      and s.lease_until > now)
            if busy:
                policy = s.overlap if d.overlap == "policy" else (
                    "skip" if d.overlap == "lease_skip" else "allow"
                )
                if policy == "skip":
                    events.append(("skipped_overlap", scheduled, now))
                    s.next_fire = following
                    continue
                if policy == "queue_one":
                    queued = min(queued + 1, 1)
                    s.next_fire = following
                    continue
                if policy == "replace":
                    running = []
                    s.lease_until = None
                    events.append(("replaced", scheduled, now))

            # --- fire ----------------------------------------------------
            events.append(("fired", scheduled, fire_at))
            running.append((fire_at + s.run_len, scheduled))
            if d.lease:
                s.lease_until = fire_at + s.run_len
            if s.always_fails:
                s.failures += 1
                if d.backoff:
                    # Full jitter, deterministic here: cap the exponential and
                    # clamp so a backoff never pushes past the next natural fire.
                    delay = min(timedelta(hours=6), timedelta(minutes=1) * (2 ** min(s.failures, 12)))
                    s.backoff_until = now + delay
                if d.breaker and s.failures >= 5:
                    s.paused = True
                    events.append(("paused", scheduled, now))
            s.next_fire = following

        now += tick
    return events


# ---- scenarios ----------------------------------------------------------


def s1_downtime(d: Design):
    """Six hours down on a five-minute schedule. How many runs come back?"""
    start = datetime(2026, 8, 10, 0, 0, tzinfo=UTC)
    s = Sched("*/5 * * * *", "Asia/Manila", 480, start,
              misfire="fire_once", grace=timedelta(minutes=2, seconds=30))
    ev = simulate(d, s, start, start + timedelta(hours=8),
                  gap=(start + timedelta(hours=1), start + timedelta(hours=7)))
    burst = sum(1 for e in ev if e[0] in ("fired", "caught_up")
                and start + timedelta(hours=7) <= e[2] < start + timedelta(hours=7, minutes=1))
    return {
        "catchup_burst": burst,
        "skipped_recorded": sum(1 for e in ev if e[0] == "skipped_late"),
        "total_fires": sum(1 for e in ev if e[0] == "fired"),
    }


def s2_dst_spring(d: Design):
    """02:30 daily, New York, across 2026-03-08. Does the day happen, and when?"""
    start = datetime(2026, 3, 6, 12, 0, tzinfo=UTC)
    s = Sched("30 2 * * *", "America/New_York", -300, start, grace=timedelta(hours=6))
    ev = simulate(d, s, start, start + timedelta(days=5), tick=timedelta(minutes=1))
    ny = ZoneInfo("America/New_York")
    fired = [e[2].astimezone(ny) for e in ev if e[0] == "fired"]
    on_the_day = [f for f in fired if f.date() == datetime(2026, 3, 8).date()]
    # How far each fire lands from the intended 02:30 local, in minutes.
    drift = [abs((f.hour * 60 + f.minute) - 150) for f in fired
             if f.date() != datetime(2026, 3, 8).date()]
    return {
        "fires_on_transition_day": len(on_the_day),
        "local_time_on_the_day": on_the_day[0].strftime("%H:%M %Z") if on_the_day else "-",
        "max_drift_min": max(drift) if drift else 0,
    }


def s3_dst_fall(d: Design):
    """01:30 daily, New York, across 2026-11-01. Exactly one fire, or two?"""
    start = datetime(2026, 10, 30, 12, 0, tzinfo=UTC)
    s = Sched("30 1 * * *", "America/New_York", -240, start, grace=timedelta(hours=6))
    ev = simulate(d, s, start, start + timedelta(days=4))
    ny = ZoneInfo("America/New_York")
    fired = [e[2].astimezone(ny) for e in ev if e[0] == "fired"]
    on_the_day = [f for f in fired if f.date() == datetime(2026, 11, 1).date()]
    drift = [abs((f.hour * 60 + f.minute) - 90) for f in fired
             if f.date() != datetime(2026, 11, 1).date()]
    return {
        "fires_on_transition_day": len(on_the_day),
        "max_drift_min": max(drift) if drift else 0,
    }


def s4_overlap(d: Design):
    """An hourly schedule whose runs take 90 minutes, for eight hours."""
    start = datetime(2026, 8, 10, 0, 0, tzinfo=UTC)
    s = Sched("0 * * * *", "Asia/Manila", 480, start,
              run_len=timedelta(minutes=90), overlap="skip",
              grace=timedelta(minutes=30))
    ev = simulate(d, s, start, start + timedelta(hours=8))
    fires = [e for e in ev if e[0] == "fired"]
    # Peak concurrency: how many 90-minute runs were live at once.
    peak = 0
    for _, _, at in fires:
        live = sum(1 for _, _, o in fires if o <= at < o + timedelta(minutes=90))
        peak = max(peak, live)
    return {
        "fires": len(fires),
        "peak_concurrent": peak,
        "skips_recorded": sum(1 for e in ev if e[0] == "skipped_overlap"),
    }


def s5_jitter(d: Design):
    """Jitter wider than the grace window. Does spreading load lose fires?"""
    start = datetime(2026, 8, 10, 0, 0, tzinfo=UTC)
    s = Sched("*/5 * * * *", "Asia/Manila", 480, start,
              grace=timedelta(seconds=150))
    ev = simulate(d, s, start, start + timedelta(hours=6))
    fired = sum(1 for e in ev if e[0] == "fired")
    lost = sum(1 for e in ev if e[0] == "skipped_late")
    delays = [int((e[2] - e[1]).total_seconds()) for e in ev if e[0] == "fired"]
    return {
        "fired": fired,
        "lost_to_jitter": lost,
        "p95_delay_s": sorted(delays)[int(0.95 * (len(delays) - 1))] if delays else 0,
    }


def s6_backoff(d: Design):
    """A schedule whose every run fails, left alone for 24 hours."""
    start = datetime(2026, 8, 10, 0, 0, tzinfo=UTC)
    s = Sched("*/5 * * * *", "Asia/Manila", 480, start, always_fails=True,
              grace=timedelta(minutes=2, seconds=30))
    ev = simulate(d, s, start, start + timedelta(hours=24))
    return {
        "spawn_attempts": sum(1 for e in ev if e[0] == "fired"),
        "paused": any(e[0] == "paused" for e in ev),
    }


def s7_goal(d: Design):
    """A goal costing $1.50 an iteration that stops making progress at 7."""
    budget_usd, budget_iters, cap = 20.0, 50, 200
    spent = 0.0
    stalls = 0
    last_hash = None
    for i in range(1, cap + 1):
        if d.budget and (spent + 1.5 > budget_usd or i > budget_iters):
            return {"iterations": i - 1, "ended": "budget_exhausted",
                    "spent_usd": round(spent, 2)}
        spent += 1.5
        # The world stops changing after iteration 7.
        world = f"state-{min(i, 7)}"
        if d.stall:
            if world == last_hash:
                stalls += 1
                if stalls >= 3:
                    return {"iterations": i, "ended": "stalled",
                            "spent_usd": round(spent, 2)}
            else:
                stalls = 0
        last_hash = world
    return {"iterations": cap, "ended": "never_stopped (capped by the harness)",
            "spent_usd": round(spent, 2)}


# ---- rubric -------------------------------------------------------------
#
# Fixed before any design was run. Weights sum to 100.

RUBRIC = [
    ("crash/restart correctness", 20),
    ("race-freedom (N processes)", 20),
    ("DST / timezone correctness", 15),
    ("operator predictability", 15),
    ("goal-loop safety", 15),
    ("implementation size", 8),
    ("observability", 7),
]

# Measured next door by bench/claim-race/claim_race.py, 8 processes, 4 s per
# arm. Reproduced in out/claim-race.txt; quoted here so the two scores that
# need real concurrency are not guesses.
RACE = {
    "naive_dup_pct": 19.88,       # design 1
    "cas_dup_pct": 0.00,          # designs 2-10
    "lease_unaccounted": 28,      # design 3: claims lost with nothing recorded
    "lease_claims": 205,
    "reaping_unaccounted": 0,     # designs 4-10
    "reaping_claims": 199,
}


def score(d: Design, r: dict) -> dict:
    """Every score below is a function of a measured number."""
    # Race-freedom: duplicate rate under 8 concurrent processes.
    race = 0 if not d.atomic_claim else 5

    # Crash: what fraction of claims end with nothing written down.
    if not d.lease:
        crash = 0            # the schedule advances; the lost run is invisible
    elif not d.lease_reaps_on_claim:
        lost = RACE["lease_unaccounted"] / RACE["lease_claims"]
        crash = 2 if lost > 0.05 else 4
    else:
        crash = 5

    # DST: fires on the transition day, and drift on ordinary days.
    spring, fall = r["s2"], r["s3"]
    dst = 5
    if spring["max_drift_min"] > 0 or fall["max_drift_min"] > 0:
        dst = 1
    if spring["fires_on_transition_day"] == 0:
        dst = min(dst, 3)
    if fall["fires_on_transition_day"] != 1:
        dst = min(dst, 2)

    # Predictability: a catch-up burst, a jitter delay and unexplained silence
    # all make the answer to "what will it do?" harder.
    pred = 5
    if r["s1"]["catchup_burst"] > 10:
        pred -= 2
    if r["s5"]["p95_delay_s"] > 60:
        pred -= 1
    if r["s4"]["peak_concurrent"] > 1:
        pred -= 1
    if r["s5"]["lost_to_jitter"] > 0:
        pred -= 1
    pred = max(pred, 0)

    # Goal safety: does the loop stop, and on which ceiling.
    g = r["s7"]
    if g["ended"].startswith("never_stopped"):
        goal = 0
    elif g["ended"] == "stalled":
        goal = 5
    else:
        goal = 3

    # Size: fewer columns and fewer lines of claim SQL is better. Normalised
    # against the largest design so the scale is the set itself.
    worst = max(x.ddl_columns + x.claim_sql_lines for x in DESIGNS)
    best = min(x.ddl_columns + x.claim_sql_lines for x in DESIGNS)
    mine = d.ddl_columns + d.claim_sql_lines
    size = round(5 * (worst - mine) / (worst - best), 1)

    # Observability: of the eleven outcomes any design in this set can reach,
    # how many does this one leave a durable row for.
    all_records = set()
    for x in DESIGNS:
        all_records |= x.records
    obs = round(5 * len(d.records) / len(all_records), 1)

    parts = [crash, race, dst, pred, goal, size, obs]
    total = sum(p * w for p, (_, w) in zip(parts, RUBRIC)) / 5.0
    return {"parts": parts, "total": round(total, 1)}


def main():
    results = {}
    for d in DESIGNS:
        r = {
            "s1": s1_downtime(d),
            "s2": s2_dst_spring(d),
            "s3": s3_dst_fall(d),
            "s4": s4_overlap(d),
            "s5": s5_jitter(d),
            "s6": s6_backoff(d),
            "s7": s7_goal(d),
        }
        r["score"] = score(d, r)
        results[d.n] = r

    print("Ten design iterations, one fake clock, seven scenarios\n")
    print("RUBRIC (fixed before the first run):")
    for name, w in RUBRIC:
        print(f"  {w:>3}  {name}")

    print("\n== SCENARIO OUTCOMES ==")
    hdr = (f"{'#':>2} {'design':<26}{'catch':>6}{'sprg':>6}{'fall':>6}"
           f"{'peak':>6}{'jitL':>6}{'fail':>6}{'goal-iters':>11} {'goal end':<24}")
    print(hdr)
    for d in DESIGNS:
        r = results[d.n]
        print(
            f"{d.n:>2} {d.name:<26}"
            f"{r['s1']['catchup_burst']:>6}"
            f"{r['s2']['fires_on_transition_day']:>6}"
            f"{r['s3']['fires_on_transition_day']:>6}"
            f"{r['s4']['peak_concurrent']:>6}"
            f"{r['s5']['lost_to_jitter']:>6}"
            f"{r['s6']['spawn_attempts']:>6}"
            f"{r['s7']['iterations']:>11} {r['s7']['ended']:<24}"
        )
    print("  catch = runs launched in the first minute back after a 6h outage")
    print("  sprg  = fires on 2026-03-08 (02:30 daily, New York) — 0 means the day vanished")
    print("  fall  = fires on 2026-11-01 (01:30 daily, New York) — 2 means it ran twice")
    print("  peak  = concurrent runs, hourly schedule with 90-minute runs")
    print("  jitL  = fires lost because jitter pushed them past grace_ms")
    print("  fail  = spawn attempts in 24h for a schedule whose every run fails")

    print("\n== DST DETAIL ==")
    for d in DESIGNS:
        r = results[d.n]
        print(f"{d.n:>2} {d.name:<26} spring-forward fire: "
              f"{r['s2']['local_time_on_the_day']:<12} "
              f"max drift on ordinary days: {r['s2']['max_drift_min']} min")

    print("\n== SCORES ==")
    cols = "".join(f"{n.split()[0][:5]:>7}" for n, _ in RUBRIC)
    print(f"{'#':>2} {'design':<26}{cols}{'TOTAL':>8}")
    for d in DESIGNS:
        s = results[d.n]["score"]
        row = "".join(f"{p:>7}" for p in s["parts"])
        print(f"{d.n:>2} {d.name:<26}{row}{s['total']:>8}")

    print("\n== RANKING (by score, not by recency) ==")
    order = sorted(DESIGNS, key=lambda d: -results[d.n]["score"]["total"])
    for rank, d in enumerate(order, 1):
        print(f"{rank:>2}. iteration {d.n:<3} {d.name:<28} {results[d.n]['score']['total']:>6}"
              f"   {d.change}")

    print("\n== REGRESSIONS: where an addition cost something measurable ==")
    for prev, cur in zip(DESIGNS, DESIGNS[1:]):
        p, c = results[prev.n]["score"], results[cur.n]["score"]
        for (name, _), a, b in zip(RUBRIC, p["parts"], c["parts"]):
            if b < a:
                print(f"  iteration {prev.n} -> {cur.n}: {name} {a} -> {b}"
                      f"   ({cur.change})")


if __name__ == "__main__":
    main()
