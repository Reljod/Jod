//! What the workspaces show, as plain data.
//!
//! These are *view models*, not the store's types. The TUI renders a schedule
//! as a name, a human gloss, two timestamps and a seven-cell outcome strip; the
//! store holds a cron expression and a table of fires. Keeping the seam here
//! means the screens can be built and tested against fixtures, and the loaders
//! below can be filled in without any screen changing shape.
//!
//! Every loader reads the store and swallows its own errors: a locked database
//! must cost one stale frame, never the session. Where the store cannot answer
//! yet the field is left empty and the gap is named in a comment, because a
//! screen with no rows says so in words rather than showing an empty box — an
//! honest empty state beats an invented one.

// A few states below are only reachable from fixtures the screens are tested
// against, so the compiler cannot see them being built. Removing them to
// silence that would mean deleting the vocabulary the screens are written
// against and adding it back when the store grows the column that fills it.
#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use jod_core::cards::{Card, Query};
use jod_core::rank;
use jod_core::roots::Root;
use jod_core::schedule::{
    Fire, FireOutcome, Goal, GoalState as StoredGoalState, Schedule,
    ScheduleState as StoredScheduleState,
};
use jod_core::store::{Edge, Fact, Origin, Store, StoredRun};
use jod_core::team::TeamTask;
use jod_core::webhook::{Delivery as StoredDelivery, DeliveryStatus, Rule};
use jod_core::Jod;

use super::app::short_duration;
use super::workspace::Workspace;

/// How a run, a fire or a delivery ended.
///
/// Every variant carries a glyph as well as a colour, because `NO_COLOR` users,
/// 8-colour terminals and colour-blind users all have to read the same table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    /// Ran but did nothing, or was skipped by an overlap policy.
    Idle,
    /// No run at all in that slot.
    Missing,
}

impl Outcome {
    /// One cell of a run-history strip. Sparkline blocks for the good cases and
    /// a cross for failure, so a strip reads as "healthy / flaky / dead" without
    /// any colour at all.
    pub fn cell(self) -> &'static str {
        match self {
            Outcome::Ok => "▇",
            Outcome::Failed => "✗",
            Outcome::Idle => "▃",
            Outcome::Missing => "▁",
        }
    }

    pub fn mark(self) -> &'static str {
        match self {
            Outcome::Ok => "✓",
            Outcome::Failed => "✗",
            Outcome::Idle => "○",
            Outcome::Missing => "—",
        }
    }
}

// ---- memory ------------------------------------------------------------

/// What kind of thing a memory node is. The three-letter tag and the glyph are
/// both shown, so neither has to carry the meaning alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    Belief,
    Entity,
    Episode,
    Procedure,
    Fact,
}

impl MemoryKind {
    pub fn tag(self) -> &'static str {
        match self {
            MemoryKind::Belief => "blf",
            MemoryKind::Entity => "ent",
            MemoryKind::Episode => "epi",
            MemoryKind::Procedure => "pro",
            MemoryKind::Fact => "fact",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            MemoryKind::Belief => "◆",
            MemoryKind::Entity => "●",
            MemoryKind::Episode => "▤",
            MemoryKind::Procedure => "▦",
            MemoryKind::Fact => "◇",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MemoryKind::Belief => "belief",
            MemoryKind::Entity => "entity",
            MemoryKind::Episode => "episode",
            MemoryKind::Procedure => "procedure",
            MemoryKind::Fact => "fact",
        }
    }

    /// The cycle `t` walks: every kind, then back to "all".
    pub const ALL: [MemoryKind; 5] = [
        MemoryKind::Belief,
        MemoryKind::Entity,
        MemoryKind::Episode,
        MemoryKind::Procedure,
        MemoryKind::Fact,
    ];
}

/// One end of an edge, named from the point of view of the node you are on.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEdge {
    /// `supports`, `contradicts`, `refines`, `derived-from`.
    pub kind: String,
    /// The id of the node at the other end.
    pub other: String,
    pub other_name: String,
    pub other_kind: MemoryKind,
    /// True for an edge in an unresolved contradiction, which earns a `⚠`.
    pub warn: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryNode {
    pub id: String,
    pub name: String,
    pub kind: MemoryKind,
    pub confidence: f64,
    /// How many edges the node has in total, which is the cheapest honest
    /// answer to "is this memory load-bearing?".
    pub degree: usize,
    pub age_ms: i64,
    pub seen: usize,
    pub body: String,
    /// In an unresolved contradiction — the `!` in the list and the `⚠` on the
    /// edge that causes it.
    pub contradicted: bool,
    pub in_edges: Vec<MemoryEdge>,
    pub out_edges: Vec<MemoryEdge>,
    /// Where it came from, one line each.
    pub provenance: Vec<String>,
}

// ---- schedules ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleState {
    Armed,
    /// `‖`, not `⏸` — U+23F8 is East-Asian *Wide* and shears every column to
    /// its right.
    Paused,
    Failing,
}

impl ScheduleState {
    pub fn glyph(self) -> &'static str {
        match self {
            ScheduleState::Armed => "●",
            ScheduleState::Paused => "‖",
            ScheduleState::Failing => "✗",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PastRun {
    pub at_ms: i64,
    pub outcome: Outcome,
    pub duration_ms: i64,
    pub cost_usd: f64,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRow {
    pub name: String,
    /// `02:00 every day` — a cron expression in a table is a puzzle, so the
    /// gloss goes in the table and the expression in the detail block.
    pub gloss: String,
    pub cron: String,
    pub timezone: String,
    pub next_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub state: ScheduleState,
    /// The last seven runs, oldest first.
    pub history: Vec<Outcome>,
    pub prompt: String,
    pub runs_as: String,
    pub policy: String,
    pub recent: Vec<PastRun>,
}

// ---- goals -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalState {
    Running,
    Satisfied,
    Waiting,
    Blocked,
    Paused,
}

impl GoalState {
    pub fn label(self) -> &'static str {
        match self {
            GoalState::Running => "running",
            GoalState::Satisfied => "satisfied",
            GoalState::Waiting => "waiting",
            GoalState::Blocked => "blocked",
            GoalState::Paused => "paused",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            GoalState::Running => "◎",
            GoalState::Satisfied => "✓",
            GoalState::Waiting => "○",
            GoalState::Blocked => "⚠",
            GoalState::Paused => "‖",
        }
    }
}

/// One line of a goal's done-when checklist. The checklist is the denominator
/// that makes a real percent-done possible, which is why goals get a progress
/// bar and nothing else does.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub done: bool,
    pub text: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Iteration {
    pub n: usize,
    pub at_ms: i64,
    pub note: String,
    pub duration_ms: i64,
    pub cost_usd: f64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalRow {
    pub name: String,
    pub cadence: String,
    pub last_ms: Option<i64>,
    pub next_ms: Option<i64>,
    pub state: GoalState,
    pub iteration: usize,
    pub objective: String,
    pub checks: Vec<Check>,
    pub stop_if: String,
    pub spent_usd: f64,
    pub budget_usd: f64,
    pub iterations: Vec<Iteration>,
    /// A looping objective that quietly needs you and never says so is worse
    /// than no goal at all, so this is on the screen rather than in a log.
    pub escalation: Option<String>,
}

impl GoalRow {
    /// Percent done, from the checklist. Zero checks means zero percent rather
    /// than a divide by nothing.
    pub fn percent(&self) -> u16 {
        if self.checks.is_empty() {
            return 0;
        }
        let done = self.checks.iter().filter(|c| c.done).count();
        ((done * 100) / self.checks.len()) as u16
    }
}

// ---- webhooks ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    Armed,
    Idle,
    Failing,
}

impl HookState {
    pub fn glyph(self) -> &'static str {
        match self {
            HookState::Armed => "●",
            HookState::Idle => "○",
            HookState::Failing => "✗",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub at_ms: i64,
    pub id: String,
    pub what: String,
    pub accepted: bool,
    /// The run this delivery started, which is what joins this screen to the
    /// fleet.
    pub run: Option<String>,
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookRow {
    pub name: String,
    pub repo: String,
    pub event: String,
    /// The agent a delivery starts.
    pub runs: String,
    pub deliveries_24h: usize,
    pub last_ms: Option<i64>,
    pub last_outcome: Outcome,
    pub state: HookState,
    pub endpoint: String,
    /// `✓ verified 2m ago`, or why not.
    pub secret: String,
    pub match_rule: String,
    pub runs_as: String,
    pub prompt: String,
    pub policy: String,
    pub created: String,
    pub total: usize,
    pub deliveries: Vec<Delivery>,
}

// ---- tasks -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Claimed,
    Open,
    Blocked,
    Done,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Running => "running",
            TaskState::Claimed => "claimed",
            TaskState::Open => "open",
            TaskState::Blocked => "blocked",
            TaskState::Done => "done",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            TaskState::Running => "●",
            TaskState::Claimed => "◐",
            TaskState::Open => "○",
            TaskState::Blocked => "⚠",
            TaskState::Done => "✓",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub owner: Option<String>,
    pub state: TaskState,
    /// The run doing it, if one is. This is what closes the loop between the
    /// board and the fleet.
    pub run: Option<String>,
    pub age_ms: i64,
    pub what: String,
    /// The runnable check. Without one, "looks done" is the only stop signal.
    pub check: String,
    pub blocked_by: Vec<String>,
    pub blocks: Vec<String>,
    pub spec: Option<String>,
    pub history: Vec<String>,
}

// ---- activity ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Run,
    Cron,
    Goal,
    Hook,
    Memory,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Run => "run",
            Source::Cron => "cron",
            Source::Goal => "goal",
            Source::Hook => "hook",
            Source::Memory => "memory",
        }
    }

    /// `◷`, not `⏰` — U+23F0 is East-Asian *Wide* and would shear the column
    /// to its right on every row.
    pub fn glyph(self) -> &'static str {
        match self {
            Source::Run => "✓",
            Source::Cron => "◷",
            Source::Goal => "◎",
            Source::Hook => "⚑",
            Source::Memory => "◆",
        }
    }

    /// What `f` cycles through, "all" being `None`.
    pub const ALL: [Source; 5] = [
        Source::Run,
        Source::Cron,
        Source::Goal,
        Source::Hook,
        Source::Memory,
    ];
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityItem {
    pub id: String,
    pub at_ms: i64,
    pub source: Source,
    pub text: String,
    pub unread: bool,
    /// True for an ending that needs a human — a goal escalation, a third
    /// consecutive schedule failure, a webhook whose secret stopped verifying.
    pub needs_you: bool,
    /// Where `⏎` jumps to: the workspace and the row within it.
    pub jump_to: Option<(Workspace, String)>,
}

// ---- the cron gloss ----------------------------------------------------

/// A cron expression as a phrase a person can check at a glance.
///
/// This belongs in `jod_core::schedule`, beside `next_fire` — the two are
/// halves of the same question, and the CLI's `jod schedules` table wants it as
/// much as this screen does. It lives here only because that module is being
/// changed by another track; move it when that lands.
///
/// Deliberately narrow. It glosses the shapes people actually write and hands
/// back the raw expression for everything else, because a *wrong* gloss is
/// worse than none: the expression is what decides when an agent runs
/// unattended, and a table that quietly mistranslates it is a table you cannot
/// use to check anything.
pub fn gloss(cron: &str) -> String {
    describe(cron).unwrap_or_else(|| cron.to_string())
}

/// The gloss, or `None` for anything this does not read with certainty.
fn describe(cron: &str) -> Option<String> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    // Five fields, and only five. croner also accepts a six-field form with
    // seconds in front, and reading that one as this one shifts every field by
    // a place — `0 0 2 * * *` would gloss as `00:02 every day`.
    let [minute, hour, day_of_month, month, day_of_week] = fields[..] else {
        return None;
    };
    // A day-of-month or a month restriction changes which days fire, and this
    // says nothing about days beyond the weekday field. Rather than gloss half
    // of it, hand the whole expression back.
    if day_of_month != "*" || month != "*" {
        return None;
    }

    // A clock time reads as `09:00 Mon–Fri`; an interval reads as `every 15
    // minutes, Mon–Fri`. Same two parts, different joins.
    let (when, is_clock) = match (minute, hour) {
        ("*", "*") => ("every minute".to_string(), false),
        (m, "*") => match step(m) {
            Some(n) => (format!("every {n} minutes"), false),
            None => (format!("every hour at :{:02}", number(m)?), false),
        },
        ("0", h) if step(h).is_some() => (format!("every {} hours", step(h)?), false),
        (m, h) => (format!("{:02}:{:02}", number(h)?, number(m)?), true),
    };

    let days = weekdays(day_of_week)?;
    Some(match (is_clock, days.as_str()) {
        (true, "every day") => format!("{when} every day"),
        (true, _) => format!("{when} {days}"),
        (false, "every day") => when,
        (false, _) => format!("{when}, {days}"),
    })
}

/// The `N` of a `*/N` step, and nothing else. `*/1` is not a step worth
/// spelling out — "every 1 minutes" is worse English than the expression.
fn step(field: &str) -> Option<u32> {
    let n = field.strip_prefix("*/")?.parse().ok()?;
    (n > 1).then_some(n)
}

fn number(field: &str) -> Option<u32> {
    field.parse().ok()
}

/// The day-of-week field as names. `None` for anything it cannot name exactly.
fn weekdays(field: &str) -> Option<String> {
    if field == "*" || field == "?" {
        return Some("every day".to_string());
    }
    if let Some((from, to)) = field.split_once('-') {
        return Some(format!("{}–{}", weekday(from)?, weekday(to)?));
    }
    let named: Option<Vec<&str>> = field.split(',').map(weekday).collect();
    Some(named?.join(", "))
}

/// One day, by number or by name. Both 0 and 7 are Sunday, which is what every
/// cron implementation accepts and what people write.
fn weekday(field: &str) -> Option<&'static str> {
    match field.trim().to_ascii_uppercase().as_str() {
        "0" | "7" | "SUN" => Some("Sun"),
        "1" | "MON" => Some("Mon"),
        "2" | "TUE" => Some("Tue"),
        "3" | "WED" => Some("Wed"),
        "4" | "THU" => Some("Thu"),
        "5" | "FRI" => Some("Fri"),
        "6" | "SAT" => Some("Sat"),
        _ => None,
    }
}

// ---- loaders -----------------------------------------------------------
//
// Each of these is the seam between the TUI and the store. They run on the
// tick, not on the render path, so a query here costs a frame's latency at
// worst — but they run about once a second, so each one is bounded by a
// constant rather than by how much history the database holds.

/// The last seven outcomes, which is what the strip on the schedules screen is.
const HISTORY: usize = 7;

/// How many past fires, deliveries or iterations a detail pane offers. The pane
/// shows five; the extra rows are what a longer terminal fills with.
const RECENT: usize = 8;

/// How many deliveries one pass reads. Deliveries are stored for every rule in
/// one table, so this is the window every rule's counts are computed over —
/// `total` on a hook row means "in this window", not "ever".
const DELIVERY_WINDOW: usize = 400;

/// The most events the activity feed holds. It is a feed, not an archive: past
/// this, older entries are the schedules and hooks screens' job.
const ACTIVITY_LIMIT: usize = 200;

/// How many memory nodes one pass lists, and how many subjects it will read
/// facts for. `memory_nodes` returns the most-connected first, so the cap drops
/// the leaves rather than an arbitrary slice; the second bounds the nodes whose
/// *asserter* is itself below the cap.
const MEMORY_NODES: usize = 200;
const MEMORY_SUBJECTS: usize = 400;

/// The most edges any one node contributes to the detail pane and the local
/// graph. A hub has more than a screen can hold, and the graph view already
/// says how many of them it is showing.
const MEMORY_EDGES: usize = 64;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Every memory node, most-connected first, with its edges already joined.
///
/// The list, and every node's degree, come from `Store::memory_nodes` — one
/// query over `entities` counting `relations` in both directions. That is the
/// authority on what Jod knows: an earlier draft of this walked outwards from
/// the subjects the TUI could name, which meant anything `/remember` wrote
/// about a subject nothing else mentioned was invisible — the common case for
/// a person's own notes, and precisely the wrong thing for a memory browser to
/// hide.
///
/// The graph says what exists and how it is connected; it cannot say *why* any
/// of it is believed. `entities` has no origin and no source, and its `kind`
/// column reads `thing` for every row it interns. So the facts are read too,
/// and they supply the three things the screen shows that structure alone
/// cannot: what kind of memory a node is, how far it is to be trusted, and
/// where it came from.
pub fn memory(jod: &Arc<Jod>) -> Vec<MemoryNode> {
    match jod.store() {
        Some(store) => memory_from(store),
        None => Vec::new(),
    }
}

fn memory_from(store: &Store) -> Vec<MemoryNode> {
    let listed = store.memory_nodes(None, MEMORY_NODES).unwrap_or_default();
    let edges: HashMap<i64, Vec<Edge>> = listed
        .iter()
        .map(|n| (n.id, store.edges_of(n.id, MEMORY_EDGES).unwrap_or_default()))
        .collect();

    // Facts for every listed node, and for the subjects that assert one — a
    // node that is only ever an object has no facts of its own, and the fact
    // that named it belongs to whatever points at it.
    //
    // One indexed lookup per name, which is the cost worth naming: `facts` is
    // indexed on `(scope, subject)`, so `facts_about` — which filters on
    // `subject` alone — cannot use it and scans. At this cap that is fine and
    // on a large store it will not be. The fix is a bulk `facts_for(&[names])`
    // beside `memory_nodes`, or `memory_nodes` returning the newest fact's
    // origin as a column, at which point this loader is two queries total.
    let mut wanted: Vec<String> = listed.iter().map(|n| n.name.clone()).collect();
    for edge in edges.values().flatten().filter(|e| !e.outgoing) {
        wanted.push(edge.other.clone());
    }
    let mut asserted: HashMap<String, Vec<Fact>> = HashMap::new();
    for name in wanted.into_iter().take(MEMORY_SUBJECTS) {
        asserted
            .entry(name)
            .or_insert_with_key(|n| store.facts_about(n).unwrap_or_default());
    }

    // The far end of an edge has to be named with its kind, so every listed
    // node's kind is settled before any edge is built.
    let kinds: HashMap<i64, MemoryKind> = listed
        .iter()
        .map(|n| (n.id, kind_of_node(&asserted, &n.name)))
        .collect();
    let kind_at = |id: i64| kinds.get(&id).copied().unwrap_or(MemoryKind::Entity);

    let now = now_ms();
    listed
        .iter()
        .map(|n| {
            let mine = edges.get(&n.id).map(Vec::as_slice).unwrap_or_default();
            // What this node is, said by whichever fact says the most about
            // it: its own newest, or — for a node only ever spoken about — the
            // newest fact that names it.
            let describing = asserted
                .get(&n.name)
                .and_then(|facts| facts.first())
                .or_else(|| asserting(&asserted, &n.name, mine));
            let (in_edges, out_edges): (Vec<&Edge>, Vec<&Edge>) =
                mine.iter().partition(|e| !e.outgoing);

            MemoryNode {
                id: n.id.to_string(),
                name: n.name.clone(),
                kind: kind_at(n.id),
                // Facts carry no confidence column, and inventing a gradient
                // would put a number on the screen nothing could justify.
                // Origin is the trust signal the store does keep — it is what
                // decides whether a fact may answer a recall at all — so the
                // column shows that, the same way every time.
                confidence: describing.map(|f| trust(f.origin)).unwrap_or(0.0),
                // Counted in the query, in both directions, over live edges
                // only. The cheapest honest answer to whether this memory is
                // load-bearing or was written once and never used again.
                degree: n.degree.max(0) as usize,
                age_ms: (now - n.last_seen_ms).max(0),
                // One edge per fact, so how often a thing has been named and
                // how connected it is are the same number until the store
                // counts restatements separately.
                seen: n.degree.max(0) as usize,
                body: describing
                    .map(|f| format!("{} {} {}", f.subject, f.predicate, f.object))
                    .unwrap_or_else(|| n.name.clone()),
                // Now answerable from the graph rather than guessed at: a node
                // with a `contradicts` edge is in a contradiction, whichever
                // end of it this is.
                contradicted: mine.iter().any(|e| e.predicate == "contradicts"),
                in_edges: mine
                    .iter()
                    .filter(|e| !e.outgoing)
                    .map(|e| edge(e, kind_at))
                    .collect(),
                out_edges: mine
                    .iter()
                    .filter(|e| e.outgoing)
                    .map(|e| edge(e, kind_at))
                    .collect(),
                provenance: provenance(describing, in_edges.len(), out_edges.len()),
            }
        })
        .collect()
}

/// How many entities and relations the graph holds, whether or not the list
/// above could reach them.
///
/// The count belongs beside the list rather than inside it. `memory()` returns
/// the most-connected few hundred, and a status bar that counted its own rows
/// would say "200 nodes" for a graph of twelve thousand and be wrong in the one
/// direction that matters — a browser claiming to show everything.
pub fn graph_size(jod: &Arc<Jod>) -> (usize, usize) {
    jod.store()
        .and_then(|store| store.graph_size().ok())
        .unwrap_or((0, 0))
}

fn edge(e: &Edge, kind_at: impl Fn(i64) -> MemoryKind) -> MemoryEdge {
    MemoryEdge {
        kind: e.predicate.clone(),
        other: e.other_id.to_string(),
        other_name: e.other.clone(),
        other_kind: kind_at(e.other_id),
        // `contradicts` is the one edge kind the screen marks: an unresolved
        // contradiction is what a person browsing their own memory most needs
        // to see.
        warn: e.predicate == "contradicts",
    }
}

/// The newest fact that names `name` without `name` being its subject.
///
/// Found through the in-edges rather than by another query: an edge and the
/// fact behind it agree on the predicate and on both ends, so the fact is
/// already in hand if its subject was read.
fn asserting<'a>(
    asserted: &'a HashMap<String, Vec<Fact>>,
    name: &str,
    edges: &[Edge],
) -> Option<&'a Fact> {
    edges
        .iter()
        .filter(|e| !e.outgoing)
        .filter_map(|e| {
            asserted
                .get(&e.other)?
                .iter()
                .find(|f| f.predicate == e.predicate && f.object == name)
        })
        .max_by_key(|f| f.recorded_at_ms)
}

/// What kind of memory a node is.
///
/// The fact a thing is the *subject* of is what says what it is; a thing only
/// ever spoken about is an entity. `entities.kind` is not consulted because it
/// is `thing` on every row `intern` writes — when the memory-types work gives
/// it a real value, this reads that instead and the origin becomes a fallback.
fn kind_of_node(asserted: &HashMap<String, Vec<Fact>>, name: &str) -> MemoryKind {
    asserted
        .get(name)
        .and_then(|facts| facts.first())
        .map(kind_of)
        .unwrap_or(MemoryKind::Entity)
}

fn provenance(describing: Option<&Fact>, into: usize, out: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(f) = describing {
        lines.push(match &f.source {
            Some(source) => format!("{source} · {}", f.origin.as_str()),
            None => format!("{} · scope {}", f.origin.as_str(), f.scope),
        });
    }
    lines.push(format!("{out} out, {into} in"));
    lines
}

/// What kind of memory a fact makes its subject.
///
/// Origin is the only thing the store records about *why* a fact is believed,
/// so it is what this reads. `MemoryKind::Procedure` is unreachable until facts
/// carry a kind of their own — nothing writes one today, and guessing which
/// sentences are procedures from their text would be a worse answer than none.
fn kind_of(f: &Fact) -> MemoryKind {
    match f.origin {
        // Reljod said so: a standing belief, not an observation.
        Origin::Owner => MemoryKind::Belief,
        // Jod recorded something that happened — a goal iteration, a run's
        // outcome. That is the episodic record by definition.
        Origin::System => MemoryKind::Episode,
        Origin::Agent | Origin::Untrusted => MemoryKind::Fact,
    }
}

/// How much a fact's origin is worth, on the 0–1 scale the node list shows.
///
/// Facts have no confidence column, and inventing a gradient would put a number
/// on the screen that nothing could ever justify. Origin is the trust signal
/// the store does keep — it is what decides whether a fact may answer a recall
/// at all — so the column shows that, and shows it the same way every time.
fn trust(origin: Origin) -> f64 {
    match origin {
        Origin::Owner => 1.0,
        Origin::System => 0.9,
        Origin::Agent => 0.7,
        Origin::Untrusted => 0.3,
    }
}

/// Every schedule, with its next fire and its last seven outcomes.
pub fn schedules(jod: &Arc<Jod>) -> Vec<ScheduleRow> {
    let Some(store) = jod.store() else {
        return Vec::new();
    };
    let Ok(list) = store.schedules() else {
        return Vec::new();
    };
    list.into_iter().map(|s| schedule_row(store, s)).collect()
}

fn schedule_row(store: &Store, s: Schedule) -> ScheduleRow {
    // Newest first out of the store; the strip reads oldest first, left to
    // right, the way a person reads a week.
    let fires = store.fires(&s.id, HISTORY.max(RECENT)).unwrap_or_default();
    let judged: Vec<(&Fire, Outcome, Option<StoredRun>)> = fires
        .iter()
        .map(|f| {
            let run = f
                .run_id
                .as_deref()
                .and_then(|id| store.run(id).ok().flatten());
            (f, fire_outcome(f, run.as_ref()), run)
        })
        .collect();

    // A fixed seven cells, padded at the *left*: a schedule that has fired
    // twice reads as two runs at the end of a week, not as a short strip.
    let mut history: Vec<Outcome> = vec![Outcome::Missing; HISTORY];
    for (i, (_, outcome, _)) in judged.iter().take(HISTORY).enumerate() {
        history[HISTORY - 1 - i] = *outcome;
    }

    let recent = judged
        .iter()
        .take(RECENT)
        .map(|(f, outcome, run)| PastRun {
            at_ms: f.fired_at_ms,
            outcome: *outcome,
            // A run records when it started and never when it stopped, so
            // there is no duration to show. `StoredRun` needs an `ended_at_ms`
            // before this can be anything but zero.
            duration_ms: 0,
            cost_usd: run.as_ref().and_then(run_cost).unwrap_or(0.0),
            note: f
                .detail
                .clone()
                .unwrap_or_else(|| run_note(run.as_ref(), f.outcome)),
        })
        .collect();

    ScheduleRow {
        gloss: gloss(&s.cron),
        state: match s.state {
            StoredScheduleState::Paused => ScheduleState::Paused,
            StoredScheduleState::Broken => ScheduleState::Failing,
            // Armed and failing is a real state and the most important one to
            // see: the breaker only trips after several failures, so a
            // schedule can be dying for hours while its column still says
            // `armed`.
            StoredScheduleState::Armed if s.consecutive_failures > 0 => ScheduleState::Failing,
            StoredScheduleState::Armed => ScheduleState::Armed,
        },
        next_ms: s.next_fire_at_ms,
        // The column is set when a fire is *released*, so a claimant that died
        // mid-run leaves it behind — and a schedule that has visibly fired
        // showing "last: —" reads as a broken screen rather than as a lost
        // lease. The newest fire is the same answer from the record of what
        // actually happened.
        last_ms: s
            .last_fire_at_ms
            .or_else(|| judged.first().map(|(f, _, _)| f.fired_at_ms)),
        runs_as: runs_as(&s.harness, &s.cwd, s.model.as_deref()),
        policy: format!(
            "misfire {} · overlap {} · grace {} · {} consecutive failures",
            s.misfire.as_str(),
            s.overlap.as_str(),
            short_duration(s.grace_ms),
            s.consecutive_failures
        ),
        prompt: s.prompt,
        timezone: s.timezone,
        cron: s.cron,
        name: s.name,
        history,
        recent,
    }
}

/// How a fire ended, judged with the run it started.
///
/// A fire records the *decision* — it ran, it was skipped, it could not be
/// spawned — and a decision to run says nothing about how the run went. Reading
/// only the fire would paint a schedule whose every run fails as seven healthy
/// cells.
fn fire_outcome(f: &Fire, run: Option<&StoredRun>) -> Outcome {
    match f.outcome {
        FireOutcome::Ran => match run.map(|r| r.status.as_str()) {
            Some("failed") | Some("killed") => Outcome::Failed,
            _ => Outcome::Ok,
        },
        // A monitor that woke nobody is a *success* — the schedule did its job
        // for the price of a hash — but it is not a run, and the strip is
        // about runs. `Idle` is the cell for "ran and did nothing", which is
        // exactly what happened.
        FireOutcome::SkippedOverlap
        | FireOutcome::SkippedMisfire
        | FireOutcome::Replaced
        | FireOutcome::MonitorQuiet => Outcome::Idle,
        FireOutcome::SpawnFailed | FireOutcome::Abandoned => Outcome::Failed,
        // A row this build cannot read. `Missing` is the honest cell: it says
        // nothing rather than claiming a run happened, which is the same choice
        // `FireOutcome::Unknown` itself exists to make.
        FireOutcome::Unknown => Outcome::Missing,
    }
}

/// What a run cost, out of the summary the store keeps verbatim.
fn run_cost(run: &StoredRun) -> Option<f64> {
    run.summary.get("usage")?.get("cost_usd")?.as_f64()
}

fn run_note(run: Option<&StoredRun>, outcome: FireOutcome) -> String {
    match run {
        Some(r) => r
            .summary
            .get("last_message")
            .and_then(|m| m.as_str())
            .map(one_line)
            .unwrap_or_else(|| r.status.clone()),
        None => outcome.as_str().replace('_', " "),
    }
}

/// The first line of a message, which is all a table row has room for.
fn one_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

fn runs_as(harness: &str, cwd: &str, model: Option<&str>) -> String {
    match model {
        Some(m) => format!("{harness} · {m} · {cwd}"),
        None => format!("{harness} · {cwd}"),
    }
}

/// Every goal, with its checklist and its iteration log.
pub fn goals(jod: &Arc<Jod>) -> Vec<GoalRow> {
    let Some(store) = jod.store() else {
        return Vec::new();
    };
    let Ok(list) = store.goals() else {
        return Vec::new();
    };
    list.into_iter().map(|g| goal_row(store, g)).collect()
}

fn goal_row(store: &Store, g: Goal) -> GoalRow {
    // A goal's progress is in the fact store rather than in its own columns —
    // that is what makes it memory rather than a job queue — so its history is
    // read the same way `Ticker::spawn_iteration` reads it.
    let facts = store
        .facts_about(&format!("goal/{}", g.name))
        .unwrap_or_default();
    let mut iterations: Vec<Iteration> = facts
        .iter()
        .filter(|f| f.predicate == "iteration")
        .map(iteration)
        .collect();
    // Newest first, by the iteration's own number before its timestamp: several
    // iterations of a fast goal can land in the same millisecond, and
    // `facts_about` orders on the timestamp alone. The number is the ordering
    // the ticker actually assigned.
    iterations.sort_by_key(|i| (Reverse(i.n), Reverse(i.at_ms)));
    let in_flight = facts.iter().any(|f| f.predicate == "current-run");

    GoalRow {
        cadence: gloss(&g.cron),
        // The newest iteration is when this goal last did anything. The goals
        // table has no `last_fire_at_ms` of its own, and the episodic record is
        // a better answer than one anyway: it is the last time something
        // *happened*, not the last time a tick looked.
        last_ms: iterations.first().map(|i| i.at_ms),
        next_ms: g.next_fire_at_ms,
        state: match g.state {
            // Running and nothing in flight means the next iteration is
            // pending, which is what `waiting` is for.
            StoredGoalState::Running if in_flight => GoalState::Running,
            StoredGoalState::Running => GoalState::Waiting,
            StoredGoalState::Paused => GoalState::Paused,
            StoredGoalState::Satisfied => GoalState::Satisfied,
            StoredGoalState::Stalled | StoredGoalState::Exhausted | StoredGoalState::Blocked => {
                GoalState::Blocked
            }
        },
        iteration: g.iteration.max(0) as usize,
        // `done_when` is one runnable command, not a checklist, so the
        // checklist is that one line — and it is ticked when the goal's own
        // state says the check passed. That single line is the denominator the
        // progress bar divides by, which is why a goal with no check reads 0%
        // rather than an invented fraction.
        checks: g
            .done_when
            .clone()
            .map(|check| {
                vec![Check {
                    done: g.state == StoredGoalState::Satisfied,
                    text: check,
                    note: None,
                }]
            })
            .unwrap_or_default(),
        stop_if: stop_if(&g),
        spent_usd: g.spent_usd,
        budget_usd: g.budget_usd.unwrap_or(0.0),
        // A goal that stopped on its own is exactly the case this line exists
        // for: an autonomous loop that quietly needs a person and never says so
        // is worse than no goal at all.
        escalation: match g.state {
            StoredGoalState::Stalled => Some(format!(
                "stalled — {} iterations moved nothing",
                g.no_progress
            )),
            StoredGoalState::Exhausted => Some("out of budget or out of iterations".to_string()),
            StoredGoalState::Blocked => Some("blocked — waiting on an answer".to_string()),
            _ => None,
        },
        objective: g.objective,
        name: g.name,
        iterations,
    }
}

fn stop_if(g: &Goal) -> String {
    let mut stops = vec![format!("{} iterations move nothing", g.stall_after)];
    if let Some(max) = g.max_iterations {
        stops.push(format!("iteration {max} finishes"));
    }
    if let Some(budget) = g.budget_usd {
        stops.push(format!("${budget:.2} is spent"));
    }
    stops.join(" · ")
}

/// One `iteration` fact, which the ticker writes as `<n>: <what happened>`.
fn iteration(f: &Fact) -> Iteration {
    let (n, note) = match f.object.split_once(':') {
        Some((n, rest)) => match n.trim().parse::<usize>() {
            Ok(n) => (n, rest.trim().to_string()),
            Err(_) => (0, f.object.clone()),
        },
        None => (0, f.object.clone()),
    };
    Iteration {
        n,
        at_ms: f.recorded_at_ms,
        // The ticker writes the run's last message, or the bare status word
        // when there was none — so the status word *is* the whole note, and
        // that is the only signal of how the iteration went that survives. The
        // run itself is gone: `current-run` is superseded every iteration, so
        // the id that could carry a duration and a cost is not in the record.
        outcome: match note.as_str() {
            "failed" | "killed" => Outcome::Failed,
            _ => Outcome::Ok,
        },
        duration_ms: 0,
        cost_usd: 0.0,
        note,
    }
}

/// Every webhook rule, with its recent deliveries.
pub fn hooks(jod: &Arc<Jod>) -> Vec<HookRow> {
    let Some(store) = jod.store() else {
        return Vec::new();
    };
    let Ok(rules) = store.webhook_rules() else {
        return Vec::new();
    };
    // One query for every rule's deliveries, rather than one per rule: they
    // share a table and this screen wants all of them.
    let deliveries = store.deliveries(DELIVERY_WINDOW).unwrap_or_default();
    let now = now_ms();
    rules
        .into_iter()
        .map(|rule| hook_row(rule, &deliveries, now))
        .collect()
}

fn hook_row(rule: Rule, deliveries: &[StoredDelivery], now: i64) -> HookRow {
    let mine: Vec<&StoredDelivery> = deliveries
        .iter()
        .filter(|d| d.rule_id.as_deref() == Some(rule.id.as_str()))
        .collect();
    let last = mine.first();
    let last_outcome = last
        .map(|d| delivery_outcome(d.status))
        .unwrap_or(Outcome::Missing);

    HookRow {
        event: match &rule.action {
            Some(action) => format!("{}.{action}", rule.event),
            None => rule.event.clone(),
        },
        runs: rule.harness.clone(),
        deliveries_24h: mine
            .iter()
            .filter(|d| now - d.received_at_ms <= 24 * 60 * 60 * 1000)
            .count(),
        last_ms: last.map(|d| d.received_at_ms),
        last_outcome,
        state: if !rule.enabled {
            HookState::Idle
        } else if last_outcome == Outcome::Failed {
            HookState::Failing
        } else {
            HookState::Armed
        },
        // The route `jod-api` serves, which is `jod_api::webhook::PATH`. Spelled
        // out rather than imported: the TUI does not depend on the daemon's
        // crate, and the host it is reachable on is the tunnel's business, not
        // the store's.
        endpoint: "POST /webhooks/github".to_string(),
        // Nothing records a secret *check*; a delivery that failed one is
        // recorded as rejected, and that is the only evidence there is. A
        // rejection is written before any rule matches, though, so it carries
        // no rule id — which is why this can only speak for the deliveries that
        // reached this rule.
        secret: match last {
            Some(d) if d.status == DeliveryStatus::Rejected => {
                "✗ the last delivery failed its signature check".to_string()
            }
            Some(_) => format!("✓ verified on {} deliveries", mine.len()),
            None => "no delivery yet — nothing has been verified".to_string(),
        },
        match_rule: match_rule(&rule),
        runs_as: runs_as(&rule.harness, &rule.cwd, rule.model.as_deref()),
        prompt: rule.prompt.clone(),
        // Not a column on the rule: `webhook_rules` has no permission field on
        // purpose, so that a webhook run cannot ask for a permission the daemon
        // would not have given it anyway.
        policy: "untrusted payload · the daemon's default permission".to_string(),
        created: day(rule.created_at_ms),
        total: mine.len(),
        deliveries: mine.iter().copied().take(RECENT).map(delivery).collect(),
        repo: rule.repo,
        name: rule.name,
    }
}

fn match_rule(rule: &Rule) -> String {
    let c = &rule.conditions;
    let mut parts = vec![format!("{} on {}", rule.source, rule.repo)];
    if !c.labels.is_empty() {
        parts.push(format!("every label of {}", c.labels.join(", ")));
    }
    if let Some(branch) = &c.branch {
        parts.push(format!("branch {branch}"));
    }
    if let Some(author) = &c.author {
        parts.push(format!("author {author}"));
    }
    if let Some(draft) = c.draft {
        parts.push(format!("draft {draft}"));
    }
    parts.join(" · ")
}

fn delivery(d: &StoredDelivery) -> Delivery {
    Delivery {
        at_ms: d.received_at_ms,
        id: d.delivery_id.clone(),
        what: match (&d.action, &d.repo) {
            (Some(action), Some(repo)) => format!("{}.{action} on {repo}", d.event),
            (Some(action), None) => format!("{}.{action}", d.event),
            (None, Some(repo)) => format!("{} on {repo}", d.event),
            (None, None) => d.event.clone(),
        },
        accepted: d.status == DeliveryStatus::Accepted,
        run: d.run_id.clone(),
        verdict: match &d.detail {
            Some(detail) => format!("{} — {}", d.status.as_str().replace('_', " "), detail),
            None => d.status.as_str().replace('_', " "),
        },
    }
}

fn delivery_outcome(status: DeliveryStatus) -> Outcome {
    match status {
        DeliveryStatus::Accepted => Outcome::Ok,
        // Understood and wanted by nobody. That is the hook working, not
        // failing — the whole point of recording a no-match is to tell it from
        // silence.
        DeliveryStatus::NoMatch | DeliveryStatus::Duplicate => Outcome::Idle,
        DeliveryStatus::Rejected | DeliveryStatus::Failed => Outcome::Failed,
    }
}

/// A day, for the fields that want a date rather than a clock.
fn day(at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string())
}

/// What happened while nobody was looking.
///
/// Built from the three tables that other processes write — schedule fires,
/// webhook deliveries, and the goal loop's episodic facts — which is why this
/// screen exists at all: none of it happened in this process, so no in-memory
/// copy could ever be authoritative. Runs are deliberately absent: a cron fire
/// already names the run it started, and the fleet screen is where a run you
/// started yourself is watched.
///
/// TODO: `Store::mark_activity_read(id)` and a table to hold it. Every item
/// below reads as read, so the `u` filter shows nothing — unread is a *per
/// person* fact about an event, and there is nowhere to put it yet.
pub fn activity(jod: &Arc<Jod>) -> Vec<ActivityItem> {
    match jod.store() {
        Some(store) => activity_from(store),
        None => Vec::new(),
    }
}

fn activity_from(store: &Store) -> Vec<ActivityItem> {
    let mut items: Vec<ActivityItem> = Vec::new();

    for s in store.schedules().unwrap_or_default() {
        for f in store.fires(&s.id, RECENT).unwrap_or_default() {
            items.push(ActivityItem {
                id: format!("fire/{}", f.id),
                at_ms: f.fired_at_ms,
                source: Source::Cron,
                text: format!("{} · {}", s.name, f.outcome.as_str().replace('_', " ")),
                unread: false,
                // A schedule Jod could not start, or one whose claimant died,
                // is silence that nothing else will report.
                needs_you: matches!(f.outcome, FireOutcome::SpawnFailed | FireOutcome::Abandoned),
                jump_to: Some((Workspace::Schedules, s.name.clone())),
            });
        }
    }

    for g in store.goals().unwrap_or_default() {
        for f in store
            .facts_about(&format!("goal/{}", g.name))
            .unwrap_or_default()
        {
            let ended = f.predicate == "ended";
            if !ended && f.predicate != "iteration" {
                continue;
            }
            items.push(ActivityItem {
                id: format!("goal/{}/{}", g.name, f.id),
                at_ms: f.recorded_at_ms,
                source: Source::Goal,
                text: format!("{} · {}", g.name, one_line(&f.object)),
                unread: false,
                // A goal ending is the one goal event a person has to see: it
                // is the loop saying it will not run again.
                needs_you: ended,
                jump_to: Some((Workspace::Goals, g.name.clone())),
            });
        }
    }

    // A delivery names its rule by id; the hooks screen's rows are keyed by
    // name. Without the translation `⏎` would jump to the screen and select
    // nothing, and the next tick's `reconcile` would move the cursor somewhere
    // else entirely — a jump that looks like it worked and did not.
    let rule_names: HashMap<String, String> = store
        .webhook_rules()
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    for d in store.deliveries(DELIVERY_WINDOW).unwrap_or_default() {
        items.push(ActivityItem {
            id: format!("delivery/{}", d.delivery_id),
            at_ms: d.received_at_ms,
            source: Source::Hook,
            text: format!(
                "{} · {}",
                delivery(&d).what,
                d.status.as_str().replace('_', " ")
            ),
            unread: false,
            // A rejected delivery is a secret that stopped verifying; a failed
            // one is a rule that matched and could not run.
            needs_you: matches!(d.status, DeliveryStatus::Rejected | DeliveryStatus::Failed),
            jump_to: d
                .rule_id
                .as_deref()
                .and_then(|id| rule_names.get(id))
                .map(|name| (Workspace::Hooks, name.clone())),
        });
    }

    items.sort_by_key(|i| Reverse(i.at_ms));
    items.truncate(ACTIVITY_LIMIT);
    items
}

/// The task board, promoted out of the team panel into a screen of its own.
///
/// With no team joined this is every team's board, because the screen is *the*
/// board rather than one team's panel — the team panel is the thing that is
/// scoped to one team.
///
/// TODO: `jod_core::team::TeamTask` carries an id, a title, an owner and a
/// status and nothing else. Four columns of this screen are waiting on it: the
/// run doing the task, the task's age (there is no created-at), its runnable
/// check, and the blocked-by/blocks pair. Each shows as empty rather than as a
/// guess.
pub fn tasks(jod: &Arc<Jod>, team: Option<&str>) -> Vec<TaskRow> {
    let Some(store) = jod.store() else {
        return Vec::new();
    };
    let teams: Vec<String> = match team {
        Some(t) => vec![t.to_string()],
        None => store.teams().unwrap_or_default(),
    };
    teams
        .iter()
        .flat_map(|t| store.team_tasks(t).unwrap_or_default())
        .map(task_row)
        .collect()
}

// ---- the decision rail, and the `@` picker ------------------------------

/// The cards the rail is showing.
///
/// The whole filter travels as a [`Query`] rather than being applied here,
/// which is the rule E2.S5 sets: the text filter is full-text search through
/// the same index `jod card ls` and the MCP tool go through, so a card found in
/// the terminal is the card found on a phone. A filter re-implemented in Rust
/// would drift from those two on the first keystroke.
pub fn cards(jod: &Arc<Jod>, query: &Query) -> Vec<Card> {
    match jod.store() {
        Some(store) => store.cards(query).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The directories a conversation may work in, in the user's own order.
///
/// Empty with no conversation *and* empty with no roots, which are the same
/// thing to the picker: both mean there is nothing to search, and E1.S3 says
/// the popup must say so rather than fall back to the process's own directory.
pub fn roots(jod: &Arc<Jod>, conversation: Option<&str>) -> Vec<Root> {
    let (Some(store), Some(conversation)) = (jod.store(), conversation) else {
        return Vec::new();
    };
    store.roots(conversation).unwrap_or_default()
}

/// Every root's candidate paths, positionally aligned with `roots`.
///
/// Loaded on the tick rather than when `@` is pressed. The enumeration is
/// cached inside [`rank::candidates_shared`] with a short TTL, so this is
/// usually a lock and a pointer copy — but *usually* is not a guarantee, and
/// the one call that misses the cache walks a repository. Doing that from the
/// key handler would stall the keystroke that opened the popup; doing it here
/// costs a frame of a background refresh instead.
///
/// A root that cannot be read contributes an empty list rather than failing the
/// lot: a conversation with two roots and one unmounted disk should still be
/// able to mention a file in the other.
pub fn candidates(roots: &[Root]) -> Vec<Arc<Vec<String>>> {
    roots
        .iter()
        .map(|root| rank::candidates_shared(&root.path).unwrap_or_default())
        .collect()
}

fn task_row(t: TeamTask) -> TaskRow {
    TaskRow {
        state: match t.status.as_str() {
            "done" => TaskState::Done,
            "blocked" => TaskState::Blocked,
            "running" => TaskState::Running,
            _ if t.is_claimed() => TaskState::Claimed,
            _ => TaskState::Open,
        },
        owner: t.owner,
        run: None,
        age_ms: 0,
        what: t.title.clone(),
        check: String::new(),
        blocked_by: Vec::new(),
        blocks: Vec::new(),
        spec: None,
        history: Vec::new(),
        title: t.title,
        id: t.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::schedule::{Misfire, Overlap};
    use jod_core::store::NewFact;
    use jod_core::webhook::Conditions;

    // ---- the cron gloss ----

    /// The three shapes the schedules screen was designed around, which are
    /// also the three most people write.
    #[test]
    fn a_cron_expression_reads_as_a_phrase() {
        assert_eq!(gloss("0 2 * * *"), "02:00 every day");
        assert_eq!(gloss("*/15 * * * *"), "every 15 minutes");
        assert_eq!(gloss("0 9 * * 1-5"), "09:00 Mon–Fri");
    }

    #[test]
    fn the_gloss_reads_hours_minutes_and_named_days() {
        assert_eq!(gloss("0 * * * *"), "every hour at :00");
        assert_eq!(gloss("30 * * * *"), "every hour at :30");
        assert_eq!(gloss("0 */2 * * *"), "every 2 hours");
        assert_eq!(gloss("* * * * *"), "every minute");
        assert_eq!(gloss("0 0 * * 0"), "00:00 Sun");
        assert_eq!(gloss("15 6 * * 1,3,5"), "06:15 Mon, Wed, Fri");
        assert_eq!(gloss("*/10 * * * 6,0"), "every 10 minutes, Sat, Sun");
        assert_eq!(gloss("0 9 * * MON-FRI"), "09:00 Mon–Fri");
    }

    /// A wrong gloss is worse than none: the expression is what decides when an
    /// agent runs unattended, so anything not read with certainty comes back
    /// exactly as it was written.
    #[test]
    fn an_expression_the_gloss_cannot_read_comes_back_untouched() {
        for cron in [
            // A day-of-month restriction, which the gloss says nothing about.
            "0 2 1 * *",
            // A month restriction, likewise.
            "0 2 * 6 *",
            // Six fields: croner's seconds-first form. Reading it as five would
            // shift every field by a place.
            "0 0 2 * * *",
            // Lists and ranges in the minute and hour fields.
            "0,30 9 * * *",
            "0 9-17 * * *",
            // Not an expression at all.
            "@daily",
            "",
        ] {
            assert_eq!(gloss(cron), cron, "{cron} must not be guessed at");
        }
    }

    // ---- schedules ----

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    const AT: i64 = 1_800_000_000_000;

    fn schedule(name: &str) -> Schedule {
        Schedule {
            id: format!("sch-{name}"),
            name: name.to_string(),
            prompt: "sweep the open PRs".into(),
            harness: "claude-code".into(),
            cwd: "/home/reljod/repo/Jod".into(),
            model: None,
            cron: "0 2 * * *".into(),
            timezone: "UTC".into(),
            state: StoredScheduleState::Armed,
            misfire: Misfire::default(),
            overlap: Overlap::default(),
            grace_ms: 60_000,
            jitter_ms: 0,
            next_fire_at_ms: Some(AT + 3_600_000),
            last_fire_at_ms: Some(AT - 3_600_000),
            consecutive_failures: 0,
            created_at_ms: AT - 86_400_000,
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

    #[test]
    fn a_schedule_row_carries_its_next_fire_its_gloss_and_its_history() {
        let store = store();
        let s = schedule("shepherd");
        store.add_schedule(&s).unwrap();
        store
            .record_fire(&fire(&s.id, AT - 3_600_000, FireOutcome::Ran))
            .unwrap();
        store
            .record_fire(&fire(&s.id, AT - 7_200_000, FireOutcome::SpawnFailed))
            .unwrap();

        let row = schedule_row(&store, s);
        assert_eq!(row.name, "shepherd");
        assert_eq!(row.gloss, "02:00 every day");
        assert_eq!(row.cron, "0 2 * * *");
        assert_eq!(row.next_ms, Some(AT + 3_600_000));
        assert_eq!(row.last_ms, Some(AT - 3_600_000));
        assert_eq!(row.state, ScheduleState::Armed);
        assert_eq!(row.history.len(), 7, "the strip is always seven cells");
        assert_eq!(
            &row.history[5..],
            &[Outcome::Failed, Outcome::Ok],
            "oldest first, and the two that happened sit at the right-hand end"
        );
        assert_eq!(row.recent.len(), 2);
        assert_eq!(
            row.recent[0].outcome,
            Outcome::Ok,
            "newest first in the detail pane"
        );
    }

    /// The breaker only trips after several failures, so a schedule can be
    /// dying for hours while its own column still says `armed`. The row has to
    /// say so anyway — that is the whole point of the glyph.
    /// A claimant that died mid-run never released the schedule, so the column
    /// still says it has never fired. The fires say otherwise, and they are the
    /// record of what actually happened.
    #[test]
    fn a_schedule_that_never_released_its_last_fire_still_says_when_it_fired() {
        let store = store();
        let mut s = schedule("orphaned");
        s.last_fire_at_ms = None;
        store.add_schedule(&s).unwrap();
        store
            .record_fire(&fire(&s.id, AT - 60_000, FireOutcome::Abandoned))
            .unwrap();
        assert_eq!(schedule_row(&store, s).last_ms, Some(AT - 60_000));
    }

    #[test]
    fn a_schedule_that_is_still_armed_but_failing_says_so() {
        let store = store();
        let mut s = schedule("flaky");
        s.consecutive_failures = 2;
        assert_eq!(schedule_row(&store, s).state, ScheduleState::Failing);
    }

    #[test]
    fn a_paused_schedule_is_paused_rather_than_failing() {
        let store = store();
        let mut s = schedule("resting");
        s.state = StoredScheduleState::Paused;
        s.consecutive_failures = 3;
        assert_eq!(schedule_row(&store, s).state, ScheduleState::Paused);
    }

    /// A skip is not a failure. "It never fired" and "it fired and was skipped"
    /// are different bugs, and the strip has to keep them different.
    #[test]
    fn a_skipped_fire_reads_as_idle_and_a_spawn_failure_as_failed() {
        assert_eq!(
            fire_outcome(&fire("s", AT, FireOutcome::SkippedOverlap), None),
            Outcome::Idle
        );
        assert_eq!(
            fire_outcome(&fire("s", AT, FireOutcome::SkippedMisfire), None),
            Outcome::Idle
        );
        assert_eq!(
            fire_outcome(&fire("s", AT, FireOutcome::SpawnFailed), None),
            Outcome::Failed
        );
        assert_eq!(
            fire_outcome(&fire("s", AT, FireOutcome::Ran), None),
            Outcome::Ok
        );
    }

    // ---- goals ----

    fn goal(name: &str) -> Goal {
        Goal {
            id: format!("goal-{name}"),
            name: name.to_string(),
            objective: "Keep the inbox at zero.".into(),
            done_when: Some("test -z \"$(inbox --older-than 48h)\"".into()),
            harness: "claude-code".into(),
            cwd: "/home/reljod".into(),
            model: None,
            cron: "0 9 * * 1-5".into(),
            timezone: "UTC".into(),
            state: StoredGoalState::Running,
            iteration: 3,
            max_iterations: Some(50),
            budget_usd: Some(25.0),
            spent_usd: 11.40,
            stall_after: 3,
            no_progress: 0,
            next_fire_at_ms: Some(AT + 600_000),
            created_at_ms: AT - 86_400_000,
        }
    }

    fn iteration_fact(goal: &Goal, object: &str) -> NewFact {
        NewFact::new(format!("goal/{}", goal.name), "iteration", object)
            .in_scope(goal.memory_scope())
            .from(Origin::System)
    }

    #[test]
    fn a_goal_row_carries_its_iteration_count_and_its_iteration_log() {
        let store = store();
        let g = goal("inbox-to-zero");
        store.add_goal(&g).unwrap();
        store
            .remember(iteration_fact(&g, "1: filed 12 messages"))
            .unwrap();
        store
            .remember(iteration_fact(&g, "2: filed 3 messages"))
            .unwrap();
        store.remember(iteration_fact(&g, "3: failed")).unwrap();

        let row = goal_row(&store, g);
        assert_eq!(row.iteration, 3, "the count is the goal's own column");
        assert_eq!(
            row.iterations.len(),
            3,
            "and every one of them is in the log"
        );
        assert_eq!(row.cadence, "09:00 Mon–Fri");
        assert_eq!(row.iterations[0].n, 3, "newest first");
        assert_eq!(row.iterations[0].note, "failed");
        assert_eq!(
            row.iterations[0].outcome,
            Outcome::Failed,
            "the ticker writes the bare status word when a run said nothing"
        );
        assert_eq!(row.iterations[2].note, "filed 12 messages");
        assert_eq!(row.iterations[2].outcome, Outcome::Ok);
        assert_eq!(
            row.last_ms,
            Some(row.iterations[0].at_ms),
            "a goal last did something when its newest iteration was written"
        );
        assert_eq!(row.spent_usd, 11.40);
        assert_eq!(row.budget_usd, 25.0);
    }

    /// `done_when` is one runnable command, so the checklist is that one line —
    /// and it is the denominator the progress bar divides by.
    #[test]
    fn a_goals_done_when_check_is_its_whole_checklist() {
        let store = store();
        let mut g = goal("inbox-to-zero");
        assert_eq!(
            goal_row(&store, g.clone()).percent(),
            0,
            "not satisfied yet"
        );
        g.state = StoredGoalState::Satisfied;
        let row = goal_row(&store, g);
        assert_eq!(row.checks.len(), 1);
        assert_eq!(row.percent(), 100);
        assert_eq!(row.state, GoalState::Satisfied);
    }

    #[test]
    fn a_goal_with_no_check_has_no_checklist_rather_than_an_invented_one() {
        let store = store();
        let mut g = goal("open-ended");
        g.done_when = None;
        let row = goal_row(&store, g);
        assert!(row.checks.is_empty());
        assert_eq!(row.percent(), 0);
    }

    /// A loop that stopped on its own has to say so on the screen: an
    /// autonomous goal that quietly needs a person and never says so is worse
    /// than no goal at all.
    #[test]
    fn a_goal_that_stopped_on_its_own_escalates() {
        let store = store();
        let mut g = goal("stuck");
        g.state = StoredGoalState::Stalled;
        g.no_progress = 4;
        let row = goal_row(&store, g);
        assert_eq!(row.state, GoalState::Blocked);
        assert!(
            row.escalation.is_some_and(|e| e.contains("stalled")),
            "the reason it stopped belongs on the row"
        );
    }

    /// Running with nothing in flight is `waiting`, and the distinction is the
    /// whole answer to "is this goal actually doing anything right now".
    #[test]
    fn a_running_goal_with_no_run_in_flight_is_waiting() {
        let store = store();
        let g = goal("hourly");
        store.add_goal(&g).unwrap();
        assert_eq!(goal_row(&store, g.clone()).state, GoalState::Waiting);

        store
            .remember(
                NewFact::new(format!("goal/{}", g.name), "current-run", "run-1")
                    .in_scope(g.memory_scope())
                    .from(Origin::System),
            )
            .unwrap();
        assert_eq!(goal_row(&store, g).state, GoalState::Running);
    }

    // ---- webhooks ----

    fn rule(name: &str) -> Rule {
        Rule {
            id: format!("rule-{name}"),
            name: name.to_string(),
            source: "github".into(),
            repo: "Reljod/Jod".into(),
            event: "pull_request".into(),
            action: Some("opened".into()),
            conditions: Conditions {
                labels: vec!["agent".into()],
                ..Conditions::default()
            },
            prompt: "review {{pull_request.title}}".into(),
            harness: "claude-code".into(),
            cwd: "/home/reljod/repo/Jod".into(),
            model: None,
            enabled: true,
            created_at_ms: AT - 86_400_000,
        }
    }

    fn stored_delivery(rule: &Rule, at_ms: i64, status: DeliveryStatus) -> StoredDelivery {
        StoredDelivery {
            id: 0,
            delivery_id: format!("d-{at_ms}"),
            source: "github".into(),
            event: "pull_request".into(),
            action: Some("opened".into()),
            repo: Some("Reljod/Jod".into()),
            rule_id: Some(rule.id.clone()),
            run_id: Some("run-7".into()),
            status,
            detail: None,
            received_at_ms: at_ms,
        }
    }

    #[test]
    fn a_hook_row_counts_only_its_own_deliveries() {
        let r = rule("pr-review");
        let other = rule("issues");
        let deliveries = vec![
            stored_delivery(&r, AT - 60_000, DeliveryStatus::Accepted),
            stored_delivery(&r, AT - 3_600_000, DeliveryStatus::NoMatch),
            // Older than a day, so it counts towards the total and not the 24h.
            stored_delivery(&r, AT - 200_000_000, DeliveryStatus::Accepted),
            stored_delivery(&other, AT - 60_000, DeliveryStatus::Accepted),
        ];
        let row = hook_row(r, &deliveries, AT);
        assert_eq!(row.event, "pull_request.opened");
        assert_eq!(row.total, 3);
        assert_eq!(row.deliveries_24h, 2);
        assert_eq!(row.last_ms, Some(AT - 60_000));
        assert_eq!(row.last_outcome, Outcome::Ok);
        assert_eq!(row.state, HookState::Armed);
        assert!(
            row.match_rule.contains("agent"),
            "conditions belong in the rule line"
        );
    }

    /// Understood and wanted by nobody is the hook *working*. Recording a
    /// no-match exists precisely to tell it from silence, so it must not read
    /// as a failure.
    #[test]
    fn a_delivery_nothing_matched_is_idle_rather_than_failed() {
        assert_eq!(delivery_outcome(DeliveryStatus::NoMatch), Outcome::Idle);
        assert_eq!(delivery_outcome(DeliveryStatus::Accepted), Outcome::Ok);
        assert_eq!(delivery_outcome(DeliveryStatus::Rejected), Outcome::Failed);
        assert_eq!(delivery_outcome(DeliveryStatus::Failed), Outcome::Failed);
    }

    #[test]
    fn a_disabled_rule_is_idle_and_a_rule_whose_last_delivery_failed_is_failing() {
        let mut r = rule("pr-review");
        r.enabled = false;
        assert_eq!(hook_row(r.clone(), &[], AT).state, HookState::Idle);

        r.enabled = true;
        let failed = vec![stored_delivery(&r, AT, DeliveryStatus::Failed)];
        assert_eq!(hook_row(r, &failed, AT).state, HookState::Failing);
    }

    // ---- activity ----

    /// `⏎` sets the target list's selection to this id, so it has to be the id
    /// that list is keyed by — a name for schedules, goals and hooks, never the
    /// row's database id. Getting it wrong renders perfectly and jumps nowhere.
    #[test]
    fn every_activity_item_jumps_to_an_id_its_screen_actually_has() {
        let store = store();
        let s = schedule("shepherd");
        store.add_schedule(&s).unwrap();
        store
            .record_fire(&fire(&s.id, AT, FireOutcome::Ran))
            .unwrap();
        let g = goal("inbox-to-zero");
        store.add_goal(&g).unwrap();
        store
            .remember(iteration_fact(&g, "1: filed 12 messages"))
            .unwrap();
        let r = rule("pr-review");
        store.add_webhook_rule(&r).unwrap();
        store
            .record_delivery(&stored_delivery(&r, AT, DeliveryStatus::Accepted))
            .unwrap();

        let items = activity_from(&store);
        let jumps: Vec<(Workspace, String)> =
            items.iter().filter_map(|i| i.jump_to.clone()).collect();
        assert!(jumps.contains(&(Workspace::Schedules, "shepherd".to_string())));
        assert!(jumps.contains(&(Workspace::Goals, "inbox-to-zero".to_string())));
        assert!(
            jumps.contains(&(Workspace::Hooks, "pr-review".to_string())),
            "a delivery names its rule by id, and the hooks list is keyed by name"
        );
    }

    #[test]
    fn a_delivery_that_failed_its_signature_check_needs_a_person() {
        let store = store();
        let r = rule("pr-review");
        store.add_webhook_rule(&r).unwrap();
        let mut rejected = stored_delivery(&r, AT, DeliveryStatus::Rejected);
        rejected.rule_id = None;
        store.record_delivery(&rejected).unwrap();

        let items = activity_from(&store);
        assert_eq!(items.len(), 1);
        assert!(items[0].needs_you);
        assert_eq!(
            items[0].jump_to, None,
            "a rejection is recorded before any rule matched, so it names none"
        );
    }

    // ---- memory ----

    #[test]
    fn a_node_carries_both_directions_of_every_edge_it_has() {
        let store = store();
        let g = goal("inbox-to-zero");
        store.add_goal(&g).unwrap();
        store
            .remember(iteration_fact(&g, "1: filed 12 messages"))
            .unwrap();

        let rows = memory_from(&store);
        let subject = rows
            .iter()
            .find(|n| n.name == "goal/inbox-to-zero")
            .expect("the goal's own subject is a node");
        assert_eq!(subject.out_edges.len(), 1);
        assert_eq!(subject.out_edges[0].kind, "iteration");
        assert_eq!(
            subject.kind,
            MemoryKind::Episode,
            "the ticker writes iterations as the system, which is the episodic record"
        );

        let object = rows
            .iter()
            .find(|n| n.name == "1: filed 12 messages")
            .expect("what a fact points at is a node too");
        assert_eq!(object.in_edges.len(), 1);
        assert_eq!(object.in_edges[0].other_name, "goal/inbox-to-zero");
        assert_eq!(
            object.kind,
            MemoryKind::Entity,
            "a thing only ever spoken about is an entity"
        );
        assert_eq!(subject.degree, 1);
    }

    /// The whole reason this reads `memory_nodes` rather than walking out from
    /// the subjects the TUI can name: a fact about a subject nothing else
    /// mentions is exactly what a person's own notes look like, and the walk
    /// could not see one.
    #[test]
    fn a_fact_about_nothing_else_is_still_in_the_list() {
        let store = store();
        store
            .remember(NewFact {
                source: Some("AGENTS.md".into()),
                ..NewFact::new("reljod", "prefers", "linear for tasks").from(Origin::Owner)
            })
            .unwrap();

        let rows = memory_from(&store);
        let node = rows
            .iter()
            .find(|n| n.name == "reljod")
            .expect("a subject nothing else points at is still a node");
        assert_eq!(node.kind, MemoryKind::Belief, "Reljod said so");
        assert_eq!(node.confidence, 1.0);
        assert_eq!(node.body, "reljod prefers linear for tasks");
        assert_eq!(node.degree, 1);
        assert!(node.provenance.iter().any(|p| p.contains("AGENTS.md")));
    }

    /// A node with a `contradicts` edge is in a contradiction whichever end of
    /// it it is, and the list marks it with `!` so the state survives a
    /// monochrome terminal.
    #[test]
    fn both_ends_of_a_contradiction_are_marked() {
        let store = store();
        store
            .remember(NewFact::new("spec first", "contradicts", "ship fast").from(Origin::Agent))
            .unwrap();
        store
            .remember(NewFact::new("reljod", "prefers", "spec first").from(Origin::Owner))
            .unwrap();

        let rows = memory_from(&store);
        for name in ["spec first", "ship fast"] {
            let node = rows.iter().find(|n| n.name == name).expect(name);
            assert!(
                node.contradicted,
                "{name} is in an unresolved contradiction"
            );
        }
        let untouched = rows.iter().find(|n| n.name == "reljod").unwrap();
        assert!(!untouched.contradicted, "a node with no such edge is not");
    }

    /// The list is capped at the most-connected few hundred, so the status bar
    /// counts the graph rather than the rows — a browser that counts its own
    /// rows says it is showing everything.
    #[test]
    fn the_graph_is_counted_separately_from_what_is_listed() {
        let store = store();
        store.remember(NewFact::new("a", "links", "b")).unwrap();
        store.remember(NewFact::new("b", "links", "c")).unwrap();
        assert_eq!(store.graph_size().unwrap(), (3, 2));
    }

    /// Origin is the only reason-to-believe the store records, so it is what
    /// the confidence column shows — the same number every time, rather than an
    /// invented gradient.
    #[test]
    fn a_nodes_confidence_is_the_trust_of_what_asserted_it() {
        assert!(trust(Origin::Owner) > trust(Origin::System));
        assert!(trust(Origin::System) > trust(Origin::Agent));
        assert!(trust(Origin::Agent) > trust(Origin::Untrusted));
    }

    #[test]
    fn what_reljod_said_is_a_belief_and_what_an_agent_concluded_is_a_fact() {
        let store = store();
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks").from(Origin::Owner))
            .unwrap();
        store
            .remember(NewFact::new("linear", "is a", "system of record").from(Origin::Agent))
            .unwrap();
        let rows = memory_from(&store);

        let said = rows.iter().find(|n| n.name == "reljod").unwrap();
        assert_eq!(said.kind, MemoryKind::Belief);
        assert_eq!(said.body, "reljod prefers linear for tasks");
        assert_eq!(said.confidence, 1.0);

        let concluded = rows.iter().find(|n| n.name == "linear").unwrap();
        assert_eq!(concluded.kind, MemoryKind::Fact);
        assert!(
            concluded.confidence < said.confidence,
            "an agent is not the owner"
        );
    }

    // ---- empty states ----

    /// A screen with nothing on it has to say what is missing and how to make
    /// one. An empty loader must degrade to that sentence, never to a blank
    /// box — the difference between "nothing scheduled" and "the TUI is broken"
    /// is the only thing the words carry.
    #[test]
    fn a_workspace_with_no_rows_renders_its_empty_state_rather_than_a_blank_box() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = super::super::App::new(
            jod_core::HarnessKind::ClaudeCode,
            None,
            jod_core::Resume::Fresh,
        );
        for (workspace, expected) in [
            (Workspace::Memory, "nothing remembered yet"),
            (Workspace::Schedules, "nothing scheduled yet"),
            (Workspace::Goals, "no goals yet"),
            (Workspace::Hooks, "no webhooks yet"),
            (Workspace::Tasks, "the board is empty"),
            (Workspace::Activity, "nothing has happened yet"),
        ] {
            app.go(workspace);
            app.reconcile();
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|f| {
                    super::super::ui::draw(f, &app);
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let screen: String = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                screen.contains(expected),
                "{} must say `{expected}`, and said:\n{screen}",
                workspace.title()
            );
        }
    }

    /// Colour is never the only channel, so every state has to answer with a
    /// glyph — and none of them may be an East-Asian *Wide* codepoint, which
    /// occupies two cells and shears every column to its right.
    #[test]
    fn every_state_glyph_is_one_cell_wide() {
        let mut glyphs: Vec<&str> = vec![
            Outcome::Ok.cell(),
            Outcome::Failed.cell(),
            Outcome::Idle.cell(),
            Outcome::Missing.cell(),
        ];
        glyphs.extend(MemoryKind::ALL.iter().map(|k| k.glyph()));
        glyphs.extend([
            ScheduleState::Armed.glyph(),
            ScheduleState::Paused.glyph(),
            ScheduleState::Failing.glyph(),
            HookState::Armed.glyph(),
            HookState::Idle.glyph(),
            HookState::Failing.glyph(),
            TaskState::Running.glyph(),
            TaskState::Blocked.glyph(),
            TaskState::Done.glyph(),
            GoalState::Running.glyph(),
            GoalState::Blocked.glyph(),
        ]);
        glyphs.extend(Source::ALL.iter().map(|s| s.glyph()));

        for glyph in glyphs {
            assert_eq!(glyph.chars().count(), 1, "{glyph} must be a single char");
            let c = glyph.chars().next().unwrap();
            assert!(
                !WIDE.contains(&c),
                "{glyph} is East-Asian Wide and would shear the columns to its right"
            );
        }
    }

    /// The two the report names by hand, having been bitten by them while
    /// drawing the wireframes.
    const WIDE: [char; 2] = ['⏰', '⏸'];

    #[test]
    fn every_memory_kind_has_a_tag_and_a_glyph_of_its_own() {
        let mut tags = Vec::new();
        for kind in MemoryKind::ALL {
            assert!(
                !tags.contains(&kind.tag()),
                "{} is claimed twice",
                kind.tag()
            );
            tags.push(kind.tag());
            assert!(kind.tag().len() <= 4);
        }
    }

    #[test]
    fn a_goals_progress_comes_from_its_checklist() {
        let mut goal = a_goal_with_two_checks();
        assert_eq!(goal.percent(), 50, "one of two checks is done");
        goal.checks[1].done = true;
        assert_eq!(goal.percent(), 100);
    }

    /// A goal with no checklist has no denominator, so it reports nothing
    /// rather than dividing by it.
    #[test]
    fn a_goal_with_no_checklist_is_zero_percent_rather_than_a_panic() {
        let mut goal = a_goal_with_two_checks();
        goal.checks.clear();
        assert_eq!(goal.percent(), 0);
    }

    fn a_goal_with_two_checks() -> GoalRow {
        GoalRow {
            name: "inbox-to-zero".into(),
            cadence: "hourly".into(),
            last_ms: None,
            next_ms: None,
            state: GoalState::Running,
            iteration: 118,
            objective: "Keep the inbox at zero.".into(),
            checks: vec![
                Check {
                    done: true,
                    text: "no item older than 48h".into(),
                    note: None,
                },
                Check {
                    done: false,
                    text: "every open item has an owner".into(),
                    note: None,
                },
            ],
            stop_if: "budget spent".into(),
            spent_usd: 11.40,
            budget_usd: 25.0,
            iterations: vec![],
            escalation: None,
        }
    }
}
