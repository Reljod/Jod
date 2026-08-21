//! The fleet as a tree: what is visible, where the cursor is, and how the
//! guides are drawn.
//!
//! The *flatten* is core's — [`jod_core::tree::Store::forest_of`] returns every
//! node with its depth and parent already worked out. What is left here is the
//! part that genuinely needs the screen: which of those rows are on it, which
//! row the cursor is on, and what the guide column looks like beside each one.
//!
//! ## Selection is an id, never an index
//!
//! Said twice — once in core and once here — because the tree reshapes *while
//! you are looking at it*. Runs finish, sessions spawn children, a work closes
//! and sorts itself downward, and the refresh happens on a tick nobody pressed.
//! An index survives none of that, and the failure is the worst kind: the
//! cursor silently points at a different row than the one you were reading when
//! you pressed enter.
//!
//! ## Expansion is a default plus two exceptions
//!
//! Everything is expanded unless you collapsed it, except a **closed** work,
//! which is collapsed unless you expanded it. That inversion is E5.S3b: a
//! closed work is an archive, and a tree that shows every archive by default
//! becomes a list of everything ever done — which is the state that makes
//! people stop reading it.

use std::collections::HashSet;

use jod_core::tree::{Node, NodeId, NodeKind};

/// The pinned main chat, as a row the tree's cursor can sit on.
///
/// A sentinel rather than a node, because there is no node to be had: the
/// forest is works and what hangs off them — core's query is
/// `WHERE c.work_id IS NOT NULL` — and the main chat belongs to no work. The
/// flat list has pinned it above the agents since it existed; without the same
/// row here, the fleet becomes a screen you can walk into and not back out of
/// the moment a single work exists, because the tree replaces the list whole.
///
/// Its own `kind_tag`, so it can collide with nothing: [`NodeId`] compares on
/// the tag as well as the id, and `main` is not a kind core mints.
pub fn main_id() -> NodeId {
    NodeId {
        kind_tag: "main",
        id: super::app::MAIN_ROW.to_string(),
    }
}

/// A run that belongs to no work, as a row the tree's cursor can sit on.
///
/// The same trick as [`main_id`] and for the same reason: core's forest is
/// `WHERE c.work_id IS NOT NULL`, so a run started by `delegate` has no node to
/// be, and the pane below the tree draws it anyway because a run nothing on
/// screen accounts for is a run nobody stops.
///
/// Giving it an id makes it *reachable*. Before this the pane was drawn and
/// nothing else: `↓` stopped at the last node of the tree, no row in it could
/// ever be highlighted, and every verb the keybar advertises — watch, stop,
/// attach — read the cursor, found no node, and did nothing without saying so.
/// One cursor over both panes is what makes the lower one a list rather than a
/// picture of a list.
///
/// Its own `kind_tag`, so it can collide with nothing: [`NodeId`] compares on
/// the tag as well as the id, and a loose run's id is also the id of the run
/// node it *would* have had if its conversation had belonged to a work.
pub fn loose_id(run: &str) -> NodeId {
    NodeId {
        kind_tag: "loose",
        id: run.to_string(),
    }
}

/// Is this row one of the runs drawn below the tree?
pub fn is_loose(id: &NodeId) -> bool {
    id.kind_tag == "loose"
}

/// Everything the fleet tree remembers between frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeState {
    /// The cursor, held as a [`NodeId`].
    pub selected: Option<NodeId>,
    /// Nodes the user explicitly collapsed.
    pub collapsed: HashSet<NodeId>,
    /// Closed works the user explicitly expanded, which is the exception to
    /// their being collapsed by default.
    pub opened: HashSet<NodeId>,
    /// Whether closed works appear at all. Off hides them entirely; on shows
    /// them collapsed, below the live ones.
    pub show_closed: bool,
}

impl TreeState {
    /// Whether this node's children are on screen.
    ///
    /// `closed` is the set of works core returned from its *closed* query, so
    /// "is this an archive" is core's answer rather than something inferred
    /// from a label here.
    pub fn is_expanded(&self, id: &NodeId, closed: &HashSet<NodeId>) -> bool {
        if self.collapsed.contains(id) {
            return false;
        }
        if closed.contains(id) {
            return self.opened.contains(id);
        }
        true
    }

    /// The rows actually on screen, as indices into `nodes`.
    ///
    /// Three passes, in this order, and the order matters: filter first so the
    /// cursor cannot land on something hidden, then collapse, because a
    /// collapsed node's children must go even when they match.
    pub fn visible(
        &self,
        nodes: &[Node],
        closed: &HashSet<NodeId>,
        needle: Option<&str>,
    ) -> Vec<usize> {
        let keep = matching(nodes, needle);
        let mut out = Vec::new();
        // The ancestors that are collapsed. A node is hidden when *any* of them
        // is, so this carries down the flattened list rather than being asked
        // per node — the list is depth-ordered, so a collapsed parent is always
        // seen before the children it hides.
        let mut hidden_below: Option<usize> = None;
        for (at, node) in nodes.iter().enumerate() {
            if let Some(depth) = hidden_below {
                if node.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            if !keep.contains(&at) {
                continue;
            }
            out.push(at);
            if node.has_children && !self.is_expanded(&node.id, closed) {
                hidden_below = Some(node.depth);
            }
        }
        out
    }

    /// Which nodes survive the filter — every hit, **plus every ancestor of a
    /// hit**.
    ///
    /// The ancestors are the point. A tree filter that dropped them would leave
    /// matching rows floating at a depth with nothing above them, which reads
    /// as a rendering fault rather than as a filter; and the path to a hit is
    /// usually what you were looking for anyway.
    pub fn row_ids(
        &self,
        nodes: &[Node],
        closed: &HashSet<NodeId>,
        needle: Option<&str>,
    ) -> Vec<NodeId> {
        self.visible(nodes, closed, needle)
            .into_iter()
            .map(|at| nodes[at].id.clone())
            .collect()
    }
}

/// Which nodes survive the filter — every hit, **plus every ancestor of a
/// hit**.
///
/// The ancestors are the point. A tree filter that dropped them would leave
/// matching rows floating at a depth with nothing above them, which reads as a
/// rendering fault rather than as a filter; and the path to a hit is usually
/// what you were looking for anyway.
fn matching(nodes: &[Node], needle: Option<&str>) -> HashSet<usize> {
        let needle = match needle {
            Some(n) if !n.trim().is_empty() => n.to_string(),
            // No filter, or one that is open but empty: nothing is hidden.
            _ => return (0..nodes.len()).collect(),
        };
        let mut keep: HashSet<usize> = HashSet::new();
        for (at, node) in nodes.iter().enumerate() {
            if !super::workspace::matches(&needle, &format!("{} {}", node.label, node.summary)) {
                continue;
            }
            keep.insert(at);
            // Walk up by id rather than by scanning backwards for a shallower
            // depth: two sessions of different works can sit at the same depth,
            // and the nearest shallower row is not always the parent.
            let mut parent = node.parent.clone();
            while let Some(id) = parent {
                let Some(up) = nodes.iter().position(|n| n.id == id) else {
                    break;
                };
                if !keep.insert(up) {
                    // Already kept, so its ancestors are too.
                    break;
                }
                parent = nodes[up].parent.clone();
            }
        }
        keep
}

impl TreeState {
    pub fn index(&self, rows: &[NodeId]) -> usize {
        self.selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| row == id))
            .unwrap_or(0)
    }

    /// Keep the cursor on a row that still exists.
    pub fn reconcile(&mut self, rows: &[NodeId]) {
        self.reconcile_to(rows, None);
    }

    /// [`TreeState::reconcile`], but landing somewhere other than the top row
    /// when the cursor has nowhere to go.
    ///
    /// The same rule the flat list follows and for the same reason — see
    /// `ListState::reconcile_to`. The tree's first row is [`main_id`], and a
    /// cursor that defaulted there would put every verb this screen has — stop,
    /// attach, watch, toggle — one keystroke away from the thing they are for.
    /// The chat is *drawn* first because it is the anchor; the cursor starts on
    /// the work, because managing the work is what opening this screen means.
    pub fn reconcile_to(&mut self, rows: &[NodeId], fallback: Option<NodeId>) {
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        if !self
            .selected
            .as_ref()
            .is_some_and(|id| rows.iter().any(|row| row == id))
        {
            self.selected = fallback
                .filter(|id| rows.contains(id))
                .or_else(|| rows.first().cloned());
        }
    }

    /// Clamped rather than wrapping, like every other cursor here: in a list
    /// that reshapes under you, overshooting lands somewhere unrelated.
    pub fn step(&mut self, delta: isize, rows: &[NodeId]) {
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        let at = self.index(rows) as isize;
        let landed = (at + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = Some(rows[landed].clone());
    }

    pub fn first(&mut self, rows: &[NodeId]) {
        self.selected = rows.first().cloned();
    }

    pub fn last(&mut self, rows: &[NodeId]) {
        self.selected = rows.last().cloned();
    }

    /// Space: expand what is collapsed, collapse what is not.
    pub fn toggle(&mut self, closed: &HashSet<NodeId>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.is_expanded(&id, closed) {
            self.opened.remove(&id);
            self.collapsed.insert(id);
        } else {
            self.collapsed.remove(&id);
            self.opened.insert(id);
        }
    }

    /// `→` — open a closed node, or move onto its first child if it is already
    /// open.
    ///
    /// One key doing both is what makes the arrows feel like a tree rather than
    /// like a list with extra verbs: right always means "further in", and what
    /// that costs depends only on where you already were.
    pub fn expand_or_descend(&mut self, nodes: &[Node], closed: &HashSet<NodeId>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(node) = nodes.iter().find(|n| n.id == id) else {
            return;
        };
        if !node.has_children {
            return;
        }
        if !self.is_expanded(&id, closed) {
            self.collapsed.remove(&id);
            self.opened.insert(id);
            return;
        }
        // Already open: the first child is the next visible row, because the
        // flatten is depth-first. Unfiltered on purpose — descending is about
        // the tree's shape, and a filter narrows what is listed rather than
        // what is inside a node.
        let rows = self.row_ids(nodes, closed, None);
        let at = self.index(&rows);
        if let Some(next) = rows.get(at + 1) {
            self.selected = Some(next.clone());
        }
    }

    /// `←` — close an open node, or jump to its parent if it is already closed.
    ///
    /// The mirror of `→`, and the reason a deep tree never needs `Home`: left
    /// repeatedly always arrives at the top.
    pub fn collapse_or_parent(&mut self, nodes: &[Node], closed: &HashSet<NodeId>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(node) = nodes.iter().find(|n| n.id == id) else {
            return;
        };
        if node.has_children && self.is_expanded(&id, closed) {
            self.opened.remove(&id);
            self.collapsed.insert(id);
            return;
        }
        if let Some(parent) = node.parent.clone() {
            self.selected = Some(parent);
        }
    }

    pub fn expand_all(&mut self, nodes: &[Node]) {
        self.collapsed.clear();
        for node in nodes {
            self.opened.insert(node.id.clone());
        }
    }

    pub fn collapse_all(&mut self, nodes: &[Node]) {
        self.opened.clear();
        for node in nodes.iter().filter(|n| n.has_children) {
            self.collapsed.insert(node.id.clone());
        }
    }
}

/// The guide column beside one visible row.
///
/// Built from the *visible* rows rather than from the whole forest, because a
/// guide has to describe the tree that is on screen — an elbow drawn from a
/// hidden sibling points at nothing.
///
/// `ascii` is not a preference. Box-drawing characters need a font that has
/// them and a terminal that is not mangling the encoding, and over ssh to a
/// stranger's box neither is safe to assume; the fallback keeps the shape
/// legible when they are wrong.
pub fn guides(rows: &[&Node], at: usize, ascii: bool) -> String {
    let (vertical, gap, tee, elbow) = if ascii {
        ("|  ", "   ", "+- ", "`- ")
    } else {
        ("│  ", "   ", "├─ ", "└─ ")
    };
    let node = rows[at];
    if node.depth == 0 {
        return String::new();
    }
    let mut prefix = String::new();
    // One cell per ancestor level: a bar if that level has another child still
    // to come further down the screen, blank if this is the last of them.
    for level in 0..node.depth.saturating_sub(1) {
        prefix.push_str(if more_at(rows, at, level) { vertical } else { gap });
    }
    prefix.push_str(if more_at(rows, at, node.depth - 1) {
        tee
    } else {
        elbow
    });
    prefix
}

/// Is there another row at `depth + 1` below `at`, before the tree climbs back
/// out past `depth`?
///
/// That question is what decides a `├` from a `└`, and it is asked of the
/// flattened visible list rather than of the parent's child count — which is
/// the same answer only when nothing is filtered.
fn more_at(rows: &[&Node], at: usize, depth: usize) -> bool {
    for row in rows.iter().skip(at + 1) {
        if row.depth <= depth {
            return false;
        }
        if row.depth == depth + 1 {
            return true;
        }
    }
    false
}

/// The marker that says whether a row has more inside it.
pub fn marker(node: &Node, expanded: bool) -> &'static str {
    if !node.has_children {
        return "  ";
    }
    if expanded {
        "▾ "
    } else {
        "▸ "
    }
}

/// What a node's kind is called in the gutter, so the tree reads without
/// colour.
pub fn kind_glyph(kind: NodeKind) -> &'static str {
    match kind {
        // Solid, and the widest of the set: a project is the outermost group,
        // and the gutter is what says so on a screen with no colour.
        NodeKind::Project => "▪",
        // A manager is a conversation, so it takes the session's shape filled
        // in — the same kind of thing, and the one that is always there.
        NodeKind::Manager => "◆",
        NodeKind::Work => "■",
        NodeKind::Session => "◇",
        NodeKind::Run => "·",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: NodeId, parent: Option<NodeId>, kind: NodeKind, depth: usize, label: &str) -> Node {
        Node {
            id,
            parent,
            kind,
            depth,
            label: label.into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            colour: "cyan".into(),
            expanded: true,
            has_children: false,
        }
    }

    /// A work with two sessions, the first of which has a run under it.
    fn forest() -> Vec<Node> {
        let mut work = node(NodeId::work("w1"), None, NodeKind::Work, 0, "the parser");
        work.has_children = true;
        let mut first = node(
            NodeId::session("s1"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            1,
            "port the lexer",
        );
        first.has_children = true;
        let run = node(
            NodeId::run("r1"),
            Some(NodeId::session("s1")),
            NodeKind::Run,
            2,
            "run one",
        );
        let second = node(
            NodeId::session("s2"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            1,
            "write the docs",
        );
        vec![work, first, run, second]
    }

    fn nothing() -> HashSet<NodeId> {
        HashSet::new()
    }

    #[test]
    fn everything_is_visible_until_something_is_collapsed() {
        let nodes = forest();
        let tree = TreeState::default();
        assert_eq!(tree.visible(&nodes, &nothing(), None).len(), 4);
    }

    #[test]
    fn collapsing_a_node_hides_everything_under_it() {
        let nodes = forest();
        let mut tree = TreeState::default();
        tree.collapsed.insert(NodeId::session("s1"));
        let rows = tree.row_ids(&nodes, &nothing(), None);
        assert_eq!(
            rows,
            vec![NodeId::work("w1"), NodeId::session("s1"), NodeId::session("s2")],
            "the run under s1 is gone, s2 is not"
        );
    }

    #[test]
    fn collapsing_the_root_hides_the_whole_subtree() {
        let nodes = forest();
        let mut tree = TreeState::default();
        tree.collapsed.insert(NodeId::work("w1"));
        assert_eq!(tree.row_ids(&nodes, &nothing(), None), vec![NodeId::work("w1")]);
    }

    /// E5.S3b: an archive is collapsed until asked for, or the tree becomes a
    /// list of everything ever done.
    #[test]
    fn a_closed_work_is_collapsed_until_it_is_opened() {
        let nodes = forest();
        let closed: HashSet<NodeId> = [NodeId::work("w1")].into_iter().collect();
        let mut tree = TreeState::default();
        assert_eq!(tree.row_ids(&nodes, &closed, None), vec![NodeId::work("w1")]);

        tree.selected = Some(NodeId::work("w1"));
        tree.expand_or_descend(&nodes, &closed);
        assert_eq!(tree.row_ids(&nodes, &closed, None).len(), 4, "opened by hand");
    }

    /// The cursor follows the *node*, which is the whole reason it is an id.
    #[test]
    fn the_cursor_stays_on_its_node_when_the_tree_reshapes() {
        let nodes = forest();
        let mut tree = TreeState::default();
        let rows = tree.row_ids(&nodes, &nothing(), None);
        tree.reconcile(&rows);
        tree.step(3, &rows);
        assert_eq!(tree.selected, Some(NodeId::session("s2")));

        // A run finishes and disappears from above the cursor.
        let mut fewer = forest();
        fewer.remove(2);
        let rows = tree.row_ids(&fewer, &nothing(), None);
        tree.reconcile(&rows);
        assert_eq!(
            tree.selected,
            Some(NodeId::session("s2")),
            "still on the same session, at a different index"
        );
        assert_eq!(tree.index(&rows), 2);
    }

    #[test]
    fn a_node_that_disappeared_puts_the_cursor_on_the_top_row() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::run("gone")),
            ..Default::default()
        };
        let rows = tree.row_ids(&nodes, &nothing(), None);
        tree.reconcile(&rows);
        assert_eq!(tree.selected, Some(NodeId::work("w1")));
    }

    /// Right always means "further in": open what is shut, descend what is
    /// open.
    #[test]
    fn right_opens_a_shut_node_and_descends_an_open_one() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::work("w1")),
            ..Default::default()
        };
        tree.collapsed.insert(NodeId::work("w1"));

        tree.expand_or_descend(&nodes, &nothing());
        assert_eq!(tree.selected, Some(NodeId::work("w1")), "it only opened");
        assert_eq!(tree.row_ids(&nodes, &nothing(), None).len(), 4);

        tree.expand_or_descend(&nodes, &nothing());
        assert_eq!(
            tree.selected,
            Some(NodeId::session("s1")),
            "now it moves in"
        );
    }

    /// Left is the mirror, which is why a deep tree never needs a Home key.
    #[test]
    fn left_shuts_an_open_node_and_climbs_from_a_shut_one() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::session("s1")),
            ..Default::default()
        };

        tree.collapse_or_parent(&nodes, &nothing());
        assert_eq!(tree.selected, Some(NodeId::session("s1")), "it only shut");
        assert_eq!(tree.row_ids(&nodes, &nothing(), None).len(), 3);

        tree.collapse_or_parent(&nodes, &nothing());
        assert_eq!(tree.selected, Some(NodeId::work("w1")), "now it climbs");
    }

    /// A leaf has nothing to shut, so left climbs immediately.
    #[test]
    fn left_on_a_leaf_climbs_straight_to_the_parent() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::run("r1")),
            ..Default::default()
        };
        tree.collapse_or_parent(&nodes, &nothing());
        assert_eq!(tree.selected, Some(NodeId::session("s1")));
    }

    #[test]
    fn space_toggles_whichever_way_the_node_is() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::session("s1")),
            ..Default::default()
        };
        tree.toggle(&nothing());
        assert_eq!(tree.row_ids(&nodes, &nothing(), None).len(), 3);
        tree.toggle(&nothing());
        assert_eq!(tree.row_ids(&nodes, &nothing(), None).len(), 4);
    }

    #[test]
    fn expand_all_and_collapse_all_reach_both_ends() {
        let nodes = forest();
        let mut tree = TreeState::default();
        tree.collapse_all(&nodes);
        assert_eq!(tree.row_ids(&nodes, &nothing(), None), vec![NodeId::work("w1")]);
        tree.expand_all(&nodes);
        assert_eq!(tree.row_ids(&nodes, &nothing(), None).len(), 4);
    }

    /// The filter's defining property: a hit deep in the tree keeps the path to
    /// it, or the matching row floats at a depth with nothing above it.
    #[test]
    fn filtering_keeps_every_ancestor_of_every_hit() {
        let nodes = forest();
        let tree = TreeState::default();
        let rows = tree.row_ids(&nodes, &nothing(), Some("docs"));
        assert_eq!(
            rows,
            vec![NodeId::work("w1"), NodeId::session("s2")],
            "the work is kept because its child matched"
        );
    }

    #[test]
    fn filtering_a_grandchild_keeps_both_levels_above_it() {
        let mut nodes = forest();
        nodes[2].label = "cargo test".into();
        let tree = TreeState::default();
        assert_eq!(
            tree.row_ids(&nodes, &nothing(), Some("cargo")),
            vec![
                NodeId::work("w1"),
                NodeId::session("s1"),
                NodeId::run("r1")
            ]
        );
    }

    #[test]
    fn a_filter_matching_nothing_empties_the_tree() {
        let nodes = forest();
        let tree = TreeState::default();
        assert!(tree.row_ids(&nodes, &nothing(), Some("zzzz")).is_empty());
    }

    /// An open but empty filter hides nothing, as everywhere else here.
    #[test]
    fn an_open_but_empty_filter_hides_nothing() {
        let nodes = forest();
        let tree = TreeState::default();
        assert_eq!(
            tree.row_ids(&nodes, &nothing(), Some("  ")).len(),
            4,
            "an open but empty filter hides nothing"
        );
    }

    /// A collapsed parent wins over a matching child: the filter says what is
    /// interesting, and the collapse says what is on screen.
    #[test]
    fn a_collapsed_node_hides_its_matching_children() {
        let nodes = forest();
        let mut tree = TreeState::default();
        tree.collapsed.insert(NodeId::work("w1"));
        assert_eq!(
            tree.row_ids(&nodes, &nothing(), Some("docs")),
            vec![NodeId::work("w1")]
        );
    }

    /// The guides describe the tree *on screen*: the last child gets an elbow,
    /// the ones before it get a tee, and a deeper row carries a bar for every
    /// ancestor that still has siblings to come.
    #[test]
    fn the_guides_elbow_the_last_child_and_tee_the_rest() {
        let nodes = forest();
        let tree = TreeState::default();
        let rows: Vec<&Node> = tree
            .visible(&nodes, &nothing(), None)
            .into_iter()
            .map(|at| &nodes[at])
            .collect();

        assert_eq!(guides(&rows, 0, false), "", "a root has no guide");
        assert_eq!(guides(&rows, 1, false), "├─ ", "s1 has s2 after it");
        assert_eq!(
            guides(&rows, 2, false),
            "│  └─ ",
            "the run is s1's last child, under a work that still has s2"
        );
        assert_eq!(guides(&rows, 3, false), "└─ ", "s2 is the last child");
    }

    /// Over ssh to a stranger's box, neither the font nor the encoding is safe
    /// to assume — so the shape has to survive without box-drawing characters.
    #[test]
    fn the_guides_have_an_ascii_shape_of_the_same_width() {
        let nodes = forest();
        let tree = TreeState::default();
        let rows: Vec<&Node> = tree
            .visible(&nodes, &nothing(), None)
            .into_iter()
            .map(|at| &nodes[at])
            .collect();
        for at in 0..rows.len() {
            let unicode = guides(&rows, at, false);
            let ascii = guides(&rows, at, true);
            assert_eq!(
                unicode.chars().count(),
                ascii.chars().count(),
                "row {at} changes width between the two alphabets"
            );
            assert!(ascii.is_ascii(), "row {at}: {ascii}");
        }
        assert_eq!(guides(&rows, 2, true), "|  `- ");
    }

    /// A guide is drawn from what is visible. An elbow inherited from a
    /// filtered-out sibling would point at nothing.
    #[test]
    fn the_guides_follow_the_filter_rather_than_the_whole_forest() {
        let nodes = forest();
        let tree = TreeState::default();
        let rows: Vec<&Node> = tree
            .visible(&nodes, &nothing(), Some("lexer"))
            .into_iter()
            .map(|at| &nodes[at])
            .collect();
        assert_eq!(rows.len(), 2, "the work and the session that matched");
        assert_eq!(
            guides(&rows, 1, false),
            "└─ ",
            "s2 is filtered out, so s1 is now the last child on screen"
        );
    }

    #[test]
    fn the_marker_says_whether_there_is_more_inside() {
        let nodes = forest();
        assert_eq!(marker(&nodes[0], true), "▾ ");
        assert_eq!(marker(&nodes[0], false), "▸ ");
        assert_eq!(marker(&nodes[2], true), "  ", "a leaf has no marker");
    }
}
