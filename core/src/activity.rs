//! What happened while nobody was looking, as one projection every client reads.
//!
//! Built from the three tables that *other processes* write — schedule fires,
//! webhook deliveries, and the goal loop's episodic facts. That is the whole
//! reason this projection exists: none of it happened in the process doing the
//! asking, so no in-memory copy could ever be authoritative. Runs are
//! deliberately absent — a cron fire already names the run it started, and a run
//! you started yourself is watched on the fleet screen.
//!
//! ## Why it lives in core
//!
//! It used to live twice, once for the terminal and once for HTTP, and the
//! drift both handlers predicted had already happened: the HTTP feed was
//! missing webhook deliveries entirely, so a phone could not see a rejected
//! signature or a rule that failed to start its run — exactly the silences
//! [`Item::needs_you`] exists to surface.
//!
//! So the *rule* — what counts as activity, what its line says, which outcomes
//! want a human, and what order it comes back in — is settled here, once.
//!
//! ## What is deliberately not here
//!
//! **Presentation.** A source's glyph, a relative timestamp, the colour of a
//! failed row: those are the caller's. This module hands back a `&'static str`
//! label and a millisecond epoch and stops, because a gloss is a sentence in one
//! language and a relative time is only true for the second it was rendered in.
//!
//! **Unread.** Read state is a fact about a *person*, not about an event, and
//! there is nowhere to put it yet. The terminal tracks it in its own memory and
//! the API does not serve it, so inventing a shared notion here would only make
//! two clients disagree with more confidence. A caller that wants unread keeps
//! it beside the item, not inside it.
//!
//! **The window.** How far back to look is a question about the screen asking,
//! not about what activity *is* — see [`Query`].

use std::cmp::Reverse;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::schedule::FireOutcome;
use crate::store::Store;
use crate::webhook::{Delivery, DeliveryStatus};

/// Where one line came from.
///
/// `Run` and `Memory` have no producer in [`feed`] yet and are carried because
/// the terminal's source filter cycles through all five; a filter whose options
/// appear and disappear as the store fills is worse than one with a quiet
/// option. They are listed here rather than in the caller so that the day a
/// producer lands, every client gains it at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Run,
    Cron,
    Goal,
    Hook,
    Memory,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Run => "run",
            Source::Cron => "cron",
            Source::Goal => "goal",
            Source::Hook => "hook",
            Source::Memory => "memory",
        }
    }

    /// Every source, in the order a filter should cycle them.
    pub const ALL: [Source; 5] = [
        Source::Run,
        Source::Cron,
        Source::Goal,
        Source::Hook,
        Source::Memory,
    ];
}

/// Which screen a line points at.
///
/// An activity row that names a schedule and cannot reach it is the feature
/// without the point of it, so the destination travels with the item rather than
/// being re-derived from the id by each client that fancies parsing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Jump {
    Schedules,
    Goals,
    Hooks,
}

impl Jump {
    pub fn as_str(self) -> &'static str {
        match self {
            Jump::Schedules => "schedules",
            Jump::Goals => "goals",
            Jump::Hooks => "hooks",
        }
    }
}

/// One line in the activity feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Stable across refetches, so a client can diff rather than redraw.
    pub id: String,
    pub at_ms: i64,
    pub source: Source,
    pub text: String,
    /// True for an ending nothing else in the product will report.
    pub needs_you: bool,
    /// The screen and the row within it, as `("schedules", name)`.
    pub jump_to: Option<(Jump, String)>,
}

/// What to ask the feed for.
///
/// Kept out of [`Item`] and out of the rules because it is a property of the
/// screen doing the asking, not of activity itself: the terminal draws a fixed
/// panel and wants a few fires per schedule, while an HTTP caller passes a
/// `?limit=` and expects it honoured. Both get the same rules over a different
/// window, which is the distinction worth preserving — sharing the *rules* was
/// the point, sharing the tuning never was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    /// Fires read per schedule before merging.
    pub fires_per_schedule: usize,
    /// Deliveries read across all rules before merging.
    pub deliveries: usize,
    /// Rows returned after sorting newest-first.
    pub limit: usize,
    /// Keep only lines that want a human.
    ///
    /// Applied *before* [`Query::limit`], which is the whole reason it lives
    /// here rather than in the caller: filtering a page that has already been
    /// truncated gives you whatever escalations happened to survive the cut of a
    /// mixed feed, which for a busy schedule is reliably none of them. "Show me
    /// what needs me" has to mean a page of those, not a page of everything with
    /// the rest crossed out.
    pub only_needs_you: bool,
}

/// Rows returned when a caller expresses no opinion.
pub const DEFAULT_LIMIT: usize = 200;

impl Query {
    /// A caller that asked for `limit` rows.
    ///
    /// The delivery net is opened wider than the limit because deliveries are
    /// read once across every rule rather than per rule, so a narrow read would
    /// let one chatty repository crowd every other hook out of the feed before
    /// the sort ever ran.
    pub fn with_limit(limit: usize) -> Query {
        Query {
            fires_per_schedule: limit,
            deliveries: limit.saturating_mul(2),
            limit,
            only_needs_you: false,
        }
    }

    /// Narrow to the lines that want a human.
    pub fn needing_you(mut self, only: bool) -> Query {
        self.only_needs_you = only;
        self
    }
}

impl Default for Query {
    fn default() -> Self {
        Query::with_limit(DEFAULT_LIMIT)
    }
}

/// Which fire outcomes are silence that nothing else will report.
///
/// A schedule Jod could not start, and one whose claimant died without recording
/// anything, are both invisible everywhere else in the product. Every other
/// outcome — including [`FireOutcome::MonitorQuiet`], which is a *success* — is
/// the system working and does not want a human.
pub fn fire_needs_you(outcome: FireOutcome) -> bool {
    matches!(outcome, FireOutcome::SpawnFailed | FireOutcome::Abandoned)
}

/// Which delivery outcomes want a human.
///
/// A rejected delivery is a secret that stopped verifying; a failed one is a
/// rule that matched and could not run. A no-match is the hook working — the
/// whole point of recording one is to tell it from silence — and a duplicate is
/// GitHub being at-least-once as documented.
pub fn delivery_needs_you(status: DeliveryStatus) -> bool {
    matches!(status, DeliveryStatus::Rejected | DeliveryStatus::Failed)
}

/// First line, trimmed — a feed row is one line by construction.
pub fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// What a delivery was, as one phrase: `push.opened on owner/repo`.
pub fn delivery_what(d: &Delivery) -> String {
    match (&d.action, &d.repo) {
        (Some(action), Some(repo)) => format!("{}.{action} on {repo}", d.event),
        (Some(action), None) => format!("{}.{action}", d.event),
        (None, Some(repo)) => format!("{} on {repo}", d.event),
        (None, None) => d.event.clone(),
    }
}

/// The feed, newest first.
///
/// Fallible, and deliberately so: what to do about a locked database is the
/// caller's decision and the two callers genuinely differ. The terminal swallows
/// it, because a busy store must cost one stale panel and never the session. The
/// HTTP API propagates it, because a 200 carrying a short list is
/// indistinguishable from "nothing happened" — a client cannot tell silence from
/// breakage, and over a wire that is the worse failure. Swallowing it here would
/// have quietly imposed the terminal's answer on both.
pub fn feed(store: &Store, query: Query) -> crate::Result<Vec<Item>> {
    let mut items: Vec<Item> = Vec::new();

    for s in store.schedules()? {
        for f in store.fires(&s.id, query.fires_per_schedule)? {
            items.push(Item {
                id: format!("fire/{}", f.id),
                at_ms: f.fired_at_ms,
                source: Source::Cron,
                text: format!("{} · {}", s.name, f.outcome.as_str().replace('_', " ")),
                needs_you: fire_needs_you(f.outcome),
                jump_to: Some((Jump::Schedules, s.name.clone())),
            });
        }
    }

    for g in store.goals()? {
        // In the goal's own scope. Read by subject alone, the feed would show
        // a new goal the ending of a removed one that happened to share its
        // name — and an ending is the one goal event marked as needing you.
        for f in store.facts_about_in_scope(&g.memory_scope(), &format!("goal/{}", g.name))? {
            let ended = f.predicate == "ended";
            if !ended && f.predicate != "iteration" {
                continue;
            }
            items.push(Item {
                id: format!("goal/{}/{}", g.name, f.id),
                at_ms: f.recorded_at_ms,
                source: Source::Goal,
                text: format!("{} · {}", g.name, one_line(&f.object)),
                // A goal ending is the one goal event a person has to see: it is
                // the loop saying it will not run again.
                needs_you: ended,
                jump_to: Some((Jump::Goals, g.name.clone())),
            });
        }
    }

    // A delivery names its rule by id; the hooks screen's rows are keyed by
    // name. Without the translation a jump would reach the screen and select
    // nothing — a jump that looks like it worked and did not.
    let rule_names: HashMap<String, String> = store
        .webhook_rules()?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    for d in store.deliveries(query.deliveries)? {
        items.push(Item {
            id: format!("delivery/{}", d.delivery_id),
            at_ms: d.received_at_ms,
            source: Source::Hook,
            text: format!(
                "{} · {}",
                delivery_what(&d),
                d.status.as_str().replace('_', " ")
            ),
            needs_you: delivery_needs_you(d.status),
            jump_to: d
                .rule_id
                .as_deref()
                .and_then(|id| rule_names.get(id))
                .map(|name| (Jump::Hooks, name.clone())),
        });
    }

    // Newest first, so the truncate below keeps the most recent rather than the
    // oldest — the order and the limit are one decision, not two. The filter
    // runs between them for the reason given on [`Query::only_needs_you`].
    items.sort_by_key(|i| Reverse(i.at_ms));
    if query.only_needs_you {
        items.retain(|i| i.needs_you);
    }
    items.truncate(query.limit);
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{Fire, Misfire, Overlap, Schedule, ScheduleState};
    use crate::store::NewFact;
    use crate::webhook::{Conditions, Rule};

    const AT: i64 = 1_800_000_000_000;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn schedule(name: &str) -> Schedule {
        Schedule {
            id: format!("sch-{name}"),
            name: name.to_string(),
            prompt: "sweep the open PRs".into(),
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 2 * * *".into(),
            timezone: "UTC".into(),
            state: ScheduleState::Armed,
            misfire: Misfire::default(),
            overlap: Overlap::default(),
            grace_ms: 60_000,
            jitter_ms: 0,
            next_fire_at_ms: None,
            last_fire_at_ms: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        }
    }

    fn fire(schedule_id: &str, at_ms: i64, outcome: FireOutcome) -> Fire {
        Fire {
            id: 0,
            schedule_id: schedule_id.to_string(),
            due_at_ms: at_ms,
            fired_at_ms: at_ms,
            run_id: None,
            outcome,
            detail: None,
        }
    }

    fn rule(name: &str) -> Rule {
        Rule {
            id: format!("wr-{name}"),
            name: name.to_string(),
            source: "github".into(),
            repo: "Reljod/Jod".into(),
            event: "pull_request".into(),
            action: None,
            conditions: Conditions::default(),
            prompt: "Look at {{title}}".into(),
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            enabled: true,
            created_at_ms: 0,
        }
    }

    fn stored_delivery(r: &Rule, at_ms: i64, status: DeliveryStatus) -> Delivery {
        Delivery {
            id: 0,
            delivery_id: format!("d-{at_ms}"),
            source: "github".into(),
            event: "pull_request".into(),
            action: Some("opened".into()),
            repo: Some("Reljod/Jod".into()),
            rule_id: Some(r.id.clone()),
            run_id: None,
            status,
            detail: None,
            received_at_ms: at_ms,
        }
    }

    /// The merge this module exists to do once. Three tables, three sources, one
    /// ordering — and the webhook row is the one the HTTP API used to be missing.
    #[test]
    fn the_feed_merges_all_three_tables_newest_first() {
        let s = store();

        let sch = schedule("shepherd");
        s.add_schedule(&sch).unwrap();
        s.record_fire(&fire(&sch.id, AT - 3_000, FireOutcome::Ran))
            .unwrap();

        let r = rule("pr-review");
        s.add_webhook_rule(&r).unwrap();
        s.record_delivery(&stored_delivery(&r, AT - 1_000, DeliveryStatus::Accepted))
            .unwrap();

        let items = feed(&s, Query::default()).unwrap();

        let sources: Vec<Source> = items.iter().map(|i| i.source).collect();
        assert!(sources.contains(&Source::Cron), "no cron row: {items:?}");
        assert!(sources.contains(&Source::Hook), "no webhook row: {items:?}");

        // Newest first, so the truncate keeps the most recent.
        let times: Vec<i64> = items.iter().map(|i| i.at_ms).collect();
        let mut sorted = times.clone();
        sorted.sort_by_key(|t| Reverse(*t));
        assert_eq!(times, sorted, "the feed is not newest-first: {items:?}");
    }

    /// A delivery stores its rule by *id*; the hooks screen is keyed by *name*.
    /// Core does the translation so that no client has to, and a jump that
    /// carries an id reaches the screen and selects nothing.
    #[test]
    fn a_webhook_row_jumps_by_rule_name_not_rule_id() {
        let s = store();
        let r = rule("pr-review");
        s.add_webhook_rule(&r).unwrap();
        s.record_delivery(&stored_delivery(&r, AT, DeliveryStatus::Rejected))
            .unwrap();

        let items = feed(&s, Query::default()).unwrap();
        let hook = items.iter().find(|i| i.source == Source::Hook).unwrap();

        assert_eq!(hook.jump_to, Some((Jump::Hooks, "pr-review".to_string())));
        assert!(hook.needs_you, "a rejected signature must ask for a human");
    }

    /// The filter runs before the page is cut. With a limit of one and newer
    /// routine traffic in front of it, filtering afterwards answers "nothing
    /// needs you" while an escalation sits one row behind.
    #[test]
    fn a_narrow_page_of_escalations_survives_newer_routine_traffic() {
        let s = store();
        let r = rule("pr-review");
        s.add_webhook_rule(&r).unwrap();
        s.record_delivery(&stored_delivery(&r, AT, DeliveryStatus::Rejected))
            .unwrap();
        s.record_delivery(&stored_delivery(&r, AT + 5_000, DeliveryStatus::Accepted))
            .unwrap();

        let narrow = Query::with_limit(1).needing_you(true);
        let items = feed(&s, narrow).unwrap();

        assert_eq!(items.len(), 1, "the escalation was cut before the filter ran");
        assert!(items[0].needs_you);
    }

    /// A goal ending is the one goal event a person has to see; an ordinary
    /// iteration is progress and must stay quiet.
    #[test]
    fn a_goal_ending_asks_for_a_human_and_an_iteration_does_not() {
        let s = store();
        let g = crate::schedule::Goal {
            id: "goal-ship".into(),
            name: "ship-it".into(),
            objective: "get the suite green".into(),
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
        };
        s.add_goal(&g).unwrap();
        // In the goal's own scope, which is where `Ticker::tick_goals` writes
        // every one of these and where the feed now reads them.
        s.remember(
            NewFact::new("goal/ship-it", "iteration", "first pass").in_scope(&g.memory_scope()),
        )
        .unwrap();
        s.remember(NewFact::new("goal/ship-it", "ended", "satisfied").in_scope(&g.memory_scope()))
            .unwrap();

        let items = feed(&s, Query::default()).unwrap();
        let goals: Vec<&Item> = items.iter().filter(|i| i.source == Source::Goal).collect();
        assert_eq!(goals.len(), 2, "both goal facts should appear: {items:?}");
        assert_eq!(goals.iter().filter(|i| i.needs_you).count(), 1);
    }

    /// The two outcomes nothing else reports. Pinned rather than assumed,
    /// because this predicate is the entire reason the screen exists.
    #[test]
    fn only_unstartable_and_abandoned_fires_want_a_human() {
        assert!(fire_needs_you(FireOutcome::SpawnFailed));
        assert!(fire_needs_you(FireOutcome::Abandoned));

        for quiet in [
            FireOutcome::Ran,
            FireOutcome::SkippedOverlap,
            FireOutcome::SkippedMisfire,
            FireOutcome::Replaced,
            FireOutcome::MonitorQuiet,
            FireOutcome::Unknown,
        ] {
            assert!(!fire_needs_you(quiet), "{quiet:?} should not want a human");
        }
    }

    /// A no-match is the hook working. Getting this backwards would fill the
    /// feed with every push to every watched repository.
    #[test]
    fn only_rejected_and_failed_deliveries_want_a_human() {
        assert!(delivery_needs_you(DeliveryStatus::Rejected));
        assert!(delivery_needs_you(DeliveryStatus::Failed));

        for quiet in [
            DeliveryStatus::Accepted,
            DeliveryStatus::NoMatch,
            DeliveryStatus::Duplicate,
        ] {
            assert!(
                !delivery_needs_you(quiet),
                "{quiet:?} should not want a human"
            );
        }
    }

    #[test]
    fn a_feed_row_is_one_line_however_many_the_fact_had() {
        assert_eq!(one_line("first\nsecond\nthird"), "first");
        assert_eq!(one_line("  padded  \nrest"), "padded");
        assert_eq!(one_line(""), "");
    }

    /// The jump target's wire form is load-bearing: clients match on these
    /// exact strings to pick a screen.
    #[test]
    fn a_jump_names_the_screen_a_client_can_route_to() {
        assert_eq!(Jump::Schedules.as_str(), "schedules");
        assert_eq!(Jump::Goals.as_str(), "goals");
        assert_eq!(Jump::Hooks.as_str(), "hooks");
    }

    /// Deliveries are read once across every rule, so the net has to be wider
    /// than the page or one chatty repository crowds the rest out before the
    /// sort runs.
    #[test]
    fn a_requested_limit_opens_the_delivery_net_wider_than_the_page() {
        let w = Query::with_limit(50);
        assert_eq!(w.limit, 50);
        assert_eq!(w.fires_per_schedule, 50);
        assert!(w.deliveries > w.limit);
    }

    /// `usize::MAX * 2` must not wrap a limit into a window of nothing.
    #[test]
    fn an_absurd_limit_saturates_rather_than_wrapping_to_zero() {
        let w = Query::with_limit(usize::MAX);
        assert_eq!(w.deliveries, usize::MAX);
    }

    /// Source labels are the API's wire vocabulary; `hook` is the one this
    /// refactor added and the one a client will not have seen before.
    #[test]
    fn every_source_has_a_wire_label() {
        assert_eq!(Source::Cron.as_str(), "cron");
        assert_eq!(Source::Goal.as_str(), "goal");
        assert_eq!(Source::Hook.as_str(), "hook");
        assert_eq!(Source::ALL.len(), 5);
    }

    #[test]
    fn a_delivery_phrase_degrades_as_fields_go_missing() {
        let mut d = Delivery::new("d1", "pull_request");
        d.action = Some("opened".into());
        d.repo = Some("o/r".into());
        assert_eq!(delivery_what(&d), "pull_request.opened on o/r");

        d.repo = None;
        assert_eq!(delivery_what(&d), "pull_request.opened");

        d.action = None;
        assert_eq!(delivery_what(&d), "pull_request");

        d.repo = Some("o/r".into());
        assert_eq!(delivery_what(&d), "pull_request on o/r");
    }
}
