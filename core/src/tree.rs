//! The forest: works, the sessions under them, and the runs under those.
//!
//! The fleet screen renders this, but none of it is drawing — flattening a
//! tree, deciding what is visible, and holding a selection across a reshape
//! are logic, and logic that needs a terminal to be tested is logic in the
//! wrong place.
//!
//! ## Selection is held by id, never by index
//!
//! The tree reshapes while you are looking at it: runs finish, sessions spawn
//! children, a work closes and sorts itself downward. An index survives none
//! of that, and the failure mode is the worst kind — the cursor silently
//! points at a different row than the one the user was looking at when they
//! pressed enter.

use serde::{Deserialize, Serialize};

/// What a row in the tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Work,
    /// A conversation. A session that spawned children has them beneath it,
    /// which is what makes the tree deeper than two levels.
    Session,
    Run,
}

/// A stable identity for a row, so a selection survives the tree being rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId {
    pub kind_tag: &'static str,
    pub id: String,
}

impl NodeId {
    pub fn work(id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "work", id: id.into() }
    }
    pub fn session(id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "session", id: id.into() }
    }
    pub fn run(id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "run", id: id.into() }
    }
}

/// One row, already flattened for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    /// How deep, for the tree guides.
    pub depth: usize,
    pub label: String,
    /// The newest message or tool call — no extra model call, refreshed on the
    /// existing tick and off the render path.
    pub summary: String,
    pub running: bool,
    /// Open cards anywhere in this node's subtree, so the tree says where the
    /// questions are without being expanded.
    pub cards: usize,
    /// Open cards in the subtree that are blocking.
    pub blocked: usize,
    /// The owning work's colour, for tinting the row.
    pub colour: String,
    pub expanded: bool,
    pub has_children: bool,
}

impl Node {
    /// Whether this row should draw an expansion marker at all.
    pub fn is_expandable(&self) -> bool {
        self.has_children
    }
}
