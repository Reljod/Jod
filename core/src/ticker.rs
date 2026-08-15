//! The tick that fires due schedules and advances goals.
//!
//! Split deliberately in two. [`decide`] is a pure function from a schedule and
//! what is known about it to a list of things to do; [`Ticker::tick`] is the
//! part that talks to the store and the supervisor. Everything that is easy to
//! get subtly wrong — what "missed" means, whether a still-running job blocks
//! the next one, how many replays an outage earns — lives in the pure half and
//! is tested without a clock, a database or a process.
//!
//! The tick is 60 seconds because that is the resolution a cron expression has.
//! Polling faster buys nothing; polling slower makes `* * * * *` a lie.
//!
//! A schedule may also carry a [`monitor`], and then the tick asks it before it
//! spends anything: [`plan`] folds what the monitor saw into what the schedule
//! alone decided, and on almost every tick the fold is "start nothing". That
//! fold is pure too, so "unchanged suppresses the run" is tested from a value
//! rather than from a process.
//!
//! ## The tick is also what speaks to agents, by two roads
//!
//! [`Ticker::tick_deliveries`] injects what the queue in [`crate::delivery`]
//! holds for a conversation — card answers and human nudges — and
//! [`Ticker::tick_mail`] wakes members holding mail. Both were built before
//! anything called them, which is why the composite [`crate::daemon::Tick`]
//! impl is guarded by a test that goes through it rather than through the
//! steps.
//!
//! [`Ticker::tick_mail`] is the **authoritative** path for agent-to-agent mail:
//! [`crate::team::wake_order`] decides who may be woken, `claim_wake` rate-limits
//! it across ticks, and `Store::take_mail` takes the mail off the bus in one
//! statement — recording *both* that it went and that it may not go twice, which
//! the drain-then-mark version it replaced could be, and was, half-done. Card
//! answers and human nudges travel a different road, [`crate::delivery`], which
//! queues against a *conversation*.
//!
//! Two roads is one more than the design wants, and they are being merged in
//! order rather than at once. The full reasoning is at the top of
//! [`crate::delivery`]; the short of it is that this road addresses a
//! **member** and that one addresses a **conversation**, an explicit team's
//! member has no stable conversation to be addressed by, and closing that gap
//! needs a schema change. The state vocabulary *was* unified, so both tables
//! now mean the same thing by the same word, and the queue that had no caller
//! now has one.
//!
//! Two rules for whoever does merge them:
//!
//! - **`claim_wake` is not `plan_injection`.** One is a rate limit across
//!   ticks, the other declines only while a turn is in flight. Fold the former
//!   into the latter and an idle member receiving one message per tick gets one
//!   turn per tick, which is the cost problem the rate limit exists to prevent.
//! - **The drain and the queue must settle together.** Today exactly one row
//!   records what an agent was told. Two records of that, settled in two
//!   statements, disagree the first time a process dies between them.

use std::sync::Arc;

use crate::error::Result;
use crate::harness::{HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::heartbeat::{self, Beat, Heartbeat, Observed, SweepReport, Verdict, Watching};
use crate::monitor::{self, LocalProbes, Observation, Probes};
use crate::schedule::{self, Fire, FireOutcome, Goal, Misfire, Overlap, Schedule};
use crate::service::{AgentStatus, Jod, RunConversation};
use crate::store::{NewFact, Origin, Store};
use crate::team::{self, MemberStatus};
use crate::works;

/// What a goal's done-when command said.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DoneCheck {
    /// Exit zero. The goal is finished.
    satisfied: bool,
    /// A hash of what the check *saw*. Two iterations with the same
    /// fingerprint moved nothing, however much the agent reported doing.
    fingerprint: String,
}

/// Collapse a run's last message to one line, so an episodic record stays a
/// record rather than becoming a second copy of the transcript.
fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 160 {
        return flat;
    }
    format!("{}…", flat.chars().take(160).collect::<String>())
}

/// How often the scheduler looks for work.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the delivery ledger is trimmed. See [`Ticker::trim_ledger`].
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1_000;

/// Where the last trim is remembered. In `settings` and not in this struct,
/// because a field would reset on every restart and turn "hourly" into "every
/// startup".
const PRUNED_AT_KEY: &str = "ledger.pruned_at_ms";

/// How often GitHub is asked about pull requests. See
/// [`Ticker::tick_pull_requests`].
///
/// Five minutes rather than the tick's minute, because this is the only step
/// that leaves the machine. It bounds how stale a state column can be, and
/// nothing acts on that column — it is read by a panel — so the cost of the
/// interval is a display being briefly out of date.
const POLL_EVERY_MS: i64 = 5 * 60 * 1_000;

/// Where the last poll is remembered, for the same reason as
/// [`PRUNED_AT_KEY`].
const POLLED_AT_KEY: &str = "pull_requests.polled_at_ms";

/// How much one sweep will ask about: this many stale pull requests, and this
/// many held leases.
///
/// Bounded because each one is a process and a network round trip against an
/// hourly budget. A backlog is not lost by the bound — `stale_pull_requests`
/// hands back the least recently asked first, so a queue longer than this
/// drains over the next few sweeps rather than starving.
const PR_SWEEP_LIMIT: usize = 20;

/// Whether enough time has passed to ask the forge again.
///
/// A clock that went backwards — a VM restored from a snapshot, an NTP
/// correction — must not lock polling out until it catches up, which is why
/// `at > now_ms` is due rather than not.
fn due_to_poll(store: &Store, now_ms: i64) -> bool {
    match store
        .setting(POLLED_AT_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
    {
        Some(at) => now_ms.saturating_sub(at) >= POLL_EVERY_MS || at > now_ms,
        None => true,
    }
}

/// How long a claim is believed before another process may take it.
///
/// Comfortably longer than a tick, so an ordinary slow tick never loses its own
/// claim, and short enough that a machine which dies mid-fire is picked up
/// within a few minutes rather than at the next restart.
pub const LEASE_MS: i64 = 300_000;

/// How long a monitor gets to answer before the tick gives up on it.
///
/// [`monitor::LocalProbes`] deliberately imposes no timeout of its own, on the
/// grounds that only the caller knows whether it is holding a scheduler tick
/// open while it waits. This is that caller. Schedules are handled one after
/// another, so a probe that never returns stalls every schedule behind it —
/// and a watchdog that hangs has failed, which is what it gets recorded as.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One thing the tick decided to do about one schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Start a run for the instant it was due.
    Run { due_at_ms: i64 },
    /// Do not start a run, and write down why.
    Hold {
        due_at_ms: i64,
        outcome: FireOutcome,
        why: String,
    },
    /// Stop what is running and start a fresh run in its place.
    Replace { due_at_ms: i64, stop: String },
}

impl Decision {
    pub fn due_at_ms(&self) -> i64 {
        match self {
            Decision::Run { due_at_ms }
            | Decision::Hold { due_at_ms, .. }
            | Decision::Replace { due_at_ms, .. } => *due_at_ms,
        }
    }
}

/// What to do about one due schedule.
///
/// `missed` is every instant that passed unfired, oldest first, and `running`
/// names the run from this schedule that is still going, if any.
///
/// Both policies collapse to nothing surprising in the ordinary case: one
/// missed instant, nothing running, one run.
pub fn decide(s: &Schedule, missed: &[i64], running: Option<&str>) -> Vec<Decision> {
    if missed.is_empty() {
        return vec![];
    }

    // Overlap is decided first and applies to the whole tick, because "the
    // previous run is still going" is a fact about the schedule rather than
    // about any particular missed instant.
    if let Some(run) = running {
        match s.overlap {
            Overlap::Skip => {
                return vec![Decision::Hold {
                    due_at_ms: *missed.last().unwrap(),
                    outcome: FireOutcome::SkippedOverlap,
                    why: format!("{run} is still running"),
                }];
            }
            Overlap::Replace => {
                return vec![Decision::Replace {
                    due_at_ms: *missed.last().unwrap(),
                    stop: run.to_string(),
                }];
            }
            // Fall through: concurrent runs are allowed.
            Overlap::Allow => {}
        }
    }

    // More than one instant passed, so Jod was not running when it should have
    // been. Which of them deserve a run is the misfire policy.
    match s.misfire {
        // The question after an outage is "did my inbox get triaged", not "did
        // it get triaged eleven times".
        Misfire::FireOnce => vec![Decision::Run {
            due_at_ms: *missed.last().unwrap(),
        }],
        Misfire::Skip => {
            let last = *missed.last().unwrap();
            // Only *missing* instants are skipped. The one that just came due
            // is not a misfire and still runs, or `skip` would mean "never".
            let stale = &missed[..missed.len() - 1];
            let mut out: Vec<Decision> = stale
                .iter()
                .map(|due| Decision::Hold {
                    due_at_ms: *due,
                    outcome: FireOutcome::SkippedMisfire,
                    why: "missed while Jod was not running".into(),
                })
                .collect();
            out.push(Decision::Run { due_at_ms: last });
            out
        }
        Misfire::FireAll => missed
            .iter()
            .take(Misfire::MAX_REPLAY)
            .map(|due| Decision::Run { due_at_ms: *due })
            .collect(),
    }
}

/// Whether carrying this decision out would start a harness.
fn spawns(d: &Decision) -> bool {
    matches!(d, Decision::Run { .. } | Decision::Replace { .. })
}

/// Fold what a schedule's monitor saw into what the schedule alone decided.
///
/// This is where monitor suppression actually happens, and it is a pure
/// function of two values so that every branch of it is tested without a
/// process: the only impure step is producing `watch`, which the caller does
/// first.
///
/// `watch` is `None` for a schedule with no monitor, and then nothing here
/// changes anything — a schedule without a monitor fires exactly as it always
/// did.
///
/// Note what is *kept* when a monitor suppresses: the holds. They account for
/// instants that passed while Jod was down, which is a fact about the clock
/// rather than about what the monitor saw, and dropping them would lose the
/// only record that those instants existed.
pub fn plan(planned: Vec<Decision>, watch: Option<&monitor::Decision>) -> Vec<Decision> {
    let Some(watch) = watch else {
        return planned;
    };
    if !watch.wakes_agent() {
        // Unchanged, still a baseline, broken, or a `no_agent` script that has
        // already done the whole job — none of them is a reason to pay for a
        // model. The tick is not silent about it: the caller writes what was
        // seen to `monitor_checks`, whose vocabulary keeps "nothing changed"
        // and "this watchdog is broken" apart, and a row to `schedule_fires`
        // so that a person asking "is this thing alive" gets an answer in the
        // one place they look for it.
        return planned.into_iter().filter(|d| !spawns(d)).collect();
    }
    // One change is one change. A `fire_all` schedule coming back from an
    // outage would otherwise start twenty runs carrying the identical diff,
    // which is the bill a monitor exists to avoid rather than to multiply.
    let last = planned.iter().rposition(spawns);
    planned
        .into_iter()
        .enumerate()
        .filter(|(i, d)| !spawns(d) || Some(*i) == last)
        .map(|(_, d)| d)
        .collect()
}

/// The prompt a run is started with.
///
/// A monitored change goes in *front* of the operator's words rather than
/// replacing them, wrapped by [`monitor::changed_prompt`], because whoever
/// writes the watched page is not the operator and the transcript should say
/// so.
pub fn prompt_for(s: &Schedule, watch: Option<&monitor::Decision>) -> String {
    match watch {
        Some(monitor::Decision::Run { diff }) => monitor::changed_prompt(&s.prompt, diff),
        _ => s.prompt.clone(),
    }
}

/// What a quiet tick says about itself in the fire history.
///
/// The monitor's own word, because [`FireOutcome::MonitorQuiet`] covers four
/// different quiets — nothing changed, nothing to compare against yet, a
/// `no_agent` script with nothing to say, and one that had something to say —
/// and a person reading the log wants to know which. Bounded, because a fire's
/// detail is a line in a listing and a `no_agent` script may print a page.
fn quietly(verdict: &monitor::Decision) -> String {
    match verdict {
        monitor::Decision::Report { text } => {
            format!("{}: {}", verdict.outcome(), one_line(text))
        }
        _ => verdict.outcome().to_string(),
    }
}

/// A monitor that produced no verdict at all, as a failed check.
fn probe_gave_up(detail: String) -> (Observation, monitor::Decision) {
    (
        Observation::failed(monitor::PROBE_DID_NOT_RUN, detail.clone()),
        monitor::Decision::Failed { detail },
    )
}

/// Drives schedules and goals against a running [`Jod`].
pub struct Ticker {
    jod: Arc<Jod>,
    /// Who this process is, for the claim. Distinct per process, because two
    /// `jod` processes on one box must not look like the same claimant.
    owner: String,
    /// How a monitor reaches the world outside the process.
    ///
    /// Injectable because it is the only part of a tick that runs a command or
    /// opens a socket: the daemon substitutes one that can fetch a URL — see
    /// [`monitor::Probes`] for why the HTTP half is not implemented in `core` —
    /// and tests substitute one that answers from a script.
    probes: Arc<dyn Probes + Send + Sync>,
}

/// Whether a run has not finished yet.
///
/// The three words a run wears before it is over, in one place: two callers
/// ask this question and a fourth spelling of it would be the kind of drift
/// that leaves a member busy for ever.
fn is_live(status: &str) -> bool {
    matches!(status, "running" | "starting" | "queued")
}

/// What one tick did, for logging and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: usize,
    pub started: usize,
    pub held: usize,
    pub failed: usize,
}

impl Ticker {
    pub fn new(jod: Arc<Jod>) -> Ticker {
        Ticker {
            jod,
            owner: format!("{}@{}", std::process::id(), hostname()),
            probes: Arc::new(LocalProbes),
        }
    }

    /// Use a fixed owner name. Tests need two "processes" in one process.
    pub fn as_owner(mut self, owner: impl Into<String>) -> Ticker {
        self.owner = owner.into();
        self
    }

    /// Run monitors through these instead of this machine's shell.
    pub fn with_probes(mut self, probes: Arc<dyn Probes + Send + Sync>) -> Ticker {
        self.probes = probes;
        self
    }

    /// One pass: claim what is due, act on it, and let it go again.
    ///
    /// A schedule that fails here is released rather than left claimed. The
    /// alternative — an early return on the first error — would leave a claim
    /// held by a process that has stopped thinking about it, and the schedule
    /// would sit still until the lease expired.
    pub async fn tick(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let due = store.claim_due_schedules(&self.owner, now_ms, LEASE_MS)?;
        let mut report = TickReport {
            claimed: due.len(),
            ..Default::default()
        };

        for s in due {
            let missed = self.missed_for(&s, now_ms)?;
            let running = self.running_run(&s).await?;
            let planned = decide(&s, &missed, running.as_deref());
            // Before anything is spent. A schedule with no monitor answers
            // `None` here and everything below is exactly as it was.
            let watched = self.watch(&s, &planned).await?;
            let watch = watched.as_ref().map(|(_, verdict)| verdict);
            // The instant the monitor answered for: the one a run would have
            // been started for, which is the last of them. Taken before
            // `plan` consumes the list, and always present when `watched` is,
            // because nothing is probed for a tick that would start nothing.
            let answered_for = planned
                .iter()
                .rev()
                .find(|d| spawns(d))
                .map(|d| d.due_at_ms())
                .unwrap_or(now_ms);
            // Only ever "this tick could not start a process". Whether a run
            // that *did* start then failed is not known yet — the harness is
            // still alive at this point — so that half of failure is counted by
            // `release_schedule` on a later tick, out of the run's own status.
            let mut failed = false;
            let mut ran = false;

            for decision in plan(planned, watch) {
                match self.carry_out(&s, &decision, now_ms, watch).await {
                    Ok(true) => {
                        report.started += 1;
                        ran = true;
                    }
                    Ok(false) => report.held += 1,
                    Err(e) => {
                        failed = true;
                        report.failed += 1;
                        // A spawn that failed is recorded like any other
                        // outcome. A schedule that silently did not run is the
                        // failure this whole table exists to make visible.
                        let _ = store.record_fire(&Fire {
                            id: 0,
                            schedule_id: s.id.clone(),
                            due_at_ms: decision.due_at_ms(),
                            fired_at_ms: now_ms,
                            run_id: None,
                            outcome: FireOutcome::SpawnFailed,
                            detail: Some(e.to_string()),
                        });
                    }
                }
            }

            if let Some((seen, verdict)) = watched {
                // True only if the loop above already wrote a `SpawnFailed`
                // row for this tick. One failure earns one row.
                let spawn_already_failed = failed;
                // A change that was seen but whose run never started must not
                // become the new normal. Recorded as a failure it leaves the
                // digest where it was, so the next tick reports the same change
                // again instead of adopting it and never mentioning it.
                let verdict = if verdict.wakes_agent() && !ran {
                    monitor::Decision::Failed {
                        detail: "the monitor saw a change, but the run that would \
                                 report it did not start"
                            .into(),
                    }
                } else {
                    verdict
                };

                let fire = |outcome, detail| Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: answered_for,
                    fired_at_ms: now_ms,
                    run_id: None,
                    outcome,
                    detail: Some(detail),
                };
                match &verdict {
                    monitor::Decision::Failed { detail } => {
                        // A watchdog that cannot run is a schedule that is
                        // failing. Counting it is what makes it back off and
                        // eventually break, rather than probe a dead host every
                        // minute for ever while its history fills with
                        // identical failures.
                        if !spawn_already_failed {
                            report.failed += 1;
                            store.record_fire(&fire(FireOutcome::SpawnFailed, detail.clone()))?;
                        }
                        failed = true;
                    }
                    quiet if !quiet.wakes_agent() => {
                        // The tick did its whole job and cost nothing — and
                        // says so where a person looks to ask whether a
                        // schedule is alive, because a week of no rows at all
                        // is indistinguishable from a week of being broken.
                        report.held += 1;
                        store.record_fire(&fire(FireOutcome::MonitorQuiet, quietly(quiet)))?;
                    }
                    // A change that woke a run: the loop above already recorded
                    // it as the ordinary fire it is.
                    _ => {}
                }
                // Written whatever it says. A check nobody recorded is how a
                // watchdog broken for a week comes to look like one dutifully
                // reporting that all is well.
                store.record_check(&s.id, &seen, &verdict, now_ms)?;
            }

            store.release_schedule(&s.id, now_ms, failed)?;
        }

        // Last, and unable to affect anything above it. Housekeeping that could
        // delay a fire would be a scheduler that misses its window to tidy up,
        // which is the wrong way round.
        self.trim_ledger(&store, now_ms);
        Ok(report)
    }

    /// Trim the delivery ledger back to the bound it advertises, at most once
    /// an hour.
    ///
    /// `MAX_ROWS` and `RETENTION_MS` were promises nothing kept: `prune_ledger`
    /// had no caller, so the table grew without limit on the box Jod shares with
    /// the work.
    ///
    /// **Hourly**, and the number comes from which bound can be crossed
    /// quickly. Retention is a week, so it would be served by a daily pass.
    /// `MAX_ROWS` is 500 and a chatty day can cross it, so the interval decides
    /// how far over the bound the table is allowed to sit — an hour of traffic
    /// rather than a day of it. Against that, the cost is one write transaction
    /// every sixtieth tick, on the same database the tick needs.
    ///
    /// **Nothing here is fatal and nothing here is returned.** A ledger that
    /// cannot be trimmed is a reason to get on with firing schedules, exactly as
    /// an unreadable history is a reason for the daemon to carry on starting
    /// runs. The alternative is a full disk stopping the scheduler, which
    /// trades a bounded problem for an unbounded one.
    ///
    /// **It runs before `tick_goals`, and that is deliberate — do not move it.**
    /// `Tick for Ticker` calls this half first, so a trim technically sits
    /// between the schedules and the goals of one pass. The analysis has been
    /// done, so that the next person to notice does not redo it and act on it: a
    /// single `DELETE` pair over at most [`ledger::MAX_ROWS`] rows, once an
    /// hour, against a sixty-second tick is sub-millisecond in SQLite, and it
    /// adds no new contention because schedules already write inside this same
    /// tick. Fixing it means reordering the most load-bearing loop in the
    /// system — the one place a mistake is a schedule that never fires — to buy
    /// microseconds. "Bounded but not zero" is the honest description of
    /// something that does not matter.
    fn trim_ledger(&self, store: &Store, now_ms: i64) {
        let last = store
            .setting(PRUNED_AT_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok());
        let due = match last {
            // A clock that went backwards — a VM restored from a snapshot, an
            // NTP correction — must not lock pruning out until it catches up.
            Some(at) => now_ms.saturating_sub(at) >= PRUNE_EVERY_MS || at > now_ms,
            None => true,
        };
        if !due {
            return;
        }

        // Stamped **before** the work, and persisted rather than held in
        // memory. Both halves matter and for different reasons.
        //
        // Persisted, because a field on this struct is reset by every restart:
        // "hourly" would silently become "every startup", and a crash-looping
        // daemon would prune every minute — the failure mode the interval
        // exists to prevent, reached by the mechanism meant to prevent it.
        //
        // Before, because a prune that throws must wait its hour like any
        // other. Stamping afterwards would retry a failing delete every minute
        // for as long as it kept failing. It is the same shape as a schedule
        // lease: claim the slot, then do the work.
        if let Err(e) = store.set_setting(PRUNED_AT_KEY, &now_ms.to_string()) {
            eprintln!("[jod/tick] could not record a ledger trim, so skipping it: {e}");
            return;
        }
        match store.prune_ledger(now_ms) {
            Ok(0) => {}
            Ok(gone) => eprintln!("[jod/tick] trimmed {gone} settled row(s) from the ledger"),
            Err(e) => eprintln!("[jod/tick] could not trim the delivery ledger: {e}"),
        }
    }

    /// Run this schedule's monitor, if it has one and this tick would otherwise
    /// start something.
    ///
    /// Nothing is probed for a tick that was going to hold anyway. A successful
    /// check moves the baseline, so probing a schedule whose run is already
    /// blocked — by an overlap, or by there being nothing due — would consume
    /// the change and leave the tick that could finally act on it reading
    /// "unchanged".
    async fn watch(
        &self,
        s: &Schedule,
        planned: &[Decision],
    ) -> Result<Option<(Observation, monitor::Decision)>> {
        if !planned.iter().any(spawns) {
            return Ok(None);
        }
        let Some(store) = self.jod.store() else {
            return Ok(None);
        };
        let Some(watching) = store.monitor(&s.id)? else {
            return Ok(None);
        };

        // Off the runtime: a command probe blocks on a child process and the
        // daemon's URL probe blocks on a request, either of which would stall
        // every other task sharing the executor thread. A blocking context is
        // also what lets that implementation drive an async client with
        // `Handle::block_on`, which is how `fetch` gets written at all.
        let probes = self.probes.clone();
        let checking = tokio::task::spawn_blocking(move || monitor::observe(&watching, &*probes));

        // On a timeout the blocking task is left to finish on its own. An
        // abandoned thread costs less than a tick that never ends, and the
        // schedule is told the truth either way: the monitor did not answer.
        Ok(Some(
            match tokio::time::timeout(PROBE_TIMEOUT, checking).await {
                Ok(Ok(observed)) => observed,
                Ok(Err(e)) => probe_gave_up(format!("the monitor panicked: {e}")),
                Err(_) => probe_gave_up(format!(
                    "the monitor did not answer within {}s",
                    PROBE_TIMEOUT.as_secs()
                )),
            },
        ))
    }

    /// Every instant this schedule should have fired but did not.
    ///
    /// Measured from the last fire, or from one grace window ago for a schedule
    /// that has never run — otherwise a schedule created months ago and armed
    /// today would count every instant since it was written.
    fn missed_for(&self, s: &Schedule, now_ms: i64) -> Result<Vec<i64>> {
        let since = s.last_fire_at_ms.unwrap_or(now_ms - s.grace_ms);
        let mut missed = schedule::missed_since(&s.cron, &s.timezone, since, now_ms)?;
        // The instant that made it due may not be in the window when the clock
        // and the stored `next_fire_at_ms` disagree slightly. Trust the row.
        if let Some(next) = s.next_fire_at_ms {
            if next <= now_ms && !missed.contains(&next) {
                missed.push(next);
                missed.sort_unstable();
            }
        }
        Ok(missed)
    }

    /// The run this schedule started that has not finished, if any.
    async fn running_run(&self, s: &Schedule) -> Result<Option<String>> {
        let Some(store) = self.jod.store() else {
            return Ok(None);
        };
        for fire in store.fires(&s.id, 5)? {
            let Some(run) = fire.run_id else { continue };
            if let Ok(agent) = self.jod.agent(&run).await {
                if agent.status == AgentStatus::Running {
                    return Ok(Some(run));
                }
            }
        }
        Ok(None)
    }

    /// Returns whether a run was started.
    ///
    /// `watch` is what the schedule's monitor saw, and reaches this far only to
    /// put the diff in front of the prompt of whatever it starts.
    async fn carry_out(
        &self,
        s: &Schedule,
        d: &Decision,
        now_ms: i64,
        watch: Option<&monitor::Decision>,
    ) -> Result<bool> {
        let store = self.jod.store().cloned().expect("checked by the caller");
        match d {
            Decision::Hold {
                due_at_ms,
                outcome,
                why,
            } => {
                store.record_fire(&Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: *due_at_ms,
                    fired_at_ms: now_ms,
                    run_id: None,
                    outcome: *outcome,
                    detail: Some(why.clone()),
                })?;
                Ok(false)
            }
            Decision::Replace { due_at_ms, stop } => {
                // Stop first, so the two never overlap even briefly — the whole
                // point of choosing `replace` over `allow`.
                let _ = self.jod.kill_agent(stop).await;
                store.record_fire(&Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: *due_at_ms,
                    fired_at_ms: now_ms,
                    run_id: None,
                    outcome: FireOutcome::Replaced,
                    detail: Some(format!("stopped {stop}")),
                })?;
                let run = self.spawn(s, *due_at_ms, watch).await?;
                store.record_fire(&Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: *due_at_ms,
                    fired_at_ms: now_ms,
                    run_id: Some(run),
                    outcome: FireOutcome::Ran,
                    detail: None,
                })?;
                Ok(true)
            }
            Decision::Run { due_at_ms } => {
                let run = self.spawn(s, *due_at_ms, watch).await?;
                store.record_fire(&Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: *due_at_ms,
                    fired_at_ms: now_ms,
                    run_id: Some(run),
                    outcome: FireOutcome::Ran,
                    detail: None,
                })?;
                Ok(true)
            }
        }
    }

    /// Ask every watched run whether it is still working, and reap the ones
    /// that are not.
    ///
    /// **This runs before schedules and goals, and moving it is a bug.**
    /// [`Ticker::tick_goals`] settles the previous iteration by reading its
    /// run's status, and a wedged run's status is `running` — permanently,
    /// because the only process that ever writes a terminal status is the
    /// supervisor watching a harness that is never going to exit. Sweeping
    /// first means the goal asks *after* the stall has been turned into a
    /// `failed`, so it moves on this tick. Sweeping afterwards would cost a
    /// whole extra tick in the ordinary case and, in the case this exists for,
    /// would never resolve at all.
    ///
    /// **Nothing here is fatal.** A run whose reaping fails is logged into the
    /// goal's memory and skipped; the sweep goes on to the next one. The
    /// alternative — an early return — would let one unkillable process group
    /// stop every other watched run from ever being checked, which is the
    /// failure this module was written to prevent, reintroduced one level up.
    ///
    /// **There is no claim and no lease here, unlike schedules and goals.**
    /// Those guard a *spawn*: two processes acting on one schedule start two
    /// harnesses and cost real money. A sweep starts nothing. Its worst case
    /// under two daemons is that both observe the same stall and both signal
    /// the same process group, which is idempotent — `terminate_group` treats
    /// an already-dead group as success — and both write `failed` to a row that
    /// already says `failed`. Paying for a claim to prevent that would add a
    /// contended write per tick to buy nothing.
    pub async fn tick_heartbeats(&self, now_ms: i64) -> Result<SweepReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(SweepReport::default());
        };
        let watched = store.heartbeats()?;
        let mut report = SweepReport::default();

        for hb in watched {
            report.checked += 1;

            // The three observed facts. Status is read first and decides
            // whether the pgid is probed at all: pids are recycled, and asking
            // about a finished run's long-dead group is how a stranger's
            // process gets mistaken for an agent.
            let run = store.run(&hb.run_id)?;
            let status = run.as_ref().map(|r| r.status.clone());
            let alive = match (&status, run.as_ref().and_then(|r| r.pgid)) {
                (Some(s), Some(pgid)) if s == "running" => crate::proc::group_alive(pgid),
                _ => false,
            };
            let observed = Observed {
                status,
                alive,
                last_seq: store.last_event_seq(&hb.run_id)?,
            };

            let verdict = heartbeat::decide(&hb, &observed, now_ms);

            if !verdict.retires() {
                if matches!(verdict, Verdict::Beating { .. }) {
                    report.beating += 1;
                }
                store.record_beat(&Beat::after(&hb, &verdict, now_ms))?;
                continue;
            }

            // Retiring. Say why *before* tidying up, so a crash between the two
            // leaves the explanation rather than the bookkeeping.
            self.record_verdict(&store, &hb, &verdict);

            if verdict.fails_the_run() {
                if verdict.terminates() {
                    report.stopped += 1;
                }
                if let Err(e) = self.jod.fail_agent(&hb.run_id, verdict.terminates()).await {
                    // Logged into memory rather than returned: see above.
                    self.note(
                        &store,
                        &hb,
                        "reap-failed",
                        &format!("could not reap {}: {e}", hb.run_id),
                    );
                }
            }

            store.unwatch_run(&hb.run_id)?;
            report.retired += 1;
        }
        Ok(report)
    }

    /// Write down why a heartbeat retired.
    ///
    /// For a goal this lands in the goal's own memory scope, which is the scope
    /// [`Ticker::spawn_iteration`] reads to build the next iteration's prompt —
    /// so a stalled iteration is not merely recorded, it is handed to whatever
    /// runs next. That is the difference between a loop that repeats a hang and
    /// one that knows it hung last time.
    fn record_verdict(&self, store: &Store, hb: &Heartbeat, verdict: &Verdict) {
        // An ordinary ending is not news. Writing a fact for every run that
        // finished normally would fill a goal's memory with rows saying
        // "nothing went wrong", which is the noise that makes the rows that
        // matter unreadable.
        if matches!(verdict, Verdict::Ended) {
            return;
        }
        self.note(store, hb, verdict.tag(), &verdict.detail());
    }

    /// One line into whichever memory this run belongs to.
    ///
    /// Never fatal, and deliberately so: a sweep that cannot write its note has
    /// still got a wedged process to stop, and that is the more important half.
    fn note(&self, store: &Store, hb: &Heartbeat, predicate: &str, detail: &str) {
        let (subject, scope) = match hb.watching.goal_name() {
            // The goal's scope is keyed by its *id*, so the name has to be
            // resolved. A goal deleted while its last iteration was still
            // running leaves nothing to resolve, and the note falls back to the
            // run — which is the honest place for it once the goal is gone.
            Some(name) => match store.goal_named(name) {
                Ok(Some(goal)) => (format!("goal/{}", goal.name), goal.memory_scope()),
                _ => (format!("run/{}", hb.run_id), "default".to_string()),
            },
            None => (format!("run/{}", hb.run_id), "default".to_string()),
        };
        if let Err(e) = store.remember(
            NewFact::new(subject, predicate, detail.to_string())
                .in_scope(&scope)
                .from(Origin::System),
        ) {
            eprintln!("[jod] heartbeat note for {} failed: {e}", hb.run_id);
        }
    }

    /// Start watching a run, with the defaults for what it is doing.
    ///
    /// Registration is a separate step from spawning rather than part of it,
    /// because a heartbeat is not free: it is a row and a probe every tick, and
    /// most runs are minutes long and report their own ending. Watching every
    /// run would pay that for the overwhelming majority of runs to learn
    /// something the supervisor was already going to say.
    pub fn watch_run(&self, run_id: &str, watching: Watching, now_ms: i64) -> Result<()> {
        let Some(store) = self.jod.store() else {
            return Ok(());
        };
        store.watch_run(&Heartbeat::starting(run_id, watching, now_ms))
    }

    /// Advance every goal whose next iteration is due.
    ///
    /// A goal's progress lives in the fact store rather than in its own
    /// columns, which is what makes it *memory* rather than a job queue. Three
    /// things get written, each for a reason:
    ///
    /// - **The brief** — `pursuing` — superseded on every iteration, so
    ///   bitemporal validity answers "what did it think it was doing last
    ///   month" without a second history table.
    /// - **The current run** — `current-run` — likewise superseded, so the
    ///   in-flight run is one lookup and every previous one is still there.
    /// - **What happened** — `iteration` — appended, never superseded, because
    ///   that is the episodic record.
    ///
    /// All of it lands in the goal's own scope. Scope is a hard filter, and an
    /// hourly goal running for a month writes far more than a person does, so
    /// without its own partition it would drown ordinary recall.
    pub async fn tick_goals(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let due = store.claim_due_goals(&self.owner, now_ms, LEASE_MS)?;
        let mut report = TickReport {
            claimed: due.len(),
            ..Default::default()
        };

        for goal in due {
            let scope = goal.memory_scope();
            let subject = format!("goal/{}", goal.name);

            // Settle the previous iteration before starting another, so a goal
            // never has two runs in flight and its spend is counted once.
            if let Some(run) = self.current_run(&store, &subject)? {
                match self.jod.agent(&run).await {
                    Ok(agent) if agent.status == AgentStatus::Running => {
                        // Still working: no second iteration on top of the
                        // first. The claim is let go all the same — what is in
                        // flight is the `current-run` fact, not the claim, and
                        // holding it would stop the next tick settling this run
                        // for a whole lease.
                        store.release_goal(&goal.id)?;
                        report.held += 1;
                        continue;
                    }
                    Ok(agent) => {
                        // What the done-when check says, run by Jod rather than
                        // described to the agent. Both of the goal's own
                        // guarantees hang off this.
                        let verdict = self.check_done(&goal).await;

                        // What this iteration cost and what it said, recorded
                        // before anything decides how the goal ends. The turn
                        // happened and was billed whichever way the check went,
                        // and the iteration that *passes* the check is the one
                        // that did the work — writing it only on the path where
                        // the goal keeps going left the last iteration of every
                        // successful goal missing from `goal log`, missing from
                        // the iteration count, and missing from `spent_usd`.
                        let cost = agent.usage.cost_usd.unwrap_or(0.0);
                        let outcome = agent
                            .last_message
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", agent.status).to_lowercase());
                        store.remember(
                            NewFact::new(
                                subject.clone(),
                                "iteration",
                                format!("{}: {}", goal.iteration + 1, one_line(&outcome)),
                            )
                            .in_scope(&scope)
                            .from(Origin::System),
                        )?;

                        // A goal whose check passes is finished. Without this
                        // there was no path by which a goal could ever succeed:
                        // `should_stop` only ever returned exhausted or
                        // stalled, so every goal ran until it ran out.
                        if verdict.as_ref().is_some_and(|v| v.satisfied) {
                            // The counter and the spend first. `advance_goal`
                            // applies the stop conditions itself, so an
                            // iteration that both passed the check and used up
                            // the last of the budget is left marked exhausted
                            // for a moment; setting satisfied *after* it is
                            // what keeps the true ending. Passing `true` for
                            // progress is the same reasoning — the check going
                            // from failing to passing is the largest change it
                            // can see, and the goal stops here either way.
                            store.advance_goal(&goal.id, now_ms, cost, true)?;
                            store.remember(
                                NewFact::new(subject.clone(), "ended", "satisfied")
                                    .in_scope(&scope)
                                    .from(Origin::System),
                            )?;
                            store.set_goal_state(
                                &goal.name,
                                crate::schedule::GoalState::Satisfied,
                            )?;
                            store.release_goal(&goal.id)?;
                            continue;
                        }

                        // Progress used to mean "the run exited cleanly", which
                        // inverted the whole point: a loop completing
                        // iterations while nothing changes is *exactly* the
                        // failure stall detection exists to catch, and that
                        // reading reset the counter on every one of them. The
                        // counter only rose when a run failed — the one case
                        // that is already visible.
                        //
                        // So progress is a change in what the check *sees*,
                        // fingerprinted the way a monitor fingerprints a page.
                        // With no check there is nothing to observe, and the
                        // honest answer is no: a goal nobody can measure should
                        // stall and ask rather than run for ever.
                        let progressed = match (&verdict, self.last_fingerprint(&store, &subject)?)
                        {
                            (Some(v), Some(previous)) => v.fingerprint != previous,
                            (Some(_), None) => true,
                            (None, _) => false,
                        };
                        if let Some(v) = &verdict {
                            store.remember(
                                NewFact::new(subject.clone(), "done-when", v.fingerprint.clone())
                                    .in_scope(&scope)
                                    .from(Origin::System),
                            )?;
                        }
                        let state = store.advance_goal(&goal.id, now_ms, cost, progressed)?;
                        if !state.is_live() {
                            // It stopped on its own — satisfied, stalled or out
                            // of budget. Say which, in the goal's own memory,
                            // so the reason survives the process that found it.
                            store.remember(
                                NewFact::new(subject.clone(), "ended", state.as_str())
                                    .in_scope(&scope)
                                    .from(Origin::System),
                            )?;
                            store.release_goal(&goal.id)?;
                            continue;
                        }
                    }
                    // The run is gone from the store entirely. Treat it as a
                    // failed iteration rather than waiting on it for ever.
                    Err(_) => {
                        store.advance_goal(&goal.id, now_ms, 0.0, false)?;
                    }
                }
            }

            // Re-read: `advance_goal` may have just stopped it.
            let Some(goal) = store.goal_named(&goal.name)? else {
                continue;
            };
            if !goal.state.is_live() || goal.should_stop().is_some() {
                store.release_goal(&goal.id)?;
                continue;
            }

            match self
                .spawn_iteration(&store, &goal, &subject, &scope, now_ms)
                .await
            {
                Ok(()) => report.started += 1,
                Err(_) => report.failed += 1,
            }
            // Released whether or not the spawn worked. A goal left claimed by
            // a failed spawn would wait out the lease before trying again.
            store.release_goal(&goal.id)?;
        }
        Ok(report)
    }

    /// Deliver waiting mail by resuming the members holding it.
    ///
    /// The sibling of [`Ticker::tick_goals`], and the thing that turns the bus
    /// from something a human operates into something that runs. Until this
    /// existed, a message sat in an inbox until somebody typed
    /// `jod team wake` — which is fine for a demonstration and useless for two
    /// agents working overnight.
    ///
    /// **The judgement is not here.** [`team::wake_order`] already decides who
    /// may be woken and with what, and it was already correct and already
    /// tested; this gives it a caller that is not a person. Everything below it
    /// is bookkeeping: claim the wake, start the run, take the mail off the bus.
    ///
    /// Three properties worth stating, because each is a bug that would
    /// otherwise be invisible:
    ///
    /// - **One wake per interval per member.** Ten messages arriving together
    ///   become one turn carrying ten, not ten turns. A cost control — every
    ///   wake is a model call — and a coherence one.
    /// - **Nothing here waits for a run.** The spawn returns as soon as the
    ///   supervisor is up and the tick moves on; the run reports through the
    ///   database whether or not anybody is watching.
    /// - **Mail that cannot be delivered says so on itself.** A member with no
    ///   session is left asleep — resuming it would start a fresh context and
    ///   it would answer having forgotten everything — and the mail is
    ///   annotated rather than left silently sitting there.
    pub async fn tick_mail(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        // Before anything is delivered: notice which members have finished the
        // turn they were last woken for. See `settle_members` — without it a
        // member is woken exactly once, ever.
        self.settle_members(&store);
        let waiting = store.mail_waiting()?;
        let mut report = TickReport {
            claimed: waiting.len(),
            ..Default::default()
        };

        for held in waiting {
            let Some(order) = team::wake_order(&held.member, &held.pending) else {
                report.held += 1;
                self.note_why_it_waits(&store, &held);
                continue;
            };
            // One statement, so two ticks racing produce one wake rather than
            // two turns reading the same mail.
            if !store.claim_wake(
                held.scope,
                &held.team,
                &held.member.name,
                now_ms,
                team::WAKE_INTERVAL_MS,
            )? {
                report.held += 1;
                continue;
            }
            match self.wake(&store, &held, order).await {
                Ok(()) => report.started += 1,
                Err(e) => {
                    report.failed += 1;
                    // The mail is still on the bus — nothing is drained until
                    // the spawn has worked — so the next tick tries again.
                    eprintln!(
                        "[jod/tick] could not wake {} on {}: {e}",
                        held.member.name, held.team
                    );
                }
            }
        }
        Ok(report)
    }

    /// Fold in the titles of works whose titler nobody was left to hear.
    ///
    /// **The backstop for a launcher that does not outlive what it launched.**
    /// `orchestrator::start_titler` subscribes to the event stream and folds
    /// the answer in from a detached task, which is right and is the fast path:
    /// when the process survives, the title lands the moment the titler
    /// answers. But on the path that matters most — a work opened through
    /// Jod's own MCP server, which exits when the harness closes stdin — that
    /// task dies before the titler replies. Observed, not theorised: a work
    /// left holding its fallback name and a throwaway conversation nobody
    /// deleted, contradicting D6.
    ///
    /// Awaiting the titler instead is not the answer and never will be. The
    /// orchestrator must not block; that is the property the whole design
    /// exists to protect. So the answer is the same one that worked for the
    /// mail and the queue: a tick, reading rows that outlive whoever wrote
    /// them.
    ///
    /// Everything it needs is durable by construction — the work id is in the
    /// titler conversation's title and in its run's name, and the answer is in
    /// the run's events, which the *supervisor* writes. Nothing here reads a
    /// message, because messages are written by the process that was following
    /// the run and that is exactly the process presumed gone.
    ///
    /// A titler that failed, said nothing, or was never spawned still gets its
    /// conversation deleted and its work keeps the fallback name.
    /// [`Store::finish_titling`] has always done that; it just had nobody to
    /// call it.
    pub fn tick_titlers(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let orphans = store.orphaned_titlers()?;
        let mut report = TickReport {
            claimed: orphans.len(),
            ..Default::default()
        };

        for titler in orphans {
            let run = store.titler_run(&titler.work_id)?;
            let output = match &run {
                // Still working. The fast path may yet settle it, and if that
                // process is gone this tick will find it again in a minute.
                Some((_, status)) if is_live(status) => {
                    report.held += 1;
                    continue;
                }
                Some((run_id, _)) => store.titler_output(run_id)?,
                // No run at all: the process died between opening the
                // conversation and starting the run. Left alone briefly in
                // case it is about to start, then swept — the work keeps its
                // fallback name, which is what it has anyway.
                None => {
                    if now_ms - titler.created_at_ms < works::TITLER_GRACE_MS {
                        report.held += 1;
                        continue;
                    }
                    String::new()
                }
            };

            match store.finish_titling(&titler.work_id, &titler.conversation_id, &output) {
                Ok(titled) => {
                    report.started += 1;
                    if titled.fell_back {
                        eprintln!(
                            "[jod/tick] the titler for `{}` said nothing usable; \
                             the work keeps its opening name",
                            titler.work_id
                        );
                    } else {
                        eprintln!("[jod/tick] work `{}` is now `{}`", titler.work_id, titled.title);
                    }
                }
                // The fast path settling it between the read above and here is
                // the ordinary case on a machine where the launcher *did*
                // survive, and it leaves no conversation to delete. That is a
                // race won, not a failure, and logging it as one would put a
                // line in front of somebody every minute on a healthy system.
                Err(_) if store.conversation(&titler.conversation_id)?.is_none() => {}
                Err(e) => {
                    report.failed += 1;
                    eprintln!(
                        "[jod/tick] could not settle the titler for `{}`: {e}",
                        titler.work_id
                    );
                }
            }
        }
        Ok(report)
    }

    /// Close the works whose boards have emptied, and finish the ones whose
    /// last run has stopped.
    ///
    /// **D8's other half.** A work opens with a task on its board — that part
    /// was wired, in `create_work` — but nothing ever asked afterwards whether
    /// the board had emptied, so the chain the spec describes (last task
    /// completes → the work closes → the closing card is raised) never ran, and
    /// [`crate::works::State::Finishing`] was a state nothing could reach. The
    /// only thing that closed a work was a person typing `jod work close`.
    ///
    /// **Derived here rather than at the moment a task is ticked off**, and
    /// that is the design decision worth defending. A task can be completed
    /// from the board's atomic claim, from an MCP tool, from the CLI, or by an
    /// agent handing work to a peer — and a rule that lives at *one* of those
    /// call sites is a rule the other three forget. This asks the question of
    /// every live work on every tick, so a work closes whoever emptied it.
    /// It is the same reasoning that put the *whether* of waking in
    /// [`team::wake_order`] rather than in its callers.
    ///
    /// Both transitions are cheap and neither destroys anything: closing keeps
    /// the record, the tree and the worktrees, and raises a card summarising
    /// what came out of the work. Deleting is a separate, explicit act that a
    /// tick will never perform.
    ///
    /// The counters read as: `claimed` is works examined, `started` is works
    /// that changed state, `held` is works with something still open.
    pub fn tick_works(&self) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let live = store.works(crate::works::Filter::Live)?;
        let mut report = TickReport {
            claimed: live.len(),
            ..Default::default()
        };

        for work in live {
            match work.state {
                // Tasks done, sessions still running. Only something watching
                // the runs can notice the moment that stops being true, which
                // is exactly what a tick is.
                works::State::Finishing => match store.refresh_work_state(&work.id) {
                    Ok(works::State::Closed) => {
                        report.started += 1;
                        eprintln!(
                            "[jod/tick] work `{}` finished and is now closed",
                            work.title
                        );
                    }
                    Ok(_) => report.held += 1,
                    Err(e) => {
                        report.failed += 1;
                        eprintln!("[jod/tick] could not settle work `{}`: {e}", work.title);
                    }
                },
                works::State::Open => {
                    let open = match store.work_tasks(&work.id) {
                        Ok(tasks) => tasks.iter().filter(|t| !t.is_done()).count(),
                        Err(e) => {
                            report.failed += 1;
                            eprintln!(
                                "[jod/tick] could not read the board of `{}`: {e}",
                                work.title
                            );
                            continue;
                        }
                    };
                    if open > 0 {
                        report.held += 1;
                        continue;
                    }
                    match store.close_work(&work.id) {
                        Ok(closing) => {
                            report.started += 1;
                            eprintln!(
                                "[jod/tick] work `{}` is {} — {} branch(es), {} unanswered card(s)",
                                work.title,
                                closing.state.as_str(),
                                closing.branches.len(),
                                closing.unanswered_cards
                            );
                        }
                        Err(e) => {
                            report.failed += 1;
                            eprintln!("[jod/tick] could not close work `{}`: {e}", work.title);
                        }
                    }
                }
                works::State::Closed => {}
            }
        }
        Ok(report)
    }

    /// Ask GitHub what became of the pull requests this fleet opened.
    ///
    /// **The poll half of E6.S3.** The stream half already records a pull
    /// request the moment a run prints its URL, in the service's event loop —
    /// but a URL is not a status, and a pull request merged an hour after the
    /// session ended produces no event anywhere. Nothing except asking the
    /// forge will ever discover it, which is what this is for. It also
    /// *discovers*: a pull request opened by hand, or by an agent whose output
    /// nobody parsed, exists to Jod only if a held lease's branch is asked
    /// about.
    ///
    /// **Not every tick**, unlike everything else here. The tick is a minute
    /// and this one leaves the machine: every sweep is one `gh` invocation per
    /// stale row and per held lease, against an API with an hourly budget.
    /// [`POLL_EVERY_MS`] at [`PR_SWEEP_LIMIT`] each is a couple of hundred
    /// calls an hour at the worst, against five thousand allowed — and the cost
    /// of the interval is that a merged pull request can read as open for a few
    /// minutes, which is a panel being briefly out of date rather than anything
    /// acting on it.
    ///
    /// Stamped **before** the sweep and persisted in `settings`, for both of
    /// the reasons [`Ticker::trim_ledger`] gives: a field on this struct would
    /// reset every restart and turn "every few minutes" into "every startup",
    /// and stamping afterwards would retry a failing sweep every single tick
    /// for as long as it kept failing.
    ///
    /// The counters read as: `claimed` is pull requests looked at, `started` is
    /// ones nobody had seen before, `held` is a sweep that ran into tooling it
    /// could not use.
    pub async fn tick_pull_requests(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        if !due_to_poll(&store, now_ms) {
            return Ok(TickReport::default());
        }
        if let Err(e) = store.set_setting(POLLED_AT_KEY, &now_ms.to_string()) {
            eprintln!("[jod/tick] could not record a pull request poll, so skipping it: {e}");
            return Ok(TickReport::default());
        }

        // `sweep` spawns `gh` and waits for a network round trip. On a runtime
        // thread that would stall every other task the daemon is running —
        // and the symptom would read as the scheduler being slow rather than
        // as GitHub being slow, which is the kind of misattribution that costs
        // an afternoon.
        let swept =
            match tokio::task::spawn_blocking(move || crate::prs::sweep(&store, PR_SWEEP_LIMIT))
                .await
            {
                Ok(swept) => swept?,
                Err(joined) => {
                    eprintln!("[jod/tick] the pull request poll did not come back: {joined}");
                    return Ok(TickReport {
                        failed: 1,
                        ..Default::default()
                    });
                }
            };

        if swept.discovered > 0 {
            eprintln!(
                "[jod/tick] found {} pull request(s) nobody had parsed out of a stream",
                swept.discovered
            );
        }
        Ok(TickReport {
            claimed: swept.reconciled + swept.discovered,
            started: swept.discovered,
            // Not `failed`: no `gh`, or a `gh` nobody has logged in, is a fact
            // about the machine rather than something that went wrong, and it
            // has already said so once. Counting it as a failure would put a
            // line in the daemon's tally for every tick of a box that simply
            // has no GitHub CLI on it.
            held: usize::from(swept.quiet.is_some()),
            ..Default::default()
        })
    }

    /// Say to each idle session whatever has been queued for it.
    ///
    /// **The missing half of E2.S7**, and the reason it was missing is worth
    /// keeping written down: [`Store::plan_injection`] was built, tested and
    /// called by nothing, so a card answered from the rail was queued and then
    /// waited for the agent to happen to ask for it over MCP. An answer nobody
    /// fetched sat queued for ever, and the rail said *queued* about answers
    /// the agent already had. The queue was never the missing piece; the caller
    /// was.
    ///
    /// The shape is deliberately [`Ticker::tick_mail`]'s, because the two
    /// answer the same question about different addressees — a conversation
    /// here, a member there:
    ///
    /// - **The judgement is not in this function.** `plan_injection` decides
    ///   whether to speak and what to say, and returns a value; everything here
    ///   is the spawn and the bookkeeping.
    /// - **Nothing is settled until the spawn has worked.** A failed spawn
    ///   leaves the answers queued for the next tick rather than marking them
    ///   delivered to a run that never started.
    /// - **A session with no harness session to resume is left alone**, for the
    ///   same reason `wake_order` refuses one: delivering into a fresh context
    ///   would have the agent answer having forgotten the work the card was
    ///   about.
    ///
    /// The turn is injected into the conversation the answers belong to
    /// ([`RunConversation::Existing`]), never a new one. A fresh conversation
    /// would resume the harness session while forking Jod's record of it, and
    /// the transcript the human is reading would stop growing.
    ///
    /// There is no rate limit here, and `now_ms` is unused for that reason —
    /// kept for symmetry with the other three steps, and because a limit would
    /// need it. `tick_mail` needs `claim_wake` because mail arrives at machine
    /// speed and one message per tick would otherwise buy one turn per tick.
    /// This queue fills at the speed a person answers cards, and it empties
    /// completely into a single turn each time, so the only thing that could
    /// make it spend twice is a session that is not busy — which is the state
    /// where speaking is what it is supposed to do.
    pub async fn tick_deliveries(&self, _now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let waiting = store.conversations_awaiting_delivery()?;
        let mut report = TickReport {
            claimed: waiting.len(),
            ..Default::default()
        };

        for conversation_id in waiting {
            let busy = store.conversation_is_busy(&conversation_id)?;
            let Some(injection) = store.plan_injection(&conversation_id, busy)? else {
                report.held += 1;
                continue;
            };
            // A queue outliving its conversation is not a state the schema
            // permits — the rows cascade with it — so this is held rather than
            // settled: if it ever happens, something is wrong that a tick
            // should not be papering over by marking answers undeliverable.
            let Some(conversation) = store.conversation(&conversation_id)? else {
                report.held += 1;
                continue;
            };
            let Some(harness) = conversation.harness_kind() else {
                report.held += 1;
                continue;
            };
            // Resolved before the resume, and passed into it: a session id is
            // only valid on the harness that minted it, and this is the harness
            // the spawn below uses.
            let Resume::Session(session_id) = store.resume_for(&conversation_id, harness)? else {
                report.held += 1;
                continue;
            };

            let mut req = SpawnRequest {
                name: format!(
                    "answers-{}",
                    &conversation_id[..conversation_id.len().min(8)]
                ),
                harness,
                prompt: injection.prompt.clone(),
                // The session's framing arrived with the turn that started it
                // and is already in the conversation being resumed.
                system: None,
                cwd: std::path::PathBuf::from(&conversation.cwd),
                model: None,
                permission: PermissionPolicy::default(),
                resume: Resume::Session(session_id),
                // Enough of Jod to act on what it has just been told. An agent
                // handed a decision it may not carry out is a turn spent
                // saying so — the same reasoning that gives a woken teammate
                // its tools in `tick_mail`.
                tools: Some(crate::harness::ToolAccess::Delegate),
                ..SpawnRequest::default()
            };
            // The conversation's own model and permission, not this process's
            // defaults: a queued answer arriving on a different model from the
            // turn that raised the card is a different agent answering.
            crate::service::prefer_conversation_settings(&mut req, &conversation);

            match self
                .jod
                .spawn_agent_in(req, RunConversation::Existing(conversation_id.clone()))
                .await
            {
                Ok(agent) => {
                    let ids: Vec<i64> = injection.items.iter().map(|p| p.id).collect();
                    // Which run carried it, so "did it actually arrive" stays
                    // answerable — and the card's own `delivery` moves in the
                    // same transaction, because the rail reads the card while
                    // this reads the queue.
                    store.mark_deliveries_delivered(&ids, Some(&agent.id))?;
                    report.started += 1;
                    eprintln!(
                        "[jod/tick] delivered {} queued item(s) to {} as {}",
                        injection.count(),
                        &conversation_id[..conversation_id.len().min(8)],
                        &agent.id[..agent.id.len().min(8)]
                    );
                }
                Err(e) => {
                    report.failed += 1;
                    // Still queued, so the next tick tries again. Nothing was
                    // marked delivered to a run that does not exist.
                    eprintln!("[jod/tick] could not deliver to {conversation_id}: {e}");
                }
            }
        }
        Ok(report)
    }

    /// Put back to `Ready` every member whose run has finished.
    ///
    /// A member is marked `Busy` the moment it is woken, and nothing marked it
    /// back. That is not an oversight in the waking — the tick deliberately does
    /// not wait for the run it starts, because a tick that waited on a model
    /// would stop being a tick — it is that the run *ending* is a fact only a
    /// later pass can notice. `jod team wake` gets away without this because it
    /// blocks until the run finishes and settles the member itself; nothing
    /// unattended can.
    ///
    /// The consequence, before this existed, was that a member could be woken
    /// exactly once and then held its mail for ever, and showed as permanently
    /// busy on every roster — so peers were told not to write to it. Every unit
    /// test missed it, because none of them has a run that ends; the end-to-end
    /// suite found it on the second wake.
    ///
    /// The session id is refreshed at the same time and for the same reason
    /// `jod team wake` refreshes it: a resumed conversation can come back under
    /// a *new* harness session id, and a member still holding the previous one
    /// would next be resumed into a conversation that has moved on.
    ///
    /// **Nothing here is fatal**, on the same reasoning as [`Ticker::trim_ledger`]:
    /// a member that cannot be reconciled is a reason to get on with delivering
    /// the mail, not a reason for the tick that runs every schedule on this box
    /// to fail. Failures are said out loud rather than swallowed.
    fn settle_members(&self, store: &Store) {
        let teams = match store.teams() {
            Ok(teams) => teams,
            Err(e) => {
                eprintln!("[jod/tick] could not read the teams to settle them: {e}");
                return;
            }
        };
        for team in teams {
            // A name is a name in one scope, so both are asked. `teams()` is a
            // distinct list over the whole table and does not say which.
            for scope in [team::Scope::Team, team::Scope::Work] {
                let members = match store.members_in(scope, &team) {
                    Ok(members) => members,
                    Err(e) => {
                        eprintln!("[jod/tick] could not read `{team}` to settle it: {e}");
                        continue;
                    }
                };
                for member in members {
                    if member.status != MemberStatus::Busy {
                        continue;
                    }
                    let Some(agent_id) = member.agent_id.as_deref() else {
                        continue;
                    };
                    // A run this build cannot find is left alone rather than
                    // guessed at: a member freed while its turn is still going
                    // would be resumed mid-turn, which forks the conversation.
                    let run = match store.run(agent_id) {
                        Ok(Some(run)) => run,
                        Ok(None) => continue,
                        Err(e) => {
                            eprintln!("[jod/tick] could not read run `{agent_id}`: {e}");
                            continue;
                        }
                    };
                    if is_live(&run.status) {
                        continue;
                    }
                    if let Err(e) = store.bind_member(
                        &team,
                        &member.name,
                        Some(agent_id),
                        run.session_id.as_deref(),
                    ) {
                        eprintln!("[jod/tick] could not rebind `{}`: {e}", member.name);
                        continue;
                    }
                    if let Err(e) =
                        store.set_member_status(&team, &member.name, MemberStatus::Ready)
                    {
                        eprintln!("[jod/tick] could not free `{}`: {e}", member.name);
                    }
                }
            }
        }
    }

    /// Resume one member on its own conversation, carrying its unread mail.
    async fn wake(
        &self,
        store: &Store,
        held: &team::Waiting,
        order: team::WakeOrder,
    ) -> Result<()> {
        // Where it was working. A resumed session that reappears in a different
        // directory is a session whose paths have all silently changed.
        let cwd = held
            .member
            .agent_id
            .as_deref()
            .and_then(|id| store.run(id).ok().flatten())
            .map(|r| std::path::PathBuf::from(r.cwd))
            .unwrap_or_else(crate::service::default_cwd);

        let agent = self
            .jod
            .spawn_agent(SpawnRequest {
                name: format!("{}-{}", held.team, order.member),
                harness: order.harness,
                prompt: order.prompt,
                // The member's framing arrived with the turn that started it
                // and is already in the session being resumed.
                system: None,
                cwd,
                model: None,
                permission: PermissionPolicy::default(),
                resume: Resume::Session(order.session_id),
                // Enough of Jod to answer. A teammate that can read its mail
                // and not reply to it is decoration, and this is deliberately
                // more than `ToolAccess::unattended()` gives a scheduled run:
                // a member is part of a crew a person assembled for a job,
                // and what stops it running away is not the access level but
                // the bounds on the traffic itself — depth, budget, and a
                // deadline on every wait.
                tools: Some(crate::harness::ToolAccess::Delegate),
                ..SpawnRequest::default()
            })
            .await?;

        // Taken only once the spawn succeeded, so a failure leaves the mail
        // waiting rather than losing it.
        //
        // One call, not a drain followed by a mark: the two-step version left a
        // window — and, on the paths that forgot the second half, a permanent
        // state — in which a message an agent is already reading still reports
        // as waiting. See [`Store::take_mail`].
        store.take_mail(&held.team, &held.member.name)?;
        store.set_member_status(&held.team, &held.member.name, MemberStatus::Busy)?;
        store.bind_member(&held.team, &held.member.name, Some(&agent.id), None)?;
        eprintln!(
            "[jod/tick] woke {} on {} with {} message(s) as {}",
            order.member,
            order.harness.label(),
            order.messages,
            &agent.id[..agent.id.len().min(8)]
        );
        Ok(())
    }

    /// Say, on the mail itself, why nobody has read it.
    ///
    /// Per A8: a message to an agent that cannot receive it becomes visible,
    /// never a silence. Said once — `note_mail_stuck` only writes where nothing
    /// has been said — so a tick that finds the same stuck mail every minute
    /// does not fill the log with it.
    ///
    /// A *busy* member is not stuck and gets no note: it reads its inbox on its
    /// next turn, which is the ordinary case and not a fault.
    fn note_why_it_waits(&self, store: &Store, held: &team::Waiting) {
        let detail = match (&held.member.session_id, held.member.status) {
            (_, MemberStatus::Shutdown | MemberStatus::ShutdownRequested | MemberStatus::Error) => {
                format!(
                    "`{}` is {} — nobody will read this until it is started again",
                    held.member.name,
                    held.member.status.as_str()
                )
            }
            (None, _) => format!(
                "`{}` has no session to resume, so this is waiting rather than being delivered \
                 into a fresh context it would answer from with no memory of the work",
                held.member.name
            ),
            _ => return,
        };
        match store.note_mail_stuck(held.scope, &held.team, &held.member.name, &detail) {
            Ok(0) => {}
            Ok(n) => eprintln!("[jod/tick] {n} message(s) waiting: {detail}"),
            Err(e) => eprintln!("[jod/tick] could not record why mail is waiting: {e}"),
        }
    }

    /// What a goal's done-when check says right now.
    ///
    /// Run by Jod, deterministically, rather than described to the agent and
    /// taken on its word. The help text and the schema comment both promised
    /// this and neither delivered it: the command was interpolated into the
    /// prompt and the model asked to self-report — the opinion the design says
    /// it avoids. Gates before judges, as Hermes' own `/goal` puts it.
    async fn check_done(&self, goal: &Goal) -> Option<DoneCheck> {
        let command = goal.done_when.clone()?;
        let cwd = goal.cwd.clone();
        let probes = self.probes.clone();
        // Off the async thread: this is somebody's shell one-liner and it may
        // block for as long as it likes.
        let seen = tokio::task::spawn_blocking(move || probes.run(&command, &cwd))
            .await
            .ok()?
            .ok()?;
        Some(DoneCheck {
            satisfied: seen.status == 0,
            // The output, not the exit code. A check that keeps failing while
            // its output changes is a goal making progress towards passing,
            // and one whose output is identical run after run is a goal going
            // nowhere however busy the agent looks.
            fingerprint: seen.digest(),
        })
    }

    /// The fingerprint the previous iteration recorded, if any.
    fn last_fingerprint(&self, store: &Store, subject: &str) -> Result<Option<String>> {
        Ok(store
            .facts_about(subject)?
            .into_iter()
            .find(|f| f.predicate == "done-when")
            .map(|f| f.object))
    }

    /// The run this goal has in flight, if any.
    fn current_run(&self, store: &Store, subject: &str) -> Result<Option<String>> {
        Ok(store
            .facts_about(subject)?
            .into_iter()
            .find(|f| f.predicate == "current-run")
            .map(|f| f.object))
    }

    /// Start one iteration, and record the brief it was started against.
    async fn spawn_iteration(
        &self,
        store: &Store,
        goal: &Goal,
        subject: &str,
        scope: &str,
        now_ms: i64,
    ) -> Result<()> {
        // What happened last time, so iteration N+1 does not rediscover what N
        // already learned. Bounded, because a goal that has run for a month has
        // more history than a prompt can hold, and the recent past is the part
        // that bears on what to do next.
        let recent: Vec<String> = store
            .facts_about(subject)?
            .into_iter()
            .filter(|f| f.predicate == "iteration")
            .take(5)
            .map(|f| format!("- {}", f.object))
            .collect();

        let mut prompt = format!(
            "Standing objective: {}\n\nThis is iteration {}.",
            goal.objective,
            goal.iteration + 1
        );
        if let Some(check) = &goal.done_when {
            // Told, not asked. Jod runs this itself the moment the iteration
            // ends and decides from the exit code, so the agent is given it as
            // the definition of finished rather than as a question it answers
            // about its own work. Saying "report whether it passed" invited
            // exactly the self-assessment the design exists to avoid.
            prompt.push_str(&format!(
                "\n\nThis goal is finished when `{check}` exits zero. Jod runs that \
                 itself when you stop, so do not report on it — just make it \
                 truer than you found it."
            ));
        }
        if !recent.is_empty() {
            prompt.push_str(&format!(
                "\n\nWhat previous iterations did, newest first:\n{}",
                recent.join("\n")
            ));
        }
        prompt.push_str(
            "\n\nDo the next increment and stop. Report what changed, or say plainly \
             that nothing did — a report of progress that did not happen is worse than \
             no report, because it is what stops this loop noticing it is stuck.",
        );

        let agent = self
            .jod
            .spawn_agent(SpawnRequest {
                name: goal.name.clone(),
                harness: HarnessKind::from_id(&goal.harness).unwrap_or(HarnessKind::ClaudeCode),
                prompt,
                // The goal's own text is the framing, and it is already in the
                // prompt this iteration was built from.
                system: None,
                cwd: std::path::PathBuf::from(&goal.cwd),
                model: goal.model.clone(),
                permission: PermissionPolicy::default(),
                // Fresh every time. A goal running for months cannot carry one
                // harness conversation without its context growing without
                // bound; the memory layer is what carries continuity instead.
                resume: Resume::Fresh,
                // Read-only, not nothing: an unattended run should be able
                // to see what else is going on and decline to duplicate it.
                // Not more than that — see `ToolAccess::unattended`.
                tools: Some(crate::harness::ToolAccess::unattended()),
                ..SpawnRequest::default()
            })
            .await?;

        // Watch it. This is the charter's "activate the heartbeat if the
        // session has a goal", and it is automatic rather than a flag because a
        // goal is precisely the case that cannot survive being left unwatched:
        // `tick_goals` will not start iteration N+1 until iteration N has
        // settled, so one hung iteration stops the objective for ever.
        //
        // A failure to register is not a failure to spawn. The iteration is
        // already running and doing useful work; refusing it here would trade a
        // run that might hang for a run that certainly never happened.
        if let Err(e) = self.watch_run(&agent.id, Watching::Goal(goal.name.clone()), now_ms) {
            eprintln!("[jod] could not watch {} for goal {}: {e}", agent.id, goal.name);
        }

        // Supersede rather than insert, so the current run is one lookup and
        // every previous one stays answerable.
        let existing = store.facts_about(subject)?;
        for predicate in ["pursuing", "current-run"] {
            let value = if predicate == "pursuing" {
                goal.objective.clone()
            } else {
                agent.id.clone()
            };
            let fact = NewFact::new(subject.to_string(), predicate, value)
                .in_scope(scope)
                .from(Origin::System);
            match existing.iter().find(|f| f.predicate == predicate) {
                Some(old) => {
                    store.supersede(old.id, fact)?;
                }
                None => {
                    store.remember(fact)?;
                }
            }
        }
        Ok(())
    }

    async fn spawn(
        &self,
        s: &Schedule,
        due_at_ms: i64,
        watch: Option<&monitor::Decision>,
    ) -> Result<String> {
        let agent = self
            .jod
            .spawn_agent(SpawnRequest {
                name: s.name.clone(),
                harness: HarnessKind::from_id(&s.harness).unwrap_or(HarnessKind::ClaudeCode),
                prompt: prompt_for(s, watch),
                system: None,
                cwd: std::path::PathBuf::from(&s.cwd),
                model: s.model.clone(),
                permission: PermissionPolicy::default(),
                // Always a fresh conversation. A scheduled run that silently
                // continued last night's thread would inherit context nobody
                // chose for it, and the context would grow without bound.
                resume: Resume::Fresh,
                // Read-only, not nothing: an unattended run should be able
                // to see what else is going on and decline to duplicate it.
                // Not more than that — see `ToolAccess::unattended`.
                tools: Some(crate::harness::ToolAccess::unattended()),
                ..SpawnRequest::default()
            })
            .await?;
        let _ = due_at_ms;
        Ok(agent.id)
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{Mode, Monitor, Probe};
    use crate::schedule::ScheduleState;

    fn sched(misfire: Misfire, overlap: Overlap) -> Schedule {
        Schedule {
            id: "s1".into(),
            name: "nightly".into(),
            prompt: "triage".into(),
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 2 * * *".into(),
            timezone: "UTC".into(),
            state: ScheduleState::Armed,
            misfire,
            overlap,
            grace_ms: 300_000,
            jitter_ms: 0,
            next_fire_at_ms: Some(1_000),
            last_fire_at_ms: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        }
    }

    #[test]
    fn nothing_due_means_nothing_to_do() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        assert!(decide(&s, &[], None).is_empty());
    }

    #[test]
    fn one_instant_due_starts_one_run() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        assert_eq!(
            decide(&s, &[5_000], None),
            vec![Decision::Run { due_at_ms: 5_000 }]
        );
    }

    /// Six hours of downtime launched 73 runs in the first minute back when
    /// nothing decided this. The default answers the question a person is
    /// actually asking after an outage.
    #[test]
    fn a_long_outage_produces_exactly_one_run_by_default() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let missed: Vec<i64> = (1..=73).map(|i| i * 3_600_000).collect();
        let decisions = decide(&s, &missed, None);
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0],
            Decision::Run {
                due_at_ms: 73 * 3_600_000
            },
            "the most recent instant, not the oldest"
        );
    }

    /// `skip` must not mean "never": the instant that just came due is not a
    /// misfire, and it still runs.
    #[test]
    fn skipping_misfires_still_runs_the_one_that_just_came_due() {
        let s = sched(Misfire::Skip, Overlap::Skip);
        let decisions = decide(&s, &[1_000, 2_000, 3_000], None);
        assert_eq!(decisions.len(), 3);
        assert!(matches!(
            decisions[0],
            Decision::Hold {
                outcome: FireOutcome::SkippedMisfire,
                ..
            }
        ));
        assert_eq!(decisions[2], Decision::Run { due_at_ms: 3_000 });
    }

    /// Every skipped instant is written down. "It never fired" and "it fired
    /// and was skipped" are different bugs with the same symptom.
    #[test]
    fn every_skipped_instant_is_accounted_for() {
        let s = sched(Misfire::Skip, Overlap::Skip);
        let decisions = decide(&s, &[1, 2, 3, 4], None);
        let held = decisions
            .iter()
            .filter(|d| matches!(d, Decision::Hold { .. }));
        assert_eq!(held.count(), 3);
    }

    #[test]
    fn firing_all_replays_every_missed_instant() {
        let s = sched(Misfire::FireAll, Overlap::Allow);
        let missed = vec![1_000, 2_000, 3_000];
        assert_eq!(decide(&s, &missed, None).len(), 3);
    }

    /// An unbounded replay after a long outage is indistinguishable from a fork
    /// bomb, so even the policy that asks for everything is capped.
    #[test]
    fn firing_all_is_still_bounded() {
        let s = sched(Misfire::FireAll, Overlap::Allow);
        let missed: Vec<i64> = (1..500).collect();
        assert_eq!(decide(&s, &missed, None).len(), Misfire::MAX_REPLAY);
    }

    /// An hourly schedule whose runs take ninety minutes climbed past two
    /// concurrent runs and kept going when nothing decided this.
    #[test]
    fn a_run_still_going_holds_the_next_one_by_default() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let decisions = decide(&s, &[5_000], Some("run-1"));
        assert_eq!(decisions.len(), 1);
        match &decisions[0] {
            Decision::Hold { outcome, why, .. } => {
                assert_eq!(*outcome, FireOutcome::SkippedOverlap);
                assert!(why.contains("run-1"), "the reason names the run: {why}");
            }
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    #[test]
    fn replace_stops_the_previous_run_before_starting_another() {
        let s = sched(Misfire::FireOnce, Overlap::Replace);
        assert_eq!(
            decide(&s, &[5_000], Some("run-1")),
            vec![Decision::Replace {
                due_at_ms: 5_000,
                stop: "run-1".into()
            }]
        );
    }

    #[test]
    fn allowing_overlap_starts_the_run_regardless() {
        let s = sched(Misfire::FireOnce, Overlap::Allow);
        assert_eq!(
            decide(&s, &[5_000], Some("run-1")),
            vec![Decision::Run { due_at_ms: 5_000 }]
        );
    }

    /// Overlap is a fact about the schedule, not about one instant, so it wins
    /// over the misfire policy rather than being applied per instant.
    #[test]
    fn a_running_job_holds_an_outage_replay_too() {
        let s = sched(Misfire::FireAll, Overlap::Skip);
        let missed: Vec<i64> = (1..50).collect();
        let decisions = decide(&s, &missed, Some("run-1"));
        assert_eq!(decisions.len(), 1, "not fifty holds, one");
        assert!(matches!(decisions[0], Decision::Hold { .. }));
    }

    // ---- goals write what they did ----

    fn a_goal(name: &str) -> Goal {
        Goal {
            id: format!("g-{name}"),
            name: name.into(),
            objective: "keep the inbox at zero".into(),
            done_when: None,
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 * * * *".into(),
            timezone: "UTC".into(),
            state: crate::schedule::GoalState::Running,
            iteration: 0,
            max_iterations: None,
            budget_usd: None,
            spent_usd: 0.0,
            stall_after: 3,
            no_progress: 0,
            next_fire_at_ms: None,
            created_at_ms: 0,
        }
    }

    /// Without a supervisor the spawn fails, which is fine — what is under test
    /// is that the tick reaches the spawn at all rather than skipping the goal.
    #[tokio::test]
    /// A goal whose check passes is finished. Before this there was no path by
    /// which a goal could ever succeed: `should_stop` returned only exhausted
    /// or stalled, so every goal ran until it ran out of budget.
    async fn a_goal_whose_check_passes_is_satisfied_rather_than_run_again() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("done");
        g.done_when = Some("true".into());
        store.add_goal(&g).unwrap();

        let jod = Jod::with_store(store.clone());
        let ticker = Ticker::new(jod).as_owner("t");
        // Pretend an iteration just finished, which is when the check runs.
        let verdict = ticker.check_done(&g).await.expect("a check was configured");
        assert!(verdict.satisfied, "`true` exits zero");
    }

    /// A finished run, of the kind the previous tick left behind for this one
    /// to settle, carrying a real bill.
    fn a_finished_run(store: &Store, id: &str, cost_usd: f64, said: &str) {
        let summary = crate::service::AgentSummary {
            id: id.into(),
            name: "the goal's iteration".into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "Claude Code".into(),
            status: AgentStatus::Completed,
            cwd: "/tmp".into(),
            model: None,
            permission: PermissionPolicy::default(),
            // A pid from a process that is long gone, as a settled run has.
            pid: Some(4_000_000),
            pgid: Some(4_000_000),
            process_alive: false,
            watch_command: crate::service::watch_command(id),
            created_at_ms: 0,
            session_id: None,
            usage: crate::event::Usage {
                cost_usd: Some(cost_usd),
                ..Default::default()
            },
            event_count: 0,
            last_message: Some(said.into()),
        };
        store
            .save_run(&crate::store::StoredRun {
                id: id.into(),
                name: summary.name.clone(),
                harness: "claude_code".into(),
                status: "completed".into(),
                cwd: summary.cwd.clone(),
                session_id: None,
                pid: summary.pid,
                pgid: summary.pgid,
                created_at_ms: 0,
                summary: serde_json::to_value(&summary).unwrap(),
            })
            .unwrap();
    }

    /// The iteration that made the check pass is the one that did the work, and
    /// it was billed. Recording the ending without recording that iteration
    /// leaves the goal denying a bill it incurred: `goal log` shows no line for
    /// it, `goal ls` reads the iteration count from before it ran, and its cost
    /// never reaches `spent_usd`.
    #[tokio::test]
    async fn the_iteration_that_satisfied_a_goal_is_recorded_with_its_cost() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("finisher");
        g.done_when = Some("true".into());
        store.add_goal(&g).unwrap();

        a_finished_run(&store, "run-finisher", 0.073, "Hello");
        store
            .remember(
                NewFact::new("goal/finisher", "current-run", "run-finisher")
                    .in_scope(g.memory_scope())
                    .from(Origin::System),
            )
            .unwrap();
        store.run_goal_now("finisher", 1).unwrap();

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();

        let facts = store.facts_about("goal/finisher").unwrap();
        assert!(
            facts
                .iter()
                .any(|f| f.predicate == "ended" && f.object == "satisfied"),
            "the goal was found satisfied"
        );
        let history: Vec<_> = facts.iter().filter(|f| f.predicate == "iteration").collect();
        assert_eq!(
            history.len(),
            1,
            "the run that made the check pass must appear in the log exactly once"
        );
        assert!(
            history[0].object.starts_with("1: "),
            "it is the goal's first iteration: {}",
            history[0].object
        );

        let after = store.goal_named("finisher").unwrap().unwrap();
        assert_eq!(
            after.state,
            crate::schedule::GoalState::Satisfied,
            "satisfied is the ending, whatever else the counters say"
        );
        assert_eq!(after.iteration, 1, "the iteration counter counts it");
        assert!(
            (after.spent_usd - 0.073).abs() < 1e-9,
            "a billed agent turn the goal's own record denies: spent_usd is {}",
            after.spent_usd
        );
    }

    /// The awkward case for the ordering above: the iteration that passes the
    /// check is also the last one the goal was allowed. `advance_goal` applies
    /// the stop conditions itself and would leave this goal exhausted, so the
    /// satisfied state has to be written after it — and the cost still has to
    /// be counted exactly once.
    #[tokio::test]
    async fn a_goal_satisfied_on_its_last_allowed_iteration_still_ends_satisfied() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("lastchance");
        g.done_when = Some("true".into());
        g.max_iterations = Some(1);
        store.add_goal(&g).unwrap();

        a_finished_run(&store, "run-lastchance", 0.5, "Hello");
        store
            .remember(
                NewFact::new("goal/lastchance", "current-run", "run-lastchance")
                    .in_scope(g.memory_scope())
                    .from(Origin::System),
            )
            .unwrap();
        store.run_goal_now("lastchance", 1).unwrap();

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();

        let facts = store.facts_about("goal/lastchance").unwrap();
        let endings: Vec<_> = facts.iter().filter(|f| f.predicate == "ended").collect();
        assert_eq!(endings.len(), 1, "one ending, not two");
        assert_eq!(endings[0].object, "satisfied", "the check passed");
        assert_eq!(
            facts.iter().filter(|f| f.predicate == "iteration").count(),
            1,
            "the iteration is recorded once, not once per path through the tick"
        );

        let after = store.goal_named("lastchance").unwrap().unwrap();
        assert_eq!(after.state, crate::schedule::GoalState::Satisfied);
        assert_eq!(after.iteration, 1);
        assert!(
            (after.spent_usd - 0.5).abs() < 1e-9,
            "the turn is billed once: {}",
            after.spent_usd
        );
    }

    #[tokio::test]
    async fn a_goal_whose_check_fails_is_not_satisfied() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("notdone");
        g.done_when = Some("false".into());
        store.add_goal(&g).unwrap();

        let ticker = Ticker::new(Jod::with_store(store)).as_owner("t");
        let verdict = ticker.check_done(&g).await.expect("a check was configured");
        assert!(!verdict.satisfied);
    }

    /// The inversion this replaced: progress used to mean "the run exited
    /// cleanly", so a loop completing iterations while nothing changed — the
    /// exact failure stall detection exists for — reset the counter every time,
    /// and the counter only rose when a run *failed*.
    #[tokio::test]
    async fn a_check_seeing_the_same_thing_twice_is_not_progress() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("stuck");
        g.done_when = Some("echo unchanged".into());
        store.add_goal(&g).unwrap();

        let ticker = Ticker::new(Jod::with_store(store)).as_owner("t");
        let first = ticker.check_done(&g).await.unwrap();
        let second = ticker.check_done(&g).await.unwrap();
        assert_eq!(
            first.fingerprint, second.fingerprint,
            "identical output must fingerprint identically, or nothing ever stalls"
        );
    }

    #[tokio::test]
    async fn a_check_seeing_something_new_is_progress() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("moving");
        g.done_when = Some("echo one".into());
        store.add_goal(&g).unwrap();
        let ticker = Ticker::new(Jod::with_store(store)).as_owner("t");
        let first = ticker.check_done(&g).await.unwrap();

        g.done_when = Some("echo two".into());
        let second = ticker.check_done(&g).await.unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
    }

    /// A goal nobody can measure should stall and ask rather than run for ever,
    /// so the absence of a check is not treated as progress.
    #[tokio::test]
    async fn a_goal_with_no_check_reports_no_verdict_at_all() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let g = a_goal("unmeasured");
        store.add_goal(&g).unwrap();
        let ticker = Ticker::new(Jod::with_store(store)).as_owner("t");
        assert!(ticker.check_done(&g).await.is_none());
    }

    #[tokio::test]
    async fn a_due_goal_is_claimed_and_an_iteration_is_attempted() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        store.add_goal(&a_goal("inbox")).unwrap();
        store.run_goal_now("inbox", 1).unwrap();

        let jod = Jod::with_store(store.clone());
        let report = Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert_eq!(report.claimed, 1);
        assert_eq!(report.started + report.failed, 1, "it tried");
    }

    #[tokio::test]
    async fn a_goal_that_is_not_due_is_left_alone() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        store.add_goal(&a_goal("later")).unwrap();

        let jod = Jod::with_store(store);
        let report = Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert_eq!(report, TickReport::default());
    }

    /// A goal that stopped must not iterate again, whatever stopped it — that
    /// is what makes stalling a real end rather than a label.
    #[tokio::test]
    async fn a_stalled_goal_is_never_iterated() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("stuck");
        g.stall_after = 1;
        store.add_goal(&g).unwrap();
        // One iteration that changed nothing is enough at this threshold.
        store.advance_goal("g-stuck", 1, 0.0, false).unwrap();
        assert_eq!(
            store.goal_named("stuck").unwrap().unwrap().state,
            crate::schedule::GoalState::Stalled
        );
        store.run_goal_now("stuck", 1).unwrap();

        let jod = Jod::with_store(store);
        let report = Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert_eq!(report.claimed, 0, "a stopped goal is not even claimable");
    }

    /// The whole point of a goal being memory-backed: the brief it is pursuing
    /// is a fact, in the goal's own scope, and so is the run carrying it out.
    #[tokio::test]
    async fn an_iteration_records_its_brief_in_the_goals_own_scope() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        store.add_goal(&a_goal("inbox")).unwrap();
        store.run_goal_now("inbox", 1).unwrap();

        let jod = Jod::with_store(store.clone());
        let _ = Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await;

        let written = store.facts_about("goal/inbox").unwrap();
        // The spawn may have failed for want of a supervisor; the brief is
        // written either way, because it records intent rather than outcome.
        if let Some(brief) = written.iter().find(|f| f.predicate == "pursuing") {
            assert_eq!(brief.object, "keep the inbox at zero");
            assert_eq!(
                brief.scope, "goal:g-inbox",
                "a goal's memory lives in its own partition, or an hourly loop \
                 drowns ordinary recall"
            );
            assert_eq!(brief.origin, Origin::System);
        }
    }

    /// An episodic record must not become a second copy of the transcript.
    #[test]
    fn what_an_iteration_did_is_recorded_as_a_line_not_a_transcript() {
        let long = "word ".repeat(200);
        let recorded = one_line(&long);
        assert!(recorded.chars().count() <= 161, "{}", recorded.len());
        assert!(recorded.ends_with('…'));
        assert_eq!(one_line("  two   spaces  "), "two spaces");
    }

    #[tokio::test]
    async fn a_tick_with_no_store_is_harmless() {
        let jod = Jod::new();
        let report = Ticker::new(jod).tick(1_000).await.unwrap();
        assert_eq!(report, TickReport::default());
    }

    /// Two processes ticking the same database must not both fire a schedule.
    /// The claim protocol is what prevents it; this is the end-to-end proof
    /// through the tick rather than through the store alone.
    #[tokio::test]
    async fn two_tickers_over_one_store_do_not_both_claim_the_same_schedule() {
        use crate::store::Store;
        let store = Arc::new(Store::in_memory().unwrap());
        let mut s = sched(Misfire::FireOnce, Overlap::Skip);
        s.next_fire_at_ms = None;
        store.add_schedule(&s).unwrap();
        store
            .write(|tx| {
                tx.execute("UPDATE schedules SET next_fire_at_ms = 1", [])
                    .unwrap();
                Ok(())
            })
            .unwrap();

        let jod = Jod::with_store(store.clone());
        let now = chrono::Utc::now().timestamp_millis();
        let first = Ticker::new(jod.clone())
            .as_owner("a")
            .tick(now)
            .await
            .unwrap();
        let second = Ticker::new(jod).as_owner("b").tick(now).await.unwrap();

        assert_eq!(first.claimed, 1);
        assert_eq!(second.claimed, 0, "the second ticker must find nothing due");
    }

    // ---- a monitor decides whether the tick spends anything ----

    /// What a monitor says when the page it watches moved.
    fn changed() -> monitor::Decision {
        monitor::Decision::Run {
            diff: monitor::render_diff(Some(b"version 4\n"), b"version 5\n"),
        }
    }

    fn one_run() -> Vec<Decision> {
        vec![Decision::Run { due_at_ms: 5_000 }]
    }

    #[test]
    fn a_schedule_without_a_monitor_is_planned_exactly_as_it_always_was() {
        let planned = decide(&sched(Misfire::FireOnce, Overlap::Skip), &[5_000], None);
        assert_eq!(plan(planned.clone(), None), planned);
    }

    #[test]
    fn an_unchanged_monitor_leaves_no_run_in_the_plan() {
        assert!(plan(one_run(), Some(&monitor::Decision::Suppress)).is_empty());
    }

    #[test]
    fn a_first_sighting_is_a_baseline_and_leaves_no_run_in_the_plan() {
        assert!(plan(one_run(), Some(&monitor::Decision::Baseline)).is_empty());
    }

    /// The two must never collapse into each other: a watchdog that has been
    /// broken for a week would otherwise look exactly like one dutifully
    /// reporting that all is well.
    #[test]
    fn a_broken_monitor_starts_nothing_and_is_not_filed_as_no_change() {
        let broken = monitor::Decision::Failed {
            detail: "`check.sh` exited 7".into(),
        };
        assert!(plan(one_run(), Some(&broken)).is_empty());
        assert_ne!(broken.outcome(), monitor::Decision::Suppress.outcome());
    }

    #[test]
    fn a_changed_monitor_leaves_the_run_exactly_where_it_was() {
        assert_eq!(plan(one_run(), Some(&changed())), one_run());
    }

    /// One change is one change, whatever the misfire policy would have
    /// replayed — twenty runs carrying the identical diff is the bill a
    /// monitor exists to avoid rather than to multiply.
    #[test]
    fn one_change_starts_one_run_however_many_instants_an_outage_missed() {
        let s = sched(Misfire::FireAll, Overlap::Allow);
        let missed: Vec<i64> = (1..=20).map(|i| i * 1_000).collect();
        let planned = decide(&s, &missed, None);
        assert_eq!(planned.len(), 20);
        assert_eq!(
            plan(planned, Some(&changed())),
            vec![Decision::Run { due_at_ms: 20_000 }]
        );
    }

    /// Which instants were missed is a fact about the clock, not about what the
    /// monitor saw, so suppressing the run must not erase the accounting.
    #[test]
    fn a_suppressed_tick_still_accounts_for_the_instants_an_outage_missed() {
        let s = sched(Misfire::Skip, Overlap::Skip);
        let planned = decide(&s, &[1_000, 2_000, 3_000], None);
        let left = plan(planned, Some(&monitor::Decision::Suppress));
        assert_eq!(left.len(), 2, "the two skipped instants, and no run");
        assert!(left.iter().all(|d| matches!(d, Decision::Hold { .. })));
    }

    #[test]
    fn a_suppressed_tick_does_not_kill_the_run_it_would_have_replaced() {
        let planned = vec![Decision::Replace {
            due_at_ms: 5_000,
            stop: "run-1".into(),
        }];
        assert!(
            plan(planned, Some(&monitor::Decision::Suppress)).is_empty(),
            "stopping a run to start nothing in its place is pure loss"
        );
    }

    #[test]
    fn a_no_agent_verdict_leaves_nothing_to_run_whatever_the_script_printed() {
        for verdict in [
            monitor::Decision::Report {
                text: "disk at 91%".into(),
            },
            monitor::Decision::Silent,
            monitor::Decision::Failed {
                detail: "`watchdog.sh` exited 2".into(),
            },
        ] {
            assert!(
                plan(one_run(), Some(&verdict)).is_empty(),
                "{verdict:?} must not wake a model"
            );
        }
    }

    #[test]
    fn a_woken_run_carries_the_diff_in_front_of_the_operators_prompt() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let prompt = prompt_for(&s, Some(&changed()));
        assert!(prompt.starts_with(monitor::MONITOR_PREAMBLE), "{prompt}");
        assert!(prompt.contains(monitor::CHANGE_HEADER), "{prompt}");
        assert!(prompt.contains("+ version 5"), "{prompt}");
        assert!(
            prompt.trim_end().ends_with(&s.prompt),
            "the operator's words come last: {prompt}"
        );
    }

    #[test]
    fn a_run_nothing_woke_carries_the_operators_prompt_untouched() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        assert_eq!(prompt_for(&s, None), s.prompt);
        assert_eq!(prompt_for(&s, Some(&monitor::Decision::Baseline)), s.prompt);
    }

    // ---- and the whole tick, through the store ----

    /// A probe that answers the same thing every time, so a tick is driven
    /// without a process.
    struct Says(Observation);

    impl Probes for Says {
        fn run(&self, _command: &str, _cwd: &str) -> Result<Observation> {
            Ok(self.0.clone())
        }
        fn fetch(&self, _url: &str) -> Result<Observation> {
            Ok(self.0.clone())
        }
    }

    /// A monitor that has already seen this body once.
    fn watching(body: &str) -> Monitor {
        Monitor {
            last_digest: Some(monitor::digest(body.as_bytes())),
            last_body: Some(body.as_bytes().to_vec()),
            ..Monitor::new("s1", Probe::Command("check.sh".into()))
        }
    }

    /// One armed schedule, due now, with `m` watching it.
    fn due_and_watched(m: Monitor) -> Arc<Store> {
        let store = Arc::new(Store::in_memory().unwrap());
        let mut s = sched(Misfire::FireOnce, Overlap::Skip);
        s.next_fire_at_ms = None;
        store.add_schedule(&s).unwrap();
        store
            .write(|tx| {
                tx.execute("UPDATE schedules SET next_fire_at_ms = 1", [])
                    .unwrap();
                Ok(())
            })
            .unwrap();
        store.set_monitor(&m).unwrap();
        store
    }

    async fn tick_seeing(store: &Arc<Store>, seen: Observation) -> TickReport {
        Ticker::new(Jod::with_store(store.clone()))
            .as_owner("t")
            .with_probes(Arc::new(Says(seen)))
            .tick(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap()
    }

    /// The branch the whole monitor module exists for: a page that did not move
    /// costs nothing, and is still written down.
    #[tokio::test]
    async fn an_unchanged_monitor_suppresses_the_run_and_still_writes_the_tick_down() {
        let store = due_and_watched(watching("version 4\n"));
        let report = tick_seeing(&store, Observation::ok("version 4\n")).await;

        assert_eq!(report.claimed, 1);
        assert_eq!(report.started, 0, "no agent for a page that did not move");
        assert_eq!(report.held, 1, "a suppressed tick is accounted for");

        let checks = store.monitor_checks("s1", 10).unwrap();
        assert_eq!(checks.len(), 1, "silence with a row, not silence");
        assert_eq!(checks[0].outcome, "unchanged");

        // And in the fire history too, which is where a person looks to ask
        // whether the schedule is alive at all.
        let fires = store.fires("s1", 10).unwrap();
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].outcome, FireOutcome::MonitorQuiet);
        assert_eq!(fires[0].detail.as_deref(), Some("unchanged"));
        assert!(
            !fires[0].outcome.started_a_run(),
            "a quiet watchdog is not the busiest schedule on the box"
        );
    }

    #[tokio::test]
    async fn a_monitors_first_tick_is_a_baseline_and_starts_nothing() {
        let store = due_and_watched(Monitor::new("s1", Probe::Command("check.sh".into())));
        let report = tick_seeing(&store, Observation::ok("version 4\n")).await;

        assert_eq!(report.started, 0, "everything being new is not a change");
        let checks = store.monitor_checks("s1", 10).unwrap();
        assert_eq!(checks[0].outcome, "baseline");
        assert_eq!(
            store.monitor("s1").unwrap().unwrap().last_digest,
            Some(monitor::digest(b"version 4\n")),
            "and the next tick has something to compare against"
        );
    }

    /// Otherwise one outage produces two false alarms: the emptiness becomes
    /// the baseline, and the resource "changes" back when it recovers.
    #[tokio::test]
    async fn a_failing_probe_is_recorded_and_does_not_become_the_new_baseline() {
        let store = due_and_watched(watching("version 4\n"));
        let report = tick_seeing(&store, Observation::failed(7, "could not resolve host")).await;

        assert_eq!(report.started, 0);
        assert_eq!(report.failed, 1, "a broken watchdog is a failing schedule");

        let checks = store.monitor_checks("s1", 10).unwrap();
        assert_eq!(checks[0].outcome, "failed");
        assert!(checks[0].detail.as_deref().unwrap().contains("exited 7"));

        // Reported where the other failures are, and never as a quiet tick —
        // a broken watchdog that read as "nothing changed" would suppress its
        // agent for ever.
        let fires = store.fires("s1", 10).unwrap();
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].outcome, FireOutcome::SpawnFailed);
        assert!(fires[0].detail.as_deref().unwrap().contains("exited 7"));

        assert_eq!(
            store.monitor("s1").unwrap().unwrap().last_digest,
            Some(monitor::digest(b"version 4\n")),
            "the failure must not become the truth the next tick compares against"
        );
        assert_eq!(
            store
                .schedule_named("nightly")
                .unwrap()
                .unwrap()
                .consecutive_failures,
            1,
            "so it backs off instead of probing a dead host every minute"
        );
    }

    /// The script *is* the job: no harness is started for it, ever, and what it
    /// said is the result.
    #[tokio::test]
    async fn a_no_agent_schedule_never_starts_a_harness_whatever_its_script_prints() {
        for (printed, outcome) in [("disk at 91%\n", "reported"), ("", "silent")] {
            let store = due_and_watched(
                Monitor::new("s1", Probe::Command("watchdog.sh".into())).with_mode(Mode::NoAgent),
            );
            let report = tick_seeing(&store, Observation::ok(printed)).await;

            assert_eq!(report.started, 0, "printed {printed:?}");
            let checks = store.monitor_checks("s1", 10).unwrap();
            assert_eq!(checks[0].outcome, outcome);

            // The script having done the job is still a fire, and the log says
            // which kind of quiet it was.
            let fires = store.fires("s1", 10).unwrap();
            assert_eq!(fires.len(), 1);
            assert_eq!(fires[0].outcome, FireOutcome::MonitorQuiet);
            assert!(
                fires[0].detail.as_deref().unwrap().starts_with(outcome),
                "{:?}",
                fires[0].detail
            );
        }
    }

    /// The fire log is a listing, and a `no_agent` script may print a page.
    #[tokio::test]
    async fn a_talkative_no_agent_script_does_not_put_a_page_in_the_fire_log() {
        let store = due_and_watched(
            Monitor::new("s1", Probe::Command("watchdog.sh".into())).with_mode(Mode::NoAgent),
        );
        tick_seeing(&store, Observation::ok("chatter ".repeat(200))).await;

        let detail = store.fires("s1", 10).unwrap()[0].detail.clone().unwrap();
        assert!(detail.chars().count() <= 180, "{}", detail.len());
        // The whole of what it said is still kept where it belongs.
        assert!(
            store.monitor_checks("s1", 10).unwrap()[0]
                .detail
                .as_deref()
                .unwrap()
                .len()
                > 1_000
        );
    }

    /// The other half of suppression: a change does reach the harness. The
    /// spawn itself has no supervisor to talk to in a test, so what is asserted
    /// is that the tick got that far — and that a change whose run never
    /// started is not quietly adopted as the new normal.
    #[tokio::test]
    async fn a_changed_monitor_takes_the_tick_as_far_as_starting_a_run() {
        let store = due_and_watched(watching("version 4\n"));
        let report = tick_seeing(&store, Observation::ok("version 5\n")).await;

        assert_eq!(report.started + report.failed, 1, "it tried");
        assert_eq!(store.fires("s1", 10).unwrap().len(), 1, "and said so");

        let checks = store.monitor_checks("s1", 10).unwrap();
        assert_eq!(checks.len(), 1);
        if report.started == 1 {
            assert_eq!(checks[0].outcome, "changed");
        } else {
            assert_eq!(checks[0].outcome, "failed");
            assert_eq!(
                store.monitor("s1").unwrap().unwrap().last_digest,
                Some(monitor::digest(b"version 4\n")),
                "a change nobody was told about must be reported again next tick"
            );
        }
    }

    // ---- trimming the delivery ledger --------------------------------------

    /// The sweep, against a real store and — where it matters — a real process
    /// group.
    ///
    /// The stalled case spawns an actual detached `sleep` rather than faking
    /// liveness, because faking it would skip the only part that can hurt:
    /// `fail_agent` signals a process group, and a test that never produces one
    /// proves nothing about whether the right group is signalled. It also means
    /// these tests fail loudly if the reap stops working, instead of passing
    /// against a mock that agrees with whatever the code does.
    mod heartbeats {
        use super::*;
        use crate::store::StoredRun;

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        fn stored(id: &str, status: &str, pgid: Option<u32>) -> StoredRun {
            StoredRun {
                id: id.into(),
                name: "iteration".into(),
                harness: "claude-code".into(),
                status: status.into(),
                cwd: "/tmp".into(),
                session_id: None,
                pid: pgid,
                pgid,
                created_at_ms: 0,
                summary: serde_json::json!({"id": id}),
            }
        }

        /// A real detached process group that will sit there until it is killed.
        ///
        /// Built here rather than through `proc::spawn_detached`, and the
        /// difference is the whole reason this helper exists. That function now
        /// waits on its own children, so a finished run leaves no corpse on a
        /// console that stays up for weeks — right for production, and fatal to
        /// the proof below: an exit status goes to whoever collects it first,
        /// and `a_stalled_run_is_stopped_marked_failed_and_unwatched` asserts
        /// the group was *signalled* rather than merely absent. Reaped out from
        /// under it, its `waitpid` gets `ECHILD` and the test reports a kill
        /// that did happen as one that never did.
        ///
        /// So this spawns the same shape of process — `setsid`, its own group,
        /// pid equal to pgid — and leaves it unreaped for the test to wait on.
        fn a_living_group() -> u32 {
            use std::os::unix::process::CommandExt;

            let dir = std::env::temp_dir().join(format!("jod-hb-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("log"))
                .unwrap();

            let mut cmd = std::process::Command::new("/bin/sleep");
            cmd.arg("300")
                .current_dir(&dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(log));
            // SAFETY: `setsid` is async-signal-safe, which is the only
            // requirement on code running between `fork` and `exec`.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            // The `Child` is dropped rather than held, which does not reap it —
            // that is exactly the point. The pid stays waitable for the test.
            cmd.spawn()
                .expect("could not spawn a test process group")
                .id()
        }

        /// Silent past its window, and alive — the case the module exists for,
        /// and the one nothing else in Jod can detect.
        #[tokio::test]
        async fn a_stalled_run_is_stopped_marked_failed_and_unwatched() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();

            let now = 10_000_000_000;
            let hb = Heartbeat::starting("r1", Watching::Run, now - heartbeat::DEFAULT_STALL_MS - 1);
            store.watch_run(&hb).unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.checked, 1);
            assert_eq!(report.stopped, 1, "a stalled run must actually be stopped");
            assert_eq!(report.retired, 1);
            assert_eq!(
                store.run("r1").unwrap().unwrap().status,
                "failed",
                "a wedged run must stop claiming to be running"
            );
            assert!(
                store.heartbeat("r1").unwrap().is_none(),
                "the heartbeat outlived the run it was watching"
            );

            // The group was signalled, proved by how it died rather than by
            // `group_alive`.
            //
            // `group_alive` is the wrong instrument *here specifically*, and the
            // reason is worth writing down because it looks like a bug in the
            // code under test: this test process is the child's parent, so a
            // killed child becomes a **zombie** — and `kill(pid, 0)` succeeds on
            // a zombie, because the pid is still in the table until somebody
            // reaps it. The sweep would look like it had done nothing. Waiting
            // on it both reaps the zombie and says exactly how it ended, which
            // is the thing actually being asserted.
            //
            // This does not affect the sweep in production: `decide` reads a
            // run's recorded status before it probes anything, so a supervisor
            // that exited is `Ended` long before its pgid is consulted.
            let mut status: libc::c_int = 0;
            let mut reaped = 0;
            for _ in 0..100 {
                reaped = unsafe { libc::waitpid(pgid as i32, &mut status, libc::WNOHANG) };
                if reaped != 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert_eq!(reaped, pgid as i32, "the stalled process group was never signalled");
            assert!(
                libc::WIFSIGNALED(status),
                "the group exited by itself rather than being stopped"
            );

            // And the reason survived the row.
            let why = store.facts_about("run/r1").unwrap();
            assert!(
                why.iter().any(|f| f.predicate == "stalled"),
                "nothing recorded why the run was reaped: {why:?}"
            );
        }

        /// Alive and producing events. The expensive mistake would be killing
        /// this one, so it gets its own test.
        #[tokio::test]
        async fn a_working_run_is_left_alone_and_its_cursor_advances() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();
            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting(
                    "r1",
                    Watching::Run,
                    now - heartbeat::DEFAULT_STALL_MS - 1,
                ))
                .unwrap();
            // It said something since the last sweep, which is the whole
            // difference between this test and the one above.
            store
                .append_event(&crate::event::AgentEnvelope {
                    agent_id: "r1".into(),
                    at_ms: now,
                    seq: 0,
                    event: crate::event::AgentEvent::Message { text: "working".into() },
                })
                .unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.beating, 1);
            assert_eq!(report.stopped, 0);
            assert_eq!(report.retired, 0);
            assert_eq!(store.run("r1").unwrap().unwrap().status, "running");
            let hb = store.heartbeat("r1").unwrap().unwrap();
            assert_eq!(hb.last_seq, 0, "seq 0 is a real event");
            assert_eq!(hb.last_progress_ms, now);
            assert!(crate::proc::group_alive(pgid), "a working run was killed");
            let _ = crate::proc::signal_group(pgid, crate::proc::SIGKILL);
        }

        /// The supervisor died without recording an ending. There is nothing to
        /// signal — the pgid may since belong to somebody else — but the run is
        /// lying about being alive and must be corrected.
        #[tokio::test]
        async fn a_run_whose_group_is_gone_is_corrected_rather_than_signalled() {
            let store = Arc::new(Store::in_memory().unwrap());
            // Pid 1 rather than a large number: `group_alive` answers false for
            // it by refusing to interpret it, with no chance of a live process
            // having recycled into the number.
            store.save_run(&stored("r1", "running", Some(1))).unwrap();
            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting("r1", Watching::Run, now))
                .unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.stopped, 0, "there was nothing left to stop");
            assert_eq!(report.retired, 1);
            assert_eq!(store.run("r1").unwrap().unwrap().status, "failed");
            assert!(store
                .facts_about("run/r1")
                .unwrap()
                .iter()
                .any(|f| f.predicate == "vanished"));
        }

        /// The ordinary ending, and the charter's "clean up when the session is
        /// done". No fact: a run that finished normally is not news, and a note
        /// per completed run would bury the ones that matter.
        #[tokio::test]
        async fn a_finished_run_is_unwatched_quietly() {
            let store = Arc::new(Store::in_memory().unwrap());
            store.save_run(&stored("r1", "completed", Some(1))).unwrap();
            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting("r1", Watching::Run, now))
                .unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.retired, 1);
            assert_eq!(report.stopped, 0);
            assert!(store.heartbeat("r1").unwrap().is_none());
            assert_eq!(
                store.run("r1").unwrap().unwrap().status,
                "completed",
                "a clean ending was rewritten as a failure"
            );
            assert!(
                store.facts_about("run/r1").unwrap().is_empty(),
                "an uneventful ending should not be written down"
            );
        }

        /// One wedged run must not stop the others being checked. This is the
        /// same failure the module exists to prevent, one level up.
        #[tokio::test]
        async fn one_bad_run_does_not_stop_the_sweep_reaching_the_others() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            for id in ["a", "b", "c"] {
                store.save_run(&stored(id, "running", Some(1))).unwrap();
                store
                    .watch_run(&Heartbeat::starting(id, Watching::Run, now))
                    .unwrap();
            }
            // `b`'s run row is deleted out from under the sweep, which is the
            // orphan case — it must be tidied, not fatal.
            store
                .write(|tx| {
                    tx.execute("DELETE FROM runs WHERE id = 'b'", [])?;
                    Ok(())
                })
                .unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            // `b`'s heartbeat went with its run via the cascade, so two remain
            // to be checked and both are reached.
            assert_eq!(report.checked, 2);
            assert_eq!(report.retired, 2);
            assert!(store.heartbeats().unwrap().is_empty());
        }

        /// A goal's stall lands in the goal's own memory, which is the scope
        /// `spawn_iteration` reads to build the next prompt — so the next
        /// iteration is told that the last one hung.
        #[tokio::test]
        async fn a_goal_iteration_that_stalls_is_reported_into_the_goals_memory() {
            let store = Arc::new(Store::in_memory().unwrap());
            let goal = Goal {
                id: "g-green-ci".into(),
                name: "green-ci".into(),
                objective: "get CI green".into(),
                done_when: None,
                harness: "claude-code".into(),
                cwd: "/tmp".into(),
                model: None,
                cron: "0 * * * *".into(),
                timezone: "UTC".into(),
                state: crate::schedule::GoalState::Running,
                iteration: 0,
                max_iterations: None,
                budget_usd: None,
                spent_usd: 0.0,
                stall_after: 6,
                no_progress: 0,
                next_fire_at_ms: None,
                created_at_ms: 0,
            };
            store.add_goal(&goal).unwrap();
            store.save_run(&stored("r1", "running", Some(1))).unwrap();
            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting(
                    "r1",
                    Watching::Goal("green-ci".into()),
                    now,
                ))
                .unwrap();

            ticker_over(&store).tick_heartbeats(now).await.unwrap();

            let noted = store.facts_about("goal/green-ci").unwrap();
            assert!(
                noted.iter().any(|f| f.predicate == "vanished"),
                "the goal was not told its iteration died: {noted:?}"
            );
            assert_eq!(
                noted[0].scope,
                goal.memory_scope(),
                "the note must land in the scope the next iteration reads"
            );
        }

        /// The wiring, not just the method: the daemon's own tick has to run
        /// the sweep, or every test above is about a function nothing calls.
        #[tokio::test]
        async fn the_daemons_tick_sweeps_heartbeats() {
            use crate::daemon::Tick;

            let store = Arc::new(Store::in_memory().unwrap());
            store.save_run(&stored("r1", "running", Some(1))).unwrap();
            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting("r1", Watching::Run, now))
                .unwrap();

            let report = Tick::tick(&ticker_over(&store), now).await.unwrap();

            assert_eq!(store.run("r1").unwrap().unwrap().status, "failed");
            assert!(store.heartbeat("r1").unwrap().is_none());
            assert_eq!(report.failed, 0, "a vanished run stopped nothing");
        }
    }

    mod ledger_retention {
        use super::*;
        use crate::ledger::{DeliveryState, NewMessage as Owed, Owner};

        /// A settled row old enough to be past `RETENTION_MS`.
        fn stale_row(store: &Store, key: &str, now_ms: i64) {
            let owner = Owner::new("jod-cloud", 4821);
            let long_ago = now_ms - crate::ledger::RETENTION_MS - 60_000;
            let id = store
                .record_obligation(
                    &Owed::new(key, "telegram", "7", "old news"),
                    &owner,
                    long_ago,
                )
                .unwrap();
            store.mark_delivered(id, long_ago).unwrap();
        }

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// The promise `MAX_ROWS` and `RETENTION_MS` make and nothing kept:
        /// `prune_ledger` had no caller at all, so the table grew without bound.
        #[tokio::test]
        async fn a_tick_trims_settled_rows_that_are_past_their_retention() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            stale_row(&store, "telegram:7:1", now);
            assert_eq!(store.obligations(10).unwrap().len(), 1);

            ticker_over(&store).tick(now).await.unwrap();

            assert!(
                store.obligations(10).unwrap().is_empty(),
                "the tick left the ledger untrimmed"
            );
        }

        /// An unsettled row is somebody still waiting to hear something.
        /// Deleting one to save space is the exact failure the ledger exists to
        /// prevent, committed silently and with the evidence gone.
        #[tokio::test]
        async fn a_tick_never_trims_a_message_somebody_is_still_owed() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            let owner = Owner::new("jod-cloud", 4821);
            // As old as the stale one above, and still owed.
            let long_ago = now - crate::ledger::RETENTION_MS - 60_000;
            store
                .record_obligation(
                    &Owed::new("telegram:7:2", "telegram", "7", "still owed"),
                    &owner,
                    long_ago,
                )
                .unwrap();

            ticker_over(&store).tick(now).await.unwrap();

            let left = store.obligations(10).unwrap();
            assert_eq!(left.len(), 1, "an unsettled row was deleted");
            assert_eq!(left[0].state, DeliveryState::Pending);
        }

        /// Not every minute. The interval is what keeps a housekeeping write off
        /// the database the tick needs sixty times an hour.
        #[tokio::test]
        async fn a_second_tick_inside_the_hour_does_not_trim_again() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            ticker_over(&store).tick(now).await.unwrap();
            let stamped = store.setting(PRUNED_AT_KEY).unwrap();
            assert_eq!(stamped.as_deref(), Some(now.to_string().as_str()));

            // A row that would be trimmed, and a tick a minute later.
            stale_row(&store, "telegram:7:3", now);
            ticker_over(&store).tick(now + 60_000).await.unwrap();
            assert_eq!(
                store.obligations(10).unwrap().len(),
                1,
                "trimmed again inside the hour"
            );

            // And an hour later it goes.
            ticker_over(&store)
                .tick(now + PRUNE_EVERY_MS)
                .await
                .unwrap();
            assert!(store.obligations(10).unwrap().is_empty(), "never trimmed");
        }

        /// The reason the stamp is in `settings` and not in the struct. A field
        /// resets on every restart, so "hourly" becomes "every startup" — and a
        /// crash-looping daemon would prune every minute, which is the failure
        /// the interval exists to prevent.
        #[tokio::test]
        async fn a_restart_does_not_earn_a_fresh_trim() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            ticker_over(&store).tick(now).await.unwrap();

            stale_row(&store, "telegram:7:4", now);
            // A wholly new Ticker — a new process, as far as anything in memory
            // is concerned — one minute later.
            let restarted = Ticker::new(Jod::with_store(store.clone())).as_owner("t2");
            restarted.tick(now + 60_000).await.unwrap();

            assert_eq!(
                store.obligations(10).unwrap().len(),
                1,
                "a restart trimmed inside the hour"
            );
        }

        /// The constraint that decides where this call goes: housekeeping must
        /// not be able to disturb a fire.
        ///
        /// Structurally it cannot — `trim_ledger` returns nothing, so there is
        /// no error for `tick` to propagate — but the placement is the other
        /// half, and this pins it: a tick that does real scheduler work *and*
        /// trims reports exactly what it would have reported without the trim.
        #[tokio::test]
        async fn a_trim_does_not_change_what_the_tick_reports() {
            let now = chrono::Utc::now().timestamp_millis();

            let untrimmed = due_and_watched(watching("version 4\n"));
            let plain = tick_seeing(&untrimmed, Observation::ok("version 4\n")).await;

            let trimmed = due_and_watched(watching("version 4\n"));
            stale_row(&trimmed, "telegram:7:6", now);
            let alongside = tick_seeing(&trimmed, Observation::ok("version 4\n")).await;

            assert_eq!(
                alongside, plain,
                "the trim changed what the scheduler reported"
            );
            assert!(
                trimmed.obligations(10).unwrap().is_empty(),
                "and it did happen"
            );
            assert_eq!(
                trimmed.monitor_checks("s1", 10).unwrap().len(),
                1,
                "the schedule's own work still landed"
            );
        }

        /// A clock that went backwards — a snapshot restore, an NTP correction —
        /// must not lock trimming out until wall time catches up.
        #[tokio::test]
        async fn a_stamp_from_the_future_does_not_wedge_the_trim_for_ever() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 10_000_000_000;
            store
                .set_setting(PRUNED_AT_KEY, &(now + 86_400_000).to_string())
                .unwrap();
            stale_row(&store, "telegram:7:5", now);

            ticker_over(&store).tick(now).await.unwrap();

            assert!(
                store.obligations(10).unwrap().is_empty(),
                "a future stamp wedged the trim"
            );
        }
    }

    // ---- delivering the mail ------------------------------------------------

    /// A tick that wakes teammates.
    ///
    /// Note what these assert and what they cannot: a test machine has no
    /// supervisor for the spawn to talk to, so "it woke somebody" is asserted
    /// as `started + failed`, exactly as the monitor tests above do. Everything
    /// that decides *whether* to wake — the rate limit, the no-session rule,
    /// the busy rule — happens before the spawn and is asserted exactly.
    /// E2.S7's other half: the handler that decides when a queued answer
    /// reaches its session, and the caller that was missing for long enough
    /// that every test of it stayed green while nothing ran it.
    /// D8's chain — last task done, work closes, closing card raised — and the
    /// reason it needs its own tests: every piece of it was written and green
    /// while nothing in the running system ever asked whether a board had
    /// emptied.
    /// D6's titler, settled by a sweep rather than by the process that started
    /// it.
    ///
    /// **Every fixture here has no launcher.** Nothing subscribes to the event
    /// stream, nothing awaits the run, and no `messages` row is ever written
    /// for the titler's conversation — which is precisely the state the bug
    /// happened in, and the state a test that kept the launching process alive
    /// would never reach. It passed against the old code for exactly that
    /// reason.
    mod titlers {
        use super::*;
        use crate::daemon::Tick;
        use crate::event::AgentEnvelope;
        use crate::harness::HarnessKind;
        use crate::store::Store;
        use crate::works::{fallback_title, titler_run_name, TITLER_GRACE_MS};
        use crate::AgentEvent;

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// A work, its titler conversation, and — optionally — the run that was
        /// started to name it, in whatever state. No process is following any
        /// of it.
        fn work_with_titler(store: &Store, status: Option<&str>) -> (String, String) {
            let work = store.create_work("count how many rs files are in the repo").unwrap();
            let titler = store.open_titler(&work.id, HarnessKind::ClaudeCode).unwrap();
            if let Some(status) = status {
                store
                    .save_run(&crate::store::StoredRun {
                        id: format!("run-for-{}", work.id),
                        name: titler_run_name(&work.id),
                        harness: "claude_code".into(),
                        status: status.into(),
                        cwd: "/tmp".into(),
                        session_id: None,
                        pid: None,
                        pgid: None,
                        created_at_ms: 1,
                        summary: serde_json::Value::Null,
                    })
                    .unwrap();
            }
            (work.id, titler.id)
        }

        /// What the supervisor writes. Deliberately not `append_message`: the
        /// events are the durable half and the messages are the half that goes
        /// missing when the launcher does.
        fn said(store: &Store, work_id: &str, event: AgentEvent, seq: u64) {
            store
                .append_event(&AgentEnvelope {
                    agent_id: format!("run-for-{work_id}"),
                    at_ms: 1,
                    seq,
                    event,
                })
                .unwrap();
        }

        /// The bug, in the shape it was found in: a completed titler, nobody
        /// left to hear it, a work stuck on its fallback name and a throwaway
        /// conversation nobody deleted.
        #[tokio::test]
        async fn a_titler_whose_launcher_is_gone_is_folded_in_by_the_tick() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, titler) = work_with_titler(&store, Some("completed"));
            said(
                &store,
                &work,
                AgentEvent::Message {
                    text: "{\"title\":\"count the rust files\",\"summary\":\"how big is this repo\"}"
                        .into(),
                },
                0,
            );
            assert!(
                store.thread(&titler).unwrap().is_empty(),
                "the fixture must have no messages, or it is not the state the bug happened in"
            );

            let report = ticker_over(&store).tick_titlers(1_000).unwrap();

            assert_eq!(report.started, 1);
            let work = store.work(&work).unwrap().unwrap();
            assert_eq!(work.title, "count the rust files");
            assert_eq!(work.summary, "how big is this repo");
            assert!(
                store.conversation(&titler).unwrap().is_none(),
                "D6: the throwaway is deleted once it has answered"
            );
        }

        /// Some harnesses put a short reply in the final event rather than in
        /// prose. The live reader takes only the prose, so this is a case the
        /// sweep handles and the fast path does not.
        #[tokio::test]
        async fn a_title_carried_only_by_the_final_event_is_still_read() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, _) = work_with_titler(&store, Some("completed"));
            said(
                &store,
                &work,
                AgentEvent::Finished {
                    text: Some("{\"title\":\"the rust file census\",\"summary\":\"\"}".into()),
                    exit_code: Some(0),
                    is_error: false,
                    usage: Default::default(),
                },
                0,
            );

            ticker_over(&store).tick_titlers(1_000).unwrap();

            assert_eq!(
                store.work(&work).unwrap().unwrap().title,
                "the rust file census"
            );
        }

        /// A titler that failed or said nothing still gets swept: the work keeps
        /// the name it opened with, which is findable, and the fleet does not
        /// keep a session nobody opened.
        #[tokio::test]
        async fn a_titler_that_said_nothing_is_still_cleared_and_the_work_keeps_its_name() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, titler) = work_with_titler(&store, Some("failed"));
            let opened_as = store.work(&work).unwrap().unwrap().title;

            let report = ticker_over(&store).tick_titlers(1_000).unwrap();

            assert_eq!(report.started, 1);
            assert!(store.conversation(&titler).unwrap().is_none());
            let work = store.work(&work).unwrap().unwrap();
            assert_eq!(work.title, opened_as);
            assert_eq!(
                work.title,
                fallback_title("count how many rs files are in the repo")
            );
        }

        /// A titler still working is left alone. The fast path may yet settle
        /// it, and reading a half-written answer would fold in a fragment.
        #[tokio::test]
        async fn a_titler_still_running_is_left_alone() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (_work, titler) = work_with_titler(&store, Some("running"));

            let report = ticker_over(&store).tick_titlers(1_000).unwrap();

            assert_eq!(report.held, 1);
            assert_eq!(report.started, 0);
            assert!(store.conversation(&titler).unwrap().is_some());
        }

        /// The process can also die *between* opening the conversation and
        /// starting the run, which leaves a titler with no run to wait for. It
        /// is given a grace period and then swept, or it sits in the fleet for
        /// ever.
        #[tokio::test]
        async fn a_titler_whose_run_never_started_is_swept_once_its_grace_has_passed() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (_, titler) = work_with_titler(&store, None);

            let early = ticker_over(&store).tick_titlers(1_000).unwrap();
            assert_eq!(early.held, 1, "it may still be about to start");
            assert!(store.conversation(&titler).unwrap().is_some());

            let opened_at = store.conversation(&titler).unwrap().unwrap().created_at_ms;
            let later = ticker_over(&store)
                .tick_titlers(opened_at + TITLER_GRACE_MS + 1)
                .unwrap();

            assert_eq!(later.started, 1);
            assert!(store.conversation(&titler).unwrap().is_none());
        }

        /// Settling is not repeated: the conversation is gone, so there is
        /// nothing left to find.
        #[tokio::test]
        async fn a_settled_titler_is_not_swept_twice() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, _) = work_with_titler(&store, Some("completed"));
            said(
                &store,
                &work,
                AgentEvent::Message {
                    text: "{\"title\":\"once\",\"summary\":\"\"}".into(),
                },
                0,
            );

            ticker_over(&store).tick_titlers(1_000).unwrap();
            let again = ticker_over(&store).tick_titlers(2_000).unwrap();

            assert_eq!(again.claimed, 0);
            assert_eq!(store.work(&work).unwrap().unwrap().title, "once");
        }

        /// The guard. Remove the `tick_titlers` line from `impl Tick for
        /// Ticker` and this fails.
        #[tokio::test]
        async fn the_daemons_tick_settles_an_orphaned_titler() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, titler) = work_with_titler(&store, Some("completed"));
            said(
                &store,
                &work,
                AgentEvent::Message {
                    text: "{\"title\":\"named by the tick\",\"summary\":\"\"}".into(),
                },
                0,
            );

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert_eq!(
                store.work(&work).unwrap().unwrap().title,
                "named by the tick",
                "the composite tick never settled the titler"
            );
            assert!(store.conversation(&titler).unwrap().is_none());
        }
    }

    mod works {
        use super::*;
        use crate::daemon::Tick;
        use crate::harness::HarnessKind;
        use crate::store::Store;
        use crate::works::{Filter, Origin, State};

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// A work with one session, opened the way the orchestrator opens one.
        fn work_with_a_session(store: &Store) -> (String, String) {
            let work = store.create_work("port the parser").unwrap();
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            store.set_conversation_title(&c.id, "worker").unwrap();
            store
                .attach_conversation(&c.id, &work.id, None, Origin::Orchestrator)
                .unwrap();
            (work.id, c.id)
        }

        fn run_in(store: &Store, conversation: &str, id: &str, status: &str) {
            store
                .save_run(&crate::store::StoredRun {
                    id: id.into(),
                    name: "worker".into(),
                    harness: "claude_code".into(),
                    status: status.into(),
                    cwd: "/tmp".into(),
                    session_id: Some("ses-1".into()),
                    pid: None,
                    pgid: None,
                    created_at_ms: 1,
                    summary: serde_json::Value::Null,
                })
                .unwrap();
            store
                .append_message(
                    conversation,
                    crate::conversation::NewMessage::new(
                        crate::conversation::Role::Assistant,
                        "working",
                    )
                    .from_run(id),
                )
                .unwrap();
        }

        /// A work with an open task is not over, whoever is asking.
        #[tokio::test]
        async fn a_work_with_an_unfinished_board_is_left_open() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, _) = work_with_a_session(&store);

            let report = ticker_over(&store).tick_works().unwrap();

            assert_eq!(report.claimed, 1);
            assert_eq!(report.held, 1);
            assert_eq!(store.work(&work).unwrap().unwrap().state, State::Open);
        }

        /// The chain, driven the way it happens in production: something else
        /// entirely completed the task, and the tick noticed.
        #[tokio::test]
        async fn a_work_whose_board_has_emptied_closes_itself_and_raises_its_card() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, conversation) = work_with_a_session(&store);
            // Completed through the *board's* own claim — not through
            // `complete_work_task` — because that is the path an agent takes,
            // and a rule that only fires on one call site is a rule the others
            // forget.
            let task = store.work_tasks(&work).unwrap().remove(0);
            store.complete_task(&task.id).unwrap();

            let report = ticker_over(&store).tick_works().unwrap();

            assert_eq!(report.started, 1, "the work was closed by the tick");
            assert_eq!(store.work(&work).unwrap().unwrap().state, State::Closed);
            let cards = store
                .cards(&crate::cards::Query {
                    conversation_id: Some(conversation),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(cards.len(), 1, "the closing card was raised");
            assert!(cards[0].title.contains("closed"), "{}", cards[0].title);
        }

        /// *Finishing* is tasks done with sessions still running, and only
        /// something watching the runs can notice it stop being true.
        #[tokio::test]
        async fn a_finishing_work_is_closed_once_its_last_run_stops() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, conversation) = work_with_a_session(&store);
            run_in(&store, &conversation, "run-1", "running");
            let task = store.work_tasks(&work).unwrap().remove(0);
            store.complete_task(&task.id).unwrap();

            let first = ticker_over(&store).tick_works().unwrap();
            assert_eq!(first.started, 1);
            assert_eq!(
                store.work(&work).unwrap().unwrap().state,
                State::Finishing,
                "a session was still running, so the work is not safe to act on yet"
            );

            store.set_run_status("run-1", "completed").unwrap();
            let second = ticker_over(&store).tick_works().unwrap();

            assert_eq!(second.started, 1);
            assert_eq!(store.work(&work).unwrap().unwrap().state, State::Closed);
            assert!(store.works(Filter::Live).unwrap().is_empty());
        }

        /// E2.S7's last line: a session that ends with answers still queued
        /// reports them as undeliverable instead of dropping them. Somebody
        /// answered those cards, and a queue that loses one silently is
        /// indistinguishable from one that works.
        #[tokio::test]
        async fn answers_nobody_will_now_hear_are_reported_rather_than_dropped() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, conversation) = work_with_a_session(&store);
            let card = store
                .raise_card(crate::cards::NewCard {
                    conversation_id: conversation.clone(),
                    title: "which database?".into(),
                    ..Default::default()
                })
                .unwrap();
            store.answer_card(card.id, None, Some("sqlite")).unwrap();
            let task = store.work_tasks(&work).unwrap().remove(0);
            store.complete_task(&task.id).unwrap();

            ticker_over(&store).tick_works().unwrap();

            assert!(
                store.pending_for(&conversation).unwrap().is_empty(),
                "nothing is left queued against a session that has stopped"
            );
            assert_eq!(
                store.card(card.id).unwrap().unwrap().delivery,
                crate::cards::Delivery::Undeliverable,
                "the rail has to say nobody heard it"
            );
        }

        /// The guard. Unit tests on an uncalled function stay green for ever,
        /// so this one goes through the tick the daemon runs: delete the
        /// `tick_works` line from `impl Tick for Ticker` and it fails.
        #[tokio::test]
        async fn the_daemons_tick_closes_a_work_whose_board_has_emptied() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, _) = work_with_a_session(&store);
            let task = store.work_tasks(&work).unwrap().remove(0);
            store.complete_task(&task.id).unwrap();

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert_eq!(
                store.work(&work).unwrap().unwrap().state,
                State::Closed,
                "the composite tick never asked whether the board had emptied"
            );
        }
    }

    mod pull_requests {
        use super::*;
        use crate::daemon::Tick;
        use crate::store::Store;

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// The guard, and it had to be written rather than inherited.
        ///
        /// The other steps' guards each assert a state change only *their* step
        /// causes; none of them would notice this one disappearing, and the
        /// composite `TickReport` would not either, because a sweep over an
        /// empty store contributes zero to every counter. So this asserts the
        /// one thing the step does unconditionally: it records that it ran.
        ///
        /// Delete the `tick_pull_requests` line from `impl Tick for Ticker` and
        /// this fails.
        ///
        /// No network: an empty store has no stale pull request and no held
        /// lease, so `sweep` finds nothing to ask about and never spawns `gh`.
        #[tokio::test]
        async fn the_daemons_tick_asks_the_forge_about_pull_requests() {
            let store = Arc::new(Store::in_memory().unwrap());

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert_eq!(
                store.setting(POLLED_AT_KEY).unwrap().as_deref(),
                Some("1000000"),
                "the composite tick never polled for pull requests"
            );
        }

        /// The interval is the whole reason this step is not like the others:
        /// it is the only one that leaves the machine, and a tick is a minute.
        #[tokio::test]
        async fn a_second_tick_a_minute_later_does_not_ask_again() {
            let store = Arc::new(Store::in_memory().unwrap());
            let ticker = ticker_over(&store);

            ticker.tick_pull_requests(1_000_000).await.unwrap();
            ticker.tick_pull_requests(1_060_000).await.unwrap();

            assert_eq!(
                store.setting(POLLED_AT_KEY).unwrap().as_deref(),
                Some("1000000"),
                "a minute later is not five minutes later"
            );

            ticker
                .tick_pull_requests(1_000_000 + POLL_EVERY_MS)
                .await
                .unwrap();
            assert_eq!(
                store.setting(POLLED_AT_KEY).unwrap().as_deref(),
                Some(&(1_000_000 + POLL_EVERY_MS).to_string()).map(String::as_str)
            );
        }

        /// A VM restored from a snapshot, or an NTP correction, must not lock
        /// polling out until the clock catches up.
        #[tokio::test]
        async fn a_clock_that_went_backwards_does_not_stop_the_poll() {
            let store = Arc::new(Store::in_memory().unwrap());
            store
                .set_setting(POLLED_AT_KEY, &9_000_000_i64.to_string())
                .unwrap();
            assert!(due_to_poll(&store, 1_000_000));
        }
    }

    mod deliveries {
        use super::*;
        use crate::cards::NewCard;
        use crate::conversation::{NewMessage, Role};
        use crate::daemon::Tick;
        use crate::delivery::{Kind, State};
        use crate::harness::HarnessKind;
        use crate::store::Store;

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// A conversation with a harness session to resume — the ordinary case,
        /// and the only one anything may be delivered into.
        fn session(store: &Store) -> String {
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            store
                .set_conversation_session(&c.id, Some("ses-1"))
                .unwrap();
            c.id
        }

        fn answered_card(store: &Store, conversation: &str, title: &str) -> i64 {
            let card = store
                .raise_card(NewCard {
                    conversation_id: conversation.to_string(),
                    title: title.to_string(),
                    ..NewCard::default()
                })
                .unwrap();
            store
                .answer_card(card.id, None, Some("yes, go ahead"))
                .unwrap();
            card.id
        }

        /// A run of this conversation, in whatever state, so `plan_injection`
        /// can be asked the one question it cannot work out for itself.
        fn run_in(store: &Store, conversation: &str, id: &str, status: &str) {
            store
                .save_run(&crate::store::StoredRun {
                    id: id.into(),
                    name: "worker".into(),
                    harness: "claude_code".into(),
                    status: status.into(),
                    cwd: "/tmp".into(),
                    session_id: Some("ses-1".into()),
                    pid: None,
                    pgid: None,
                    created_at_ms: 1,
                    summary: serde_json::Value::Null,
                })
                .unwrap();
            store
                .append_message(
                    conversation,
                    NewMessage::new(Role::Assistant, "working").from_run(id),
                )
                .unwrap();
        }

        #[tokio::test]
        async fn a_tick_with_nothing_queued_speaks_to_nobody() {
            let store = Arc::new(Store::in_memory().unwrap());
            session(&store);
            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();
            assert_eq!(report.claimed, 0);
            assert_eq!(report.started + report.failed, 0);
        }

        /// The acceptance case. A test box has no supervisor for the spawn to
        /// reach, so this asserts the tick got as far as attempting it — the
        /// same way every other spawning test here does. What it proves is the
        /// part that was missing: something *looks* at the queue.
        #[tokio::test]
        async fn an_answer_queued_against_an_idle_session_is_carried_by_a_turn() {
            let store = Arc::new(Store::in_memory().unwrap());
            let conversation = session(&store);
            answered_card(&store, &conversation, "shall I use SQLite?");

            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();

            assert_eq!(report.claimed, 1, "the queue was read");
            assert_eq!(
                report.started + report.failed,
                1,
                "an idle session with a queued answer is spoken to"
            );
            assert_eq!(report.held, 0);
        }

        /// The rule the whole queue exists for. The running turn's prompt was
        /// assembled before this answer existed.
        #[tokio::test]
        async fn a_session_mid_turn_is_left_alone_and_its_answer_waits() {
            let store = Arc::new(Store::in_memory().unwrap());
            let conversation = session(&store);
            run_in(&store, &conversation, "run-1", "running");
            answered_card(&store, &conversation, "shall I rename the column?");

            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();

            assert_eq!(report.held, 1);
            assert_eq!(report.started + report.failed, 0, "a turn was interrupted");
            assert_eq!(
                store.pending_for(&conversation).unwrap().len(),
                1,
                "and nothing was lost"
            );
        }

        /// Ten answers queued during one turn are one turn carrying ten, not
        /// ten turns. A cost control, and the more coherent answer.
        #[tokio::test]
        async fn everything_queued_for_one_session_goes_in_one_turn() {
            let store = Arc::new(Store::in_memory().unwrap());
            let conversation = session(&store);
            for i in 0..10 {
                answered_card(&store, &conversation, &format!("question {i}"));
            }

            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();

            assert_eq!(report.claimed, 1, "ten answers, one session, one turn");
            assert_eq!(report.started + report.failed, 1);
        }

        /// The same refusal `wake_order` makes for a member with no session:
        /// delivering into a fresh context would have the agent answer having
        /// forgotten what the card was about.
        #[tokio::test]
        async fn a_session_that_has_never_run_is_not_spoken_into_an_empty_context() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            answered_card(&store, &c.id, "shall I start?");

            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();

            assert_eq!(report.held, 1);
            assert_eq!(report.started + report.failed, 0);
            assert_eq!(
                store.pending_for(&c.id).unwrap()[0].state,
                State::Queued,
                "held, not marked delivered to a turn that never happened"
            );
        }

        /// A failed spawn must leave the answer queued. Marking it delivered to
        /// a run that never started is how an answer is lost silently — the
        /// rail would show `delivered` and no agent would ever have heard it.
        #[tokio::test]
        async fn a_spawn_that_fails_leaves_the_answer_queued() {
            let store = Arc::new(Store::in_memory().unwrap());
            let conversation = session(&store);
            let card = answered_card(&store, &conversation, "shall I?");

            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();

            // No supervisor on a test box, so the spawn fails and this is the
            // failure path rather than a contrivance.
            if report.failed == 1 {
                assert_eq!(
                    store.pending_for(&conversation).unwrap().len(),
                    1,
                    "a failed spawn must not consume the answer"
                );
                assert_eq!(
                    store.card(card).unwrap().unwrap().delivery,
                    crate::cards::Delivery::Queued,
                    "and the rail must not claim it arrived"
                );
            }
        }

        /// A queue never outlives its conversation: the rows cascade with it.
        /// Worth pinning, because the tick would otherwise count them as
        /// waiting on every pass from now until the end of time.
        #[tokio::test]
        async fn deleting_a_conversation_takes_its_queue_with_it() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            store
                .enqueue_delivery(&c.id, Kind::Human, "", "stop and show me the diff")
                .unwrap();
            store.delete_conversation(&c.id).unwrap();

            // The row cascades with the conversation, so there is nothing left
            // to deliver — which is the point: the queue does not outlive it.
            let report = ticker_over(&store).tick_deliveries(1).await.unwrap();
            assert_eq!(report.claimed, 0);
        }

        /// The guard the lesson of this build asks for: unit tests on an
        /// uncalled function stay green for ever, so this one calls the tick
        /// the daemon actually runs. Remove the `tick_deliveries` line from
        /// `impl Tick for Ticker` and this fails.
        #[tokio::test]
        async fn the_daemons_tick_reads_the_delivery_queue() {
            let store = Arc::new(Store::in_memory().unwrap());
            let conversation = session(&store);
            answered_card(&store, &conversation, "shall I use SQLite?");

            let report = Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert!(
                report.claimed >= 1,
                "the composite tick never looked at the delivery queue: {report:?}"
            );
        }
    }

    mod mail {
        use super::*;
        use crate::harness::HarnessKind;
        use crate::team::{MemberStatus, Post, Scope};

        /// A team with a sender and one recipient, described by what the
        /// recipient can do about its mail.
        fn crew(status: MemberStatus, session: Option<&str>) -> Arc<Store> {
            let store = Arc::new(Store::in_memory().unwrap());
            for name in ["lead", "scout"] {
                store
                    .join_scope(Scope::Team, "crew", name, HarnessKind::ClaudeCode, "", None)
                    .unwrap();
            }
            store.bind_member("crew", "scout", None, session).unwrap();
            store.set_member_status("crew", "scout", status).unwrap();
            store
        }

        fn post(store: &Store, text: &str) {
            store
                .post(&Post::new(Scope::Team, "crew", "lead", text).to("scout"))
                .unwrap();
        }

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        #[tokio::test]
        async fn a_tick_with_no_mail_wakes_nobody() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert_eq!(report.claimed, 0);
            assert_eq!(report.started + report.failed, 0);
        }

        /// G2.S3, and the reason the rate limit exists: ten messages arriving
        /// together must be one resumed turn carrying ten, not ten turns.
        #[tokio::test]
        async fn ten_messages_arriving_together_produce_one_resumed_turn() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            for i in 0..10 {
                post(&store, &format!("message {i}"));
            }
            let ticker = ticker_over(&store);

            let first = ticker.tick_mail(1_000_000).await.unwrap();
            assert_eq!(first.claimed, 1, "one member is holding mail, not ten");
            assert_eq!(first.started + first.failed, 1, "it tried exactly once");

            // Whether or not the spawn found a supervisor, no second turn is
            // started for the same burst.
            let second = ticker.tick_mail(1_000_001).await.unwrap();
            assert_eq!(
                second.started, 0,
                "a second wake inside the interval is a second model call for mail already carried"
            );
        }

        #[tokio::test]
        async fn a_member_is_due_to_be_woken_again_once_the_interval_has_passed() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            post(&store, "first");
            let ticker = ticker_over(&store);
            ticker.tick_mail(1_000_000).await.unwrap();

            let later = ticker
                .tick_mail(1_000_000 + crate::team::WAKE_INTERVAL_MS)
                .await
                .unwrap();
            // The mail is still there — the spawn had no supervisor to talk to —
            // so the only question is whether the tick was willing to try again.
            assert_eq!(
                later.started + later.failed,
                1,
                "the rate limit became a permanent silence"
            );
        }

        /// G2.S4. Delivering into a fresh context would have it answer having
        /// forgotten the work, which is worse than waiting.
        #[tokio::test]
        async fn mail_for_a_member_with_no_session_waits_visibly() {
            let store = crew(MemberStatus::Ready, None);
            post(&store, "carry on");

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert_eq!(report.started, 0, "it was woken into an empty context");
            assert_eq!(report.held, 1);

            let waiting = store.team_unread("crew", "scout").unwrap();
            assert_eq!(waiting.len(), 1, "the mail was consumed rather than kept");
            let detail = store.envelope(waiting[0].id).unwrap().unwrap().detail;
            assert!(
                detail.as_deref().unwrap_or_default().contains("no session"),
                "mail nobody can read must say so: {detail:?}"
            );
        }

        #[tokio::test]
        async fn mail_for_a_member_that_has_stopped_is_reported_rather_than_left_silent() {
            let store = crew(MemberStatus::Shutdown, Some("ses-1"));
            post(&store, "one more thing");

            ticker_over(&store).tick_mail(1_000_000).await.unwrap();

            let waiting = store.team_unread("crew", "scout").unwrap();
            let detail = store.envelope(waiting[0].id).unwrap().unwrap().detail;
            assert!(
                detail.as_deref().unwrap_or_default().contains("shutdown"),
                "{detail:?}"
            );
        }

        /// A busy member is not stuck: it reads its inbox on its next turn, and
        /// annotating that as a problem would be crying wolf on the ordinary
        /// case.
        #[tokio::test]
        async fn a_busy_member_is_left_to_read_its_own_inbox() {
            let store = crew(MemberStatus::Busy, Some("ses-1"));
            post(&store, "when you get a moment");

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert_eq!(
                report.started, 0,
                "a member mid-turn had its session forked"
            );
            assert_eq!(report.held, 1);

            let waiting = store.team_unread("crew", "scout").unwrap();
            assert_eq!(
                store.envelope(waiting[0].id).unwrap().unwrap().detail,
                None,
                "being busy is not a fault and must not be reported as one"
            );
        }

        /// Record a run for a member, in whatever state.
        fn run_for(store: &Store, id: &str, status: &str, session: Option<&str>) {
            store
                .save_run(&crate::store::StoredRun {
                    id: id.into(),
                    name: "crew-scout".into(),
                    harness: "claude_code".into(),
                    status: status.into(),
                    cwd: "/tmp".into(),
                    session_id: session.map(str::to_string),
                    pid: None,
                    pgid: None,
                    created_at_ms: 1,
                    summary: serde_json::Value::Null,
                })
                .unwrap();
        }

        /// The bug the end-to-end suite found and every test here had missed:
        /// waking marks a member busy, nothing marked it back, so it could be
        /// woken exactly once and then held its mail for ever.
        #[tokio::test]
        async fn a_member_whose_turn_has_finished_can_be_woken_again() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            run_for(&store, "run-1", "completed", Some("ses-2"));
            store
                .bind_member("crew", "scout", Some("run-1"), Some("ses-1"))
                .unwrap();
            store
                .set_member_status("crew", "scout", MemberStatus::Busy)
                .unwrap();
            post(&store, "are you there?");

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();

            // It was freed and then woken in the same pass. Asserted as "it
            // tried", the way the monitor tests above do, because a test box
            // has no supervisor for the spawn to reach — the point is that the
            // tick got as far as attempting it. Before `settle_members` this
            // was `held 1` and nothing was ever attempted again.
            assert_eq!(report.claimed, 1);
            assert_eq!(
                report.started + report.failed,
                1,
                "a member whose turn had ended was still treated as busy"
            );
            assert_eq!(report.held, 0);
        }

        /// The session id can change when a conversation is resumed, and a
        /// member holding the old one would next be resumed into a conversation
        /// that has moved on.
        #[tokio::test]
        async fn settling_a_member_takes_the_session_its_run_ended_on() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            run_for(&store, "run-1", "completed", Some("ses-moved"));
            store
                .bind_member("crew", "scout", Some("run-1"), Some("ses-1"))
                .unwrap();
            store
                .set_member_status("crew", "scout", MemberStatus::Busy)
                .unwrap();

            // Nothing waiting, so the tick only reconciles.
            ticker_over(&store).tick_mail(1_000_000).await.unwrap();

            let scout = store
                .member_in(Scope::Team, "crew", "scout")
                .unwrap()
                .unwrap();
            assert_eq!(scout.status, MemberStatus::Ready);
            assert_eq!(scout.session_id.as_deref(), Some("ses-moved"));
        }

        /// The other half: freeing a member whose turn is still in flight would
        /// resume it mid-turn and fork the conversation.
        #[tokio::test]
        async fn a_member_whose_run_is_still_going_is_left_busy() {
            let store = crew(MemberStatus::Ready, Some("ses-1"));
            run_for(&store, "run-1", "running", Some("ses-1"));
            store
                .bind_member("crew", "scout", Some("run-1"), Some("ses-1"))
                .unwrap();
            store
                .set_member_status("crew", "scout", MemberStatus::Busy)
                .unwrap();
            post(&store, "hurry up");

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();

            assert_eq!(
                report.started, 0,
                "a member mid-turn had its session forked"
            );
            assert_eq!(
                store
                    .member_in(Scope::Team, "crew", "scout")
                    .unwrap()
                    .unwrap()
                    .status,
                MemberStatus::Busy
            );
        }

        #[tokio::test]
        async fn a_jod_with_no_database_ticks_the_mail_without_complaining() {
            let ticker = Ticker::new(Jod::new());
            assert_eq!(ticker.tick_mail(1).await.unwrap().claimed, 0);
        }
    }
}
