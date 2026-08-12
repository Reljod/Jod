//! Worktrees a session claimed to write in.
//!
//! A session starts pointed at your real checkout, read-only. The moment it
//! needs to change something it claims a lease — a fresh branch and worktree —
//! and that becomes its only *writable* root, with the original still beside it
//! so it can diff against what you are editing.
//!
//! ## Claiming is the agent's step, not Jod's inference
//!
//! "Detect the first write" has no harness-agnostic implementation: every
//! harness spells its pre-write hook differently and two of the three barely
//! have one. So the agent is told plainly that its root is read-only and that
//! it must claim before changing anything, and a watcher on the read-only root
//! is the backstop — a write that lands there anyway raises a card rather than
//! being silently kept. The watcher reports; it does not revert.
//!
//! ## A lease outlives the work that cut it
//!
//! Deleting a work does **not** remove its worktrees or their branches. Jod's
//! records are cheap to recreate and a branch with uncommitted work on it is
//! not — and the moment of deleting a session's history is exactly the moment
//! nobody is left to remember what was on it. So the row survives with a null
//! work, keeping [`Lease::work_title`] so an orphan can still say what it was
//! for; an orphaned lease that cannot explain itself is one nobody dares
//! delete.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Whether a lease is still held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// In use. Offered to a sibling session on the same repository before a
    /// second branch is cut.
    Held,
    /// Given up, but the worktree is still on disk — because it was dirty, or
    /// unmerged, or both.
    Released,
    /// Given up and cleaned off disk. Only reachable when the tree was clean
    /// and merged.
    Removed,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Held => "held",
            State::Released => "released",
            State::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "released" => State::Released,
            "removed" => State::Removed,
            _ => State::Held,
        }
    }
}

/// One claimed worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: i64,
    /// Null once the work has been deleted. The lease deliberately survives it.
    pub work_id: Option<String>,
    /// Remembered so an orphaned lease can still say what it was for.
    pub work_title: String,
    pub conversation_id: Option<String>,
    /// The real checkout this was cut from.
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_ref: String,
    pub state: State,
    pub created_at_ms: i64,
    pub released_at_ms: Option<i64>,
}

/// What a lease looks like on disk right now.
///
/// Read from git at the moment somebody asks, never cached: a lease that was
/// clean an hour ago tells you nothing about whether deleting it now would
/// lose work. This is what the delete refusal prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub worktree_path: PathBuf,
    pub branch: String,
    /// Uncommitted changes present.
    pub dirty: bool,
    /// Every commit on this branch is reachable from its base.
    pub merged: bool,
    /// The worktree directory is gone from disk — somebody removed it by hand.
    pub missing: bool,
}

impl Condition {
    /// Whether removing this would destroy something that cannot be recovered.
    ///
    /// The single question the removal path is allowed to ask. Deliberately
    /// conservative: unmerged *or* dirty is enough to refuse, because the cost
    /// of keeping a stale worktree is a directory and the cost of the mistake
    /// is somebody's afternoon.
    pub fn safe_to_remove(&self) -> bool {
        self.missing || (!self.dirty && self.merged)
    }
}
