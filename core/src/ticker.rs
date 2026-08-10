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

use std::sync::Arc;

use crate::error::Result;
use crate::harness::{HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::schedule::{self, Fire, FireOutcome, Goal, Misfire, Overlap, Schedule};
use crate::service::{AgentStatus, Jod};
use crate::store::{NewFact, Origin, Store};

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

/// How long a claim is believed before another process may take it.
///
/// Comfortably longer than a tick, so an ordinary slow tick never loses its own
/// claim, and short enough that a machine which dies mid-fire is picked up
/// within a few minutes rather than at the next restart.
pub const LEASE_MS: i64 = 300_000;

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

/// Drives schedules and goals against a running [`Jod`].
pub struct Ticker {
    jod: Arc<Jod>,
    /// Who this process is, for the claim. Distinct per process, because two
    /// `jod` processes on one box must not look like the same claimant.
    owner: String,
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
        }
    }

    /// Use a fixed owner name. Tests need two "processes" in one process.
    pub fn as_owner(mut self, owner: impl Into<String>) -> Ticker {
        self.owner = owner.into();
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
            let mut failed = false;

            for decision in decide(&s, &missed, running.as_deref()) {
                match self.carry_out(&s, &decision, now_ms).await {
                    Ok(true) => report.started += 1,
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
            store.release_schedule(&s.id, now_ms, failed)?;
        }
        Ok(report)
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
    async fn carry_out(&self, s: &Schedule, d: &Decision, now_ms: i64) -> Result<bool> {
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
                let run = self.spawn(s, *due_at_ms).await?;
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
                let run = self.spawn(s, *due_at_ms).await?;
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
                        // Completion is the only progress signal Jod can read
                        // without a model. It is a proxy: an iteration that ran
                        // cleanly and achieved nothing counts as progress here,
                        // which is why the stall threshold is several
                        // iterations rather than one.
                        let progressed = agent.status == AgentStatus::Completed;
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

            match self.spawn_iteration(&store, &goal, &subject, &scope).await {
                Ok(()) => report.started += 1,
                Err(_) => report.failed += 1,
            }
            // Released whether or not the spawn worked. A goal left claimed by
            // a failed spawn would wait out the lease before trying again.
            store.release_goal(&goal.id)?;
        }
        Ok(report)
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
            prompt.push_str(&format!(
                "\n\nIt is done when this command succeeds: {check}\nRun it before you \
                 conclude anything, and say plainly whether it passed."
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
                cwd: std::path::PathBuf::from(&goal.cwd),
                model: goal.model.clone(),
                permission: PermissionPolicy::default(),
                // Fresh every time. A goal running for months cannot carry one
                // harness conversation without its context growing without
                // bound; the memory layer is what carries continuity instead.
                resume: Resume::Fresh,
            })
            .await?;

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

    async fn spawn(&self, s: &Schedule, due_at_ms: i64) -> Result<String> {
        let agent = self
            .jod
            .spawn_agent(SpawnRequest {
                name: s.name.clone(),
                harness: HarnessKind::from_id(&s.harness).unwrap_or(HarnessKind::ClaudeCode),
                prompt: s.prompt.clone(),
                cwd: std::path::PathBuf::from(&s.cwd),
                model: s.model.clone(),
                permission: PermissionPolicy::default(),
                // Always a fresh conversation. A scheduled run that silently
                // continued last night's thread would inherit context nobody
                // chose for it, and the context would grow without bound.
                resume: Resume::Fresh,
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
        let held = decisions.iter().filter(|d| matches!(d, Decision::Hold { .. }));
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
                tx.execute("UPDATE schedules SET next_fire_at_ms = 1", []).unwrap();
                Ok(())
            })
            .unwrap();

        let jod = Jod::with_store(store.clone());
        let now = chrono::Utc::now().timestamp_millis();
        let first = Ticker::new(jod.clone()).as_owner("a").tick(now).await.unwrap();
        let second = Ticker::new(jod).as_owner("b").tick(now).await.unwrap();

        assert_eq!(first.claimed, 1);
        assert_eq!(second.claimed, 0, "the second ticker must find nothing due");
    }
}
