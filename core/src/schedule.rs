//! Scheduled work, and the objectives that outlive a single run.
//!
//! Two things live here, and they differ in one way that decides everything
//! else: a **schedule** fires on the clock and does not care what happened last
//! time, while a **goal** is pursued until it is satisfied and therefore has to
//! remember what it already tried.
//!
//! The policies are not preferences. Each one is here because a simulation over
//! a fake clock measured what happens without it
//! ([`research/scheduling-2026/out/design-sim.txt`]):
//!
//! - Without a **misfire** policy, six hours of downtime launched **73 runs in
//!   the first minute back**.
//! - Without an **overlap** policy, an hourly schedule whose runs take ninety
//!   minutes reached two concurrent runs and kept climbing.
//! - Without **backoff and a circuit breaker**, a schedule whose every run
//!   fails made **288 spawn attempts in 24 hours**.
//! - **Jitter, which sounds prudent, is off by default**: a 300 s spread
//!   against a 150 s grace window *lost 34 of 72 fires*, and operator
//!   predictability scored 5 → 3. It is the one addition that made things
//!   worse, so it defaults to nothing and is refused when it would exceed the
//!   grace window.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{JodError, Result};

/// What to do about fires that were missed while Jod was not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Misfire {
    /// Run once, now, however many were missed. The default, because the
    /// question a person asks after an outage is "did my inbox get triaged",
    /// not "did it get triaged eleven times".
    #[default]
    FireOnce,
    /// Pretend they did not happen and wait for the next scheduled instant.
    Skip,
    /// Replay every missed instant. Bounded — see [`Misfire::MAX_REPLAY`] —
    /// because the same outage replayed 72 fires unbounded.
    FireAll,
}

impl Misfire {
    /// The most instants `FireAll` will replay.
    ///
    /// A cap rather than a promise: an unbounded replay after a long outage is
    /// indistinguishable from a fork bomb, and nobody wants a week of nightly
    /// digests at once.
    pub const MAX_REPLAY: usize = 100;

    pub fn as_str(&self) -> &'static str {
        match self {
            Misfire::FireOnce => "fire_once",
            Misfire::Skip => "skip",
            Misfire::FireAll => "fire_all",
        }
    }
}

impl FromStr for Misfire {
    type Err = JodError;
    fn from_str(s: &str) -> Result<Misfire> {
        match s {
            "fire_once" => Ok(Misfire::FireOnce),
            "skip" => Ok(Misfire::Skip),
            "fire_all" => Ok(Misfire::FireAll),
            other => Err(JodError::Invalid(format!("unknown misfire policy {other}"))),
        }
    }
}

/// What to do when a schedule comes due while its previous run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    /// Leave the running one alone and record that this fire was skipped. The
    /// default: for a digest or a sweep, the next one will cover it anyway.
    #[default]
    Skip,
    /// Stop the running one and start a fresh run. For a job whose answer goes
    /// stale — a health check, a dashboard refresh.
    Replace,
    /// Let them run concurrently. Correct only when runs cannot interfere.
    Allow,
}

impl Overlap {
    pub fn as_str(&self) -> &'static str {
        match self {
            Overlap::Skip => "skip",
            Overlap::Replace => "replace",
            Overlap::Allow => "allow",
        }
    }
}

impl FromStr for Overlap {
    type Err = JodError;
    fn from_str(s: &str) -> Result<Overlap> {
        match s {
            "skip" => Ok(Overlap::Skip),
            "replace" => Ok(Overlap::Replace),
            "allow" => Ok(Overlap::Allow),
            other => Err(JodError::Invalid(format!("unknown overlap policy {other}"))),
        }
    }
}

/// Whether a schedule is firing, deliberately stopped, or broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    #[default]
    Armed,
    /// Stopped by a person. Never resumed automatically.
    Paused,
    /// Stopped by the circuit breaker after repeated failures. Distinguished
    /// from `Paused` because it says *why* it stopped, and because resuming it
    /// is a different decision.
    Broken,
}

impl ScheduleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScheduleState::Armed => "armed",
            ScheduleState::Paused => "paused",
            ScheduleState::Broken => "broken",
        }
    }

    pub fn parse(s: &str) -> ScheduleState {
        match s {
            "paused" => ScheduleState::Paused,
            "broken" => ScheduleState::Broken,
            _ => ScheduleState::Armed,
        }
    }
}

/// How many consecutive failures trip the breaker.
///
/// Measured: a schedule whose every run fails made 288 spawn attempts in 24
/// hours without one, and 5 with backoff and this breaker.
pub const BREAK_AFTER_FAILURES: i64 = 5;

/// One scheduled job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    /// What the agent is asked to do. Untouched by Jod — it goes to the harness
    /// as an argument, never through a shell.
    pub prompt: String,
    pub harness: String,
    pub cwd: String,
    pub model: Option<String>,
    /// A cron expression, as croner parses it.
    pub cron: String,
    /// An IANA zone *name*, never a captured offset. An offset is only correct
    /// until the next transition, and a schedule outlives transitions.
    pub timezone: String,
    pub state: ScheduleState,
    pub misfire: Misfire,
    pub overlap: Overlap,
    /// How late a fire may be and still count as that fire. Also the bound on
    /// what "missed" means after an outage.
    pub grace_ms: i64,
    /// Deliberately 0 by default. The one addition that measured worse.
    pub jitter_ms: i64,
    pub next_fire_at_ms: Option<i64>,
    pub last_fire_at_ms: Option<i64>,
    pub consecutive_failures: i64,
    pub created_at_ms: i64,
}

/// How a due schedule was resolved. Every outcome is written down, including
/// the ones where nothing ran — a skip nobody recorded is a silent failure, and
/// "it never fired" and "it fired and was skipped" are different bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireOutcome {
    /// A run was started.
    Ran,
    /// Skipped because the previous run was still going.
    SkippedOverlap,
    /// Skipped because it was missed while Jod was down and the policy said so.
    SkippedMisfire,
    /// The previous run was stopped to make room for this one.
    Replaced,
    /// Jod tried to start a run and could not.
    SpawnFailed,
    /// A claimant took the schedule and then died without recording anything.
    /// Written by whoever displaces the dead lease.
    Abandoned,
    /// A monitor ran, nothing it watches had changed, and so no agent was
    /// woken.
    ///
    /// This is a *success*: the schedule did its job for the price of a hash.
    /// It gets a row because the alternative is a schedule that shows no fires
    /// for a week, which is indistinguishable from one that is broken — and
    /// telling those apart is the entire reason this table exists.
    MonitorQuiet,
    /// Read back from a row this build does not understand.
    ///
    /// Only ever produced by parsing, never written. It exists because the
    /// fallback used to be `Ran`, which meant a future outcome added without a
    /// matching parse arm would read back out of the database as a successful
    /// run — the precise lie every other decision here is arranged to prevent.
    /// An honest "I do not know what this was" is always better.
    Unknown,
}

impl FireOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            FireOutcome::Ran => "ran",
            FireOutcome::SkippedOverlap => "skipped_overlap",
            FireOutcome::SkippedMisfire => "skipped_misfire",
            FireOutcome::Replaced => "replaced",
            FireOutcome::SpawnFailed => "spawn_failed",
            FireOutcome::Abandoned => "abandoned",
            FireOutcome::MonitorQuiet => "monitor_quiet",
            FireOutcome::Unknown => "unknown",
        }
    }

    /// Whether this outcome means an agent actually ran.
    ///
    /// `MonitorQuiet` is deliberately *not* a run: the point of a monitor is
    /// that nothing was spawned. Anything that counts quiet ticks as runs would
    /// report a watchdog as the busiest schedule on the box.
    pub fn started_a_run(&self) -> bool {
        matches!(self, FireOutcome::Ran | FireOutcome::Replaced)
    }
}

/// One recorded firing decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fire {
    pub id: i64,
    pub schedule_id: String,
    /// The instant this fire was *for*, which is not when it happened.
    pub due_at_ms: i64,
    pub fired_at_ms: i64,
    pub run_id: Option<String>,
    pub outcome: FireOutcome,
    pub detail: Option<String>,
}

/// The next instant a cron expression fires, at or after `after_ms`.
///
/// The zone is resolved from its IANA name on every call rather than cached as
/// an offset, which is what makes the two DST cases come out right.
pub fn next_fire(cron: &str, timezone: &str, after_ms: i64) -> Result<Option<i64>> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| JodError::Invalid(format!("unknown timezone {timezone}")))?;
    let pattern = croner::Cron::from_str(cron)
        .map_err(|e| JodError::Invalid(format!("{cron} is not a cron expression: {e}")))?;

    let after = chrono::DateTime::from_timestamp_millis(after_ms)
        .ok_or_else(|| JodError::Invalid(format!("{after_ms} is not a time")))?
        .with_timezone(&zone);

    Ok(pattern
        .find_next_occurrence(&after, false)
        .ok()
        // Truncated to the whole second. croner carries the sub-second part of
        // whatever it was asked from, so walking a series by stepping one
        // millisecond past each result accumulates drift — six hops in, the
        // instant is six milliseconds past the hour and no longer compares
        // equal to the hour it represents. Cron has no sub-second meaning, so
        // the fractional part is noise that only ever causes bugs.
        .map(|t| t.timestamp_millis().div_euclid(1000) * 1000))
}

/// Check a cron expression and zone without computing anything.
///
/// Used at creation time so a schedule that can never fire is refused when it
/// is written rather than discovered as silence weeks later.
/// An expression with no next occurrence is refused here rather than stored.
/// `next_fire` answers `Ok(None)` for a well formed expression that names a
/// date which never arrives, such as the 31st of February, so accepting every
/// `Ok` would arm a schedule that waits for ever and looks healthy doing it.
pub fn validate(cron: &str, timezone: &str) -> Result<()> {
    match next_fire(cron, timezone, chrono::Utc::now().timestamp_millis())? {
        Some(_) => Ok(()),
        None => Err(JodError::Invalid(format!(
            "{cron} never comes round in {timezone}, so a schedule on it would \
             sit armed and never fire. Check the day of the month against the \
             month — February has no 31st — and pick a date that exists."
        ))),
    }
}

/// Every instant a schedule should have fired in `(after_ms, until_ms]`.
///
/// Bounded by [`Misfire::MAX_REPLAY`] so a long outage cannot produce an
/// unbounded replay.
pub fn missed_since(
    cron: &str,
    timezone: &str,
    after_ms: i64,
    until_ms: i64,
) -> Result<Vec<i64>> {
    let mut found = Vec::new();
    let mut at = after_ms;
    while found.len() < Misfire::MAX_REPLAY {
        match next_fire(cron, timezone, at)? {
            Some(next) if next <= until_ms => {
                found.push(next);
                // Step past this instant, or a schedule firing on the boundary
                // would be found for ever.
                at = next + 1;
            }
            _ => break,
        }
    }
    Ok(found)
}

/// How long to wait before retrying a schedule that keeps failing.
///
/// Exponential on the consecutive-failure count, capped at an hour. The cap
/// matters more than the curve: without one, a schedule that broke on Friday is
/// still backed off on Monday.
pub fn backoff_ms(consecutive_failures: i64) -> i64 {
    const CAP: i64 = 3_600_000;
    if consecutive_failures <= 0 {
        return 0;
    }
    let doubled = 60_000i64.saturating_mul(1 << consecutive_failures.min(16));
    doubled.min(CAP)
}

/// What [`settle`] worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settlement {
    /// The schedule's consecutive-failure count after this tick.
    pub failures: i64,
    /// How many of the runs it was given, counting from the oldest, are now
    /// accounted for. The rest had not finished and belong to a later tick.
    pub settled: usize,
}

/// A schedule's failure count, once the runs it started have ended.
///
/// This exists because the failure that matters is not known at the moment the
/// tick lets the schedule go. Starting the harness process nearly always
/// succeeds; what fails is the harness itself a second later — a working
/// directory that has been deleted, an agent that crashes, a model that cannot
/// be reached — and the supervisor writes that into the run's own status long
/// after the tick has moved on. While only the synchronous spawn error was
/// counted, the breaker never tripped for the ordinary way a schedule breaks: a
/// schedule pointed at a deleted directory failed every run and sat at zero
/// failures, still `armed`, for ever.
///
/// `ended` is `runs.status` for each run this schedule started and has not yet
/// been judged on, oldest first. `spawn_failed` is the old signal, unchanged:
/// this tick could not start a run at all.
///
/// Two rules here matter more than the arithmetic:
///
/// - **A run that has not finished is not a failure.** The walk stops at the
///   first `running` row rather than guessing, so a long run that is going to
///   succeed cannot trip the breaker, and runs started after it are judged on a
///   later tick instead of out of order.
/// - **A tick that learned nothing resets the count to zero**, which is what
///   every release without a spawn error did before this function existed. The
///   one exception is a tick still waiting on a run it started: that leaves the
///   count where it was, so failures are not forgotten every time a slow run is
///   in flight.
pub fn settle(previous: i64, ended: &[&str], spawn_failed: bool) -> Settlement {
    let mut failures = previous;
    let mut settled = 0;
    let mut judged = false;
    let mut waiting = false;

    for status in ended {
        match *status {
            "running" => {
                waiting = true;
                break;
            }
            "failed" => {
                failures += 1;
                judged = true;
            }
            "completed" => {
                failures = 0;
                judged = true;
            }
            // `killed` — a run stopped by hand or displaced by the overlap
            // policy — and any status this build does not recognise say nothing
            // about whether the schedule works. Accounted for so the same row
            // is not read again, but not counted either way.
            _ => {}
        }
        settled += 1;
    }

    if spawn_failed {
        failures += 1;
        judged = true;
    }
    if !judged && !waiting {
        failures = 0;
    }
    Settlement { failures, settled }
}

// ---- goals --------------------------------------------------------------

/// Where a goal has got to.
///
/// A goal is the one thing here that can end without being told to. The states
/// that stop it — satisfied, stalled, exhausted — exist so that "still going"
/// always means something is actually happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    #[default]
    Running,
    /// Stopped by a person.
    Paused,
    /// Its own done-when check passed.
    Satisfied,
    /// Iterations kept completing but nothing moved. The most important
    /// terminal state: a loop that runs for ever making no progress is the
    /// characteristic failure of an autonomous agent, and it is invisible
    /// unless something counts.
    Stalled,
    /// Out of budget or out of iterations.
    Exhausted,
    /// Waiting for a person to answer something.
    Blocked,
}

impl GoalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalState::Running => "running",
            GoalState::Paused => "paused",
            GoalState::Satisfied => "satisfied",
            GoalState::Stalled => "stalled",
            GoalState::Exhausted => "exhausted",
            GoalState::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> GoalState {
        match s {
            "paused" => GoalState::Paused,
            "satisfied" => GoalState::Satisfied,
            "stalled" => GoalState::Stalled,
            "exhausted" => GoalState::Exhausted,
            "blocked" => GoalState::Blocked,
            _ => GoalState::Running,
        }
    }

    /// Whether this state still schedules iterations.
    pub fn is_live(&self) -> bool {
        matches!(self, GoalState::Running)
    }
}

/// A standing objective, pursued on a cadence until it is satisfied.
///
/// Its progress lives in the memory layer rather than in a column here: the
/// brief is a prospective fact superseded each iteration, so bitemporal
/// validity answers "what did it think it was doing last month"; what happened
/// each iteration is episodic, written in a `goal:<id>` scope so a high
/// frequency of writes cannot drown ordinary recall. Only the counters the
/// claim reads on every tick stay as columns — a claim must not depend on a
/// text index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub name: String,
    pub objective: String,
    /// The check that decides "done". Deterministic and run *before* anything
    /// is asked to judge progress, so a passing gate is evidence rather than an
    /// opinion.
    pub done_when: Option<String>,
    pub harness: String,
    pub cwd: String,
    pub model: Option<String>,
    pub cron: String,
    pub timezone: String,
    pub state: GoalState,
    pub iteration: i64,
    /// Stops that are checked before every iteration.
    pub max_iterations: Option<i64>,
    pub budget_usd: Option<f64>,
    pub spent_usd: f64,
    /// How many iterations may finish without progress before the goal is
    /// called stalled.
    pub stall_after: i64,
    pub no_progress: i64,
    pub next_fire_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

impl Goal {
    /// The scope a goal's episodic memory is written under.
    ///
    /// Its own partition, because a goal iterating hourly for a month writes
    /// far more than a person ever does, and scope is a hard filter — so those
    /// writes cannot crowd out ordinary recall.
    pub fn memory_scope(&self) -> String {
        format!("goal:{}", self.id)
    }

    /// Why this goal should stop, if it should.
    ///
    /// Checked before an iteration is started rather than after, so an
    /// exhausted goal never spends the budget that proves it is exhausted.
    pub fn should_stop(&self) -> Option<GoalState> {
        if let Some(max) = self.max_iterations {
            if self.iteration >= max {
                return Some(GoalState::Exhausted);
            }
        }
        if let Some(budget) = self.budget_usd {
            if self.spent_usd >= budget {
                return Some(GoalState::Exhausted);
            }
        }
        if self.no_progress >= self.stall_after {
            return Some(GoalState::Stalled);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    }

    fn at(ms: i64, zone: &str) -> String {
        let tz: chrono_tz::Tz = zone.parse().unwrap();
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .with_timezone(&tz)
            .format("%Y-%m-%d %H:%M %Z")
            .to_string()
    }

    /// croner returns an instant carrying the sub-second part of its input, so
    /// a series walked by stepping past each result drifts. Nothing above the
    /// seam may ever see that.
    #[test]
    fn every_fire_lands_exactly_on_a_whole_second() {
        let mut cursor = ms("2026-08-10T00:00:00Z");
        for _ in 0..8 {
            let next = next_fire("0 * * * *", "UTC", cursor).unwrap().unwrap();
            assert_eq!(next % 1000, 0, "{next} is not a whole second");
            cursor = next + 1;
        }
    }

    #[test]
    fn a_daily_schedule_fires_at_the_same_local_time_each_day() {
        let start = ms("2026-08-10T12:00:00Z");
        let next = next_fire("0 2 * * *", "UTC", start).unwrap().unwrap();
        assert_eq!(at(next, "UTC"), "2026-08-11 02:00 UTC");
    }

    /// The whole reason a zone *name* is stored rather than an offset: the
    /// local time must stay put across a transition, which an offset cannot do.
    #[test]
    fn a_local_schedule_keeps_its_local_time_across_a_dst_transition() {
        // Afternoon of the day before the clocks go back, so the next 09:00 is
        // the morning *after* the transition.
        let before = ms("2026-10-31T18:00:00Z"); // 14:00 EDT
        let next = next_fire("0 9 * * *", "America/New_York", before)
            .unwrap()
            .unwrap();
        // Still 09:00 local, and now on the other side of the offset change —
        // which a stored offset could not have done.
        assert_eq!(at(next, "America/New_York"), "2026-11-01 09:00 EST");
    }

    /// 2026-03-08 has no 02:30 in New York. The measured crates split three
    /// ways here; croner runs at 03:00 rather than losing the day.
    #[test]
    fn a_schedule_in_the_spring_forward_gap_still_runs_that_day() {
        let before = ms("2026-03-07T12:00:00-05:00");
        let next = next_fire("30 2 * * *", "America/New_York", before)
            .unwrap()
            .unwrap();
        let shown = at(next, "America/New_York");
        assert!(
            shown.starts_with("2026-03-08"),
            "the day must not vanish: {shown}"
        );
    }

    /// The fall-back case, where two crates fire a daily job *twice*.
    #[test]
    fn a_schedule_across_fall_back_fires_once_not_twice() {
        let before = ms("2026-10-31T12:00:00-04:00");
        let first = next_fire("30 1 * * *", "America/New_York", before)
            .unwrap()
            .unwrap();
        let second = next_fire("30 1 * * *", "America/New_York", first + 1)
            .unwrap()
            .unwrap();
        assert!(at(first, "America/New_York").starts_with("2026-11-01"));
        assert!(
            at(second, "America/New_York").starts_with("2026-11-02"),
            "the repeated local hour must not fire again: {}",
            at(second, "America/New_York")
        );
    }

    #[test]
    fn a_nonsense_expression_is_refused_when_it_is_written() {
        assert!(validate("not a cron", "UTC").is_err());
        assert!(validate("0 2 * * *", "Mars/Olympus").is_err());
        assert!(validate("0 2 * * *", "Asia/Manila").is_ok());
    }

    /// A cron expression can be perfectly well formed and still name a date
    /// that never arrives. February has no 31st and April has no 31st either,
    /// so a schedule on one of those expressions sits armed for ever. This is
    /// the case `validate` exists to catch, and catching it means looking at
    /// whether an occurrence was actually found rather than only at whether
    /// the search came back without an error.
    #[test]
    fn an_expression_that_names_a_date_that_never_comes_is_refused() {
        for impossible in ["0 0 31 2 *", "0 0 30 2 *", "0 0 31 4 *", "0 0 31 11 *"] {
            assert!(
                validate(impossible, "UTC").is_err(),
                "{impossible} names a date that never comes and should have been refused"
            );
        }
        // Asserted alongside, so the refusal cannot pass by turning everything
        // away. February 29th is the interesting one: it is rare, not
        // impossible, and it has to keep working.
        for real in ["0 9 * * *", "0 0 29 2 *", "0 0 31 1 *", "@daily"] {
            assert!(
                validate(real, "UTC").is_ok(),
                "{real} does come round and should have been accepted"
            );
        }
    }

    #[test]
    fn a_shorthand_expression_is_accepted() {
        assert!(validate("@daily", "UTC").is_ok());
        assert!(validate("@hourly", "UTC").is_ok());
    }

    /// Six hours down launched 73 runs in the first minute back when nothing
    /// counted what was missed.
    #[test]
    fn every_instant_missed_during_an_outage_is_counted() {
        let down_at = ms("2026-08-10T00:00:00Z");
        let back_at = ms("2026-08-10T06:00:00Z");
        let missed = missed_since("0 * * * *", "UTC", down_at, back_at).unwrap();
        assert_eq!(missed.len(), 6, "one per hour, including the hour it returned");
    }

    /// An unbounded replay after a long outage is indistinguishable from a fork
    /// bomb.
    #[test]
    fn replaying_a_long_outage_is_bounded() {
        let down_at = ms("2026-01-01T00:00:00Z");
        let back_at = ms("2026-08-10T00:00:00Z");
        let missed = missed_since("* * * * *", "UTC", down_at, back_at).unwrap();
        assert_eq!(missed.len(), Misfire::MAX_REPLAY);
    }

    #[test]
    fn nothing_is_missed_when_nothing_was_due() {
        let from = ms("2026-08-10T02:01:00Z");
        let to = ms("2026-08-10T02:59:00Z");
        assert!(missed_since("0 2 * * *", "UTC", from, to).unwrap().is_empty());
    }

    /// Without a cap, a schedule that broke on Friday is still backed off on
    /// Monday.
    #[test]
    fn backoff_grows_and_then_stops_growing() {
        assert_eq!(backoff_ms(0), 0);
        assert!(backoff_ms(1) < backoff_ms(3));
        assert_eq!(backoff_ms(50), 3_600_000, "capped at an hour");
        assert_eq!(backoff_ms(-1), 0, "a negative count is not a delay");
    }

    /// The failure this whole breaker exists for, and the one it used to miss:
    /// the process started, and the harness inside it died a moment later.
    #[test]
    fn a_run_that_started_and_then_failed_counts_as_a_failure() {
        assert_eq!(settle(0, &["failed"], false).failures, 1);
        assert_eq!(settle(4, &["failed"], false).failures, 5);
    }

    #[test]
    fn a_run_that_finished_clears_the_count() {
        assert_eq!(settle(4, &["completed"], false).failures, 0);
    }

    /// A schedule whose runs take longer than its own period must not break
    /// simply for being slow.
    #[test]
    fn a_run_that_has_not_finished_is_not_a_failure() {
        let s = settle(0, &["running"], false);
        assert_eq!(s.failures, 0);
        assert_eq!(s.settled, 0, "and it is judged again on a later tick");
    }

    /// Waiting on a run is not evidence that the last one worked. Forgetting
    /// the count here would leave a schedule that fails slowly stuck at zero for
    /// ever, which is the bug this function was written for.
    #[test]
    fn waiting_on_a_run_keeps_the_failures_already_counted() {
        assert_eq!(settle(3, &["running"], false).failures, 3);
    }

    /// Nothing to go on means nothing held against the schedule — the old
    /// behaviour of every release that reported no spawn error.
    #[test]
    fn a_tick_that_learned_nothing_resets_the_count() {
        assert_eq!(settle(3, &[], false).failures, 0);
    }

    /// Runs are judged in the order they happened, so the newest outcome is the
    /// one the schedule ends up wearing.
    #[test]
    fn a_run_of_failures_ending_in_a_success_leaves_nothing_behind() {
        assert_eq!(settle(0, &["failed", "failed", "completed"], false).failures, 0);
        assert_eq!(settle(0, &["completed", "failed", "failed"], false).failures, 2);
    }

    /// The walk stops at the first unfinished run rather than reading past it,
    /// because "run 3 failed" says nothing while run 2 is still going.
    #[test]
    fn nothing_after_an_unfinished_run_is_read() {
        let s = settle(0, &["failed", "running", "failed"], false);
        assert_eq!(s.failures, 1);
        assert_eq!(s.settled, 1);
    }

    /// A run somebody stopped, and a status from a newer build, are neither a
    /// failure nor a success. They are accounted for so they are not read
    /// twice.
    #[test]
    fn a_stopped_run_is_accounted_for_without_being_judged() {
        for status in ["killed", "something_new"] {
            let s = settle(2, &[status], false);
            assert_eq!(s.failures, 0, "{status} says nothing either way");
            assert_eq!(s.settled, 1);
        }
    }

    /// The failure that was always counted still is, and exactly once.
    #[test]
    fn a_spawn_that_failed_still_counts_on_its_own_tick() {
        assert_eq!(settle(1, &[], true).failures, 2);
        assert_eq!(settle(1, &["failed"], true).failures, 3);
    }

    #[test]
    fn every_policy_survives_a_round_trip_through_text() {
        for m in [Misfire::FireOnce, Misfire::Skip, Misfire::FireAll] {
            assert_eq!(m.as_str().parse::<Misfire>().unwrap(), m);
        }
        for o in [Overlap::Skip, Overlap::Replace, Overlap::Allow] {
            assert_eq!(o.as_str().parse::<Overlap>().unwrap(), o);
        }
        assert!("wibble".parse::<Misfire>().is_err());
        assert!("wibble".parse::<Overlap>().is_err());
    }

    #[test]
    fn an_unknown_state_reads_as_the_live_one_rather_than_failing() {
        assert_eq!(ScheduleState::parse("armed"), ScheduleState::Armed);
        assert_eq!(ScheduleState::parse("paused"), ScheduleState::Paused);
        assert_eq!(ScheduleState::parse("broken"), ScheduleState::Broken);
        assert_eq!(ScheduleState::parse("nonsense"), ScheduleState::Armed);
    }

    // ---- goals ----

    fn goal() -> Goal {
        Goal {
            id: "g1".into(),
            name: "inbox-to-zero".into(),
            objective: "keep the inbox empty".into(),
            done_when: None,
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 * * * *".into(),
            timezone: "UTC".into(),
            state: GoalState::Running,
            iteration: 0,
            max_iterations: None,
            budget_usd: None,
            spent_usd: 0.0,
            stall_after: 6,
            no_progress: 0,
            next_fire_at_ms: None,
            created_at_ms: 0,
        }
    }

    #[test]
    fn a_goal_with_room_left_keeps_going() {
        assert_eq!(goal().should_stop(), None);
    }

    /// The characteristic failure of an autonomous loop: it keeps completing
    /// iterations and nothing changes. Invisible unless something counts.
    #[test]
    fn a_goal_that_stops_making_progress_is_called_stalled() {
        let mut g = goal();
        g.no_progress = 6;
        assert_eq!(g.should_stop(), Some(GoalState::Stalled));
    }

    #[test]
    fn a_goal_stops_when_its_budget_is_spent() {
        let mut g = goal();
        g.budget_usd = Some(25.0);
        g.spent_usd = 25.0;
        assert_eq!(g.should_stop(), Some(GoalState::Exhausted));
    }

    #[test]
    fn a_goal_stops_after_the_iterations_it_was_given() {
        let mut g = goal();
        g.max_iterations = Some(10);
        g.iteration = 10;
        assert_eq!(g.should_stop(), Some(GoalState::Exhausted));
    }

    /// A goal iterating hourly for a month writes far more than a person does.
    /// Scope is a hard filter, so its own partition is what stops those writes
    /// crowding out ordinary recall.
    #[test]
    fn a_goals_episodic_memory_lives_in_its_own_scope() {
        assert_eq!(goal().memory_scope(), "goal:g1");
    }

    #[test]
    fn only_a_running_goal_schedules_iterations() {
        assert!(GoalState::Running.is_live());
        for done in [
            GoalState::Paused,
            GoalState::Satisfied,
            GoalState::Stalled,
            GoalState::Exhausted,
            GoalState::Blocked,
        ] {
            assert!(!done.is_live(), "{done:?} must not keep firing");
        }
    }
}
