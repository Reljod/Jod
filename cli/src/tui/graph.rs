//! The memory local graph: one node, its neighbours, and where you have been.
//!
//! This is the answer to "how do you draw a graph in a terminal": you don't.
//! Terminal rows are the scarce resource, and a node-link drawing that is
//! impressive at twenty nodes is unusable at two hundred — in a terminal or out
//! of it. So there is no layout algorithm here, no edge crossings and no zoom:
//! one focus node, incoming edges above, outgoing below, and re-centring on a
//! single keypress.
//!
//! The state that makes it navigable is the **visit stack**. `⏎` re-centres and
//! pushes where you were; `Backspace` pops it. Walking a graph without being
//! able to walk back out of it is how you get lost in one.

use super::data::{MemoryEdge, MemoryNode};

/// Which way an edge points, from the focus node's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Someone points at the focus — drawn above it.
    In,
    /// The focus points at someone — drawn below it.
    Out,
}

/// One row of the neighbour list, which `↑↓` walks and `⏎` re-centres on.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub direction: Direction,
    pub edge: MemoryEdge,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphView {
    /// The node in the middle.
    pub focus: String,
    /// Where you have been, oldest first. The focus is *not* in it.
    pub trail: Vec<String>,
    /// Which neighbour row is highlighted.
    pub sel: usize,
    /// 1 or 2. Two hops is a different question — "what is this near?" — and
    /// costs rows, so it is a key rather than the default.
    pub hops: u8,
    /// Show only edges of this kind; `None` is all of them.
    pub edge_kind: Option<String>,
}

impl GraphView {
    pub fn new(focus: impl Into<String>) -> GraphView {
        GraphView {
            focus: focus.into(),
            trail: Vec::new(),
            sel: 0,
            hops: 1,
            edge_kind: None,
        }
    }

    /// Move to a neighbour, remembering where you came from.
    pub fn recentre(&mut self, on: impl Into<String>) {
        let on = on.into();
        if on == self.focus {
            return;
        }
        self.trail.push(std::mem::replace(&mut self.focus, on));
        self.sel = 0;
    }

    /// Walk back one step. `false` when there is nowhere left to go, which is
    /// what tells the caller to leave the graph rather than sit still.
    pub fn back(&mut self) -> bool {
        match self.trail.pop() {
            Some(previous) => {
                self.focus = previous;
                self.sel = 0;
                true
            }
            None => false,
        }
    }

    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    /// One hop shows what a node is directly about; two shows what it is near.
    pub fn toggle_hops(&mut self) {
        self.hops = if self.hops == 2 { 1 } else { 2 };
    }

    /// Cycle the edge-kind filter through the kinds actually present, then off
    /// again — offering a kind the focus node has none of would filter the
    /// screen to nothing and look broken.
    pub fn cycle_edge_kind(&mut self, present: &[String]) {
        if present.is_empty() {
            self.edge_kind = None;
            return;
        }
        let next = match &self.edge_kind {
            None => Some(present[0].clone()),
            Some(current) => match present.iter().position(|k| k == current) {
                Some(at) if at + 1 < present.len() => Some(present[at + 1].clone()),
                _ => None,
            },
        };
        self.edge_kind = next;
        self.sel = 0;
    }

    /// The breadcrumb along the bottom: where you have been, ending on where
    /// you are.
    pub fn trail_line(&self) -> String {
        self.trail
            .iter()
            .chain(std::iter::once(&self.focus))
            .map(|name| format!("⟨ {name} ⟩"))
            .collect::<Vec<_>>()
            .join("  ")
    }
}

/// The focus node's neighbours, incoming first, filtered by edge kind.
pub fn neighbours(node: &MemoryNode, kind: Option<&str>) -> Vec<Neighbour> {
    let keep = |edge: &MemoryEdge| kind.is_none_or(|k| edge.kind == k);
    node.in_edges
        .iter()
        .filter(|e| keep(e))
        .map(|edge| Neighbour {
            direction: Direction::In,
            edge: edge.clone(),
        })
        .chain(
            node.out_edges
                .iter()
                .filter(|e| keep(e))
                .map(|edge| Neighbour {
                    direction: Direction::Out,
                    edge: edge.clone(),
                }),
        )
        .collect()
}

/// Every edge kind on this node, in the order they appear, without repeats.
pub fn edge_kinds(node: &MemoryNode) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for edge in node.in_edges.iter().chain(&node.out_edges) {
        if !kinds.contains(&edge.kind) {
            kinds.push(edge.kind.clone());
        }
    }
    kinds
}

/// The header line — `hop 1 shows 5 of 17 edges`.
///
/// A pane that silently truncates leaves you believing you have seen a node's
/// whole neighbourhood, which is exactly the wrong belief to hold about a graph.
pub fn coverage(hops: u8, shown: usize, total: usize) -> String {
    if shown >= total {
        return format!("hop {hops} shows all {total} edges.");
    }
    format!("hop {hops} shows {shown} of {total} edges.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::MemoryKind;

    fn edge(kind: &str, other: &str) -> MemoryEdge {
        MemoryEdge {
            kind: kind.into(),
            other: other.into(),
            other_name: other.into(),
            other_kind: MemoryKind::Belief,
            warn: false,
        }
    }

    fn node() -> MemoryNode {
        MemoryNode {
            id: "prefers-spec-first".into(),
            name: "prefers-spec-first".into(),
            kind: MemoryKind::Belief,
            confidence: 0.86,
            degree: 17,
            age_ms: 0,
            seen: 23,
            body: "Non-trivial work starts with a spec.".into(),
            contradicted: false,
            in_edges: vec![
                edge("supports", "linear-is-truth"),
                edge("refines", "how-to-open-a-pr"),
            ],
            out_edges: vec![edge("contradicts", "ship-fast-iterate")],
            provenance: vec![],
        }
    }

    #[test]
    fn incoming_edges_come_before_outgoing_ones() {
        let rows = neighbours(&node(), None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].direction, Direction::In);
        assert_eq!(rows[2].direction, Direction::Out);
        assert_eq!(rows[2].edge.other, "ship-fast-iterate");
    }

    #[test]
    fn filtering_by_edge_kind_keeps_only_that_kind() {
        let rows = neighbours(&node(), Some("supports"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].edge.other, "linear-is-truth");
    }

    /// Walking a graph without being able to walk back out of it is how you get
    /// lost in one.
    #[test]
    fn re_centring_pushes_the_old_focus_onto_the_visit_stack() {
        let mut view = GraphView::new("a");
        view.recentre("b");
        view.recentre("c");
        assert_eq!(view.focus, "c");
        assert_eq!(view.trail, vec!["a".to_string(), "b".to_string()]);

        assert!(view.back());
        assert_eq!(view.focus, "b");
        assert!(view.back());
        assert_eq!(view.focus, "a");
        assert!(!view.back(), "an empty stack tells the caller to leave");
        assert_eq!(view.focus, "a", "and leaves the focus alone");
    }

    /// Re-centring on the node you are already on would push a duplicate and
    /// make `Backspace` a key that appears not to work.
    #[test]
    fn re_centring_on_the_current_focus_changes_nothing() {
        let mut view = GraphView::new("a");
        view.recentre("a");
        assert!(view.trail.is_empty());
    }

    #[test]
    fn re_centring_puts_the_cursor_back_on_the_first_neighbour() {
        let mut view = GraphView::new("a");
        view.sel = 4;
        view.recentre("b");
        assert_eq!(view.sel, 0);
    }

    #[test]
    fn the_neighbour_cursor_clamps_rather_than_wrapping() {
        let mut view = GraphView::new("a");
        view.step(-1, 3);
        assert_eq!(view.sel, 0);
        view.step(9, 3);
        assert_eq!(view.sel, 2);
        view.step(1, 0);
        assert_eq!(view.sel, 0, "an unconnected node has nothing to walk");
    }

    #[test]
    fn hops_toggle_between_one_and_two() {
        let mut view = GraphView::new("a");
        assert_eq!(view.hops, 1);
        view.toggle_hops();
        assert_eq!(view.hops, 2);
        view.toggle_hops();
        assert_eq!(view.hops, 1);
    }

    /// Offering a kind the node has none of would filter the screen to nothing
    /// and read as a bug.
    #[test]
    fn the_edge_filter_only_offers_kinds_the_node_actually_has() {
        let kinds = edge_kinds(&node());
        assert_eq!(kinds, vec!["supports", "refines", "contradicts"]);

        let mut view = GraphView::new("prefers-spec-first");
        view.cycle_edge_kind(&kinds);
        assert_eq!(view.edge_kind.as_deref(), Some("supports"));
        view.cycle_edge_kind(&kinds);
        view.cycle_edge_kind(&kinds);
        assert_eq!(view.edge_kind.as_deref(), Some("contradicts"));
        view.cycle_edge_kind(&kinds);
        assert_eq!(view.edge_kind, None, "and back to all of them");
    }

    #[test]
    fn cycling_the_edge_filter_on_an_unconnected_node_is_harmless() {
        let mut view = GraphView::new("lonely");
        view.cycle_edge_kind(&[]);
        assert_eq!(view.edge_kind, None);
    }

    #[test]
    fn the_trail_ends_on_where_you_are_now() {
        let mut view = GraphView::new("reljod");
        view.recentre("linear-is-truth");
        view.recentre("prefers-spec-first");
        let line = view.trail_line();
        assert_eq!(
            line,
            "⟨ reljod ⟩  ⟨ linear-is-truth ⟩  ⟨ prefers-spec-first ⟩"
        );
    }

    /// A pane that silently truncates leaves you believing you have seen the
    /// whole neighbourhood.
    #[test]
    fn a_truncated_neighbourhood_says_how_much_it_is_hiding() {
        assert_eq!(coverage(1, 5, 17), "hop 1 shows 5 of 17 edges.");
        assert_eq!(coverage(2, 17, 17), "hop 2 shows all 17 edges.");
    }
}
