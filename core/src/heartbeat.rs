//! Liveness for a run that is supposed to take hours.
//!
//! Schema is migration `0013_heartbeats` in [`crate::store`].
//!
//! [`crate::proc::group_alive`] already answers *is there a process*. What it
//! cannot answer is **is that process still working**, and the two come apart
//! in exactly the case this exists for: a harness blocked on a socket that will
//! never answer, a model call retrying for ever. The group is alive and nothing
//! has happened for two hours.
//!
//! The gap was not theoretical. [`crate::ticker::Ticker::tick_goals`] settles
//! the previous iteration by asking the run's status, and a wedged run reads
//! `running` for ever — the only writer of a terminal status is the supervisor
//! watching a harness that never exits. So the goal waits for ever, silently.
//!
//! ## Two questions, both required
//!
//! - **Is the process there?** `kill(pgid, 0)` — cheap, certain, and passed by
//!   every wedged run in existence.
//! - **Is it producing events?** The high-water `seq` in `events`, against what
//!   the last beat saw. A harness that is working writes.
//!
//! Either alone misses half the failures. The second measures whether the agent
//! is doing anything *at all*, not anything useful — judging usefulness is what
//! a goal's `done_when` is for. This is the floor beneath it.
//!
//! ## What is pure, and why it matters
//!
//! [`decide`] maps a stored heartbeat and three [`Observed`] facts to one
//! [`Verdict`], with no clock, database or process — so a stall exactly on the
//! boundary, a run that finished in the same tick, and a clock that went
//! backwards are all tested from values.
//! [`crate::ticker::Ticker::tick_heartbeats`] gathers the facts and holds no
//! policy.
//!
//! ## Cleanup is the schema's job, not a code path
//!
//! A heartbeat must go when its session is deleted, fails, or finishes. Two are
//! enforced by the table rather than by remembering to call something:
//! `ON DELETE CASCADE` covers deletion, and [`Verdict::Ended`] — which
//! [`decide`] reaches first — drops the row on a clean ending.
//!
//! Only the third needs code, because a wedged run never reports its own
//! ending. That asymmetry is the design.
//!
//! A retired heartbeat is *deleted*, not parked in a terminal state; a table of
//! tombstones is not cleanup. The reason is written to the memory layer on the
//! way out.
//!
//! **Not to the run's event stream, and that is not an oversight.** The
//! supervisor allocates `seq` from a counter in its own memory, and `events` is
//! `UNIQUE(run_id, seq)` with duplicates ignored — so a watchdog writing
//! `last_seq + 1` races the one process that owns that sequence, and loses
//! *silently*, costing either its explanation or the supervisor's `Finished`.
//! For a stalled run those writes are near-simultaneous by construction. A fact
//! has no such contention, and for a goal it lands in the scope the *next
//! iteration's prompt is built from*.

use serde::{Deserialize, Serialize};

/// How long a run may produce no events before it is declared stalled.
///
/// Twenty minutes, and deliberately generous. The two errors are not
/// symmetric: killing a run that was merely slow destroys work a person waited
/// on, while noticing a wedged run twenty minutes late costs twenty minutes of
/// an idle process. A build, a long test suite, or a model thinking hard about
/// a large diff can all be silent for a while; nothing legitimate is silent for
/// twenty minutes and then continues.
pub const DEFAULT_STALL_MS: i64 = 20 * 60 * 1000;

/// The ceiling a goal's iteration gets, when it does not name its own.
///
/// A goal fires on a cron and its iterations are meant to be increments —
/// "do the next thing and stop". One that has been going six hours is not an
/// increment, whatever its event stream says, and letting it run means the
/// goal's own cadence has quietly stopped meaning anything.
pub const GOAL_MAX_LIFETIME_MS: i64 = 6 * 60 * 60 * 1000;

/// The floor under a configured stall window.
///
/// A window shorter than the tick is a promise the scheduler cannot keep: the
/// sweep runs every [`crate::ticker::TICK`], so anything below it is rounded up
/// to the tick by physics. Refusing it at the edge is better than accepting a
/// number and quietly meaning a different one.
pub const MIN_STALL_MS: i64 = 60 * 1000;

/// Why a heartbeat is being kept.
///
/// Carried so that a sweep's verdict can be reported against the thing a person
/// actually named — "goal `green-ci` stalled", not "run 7f3a stalled".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Watching {
    /// An iteration of a standing objective. Registered automatically, which is
    /// the charter's "activate this if the session has a goal".
    Goal(String),
    /// A long-running delegation somebody asked to be watched.
    Run,
}

impl Watching {
    pub fn goal_name(&self) -> Option<&str> {
        match self {
            Watching::Goal(name) => Some(name),
            Watching::Run => None,
        }
    }

    /// Rebuild from the nullable column that stores it.
    pub fn from_goal(name: Option<String>) -> Watching {
        match name {
            Some(n) => Watching::Goal(n),
            None => Watching::Run,
        }
    }
}

/// The stored liveness record for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub run_id: String,
    pub watching: Watching,
    pub started_at_ms: i64,
    /// Silence longer than this means stalled. See [`DEFAULT_STALL_MS`].
    pub stall_ms: i64,
    /// A hard ceiling on the whole run, or `None` for "as long as it takes".
    ///
    /// `None` is the default for a delegation, because "this task is long" is
    /// the entire premise. A goal iteration gets [`GOAL_MAX_LIFETIME_MS`].
    pub max_lifetime_ms: Option<i64>,
    /// The highest event `seq` the last beat saw. `-1` before the first beat,
    /// which is distinguishable from seq 0 — a run whose very first event
    /// arrives between registration and the first sweep has made progress, and
    /// starting at 0 would score that as silence.
    pub last_seq: i64,
    /// When this run last produced an event. The stall window is measured from
    /// here, never from the last beat: sweeps happen on the scheduler's clock
    /// and say nothing about the run.
    pub last_progress_ms: i64,
    pub last_beat_ms: i64,
    pub beats: i64,
}

impl Heartbeat {
    /// A heartbeat for a run starting now, with the defaults for its kind.
    pub fn starting(run_id: impl Into<String>, watching: Watching, now_ms: i64) -> Heartbeat {
        let max_lifetime_ms = match watching {
            Watching::Goal(_) => Some(GOAL_MAX_LIFETIME_MS),
            Watching::Run => None,
        };
        Heartbeat {
            run_id: run_id.into(),
            watching,
            started_at_ms: now_ms,
            stall_ms: DEFAULT_STALL_MS,
            max_lifetime_ms,
            last_seq: -1,
            // Not zero, and not "unknown": a run that has just started has by
            // definition not been silent yet, so its first stall window runs
            // from its launch. Leaving this at zero would make every run stall
            // on its first sweep, fifty-six years of silence having elapsed
            // since the epoch.
            last_progress_ms: now_ms,
            last_beat_ms: now_ms,
            beats: 0,
        }
    }

    /// Override the silence window. Refuses anything below [`MIN_STALL_MS`].
    pub fn with_stall_ms(mut self, stall_ms: i64) -> Heartbeat {
        self.stall_ms = stall_ms.max(MIN_STALL_MS);
        self
    }

    /// Override the ceiling. `None` means the run may take as long as it takes.
    pub fn with_max_lifetime_ms(mut self, max: Option<i64>) -> Heartbeat {
        self.max_lifetime_ms = max;
        self
    }

    /// How long this run has been silent as of `now_ms`.
    pub fn silence_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.last_progress_ms).max(0)
    }
}

/// What the sweep found out about a run, before deciding anything.
///
/// Separated from [`decide`] so that every branch below is reachable from a
/// literal in a test. Produced by [`Beat::observe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The run's recorded status, or `None` if the row is gone.
    pub status: Option<String>,
    /// Whether the run's process group still exists.
    ///
    /// Meaningful only while `status` is `running`: pids are recycled, and
    /// probing a finished run's long-dead pgid is how a stranger's process gets
    /// mistaken for an agent. [`decide`] therefore reads `status` first and
    /// never consults this for a run that has already ended.
    pub alive: bool,
    /// The highest event `seq` written for this run, or `-1` for none yet.
    pub last_seq: i64,
}

/// One sweep's conclusion about one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// New events since the last beat. The run is working; record and move on.
    Beating { seq: i64 },
    /// No new events, but inside the silence window. Nothing is wrong yet.
    Quiet { silence_ms: i64 },
    /// Alive, and silent for longer than the window allows. Stop it.
    Stalled { silence_ms: i64 },
    /// The process group is gone while the run still claims to be running. The
    /// supervisor died without writing a status — nothing to kill, but the run
    /// is lying and must be corrected.
    Vanished,
    /// Outlived its ceiling. Stop it, whatever it is doing.
    Expired { lifetime_ms: i64 },
    /// The run reached a terminal status, or its row is gone. The heartbeat has
    /// done its job and is dropped.
    Ended,
}

impl Verdict {
    /// Whether carrying this out means signalling the run's process group.
    ///
    /// [`Verdict::Vanished`] is deliberately not in this list: there is no
    /// group left to signal, and `kill(-pgid, …)` against a recycled pgid would
    /// reach whatever now holds that number.
    pub fn terminates(&self) -> bool {
        matches!(self, Verdict::Stalled { .. } | Verdict::Expired { .. })
    }

    /// Whether the run's status should be corrected to `failed`.
    ///
    /// A stalled or vanished run did not complete, and must not read as though
    /// it did — the charter's "blocked is a legal ending" is about work a person
    /// can see, not about a row that quietly stays `running` for ever.
    pub fn fails_the_run(&self) -> bool {
        matches!(
            self,
            Verdict::Stalled { .. } | Verdict::Expired { .. } | Verdict::Vanished
        )
    }

    /// Whether the heartbeat row should be removed after this verdict.
    ///
    /// Everything except the two "still going" answers: once a run has stalled,
    /// expired, vanished or ended there is nothing further to watch, and a row
    /// left behind would be swept for ever against a pgid that no longer means
    /// anything.
    pub fn retires(&self) -> bool {
        !matches!(self, Verdict::Beating { .. } | Verdict::Quiet { .. })
    }

    /// The predicate the sweep records this verdict under.
    pub fn tag(&self) -> &'static str {
        match self {
            Verdict::Beating { .. } => "beating",
            Verdict::Quiet { .. } => "quiet",
            Verdict::Stalled { .. } => "stalled",
            Verdict::Vanished => "vanished",
            Verdict::Expired { .. } => "expired",
            Verdict::Ended => "ended",
        }
    }

    /// One line for the run's event stream and for `jod heartbeats`.
    pub fn detail(&self) -> String {
        match self {
            Verdict::Beating { seq } => format!("working (event {seq})"),
            Verdict::Quiet { silence_ms } => format!("quiet for {}", human_ms(*silence_ms)),
            Verdict::Stalled { silence_ms } => {
                format!("stalled — no output for {}", human_ms(*silence_ms))
            }
            Verdict::Vanished => "process group gone while still marked running".to_string(),
            Verdict::Expired { lifetime_ms } => {
                format!("ran past its ceiling after {}", human_ms(*lifetime_ms))
            }
            Verdict::Ended => "run finished".to_string(),
        }
    }
}

/// A duration a person reads, not a number of milliseconds.
fn human_ms(ms: i64) -> String {
    let secs = ms / 1000;
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

/// What to do about one run, from what is stored and what was observed.
///
/// The order of these checks is load-bearing and each one is here because
/// putting it later is wrong:
///
/// 1. **Ended first.** A run that finished normally in the same tick it would
///    have been declared stalled must read as finished. Checking silence first
///    would `SIGTERM` a process group that has already exited — and, given pid
///    recycling, possibly one that now belongs to somebody else.
/// 2. **Vanished before the clocks.** A dead group cannot be terminated and its
///    lifetime no longer means anything; the only useful thing left to say is
///    that the run's status is wrong.
/// 3. **Progress before staleness.** A run that produced an event this tick is
///    working, even if it is also past a ceiling somebody set optimistically —
///    no, it is not: see 4.
/// 4. **The ceiling beats progress.** A run wedged in a retry loop produces
///    events for ever, so a ceiling that yielded to progress would never fire.
///    That is exactly the failure a ceiling is for, so `Expired` is checked
///    before `Beating`.
pub fn decide(hb: &Heartbeat, obs: &Observed, now_ms: i64) -> Verdict {
    // 1. Terminal, or gone. Either way there is nothing left to watch.
    match obs.status.as_deref() {
        None => return Verdict::Ended,
        Some("running") => {}
        Some(_) => return Verdict::Ended,
    }

    // 2. Still marked running, but there is no process. The supervisor died
    //    without recording an ending, which is precisely the case that leaves a
    //    run listed as working for ever.
    if !obs.alive {
        return Verdict::Vanished;
    }

    // 3. The ceiling, ahead of progress — a run stuck retrying is busy, and a
    //    ceiling that busy work could postpone would never be reached.
    let lifetime_ms = now_ms.saturating_sub(hb.started_at_ms).max(0);
    if let Some(max) = hb.max_lifetime_ms {
        if lifetime_ms >= max {
            return Verdict::Expired { lifetime_ms };
        }
    }

    // 4. Did anything happen? Strictly greater: an unchanged high-water mark is
    //    silence, and `>=` would score every sweep of an idle run as progress.
    if obs.last_seq > hb.last_seq {
        return Verdict::Beating { seq: obs.last_seq };
    }

    // 5. Silent — for how long? Measured from the last *event*, never from the
    //    last sweep, so a scheduler that was itself down does not reset the
    //    window and hide a run that stalled during the outage.
    let silence_ms = hb.silence_ms(now_ms);
    if silence_ms >= hb.stall_ms {
        return Verdict::Stalled { silence_ms }
    }
    Verdict::Quiet { silence_ms }
}

/// The record a sweep writes back after a verdict that leaves the run going.
///
/// Split out so the store's update is a value rather than a pile of arguments,
/// and so "a quiet beat does not move `last_progress_ms`" is a property of a
/// function rather than of whichever caller remembered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub run_id: String,
    pub last_seq: i64,
    pub last_progress_ms: i64,
    pub last_beat_ms: i64,
}

impl Beat {
    /// Fold a verdict into the heartbeat it came from.
    ///
    /// `last_progress_ms` advances only on [`Verdict::Beating`]. A quiet sweep
    /// still records that the sweep happened, because "the scheduler has not
    /// looked at this run since Tuesday" and "this run has been silent since
    /// Tuesday" are different problems, and one column cannot answer both.
    pub fn after(hb: &Heartbeat, verdict: &Verdict, now_ms: i64) -> Beat {
        match verdict {
            Verdict::Beating { seq } => Beat {
                run_id: hb.run_id.clone(),
                last_seq: *seq,
                last_progress_ms: now_ms,
                last_beat_ms: now_ms,
            },
            _ => Beat {
                run_id: hb.run_id.clone(),
                last_seq: hb.last_seq,
                last_progress_ms: hb.last_progress_ms,
                last_beat_ms: now_ms,
            },
        }
    }
}

/// What one sweep did, for the tick's report and for the daemon's log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Runs looked at.
    pub checked: usize,
    /// Runs that had produced something since the last sweep.
    pub beating: usize,
    /// Runs stopped: stalled or expired.
    pub stopped: usize,
    /// Heartbeats retired, for any reason.
    pub retired: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb(now: i64) -> Heartbeat {
        Heartbeat::starting("run-1", Watching::Run, now)
    }

    fn running(last_seq: i64) -> Observed {
        Observed {
            status: Some("running".into()),
            alive: true,
            last_seq,
        }
    }

    #[test]
    fn a_run_producing_events_is_working() {
        let h = hb(0);
        assert_eq!(
            decide(&h, &running(0), 1_000),
            Verdict::Beating { seq: 0 },
            "seq 0 is a real event; a heartbeat starting at 0 would miss it"
        );
    }

    /// The distinction the whole module exists for: alive is not working.
    #[test]
    fn a_process_that_is_alive_but_silent_past_the_window_is_stalled() {
        let h = hb(0);
        let silent = Observed {
            status: Some("running".into()),
            alive: true,
            last_seq: -1,
        };
        let verdict = decide(&h, &silent, DEFAULT_STALL_MS);
        assert_eq!(
            verdict,
            Verdict::Stalled {
                silence_ms: DEFAULT_STALL_MS
            }
        );
        assert!(verdict.terminates(), "a stalled run must actually be stopped");
        assert!(verdict.fails_the_run(), "it did not complete");
    }

    #[test]
    fn silence_inside_the_window_is_not_a_failure() {
        let h = hb(0);
        let quiet = decide(&h, &running(-1), DEFAULT_STALL_MS - 1);
        assert_eq!(
            quiet,
            Verdict::Quiet {
                silence_ms: DEFAULT_STALL_MS - 1
            }
        );
        assert!(!quiet.terminates());
        assert!(!quiet.retires(), "a quiet run is still being watched");
    }

    /// Ordering check 1. Getting this wrong signals a group that has exited —
    /// and pids are recycled, so it can reach a stranger's process.
    #[test]
    fn a_run_that_finished_in_the_same_tick_it_would_have_stalled_reads_as_finished() {
        let h = hb(0);
        let done = Observed {
            status: Some("completed".into()),
            alive: false,
            last_seq: -1,
        };
        let verdict = decide(&h, &done, DEFAULT_STALL_MS * 10);
        assert_eq!(verdict, Verdict::Ended);
        assert!(!verdict.terminates(), "nothing may be signalled after an exit");
        assert!(!verdict.fails_the_run(), "a completed run is not a failure");
        assert!(verdict.retires(), "and the heartbeat is cleaned up");
    }

    /// The charter's "clean up when the session is deleted". The cascade does
    /// the deleting; this is the case where the row outlives its run anyway.
    #[test]
    fn a_heartbeat_whose_run_row_is_gone_retires_rather_than_probing() {
        let h = hb(0);
        let orphan = Observed {
            status: None,
            alive: false,
            last_seq: -1,
        };
        let verdict = decide(&h, &orphan, 1_000);
        assert_eq!(verdict, Verdict::Ended);
        assert!(verdict.retires());
    }

    #[test]
    fn a_run_whose_supervisor_died_is_corrected_but_not_signalled() {
        let h = hb(0);
        let gone = Observed {
            status: Some("running".into()),
            alive: false,
            last_seq: 4,
        };
        let verdict = decide(&h, &gone, 1_000);
        assert_eq!(verdict, Verdict::Vanished);
        assert!(
            !verdict.terminates(),
            "there is no group left, and the pgid may since belong to somebody else"
        );
        assert!(verdict.fails_the_run(), "but it must stop claiming to run");
    }

    /// Ordering check 4, and the reason a ceiling exists at all: a run wedged in
    /// a retry loop is *busy*, so progress can never be allowed to postpone it.
    #[test]
    fn a_ceiling_beats_progress_or_a_retry_loop_would_run_for_ever() {
        let h = hb(0).with_max_lifetime_ms(Some(1_000));
        let mut busy = running(99);
        busy.last_seq = 99;
        let verdict = decide(&h, &busy, 1_000);
        assert_eq!(verdict, Verdict::Expired { lifetime_ms: 1_000 });
        assert!(verdict.terminates());
    }

    #[test]
    fn a_run_with_no_ceiling_may_take_as_long_as_it_takes() {
        let h = hb(0);
        assert_eq!(h.max_lifetime_ms, None, "long-running is the premise");
        let week = 7 * 24 * 60 * 60 * 1000;
        assert_eq!(decide(&h, &running(5), week), Verdict::Beating { seq: 5 });
    }

    #[test]
    fn a_goal_iteration_gets_a_ceiling_because_an_increment_is_not_six_hours() {
        let g = Heartbeat::starting("r", Watching::Goal("green-ci".into()), 0);
        assert_eq!(g.max_lifetime_ms, Some(GOAL_MAX_LIFETIME_MS));
        assert_eq!(g.watching.goal_name(), Some("green-ci"));
    }

    /// A window shorter than the tick is a promise the scheduler cannot keep.
    #[test]
    fn a_stall_window_below_the_tick_is_raised_rather_than_quietly_meaning_something_else() {
        assert_eq!(hb(0).with_stall_ms(1).stall_ms, MIN_STALL_MS);
        assert_eq!(hb(0).with_stall_ms(0).stall_ms, MIN_STALL_MS);
        assert_eq!(hb(0).with_stall_ms(-5).stall_ms, MIN_STALL_MS);
    }

    /// The window is measured from the last event, not the last sweep, so a
    /// scheduler outage cannot launder a run that stalled during it.
    #[test]
    fn a_scheduler_outage_does_not_reset_the_silence_window() {
        let mut h = hb(0);
        // Last event long ago; last sweep also long ago, because nothing was
        // running to sweep. A window measured from the sweep would read zero.
        h.last_progress_ms = 0;
        h.last_beat_ms = 0;
        let after_outage = DEFAULT_STALL_MS + 60_000;
        assert!(matches!(
            decide(&h, &running(-1), after_outage),
            Verdict::Stalled { .. }
        ));
    }

    #[test]
    fn a_quiet_beat_records_the_sweep_without_inventing_progress() {
        let h = hb(0);
        let beat = Beat::after(&h, &Verdict::Quiet { silence_ms: 5 }, 9_000);
        assert_eq!(beat.last_progress_ms, 0, "nothing happened, so nothing moved");
        assert_eq!(beat.last_beat_ms, 9_000, "but the sweep did happen");
        assert_eq!(beat.last_seq, h.last_seq);
    }

    #[test]
    fn a_beating_sweep_advances_both_the_cursor_and_the_clock() {
        let h = hb(0);
        let beat = Beat::after(&h, &Verdict::Beating { seq: 7 }, 9_000);
        assert_eq!(beat.last_seq, 7);
        assert_eq!(beat.last_progress_ms, 9_000);
    }

    /// Exactly on the boundary counts. A window of "20 minutes" that fires at
    /// 20 minutes and one millisecond is a different window.
    #[test]
    fn the_stall_boundary_is_inclusive() {
        let h = hb(0);
        assert!(matches!(
            decide(&h, &running(-1), DEFAULT_STALL_MS),
            Verdict::Stalled { .. }
        ));
        assert!(matches!(
            decide(&h, &running(-1), DEFAULT_STALL_MS - 1),
            Verdict::Quiet { .. }
        ));
    }

    /// A clock that went backwards — an NTP correction, a restored snapshot —
    /// must not read as an enormous silence and kill every live run.
    #[test]
    fn a_clock_that_went_backwards_does_not_stall_everything() {
        let mut h = hb(10_000_000);
        h.last_progress_ms = 10_000_000;
        assert!(matches!(
            decide(&h, &running(-1), 5_000_000),
            Verdict::Quiet { silence_ms: 0 }
        ));
    }

    /// Every verdict that stops watching must be able to say why. A heartbeat
    /// is deleted on retirement, so a verdict with nothing to record would take
    /// the reason a run died away with the row.
    #[test]
    fn every_retiring_verdict_carries_a_tag_and_a_readable_reason() {
        for v in [
            Verdict::Stalled { silence_ms: 1 },
            Verdict::Expired { lifetime_ms: 1 },
            Verdict::Vanished,
            Verdict::Ended,
        ] {
            assert!(v.retires());
            assert!(!v.tag().is_empty());
            assert!(!v.detail().is_empty(), "{v:?} would retire without a reason");
        }
    }

    /// The pairing that decides what a sweep actually does. Stated as a table
    /// because getting one cell wrong is either a run killed for working or a
    /// wedged run left running for ever.
    #[test]
    fn only_the_two_stopping_verdicts_signal_and_only_the_three_bad_ones_fail_the_run() {
        let cases = [
            (Verdict::Beating { seq: 1 }, false, false, false),
            (Verdict::Quiet { silence_ms: 1 }, false, false, false),
            (Verdict::Stalled { silence_ms: 1 }, true, true, true),
            (Verdict::Expired { lifetime_ms: 1 }, true, true, true),
            (Verdict::Vanished, false, true, true),
            (Verdict::Ended, false, false, true),
        ];
        for (v, terminates, fails, retires) in cases {
            assert_eq!(v.terminates(), terminates, "{v:?} terminates");
            assert_eq!(v.fails_the_run(), fails, "{v:?} fails the run");
            assert_eq!(v.retires(), retires, "{v:?} retires");
        }
    }

    #[test]
    fn durations_read_as_durations() {
        assert_eq!(human_ms(45_000), "45s");
        assert_eq!(human_ms(20 * 60_000), "20m");
        assert_eq!(human_ms(3 * 3_600_000 + 25 * 60_000), "3h25m");
    }
}
