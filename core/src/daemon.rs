//! The process that keeps the scheduler running.
//!
//! [`Ticker::tick`] already decides everything: what is due, what to do about
//! an instant missed while Jod was down, whether a still-running job blocks the
//! next one. This module adds only the two things a tick cannot do for itself —
//! *happen again in sixty seconds*, and *survive the last one having failed*.
//!
//! ## Why a resident process rather than a systemd timer
//!
//! `OnCalendar=*:0/1` invoking `jod tick` is the smaller-looking design, and it
//! is the wrong one here for three reasons.
//!
//! **Startup is not free, and it is not bounded by anything we control.** Every
//! one-shot process opens the SQLite file, replays prior runs back into memory
//! ([`Jod::rehydrate`]) and re-parses every armed cron expression before it can
//! answer "is anything due". That is work proportional to the history, repeated
//! 1,440 times a day to do 1,440 tiny reads. A lease that outlives a one-shot
//! process is fine — [`ticker::LEASE_MS`] is five minutes precisely so a
//! process that dies mid-fire is recovered — but a *tick interval shorter than
//! process startup* is not fine, and a timer design quietly puts a floor under
//! the interval at whatever boot costs on the day. The resident loop has no
//! such floor: it pays startup once.
//!
//! **The store is in WAL mode.** WAL's whole benefit — readers that never block
//! the writer, a warm page cache, one checkpointing story — accrues to a
//! connection that stays open. A process that opens, writes one row and exits
//! leaves the WAL for whoever comes next to checkpoint, so the cheapest possible
//! tick still touches the most files.
//!
//! **A tick is not the only thing a resident Jod does.** The same process holds
//! the rehydrated run state that answers `jod ls` and feeds the API's
//! subscribers. Splitting the scheduler out into a process that exists for
//! 40 ms means the schedule's run is launched by a process with no followers
//! attached to it.
//!
//! The timer shape is still supported — [`Daemon::run_once`] is exactly one
//! tick and a return value — because someone may prefer it, and because it is
//! what makes the loop's own behaviour testable.

use std::future::Future;
use std::time::Duration;

use crate::error::Result;
use crate::service::Jod;
use crate::ticker::{self, TickReport, Ticker};

/// How many prior runs the daemon reloads at boot.
///
/// The same figure the CLI and the API use. It bounds the boot cost of a long
/// history; runs older than this are still on disk, just not in memory.
const REHYDRATE_LIMIT: usize = 200;

/// One pass of the scheduler.
///
/// A trait rather than a bare [`Ticker`] so the loop's own promises — that a
/// failing tick does not end it, that a shutdown finishes the tick in flight —
/// can be tested without a database, a harness binary or a real minute.
pub trait Tick: Send + Sync {
    fn tick(&self, now_ms: i64) -> impl Future<Output = Result<TickReport>> + Send;
}

impl Tick for Ticker {
    /// Heartbeats first, then schedules, then goals, then the mail, then the
    /// queue — every step every pass.
    ///
    /// Separate claims rather than one, because they are different contended
    /// resources — a process holding a goal must not thereby hold a schedule —
    /// and a failure in one must not stop the others. A goal whose harness is
    /// wedged should not silently stop the nightly backup.
    ///
    /// **The sweep goes first, and the order is load-bearing.**
    /// [`Ticker::tick_goals`] settles the previous iteration by reading its
    /// run's status, and a wedged run's status is `running` for ever: the only
    /// process that writes a terminal status is the supervisor watching a
    /// harness that is never going to exit. Reaping before the goals are asked
    /// is what turns a hang into a `failed` the same tick, so the objective
    /// moves on instead of waiting on a run that will never end.
    ///
    /// A sweep that fails does not stop the pass. It watches runs that are
    /// already going; schedules that are due are a separate promise, and one
    /// unkillable process group must not become a scheduler that stopped.
    ///
    /// The two delivery steps are last and deliberately so: both resume agents
    /// that are already working, and nothing else in a tick waits on either.
    /// They are also the halves that turn Jod from something a person operates
    /// into something that runs — without [`Ticker::tick_mail`] a teammate's
    /// message sits in an inbox until somebody types `jod team wake`, and
    /// without [`Ticker::tick_deliveries`] a card answered in the rail is
    /// queued and never spoken.
    ///
    /// **This function is the whole wiring.** Both of those were built and
    /// tested against nothing for a while, and unit tests on an uncalled
    /// function stay green for ever; the test that guards this one calls
    /// `Tick::tick` rather than the steps, so removing a line here fails it.
    async fn tick(&self, now_ms: i64) -> Result<TickReport> {
        let swept = match self.tick_heartbeats(now_ms).await {
            Ok(report) => report,
            Err(e) => {
                eprintln!("[jod] heartbeat sweep failed: {e}");
                Default::default()
            }
        };
        let schedules = Ticker::tick(self, now_ms).await?;
        let goals = self.tick_goals(now_ms).await?;
        let mail = self.tick_mail(now_ms).await?;
        let queued = self.tick_deliveries(now_ms).await?;
        Ok(TickReport {
            claimed: schedules.claimed + goals.claimed + mail.claimed + queued.claimed,
            started: schedules.started + goals.started + mail.started + queued.started,
            held: schedules.held + goals.held + mail.held + queued.held,
            // A reaped run is a failure that happened, and the daemon's own log
            // line is where a person finds out that anything did. Counting it
            // here rather than in a fourth field keeps `TickReport` meaning
            // "what went wrong this pass", which is what reads it.
            failed: schedules.failed + goals.failed + mail.failed + queued.failed + swept.stopped,
        })
    }
}

/// What the daemon did over its whole life, for the line it logs on the way
/// out and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DaemonReport {
    /// Ticks attempted, failures included.
    pub ticks: usize,
    /// Ticks that returned an error. See [`Daemon::run`] for why this is a
    /// counter and not a reason to stop.
    pub failed: usize,
    /// Runs started across every tick.
    pub started: usize,
}

/// A scheduler that keeps ticking.
pub struct Daemon<T = Ticker> {
    tick: T,
    interval: Duration,
}

impl Daemon<Ticker> {
    /// Open `~/.jod/jod.db`, reload what was running before this process, and
    /// drive the real ticker.
    ///
    /// Rehydrating first is not optional. The overlap policy asks "is a run
    /// from this schedule still going", and a daemon that has just restarted
    /// answers *no* to that question about every run it did not launch itself —
    /// so an hourly job whose last run is still working would be started a
    /// second time by the very restart that was supposed to be invisible.
    pub async fn persistent() -> Result<Daemon<Ticker>> {
        let jod = Jod::persistent()?;
        match jod.rehydrate(REHYDRATE_LIMIT).await {
            Ok(n) if n > 0 => eprintln!("[jod/daemon] reloaded {n} prior run(s)"),
            Ok(_) => {}
            // Not fatal. A history this process cannot read is a reason to
            // treat every run as new, not a reason to leave the schedules
            // unfired until someone notices.
            Err(e) => eprintln!("[jod/daemon] could not reload prior runs: {e}"),
        }
        Ok(Daemon::new(Ticker::new(jod)))
    }
}

impl<T: Tick> Daemon<T> {
    /// Tick every [`ticker::TICK`] — sixty seconds, cron's own resolution.
    pub fn new(tick: T) -> Daemon<T> {
        Daemon {
            tick,
            interval: ticker::TICK,
        }
    }

    /// Tick on some other interval. For tests, which are not willing to wait a
    /// minute to watch a loop go round twice.
    pub fn every(mut self, interval: Duration) -> Daemon<T> {
        self.interval = interval;
        self
    }

    /// Tick exactly once, now, and hand back what happened.
    ///
    /// The one-shot half of the module doc: a systemd timer, a `jod tick` for a
    /// person who wants to see the scheduler move, and the seam every test in
    /// here is written through. The error is returned rather than logged,
    /// because a one-shot caller has an exit code to set.
    pub async fn run_once(&self) -> Result<TickReport> {
        self.tick.tick(now_ms()).await
    }

    /// Tick until `shutdown` resolves, then return.
    ///
    /// Two properties matter more than anything else this function does.
    ///
    /// **A failing tick is logged and forgotten.** Ending the loop on the first
    /// error would be worse than having no scheduler at all: the unit stays
    /// `active`, `systemctl status` stays green, and nothing fires again until
    /// somebody happens to look. A tick fails for reasons that are usually
    /// about *this minute* — a locked database, a harness binary being
    /// replaced — and the next minute is a free retry.
    ///
    /// **A shutdown never interrupts a tick.** The await on the tick is
    /// deliberately outside the `select!`, so a `SIGTERM` arriving mid-tick is
    /// noticed only once that tick has finished claiming, spawning and
    /// releasing. A claim abandoned between the claim and the fire is exactly
    /// the case the lease exists to recover, and five minutes of a schedule
    /// looking claimed is not worth saving a second at shutdown.
    pub async fn run(&self, shutdown: impl Future<Output = ()>) -> DaemonReport {
        let mut report = DaemonReport::default();
        tokio::pin!(shutdown);

        let mut clock = tokio::time::interval(self.interval);
        // A tick that overran must not be chased by a burst of catch-up ticks.
        // The instants that passed while it ran were not missed — they are in
        // the next tick's window, and the misfire policy already owns them.
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                // Shutdown wins a tie. The first `clock.tick()` is ready
                // immediately — a daemon that has just come up is the one most
                // likely to have missed something, so it looks straight away
                // rather than idling for a minute first — and without the bias
                // a stop arriving in that same instant would still tick.
                biased;
                _ = &mut shutdown => break,
                _ = clock.tick() => {}
            }

            match self.tick.tick(now_ms()).await {
                Ok(r) => {
                    report.ticks += 1;
                    report.started += r.started;
                }
                Err(e) => {
                    report.ticks += 1;
                    report.failed += 1;
                    eprintln!("[jod/daemon] tick failed, continuing: {e}");
                }
            }
        }

        eprintln!(
            "[jod/daemon] stopping after {} tick(s), {} failed, {} run(s) started",
            report.ticks, report.failed, report.started
        );
        report
    }
}

/// Resolves on `SIGTERM` or Ctrl-C.
///
/// `SIGTERM` is what `systemctl stop` and `systemctl restart` send; Ctrl-C is
/// for the terminal. Same handler as `jod-api`'s, for the same reason: a
/// restart should be a boundary the work does not notice.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            // No handler means nothing to wait for, which must not be mistaken
            // for "stop now".
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    use crate::error::JodError;

    const FAST: Duration = Duration::from_millis(1);

    /// Counts its calls, fails or succeeds as told, and rings a bell after a
    /// chosen number of ticks so a test can stop the loop without a timeout.
    struct Spy {
        calls: AtomicUsize,
        /// Ticks completed — incremented *after* the work, so a tick that was
        /// cut short would not be counted.
        completed: AtomicUsize,
        fail: bool,
        /// How long one tick takes.
        takes: Duration,
        /// Rung when `calls` reaches `ring_at`.
        bell: Notify,
        ring_at: usize,
        /// Ring on entry rather than on completion, to shut the daemon down
        /// while a tick is still in flight.
        ring_on_entry: bool,
    }

    impl Spy {
        fn new() -> Arc<Spy> {
            Arc::new(Spy {
                calls: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
                fail: false,
                takes: Duration::ZERO,
                bell: Notify::new(),
                ring_at: usize::MAX,
                ring_on_entry: false,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn completed(&self) -> usize {
            self.completed.load(Ordering::SeqCst)
        }

        /// A future that resolves when the bell rings. `Notify` keeps the
        /// permit, so a bell rung before anyone listens is not lost.
        async fn rung(self: Arc<Spy>) {
            self.bell.notified().await;
        }
    }

    impl Tick for Arc<Spy> {
        async fn tick(&self, _now_ms: i64) -> Result<TickReport> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.ring_on_entry && n >= self.ring_at {
                self.bell.notify_one();
            }
            if !self.takes.is_zero() {
                tokio::time::sleep(self.takes).await;
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            if !self.ring_on_entry && n >= self.ring_at {
                self.bell.notify_one();
            }
            if self.fail {
                return Err(JodError::Invalid("the database is locked".into()));
            }
            Ok(TickReport {
                started: 1,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn one_tick_against_an_empty_store_starts_nothing_and_fails_nothing() {
        use crate::store::Store;
        let jod = Jod::with_store(Arc::new(Store::in_memory().unwrap()));
        let report = Daemon::new(Ticker::new(jod)).run_once().await.unwrap();
        assert_eq!(report, TickReport::default());
    }

    #[tokio::test]
    async fn the_one_shot_mode_ticks_exactly_once() {
        let spy = Spy::new();
        let report = Daemon::new(spy.clone()).run_once().await.unwrap();
        assert_eq!(spy.calls(), 1, "one call, not zero and not two");
        assert_eq!(report.started, 1);
    }

    /// The failure this whole module exists to prevent: a scheduler that ends
    /// on the first bad tick leaves the unit `active` and nothing firing, which
    /// is strictly worse than no scheduler because it looks like one.
    #[tokio::test]
    async fn a_failing_tick_does_not_end_the_loop() {
        let mut spy = Spy::new();
        {
            let s = Arc::get_mut(&mut spy).unwrap();
            s.fail = true;
            s.ring_at = 3;
        }
        let daemon = Daemon::new(spy.clone()).every(FAST);
        let report = daemon.run(spy.clone().rung()).await;

        assert!(report.ticks >= 3, "the loop went round: {report:?}");
        assert_eq!(report.failed, report.ticks, "every tick failed");
        assert_eq!(report.started, 0);
    }

    /// A claim abandoned between claiming and firing is the case the lease
    /// exists to recover. Better not to create it for the sake of one second at
    /// shutdown.
    #[tokio::test]
    async fn a_shutdown_lets_the_tick_in_flight_finish() {
        let mut spy = Spy::new();
        {
            let s = Arc::get_mut(&mut spy).unwrap();
            s.ring_at = 1;
            s.ring_on_entry = true;
            s.takes = Duration::from_millis(50);
        }
        let daemon = Daemon::new(spy.clone()).every(FAST);
        let report = daemon.run(spy.clone().rung()).await;

        assert_eq!(spy.completed(), spy.calls(), "no tick was cut short");
        assert_eq!(report.ticks, spy.calls(), "and each one was accounted for");
    }

    /// The stop path is a `break`, not a panic or an abort, so the loop's
    /// tally survives to be logged.
    #[tokio::test]
    async fn a_daemon_told_to_stop_before_it_starts_never_ticks() {
        let spy = Spy::new();
        let daemon = Daemon::new(spy.clone()).every(FAST);
        let report = daemon.run(std::future::ready(())).await;

        assert_eq!(spy.calls(), 0);
        assert_eq!(report, DaemonReport::default());
    }

    /// Sixty seconds is cron's own resolution; polling faster buys nothing and
    /// polling slower makes `* * * * *` a lie.
    #[test]
    fn the_default_interval_is_the_scheduler_tick() {
        let daemon = Daemon::new(Spy::new());
        assert_eq!(daemon.interval, ticker::TICK);
    }
}
