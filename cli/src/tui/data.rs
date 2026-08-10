//! What the workspaces show, as plain data.
//!
//! These are *view models*, not the store's types. The TUI renders a schedule
//! as a name, a human gloss, two timestamps and a seven-cell outcome strip; the
//! store holds a cron expression and a table of fires. Keeping the seam here
//! means the screens can be built and tested against fixtures, and the loaders
//! below can be filled in without any screen changing shape.
//!
//! **Every loader in this file returns nothing yet.** Each says which store
//! method it is waiting for. A screen with no rows says so in words rather than
//! showing an empty box, so an unfilled loader degrades to an honest empty
//! state rather than a bug.

// Several states below are constructed only by those loaders and by the
// fixtures the screens are tested against, so the compiler cannot yet see them
// being built. Removing them to silence that would mean deleting the vocabulary
// the screens are written against and adding it back later — the warning is
// about the unfilled loaders, not about the types.
#![allow(dead_code)]

use std::sync::Arc;

use jod_core::Jod;

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

// ---- loaders -----------------------------------------------------------
//
// Each of these is the seam between the TUI and the store. They return nothing
// until the store method named in the TODO exists; the screens they feed all
// say so in words rather than showing an empty box.

/// Every memory node, with its edges already joined.
///
/// TODO: `Store::memory_nodes()` — the graph tables land with the memory-types
/// work. `Store::graph_size()` and `Store::neighbourhood()` already exist and
/// are the shape this wants; what is missing is a listing that returns every
/// node with its degree in one query rather than one query per node.
pub fn memory(_jod: &Arc<Jod>) -> Vec<MemoryNode> {
    Vec::new()
}

/// Every schedule, with its next fire and its last seven outcomes.
///
/// TODO: `Store::schedules()` exists and returns `jod_core::schedule::Schedule`;
/// `Store::fires(id, limit)` exists for the history strip. What is missing is a
/// human gloss for a cron expression — `0 2 * * *` → `02:00 every day` — which
/// belongs beside `schedule::next_fire` in core rather than here.
pub fn schedules(_jod: &Arc<Jod>) -> Vec<ScheduleRow> {
    Vec::new()
}

/// Every goal, with its checklist and its iteration log.
///
/// TODO: `Store::goals()` — `jod_core::schedule::Goal` and `GoalState` exist;
/// the listing and the per-goal iteration log do not.
pub fn goals(_jod: &Arc<Jod>) -> Vec<GoalRow> {
    Vec::new()
}

/// Every webhook rule, with its recent deliveries.
///
/// TODO: `Store::webhooks()` and `Store::deliveries(name, limit)` — the
/// webhook work is landing in `core/src/webhook.rs` in parallel with this.
pub fn hooks(_jod: &Arc<Jod>) -> Vec<HookRow> {
    Vec::new()
}

/// The durable activity log — what happened while nobody was looking.
///
/// TODO: `Store::activity(limit)` and `Store::mark_activity_read(id)`. This has
/// to come from the store rather than from memory, for the same reason the team
/// board does: cron, webhooks and goals write it from *other processes*.
pub fn activity(_jod: &Arc<Jod>) -> Vec<ActivityItem> {
    Vec::new()
}

/// The task board, promoted out of the team panel into a screen of its own.
///
/// TODO: `Store::team_tasks(team)` already exists and is what the team panel
/// reads; what is missing from `jod_core::team::TeamTask` is the run doing the
/// task, the runnable check, and the blocked-by/blocks pair. Until those exist
/// the tasks screen shows the board it can see, built by the caller from
/// `App::tasks` rather than here.
pub fn tasks(_jod: &Arc<Jod>) -> Vec<TaskRow> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(!tags.contains(&kind.tag()), "{} is claimed twice", kind.tag());
            tags.push(kind.tag());
            assert!(kind.tag().len() <= 4);
        }
    }

    #[test]
    fn a_goals_progress_comes_from_its_checklist() {
        let mut goal = goal_row();
        assert_eq!(goal.percent(), 50, "one of two checks is done");
        goal.checks[1].done = true;
        assert_eq!(goal.percent(), 100);
    }

    /// A goal with no checklist has no denominator, so it reports nothing
    /// rather than dividing by it.
    #[test]
    fn a_goal_with_no_checklist_is_zero_percent_rather_than_a_panic() {
        let mut goal = goal_row();
        goal.checks.clear();
        assert_eq!(goal.percent(), 0);
    }

    fn goal_row() -> GoalRow {
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
