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
use std::collections::HashMap;

use crate::error::Result;
use crate::store::Store;
use crate::works::{Filter, State, Work};

/// What a row in the tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A repository. The level above works, so the tree groups by the thing
    /// that outlives every work and every session.
    Project,
    /// The conversation that owns a project over time. Always the first child
    /// of its project, and always the same row — entering it is how you say
    /// something about the repository rather than about one job in it.
    Manager,
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
    pub fn project(id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "project", id: id.into() }
    }
    /// Carries the *conversation* id, not the project's.
    ///
    /// A manager row exists to be entered, and what you enter is a
    /// conversation. Keying it by project would make every reader look the
    /// conversation up again from a row that already knew it.
    pub fn manager(conversation_id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "manager", id: conversation_id.into() }
    }
}

/// One row, already flattened for rendering.
///
/// `Serialize` so the HTTP API can hand the *same* forest to a browser that the
/// TUI draws in a terminal. Not `Deserialize`: nothing reads a forest back in,
/// and a client that could would be tempted to hold one as state it owns — a
/// forest is a query result, stale the moment it is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// How a run ended, straight from `runs.status` — `completed`, `failed`,
    /// `killed`, or `running` while it is still going.
    ///
    /// `running` alone cannot say this. It is false for a run that finished
    /// cleanly, for one that failed and for one that was killed, so a screen
    /// with only that bool draws all three the same way and a person cannot
    /// see that something broke. Null on a work and on a session, which have
    /// no status of their own: a work's state lives in `works.state` and a
    /// session is a conversation, not a process.
    pub status: Option<String>,
    /// How long this run has been silent, or `None` if it is healthy.
    ///
    /// A duration rather than the stored instant, so a renderer does not need a
    /// clock — and so every surface drawing this forest agrees on the age
    /// instead of each subtracting its own `now`.
    ///
    /// Null on works and sessions. A stall is a fact about a process, and a
    /// session is a conversation.
    pub stalled_for_ms: Option<i64>,
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

/// Draw a flattened forest as indented text.
///
/// Deliberately plain — no box-drawing characters, no colour, no width. What
/// makes a tree *readable* in a terminal is the renderer's job; what makes it
/// *right* is the order and the depth, and those are checkable without one.
pub fn render(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        for _ in 0..node.depth {
            out.push_str("  ");
        }
        out.push_str(&node.label);
        // Before `[running]`, and instead of it. A stalled run *is* still
        // running — that is the entire problem — so drawing both would put the
        // reassuring word next to the alarming one and let the eye take the
        // wrong one.
        if let Some(silent_for) = node.stalled_for_ms {
            out.push_str(&format!(" [stalled {}]", crate::heartbeat::human_ms(silent_for)));
        } else if node.running {
            out.push_str(" [running]");
        }
        if node.blocked > 0 {
            out.push_str(&format!(" [{} blocked]", node.blocked));
        } else if node.cards > 0 {
            out.push_str(&format!(" [{} cards]", node.cards));
        }
        out.push('\n');
    }
    out
}

/// One conversation, as the flatten reads it before it becomes a [`Node`].
struct RawSession {
    id: String,
    parent: Option<String>,
    title: String,
    summary: String,
    running: bool,
    /// Open cards raised by this conversation alone.
    cards: usize,
    blocked: usize,
}

/// Emit one work, its sessions and their runs, and say what cascaded up.
///
/// Extracted so a work can hang from a project row as easily as from the top
/// level — `base_depth` and `parent` are the only difference between the two,
/// and duplicating the walk to get them would be duplicating the part most
/// likely to drift.
///
/// Returns the work's own cards, blocked cards and whether anything under it is
/// running, so a project row can add them up the same way a work adds up its
/// sessions.
#[allow(clippy::too_many_arguments)]
fn push_work(
    out: &mut Vec<Node>,
    work: &Work,
    sessions: &mut HashMap<String, Vec<RawSession>>,
    runs: &mut HashMap<String, Vec<RawRun>>,
    stalled: &HashMap<String, i64>,
    now_ms: i64,
    base_depth: usize,
    parent: Option<NodeId>,
) -> (usize, usize, bool) {
    let own = sessions.remove(&work.id).unwrap_or_default();
    // A session whose parent is outside this work — the main chat is the usual
    // one — hangs from the work itself. Otherwise the whole subtree would be
    // dropped for pointing at a row that is not here.
    let ids: std::collections::HashSet<&str> = own.iter().map(|s| s.id.as_str()).collect();
    let mut children: HashMap<Option<String>, Vec<&RawSession>> = HashMap::new();
    for session in &own {
        let key = session
            .parent
            .as_deref()
            .filter(|p| ids.contains(p))
            .map(str::to_string);
        children.entry(key).or_default().push(session);
    }

    let work_node = out.len();
    out.push(Node {
        id: NodeId::work(&work.id),
        parent,
        kind: NodeKind::Work,
        depth: base_depth,
        label: if work.title.is_empty() {
            work.instruction.clone()
        } else {
            work.title.clone()
        },
        summary: work.summary.clone(),
        running: false,
        status: None,
        stalled_for_ms: None,
        cards: 0,
        blocked: 0,
        colour: work.colour.clone(),
        expanded: true,
        has_children: !own.is_empty(),
    });

    // Depth-first, oldest first, so a session always appears directly below the
    // session that spawned it — pushed in reverse because this is a stack and
    // the tree is read top-down.
    let mut stack: Vec<(&RawSession, usize, Option<String>)> = children
        .get(&None)
        .map(|top| {
            top.iter()
                .rev()
                .map(|s| (*s, base_depth + 1, None))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut work_cards = 0usize;
    let mut work_blocked = 0usize;
    let mut work_running = false;
    while let Some((session, depth, session_parent)) = stack.pop() {
        if let Some(kids) = children.get(&Some(session.id.clone())) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1, Some(session.id.clone())));
            }
        }
        let session_runs = runs.remove(&session.id).unwrap_or_default();
        let has_children = !session_runs.is_empty()
            || children.get(&Some(session.id.clone())).is_some_and(|c| !c.is_empty());
        work_cards += session.cards;
        work_blocked += session.blocked;
        work_running |= session.running;
        out.push(Node {
            id: NodeId::session(&session.id),
            parent: Some(match session_parent {
                Some(p) => NodeId::session(p),
                None => NodeId::work(&work.id),
            }),
            kind: NodeKind::Session,
            depth,
            label: if session.title.is_empty() {
                session.id.chars().take(8).collect()
            } else {
                session.title.clone()
            },
            summary: session.summary.clone(),
            running: session.running,
            status: None,
            stalled_for_ms: None,
            cards: session.cards,
            blocked: session.blocked,
            colour: work.colour.clone(),
            expanded: true,
            has_children,
        });
        for run in session_runs {
            out.push(Node {
                id: NodeId::run(&run.id),
                parent: Some(NodeId::session(&session.id)),
                kind: NodeKind::Run,
                depth: depth + 1,
                label: run.label,
                summary: run.summary,
                running: run.status == "running",
                // Only for a run that still claims to be running. A finished
                // run's leftover mark, if a sweep has not yet retired the row,
                // would draw a badge on something that has already stopped.
                stalled_for_ms: stalled
                    .get(&run.id)
                    .filter(|_| run.status == "running")
                    .map(|since| now_ms.saturating_sub(*since).max(0)),
                status: Some(run.status),
                cards: 0,
                blocked: 0,
                colour: work.colour.clone(),
                expanded: true,
                has_children: false,
            });
        }
    }
    // Cards cascade upward only, so the work's counts are its sessions' — which
    // is what makes the tree say where the questions are without being
    // expanded.
    out[work_node].cards = work_cards;
    out[work_node].blocked = work_blocked;
    let running = work_running && work.state != State::Closed;
    out[work_node].running = running;
    (work_cards, work_blocked, running)
}

/// One run under a session.
struct RawRun {
    id: String,
    conversation_id: String,
    label: String,
    summary: String,
    /// The whole of `runs.status`, not just whether it equals `running`.
    status: String,
}

impl Store {
    /// Every work, its sessions and their runs, flattened for rendering.
    ///
    /// The fleet tree is a **self-join over what already exists** — a work is a
    /// group, not a new kind of session — so this is four queries and one walk
    /// rather than a query per node. That matters: the alternative is a read
    /// per row on a screen that repaints on a tick, and the tree is the screen
    /// most likely to be open while forty runs are going.
    ///
    /// Every node comes back expanded. What is *visible* is the caller's state
    /// to hold, because it survives across rebuilds and this does not.
    pub fn forest(&self) -> Result<Vec<Node>> {
        self.forest_of(Filter::All)
    }

    pub fn forest_of(&self, filter: Filter) -> Result<Vec<Node>> {
        let works = self.works(filter)?;
        if works.is_empty() {
            return Ok(Vec::new());
        }

        // One read for the whole forest, joined in memory against the runs
        // below. The alternative is a lookup per run node, on the screen most
        // likely to be open while forty runs are going.
        let stalled = self.stalled_runs()?;
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut cards: HashMap<String, (usize, usize)> = HashMap::new();
        let mut sessions: HashMap<String, Vec<RawSession>> = HashMap::new();
        let mut runs: HashMap<String, Vec<RawRun>> = HashMap::new();
        // Declared out here because a manager's row wants it too, and a manager
        // is not a session inside a work — it hangs from its project, so it is
        // not reached by the walk below.
        let mut latest: HashMap<String, String> = HashMap::new();
        {
            let conn = self.conn.lock().expect("store lock poisoned");

            let mut stmt = conn.prepare(
                "SELECT conversation_id, COUNT(*), COALESCE(SUM(blocking), 0)
                   FROM cards WHERE status = 'open' GROUP BY conversation_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize),
                ))
            })?;
            for row in rows {
                let (id, counts) = row?;
                cards.insert(id, counts);
            }

            // The newest thing each session said, which is its summary. Read
            // from the transcript rather than asked of a model: a tree that
            // costs a model call per row is a tree nobody can afford to leave
            // open.
            let mut stmt = conn.prepare(
                "SELECT m.conversation_id, m.text, m.tool_name FROM messages m
                   JOIN (SELECT conversation_id, MAX(id) AS id FROM messages
                          GROUP BY conversation_id) last ON last.id = m.id",
            )?;
            let rows = stmt.query_map([], |r| {
                let text: String = r.get(1)?;
                let tool: Option<String> = r.get(2)?;
                Ok((r.get::<_, String>(0)?, summarise(&text, tool.as_deref())))
            })?;
            for row in rows {
                let (id, summary) = row?;
                latest.insert(id, summary);
            }

            let mut stmt = conn.prepare(
                "SELECT c.work_id, c.id, c.parent_conversation_id, c.title,
                        EXISTS (SELECT 1 FROM messages msg JOIN runs r ON r.id = msg.run_id
                                 WHERE msg.conversation_id = c.id AND r.status = 'running')
                   FROM conversations c
                  WHERE c.work_id IS NOT NULL
                  ORDER BY c.created_at_ms, c.id",
            )?;
            let rows = stmt.query_map([], |r| {
                let work_id: String = r.get(0)?;
                let id: String = r.get(1)?;
                let (open, blocked) = cards.get(&id).copied().unwrap_or((0, 0));
                Ok((
                    work_id,
                    RawSession {
                        parent: r.get(2)?,
                        title: r.get(3)?,
                        running: r.get::<_, i64>(4)? != 0,
                        summary: latest.get(&id).cloned().unwrap_or_default(),
                        cards: open,
                        blocked,
                        id,
                    },
                ))
            })?;
            for row in rows {
                let (work_id, session) = row?;
                sessions.entry(work_id).or_default().push(session);
            }

            // A run belongs to the conversation it wrote into. There is no
            // column saying so — `messages.run_id` is the join, and it is the
            // same one `conversation_for_run` uses.
            //
            // The summary comes from the newest message the run itself wrote,
            // through the same `summarise` the sessions above use, and *not*
            // from `runs.summary`. That column holds a serialised
            // `AgentSummary`, so printing it put `{"created_at_ms":1…` on a
            // screen where every other row was showing prose. Keyed on
            // `messages.run_id` rather than on the conversation, because one
            // session usually holds several runs and a conversation-wide
            // summary would give every one of them the same line.
            let mut stmt = conn.prepare(
                "SELECT m.conversation_id, r.id, r.name, r.status,
                        last.text, last.tool_name
                   FROM runs r
                   JOIN (SELECT DISTINCT conversation_id, run_id FROM messages
                          WHERE run_id IS NOT NULL) m ON m.run_id = r.id
                   JOIN (SELECT run_id, MAX(id) AS id FROM messages
                          WHERE run_id IS NOT NULL GROUP BY run_id)
                        tail ON tail.run_id = r.id
                   JOIN messages last ON last.id = tail.id
                  ORDER BY r.created_at_ms, r.id",
            )?;
            let rows = stmt.query_map([], |r| {
                let text: String = r.get(4)?;
                let tool: Option<String> = r.get(5)?;
                Ok(RawRun {
                    conversation_id: r.get(0)?,
                    id: r.get(1)?,
                    label: r.get(2)?,
                    summary: summarise(&text, tool.as_deref()),
                    status: r.get(3)?,
                })
            })?;
            for row in rows {
                let run = row?;
                runs.entry(run.conversation_id.clone()).or_default().push(run);
            }
        }

        // Grouped by project, in the order the works themselves came back, so
        // the most recently touched repository stays at the top and the tree
        // does not reorder itself against `works`' own sort.
        //
        // Works with no project sit at the top level exactly as they did
        // before. Old ones have a null and are not going to be given one:
        // backfilling them is its own task, and hiding them under an invented
        // project would be worse than leaving them loose.
        let mut order: Vec<String> = Vec::new();
        let mut by_project: HashMap<String, Vec<Work>> = HashMap::new();
        let mut loose: Vec<Work> = Vec::new();
        for work in works {
            match work.project_id.clone() {
                Some(project_id) => {
                    if !by_project.contains_key(&project_id) {
                        order.push(project_id.clone());
                    }
                    by_project.entry(project_id).or_default().push(work);
                }
                None => loose.push(work),
            }
        }

        let mut out = Vec::new();
        for project_id in order {
            let Some(project) = self.project(&project_id)? else {
                // Catalogued once, deleted since. `works.project_id` is
                // `ON DELETE SET NULL`, so this is only reachable on a database
                // mid-repair — and dropping the works would be the wrong
                // remedy. They fall through to the loose list instead.
                loose.extend(by_project.remove(&project_id).unwrap_or_default());
                continue;
            };
            let project_node = out.len();
            out.push(Node {
                id: NodeId::project(&project.id),
                parent: None,
                kind: NodeKind::Project,
                depth: 0,
                label: project.name.clone(),
                summary: project.notes.clone(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                colour: project.colour.clone(),
                expanded: true,
                has_children: true,
            });

            // The manager first, then the works. First because it is the row
            // you go to when you want to say something about this repository
            // rather than about one job in it — and because it is the one row
            // under a project that is always the same row.
            if let Some(manager) = &project.manager_conversation_id {
                out.push(Node {
                    id: NodeId::manager(manager),
                    parent: Some(NodeId::project(&project.id)),
                    kind: NodeKind::Manager,
                    depth: 1,
                    label: "manager".to_string(),
                    summary: latest.get(manager).cloned().unwrap_or_default(),
                    running: false,
                    status: None,
                    stalled_for_ms: None,
                    cards: 0,
                    blocked: 0,
                    colour: project.colour.clone(),
                    expanded: true,
                    has_children: false,
                });
            }

            let mut project_cards = 0usize;
            let mut project_blocked = 0usize;
            let mut project_running = false;
            for work in by_project.remove(&project.id).unwrap_or_default() {
                let (cards, blocked, running) = push_work(
                    &mut out,
                    &work,
                    &mut sessions,
                    &mut runs,
                    &stalled,
                    now_ms,
                    1,
                    Some(NodeId::project(&project.id)),
                );
                project_cards += cards;
                project_blocked += blocked;
                project_running |= running;
            }
            out[project_node].cards = project_cards;
            out[project_node].blocked = project_blocked;
            out[project_node].running = project_running;
        }

        for work in loose {
            push_work(
                &mut out,
                &work,
                &mut sessions,
                &mut runs,
                &stalled,
                now_ms,
                0,
                None,
            );
        }

        Ok(out)
    }
}

/// The one line a node shows: what was last said, or which tool was last
/// called.
fn summarise(text: &str, tool: Option<&str>) -> String {
    if let Some(tool) = tool {
        if !tool.is_empty() {
            return format!("{tool}…");
        }
    }
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::NewCard;
    use crate::conversation::{NewMessage, Role};
    use crate::harness::HarnessKind;
    use crate::store::StoredRun;
    use crate::works::Origin;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn session(s: &Store, work: &str, parent: Option<&str>, title: &str) -> String {
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.set_conversation_title(&c.id, title).unwrap();
        s.attach_conversation(&c.id, work, parent, Origin::Agent)
            .unwrap();
        c.id
    }

    fn run_for(s: &Store, conversation: &str, id: &str, status: &str) {
        s.save_run(&StoredRun {
            id: id.into(),
            name: format!("run {id}"),
            harness: "claude-code".into(),
            status: status.into(),
            cwd: "/tmp".into(),
            session_id: None,
            pid: None,
            pgid: None,
            created_at_ms: 1,
            summary: serde_json::json!({}),
        })
        .unwrap();
        s.append_message(
            conversation,
            NewMessage::new(Role::Assistant, "on it").from_run(id),
        )
        .unwrap();
    }

    #[test]
    fn an_empty_forest_has_no_rows() {
        assert!(store().forest().unwrap().is_empty());
    }

    /// The shape the fleet screen renders: a work, the sessions under it, and
    /// the runs under those — each one directly below the row that owns it.
    #[test]
    fn a_work_its_sessions_and_their_runs_flatten_in_document_order() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        s.set_work_title(&work.id, "the parser").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        let child = session(&s, &work.id, Some(&lead), "worker");
        run_for(&s, &lead, "run-1", "running");

        let nodes = s.forest().unwrap();
        let shape: Vec<(&str, usize, &str)> = nodes
            .iter()
            .map(|n| (n.id.kind_tag, n.depth, n.label.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                ("work", 0, "the parser"),
                ("session", 1, "lead"),
                ("run", 2, "run run-1"),
                ("session", 2, "worker"),
            ]
        );
        assert_eq!(nodes[1].parent, Some(NodeId::work(&work.id)));
        assert_eq!(nodes[3].parent, Some(NodeId::session(&lead)));
        assert_eq!(nodes[1].colour, nodes[0].colour, "the work tints its rows");
        assert!(nodes[0].running, "a work with a run in flight is running");
        assert!(nodes[0].has_children);
        assert!(!nodes[3].has_children);
        let _ = child;
    }

    /// Cascade is upward only: a work's row says where the questions are
    /// without being expanded, and a child never shows its parent's.
    #[test]
    fn a_sessions_cards_are_counted_on_its_work() {
        let s = store();
        let work = s.create_work("a job").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        let child = session(&s, &work.id, Some(&lead), "worker");
        for (conversation, blocking) in [(&lead, false), (&child, true), (&child, false)] {
            s.raise_card(NewCard {
                conversation_id: conversation.clone(),
                work_id: Some(work.id.clone()),
                blocking,
                title: format!("card for {conversation} {blocking}"),
                ..NewCard::default()
            })
            .unwrap();
        }

        let nodes = s.forest().unwrap();
        assert_eq!((nodes[0].cards, nodes[0].blocked), (3, 1));
        assert_eq!((nodes[1].cards, nodes[1].blocked), (1, 0), "the lead's own");
        assert_eq!((nodes[2].cards, nodes[2].blocked), (2, 1));
    }

    /// The first session of a work is usually spawned by the main chat, which
    /// is in no work at all. Dropping subtrees whose parent is not in the tree
    /// would lose the whole work.
    #[test]
    fn a_session_whose_parent_is_outside_the_work_hangs_from_the_work() {
        let s = store();
        let work = s.create_work("a job").unwrap();
        let main = s
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.set_conversation_title(&c.id, "worker").unwrap();
        s.attach_conversation(&c.id, &work.id, Some(&main), Origin::Orchestrator)
            .unwrap();

        let nodes = s.forest().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[1].parent, Some(NodeId::work(&work.id)));
    }

    #[test]
    fn a_closed_work_still_lists_and_stops_claiming_to_be_running() {
        let s = store();
        let work = s.create_work("a job").unwrap();
        let c = session(&s, &work.id, None, "worker");
        run_for(&s, &c, "run-1", "running");
        let task = s.work_tasks(&work.id).unwrap().remove(0);
        s.complete_work_task(&task.id).unwrap();
        // Finishing, then closed once the run stops.
        s.set_run_status("run-1", "completed").unwrap();
        s.refresh_work_state(&work.id).unwrap();

        let nodes = s.forest_of(crate::works::Filter::Closed).unwrap();
        assert_eq!(nodes[0].kind, NodeKind::Work);
        assert!(!nodes[0].running);
        assert!(s.forest_of(crate::works::Filter::Live).unwrap().is_empty());
    }

    /// A run node keeps its own status, not just whether it was running.
    ///
    /// `running: bool` is false for a run that finished cleanly, one that
    /// failed and one that was killed, so a tree carrying only that bool hands
    /// the renderer three rows it cannot tell apart.
    #[test]
    fn a_run_node_says_whether_it_finished_failed_or_was_killed() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        for (id, status) in [
            ("run-1", "completed"),
            ("run-2", "failed"),
            ("run-3", "killed"),
            ("run-4", "running"),
        ] {
            run_for(&s, &lead, id, status);
        }

        let nodes = s.forest().unwrap();
        let states: Vec<(&str, Option<&str>, bool)> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Run)
            .map(|n| (n.id.id.as_str(), n.status.as_deref(), n.running))
            .collect();
        assert_eq!(
            states,
            [
                ("run-1", Some("completed"), false),
                ("run-2", Some("failed"), false),
                ("run-3", Some("killed"), false),
                ("run-4", Some("running"), true),
            ]
        );
        assert_eq!(nodes[0].status, None, "a work has no run status");
        assert_eq!(nodes[1].status, None, "nor does a session");
    }

    /// A run is summarised by what it said, exactly as a session is.
    ///
    /// It used to be summarised by `runs.summary`, which holds a serialised
    /// `AgentSummary`, so the row read as JSON. Two runs share one session
    /// here because that is the case a conversation-wide summary gets wrong:
    /// it would hand both of them the newest line in the thread, and the
    /// second run's row would claim the first run's work.
    #[test]
    fn a_run_is_summarised_by_what_it_said_and_not_by_its_stored_json() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        for (id, said) in [("run-1", "wrote the lexer"), ("run-2", "ran the suite")] {
            s.save_run(&StoredRun {
                id: id.into(),
                name: format!("run {id}"),
                harness: "claude-code".into(),
                status: "completed".into(),
                cwd: "/tmp".into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 1,
                summary: serde_json::json!({"id": id, "created_at_ms": 1}),
            })
            .unwrap();
            s.append_message(
                &lead,
                NewMessage::new(Role::Assistant, said).from_run(id),
            )
            .unwrap();
        }

        let nodes = s.forest().unwrap();
        let runs: Vec<(&str, &str)> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Run)
            .map(|n| (n.id.id.as_str(), n.summary.as_str()))
            .collect();
        assert_eq!(
            runs,
            [("run-1", "wrote the lexer"), ("run-2", "ran the suite")],
            "each run says its own last line",
        );
        // And the session still says the newest thing in the whole thread.
        assert_eq!(nodes[1].summary, "ran the suite");
    }

    /// A run whose last turn was a tool call names the tool, the way a session
    /// in the same state already does.
    #[test]
    fn a_run_that_last_called_a_tool_names_it() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        run_for(&s, &lead, "run-1", "running");
        s.append_message(
            &lead,
            NewMessage {
                role: Role::ToolCall,
                text: "cargo test".into(),
                tool_name: Some("Bash".into()),
                tool_input: None,
                run_id: Some("run-1".into()),
                run_seq: None,
            },
        )
        .unwrap();

        let nodes = s.forest().unwrap();
        let run = nodes.iter().find(|n| n.kind == NodeKind::Run).unwrap();
        assert_eq!(run.summary, "Bash…");
    }

    #[test]
    fn rendering_indents_by_depth_and_marks_what_needs_attention() {
        let nodes = vec![
            Node {
                id: NodeId::work("w"),
                parent: None,
                kind: NodeKind::Work,
                depth: 0,
                label: "the parser".into(),
                summary: String::new(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 2,
                blocked: 1,
                colour: "cyan".into(),
                expanded: true,
                has_children: true,
            },
            Node {
                id: NodeId::session("c"),
                parent: Some(NodeId::work("w")),
                kind: NodeKind::Session,
                depth: 1,
                label: "lead".into(),
                summary: String::new(),
                running: true,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                colour: "cyan".into(),
                expanded: true,
                has_children: false,
            },
        ];
        assert_eq!(render(&nodes), "the parser [1 blocked]\n  lead [running]\n");
    }

    /// Check 15. The level above works, so the tree groups by the thing that
    /// outlives every work and every session.
    #[test]
    fn a_project_holds_its_manager_first_and_then_its_works() {
        let s = store();
        let dir = format!("/tmp/jod-tree-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let (manager, _) = s
            .manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .unwrap();
        let work = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        session(&s, &work.id, None, "engineer");

        let nodes = s.forest().unwrap();

        assert_eq!(nodes[0].kind, NodeKind::Project, "{nodes:?}");
        assert_eq!(nodes[0].label, "tetris");
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].parent, None);

        assert_eq!(nodes[1].kind, NodeKind::Manager, "{nodes:?}");
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(
            nodes[1].parent,
            Some(NodeId::project(&project.id)),
            "the manager has to hang from its project"
        );
        assert_eq!(
            nodes[1].id,
            NodeId::manager(&manager),
            "the row carries the conversation to enter, not the project"
        );

        assert_eq!(nodes[2].kind, NodeKind::Work, "{nodes:?}");
        assert_eq!(nodes[2].depth, 1, "a project's works sit beside its manager");
        assert_eq!(nodes[2].parent, Some(NodeId::project(&project.id)));
        assert_eq!(nodes[3].kind, NodeKind::Session);
        assert_eq!(nodes[3].depth, 2, "and the whole subtree shifts down with it");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A work opened before projects were recorded keeps its null and stays at
    /// the top level. Backfilling is its own task, and hiding old works under
    /// an invented project would be worse than leaving them loose.
    #[test]
    fn a_work_with_no_project_stays_at_the_top_level() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        session(&s, &work.id, None, "engineer");

        let nodes = s.forest().unwrap();
        assert_eq!(nodes[0].kind, NodeKind::Work, "{nodes:?}");
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].parent, None);
    }

    /// A project with no manager yet is still a project. It gets one on the
    /// first instruction routed to it, and until then the row would otherwise
    /// have to invent a child that does not exist.
    #[test]
    fn a_project_whose_manager_has_not_been_started_yet_still_holds_its_works() {
        let s = store();
        let dir = format!("/tmp/jod-tree-nomgr-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let work = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        session(&s, &work.id, None, "engineer");

        let nodes = s.forest().unwrap();
        assert_eq!(nodes[0].kind, NodeKind::Project);
        assert_eq!(nodes[1].kind, NodeKind::Work, "{nodes:?}");
        assert_eq!(nodes[1].depth, 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Cards cascade to the project row too, or the level added to make the
    /// fleet readable would be the one level that hides where the questions
    /// are.
    #[test]
    fn a_projects_row_counts_the_cards_under_it() {
        let s = store();
        let dir = format!("/tmp/jod-tree-cards-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let work = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        let engineer = session(&s, &work.id, None, "engineer");
        s.raise_card(NewCard {
            conversation_id: engineer,
            work_id: Some(work.id.clone()),
            blocking: true,
            title: "which parser".into(),
            ..Default::default()
        })
        .unwrap();

        let nodes = s.forest().unwrap();
        let project_row = &nodes[0];
        assert_eq!(project_row.kind, NodeKind::Project);
        assert_eq!(project_row.cards, 1, "{nodes:?}");
        assert_eq!(project_row.blocked, 1, "{nodes:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stalled run must not also read as running. It *is* still running —
    /// that is the whole problem — so a row carrying both words would put the
    /// reassuring one next to the alarming one and let the eye take the wrong
    /// one. The badge replaces it rather than joining it.
    #[test]
    fn a_stalled_run_says_so_instead_of_saying_running() {
        let nodes = vec![Node {
            id: NodeId::run("r"),
            parent: None,
            kind: NodeKind::Run,
            depth: 0,
            label: "engineer".into(),
            summary: String::new(),
            running: true,
            status: Some("running".into()),
            stalled_for_ms: Some(45 * 60_000),
            cards: 0,
            blocked: 0,
            colour: "cyan".into(),
            expanded: true,
            has_children: false,
        }];
        let drawn = render(&nodes);
        assert_eq!(drawn, "engineer [stalled 45m]\n");
        assert!(
            !drawn.contains("[running]"),
            "a spinner that keeps spinning on a wedged agent is the bug: {drawn}"
        );
    }
}
