//! The directories a conversation may work in.
//!
//! A session can be pointed at several repositories at once, so "where does
//! this agent work" stopped being one path. `conversations.cwd` keeps its old
//! and narrower meaning — the directory the harness process starts in — and
//! this is the set of places it may read, mention and, for exactly one of
//! them, write.
//!
//! ## Read-only is the default, and it is a convention
//!
//! A session opens pointed at your real checkout with [`Root::writable`]
//! false. It writes only in a worktree it claimed for itself (see
//! [`crate::leases`]), and the original stays beside it, readable, so it can
//! still diff against what you are editing.
//!
//! Jod passes a deny rule where a harness supports one, but **nothing here
//! stops a determined agent writing outside its roots**. What the design
//! actually guarantees is narrower and still worth having: work happens on a
//! branch by default, so your checkout is not where a run's half-finished
//! state accumulates. Anything in this module that starts to read like a
//! sandbox is a comment that needs fixing, not a feature that needs adding.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How a root came to be on a conversation, so the UI can explain one nobody
/// remembers adding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Somebody added it — the picker, the CLI, or a launch flag.
    Human,
    /// The conversation's original `cwd`, carried forward when roots were
    /// introduced so no existing conversation lost the directory it had.
    Inherited,
    /// A worktree this session claimed. The only kind that is writable.
    Lease,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Inherited => "inherited",
            Origin::Lease => "lease",
        }
    }

    pub fn parse(s: &str) -> Origin {
        match s {
            "inherited" => Origin::Inherited,
            "lease" => Origin::Lease,
            _ => Origin::Human,
        }
    }
}

/// One directory a conversation may work in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub id: i64,
    pub conversation_id: String,
    pub path: PathBuf,
    /// Whether the agent may change anything here. False for a real checkout;
    /// true only for a claimed worktree.
    pub writable: bool,
    /// The user's order, not the database's. The first root is the one an
    /// unqualified `@mention` resolves against.
    pub position: i64,
    pub origin: Origin,
    pub added_at_ms: i64,
}

impl Root {
    /// Whether `candidate` lies inside this root.
    ///
    /// Used to decide which root a mention belongs to and to label a path in
    /// the picker. Purely lexical over already-normalised paths: this answers
    /// "which root does this belong to", and must never be mistaken for a
    /// permission check — see the module note about sandboxes.
    pub fn contains(&self, candidate: &std::path::Path) -> bool {
        candidate.starts_with(&self.path)
    }

    /// The short label a mention shows when several roots are set, so
    /// `@src/main.rs` is unambiguous across two repositories that both have
    /// one.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }
}

/// A root about to be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRoot {
    pub path: PathBuf,
    pub writable: bool,
    pub origin: Origin,
}

impl NewRoot {
    /// The ordinary case: somewhere to read, added by a person.
    pub fn reading(path: impl Into<PathBuf>) -> NewRoot {
        NewRoot {
            path: path.into(),
            writable: false,
            origin: Origin::Human,
        }
    }
}
