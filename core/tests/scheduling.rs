//! Scheduling across process restarts, against a real database file.
//!
//! The unit tests cover the decisions; this covers the property those decisions
//! exist to protect: **a schedule survives the process that created it.** Every
//! test here closes the store and opens it again, because "it works while the
//! program is running" is exactly the guarantee a cron replacement cannot make.
//!
//! A `Store` per test process, on disk, in its own directory — an in-memory
//! store cannot be reopened, and reopening is the whole point.

use jod_core::schedule::{
    Fire, FireOutcome, Goal, GoalState, Misfire, Overlap, Schedule, ScheduleState,
};
use jod_core::store::Store;

/// A private directory for one test, removed on the way out.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("jod-sched-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn db(&self) -> std::path::PathBuf {
        self.0.join("jod.db")
    }

    /// Open the database as a fresh process would.
    fn open(&self) -> Store {
        Store::open(&self.db()).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn schedule(name: &str, cron: &str) -> Schedule {
    Schedule {
        id: format!("id-{name}"),
        name: name.into(),
        prompt: "triage the inbox".into(),
        harness: "claude_code".into(),
        cwd: "/tmp".into(),
        model: None,
        cron: cron.into(),
        timezone: "UTC".into(),
        state: ScheduleState::Armed,
        misfire: Misfire::FireOnce,
        overlap: Overlap::Skip,
        grace_ms: 300_000,
        jitter_ms: 0,
        next_fire_at_ms: None,
        last_fire_at_ms: None,
        consecutive_failures: 0,
        created_at_ms: 0,
    }
}

/// Bring everything forward so it is due, through the same public call the TUI's
/// "run now" uses — so these tests exercise the shipped path rather than a back
/// door only tests can open.
fn make_due(store: &Store) {
    // A minute in the past, not `now()`. A test that stamps "due at this exact
    // millisecond" and then claims with a reference time captured a few
    // statements earlier is racing its own clock, and fails on the ordering
    // rather than on anything the scheduler did.
    let at = now() - 60_000;
    for s in store.schedules().unwrap() {
        store.run_schedule_now(&s.name, at).unwrap();
    }
    for g in store.goals().unwrap() {
        store.run_goal_now(&g.name, at).unwrap();
    }
}

#[test]
fn a_schedule_written_by_one_process_is_found_by_the_next() {
    let scratch = Scratch::new("survives");
    {
        let store = scratch.open();
        store.add_schedule(&schedule("nightly", "0 2 * * *")).unwrap();
    } // the process that created it is gone

    let store = scratch.open();
    let found = store.schedule_named("nightly").unwrap().unwrap();
    assert_eq!(found.prompt, "triage the inbox");
    assert_eq!(found.state, ScheduleState::Armed);
    assert!(
        found.next_fire_at_ms.unwrap() > now(),
        "it must still be armed for a future instant"
    );
}

/// The property the whole claim protocol exists for, across a restart rather
/// than across threads: a schedule claimed by a process that then died must be
/// claimable again, and the dead claim must leave a trace.
#[test]
fn a_claim_held_by_a_dead_process_is_recovered_and_recorded() {
    let scratch = Scratch::new("dead-claim");
    let at = now();
    {
        let store = scratch.open();
        store.add_schedule(&schedule("orphan", "0 2 * * *")).unwrap();
        make_due(&store);
        let taken = store.claim_due_schedules("process-a", at, 1_000).unwrap();
        assert_eq!(taken.len(), 1);
        // and now process-a dies without ever releasing it
    }

    let store = scratch.open();
    let recovered = store
        .claim_due_schedules("process-b", at + 5_000, 60_000)
        .unwrap();
    assert_eq!(recovered.len(), 1, "the expired lease must be reclaimable");

    let history = store.fires("id-orphan", 10).unwrap();
    assert_eq!(history.len(), 1, "the dead claim must not vanish silently");
    assert_eq!(history[0].outcome, FireOutcome::Abandoned);
    assert!(history[0].detail.as_ref().unwrap().contains("process-a"));
}

/// A skip that leaves no row is indistinguishable from a schedule that never
/// fired at all, and the difference matters at 3am.
#[test]
fn firing_history_outlives_the_process_that_wrote_it() {
    let scratch = Scratch::new("history");
    {
        let store = scratch.open();
        store.add_schedule(&schedule("noisy", "0 2 * * *")).unwrap();
        for (due, outcome) in [
            (1_000, FireOutcome::Ran),
            (2_000, FireOutcome::SkippedOverlap),
            (3_000, FireOutcome::SpawnFailed),
        ] {
            store
                .record_fire(&Fire {
                    id: 0,
                    schedule_id: "id-noisy".into(),
                    due_at_ms: due,
                    fired_at_ms: due,
                    run_id: None,
                    outcome,
                    detail: None,
                })
                .unwrap();
        }
    }

    let store = scratch.open();
    let history = store.fires("id-noisy", 10).unwrap();
    assert_eq!(history.len(), 3);
    // Newest first, so a reader sees the most recent decision without paging.
    assert_eq!(history[0].outcome, FireOutcome::SpawnFailed);
    assert_eq!(history[2].outcome, FireOutcome::Ran);
}

/// A schedule the breaker stopped must stay stopped across a restart, or a
/// reboot would silently re-arm something that had been failing all week.
#[test]
fn a_broken_schedule_stays_broken_across_a_restart() {
    let scratch = Scratch::new("broken");
    {
        let store = scratch.open();
        store.add_schedule(&schedule("doomed", "* * * * *")).unwrap();
        for _ in 0..jod_core::schedule::BREAK_AFTER_FAILURES {
            store.release_schedule("id-doomed", now(), true).unwrap();
        }
    }

    let store = scratch.open();
    assert_eq!(
        store.schedule_named("doomed").unwrap().unwrap().state,
        ScheduleState::Broken
    );
    make_due(&store);
    assert!(
        store.claim_due_schedules("anyone", now(), 60_000).unwrap().is_empty(),
        "a broken schedule must not fire again on its own"
    );
}

/// Deleting a schedule must take its history with it. A row pointing at a
/// schedule that no longer exists is a leak that grows for ever.
#[test]
fn deleting_a_schedule_leaves_nothing_behind() {
    let scratch = Scratch::new("delete");
    {
        let store = scratch.open();
        store.add_schedule(&schedule("temp", "0 2 * * *")).unwrap();
        store
            .record_fire(&Fire {
                id: 0,
                schedule_id: "id-temp".into(),
                due_at_ms: 1,
                fired_at_ms: 1,
                run_id: None,
                outcome: FireOutcome::Ran,
                detail: None,
            })
            .unwrap();
        assert!(store.delete_schedule("temp").unwrap());
    }

    let store = scratch.open();
    assert!(store.schedule_named("temp").unwrap().is_none());
    assert!(store.fires("id-temp", 10).unwrap().is_empty());
}

// ---- goals ----

fn goal(name: &str) -> Goal {
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
        state: GoalState::Running,
        iteration: 0,
        max_iterations: None,
        budget_usd: Some(10.0),
        spent_usd: 0.0,
        stall_after: 3,
        no_progress: 0,
        next_fire_at_ms: None,
        created_at_ms: 0,
    }
}

/// A goal is the thing most likely to outlive many restarts — that is what
/// "indefinitely" means — so its accumulated state has to be durable, not
/// merely in-memory between iterations.
#[test]
fn a_goals_progress_survives_every_restart() {
    let scratch = Scratch::new("goal-progress");
    {
        let store = scratch.open();
        store.add_goal(&goal("inbox")).unwrap();
        store.advance_goal("g-inbox", now(), 1.5, true).unwrap();
    }
    {
        let store = scratch.open();
        let g = store.goal_named("inbox").unwrap().unwrap();
        assert_eq!(g.iteration, 1);
        assert!((g.spent_usd - 1.5).abs() < 1e-9);
        store.advance_goal("g-inbox", now(), 2.0, false).unwrap();
    }

    let store = scratch.open();
    let g = store.goal_named("inbox").unwrap().unwrap();
    assert_eq!(g.iteration, 2, "iterations accumulate across processes");
    assert!((g.spent_usd - 3.5).abs() < 1e-9, "so does spend");
    assert_eq!(g.no_progress, 1, "and so does the stall counter");
}

/// The counter that stops a runaway loop is worthless if a restart resets it —
/// a goal that stalls, reboots, and starts again has not stopped at all.
#[test]
fn a_stall_is_remembered_rather_than_reset_by_a_restart() {
    let scratch = Scratch::new("goal-stall");
    {
        let store = scratch.open();
        store.add_goal(&goal("stuck")).unwrap();
        for _ in 0..3 {
            store.advance_goal("g-stuck", now(), 0.0, false).unwrap();
        }
    }

    let store = scratch.open();
    assert_eq!(
        store.goal_named("stuck").unwrap().unwrap().state,
        GoalState::Stalled
    );
    make_due(&store);
    assert!(
        store.claim_due_goals("anyone", now(), 60_000).unwrap().is_empty(),
        "a stalled goal must not quietly resume after a reboot"
    );
}

#[test]
fn a_goal_that_spent_its_budget_does_not_resume_after_a_restart() {
    let scratch = Scratch::new("goal-budget");
    {
        let store = scratch.open();
        store.add_goal(&goal("pricey")).unwrap();
        store.advance_goal("g-pricey", now(), 11.0, true).unwrap();
    }

    let store = scratch.open();
    assert_eq!(
        store.goal_named("pricey").unwrap().unwrap().state,
        GoalState::Exhausted
    );
    make_due(&store);
    assert!(store.claim_due_goals("anyone", now(), 60_000).unwrap().is_empty());
}

/// Two processes on one machine, contending for one file — the shape the VPS
/// actually has when a tick overruns and the next one starts.
#[test]
fn two_processes_over_one_file_never_both_take_a_schedule() {
    let scratch = Scratch::new("contended");
    {
        let store = scratch.open();
        for i in 0..6 {
            store.add_schedule(&schedule(&format!("job{i}"), "0 2 * * *")).unwrap();
        }
        make_due(&store);
    }

    let a = scratch.open();
    let b = scratch.open();
    let at = now();
    let mut taken: Vec<String> = a
        .claim_due_schedules("a", at, 60_000)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    taken.extend(
        b.claim_due_schedules("b", at, 60_000)
            .unwrap()
            .into_iter()
            .map(|s| s.id),
    );

    let distinct: std::collections::HashSet<_> = taken.iter().collect();
    assert_eq!(taken.len(), distinct.len(), "claimed twice: {taken:?}");
    assert_eq!(distinct.len(), 6, "and every one was claimed exactly once");
}
