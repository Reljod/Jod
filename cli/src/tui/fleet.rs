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
//! ## The tree is two levels: a project, and the agents in it
//!
//! Core's forest is the whole truth — a project holds works, a work holds the
//! session leading it, that session holds the sessions it spawned, and each of
//! those holds its runs. Five levels is the right model and the wrong screen.
//! What you want from the fleet is who is working on this repository right now,
//! and that question was three expansions deep.
//!
//! So [`jod_core::tree::condense`] folds the middle away before anything is
//! drawn, and it does that in core rather than here because the browser draws
//! the same two levels — see its documentation for what is kept and what is
//! reachable elsewhere. What is left in this module is the part that genuinely
//! needs a screen: which rows are on it, where the cursor is, and the guides.
//!
//! ## Expansion is a default plus three exceptions
//!
//! Everything is expanded unless you collapsed it, except a **project** and a
//! **closed** work, which are collapsed unless you expanded them.
//!
//! A closed work is an archive, and a tree that shows every archive by default
//! becomes a list of everything ever done — the state that makes people stop
//! reading it (E5.S3b). A project is shut for the neighbouring reason: with
//! every agent in every repository open at once, the screen is a wall of rows
//! and the one repository you came to look at is somewhere in it. Shut, the
//! fleet opens as the list of repositories, and one keystroke opens the one you
//! want.

use std::collections::HashSet;

use jod_core::tree::{Node, NodeId, NodeKind};

/// The pinned main chat, as a row the tree's cursor can sit on — **when core
/// has not already given it one**.
///
/// A sentinel rather than a node, because for a long time there was no node to
/// be had: the forest was works and what hangs off them — core's query is
/// `WHERE c.work_id IS NOT NULL` — and the main chat belongs to no work. The
/// flat list has pinned it above the agents since it existed; without the same
/// row here, the fleet becomes a screen you can walk into and not back out of
/// the moment a single work exists, because the tree replaces the list whole.
///
/// `forest_of` emits a real [`NodeKind::Main`] row now, carrying the
/// conversation id, its runs and its liveness — none of which a sentinel has.
/// So this became the fallback rather than the answer, and
/// `App::forest_holds_main` is what chooses: the real row when there is one,
/// this when there is not. Two rows for one chat would be worse than the
/// problem the sentinel was added to solve.
///
/// It keeps its own `kind_tag`, and it is no longer true that nothing else
/// mints one — `NodeId::main` uses the same tag with a conversation id where
/// this uses `MAIN_ROW`. They do not collide, because [`NodeId`] compares on
/// the id as well as the tag and no conversation is called `main`, and they
/// never appear together anyway.
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
    /// Show the scratch sessions that have been put away.
    ///
    /// Off by default and off again after looking, because a pane holding
    /// everything ever asked is one people stop reading.
    ///
    /// **This used to ride on the closed-works filter and now stands alone.**
    /// `z` widened the *works* query from live to all, and archived scratch
    /// rows came back as a side effect of the same switch — one key for one
    /// intention, which was right while both halves existed. The works toggle
    /// has since gone, so there is nothing left to ride on: either the reveal
    /// gets a flag of its own or archiving becomes one-way inside the console.
    /// It reveals the lane and nothing else, which is what the key now says.
    pub show_archived_scratch: bool,
}

impl TreeState {
    /// Whether this node's children are on screen.
    ///
    /// `closed` is the set of works core returned from its *closed* query, so
    /// "is this an archive" is core's answer rather than something inferred
    /// from a label here.
    ///
    /// A project is shut until it is opened, which is what makes the fleet open
    /// as `main` and a list of repositories rather than as every agent on the
    /// box at once. The kind is read off the id's own tag rather than off a
    /// [`Node`], because every caller here holds an id and only some of them
    /// can reach the node it names.
    pub fn is_expanded(&self, id: &NodeId, closed: &HashSet<NodeId>) -> bool {
        if self.collapsed.contains(id) {
            return false;
        }
        if id.kind_tag == "project" || closed.contains(id) {
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

    /// Put the cursor on one node, opening whatever is closed above it.
    ///
    /// What backing out of a conversation needs. The row that opened it may sit
    /// under a project somebody collapsed, and a cursor set to a hidden row is
    /// dropped by [`TreeState::reconcile_to`] on the very next frame — so the
    /// trip out would land nowhere near the trip in. Opening the ancestors is
    /// what makes `←` and `⏎` a round trip rather than a one-way door.
    ///
    /// A node the forest does not hold leaves the cursor alone, so callers may
    /// offer a row that may or may not exist without checking first.
    pub fn reveal(&mut self, nodes: &[Node], id: &NodeId) {
        let Some(node) = nodes.iter().find(|n| n.id == *id) else {
            return;
        };
        // Walked by parent id rather than by scanning back for a shallower row,
        // for the reason `matching` gives: two works' sessions sit at the same
        // depth, and the nearest shallower row is not always the ancestor.
        //
        // `seen` is the same stop `matching` puts on the same walk. A forest
        // whose parent links loop would otherwise spin here forever, and this
        // runs on a keypress — the console would simply stop repainting, which
        // is the one failure a user cannot report usefully.
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut parent = node.parent.clone();
        while let Some(up) = parent {
            if !seen.insert(up.clone()) {
                break;
            }
            parent = nodes
                .iter()
                .find(|n| n.id == up)
                .and_then(|n| n.parent.clone());
            self.collapsed.remove(&up);
            self.opened.insert(up);
        }
        self.selected = Some(id.clone());
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
        // Jod's own row, and the only glyph that is not a block or a diamond.
        // He sits above the repositories rather than being one of them, so the
        // gutter says that before any colour does.
        NodeKind::Main => "★",
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
    use jod_core::tree::condense;

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
            stalled: 0,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
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

    /// A repository as core hands it over: a project, its manager, two works,
    /// the sessions under them, a run, and a session one of them spawned.
    fn repository() -> Vec<Node> {
        let mut project = node(NodeId::project("p1"), None, NodeKind::Project, 0, "jod");
        project.has_children = true;
        project.cards = 2;
        project.blocked = 1;
        let manager = node(
            NodeId::manager("m1"),
            Some(NodeId::project("p1")),
            NodeKind::Manager,
            1,
            "manager",
        );

        let mut parser = node(
            NodeId::work("w1"),
            Some(NodeId::project("p1")),
            NodeKind::Work,
            1,
            "the parser",
        );
        parser.has_children = true;
        let mut lead = node(
            NodeId::session("s1"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            2,
            "port the lexer",
        );
        lead.has_children = true;
        lead.running = true;
        let mut run = node(
            NodeId::run("r1"),
            Some(NodeId::session("s1")),
            NodeKind::Run,
            3,
            "run one",
        );
        run.running = true;
        run.status = Some("running".into());
        // The session `s1` spawned, which is what makes core's tree deeper than
        // three levels.
        let helper = node(
            NodeId::session("s2"),
            Some(NodeId::session("s1")),
            NodeKind::Session,
            3,
            "write the docs",
        );

        let mut deploy = node(
            NodeId::work("w2"),
            Some(NodeId::project("p1")),
            NodeKind::Work,
            1,
            "the deploy",
        );
        deploy.has_children = true;
        let ci = node(
            NodeId::session("s3"),
            Some(NodeId::work("w2")),
            NodeKind::Session,
            2,
            "fix the CI",
        );

        vec![project, manager, parser, lead, run, helper, deploy, ci]
    }

    /// Every row as `(kind, depth, label)`, which is the whole claim a fold
    /// makes.
    fn shape(nodes: &[Node]) -> Vec<(&'static str, usize, &str)> {
        nodes
            .iter()
            .map(|n| (n.id.kind_tag, n.depth, n.label.as_str()))
            .collect()
    }

    /// The shape the fleet asks for: a project, then its manager and every
    /// agent in it, all at one level. No works, no runs, and no session nested
    /// under the session that spawned it.
    ///
    /// The agents are numbered rather than titled — see `tree::hired_as`. The
    /// numbering runs across the repository and not inside each work, because
    /// the fold has just put both works' agents on one level and two
    /// `engineer#1`s under one project would be two rows nobody can tell apart:
    /// `fix the CI` belongs to `w2` and is still the third seat here.
    #[test]
    fn a_project_holds_its_manager_and_every_agent_at_the_same_level() {
        let folded = condense(&repository(), &nothing());
        assert_eq!(
            shape(&folded.nodes),
            [
                ("project", 0, "jod"),
                ("manager", 1, "manager"),
                ("session", 1, "engineer#1"),
                ("session", 1, "engineer#2"),
                ("session", 1, "engineer#3"),
            ]
        );
        for row in folded.nodes.iter().filter(|n| n.depth == 1) {
            assert_eq!(
                row.parent,
                Some(NodeId::project("p1")),
                "{} hangs off something other than the project",
                row.label
            );
        }
        assert!(
            folded.nodes[0].has_children,
            "the project is the one row left with anything inside it"
        );
        assert!(
            folded.nodes[1..].iter().all(|n| !n.has_children),
            "an agent's row is a leaf, so nothing draws an expansion marker"
        );
    }

    /// The verbs that acted on a work row have to keep working from the rows
    /// that replaced it, or `T` becomes a key that says there is no bus.
    #[test]
    fn every_agent_still_remembers_the_work_it_came_out_of() {
        let folded = condense(&repository(), &nothing());
        assert_eq!(folded.works.get(&NodeId::session("s1")), Some(&"w1".into()));
        assert_eq!(folded.works.get(&NodeId::session("s2")), Some(&"w1".into()));
        assert_eq!(folded.works.get(&NodeId::session("s3")), Some(&"w2".into()));
        assert_eq!(
            folded.works.get(&NodeId::manager("m1")),
            None,
            "a manager belongs to the repository rather than to a job in it"
        );
    }

    /// A stall is written on a run and the run rows are gone, so it has to ride
    /// up — a wedged agent that says so only on a row that is no longer drawn is
    /// a wedged agent nobody sees.
    #[test]
    fn a_stalled_run_marks_the_agent_it_was_running_under() {
        let mut nodes = repository();
        let run = nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Run)
            .expect("the fixture has a run");
        run.stalled_for_ms = Some(2_700_000);

        let folded = condense(&nodes, &nothing());
        let agent = folded
            .nodes
            .iter()
            .find(|n| n.id == NodeId::session("s1"))
            .expect("the agent that took the run");
        assert_eq!(agent.stalled_for_ms, Some(2_700_000));
    }

    /// The row is the agent, so the verbs that stop a process act on the run it
    /// is holding.
    #[test]
    fn an_agents_row_answers_for_the_run_it_is_holding() {
        let folded = condense(&repository(), &nothing());
        assert_eq!(
            folded.run_of.get(&NodeId::session("s1")),
            Some(&"r1".into())
        );
        assert!(
            folded.runs.contains("r1"),
            "and the run is accounted for, so the pane of loose runs leaves it alone"
        );
    }

    /// A run that ended is the only thing that can say *how*, and its row is
    /// gone — so the agent wears the ending until it takes another run.
    #[test]
    fn the_last_runs_ending_shows_on_the_idle_agent() {
        let mut nodes = repository();
        for row in nodes.iter_mut() {
            if row.kind == NodeKind::Run || row.id == NodeId::session("s1") {
                row.running = false;
            }
            if row.kind == NodeKind::Run {
                row.status = Some("failed".into());
            }
        }
        let folded = condense(&nodes, &nothing());
        let agent = folded
            .nodes
            .iter()
            .find(|n| n.id == NodeId::session("s1"))
            .expect("the agent that took the run");
        assert_eq!(agent.status.as_deref(), Some("failed"));
    }

    /// The old works have no project, and promoting their sessions to the top
    /// level would leave them on screen with nothing saying what they belong to.
    #[test]
    fn a_work_with_no_project_stays_a_heading_of_its_own() {
        let folded = condense(&forest(), &nothing());
        assert_eq!(
            shape(&folded.nodes),
            [
                ("work", 0, "the parser"),
                ("session", 1, "engineer#1"),
                ("session", 1, "engineer#2"),
            ]
        );
    }

    /// `z` shows the archives, and an archive flattened into the roster would be
    /// a finished agent sitting among the working ones with nothing marking it.
    ///
    /// The closed work's own agent starts its heading's numbering again, which
    /// is the reading that matches the screen: the seats being counted are the
    /// ones under the row you are looking at, and an archive is its own row.
    #[test]
    fn a_closed_work_keeps_its_heading_under_the_project() {
        let closed: HashSet<NodeId> = [NodeId::work("w2")].into_iter().collect();
        let folded = condense(&repository(), &closed);
        assert_eq!(
            shape(&folded.nodes),
            [
                ("project", 0, "jod"),
                ("manager", 1, "manager"),
                ("session", 1, "engineer#1"),
                ("session", 1, "engineer#2"),
                ("work", 1, "the deploy"),
                ("session", 2, "engineer#1"),
            ]
        );
    }

    /// `z` asks core a second time, and the second forest repeats every project
    /// and manager row of the first. Drawn as they arrive, a repository with one
    /// live work and one closed one appeared twice.
    #[test]
    fn a_project_the_archive_query_repeats_is_drawn_once() {
        let mut nodes = repository();
        let closed: HashSet<NodeId> = [NodeId::work("w2")].into_iter().collect();
        // The archive query's own answer for the same project, appended the way
        // `data::forest` appends it.
        let archived: Vec<Node> = repository()
            .into_iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Project | NodeKind::Manager)
                    || n.id == NodeId::work("w2")
                    || n.id == NodeId::session("s3")
            })
            .collect();
        nodes.retain(|n| n.id != NodeId::work("w2") && n.id != NodeId::session("s3"));
        nodes.extend(archived);

        let folded = condense(&nodes, &closed);
        assert_eq!(
            folded
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Project)
                .count(),
            1,
            "the repository is one row, whichever query found it: {:?}",
            shape(&folded.nodes)
        );
        assert_eq!(
            folded
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Manager)
                .count(),
            1
        );
    }

    /// The fleet opens as the repositories and nothing else, and one keystroke
    /// opens the one you came for.
    #[test]
    fn a_project_is_shut_until_it_is_opened() {
        let folded = condense(&repository(), &nothing());
        let mut tree = TreeState::default();
        assert_eq!(
            tree.row_ids(&folded.nodes, &nothing(), None),
            vec![NodeId::project("p1")],
            "the agents are inside the project, not on the screen beside it"
        );

        tree.selected = Some(NodeId::project("p1"));
        tree.toggle(&nothing());
        assert_eq!(
            tree.row_ids(&folded.nodes, &nothing(), None).len(),
            5,
            "and one keystroke shows the roster"
        );
    }

    /// A work with no project has no project row to open, so shutting projects
    /// by default must not shut it too — that would be a tree whose every row is
    /// closed and whose top level says nothing.
    #[test]
    fn a_work_with_no_project_is_open_the_way_it_always_was() {
        let folded = condense(&forest(), &nothing());
        let tree = TreeState::default();
        assert_eq!(tree.row_ids(&folded.nodes, &nothing(), None).len(), 3);
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

    /// Revealing a row deep in a folded tree opens every ancestor, not only the
    /// nearest one — a run whose session is open but whose work is shut is still
    /// off screen.
    #[test]
    fn revealing_a_row_opens_every_branch_above_it() {
        let nodes = forest();
        let mut tree = TreeState::default();
        tree.collapsed.insert(NodeId::work("w1"));
        tree.collapsed.insert(NodeId::session("s1"));

        tree.reveal(&nodes, &NodeId::run("r1"));

        assert_eq!(tree.selected, Some(NodeId::run("r1")));
        assert!(
            tree.row_ids(&nodes, &nothing(), None)
                .contains(&NodeId::run("r1")),
            "the cursor is on a row nothing draws"
        );
    }

    /// A row the forest does not hold leaves the cursor where it was, so a
    /// caller may offer one that may or may not exist without checking first.
    #[test]
    fn revealing_a_row_that_is_not_there_moves_nothing() {
        let nodes = forest();
        let mut tree = TreeState {
            selected: Some(NodeId::work("w1")),
            ..Default::default()
        };

        tree.reveal(&nodes, &NodeId::run("gone"));
        assert_eq!(tree.selected, Some(NodeId::work("w1")));
    }

    /// The walk runs on a keypress, so a forest whose parent links loop has to
    /// stop rather than spin: a console that quietly stops repainting is the one
    /// failure a user cannot report usefully.
    #[test]
    fn revealing_a_row_in_a_forest_that_loops_still_returns() {
        let mut first = node(
            NodeId::work("w1"),
            Some(NodeId::work("w2")),
            NodeKind::Work,
            0,
            "the parser",
        );
        first.has_children = true;
        let second = node(
            NodeId::work("w2"),
            Some(NodeId::work("w1")),
            NodeKind::Work,
            0,
            "the lexer",
        );
        let mut tree = TreeState::default();

        tree.reveal(&[first, second], &NodeId::work("w1"));
        assert_eq!(tree.selected, Some(NodeId::work("w1")));
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
