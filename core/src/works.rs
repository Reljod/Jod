//! A work: one intent, spanning several conversations.
//!
//! A work is a **group, not a new kind of session**. Nothing in Jod learns a
//! second session type; a project-session is an ordinary conversation with a
//! work attached, and the fleet tree is a self-join over what already exists.
//! That is the decision this whole module hangs off — it is why works could be
//! added without touching how a conversation runs.
//!
//! ## When a work is over
//!
//! *Done* is not a judgement call. A work opens with at least one task — the
//! instruction itself if nothing finer is known — and is [`State::Closed`]
//! when every task on its board is complete. The board is the existing `tasks`
//! table, because claiming there is already one atomic statement and that
//! statement is the reason two agents racing produce one winner.
//!
//! [`State::Finishing`] is tasks done but sessions still running. It exists
//! because "the work is over" and "it is safe to act on the work" are
//! different questions, and only one of them can be answered by counting
//! tasks.

use serde::{Deserialize, Serialize};

/// Where a work is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    /// Every task complete, but at least one session is still running. Not
    /// safe to delete: that is the state where deleting would interrupt an
    /// agent mid-commit.
    Finishing,
    /// Over. The record stays, the tree stays, the worktrees stay. Closing
    /// destroys nothing — deleting is a separate, explicit act.
    Closed,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Open => "open",
            State::Finishing => "finishing",
            State::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "finishing" => State::Finishing,
            "closed" => State::Closed,
            _ => State::Open,
        }
    }

    /// Whether new work should still be routed here.
    pub fn is_live(&self) -> bool {
        matches!(self, State::Open | State::Finishing)
    }
}

/// One intent, and everything Jod remembers about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    /// A cheap model's paraphrase, produced by a throwaway conversation that
    /// is deleted immediately afterwards.
    pub title: String,
    pub summary: String,
    /// What Reljod actually asked for, kept verbatim. The title and summary
    /// are both paraphrases; when they are wrong, this is what says so.
    pub instruction: String,
    /// Distinguishes one work from another at a glance in the tree and on
    /// every cascaded card.
    pub colour: String,
    pub state: State,
    /// Messages this work's agents may exchange before the human is asked
    /// whether to continue. `None` means the default.
    ///
    /// Two agents in a polite loop spend money at machine speed, and the
    /// failure is invisible because every individual message looks reasonable.
    pub message_budget: Option<i64>,
    pub messages_used: i64,
    /// Maximum hops in one thread.
    pub max_depth: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub closed_at_ms: Option<i64>,
}

impl Work {
    /// Remaining message budget, or `None` when unbounded.
    pub fn budget_left(&self) -> Option<i64> {
        self.message_budget.map(|b| (b - self.messages_used).max(0))
    }

    /// Whether the traffic bound has been reached. Hitting it raises a card
    /// and pauses the thread — never the work, and never the sessions.
    pub fn over_budget(&self) -> bool {
        self.budget_left() == Some(0)
    }
}

/// Defaults generous enough that ordinary coordination never sees them.
///
/// The mechanism matters now; the numbers can be wrong and adjusted once there
/// is real traffic to look at. They are deliberately in one place so that
/// tuning them is a single edit and raising them is a visible one — per the
/// spec, changing a bound is an escalation, because this is the money axis.
pub const DEFAULT_MESSAGE_BUDGET: i64 = 200;

/// Maximum hops in a single thread before the humans are asked.
pub const DEFAULT_MAX_DEPTH: i64 = 12;
