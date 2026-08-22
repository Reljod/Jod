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
use crate::cards::{CardKind, Importance, NewCard, Source};
use crate::heartbeat::{
    self, Beat, Heartbeat, Observed, Response, SweepReport, Verdict, Watching,
};
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

/// What can honestly be said about a settled iteration whose run cannot be read
/// back, as a recovered cost and a line for `jod goal log`.
///
/// A real, paid-for run reaches this by two routes that have both been seen.
/// It can fall outside the daemon's 200-run rehydrate window, so nothing ever
/// loaded it into memory; or `rehydrate` can skip it because the summary on
/// disk was written by a build whose `AgentSummary` had a different shape.
/// Neither route destroys anything. The `runs` row and the run's events are
/// both still there — the only thing that has failed is turning them back into
/// a struct this build understands.
///
/// So the cost is recovered rather than assumed, and only when both places have
/// been asked and neither answered does the line say the cost is unknown. That
/// distinction is the point: an unknown cost written down as unknown can be
/// chased later, while an unknown cost written down as `$0.00` looks like a
/// settled fact and nobody ever asks again.
fn unreadable_iteration(store: &Store, run_id: &str) -> (Option<f64>, String) {
    let recovered = recorded_cost(store, run_id);
    let outcome = match recovered {
        Some(spent) => format!(
            "the run {run_id} could not be read back; its recorded cost was ${spent:.4}"
        ),
        None => format!("the run {run_id} could not be read back, and its cost is unknown"),
    };
    (recovered, outcome)
}

/// The bill for a run the service cannot hand back, read straight out of the
/// store.
///
/// Two places are asked, because the two ways a run becomes unreadable damage
/// different things. A run outside the rehydrate window has a perfectly good
/// summary that nobody loaded, so the row answers. A summary written by an
/// incompatible build cannot become an `AgentSummary`, but it is still JSON and
/// the usage it recorded is still in it, so the row usually answers that case
/// too. The `Finished` event is the fallback for a row whose summary really has
/// lost its usage: it is the harness's own report of what the turn cost, stored
/// separately and never rewritten.
fn recorded_cost(store: &Store, run_id: &str) -> Option<f64> {
    if let Ok(Some(row)) = store.run(run_id) {
        if let Some(spent) = row
            .summary
            .pointer("/usage/cost_usd")
            .and_then(serde_json::Value::as_f64)
        {
            return Some(spent);
        }
    }
    store
        .events(run_id)
        .ok()?
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            crate::event::AgentEvent::Finished { usage, .. } => usage.cost_usd,
            _ => None,
        })
}

/// How a goal's ending should read to the person who finds it in
/// `jod goal log`.
///
/// The state word on its own is not enough. "exhausted" on a goal that ran
/// twenty times means it did the work and used up what it was given.
/// "exhausted" on a goal that never started means it was created with a limit
/// it was already past, so nothing it was asked to do was ever attempted —
/// that is a mistake in how the goal was written down, not a result. The two
/// need to read differently, and the reason names the limit and the number it
/// was compared against so the person does not have to go looking.
///
/// "satisfied" has the same two readings and needs the same treatment. A goal
/// satisfied after twenty iterations was met by work it did and paid for. A
/// goal satisfied at iteration zero was already met when it was written down,
/// and the only reason anyone knows is that the done-when check was run once,
/// which costs nothing. That is the only way this is called with
/// [`schedule::GoalState::Satisfied`], so the reason may say so outright.
fn ending_note(goal: &Goal, stop: schedule::GoalState) -> String {
    let reason = match stop {
        schedule::GoalState::Exhausted => match goal.max_iterations {
            Some(max) if goal.iteration >= max => format!(
                "it is allowed {max} iterations and has run {}",
                goal.iteration
            ),
            _ => format!(
                "it is allowed ${:.2} and has spent ${:.2}",
                goal.budget_usd.unwrap_or(0.0),
                goal.spent_usd
            ),
        },
        schedule::GoalState::Satisfied => format!(
            "`{}` already passed, so no iteration was started and nothing was spent",
            goal.done_when.as_deref().unwrap_or("the done-when check")
        ),
        _ => format!(
            "{} iterations in a row changed nothing, and {} is the limit",
            goal.no_progress, goal.stall_after
        ),
    };
    if goal.iteration == 0 {
        format!("{} before its first iteration: {reason}", stop.as_str())
    } else {
        format!("{}: {reason}", stop.as_str())
    }
}

/// How often the scheduler looks for work.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the delivery ledger is trimmed. See [`Ticker::trim_ledger`].
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1_000;

/// Where the last trim is remembered. In `settings` and not in this struct,
/// because a field would reset on every restart and turn "hourly" into "every
/// startup".
const PRUNED_AT_KEY: &str = "ledger.pruned_at_ms";

/// When the heartbeat sweep last ran.
///
/// Stamped on every sweep, so a process that is not the daemon can tell whether
/// anything is actually watching. This matters now in a way it did not before:
/// every spawn arms a heartbeat, but the sweep only runs inside `jod daemon`,
/// and a heartbeat nothing sweeps is a promise that quietly does not hold. The
/// fleet would show every wedged agent as healthy and never say why.
///
/// In `settings` rather than in the [`Ticker`], because the reader is a
/// different process.
pub const SWEPT_AT_KEY: &str = "heartbeats.swept_at_ms";

/// How long the fleet keeps believing in a sweep it has not seen.
///
/// Three ticks. One is far too tight — a sweep that ran a shade over a minute
/// ago is a healthy daemon on a busy box, and warning about it would train the
/// reader to ignore the warning, which is worse than not having one.
pub const SWEEP_STALE_AFTER_MS: i64 = 3 * 60 * 1_000;

/// Whether anything is sweeping heartbeats, given when one last ran.
///
/// `None` means no sweep has ever run against this database, which is the
/// ordinary state of a machine where nobody has started the daemon — and the
/// case the warning most needs to catch.
pub fn sweep_is_stale(last_sweep_ms: Option<i64>, now_ms: i64) -> bool {
    match last_sweep_ms {
        None => true,
        // Saturating, so a clock that went backwards reads as "recent" rather
        // than as an enormous gap. A restored snapshot must not make the fleet
        // announce that the daemon is down when it is running fine.
        Some(last) => now_ms.saturating_sub(last) > SWEEP_STALE_AFTER_MS,
    }
}

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

/// One day, for turning `scratch_retention_days` into a cutoff.
///
/// Named rather than written out at the one call site, because the number that
/// decides how long a transcript survives should be readable as a duration
/// rather than counted in zeroes.
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

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

/// What a `fire_once` catch-up passed over.
///
/// [`decide`] answers a long outage under the default policy with a single
/// [`Decision::Run`] and nothing else, and that is right: the question after an
/// outage is whether the inbox got triaged, not whether it got triaged eleven
/// times. But the instants it passed over still happened, and [`FireOutcome`]
/// states the rule they fall foul of — every outcome is written down, because a
/// skip nobody recorded is a silent failure. Before this existed, a six-hour
/// outage on a fifteen-minute schedule left one ordinary-looking `ran` row and
/// no trace at all of the twenty-three instants that were dropped to produce
/// it.
///
/// **One row, not one row per instant, and the difference is the fact being
/// recorded.** Under `skip`, each missed instant got nothing whatsoever, so
/// each one is its own outcome and each one earns its own row. Under
/// `fire_once` they were not discarded one by one — they were folded into the
/// single run that stands in for all of them, which is one thing that happened
/// once. Twenty-three rows identical to `skip`'s would tell a person that
/// nothing ran for those instants, when something did, and would push the run
/// itself off the ten lines `jod schedule log` shows by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaughtUp {
    /// How many instants the catch-up passed over. Never zero.
    pub passed_over: usize,
    /// The oldest instant passed over.
    pub from_ms: i64,
    /// The newest instant passed over. The one after it is the instant that
    /// got the run.
    pub to_ms: i64,
}

impl CaughtUp {
    /// The line a person reads in `jod schedule log`.
    ///
    /// It says how many and over what window, because "some instants went
    /// missing" answers none of the questions somebody reading an outage has.
    /// The window is in the schedule's own zone, which is the zone its cron
    /// expression is written in.
    ///
    /// It describes what the policy did rather than what the run then did. The
    /// run's own row, written straight after this one, is where whether it
    /// actually started belongs — and it may say `spawn_failed`.
    pub fn detail(&self, timezone: &str) -> String {
        format!(
            "{} instants missed while Jod was not running, {} to {}; \
             fire_once kept only the most recent",
            self.passed_over,
            in_zone(self.from_ms, timezone),
            in_zone(self.to_ms, timezone),
        )
    }
}

/// A timestamp as the schedule's own zone would write it.
///
/// An unreadable zone falls back to UTC rather than failing. This text is a
/// record of something that already happened, and refusing to write it down
/// over a bad zone name would lose the very thing being recorded.
fn in_zone(at_ms: i64, timezone: &str) -> String {
    let zone: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|t| t.with_timezone(&zone).format("%Y-%m-%d %H:%M %Z").to_string())
        .unwrap_or_else(|| at_ms.to_string())
}

/// The instants a tick's catch-up passed over, if it passed over any.
///
/// `steps` is the plan as it will actually be carried out — after [`plan`] has
/// had its say — because a tick that is going to start nothing has caught
/// nothing up and must not claim it did. A monitor that suppressed the run, or
/// an overlap hold because the previous run is still going, each write their
/// own row saying what really happened to this tick.
///
/// Only `fire_once` needs this. `skip` already records every instant it
/// dropped, one row each, and `fire_all` drops nothing it was not bounded out
/// of.
pub fn caught_up(s: &Schedule, missed: &[i64], steps: &[Decision]) -> Option<CaughtUp> {
    if s.misfire != Misfire::FireOnce || missed.len() < 2 || !steps.iter().any(spawns) {
        return None;
    }
    let passed = &missed[..missed.len() - 1];
    Some(CaughtUp {
        passed_over: passed.len(),
        from_ms: passed[0],
        to_ms: passed[passed.len() - 1],
    })
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

/// How much of Jod a conversation resumed by [`Ticker::tick_deliveries`] holds.
///
/// `Delegate` for an ordinary session, which is enough to act on what it has
/// just been told. `Orchestrate` for the main chat, because that is what
/// [`crate::orchestrator::hand_to_orchestrator`] gives it on every turn a person
/// starts, and a turn started by an answer coming back from a delegated run is
/// the same chat doing the same job. A main chat that could arm a schedule when
/// Reljod typed and not when the answer arrived would be two different
/// orchestrators depending on who spoke last.
///
/// A free function rather than a branch inside the loop so the rule is one
/// decision with one reason, and can be asserted without spawning anything.
fn delivery_access(
    conversation_id: &str,
    main_chat: Option<&str>,
) -> crate::harness::ToolAccess {
    if main_chat == Some(conversation_id) {
        crate::harness::ToolAccess::Orchestrate
    } else {
        crate::harness::ToolAccess::Delegate
    }
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

            let steps = plan(planned, watch);
            // What the default policy dropped to produce the run below.
            //
            // Written *before* the run and not as one of the decisions, and
            // both of those are deliberate. Before, so the run keeps the newest
            // row and `jod schedule log` still opens on it. Not a decision,
            // because `decide` answering a long outage with exactly one
            // `Decision::Run` is the behaviour a person wants and is pinned by
            // a test — this records what that answer cost without changing the
            // answer.
            if let Some(caught) = caught_up(&s, &missed, &steps) {
                store.record_fire(&Fire {
                    id: 0,
                    schedule_id: s.id.clone(),
                    due_at_ms: caught.from_ms,
                    fired_at_ms: now_ms,
                    run_id: None,
                    outcome: FireOutcome::SkippedMisfire,
                    detail: Some(caught.detail(&s.timezone)),
                })?;
                report.held += 1;
            }

            for decision in steps {
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
            let response = heartbeat::respond(&hb, &verdict);

            if response.keeps_watching() {
                // Counted off the verdict, not the response. A run that had
                // been marked and has just come back produces `Clear` rather
                // than `Beat`, and it is still a run that did something this
                // tick — reading the response here would report it as idle.
                if matches!(verdict, Verdict::Beating { .. }) {
                    report.beating += 1;
                }
                match response {
                    // The first tick that marks a run is the only one that says
                    // anything. `hb.stalled_since_ms` is still what the *stored*
                    // row said, so this is a transition test, not a state test,
                    // and the fiftieth tick of the same stall is silent.
                    Response::Mark if !hb.is_stalled() => {
                        report.marked += 1;
                        self.record_verdict(&store, &hb, &verdict);
                        self.raise_stalled_card(&store, &hb, &verdict);
                    }
                    Response::Clear => {
                        // Coming back is worth recording too, or the memory
                        // scope keeps a stall that has since resolved and
                        // nothing ever says it did.
                        self.note(&store, &hb, "recovered", "went quiet and came back");
                    }
                    _ => {}
                }
                store.record_beat(&Beat::after(&hb, &verdict, now_ms))?;
                continue;
            }

            // Retiring. Say why *before* tidying up, so a crash between the two
            // leaves the explanation rather than the bookkeeping.
            self.record_verdict(&store, &hb, &verdict);

            if let Response::Reap { terminate } = response {
                if terminate {
                    report.stopped += 1;
                }
                if let Err(e) = self.jod.fail_agent(&hb.run_id, terminate).await {
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

        // Stamped even when nothing was watched. "The sweep ran and there was
        // nothing to do" and "nothing has swept in three hours" are different
        // facts, and only stamping on a non-empty pass would make an idle fleet
        // indistinguishable from a dead daemon.
        if let Err(e) = store.set_setting(SWEPT_AT_KEY, &now_ms.to_string()) {
            eprintln!("[jod] could not record the heartbeat sweep: {e}");
        }
        Ok(report)
    }

    /// Put a stalled session on the rail, once.
    ///
    /// Never fatal, for the reason the note is not: a sweep that cannot raise a
    /// card has still marked the row, and the fleet tree reads the mark rather
    /// than the card.
    ///
    /// A run that stalled before writing its first message has no conversation
    /// to raise a card against, and gets the mark without the card. That is the
    /// honest outcome — the rail is organised by conversation, and inventing one
    /// to hang a notice on would put a thread in the tree that never existed.
    fn raise_stalled_card(&self, store: &Store, hb: &Heartbeat, verdict: &Verdict) {
        let Ok(Some(conversation_id)) = store.conversation_for_run(&hb.run_id) else {
            return;
        };
        if let Err(e) = store.raise_card(NewCard {
            conversation_id,
            run_id: Some(hb.run_id.clone()),
            kind: Some(CardKind::Question),
            importance: Some(Importance::Normal),
            // Not blocking. A blocking card says a run cannot continue past
            // this question; this one says a run has stopped continuing on its
            // own, and nothing is waiting on the answer.
            blocking: false,
            title: format!(
                "{} looks stuck",
                hb.run_id.chars().take(8).collect::<String>()
            ),
            body: format!(
                "{}. It has not been stopped — it is still running, and still \
                 holding whatever it was working in. Stop it, or leave it and \
                 start a fresh session beside it.",
                verdict.detail()
            ),
            // The backstop under the transition test above. If a daemon
            // restarts mid-stall and re-reads a row it has already marked, the
            // key is what stops a second card for the same stall.
            dedupe_key: Some(format!("stalled:{}", hb.run_id)),
            source: Some(Source::Jod),
            ..Default::default()
        }) {
            eprintln!("[jod] stalled card for {} failed: {e}", hb.run_id);
        }
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

        // Pausing a goal means "start no new iterations". It also stopped Jod
        // noticing the iteration that was already in flight, which is a
        // different thing and was never the intention: `claim_due_goals` looks
        // only at running goals, and this loop is the only thing that settles a
        // finished run or reads what it cost. So a goal paused mid-iteration
        // read `iter 0 · $0.00` beside a run that had finished and cost
        // $0.0963, and only a resume made it admit the bill.
        //
        // Settling is separated from spawning rather than the claim being
        // widened. A paused goal is claimed here only when it has a run in
        // flight, so one with nothing to settle is never claimed at all and
        // cannot spin; and the guard below — a goal whose state is not live
        // releases before the spawn — is what keeps the pause meaning what it
        // says.
        let mut settling = Vec::new();
        for goal in store.paused_goals()? {
            if self
                .current_run(&store, &goal.memory_scope(), &format!("goal/{}", goal.name))?
                .is_none()
            {
                continue;
            }
            if let Some(claimed) = store.claim_paused_goal(&goal.id, &self.owner, now_ms, LEASE_MS)?
            {
                settling.push(claimed);
            }
        }

        let mut report = TickReport {
            claimed: due.len() + settling.len(),
            ..Default::default()
        };

        for goal in due.into_iter().chain(settling) {
            let scope = goal.memory_scope();
            let subject = format!("goal/{}", goal.name);

            // Settle the previous iteration before starting another, so a goal
            // never has two runs in flight and its spend is counted once.
            // Whether there was one is also what tells the code below that this
            // goal has never run at all, so it is read once and kept.
            let in_flight = self.current_run(&store, &scope, &subject)?;
            if let Some(run) = &in_flight {
                match self.jod.agent(run).await {
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
                    // A settled iteration, whether or not the run behind it can
                    // still be read. Both cases take the same path from here,
                    // because the goal paid for the turn either way, and the
                    // only honest difference between them is how much can be
                    // said about what the turn cost and what it did.
                    //
                    // A run that cannot be read used to have its own short arm
                    // that advanced the counter and recorded nothing else. That
                    // arm claimed the iteration was free, said nothing about it
                    // in `goal log`, and counted it against the stall counter —
                    // three claims, all of them false, about work Jod had
                    // simply lost track of.
                    settled => {
                        // What the done-when check says, run by Jod rather than
                        // described to the agent. Both of the goal's own
                        // guarantees hang off this. It shells out to the check
                        // command and never touches the run, which is why it is
                        // still the right answer for an iteration whose run has
                        // gone missing.
                        let verdict = self.check_done(&goal).await;

                        // What this iteration cost and what it said, recorded
                        // before anything decides how the goal ends. The turn
                        // happened and was billed whichever way the check went,
                        // and the iteration that *passes* the check is the one
                        // that did the work — writing it only on the path where
                        // the goal keeps going left the last iteration of every
                        // successful goal missing from `goal log`, missing from
                        // the iteration count, and missing from `spent_usd`.
                        let (billed, outcome) = match settled {
                            Ok(agent) => (
                                agent.usage.cost_usd,
                                agent.last_message.clone().unwrap_or_else(|| {
                                    format!("{:?}", agent.status).to_lowercase()
                                }),
                            ),
                            Err(_) => unreadable_iteration(&store, run),
                        };
                        // A cost nobody can recover is added as nothing, since
                        // there is no other number to add — but the line above
                        // says so in words, so `goal log` reads "cost unknown"
                        // rather than implying the turn was free.
                        let cost = billed.unwrap_or(0.0);
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
                        let progressed =
                            match (&verdict, self.last_fingerprint(&store, &scope, &subject)?) {
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
                        // A pause is not an ending, and a goal settled while it
                        // is paused comes back from `advance_goal` still
                        // paused. Writing that down as an ending would leave
                        // `ended: paused` in the goal's memory for good, which
                        // a resume cannot take back and which `goal log` would
                        // show for ever beside a goal that went on to finish.
                        if !state.is_live() && state != crate::schedule::GoalState::Paused {
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
                }
            }

            // Re-read: `advance_goal` may have just stopped it.
            let Some(goal) = store.goal_named(&goal.name)? else {
                continue;
            };
            if !goal.state.is_live() {
                // Every branch below this leads to a spawn, and a goal that is
                // paused or has just ended is getting no new iteration. What it
                // does need is the `current-run` pointer retired: the run that
                // pointer names has just been settled above, and leaving it in
                // place would have the next tick claim this goal again, settle
                // the same run a second time, and charge the goal twice for one
                // turn.
                if in_flight.is_some() {
                    store.forget(&scope, &subject, "current-run")?;
                }
                store.release_goal(&goal.id)?;
                continue;
            }
            // A goal's objective can already be true the moment it is written
            // down, and until now nothing ever asked before paying to find
            // out. The done-when check only ever ran in the block above, which
            // settles a previous run, so a goal on its very first tick had no
            // run to settle, skipped that block, and went straight to spawning
            // an agent. The second tick then settled that agent, found the
            // check passing, and stopped — one real iteration and one real
            // bill for an objective that was met before the goal existed.
            //
            // So ask first, whenever nothing is in flight. That is a goal which
            // has never run, and now also a goal that was paused while an
            // iteration was in flight: settling that iteration retires the
            // `current-run` pointer, so the first tick after the resume arrives
            // here. Asking there is right rather than wasteful, because a goal
            // can sit paused for weeks and something other than the goal itself
            // can meet its objective in the meantime.
            //
            // Nothing is recorded that would look like work. There is no
            // `iteration` fact and no `advance_goal` call, because both would
            // claim an iteration that never ran and a cost nobody was charged
            // — the mirror of the bug where a goal denied the iteration that
            // did satisfy it. The ending itself says which of the two happened.
            //
            // This runs before the exhausted check below, so a goal that is
            // both already met and already past its limit ends satisfied. The
            // limit only says how much work the goal was allowed; it has no
            // bearing on an objective that needed none. Whichever branch is
            // taken ends the goal and moves on, so exactly one ending is ever
            // written.
            if in_flight.is_none() && self.check_done(&goal).await.is_some_and(|v| v.satisfied) {
                let state = crate::schedule::GoalState::Satisfied;
                store.set_goal_state(&goal.name, state)?;
                store.remember(
                    NewFact::new(subject.clone(), "ended", ending_note(&goal, state))
                        .in_scope(&scope)
                        .from(Origin::System),
                )?;
                store.release_goal(&goal.id)?;
                continue;
            }
            // A goal can already be past its own stop condition before it has
            // ever started an iteration: `--max-iterations 0`, a budget of
            // zero, and a negative budget are all accepted at creation today.
            // This branch used to release such a goal and move on without
            // recording anything, so its state column still said `running` and
            // the next tick claimed it, found exactly the same thing, and
            // released it again — for ever, at `iter 0`, looking in
            // `jod goal ls` exactly like a goal doing work. So record the
            // ending here the way the post-iteration path records it. The
            // state goes first, because that is the half that stops the goal
            // being claimed again; the fact then says why, so the reason
            // outlives the process that found it.
            if let Some(stop) = goal.should_stop() {
                store.set_goal_state(&goal.name, stop)?;
                store.remember(
                    NewFact::new(subject.clone(), "ended", ending_note(&goal, stop))
                        .in_scope(&scope)
                        .from(Origin::System),
                )?;
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
            // The main chat is answered, not woken. Everything else on this
            // list is a member whose turn is a fresh spawn resuming its own
            // harness session; the orchestrator is one pinned conversation that
            // has to keep accumulating, so its mail joins the delivery queue
            // and `tick_deliveries` resumes the conversation itself. Waking it
            // the ordinary way would start a run in a *new* conversation, and
            // the chat Reljod reads would never show the answer at all.
            //
            // Asked of the row rather than of the name. The name is reserved
            // from now on, but a database written before it was is not, so a
            // teammate somebody called `main` years ago must keep receiving its
            // own mail rather than having it quietly diverted here.
            if store
                .is_main_chat_member(held.scope, &held.team, &held.member.name)
                .unwrap_or(false)
            {
                match self.hand_to_main(&store, &held) {
                    Ok(0) => report.held += 1,
                    Ok(_) => report.started += 1,
                    Err(e) => {
                        report.failed += 1;
                        eprintln!("[jod/tick] could not hand mail to the main chat: {e}");
                    }
                }
                continue;
            }
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

    /// Put finished scratch sessions out of the way, and delete the ones that
    /// have been out of the way long enough.
    ///
    /// **B2 and B3 of the scratch lane, in one pass.** A scratch conversation is
    /// something the assistant started for a one-shot job — a lookup, a fetch, a
    /// calculation — and the promise the spec makes about it is that it tidies
    /// itself away when it is done. Nothing else in the system would ever do
    /// that: the run ends, the row stays in the fleet, and a week of one-line
    /// questions leaves a loose pane nobody can read.
    ///
    /// The two halves are deliberately different in what they cost you if they
    /// are wrong. Archiving hides a row and nothing more — the transcript is
    /// still there, `z` still reveals it, and a run started on the conversation
    /// puts it back. Deleting is final. So archiving happens the moment a
    /// session is finished and delivered, and deleting waits days.
    ///
    /// **A stuck session is never archived**, which is the half of B2 that is
    /// easy to lose: the whole reason to look at the fleet is that something
    /// went wrong, so a run that failed, was killed, or has been marked stalled
    /// by [`Ticker::tick_heartbeats`] keeps its row. That rule is not repeated
    /// here — [`Store::scratch_ready_to_archive`] holds every clause of it,
    /// including the join against `heartbeats.stalled_since_ms` — and it is
    /// worth saying why the temptation to repeat it should be resisted. Two
    /// copies of one rule is two places to change it and one of them will be
    /// forgotten, and the copy in this file would be the one nobody reads,
    /// because the query is where a person goes to find out what "ready" means.
    /// The tests below still assert the whole rule through this sweep, which is
    /// the level a person actually cares about it at.
    ///
    /// **Retention of zero deletes nothing at all.** That is the escape hatch
    /// for anyone who wants a scratch session kept for ever, and it is one
    /// subtraction away from being the opposite: `now_ms - 0` is `now_ms`, and a
    /// cutoff of now matches every archived row there is. The guard below is
    /// what stands between "never delete" and "delete everything, immediately",
    /// and `a_retention_of_zero_deletes_nothing` fails if it is removed.
    ///
    /// The counters read as: `claimed` is conversations this sweep had reason
    /// to act on, `started` is conversations archived or deleted, and `failed`
    /// is ones that would not budge. Nothing is ever counted `held` here, which
    /// is the one way this sweep reads differently from its siblings: a
    /// conversation that has to stay visible is never returned by the queries
    /// at all, so the rows it decides to leave alone are rows it never sees.
    pub fn tick_scratch(&self, now_ms: i64) -> Result<TickReport> {
        let Some(store) = self.jod.store().cloned() else {
            return Ok(TickReport::default());
        };
        let ready = store.scratch_ready_to_archive()?;
        let mut report = TickReport {
            claimed: ready.len(),
            ..Default::default()
        };

        for conversation_id in ready {
            match store.archive_conversation(&conversation_id, now_ms) {
                Ok(()) => report.started += 1,
                Err(e) => {
                    report.failed += 1;
                    eprintln!("[jod/tick] could not archive `{conversation_id}`: {e}");
                }
            }
        }

        let days = store.scratch_retention_days()?;
        // Zero means never, and so does anything below it: a negative setting
        // would put the cutoff in the future and match every archived row. The
        // reading that keeps a transcript is the safe one either way.
        if days <= 0 {
            return Ok(report);
        }
        // Saturating, so a machine whose clock has not been set — `now_ms` near
        // zero — produces a cutoff in the distant past that matches nothing,
        // rather than wrapping into one that matches everything.
        let before_ms = now_ms.saturating_sub(days.saturating_mul(DAY_MS));

        let old = store.scratch_ready_to_delete(before_ms)?;
        report.claimed += old.len();
        for conversation_id in old {
            match store.delete_conversation_cascade(&conversation_id) {
                Ok(()) => report.started += 1,
                Err(e) => {
                    report.failed += 1;
                    eprintln!("[jod/tick] could not delete `{conversation_id}`: {e}");
                }
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

        // **Before the interval, deliberately.** The interval below exists to
        // protect a rate-limited API, and asking a session to open its own pull
        // request never leaves this machine: it is one `git rev-list` in a
        // worktree and two writes to Jod's own database. Making it wait for the
        // poll's turn would delay it by up to five minutes for no reason, and
        // an interval is not what stops it repeating — the record on the lease
        // is, which is why once per lease is once for ever however often this
        // runs.
        self.ask_for_pull_requests(&store).await;

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

    /// Ask each session whose work looks finished to open its pull request.
    ///
    /// **The caller auto-PR never had.** [`crate::prs::auto_pr_instruction`]
    /// wrote the words and [`Store::auto_pr`] held the switch, and between them
    /// nothing ever decided a session should be asked — so the whole feature was
    /// a subsystem whose tests stayed green because nothing ran it. Reljod's
    /// requirement is that a project with a remote gets a pull request per
    /// worktree, and until this line existed that depended entirely on a model
    /// remembering to open one.
    ///
    /// **Jod does not open it.** The instruction goes to the session, which has
    /// the context and the `create-pr` skill; Jod's process never saw the work
    /// happen and a pull request it opened itself would carry no evidence. That
    /// is the same reasoning `auto_pr_instruction` is built on and this does not
    /// weaken it.
    ///
    /// The judgement is all in [`crate::prs::ask_for_pull_requests`] — whether
    /// the setting is on, whether the board is empty, whether the branch has
    /// anything on it, whether it has been asked before. What is here is the
    /// delivery and the bookkeeping, the same division `tick_deliveries` keeps
    /// with `plan_injection`.
    ///
    /// The words travel by the delivery queue rather than by a spawn of its
    /// own, so a session that is mid-turn is not trampled and one with no
    /// harness session to resume is left alone — `tick_deliveries` already
    /// decides both, and doing it again here would be the same question
    /// answered in two places.
    ///
    /// **Nothing here is fatal.** A pull request nobody was asked for is a
    /// missing pull request, not a reason to stop a tick that also fires
    /// schedules — the same call `trim_ledger` makes.
    async fn ask_for_pull_requests(&self, store: &Arc<Store>) {
        let store = store.clone();
        // Blocking: it spawns `git` and writes to SQLite. On a runtime thread
        // that stalls every other task the daemon is running, and the symptom
        // reads as the scheduler being slow.
        let asked = tokio::task::spawn_blocking(move || {
            crate::prs::ask_for_pull_requests(&store, |ask| {
                store
                    .enqueue_delivery(
                        &ask.candidate.conversation_id,
                        crate::delivery::Kind::Jod,
                        // The lease, because that is what the ask is *about* and
                        // what is recorded against — one worktree, one branch,
                        // one pull request.
                        &format!("auto-pr:{}", ask.candidate.lease_id),
                        &ask.instruction,
                    )
                    .map(|_| ())
            })
        })
        .await;

        match asked {
            Ok(Ok(asks)) if !asks.is_empty() => eprintln!(
                "[jod/tick] asked {} session(s) to open a pull request",
                asks.len()
            ),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("[jod/tick] could not ask for a pull request: {e}"),
            Err(joined) => eprintln!("[jod/tick] the pull request ask did not come back: {joined}"),
        }
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
        // Read once for the whole sweep, so the question "is this the main
        // chat" is answered from one value rather than re-resolved per
        // conversation and given a chance to disagree with itself.
        let main_chat = store.pinned_conversation()?;
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
                //
                // The main chat is the exception, and it has to be. It is the
                // orchestrator, `hand_to_orchestrator` gives it
                // `ToolAccess::Orchestrate` on every turn a person starts, and
                // a turn started by an answer coming back from a delegated run
                // is the same chat with the same job. Handing it a smaller
                // toolbox because of how the turn began would mean the answer
                // it delegated for arrives and it can no longer arm the
                // schedule it was about to arm.
                tools: Some(delivery_access(&conversation_id, main_chat.as_deref())),
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
    /// Move mail addressed to the main chat onto the main chat's own queue.
    ///
    /// The return leg Reljod asked for: a run he delegated something to says
    /// what the answer is, and the orchestrator takes a turn carrying it. No
    /// new delivery mechanism — [`crate::delivery`] already turns something
    /// waiting for a conversation into a resumed turn, batching whatever else
    /// arrives in the meantime into the same one, and the main chat is the one
    /// member of any roster that *is* a conversation.
    ///
    /// Returns how many messages moved. Zero means there was nothing to hand
    /// over, or that the chat is not resumable yet — a pinned conversation that
    /// has never run has no session, and an orchestrator resumed into a fresh
    /// context would answer having forgotten what it delegated. The mail stays
    /// on the bus and visible, which is the same choice `wake_order` makes for
    /// a member with no session.
    fn hand_to_main(&self, store: &Store, held: &team::Waiting) -> Result<usize> {
        let Some(conversation) = store.pinned_conversation()? else {
            return Ok(0);
        };
        if !store.main_chat_is_resumable()? {
            return Ok(0);
        }
        let moved =
            store.hand_mail_to_conversation(&held.team, &held.member.name, &conversation)?;
        if moved > 0 {
            eprintln!(
                "[jod/tick] queued {moved} message(s) for the main chat from {}",
                held.team
            );
        }
        Ok(moved)
    }

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
    ///
    /// Read in the goal's own scope, not by subject alone. The subject is
    /// `goal/<name>` and a name can be handed to a second goal, so a
    /// subject-only read would compare this goal's check against one a
    /// removed goal ran.
    fn last_fingerprint(
        &self,
        store: &Store,
        scope: &str,
        subject: &str,
    ) -> Result<Option<String>> {
        Ok(store
            .facts_about_in_scope(scope, subject)?
            .into_iter()
            .find(|f| f.predicate == "done-when")
            .map(|f| f.object))
    }

    /// The run this goal has in flight, if any.
    ///
    /// Scoped for the same reason as [`Ticker::last_fingerprint`], and here it
    /// matters more: a stale pointer inherited from another goal names a run
    /// this goal never started, and the tick would wait on it.
    fn current_run(&self, store: &Store, scope: &str, subject: &str) -> Result<Option<String>> {
        Ok(store
            .facts_about_in_scope(scope, subject)?
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
            .facts_about_in_scope(scope, subject)?
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
        let existing = store.facts_about_in_scope(scope, subject)?;
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

    /// A real run row, copied out of a live `jod.db`, with one non-defaulted
    /// field — `harness_label` — taken out of its summary. That single edit
    /// stands in for a summary written by a build whose `AgentSummary` had a
    /// different shape, which is one of the two ways a real run stops being
    /// readable. Everything else, the cost included, is exactly as the run
    /// recorded it.
    const REAL_RUN_ID: &str = "3fc37418-0319-4745-a582-3ce7698429ea";
    const REAL_RUN_COST: f64 = 0.2248795;
    const REAL_RUN_SUMMARY_FROM_AN_OLDER_BUILD: &str = r#"{
        "created_at_ms": 1786732217935, "cwd": "/home/reljod", "event_count": 14,
        "harness": "claude_code", "id": "3fc37418-0319-4745-a582-3ce7698429ea",
        "last_message": "Run `e442de9a` is live and building",
        "model": "claude-opus-5", "name": "main", "permission": "accept_edits",
        "pgid": 3685911, "pid": 3685911, "process_alive": false,
        "session_id": "2d844cee-13f4-418c-9e92-f5d5c764e885", "status": "completed",
        "usage": {"cache_read_tokens": 49179, "cache_write_tokens": 19527,
                  "cost_usd": 0.2248795, "input_tokens": 4, "output_tokens": 200},
        "watch_command": "jod watch 3fc37418-0319-4745-a582-3ce7698429ea"
    }"#;

    /// A settled run under a goal, stored with whatever summary is given.
    fn a_run_with_summary(store: &Store, id: &str, summary: serde_json::Value) {
        store
            .save_run(&crate::store::StoredRun {
                id: id.into(),
                name: "the goal's iteration".into(),
                harness: "claude_code".into(),
                status: "completed".into(),
                cwd: "/tmp".into(),
                session_id: None,
                pid: Some(4_000_000),
                pgid: Some(4_000_000),
                created_at_ms: 0,
                summary,
            })
            .unwrap();
    }

    /// Point a goal at a finished run and give it a reading of its check from
    /// the iteration before, so the tick has something to compare against.
    fn goal_waiting_on(store: &Store, goal: &Goal, run_id: &str, previous_reading: &str) {
        for (predicate, object) in [("current-run", run_id), ("done-when", previous_reading)] {
            store
                .remember(
                    NewFact::new(format!("goal/{}", goal.name), predicate, object)
                        .in_scope(goal.memory_scope())
                        .from(Origin::System),
                )
                .unwrap();
        }
        store.run_goal_now(&goal.name, 1).unwrap();
    }

    /// G13. A run that cannot be read back is still an iteration the goal ran
    /// and was billed for, and the old code said none of that: it advanced the
    /// counter, wrote no line into `jod goal log`, added `0.0` to `spent_usd`,
    /// and counted the iteration against the stall counter.
    ///
    /// This is the route that needs no substitution at all. The run is a plain
    /// finished run with a real cost; the process simply never loaded it,
    /// exactly as happens to any run that falls outside the daemon's 200-run
    /// rehydrate window.
    #[tokio::test]
    async fn an_iteration_whose_run_cannot_be_read_is_still_recorded_in_full() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("unreadable");
        // A check that keeps failing, so the goal is not satisfied here and the
        // ordinary settling path is the one under test.
        g.done_when = Some("false".into());
        store.add_goal(&g).unwrap();
        // One earlier iteration that changed nothing, so the stall counter
        // starts at 1 and a reset is visible rather than indistinguishable
        // from never having moved.
        store.advance_goal(&g.id, 1, 0.0, false).unwrap();
        assert_eq!(store.goal_named("unreadable").unwrap().unwrap().no_progress, 1);

        a_finished_run(&store, "run-unreadable", REAL_RUN_COST, "did the work");
        goal_waiting_on(&store, &g, "run-unreadable", "what the check said last time");

        let jod = Jod::with_store(store.clone());
        // No rehydrate: this is a run the process never loaded.
        assert!(
            jod.agent("run-unreadable").await.is_err(),
            "the run has to be unreadable, or this tests the ordinary path"
        );
        Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();

        let facts = store.facts_about("goal/unreadable").unwrap();
        let history: Vec<_> = facts.iter().filter(|f| f.predicate == "iteration").collect();
        assert_eq!(
            history.len(),
            1,
            "the iteration has to appear in `jod goal log`, once"
        );
        assert!(
            history[0].object.starts_with("2: "),
            "it is the goal's second iteration: {}",
            history[0].object
        );
        assert!(
            history[0].object.contains("could not be read back"),
            "the line says plainly that the run could not be read: {}",
            history[0].object
        );
        assert!(
            history[0].object.contains("$0.2249"),
            "and it names the cost it recovered: {}",
            history[0].object
        );

        let after = store.goal_named("unreadable").unwrap().unwrap();
        assert_eq!(after.iteration, 2, "the counter still advances");
        assert!(
            (after.spent_usd - REAL_RUN_COST).abs() < 1e-9,
            "the run's real cost has to reach spent_usd, not 0.0: {}",
            after.spent_usd
        );
        assert_eq!(
            after.no_progress, 0,
            "the check moved, so the goal made progress — an unreadable run is \
             not evidence against it"
        );
    }

    /// The second demonstrated route, and the one that needed a substitution:
    /// the summary on disk cannot be turned into an `AgentSummary` by this
    /// build, so `rehydrate` skips the row. The cost is still in that summary,
    /// as JSON, and recovering it is better than writing a zero.
    #[tokio::test]
    async fn a_summary_an_older_build_wrote_still_gives_up_its_cost() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("older-build");
        g.done_when = Some("false".into());
        store.add_goal(&g).unwrap();

        a_run_with_summary(
            &store,
            REAL_RUN_ID,
            serde_json::from_str(REAL_RUN_SUMMARY_FROM_AN_OLDER_BUILD).unwrap(),
        );
        goal_waiting_on(&store, &g, REAL_RUN_ID, "what the check said last time");

        let jod = Jod::with_store(store.clone());
        assert_eq!(
            jod.rehydrate(200).await.unwrap(),
            0,
            "a summary this build cannot parse is skipped, which is what puts \
             the run out of reach"
        );
        Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();

        let after = store.goal_named("older-build").unwrap().unwrap();
        assert!(
            (after.spent_usd - REAL_RUN_COST).abs() < 1e-9,
            "the cost is still in the stored summary and has to be recovered \
             from it: {}",
            after.spent_usd
        );
        let line = store
            .facts_about("goal/older-build")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "iteration")
            .expect("the iteration is in the log");
        assert!(line.object.contains("$0.2249"), "{}", line.object);
    }

    /// And when the cost genuinely cannot be recovered from anywhere, the log
    /// says the cost is unknown. Writing `$0.00` there would be a claim that
    /// the iteration was free, which is a different and false statement.
    #[tokio::test]
    async fn a_cost_that_cannot_be_recovered_is_recorded_as_unknown_not_zero() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("no-bill");
        g.done_when = Some("false".into());
        store.add_goal(&g).unwrap();

        // A row with no usage in its summary and no events behind it: nothing
        // anywhere records what this run cost.
        a_run_with_summary(&store, "run-no-bill", serde_json::json!({"id": "run-no-bill"}));
        goal_waiting_on(&store, &g, "run-no-bill", "what the check said last time");

        let jod = Jod::with_store(store.clone());
        Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();

        let line = store
            .facts_about("goal/no-bill")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "iteration")
            .expect("the iteration is in the log even when nothing is known about it");
        assert!(
            line.object.contains("its cost is unknown"),
            "an unknown cost has to read as unknown: {}",
            line.object
        );
        assert!(
            !line.object.contains('$'),
            "and it must not name a figure it does not have: {}",
            line.object
        );
        let after = store.goal_named("no-bill").unwrap().unwrap();
        assert_eq!(after.iteration, 1, "the iteration still happened");
        assert_eq!(
            after.spent_usd, 0.0,
            "there is no number to add, so the total is unchanged"
        );
    }

    /// A goal paused in the middle of an iteration, with that iteration
    /// finished and waiting to be settled.
    fn a_goal_paused_mid_iteration(store: &Store, name: &str, check: &str, cost_usd: f64) {
        let mut g = a_goal(name);
        g.done_when = Some(check.into());
        store.add_goal(&g).unwrap();
        a_finished_run(store, &format!("run-{name}"), cost_usd, "Read three files");
        store
            .remember(
                NewFact::new(format!("goal/{name}"), "current-run", format!("run-{name}"))
                    .in_scope(g.memory_scope())
                    .from(Origin::System),
            )
            .unwrap();
        store.run_goal_now(name, 1).unwrap();
        store
            .set_goal_state(name, crate::schedule::GoalState::Paused)
            .unwrap();
    }

    /// G14. A goal paused mid-iteration knew nothing about the run it still had
    /// going. `claim_due_goals` looks only at running goals, and the tick it
    /// gates is the only thing that settles a finished run or reads what it
    /// cost, so a real goal read `iter 0 · $0.00` beside an iteration that had
    /// already finished and cost $0.0963. Only resuming it made it admit the
    /// bill, which meant the record of a paused goal was wrong for as long as
    /// the pause lasted.
    #[tokio::test]
    async fn a_paused_goals_finished_iteration_is_settled_without_a_resume() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        // A check that keeps failing, so what is under test is the ordinary
        // settling rather than the satisfied ending.
        a_goal_paused_mid_iteration(&store, "paused-mid-run", "false", 0.0963);

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        let report = Ticker::new(jod)
            .as_owner("t")
            .tick_goals(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert_eq!(
            report.started, 0,
            "pausing still means no new iteration starts"
        );

        let after = store.goal_named("paused-mid-run").unwrap().unwrap();
        assert_eq!(
            after.iteration, 1,
            "the iteration finished, so the goal has run one"
        );
        assert!(
            (after.spent_usd - 0.0963).abs() < 1e-9,
            "the goal was billed $0.0963 for a turn it denies: spent_usd is {}",
            after.spent_usd
        );
        assert_eq!(
            after.state,
            crate::schedule::GoalState::Paused,
            "settling a goal is not resuming it"
        );

        let facts = store.facts_about("goal/paused-mid-run").unwrap();
        assert_eq!(
            facts.iter().filter(|f| f.predicate == "iteration").count(),
            1,
            "`goal log` has to show the iteration, once"
        );
        assert!(
            !facts.iter().any(|f| f.predicate == "ended"),
            "a pause is not an ending, and writing one down is not undone by a resume"
        );
    }

    /// The other half of settling a paused goal: it happens exactly once. The
    /// pointer at the run has to be retired, because a paused goal is never
    /// *due* and so nothing else would ever stop the next tick claiming it,
    /// settling the same finished run again, and charging the goal a second
    /// time for one turn.
    #[tokio::test]
    async fn a_paused_goals_iteration_is_settled_once_however_many_ticks_run() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        a_goal_paused_mid_iteration(&store, "paused-twice", "false", 0.0963);

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        let ticker = Ticker::new(jod).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        ticker.tick_goals(now).await.unwrap();
        assert_eq!(
            ticker.tick_goals(now).await.unwrap().claimed,
            0,
            "with the run settled there is nothing left to claim it for"
        );

        let after = store.goal_named("paused-twice").unwrap().unwrap();
        assert_eq!(after.iteration, 1, "one turn, counted once");
        assert!(
            (after.spent_usd - 0.0963).abs() < 1e-9,
            "one turn, billed once: spent_usd is {}",
            after.spent_usd
        );
    }

    /// The loop this fix must not create. A paused goal with nothing in flight
    /// has nothing to settle, so it is never claimed at all — the failure that
    /// would follow from simply widening the due-goal claim, which would hand
    /// the same goal back on every tick for the rest of its life.
    #[tokio::test]
    async fn a_paused_goal_with_nothing_in_flight_is_never_claimed() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        store.add_goal(&a_goal("paused-idle")).unwrap();
        store.run_goal_now("paused-idle", 1).unwrap();
        store
            .set_goal_state("paused-idle", crate::schedule::GoalState::Paused)
            .unwrap();

        let ticker = Ticker::new(Jod::with_store(store.clone())).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        for tick in 1..=3 {
            assert_eq!(
                ticker.tick_goals(now).await.unwrap().claimed,
                0,
                "tick {tick} found nothing to settle and must leave the goal alone"
            );
        }
        let after = store.goal_named("paused-idle").unwrap().unwrap();
        assert_eq!(after.iteration, 0);
        assert_eq!(after.spent_usd, 0.0);
    }

    /// The judgement call this change contains, written down. Settling runs the
    /// goal's `done-when` check on a paused goal exactly as on a running one,
    /// because the verdict is a measurement of the iteration that has just
    /// finished and it cannot honestly be taken later: by the time somebody
    /// resumes the goal, days may have passed and the check would be answering
    /// about a different world. So a paused goal whose iteration met the
    /// objective ends satisfied rather than sitting on a stale record.
    ///
    /// It still starts nothing. Every state settling can leave a paused goal in
    /// — paused, satisfied, stalled or exhausted — is one that no claim will
    /// iterate, so this path can only ever leave a goal further from running.
    #[tokio::test]
    async fn settling_a_paused_goal_runs_its_check_and_can_end_it_satisfied() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        a_goal_paused_mid_iteration(&store, "paused-done", "true", 0.0963);

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        Ticker::new(jod).as_owner("t").tick_goals(now).await.unwrap();

        let after = store.goal_named("paused-done").unwrap().unwrap();
        assert_eq!(
            after.state,
            crate::schedule::GoalState::Satisfied,
            "the check passed, and the iteration that made it pass was paid for"
        );
        assert_eq!(after.iteration, 1);
        assert!(
            (after.spent_usd - 0.0963).abs() < 1e-9,
            "spent_usd is {}",
            after.spent_usd
        );
        assert!(
            store
                .claim_due_goals("t", now + 86_400_000, 60_000)
                .unwrap()
                .is_empty(),
            "whatever settling leaves behind, nothing iterates it"
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

    /// A goal can be past its own stop condition before it has ever run —
    /// `--max-iterations 0` is accepted today, and so is a budget of zero.
    /// Such a goal used to be claimed, found already stopped, released with its
    /// state column still saying `running`, and then claimed again by the very
    /// next tick, for ever, sitting at `iter 0` while `jod goal ls` reported it
    /// as working. The ending has to be recorded the first time the tick sees
    /// it, or nothing ever stops asking.
    #[tokio::test]
    async fn a_goal_that_can_never_run_is_ended_the_first_time_it_is_ticked() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("zero-iter");
        g.max_iterations = Some(0);
        store.add_goal(&g).unwrap();
        store.run_goal_now("zero-iter", 1).unwrap();

        let ticker = Ticker::new(Jod::with_store(store.clone())).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        let first = ticker.tick_goals(now).await.unwrap();
        assert_eq!(first.claimed, 1, "the first tick reaches it");
        assert_eq!(first.started, 0, "and must not start an iteration");

        let after = store.goal_named("zero-iter").unwrap().unwrap();
        assert_eq!(
            after.state,
            crate::schedule::GoalState::Exhausted,
            "the state column has to say it stopped, or it is claimed for ever"
        );

        // The loop is the failure, so prove the loop is over: a second tick
        // with nothing else changed must not find the goal at all.
        let second = ticker.tick_goals(now).await.unwrap();
        assert_eq!(
            second.claimed, 0,
            "a goal that can never run must not be claimed a second time"
        );

        // And a person has to be able to see what happened. "exhausted" on a
        // goal at iteration zero means it was never allowed to start, which is
        // not the same as running out, so the log says which.
        let endings: Vec<_> = store
            .facts_about("goal/zero-iter")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "ended")
            .collect();
        assert_eq!(endings.len(), 1, "one ending, written once");
        assert!(
            endings[0].object.starts_with("exhausted"),
            "the ending names the state: {}",
            endings[0].object
        );
        assert!(
            endings[0].object.contains("before its first iteration"),
            "the ending says it never started: {}",
            endings[0].object
        );
        assert!(
            endings[0].object.contains('0'),
            "the ending names the limit that stopped it: {}",
            endings[0].object
        );
    }

    /// The same hole, reached through the budget rather than the iteration
    /// count. A negative budget is accepted at creation today, and `should_stop`
    /// is true for it from the start.
    #[tokio::test]
    async fn a_goal_with_a_budget_it_cannot_spend_under_is_ended_too() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("neg-budget");
        g.budget_usd = Some(-5.0);
        store.add_goal(&g).unwrap();
        store.run_goal_now("neg-budget", 1).unwrap();

        let ticker = Ticker::new(Jod::with_store(store.clone())).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        ticker.tick_goals(now).await.unwrap();

        assert_eq!(
            store.goal_named("neg-budget").unwrap().unwrap().state,
            crate::schedule::GoalState::Exhausted
        );
        assert_eq!(
            ticker.tick_goals(now).await.unwrap().claimed,
            0,
            "and it is not claimed again"
        );
        let ending = store
            .facts_about("goal/neg-budget")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "ended")
            .expect("the goal recorded an ending");
        assert!(
            ending.object.contains("$-5.00"),
            "the ending names the budget it was given: {}",
            ending.object
        );
    }

    /// The objective can already be true the moment the goal is created. The
    /// check for that used to live only in the block that settles a previous
    /// run, so a brand new goal had no run to settle, skipped the block, and
    /// went straight to spawning an agent. The first tick paid for an iteration
    /// and the second one noticed there had never been anything to do. Nothing
    /// may be spawned here, and nothing may be recorded that claims work was
    /// done.
    #[tokio::test]
    async fn a_goal_whose_objective_is_already_met_never_pays_for_an_iteration() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("already-met");
        g.done_when = Some("true".into());
        store.add_goal(&g).unwrap();
        store.run_goal_now("already-met", 1).unwrap();

        let ticker = Ticker::new(Jod::with_store(store.clone())).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        let first = ticker.tick_goals(now).await.unwrap();
        assert_eq!(first.claimed, 1, "the first tick reaches it");
        assert_eq!(
            first.started + first.failed,
            0,
            "the money is spent at the spawn, so the spawn must not be attempted"
        );

        let facts = store.facts_about("goal/already-met").unwrap();
        assert!(
            !facts.iter().any(|f| f.predicate == "current-run"),
            "no run was started, so the goal has none in flight"
        );

        let after = store.goal_named("already-met").unwrap().unwrap();
        assert_eq!(
            after.state,
            crate::schedule::GoalState::Satisfied,
            "the objective was met, so the goal is finished"
        );
        // The mirror of the bug that made the satisfying iteration invisible: a
        // goal that never ran must not claim an iteration or a cost it never
        // incurred.
        assert_eq!(after.iteration, 0, "no iteration ran");
        assert_eq!(after.spent_usd, 0.0, "and nothing was spent");
        assert!(
            !facts.iter().any(|f| f.predicate == "iteration"),
            "an iteration line here would describe work that never happened"
        );

        // The ending has to read differently from a goal satisfied by its own
        // work, or the log cannot tell a free check from a paid one.
        let endings: Vec<_> = facts.iter().filter(|f| f.predicate == "ended").collect();
        assert_eq!(endings.len(), 1, "one ending, written once");
        assert!(
            endings[0].object.starts_with("satisfied"),
            "the ending names the state: {}",
            endings[0].object
        );
        assert!(
            endings[0].object.contains("before its first iteration"),
            "the ending says it never ran: {}",
            endings[0].object
        );

        assert_eq!(
            ticker.tick_goals(now).await.unwrap().claimed,
            0,
            "a finished goal is not claimed again"
        );
    }

    /// Both endings are available at once: the objective is already true and
    /// the goal was also created with a limit it is already past. Satisfied
    /// wins, because what the goal was asked to achieve has been achieved and
    /// the limit it never reached says nothing about that. Exactly one ending
    /// is recorded either way.
    #[tokio::test]
    async fn a_goal_both_already_met_and_already_out_of_iterations_ends_satisfied() {
        let store = std::sync::Arc::new(crate::store::Store::in_memory().unwrap());
        let mut g = a_goal("met-and-capped");
        g.done_when = Some("true".into());
        g.max_iterations = Some(0);
        store.add_goal(&g).unwrap();
        store.run_goal_now("met-and-capped", 1).unwrap();

        let ticker = Ticker::new(Jod::with_store(store.clone())).as_owner("t");
        let now = chrono::Utc::now().timestamp_millis();
        ticker.tick_goals(now).await.unwrap();

        assert_eq!(
            store.goal_named("met-and-capped").unwrap().unwrap().state,
            crate::schedule::GoalState::Satisfied,
            "the objective is met, whatever the unused limit says"
        );
        let endings: Vec<_> = store
            .facts_about("goal/met-and-capped")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "ended")
            .collect();
        assert_eq!(endings.len(), 1, "one ending, not one of each");
        assert!(
            endings[0].object.starts_with("satisfied"),
            "and it is the satisfied one: {}",
            endings[0].object
        );
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

    // ---- what a `fire_once` catch-up passed over ----

    /// Twenty-four instants came due, one of them gets the run, and the other
    /// twenty-three are what the catch-up passed over.
    #[test]
    fn a_catch_up_accounts_for_every_instant_but_the_one_it_ran() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let missed: Vec<i64> = (1..=24).map(|i| i * 900_000).collect();
        let steps = decide(&s, &missed, None);
        assert_eq!(steps.len(), 1, "still exactly one decision");
        assert_eq!(
            caught_up(&s, &missed, &steps),
            Some(CaughtUp {
                passed_over: 23,
                from_ms: 900_000,
                to_ms: 23 * 900_000,
            })
        );
    }

    /// An ordinary tick is not an outage and must not start narrating one.
    #[test]
    fn one_instant_due_passed_nothing_over() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let steps = decide(&s, &[5_000], None);
        assert_eq!(caught_up(&s, &[5_000], &steps), None);
    }

    /// `skip` writes a row per instant already. A summary on top of those would
    /// count the same drops twice.
    #[test]
    fn skip_gets_no_summary_because_it_already_wrote_every_row() {
        let s = sched(Misfire::Skip, Overlap::Skip);
        let missed = vec![1_000, 2_000, 3_000];
        let steps = decide(&s, &missed, None);
        assert_eq!(caught_up(&s, &missed, &steps), None);
    }

    /// Nothing was caught up by a tick that is going to start nothing. The
    /// decision that stopped it writes its own row saying so.
    #[test]
    fn a_tick_that_starts_nothing_claims_no_catch_up() {
        let s = sched(Misfire::FireOnce, Overlap::Skip);
        let missed: Vec<i64> = (1..=24).map(|i| i * 900_000).collect();

        let suppressed = plan(decide(&s, &missed, None), Some(&monitor::Decision::Suppress));
        assert_eq!(caught_up(&s, &missed, &suppressed), None);

        let held_by_overlap = decide(&s, &missed, Some("run-1"));
        assert_eq!(caught_up(&s, &missed, &held_by_overlap), None);
    }

    /// The line has to answer the questions a person actually has after an
    /// outage: how many, and over what stretch of time.
    #[test]
    fn the_line_names_how_many_and_over_what_window() {
        let caught = CaughtUp {
            passed_over: 23,
            from_ms: at_ms("2026-01-15T08:15:00Z"),
            to_ms: at_ms("2026-01-15T13:45:00Z"),
        };
        let detail = caught.detail("Asia/Manila");
        assert!(detail.starts_with("23 instants missed"), "{detail}");
        assert!(
            detail.contains("2026-01-15 16:15 PST") && detail.contains("2026-01-15 21:45 PST"),
            "the window is written in the schedule's own zone: {detail}"
        );
    }

    fn at_ms(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    }

    /// One armed `fire_once` schedule on a fifteen-minute cron that last fired
    /// six hours ago, so twenty-four instants have come due with nothing
    /// running to take them.
    fn six_hours_down() -> Arc<Store> {
        let store = Arc::new(Store::in_memory().unwrap());
        let mut s = sched(Misfire::FireOnce, Overlap::Skip);
        s.cron = "*/15 * * * *".into();
        store.add_schedule(&s).unwrap();
        // `add_schedule` arms from the real clock, so the fixture's own instants
        // are written over the top of it. `next_fire_at_ms` is deliberately one
        // of the missed instants: an instant outside the series would be added
        // to the list by `missed_for` and make the count depend on the fixture
        // rather than on the outage.
        let last = at_ms("2026-01-15T08:00:00Z");
        let next = at_ms("2026-01-15T14:00:00Z");
        store
            .write(|tx| {
                tx.execute(
                    &format!(
                        "UPDATE schedules
                            SET last_fire_at_ms = {last}, next_fire_at_ms = {next}"
                    ),
                    [],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        store
    }

    /// **The check for S4.** Both halves have to hold at once: a fix that wrote
    /// down what it dropped but also fired more than once would be worse than
    /// the silence it replaced.
    ///
    /// Six hours down on a fifteen-minute cron is twenty-four instants. One of
    /// them gets a run and the other twenty-three do not, and the tick is
    /// expected to say so — `FireOutcome`'s own rule is that every outcome is
    /// written down, because a skip nobody recorded is a silent failure.
    ///
    /// The run is asserted as `started + failed` because a spawn in a test has
    /// no supervisor to talk to. Either way it is one attempt and one row, and
    /// one is the whole point.
    #[tokio::test]
    async fn an_outage_under_the_default_policy_runs_once_and_says_what_it_dropped() {
        let store = six_hours_down();
        let now = at_ms("2026-01-15T14:00:00Z");

        let report = Ticker::new(Jod::with_store(store.clone()))
            .as_owner("t")
            .tick(now)
            .await
            .unwrap();

        assert_eq!(report.claimed, 1);
        assert_eq!(
            report.started + report.failed,
            1,
            "an outage still buys exactly one run, whatever else is recorded"
        );

        let fires = store.fires("s1", 100).unwrap();
        assert_eq!(
            fires.len(),
            2,
            "the run, and one line accounting for what it passed over: {fires:?}"
        );
        // Newest first, so the run leads and `jod schedule log` still opens on
        // the thing that happened rather than on the outage behind it.
        assert!(
            matches!(
                fires[0].outcome,
                FireOutcome::Ran | FireOutcome::SpawnFailed
            ),
            "{:?}",
            fires[0]
        );

        let passed_over = &fires[1];
        assert_eq!(passed_over.outcome, FireOutcome::SkippedMisfire);
        assert!(
            passed_over.run_id.is_none(),
            "nothing ran for the instants it names"
        );
        assert_eq!(
            passed_over.due_at_ms,
            at_ms("2026-01-15T08:15:00Z"),
            "dated to the oldest instant it accounts for"
        );
        let detail = passed_over.detail.clone().unwrap_or_default();
        for expected in ["23", "08:15", "13:45"] {
            assert!(
                detail.contains(expected),
                "the line has to name how many and over what window, not just that some went missing: {detail}"
            );
        }
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

        /// A conversation holding one message from this run, so a card raised
        /// about the run has somewhere to land.
        fn conversation_for(store: &Store, run_id: &str) -> String {
            use crate::conversation::{NewMessage, Role};
            let c = store
                .new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            store
                .append_message(
                    &c.id,
                    NewMessage::new(Role::Assistant, "starting").from_run(run_id),
                )
                .unwrap();
            c.id
        }

        /// Stop a group the test started, so a failing assertion does not leave
        /// a `sleep 300` behind on the box for five minutes.
        fn kill_group(pgid: u32) {
            unsafe { libc::kill(-(pgid as i32), libc::SIGKILL) };
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pgid as i32, &mut status, 0) };
        }

        /// Silent past its window, and alive — the case the module exists for,
        /// and the one nothing else in Jod can detect.
        ///
        /// **This used to assert the opposite**, under the name
        /// `a_stalled_run_is_stopped_marked_failed_and_unwatched`. Reljod chose
        /// mark-and-surface for a session he is watching: killing it destroys a
        /// transcript and possibly a checkout mid-edit, to fix something he can
        /// see and decide about himself. The reap is not gone — it is still
        /// what a *goal* iteration gets, which the test below this one holds.
        #[tokio::test]
        async fn a_stalled_session_is_marked_and_left_running() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();
            conversation_for(&store, "r1");

            let now = 10_000_000_000;
            let quiet_since = now - heartbeat::DEFAULT_STALL_MS - 1;
            store
                .watch_run(&Heartbeat::starting("r1", Watching::Run, quiet_since))
                .unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.checked, 1);
            assert_eq!(report.marked, 1, "it should have been marked");
            assert_eq!(report.stopped, 0, "and it must not have been stopped");
            assert_eq!(report.retired, 0, "nor stopped being watched");
            assert_eq!(
                store.run("r1").unwrap().unwrap().status,
                "running",
                "a session Reljod is watching keeps its status; only the mark is new"
            );

            // The mark is on the row, and says when it went quiet rather than
            // when the sweep happened to look.
            let hb = store.heartbeat("r1").unwrap().expect("still watched");
            assert_eq!(hb.stalled_since_ms, Some(quiet_since));
            assert_eq!(store.stalled_runs().unwrap().get("r1"), Some(&quiet_since));

            // And the process is still there. This is the assertion that would
            // have failed before the split, and the whole point of the change:
            // `WNOHANG` returning 0 means the child has not exited at all.
            let mut status: libc::c_int = 0;
            let reaped = unsafe { libc::waitpid(pgid as i32, &mut status, libc::WNOHANG) };
            assert_eq!(reaped, 0, "the sweep killed a session it was only meant to mark");

            let why = store.facts_about("run/r1").unwrap();
            assert!(
                why.iter().any(|f| f.predicate == "stalled"),
                "nothing recorded that the run went quiet: {why:?}"
            );

            kill_group(pgid);
        }

        /// Check 6. One card on the first stalled tick, and none on the second
        /// — a sweep runs every tick, and a rail that gained a card a minute for
        /// one wedged agent would be unreadable by lunchtime.
        #[tokio::test]
        async fn a_stall_raises_one_card_however_many_times_it_is_swept() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();
            let conversation = conversation_for(&store, "r1");

            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting(
                    "r1",
                    Watching::Run,
                    now - heartbeat::DEFAULT_STALL_MS - 1,
                ))
                .unwrap();

            let ticker = ticker_over(&store);
            let first = ticker.tick_heartbeats(now).await.unwrap();
            let second = ticker.tick_heartbeats(now + 60_000).await.unwrap();

            assert_eq!(first.marked, 1);
            assert_eq!(second.marked, 0, "the second sweep marked it all over again");

            let cards = store
                .cards(&crate::cards::Query {
                    conversation_id: Some(conversation),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(cards.len(), 1, "one stall, one card: {cards:?}");
            assert!(!cards[0].blocking, "nothing is waiting on an answer to this");
            assert!(
                cards[0].title.contains("stuck"),
                "the card must say what it is about: {}",
                cards[0].title
            );

            kill_group(pgid);
        }

        /// Check 5, end to end. It went quiet and came back; that is not a
        /// failure and it must stop looking like one.
        #[tokio::test]
        async fn a_marked_session_that_speaks_again_is_unmarked() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();
            conversation_for(&store, "r1");

            let now = 10_000_000_000;
            store
                .watch_run(&Heartbeat::starting(
                    "r1",
                    Watching::Run,
                    now - heartbeat::DEFAULT_STALL_MS - 1,
                ))
                .unwrap();

            let ticker = ticker_over(&store);
            ticker.tick_heartbeats(now).await.unwrap();
            assert!(store.heartbeat("r1").unwrap().unwrap().is_stalled());

            store
                .append_event(&crate::event::AgentEnvelope {
                    agent_id: "r1".into(),
                    at_ms: now,
                    seq: 0,
                    event: crate::event::AgentEvent::Message { text: "back".into() },
                })
                .unwrap();
            let report = ticker.tick_heartbeats(now + 1_000).await.unwrap();

            assert_eq!(report.beating, 1);
            assert_eq!(
                store.heartbeat("r1").unwrap().unwrap().stalled_since_ms,
                None,
                "the mark stayed on a run that is plainly working"
            );
            assert!(store.stalled_runs().unwrap().is_empty());

            kill_group(pgid);
        }

        /// Check 4, end to end, and the regression guard on the split. A goal
        /// iteration that wedges blocks its goal's loop for ever and nothing
        /// else will ever notice, so it is still reaped.
        #[tokio::test]
        async fn a_stalled_goal_iteration_is_still_stopped_failed_and_unwatched() {
            let store = Arc::new(Store::in_memory().unwrap());
            let pgid = a_living_group();
            store.save_run(&stored("r1", "running", Some(pgid))).unwrap();

            let now = 10_000_000_000;
            let hb = Heartbeat::starting(
                "r1",
                Watching::Goal("green-ci".into()),
                now - heartbeat::DEFAULT_STALL_MS - 1,
            );
            store.watch_run(&hb).unwrap();

            let report = ticker_over(&store).tick_heartbeats(now).await.unwrap();

            assert_eq!(report.checked, 1);
            assert_eq!(report.stopped, 1, "a stalled goal iteration must be stopped");
            assert_eq!(report.marked, 0, "a goal is reaped, not marked");
            assert_eq!(report.retired, 1);
            assert_eq!(
                store.run("r1").unwrap().unwrap().status,
                "failed",
                "a wedged iteration must stop claiming to be running"
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

    mod scratch {
        use super::*;
        use crate::cards::NewCard;
        use crate::conversation::{NewMessage, Role};
        use crate::daemon::Tick;
        use crate::harness::HarnessKind;
        use crate::store::{Store, SCRATCH_RETENTION_DAYS_KEY};

        fn ticker_over(store: &Arc<Store>) -> Ticker {
            Ticker::new(Jod::with_store(store.clone())).as_owner("t")
        }

        /// A run, and the message that ties it to a conversation.
        ///
        /// The message is not decoration: nothing on `runs` names a
        /// conversation, so `messages.run_id` is the only join there is, and a
        /// run saved without one belongs to nobody.
        fn run_in(store: &Store, conversation: &str, id: &str, status: &str) {
            store
                .save_run(&crate::store::StoredRun {
                    id: id.into(),
                    name: "errand".into(),
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
                    NewMessage::new(Role::Assistant, "looked it up").from_run(id),
                )
                .unwrap();
        }

        /// A scratch conversation with one run in the state given, opened the
        /// way a delegation opens one.
        fn scratch(store: &Store, run_id: &str, status: &str) -> String {
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            store.mark_ephemeral(&c.id).unwrap();
            run_in(store, &c.id, run_id, status);
            c.id
        }

        /// Whether the sweep has taken this run's conversation out of the
        /// fleet.
        ///
        /// Asked of [`Store::scratch_lane`] because nothing else exposes
        /// `archived_at_ms` — `Conversation` does not carry the column — and
        /// because it is the answer the fleet screen itself gets, so these
        /// tests assert what a person would see rather than what a column
        /// says. `Filter::All` is the reading that includes held rows.
        fn archived(store: &Store, run_id: &str) -> bool {
            store
                .scratch_lane(crate::works::Filter::All)
                .unwrap()
                .archived
                .contains(run_id)
        }

        /// An answer waiting to be spoken into a conversation, which is what a
        /// `queued` row in `pending_deliveries` is.
        fn answer_waiting_for(store: &Store, conversation: &str) {
            let card = store
                .raise_card(NewCard {
                    conversation_id: conversation.to_string(),
                    title: "which spelling?".to_string(),
                    ..NewCard::default()
                })
                .unwrap();
            store.answer_card(card.id, None, Some("the second one")).unwrap();
        }

        /// Check 15. The ordinary ending: it finished, its answer landed, and
        /// the row gets out of the way.
        #[tokio::test]
        async fn a_finished_scratch_session_whose_answer_has_landed_is_archived() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "completed");

            let report = ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert_eq!(report.claimed, 1);
            assert_eq!(report.started, 1, "the sweep archived nothing");
            assert!(archived(&store, "r1"));
        }

        /// Check 16. Archiving before the report has been spoken would hide the
        /// row and strand the reply, which is R3b by another route.
        #[tokio::test]
        async fn a_scratch_session_with_an_answer_still_queued_stays_in_the_fleet() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = scratch(&store, "r1", "completed");
            answer_waiting_for(&store, &c);

            let report = ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert_eq!(report.claimed, 0, "it was not even a candidate");
            assert!(!archived(&store, "r1"));
        }

        /// Check 17. Half of "unless it gets stuck": a session that failed is
        /// the one somebody wants to read.
        #[tokio::test]
        async fn a_scratch_session_whose_run_failed_stays_in_the_fleet() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "failed");

            ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert!(!archived(&store, "r1"));
        }

        /// Arm a run with a heartbeat that has already been marked stalled.
        fn marked_stalled(store: &Store, run_id: &str) {
            let mut hb = Heartbeat::starting(run_id, Watching::Run, 1);
            hb.stalled_since_ms = Some(1);
            store.watch_run(&hb).unwrap();
        }

        /// The stall clause of [`Store::scratch_ready_to_archive`], on its own.
        ///
        /// The run is `completed` on purpose. A stalled run is nearly always
        /// still `running`, so a test that left the status alone would pass on
        /// the status check and never reach the stall clause at all.
        ///
        /// **This is a test of the step, not a promise about the daemon**, and
        /// the difference turned out to matter. Read
        /// `a_session_that_stalled_and_then_finished_is_archived_once_the_mark_is_retired`
        /// next: the composite tick sweeps heartbeats first, which retires the
        /// mark this test relies on, so the end-to-end answer is the opposite
        /// one. Check 18 of the spec is about neither of these — it is about a
        /// session that is *still* wedged, which is
        /// `the_daemons_tick_leaves_a_wedged_scratch_session_in_the_fleet`.
        #[tokio::test]
        async fn the_sweep_alone_leaves_a_run_carrying_a_stall_mark_in_the_fleet() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "completed");
            marked_stalled(&store, "r1");

            ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert!(!archived(&store, "r1"));
        }

        /// Check 18, as a person means it: a session that is stuck stays where
        /// you can see it, through the whole tick the daemon runs.
        ///
        /// A wedged run's status is `running`, and that is what carries the
        /// promise rather than the stall mark. The heartbeat sweep goes first
        /// and does one of two things to it — marks it and leaves it running
        /// when its process group is alive, or reaps it to `failed` when the
        /// group has gone, which is what happens here — and neither of those
        /// is `completed`, so the row stays either way. That redundancy is the
        /// point: the promise does not rest on the mark surviving the sweep,
        /// because it does not survive it.
        #[tokio::test]
        async fn the_daemons_tick_leaves_a_wedged_scratch_session_in_the_fleet() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "running");
            marked_stalled(&store, "r1");

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert!(
                !archived(&store, "r1"),
                "a wedged scratch session tidied itself away, which is the one \
                 thing B2 says it must never do"
            );
            assert_ne!(
                store.run("r1").unwrap().unwrap().status,
                "completed",
                "the status is what keeps this row visible, so it must not read completed"
            );
        }

        /// The other side of the same coin, written down because it surprised
        /// me and will surprise the next person.
        ///
        /// A run that went quiet, got marked, and then finished **is** archived
        /// by the daemon, even though `tick_scratch` on its own refuses to
        /// archive it. The heartbeat sweep runs first in `Tick::tick`, sees a
        /// run that has ended, and retires the row — taking `stalled_since_ms`
        /// with it — so by the time the scratch sweep looks there is no mark
        /// left to find.
        ///
        /// That is the right answer, which is why this pins the behaviour
        /// rather than reporting a bug. B2 keeps a session visible because it
        /// is *stuck*, and one that delivered its answer is not stuck any more.
        /// Nothing is lost either: archiving only hides the row, and `z` brings
        /// it back for as long as the retention window lasts.
        #[tokio::test]
        async fn a_session_that_stalled_and_then_finished_is_archived_once_the_mark_is_retired() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "completed");
            marked_stalled(&store, "r1");

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert!(
                store.heartbeat("r1").unwrap().is_none(),
                "the heartbeat sweep is what retires the mark, and it runs first"
            );
            assert!(archived(&store, "r1"));
        }

        /// Check 19, over every case above. Holding a row is unconditional, so
        /// it is worth asserting against the case that would otherwise archive
        /// as well as the ones that would not.
        #[tokio::test]
        async fn a_held_scratch_session_survives_every_reason_to_archive_it() {
            let store = Arc::new(Store::in_memory().unwrap());
            let finished = scratch(&store, "r1", "completed");
            let failed = scratch(&store, "r2", "failed");
            let queued = scratch(&store, "r3", "completed");
            answer_waiting_for(&store, &queued);
            for c in [&finished, &failed, &queued] {
                store.set_held(c, true).unwrap();
            }

            let report = ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert_eq!(report.claimed, 0, "a held row is never a candidate");
            for run in ["r1", "r2", "r3"] {
                assert!(!archived(&store, run), "a held row was archived: {run}");
            }
        }

        /// Check 20. Both halves in one store, because the bug worth catching
        /// is a cutoff that is off by a window rather than one that is missing.
        #[tokio::test]
        async fn the_sweep_deletes_a_session_past_the_window_and_keeps_one_inside_it() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 100 * DAY_MS;
            let old = scratch(&store, "r1", "completed");
            let recent = scratch(&store, "r2", "completed");
            // Eight days ago against a seven-day default, and one day ago.
            store.archive_conversation(&old, now - 8 * DAY_MS).unwrap();
            store.archive_conversation(&recent, now - DAY_MS).unwrap();

            ticker_over(&store).tick_scratch(now).unwrap();

            assert!(store.conversation(&old).unwrap().is_none(), "the old one is still here");
            assert!(store.conversation(&recent).unwrap().is_some(), "the recent one was deleted");
        }

        /// Check 21, and the guard on the most dangerous line in the sweep.
        ///
        /// Zero means never delete. Take the guard out and `now_ms - 0` is
        /// `now_ms`, a cutoff every archived row is older than, and the setting
        /// that was supposed to keep everything for ever deletes the lot on the
        /// next tick. This test fails the moment that guard goes.
        #[tokio::test]
        async fn a_retention_of_zero_deletes_nothing() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 100 * DAY_MS;
            let ancient = scratch(&store, "r1", "completed");
            store.archive_conversation(&ancient, now - 400 * DAY_MS).unwrap();
            store.set_setting(SCRATCH_RETENTION_DAYS_KEY, "0").unwrap();

            ticker_over(&store).tick_scratch(now).unwrap();

            assert!(
                store.conversation(&ancient).unwrap().is_some(),
                "a retention of zero means never delete, and it deleted"
            );
        }

        /// Check 19's other half. Holding a row has to outlast the retention
        /// window as well as the archive rule, or "keep this" means "keep this
        /// for a week".
        #[tokio::test]
        async fn a_held_session_is_never_deleted_however_old_it_is() {
            let store = Arc::new(Store::in_memory().unwrap());
            let now = 100 * DAY_MS;
            let kept = scratch(&store, "r1", "completed");
            store.archive_conversation(&kept, now - 90 * DAY_MS).unwrap();
            store.set_held(&kept, true).unwrap();

            ticker_over(&store).tick_scratch(now).unwrap();

            assert!(store.conversation(&kept).unwrap().is_some(), "a held row was swept");
        }

        /// Nothing that existed before the lane did is scratch, so a sweep on
        /// an ordinary database must be a no-op. This is the assertion that a
        /// person upgrading cares about most.
        #[tokio::test]
        async fn an_ordinary_conversation_is_never_touched() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            run_in(&store, &c.id, "r1", "completed");

            let report = ticker_over(&store).tick_scratch(1_000_000).unwrap();

            assert_eq!(report, TickReport::default(), "the sweep touched a real conversation");
        }


        /// The guard, and the reason it is written the way the other steps'
        /// guards are. A unit test on a function nothing calls stays green for
        /// ever, so this one goes through the tick the daemon actually runs:
        /// delete the `tick_scratch` line from `impl Tick for Ticker` and this
        /// fails, where every test above it carries on passing.
        #[tokio::test]
        async fn the_daemons_tick_archives_a_finished_scratch_session() {
            let store = Arc::new(Store::in_memory().unwrap());
            scratch(&store, "r1", "completed");

            Tick::tick(&ticker_over(&store), 1_000_000).await.unwrap();

            assert!(
                archived(&store, "r1"),
                "the composite tick never swept the scratch lane"
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

        /// **The guard on the auto-PR wiring**, which is the half that never
        /// had a caller: the instruction text and the setting both existed and
        /// nothing ever decided a session should be asked.
        ///
        /// Delete the `ask_for_pull_requests` line from `tick_pull_requests`
        /// and this fails. It asserts the one thing only that step does — the
        /// ask written down on the lease — rather than anything the sweep also
        /// touches.
        ///
        /// No network, and by construction rather than by luck: the poll is
        /// stamped as already done for this window, so the sweep returns before
        /// it reaches the leases and `gh` is never spawned. That the ask still
        /// happens is the point — it runs ahead of the interval because it does
        /// not leave the machine.
        #[tokio::test]
        async fn the_tick_asks_a_finished_session_to_open_its_pull_request() {
            let store = Arc::new(Store::in_memory().unwrap());
            store.set_auto_pr(true).unwrap();
            // Already polled in this window, so the sweep is skipped entirely.
            store.set_setting(POLLED_AT_KEY, "1000000").unwrap();
            let Some((lease, session)) = crate::prs::a_finished_session(&store) else {
                return;
            };

            ticker_over(&store)
                .tick_pull_requests(1_000_001)
                .await
                .unwrap();

            assert!(
                store.pull_request_asked_at(lease).unwrap().is_some(),
                "the tick never asked, so auto-PR is still a subsystem nothing calls"
            );
            let queued = store.pending_for(&session).unwrap();
            assert_eq!(queued.len(), 1, "and the words are queued for the session");
            assert_eq!(queued[0].kind, crate::delivery::Kind::Jod);
            assert!(
                queued[0].body.contains("create-pr"),
                "the session is asked to run the skill, not told Jod opened one: {}",
                queued[0].body
            );
        }

        /// Once, and not again on the next tick. The tick loop runs every
        /// minute and the record on the lease is the only thing between that
        /// and a session nagged sixty times an hour.
        #[tokio::test]
        async fn the_tick_does_not_ask_the_same_session_twice() {
            let store = Arc::new(Store::in_memory().unwrap());
            store.set_auto_pr(true).unwrap();
            store.set_setting(POLLED_AT_KEY, "1000000").unwrap();
            let Some((_, session)) = crate::prs::a_finished_session(&store) else {
                return;
            };
            let ticker = ticker_over(&store);

            ticker.tick_pull_requests(1_000_001).await.unwrap();
            ticker.tick_pull_requests(1_000_002).await.unwrap();
            ticker.tick_pull_requests(1_000_003).await.unwrap();

            assert_eq!(
                store.pending_for(&session).unwrap().len(),
                1,
                "three ticks, one ask — anything else is a session nagged for ever"
            );
        }

        /// Off by default and it stays off. Opening a pull request is
        /// externally visible, so a box whose owner never turned this on must
        /// never have it happen.
        #[tokio::test]
        async fn the_tick_asks_nobody_while_auto_pr_is_off() {
            let store = Arc::new(Store::in_memory().unwrap());
            store.set_setting(POLLED_AT_KEY, "1000000").unwrap();
            let Some((lease, session)) = crate::prs::a_finished_session(&store) else {
                return;
            };

            ticker_over(&store)
                .tick_pull_requests(1_000_001)
                .await
                .unwrap();

            assert!(store.pull_request_asked_at(lease).unwrap().is_none());
            assert!(store.pending_for(&session).unwrap().is_empty());
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

        // ---- the way back to the orchestrator ----------------------------

        /// A main chat that has already had a turn, and one delegated run with
        /// a channel back to it. This is the ordinary state: a delegated run
        /// exists only because the chat ran and delegated it.
        fn a_delegated_run() -> (Arc<Store>, String) {
            let store = Arc::new(Store::in_memory().unwrap());
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            store
                .record_session(&main, HarnessKind::ClaudeCode, "ses-main")
                .unwrap();
            store
                .open_return_channel("run-1", "reporter", HarnessKind::ClaudeCode)
                .unwrap();
            (store, main)
        }

        /// R3, end to end through the tick that actually runs.
        ///
        /// The measured failure was that a delegated run had no address for the
        /// chat that started it and nothing woke the chat when it tried. This
        /// asserts the whole leg: the run addresses `main`, the tick takes the
        /// message off the bus, and the orchestrator has a turn waiting that
        /// carries the answer. `tick_deliveries` is what resumes it, and it is
        /// asserted separately below, because a spawn needs a supervisor and a
        /// test box has none.
        #[tokio::test]
        async fn a_delegated_runs_answer_becomes_a_turn_of_the_main_chats() {
            let (store, main) = a_delegated_run();
            store
                .post(
                    &Post::new(Scope::Team, "run-1", "reporter", "the answer is 42").to("main"),
                )
                .unwrap();

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert_eq!(report.claimed, 1);
            assert_eq!(report.started, 1, "the answer never reached the chat");
            assert_eq!(report.failed, 0);

            assert!(
                store.mail_waiting().unwrap().is_empty(),
                "the message was queued and left on the bus as well"
            );
            let injection = store
                .plan_injection(&main, false)
                .unwrap()
                .expect("the orchestrator must have a turn waiting");
            assert!(
                injection.prompt.contains("the answer is 42"),
                "{}",
                injection.prompt
            );
            assert!(
                injection.prompt.contains("call `reply`"),
                "the chat has to be told how to answer it: {}",
                injection.prompt
            );
        }

        /// The chat is answered, never woken. Waking it the way a teammate is
        /// woken would spawn a run in a *new* conversation, and the chat Reljod
        /// reads would never show the answer at all.
        #[tokio::test]
        async fn mail_for_the_main_chat_starts_no_run_of_its_own() {
            let (store, _) = a_delegated_run();
            store
                .post(&Post::new(Scope::Team, "run-1", "reporter", "done").to("main"))
                .unwrap();

            ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert!(
                store.runs(10).unwrap().is_empty(),
                "handing mail to the chat spawned something"
            );
        }

        /// A pinned chat with no session to resume gets nothing handed to it.
        /// Delivering into a fresh context would have the orchestrator answer
        /// having forgotten what it delegated — the same judgement `wake_order`
        /// makes for a member with no session.
        #[tokio::test]
        async fn mail_for_a_main_chat_that_has_never_run_waits() {
            let store = Arc::new(Store::in_memory().unwrap());
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            store
                .open_return_channel("run-1", "reporter", HarnessKind::ClaudeCode)
                .unwrap();
            store
                .post(&Post::new(Scope::Team, "run-1", "reporter", "done").to("main"))
                .unwrap();

            let report = ticker_over(&store).tick_mail(1_000_000).await.unwrap();
            assert_eq!(report.started, 0);
            assert_eq!(report.held, 1);
            assert_eq!(
                store.team_unread("run-1", "main").unwrap().len(),
                1,
                "holding is not dropping"
            );
            assert!(store.pending_for(&main).unwrap().is_empty());
        }

        /// The chat resumed by an answer is the same orchestrator as the chat
        /// resumed by Reljod typing, so it gets the same toolbox. Handing it a
        /// smaller one would mean the answer it delegated for arrives and it can
        /// no longer arm the schedule it was about to arm.
        #[test]
        fn the_main_chat_keeps_its_own_tools_when_an_answer_resumes_it() {
            assert_eq!(
                delivery_access("c-main", Some("c-main")),
                crate::harness::ToolAccess::Orchestrate
            );
            assert_eq!(
                delivery_access("c-worker", Some("c-main")),
                crate::harness::ToolAccess::Delegate
            );
            assert_eq!(
                delivery_access("c-worker", None),
                crate::harness::ToolAccess::Delegate
            );
        }
    }
}
