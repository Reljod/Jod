//! Liveness for a run that is supposed to take hours.
//!
//! Schema is migration `0013_heartbeats` in [`crate::store`].
//!
//! Jod could already answer *is there a process* — [`crate::proc::group_alive`]
//! asks the kernel and never lies. What it could not answer is **is that
//! process still working**, and those come apart precisely in the case this
//! module exists for: a harness blocked on a socket that will never answer, a
//! tool waiting on input that cannot arrive, a model call retrying for ever.
//! The process group is alive, the pgid probe says so, and nothing has happened
//! for two hours.
//!
//! That gap was not theoretical. [`crate::ticker::Ticker::tick_goals`] settles
//! the previous iteration before starting the next, and it settles it by asking
//! the run's status. A wedged run's status is `running` and stays `running`,
//! because the only process that ever writes a terminal status is the
//! supervisor watching the harness exit — and the harness never exits. So the
//! goal waits. Not for an iteration, not for a day: for ever, silently, still
//! listed as `running` in `jod ls`. A heartbeat is what turns that into a
//! failure somebody can see.
//!
//! ## Two questions, both required
//!
//! - **Is the process there?** `kill(pgid, 0)`. Cheap, certain, and passed by
//!   every wedged run in existence.
//! - **Is it producing events?** The high-water `seq` in `events` for this run,
//!   compared with what the last beat saw. A harness that is working writes —
//!   tool calls, output, thinking. One that is not, does not.
//!
//! Either alone is a liveness check that misses half the failures. The
//! interesting part is the second, and it is worth being clear about what it
//! measures: not whether the agent is doing anything *useful*, only whether it
//! is doing anything at all. Judging usefulness is what a goal's `done_when`
//! and stall counter are for. This is the floor beneath them.
//!
//! ## What is pure, and why it matters
//!
//! [`decide`] is a function from a stored heartbeat and three observed facts
//! ([`Observed`]) to one [`Verdict`]. No clock, no database, no process — so "a
//! run that stalls exactly on the boundary", "a run that finished in the same
//! tick it would have been declared stalled" and "a clock that went backwards"
//! are all tested from values. Gathering the three facts and carrying the
//! verdict out is [`crate::ticker::Ticker::tick_heartbeats`], and that half
//! holds no policy at all.
//!
//! ## Cleanup is the schema's job, not a code path
//!
//! The charter's requirement is that a heartbeat goes away when its session is
//! deleted, fails, or finishes. Two of those are enforced by the table rather
//! than by remembering to call something:
//!
//! - **Deleted** — `run_id` is `REFERENCES runs(id) ON DELETE CASCADE`, and the
//!   store runs with `PRAGMA foreign_keys = ON`. Deleting the run deletes the
//!   heartbeat, including deletions written by code that has never heard of
//!   this module.
//! - **Finished or failed** — [`Verdict::Ended`], which [`decide`] reaches
//!   before it looks at anything else, and which drops the row.
//!
//! Only the third case needs code, and it needs it because a wedged run never
//! reports its own ending. That asymmetry is the whole design: the paths that
//! can be made automatic are, and the one that cannot is the one the tick runs.
//!
//! A retired heartbeat is *deleted*, not parked in a terminal state — cleanup
//! is the requirement, and a table of tombstones is not cleanup. The reason it
//! retired is written to the memory layer on the way out, so nothing is lost.
//!
//! **Not to the run's event stream, and that is not an oversight.** `events` is
//! `UNIQUE(run_id, seq)` and [`crate::store::Store::append_event`] ignores a
//! duplicate, while the supervisor allocates `seq` from a counter it holds in
//! its own memory. A watchdog writing `last_seq + 1` would therefore be racing
//! the one process that owns that sequence — and losing that race is *silent*,
//! costing either the watchdog's explanation or the supervisor's `Finished`.
//! For a stalled run those writes are near-simultaneous by construction: the
//! sweep signals the group, and the supervisor's own final event follows
//! milliseconds later. A fact has no such contention, and for a goal it lands
//! in the scope the *next iteration's prompt is built from*, so a stall is not
//! merely recorded — it is told to whatever runs next.

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
    /// When this run went quiet, for a run that is being marked rather than
    /// reaped. `None` means healthy.
    ///
    /// This is the moment of the *last event*, not the moment the sweep noticed
    /// — so it is the same value on the first stalled tick and the fiftieth,
    /// and "how long has it been silent" is one subtraction rather than a
    /// number that depends on when a daemon happened to look.
    ///
    /// A fact derived from the heartbeat rather than a variant on
    /// [`crate::AgentStatus`], because `runs.status` is written by the
    /// supervisor — a separate process — and a status that process never writes
    /// would drift the moment the two disagreed.
    pub stalled_since_ms: Option<i64>,
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
            stalled_since_ms: None,
        }
    }

    /// Whether this run is currently marked as stalled.
    pub fn is_stalled(&self) -> bool {
        self.stalled_since_ms.is_some()
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
pub fn human_ms(ms: i64) -> String {
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

/// What the sweep does about a verdict, once it takes into account *what* is
/// being watched.
///
/// [`decide`] answers "what is true of this run" and knows nothing about who
/// asked. This answers "so what do we do", and it is a separate function
/// because the same verdict means two different things:
///
/// - A **goal iteration** that wedges blocks its goal's loop for ever. Nothing
///   else will ever notice, and the goal's own cadence stops meaning anything.
///   Reaping it is the case heartbeats were written for.
/// - A **session Reljod is watching** is not blocking a loop. Killing it
///   destroys a transcript and possibly a checkout mid-edit, to fix a problem
///   he can see and decide about himself. He chose mark-and-surface, so a stall
///   here is a *fact on a row*, not an execution.
///
/// Splitting it here rather than inside [`decide`] keeps the verdict honest —
/// a silent run is stalled whoever is watching it, and only the response
/// differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Still going, or quiet inside its window. Record the beat, keep watching.
    Beat,
    /// Silent past its window, on a run that is not to be killed for it. Write
    /// the mark, keep watching, signal nothing.
    Mark,
    /// It went quiet and came back. Clear the mark and carry on. Coming back is
    /// not a failure and must stop looking like one.
    Clear,
    /// Stop watching, and correct the run to `failed`. `terminate` says whether
    /// there is still a process group to signal — [`Verdict::Vanished`] means
    /// there is not, and `kill` against a recycled pgid reaches a stranger.
    Reap { terminate: bool },
    /// The run ended on its own. Drop the row, touch nothing.
    Retire,
}

impl Response {
    /// Whether the heartbeat row survives this response.
    pub fn keeps_watching(&self) -> bool {
        matches!(self, Response::Beat | Response::Mark | Response::Clear)
    }
}

/// What to do about one run, given the verdict and what the heartbeat watches.
///
/// The one place the mark-don't-kill rule lives. Pure, so every cell of the
/// table below is reachable from a literal in a test.
pub fn respond(hb: &Heartbeat, verdict: &Verdict) -> Response {
    match verdict {
        // Progress clears a mark. Checked here rather than in the store so
        // that "it came back" is a decision with a name, not an `UPDATE` that
        // happens to set a column to null.
        Verdict::Beating { .. } if hb.is_stalled() => Response::Clear,
        Verdict::Beating { .. } | Verdict::Quiet { .. } => Response::Beat,

        // The split. `Expired` rides along with `Stalled` rather than being
        // special-cased twice: a run that outlived a ceiling somebody set is
        // still just a session, and `Watching::Run` has no ceiling by default
        // anyway (see `Heartbeat::starting`), so this only fires for one that
        // was given one explicitly.
        Verdict::Stalled { .. } | Verdict::Expired { .. } => match hb.watching {
            Watching::Goal(_) => Response::Reap { terminate: true },
            Watching::Run => Response::Mark,
        },

        // Not part of the split, and deliberately. A vanished run has no
        // process left: its supervisor died without recording an ending, so the
        // row is lying. Marking it stalled would leave `jod ls` claiming a
        // process is running when the kernel says otherwise, which is the bug
        // this module exists to remove rather than one to add a second copy of.
        Verdict::Vanished => Response::Reap { terminate: false },
        Verdict::Ended => Response::Retire,
    }
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
    /// The mark, carried on the same write as the cursor.
    ///
    /// One write rather than a separate `mark_stalled`/`clear_stalled` pair,
    /// because every sweep that sets or clears this is already writing the
    /// cursor in the same tick — and two statements would let a crash between
    /// them leave a run marked stalled with a cursor that says it is working.
    pub stalled_since_ms: Option<i64>,
}

impl Beat {
    /// Fold a verdict into the heartbeat it came from.
    ///
    /// `last_progress_ms` advances only on [`Verdict::Beating`]. A quiet sweep
    /// still records that the sweep happened, because "the scheduler has not
    /// looked at this run since Tuesday" and "this run has been silent since
    /// Tuesday" are different problems, and one column cannot answer both.
    ///
    /// The mark follows [`respond`], so the column and the decision cannot
    /// disagree: a `Beating` verdict clears it, a stall that is being marked
    /// sets it to when the run actually went quiet, and everything else leaves
    /// it as it was.
    pub fn after(hb: &Heartbeat, verdict: &Verdict, now_ms: i64) -> Beat {
        let stalled_since_ms = match respond(hb, verdict) {
            Response::Clear => None,
            // `last_progress_ms`, not `now_ms`: the run went quiet when it
            // stopped producing events, not when a daemon got round to
            // noticing. Recording the sweep's clock would make the same stall
            // report a different age depending on how busy the box was, and
            // would move every tick.
            Response::Mark => Some(hb.stalled_since_ms.unwrap_or(hb.last_progress_ms)),
            _ => hb.stalled_since_ms,
        };
        match verdict {
            Verdict::Beating { seq } => Beat {
                run_id: hb.run_id.clone(),
                last_seq: *seq,
                last_progress_ms: now_ms,
                last_beat_ms: now_ms,
                stalled_since_ms,
            },
            _ => Beat {
                run_id: hb.run_id.clone(),
                last_seq: hb.last_seq,
                last_progress_ms: hb.last_progress_ms,
                last_beat_ms: now_ms,
                stalled_since_ms,
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
    /// Runs marked stalled and left alone — counted separately from `stopped`
    /// precisely because they are the ones that were *not* stopped.
    pub marked: usize,
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

    // ---- the mark-don't-kill split --------------------------------------

    /// Check 3, in the pure half. The verdict is unchanged — a silent run is
    /// stalled whoever is watching — and only the response differs.
    #[test]
    fn a_watched_session_that_stalls_is_marked_rather_than_killed() {
        let h = hb(0);
        let verdict = decide(&h, &running(-1), DEFAULT_STALL_MS);
        assert!(matches!(verdict, Verdict::Stalled { .. }), "still stalled");
        assert_eq!(respond(&h, &verdict), Response::Mark);
        assert!(
            respond(&h, &verdict).keeps_watching(),
            "the heartbeat must survive, or the next tick forgets it stalled"
        );
    }

    /// Check 4, the regression guard on the split. A goal iteration that wedges
    /// blocks its goal's loop for ever, and nothing else will notice.
    #[test]
    fn a_goal_iteration_that_stalls_is_still_reaped() {
        let g = Heartbeat::starting("r", Watching::Goal("green-ci".into()), 0);
        let verdict = decide(&g, &running(-1), DEFAULT_STALL_MS);
        assert_eq!(respond(&g, &verdict), Response::Reap { terminate: true });
        assert!(!respond(&g, &verdict).keeps_watching());
    }

    /// The `Expired` half of the same split, so a ceiling cannot become a
    /// second way to kill a session Reljod is watching.
    #[test]
    fn a_ceiling_reaps_a_goal_and_only_marks_a_session() {
        let session = hb(0).with_max_lifetime_ms(Some(1_000));
        let expired = decide(&session, &running(99), 1_000);
        assert_eq!(expired, Verdict::Expired { lifetime_ms: 1_000 });
        assert_eq!(respond(&session, &expired), Response::Mark);

        let mut goal = Heartbeat::starting("r", Watching::Goal("g".into()), 0);
        goal.max_lifetime_ms = Some(1_000);
        let expired = decide(&goal, &running(99), 1_000);
        assert_eq!(
            respond(&goal, &expired),
            Response::Reap { terminate: true },
            "a goal's ceiling is the whole reason it has one"
        );
    }

    /// A session gets no ceiling unless somebody asks for one, which is what
    /// keeps `Expired` from firing on an ordinary long run at all.
    #[test]
    fn a_watched_session_has_no_ceiling_to_expire_against() {
        assert_eq!(hb(0).max_lifetime_ms, None);
    }

    /// Check 5. Coming back is not a failure and must stop looking like one.
    #[test]
    fn a_marked_run_that_produces_an_event_is_unmarked() {
        let mut h = hb(0);
        h.stalled_since_ms = Some(0);
        let verdict = decide(&h, &running(7), DEFAULT_STALL_MS * 2);
        assert_eq!(verdict, Verdict::Beating { seq: 7 });
        assert_eq!(respond(&h, &verdict), Response::Clear);
        assert_eq!(
            Beat::after(&h, &verdict, 99_000).stalled_since_ms,
            None,
            "the mark has to actually come off the row"
        );
    }

    /// The mark names when the run went quiet, not when a daemon noticed. A
    /// sweep that ran late, or ran fifty times, must report the same age.
    #[test]
    fn the_mark_does_not_move_when_the_sweep_runs_again() {
        let h = hb(0);
        let first = Beat::after(&h, &decide(&h, &running(-1), DEFAULT_STALL_MS), DEFAULT_STALL_MS);
        assert_eq!(
            first.stalled_since_ms,
            Some(0),
            "it went quiet at its last event, which for a fresh run is its start"
        );

        // Feed that back in as the stored row and sweep again, much later.
        let mut second = h.clone();
        second.stalled_since_ms = first.stalled_since_ms;
        let later = DEFAULT_STALL_MS * 9;
        let again = Beat::after(&second, &decide(&second, &running(-1), later), later);
        assert_eq!(
            again.stalled_since_ms, first.stalled_since_ms,
            "a second sweep must not restamp the moment it went quiet"
        );
    }

    /// A vanished run is not marked. There is no process, so a row that said
    /// `running` would keep lying — which is the bug this module removes.
    #[test]
    fn a_vanished_run_is_still_corrected_rather_than_marked() {
        let h = hb(0);
        let gone = Observed {
            status: Some("running".into()),
            alive: false,
            last_seq: 4,
        };
        let verdict = decide(&h, &gone, 1_000);
        assert_eq!(verdict, Verdict::Vanished);
        assert_eq!(respond(&h, &verdict), Response::Reap { terminate: false });
    }

    /// The whole table, stated in one place for the same reason the terminates/
    /// fails/retires one is: a single wrong cell is either a session killed for
    /// being slow or a wedged goal blocking its loop for ever.
    #[test]
    fn what_each_verdict_does_depends_on_what_is_being_watched() {
        let session = hb(0);
        let goal = Heartbeat::starting("r", Watching::Goal("g".into()), 0);
        let cases = [
            (Verdict::Beating { seq: 1 }, Response::Beat, Response::Beat),
            (Verdict::Quiet { silence_ms: 1 }, Response::Beat, Response::Beat),
            (
                Verdict::Stalled { silence_ms: 1 },
                Response::Mark,
                Response::Reap { terminate: true },
            ),
            (
                Verdict::Expired { lifetime_ms: 1 },
                Response::Mark,
                Response::Reap { terminate: true },
            ),
            (
                Verdict::Vanished,
                Response::Reap { terminate: false },
                Response::Reap { terminate: false },
            ),
            (Verdict::Ended, Response::Retire, Response::Retire),
        ];
        for (verdict, for_session, for_goal) in cases {
            assert_eq!(respond(&session, &verdict), for_session, "session {verdict:?}");
            assert_eq!(respond(&goal, &verdict), for_goal, "goal {verdict:?}");
        }
    }

    #[test]
    fn durations_read_as_durations() {
        assert_eq!(human_ms(45_000), "45s");
        assert_eq!(human_ms(20 * 60_000), "20m");
        assert_eq!(human_ms(3 * 3_600_000 + 25 * 60_000), "3h25m");
    }
}
