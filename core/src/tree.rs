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
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::error::Result;
use crate::store::Store;
use crate::works::{Filter, State, Work};

/// What a row in the tree is.
///
/// The first three are the chain of command — Jod takes an instruction, hands
/// anything touching a repository to that repository's manager, and the manager
/// is what puts an engineer on it. The rest is the work itself. A reader that
/// only knows about works and runs can see *that* something is running but not
/// *who asked for it*, which is the difference between a list and a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Jod himself — the pinned conversation every instruction arrives in.
    ///
    /// One row, always first, and the only row in the forest that is not under
    /// a repository. It is here because a manager's work begins as something
    /// said in this conversation, and a tree whose top level is a list of
    /// repositories cannot show that.
    Main,
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
    /// Jod's own row. Carries the pinned conversation's id, for the same reason
    /// [`NodeId::manager`] carries its conversation's.
    pub fn main(conversation_id: impl Into<String>) -> NodeId {
        NodeId { kind_tag: "main", id: conversation_id.into() }
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

/// Everything a work's rows are built out of, read once for the whole forest.
///
/// Gathered into one value rather than passed as four arguments, because
/// [`push_work`] already needs a depth and a parent on top of them and the pile
/// was over clippy's limit. Bundling the *reads* is also the honest grouping:
/// these four are the query results, and the other two say where the work goes.
///
/// The maps are `&mut` because the walk drains them — a session belongs to one
/// work, so taking it out is both cheaper than cloning and a guard against
/// emitting it twice.
struct Flatten<'a> {
    sessions: &'a mut HashMap<String, Vec<RawSession>>,
    runs: &'a mut HashMap<String, Vec<RawRun>>,
    stalled: &'a HashMap<String, i64>,
    now_ms: i64,
}

/// Emit every run that wrote into one conversation, and say if any is alive.
///
/// Three rows own runs now — a session inside a work, a project's manager, and
/// Jod himself — and only the first is reached by the walk over works. Before
/// this was pulled out, the other two were rows with nothing under them: a
/// manager that had been running for ten minutes drew exactly like one that had
/// never been asked anything, because the only code that turned a run into a row
/// lived inside the loop over a work's sessions.
///
/// The caller decides `has_children` before calling, since the node has to be
/// pushed before the rows that hang from it.
fn push_runs(
    out: &mut Vec<Node>,
    from: &mut Flatten<'_>,
    conversation_id: &str,
    parent: &NodeId,
    depth: usize,
    colour: &str,
) -> bool {
    let mut any_running = false;
    for run in from.runs.remove(conversation_id).unwrap_or_default() {
        let running = run.status == "running";
        any_running |= running;
        out.push(Node {
            id: NodeId::run(&run.id),
            parent: Some(parent.clone()),
            kind: NodeKind::Run,
            depth,
            label: run.label,
            summary: run.summary,
            running,
            // Only for a run that still claims to be running. A finished run's
            // leftover mark, if a sweep has not yet retired the row, would draw
            // a badge on something that has already stopped.
            stalled_for_ms: from
                .stalled
                .get(&run.id)
                .filter(|_| running)
                .map(|since| from.now_ms.saturating_sub(*since).max(0)),
            status: Some(run.status),
            cards: 0,
            blocked: 0,
            colour: colour.to_string(),
            expanded: true,
            has_children: false,
        });
    }
    any_running
}

/// Whether any run wrote into this conversation, without draining them.
///
/// `has_children` has to be decided before the owning row is pushed, and
/// [`push_runs`] can only answer once it has taken them.
fn holds_runs(from: &Flatten<'_>, conversation_id: &str) -> bool {
    from.runs.get(conversation_id).is_some_and(|runs| !runs.is_empty())
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
fn push_work(
    out: &mut Vec<Node>,
    work: &Work,
    from: &mut Flatten<'_>,
    base_depth: usize,
    parent: Option<NodeId>,
) -> (usize, usize, bool) {
    let own = from.sessions.remove(&work.id).unwrap_or_default();
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
        let has_children = holds_runs(from, &session.id)
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
        work_running |= push_runs(
            out,
            from,
            &session.id,
            &NodeId::session(&session.id),
            depth + 1,
            &work.colour,
        );
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
        let main = self.pinned_conversation()?;
        // Every tracked project, not only the ones a work has been opened
        // under. A manager owns its repository from the moment Jod first hands
        // it an instruction, and that instruction usually lands long before any
        // work does — so a fleet built from works alone drew nothing at all
        // during exactly the stretch when somebody is watching to see whether
        // the manager picked the job up.
        let projects = self.projects(false)?;
        if works.is_empty() && main.is_none() && projects.is_empty() {
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
                "SELECT m.conversation_id, m.text, m.tool_name, m.role FROM messages m
                   JOIN (SELECT conversation_id, MAX(id) AS id FROM messages
                          GROUP BY conversation_id) last ON last.id = m.id",
            )?;
            let rows = stmt.query_map([], |r| {
                let text: String = r.get(1)?;
                let tool: Option<String> = r.get(2)?;
                let role: String = r.get(3)?;
                Ok((
                    r.get::<_, String>(0)?,
                    summarise(&role, &text, tool.as_deref()),
                ))
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
                        last.text, last.tool_name, last.role
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
                let role: String = r.get(6)?;
                Ok(RawRun {
                    conversation_id: r.get(0)?,
                    id: r.get(1)?,
                    label: r.get(2)?,
                    summary: summarise(&role, &text, tool.as_deref()),
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
        // before. Migration `0023` gave one to every old work whose checkout
        // named a catalogued repository, so what is left here is the honest
        // remainder: work opened somewhere nobody had catalogued. Hiding those
        // under an invented project would be worse than leaving them loose.
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

        // Then every remaining tracked project that has a manager, after the
        // ones works put in order. A manager is a row worth showing on its own
        // — it is where an instruction about the repository goes — but a
        // repository nobody has said anything about yet has no chain to draw,
        // so a project with neither works nor a manager stays off the tree.
        for project in &projects {
            if project.manager_conversation_id.is_some() && !by_project.contains_key(&project.id) {
                order.push(project.id.clone());
            }
        }

        let mut from = Flatten {
            sessions: &mut sessions,
            runs: &mut runs,
            stalled: &stalled,
            now_ms,
        };

        let mut out = Vec::new();

        // Jod first, above every repository. He is not their parent — he owns
        // no checkout and does none of the work — but every instruction starts
        // in this conversation, so it is the row that explains why any of the
        // rest is happening.
        if let Some(main) = &main {
            let main_node = out.len();
            let id = NodeId::main(main);
            let (main_cards, main_blocked) = cards.get(main).copied().unwrap_or((0, 0));
            out.push(Node {
                id: id.clone(),
                parent: None,
                kind: NodeKind::Main,
                depth: 0,
                label: "jod".to_string(),
                summary: latest.get(main).cloned().unwrap_or_default(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: main_cards,
                blocked: main_blocked,
                colour: "cyan".to_string(),
                expanded: true,
                has_children: holds_runs(&from, main),
            });
            let running = push_runs(&mut out, &mut from, main, &id, 1, "cyan");
            out[main_node].running = running;
        }

        for project_id in order {
            let Some(project) = self.project(&project_id)? else {
                // Catalogued once, deleted since. `works.project_id` is
                // `ON DELETE SET NULL`, so this is only reachable on a database
                // mid-repair — and dropping the works would be the wrong
                // remedy. They fall through to the loose list instead.
                loose.extend(by_project.remove(&project_id).unwrap_or_default());
                continue;
            };
            // Untracked, and untracked means gone from here. `Archived` was
            // already off every everyday listing — `Store::projects` filters on
            // it — and the fleet was the one surface that never asked, because
            // it builds from `works` and only reads the project afterwards to
            // find a heading for them. So archiving a repository took it out of
            // the catalog panel and left the tree drawing it, its manager, and
            // every work under it exactly as before, which reads as the archive
            // having silently failed.
            //
            // The works go with it rather than falling through to `loose`. They
            // are the subtree the heading was hiding, and promoting them to the
            // top level would move the rows up a level instead of taking them
            // off the screen — a louder version of the thing being asked to
            // stop. Nothing is deleted and nothing is lost: the rows stay in the
            // database, the sessions screen and `list_agents` still show any
            // agent still running in there, and `jod project restore` puts the
            // whole subtree back.
            //
            // `Paused` deliberately stays. It is the state that means dormant
            // rather than finished, and it sides with `Active` on every listing
            // — see `projects::State`.
            if project.state == crate::projects::State::Archived {
                by_project.remove(&project.id);
                continue;
            }
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

            let mut project_cards = 0usize;
            let mut project_blocked = 0usize;
            let mut project_running = false;

            // The manager first, then the works. First because it is the row
            // you go to when you want to say something about this repository
            // rather than about one job in it — and because it is the one row
            // under a project that is always the same row.
            //
            // Its runs hang from it exactly as a session's do. That row used to
            // be a leaf that was permanently `running: false`, which made a
            // manager mid-instruction indistinguishable from one that had never
            // been asked anything, and left it with nothing beneath it for a
            // reader to open.
            if let Some(manager) = &project.manager_conversation_id {
                let manager_node = out.len();
                let id = NodeId::manager(manager);
                let (manager_cards, manager_blocked) =
                    cards.get(manager).copied().unwrap_or((0, 0));
                out.push(Node {
                    id: id.clone(),
                    parent: Some(NodeId::project(&project.id)),
                    kind: NodeKind::Manager,
                    depth: 1,
                    label: "manager".to_string(),
                    summary: latest.get(manager).cloned().unwrap_or_default(),
                    running: false,
                    status: None,
                    stalled_for_ms: None,
                    cards: manager_cards,
                    blocked: manager_blocked,
                    colour: project.colour.clone(),
                    expanded: true,
                    has_children: holds_runs(&from, manager),
                });
                let running =
                    push_runs(&mut out, &mut from, manager, &id, 2, &project.colour);
                out[manager_node].running = running;
                project_cards += manager_cards;
                project_blocked += manager_blocked;
                project_running |= running;
            }

            for work in by_project.remove(&project.id).unwrap_or_default() {
                let (cards, blocked, running) = push_work(
                    &mut out,
                    &work,
                    &mut from,
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
            push_work(&mut out, &work, &mut from, 0, None);
        }

        Ok(out)
    }
}

// ─── the fold ────────────────────────────────────────────────────────────────

/// The forest folded to a roster, and what the fold took with it.
///
/// It lives here rather than in the TUI that first needed it, because both
/// surfaces draw this shape now. `/v1/fleet` serves it and the browser renders
/// it unchanged, which is the same rule [`Store::forest_of`] is under: a client
/// that draws the fleet is drawing the TUI's screen, and a second fold on the
/// far side of the wire is how the two would come to disagree about what a
/// repository is doing.
pub struct Condensed {
    /// The rows the fleet draws: projects, and the agents inside them.
    pub nodes: Vec<Node>,
    /// Which work each remaining row belongs to.
    ///
    /// The work rows are gone from the tree, so the keys that act on a work —
    /// `T`, which opens its message bus — can no longer climb to one. This is
    /// how they still find it, and it is built from the forest *before* the
    /// fold, where the answer is still written down.
    pub works: HashMap<NodeId, String>,
    /// The run each agent's row answers for — the one still going if there is
    /// one, otherwise the last one it took.
    ///
    /// `s`, `a` and `t` act on a process, and the row that held one was the run
    /// row this fold removes. The agent's own row inherits the verbs, which is
    /// also the reading that matches the screen: the row says an agent is
    /// running, so stopping it should stop that.
    pub run_of: HashMap<NodeId, String>,
    /// The ids of the runs the fold swallowed.
    ///
    /// The pane below the tree holds the runs the tree cannot show, and it used
    /// to work that out by looking for a run's node. There are no run rows any
    /// more, so without this list every run in the fleet would read as loose and
    /// the pane would become a second copy of the whole flat list.
    pub runs: HashSet<String>,
}

impl Condensed {
    /// [`Condensed::run_of`], keyed the way a client keys a row.
    ///
    /// `NodeId` is a struct, and a JSON object's keys are strings, so the map
    /// cannot go on the wire as it stands. `"<kind_tag>:<id>"` is the key the
    /// browser already builds to hold a selection across a rebuild, so the
    /// answer arrives in the form the reader is going to look it up by.
    ///
    /// Ordered, so the payload is stable between two identical reads and a
    /// diff of two captures is about the fleet rather than about hashing.
    pub fn run_by_key(&self) -> BTreeMap<String, String> {
        self.run_of
            .iter()
            .map(|(id, run)| (format!("{}:{}", id.kind_tag, id.id), run.clone()))
            .collect()
    }
}

/// Fold the forest to the two levels the fleet reads as a roster.
///
/// A project keeps its manager and gains every session under every one of its
/// works, all at the same level, in the order the works came back. Works and
/// runs are dropped, and a session that spawned children sits beside them
/// rather than above them — the fleet answers "who is on this repository",
/// which is one list, not a hierarchy.
///
/// Nothing on it becomes unreachable. A run is inside the session that started
/// it, so `⏎` on the session and the transcript has it; the work's bus is still
/// one `T` away through [`Condensed::works`]; and what a run was *saying* — the
/// stall, the status of the last one — is carried up onto the session's own
/// row, because a wedged agent that says so only on a row three levels down is
/// a wedged agent nobody sees.
///
/// Two headings survive the fold:
///
/// - A **work with no project**, which becomes a top-level row of its own.
///   Those are the old ones with a null `project_id`; promoting their sessions
///   to the top level would leave them loose on the screen with nothing saying
///   what they belong to.
/// - A **closed** work, which stays a heading under its project. `z` exists to
///   show the archives, and flattening them in would leave a project holding a
///   pile of finished agents with nothing marking which are over.
pub fn condense(nodes: &[Node], closed: &HashSet<NodeId>) -> Condensed {
    let mut out: Vec<Node> = Vec::new();
    let mut works: HashMap<NodeId, String> = HashMap::new();
    let mut runs: HashSet<String> = HashSet::new();
    let mut run_of: HashMap<NodeId, String> = HashMap::new();
    // The sessions whose chosen run is still going, so a finished run that
    // comes after one does not take the row's verbs off a live process.
    let mut live: HashSet<NodeId> = HashSet::new();
    // Ids already emitted. `show_closed` asks core twice and the second forest
    // repeats every project and manager row of the first, so without this a
    // repository with one live work and one closed one is drawn twice.
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut project: Option<NodeId> = None;
    // Where the sessions being read now hang, and how deep that row is.
    let mut under: Option<(NodeId, usize)> = None;
    let mut work: Option<String> = None;
    // The rows a run's news has to reach: the conversation that owns it, by
    // index into `out`. Jod and a manager are in here beside the sessions,
    // because all three are conversations that hold runs.
    let mut owner_at: HashMap<NodeId, usize> = HashMap::new();

    for node in nodes {
        match node.kind {
            // Jod is a conversation holding runs, like a manager or a session,
            // so his row collects their news the same way. He heads nothing —
            // the repositories sit beside him, not under him — so `under` is
            // deliberately untouched and the cursor never descends into him.
            NodeKind::Main => {
                if seen.insert(node.id.clone()) {
                    owner_at.insert(node.id.clone(), out.len());
                    out.push(Node {
                        parent: None,
                        depth: 0,
                        ..node.clone()
                    });
                }
            }
            NodeKind::Project => {
                project = Some(node.id.clone());
                under = Some((node.id.clone(), 0));
                work = None;
                match out.iter_mut().find(|row| row.id == node.id) {
                    // The same project from the archive query. Its counts are
                    // over the *closed* works, so they add to the live ones
                    // rather than replacing them.
                    Some(row) => {
                        row.cards += node.cards;
                        row.blocked += node.blocked;
                        row.running |= node.running;
                    }
                    None => {
                        seen.insert(node.id.clone());
                        out.push(Node {
                            parent: None,
                            depth: 0,
                            ..node.clone()
                        });
                    }
                }
            }
            NodeKind::Manager => {
                if seen.insert(node.id.clone()) {
                    owner_at.insert(node.id.clone(), out.len());
                    out.push(Node {
                        parent: project.clone(),
                        depth: 1,
                        ..node.clone()
                    });
                }
            }
            NodeKind::Work => {
                work = Some(node.id.id.clone());
                // Whether this work belongs to a repository is read off the
                // node, not off whether a project has been seen. `forest_of`
                // emits every project first and the works with a null
                // `project_id` after all of them, so the running `project` is
                // still the last repository read — and asking it would file a
                // loose work's agents under a repository they have nothing to
                // do with.
                let loose = node.parent.is_none();
                if loose {
                    project = None;
                }
                let heading = loose || closed.contains(&node.id);
                if !heading {
                    under = project.clone().map(|id| (id, 0));
                    continue;
                }
                let depth = usize::from(project.is_some());
                under = Some((node.id.clone(), depth));
                if seen.insert(node.id.clone()) {
                    works.insert(node.id.clone(), node.id.id.clone());
                    out.push(Node {
                        parent: project.clone(),
                        depth,
                        ..node.clone()
                    });
                }
            }
            NodeKind::Session => {
                let Some((parent, depth)) = under.clone() else {
                    continue;
                };
                if !seen.insert(node.id.clone()) {
                    continue;
                }
                if let Some(work) = &work {
                    works.insert(node.id.clone(), work.clone());
                }
                owner_at.insert(node.id.clone(), out.len());
                out.push(Node {
                    parent: Some(parent),
                    depth: depth + 1,
                    ..node.clone()
                });
            }
            // Dropped as a row, and read as news about the conversation above
            // it. A stall is the whole reason the fleet is worth looking at,
            // and it is a fact core only ever writes on a run.
            NodeKind::Run => {
                runs.insert(node.id.id.clone());
                let Some(parent) = node.parent.as_ref().and_then(|id| owner_at.get(id)) else {
                    continue;
                };
                let row = &mut out[*parent];
                // The longest silence, not the newest: an agent with one wedged
                // run and one chatty one is wedged.
                if let Some(silent_for) = node.stalled_for_ms {
                    row.stalled_for_ms = Some(row.stalled_for_ms.unwrap_or(0).max(silent_for));
                }
                // The newest run's ending, so a session whose last run failed
                // wears the failure. Runs arrive oldest first, so the last one
                // written wins. Only while the session itself is idle — a
                // session with something running is running, whatever the run
                // before it did.
                if !row.running && !node.running {
                    row.status = node.status.clone();
                }
                let owner = row.id.clone();
                if node.running {
                    run_of.insert(owner.clone(), node.id.id.clone());
                    live.insert(owner);
                } else if !live.contains(&owner) {
                    run_of.insert(owner, node.id.id.clone());
                }
            }
        }
    }

    // The shape that is left, rather than the one core described: a session
    // that had runs under it is now a leaf, and a project whose only work was
    // dropped has children it did not have before.
    for at in 0..out.len() {
        out[at].has_children = out
            .get(at + 1)
            .is_some_and(|next| next.depth > out[at].depth);
    }
    Condensed {
        nodes: out,
        works,
        run_of,
        runs,
    }
}

impl Store {
    /// The fleet as a screen draws it: the forest, folded.
    ///
    /// The queries and the fold are here rather than in each caller because
    /// they are one answer, and because there are now two surfaces asking it.
    /// Also returned: which works are archives, which the caller needs to know
    /// to draw one shut and which a [`Node`] cannot say — it carries no state,
    /// and inferring one from a label would be guessing.
    ///
    /// **`Filter::All` means two queries here, not one.** `forest_of(All)`
    /// returns the live and closed works interleaved; this asks twice instead,
    /// because the archives belong below the live rows and core's own ordering
    /// already puts them there. Re-sorting the single answer afterwards would
    /// be this function having an opinion about where a work goes.
    ///
    /// [`Filter::Live`] is the cheaper path *and* the default, because a fleet
    /// that opens as a list of everything ever done is one people stop reading.
    pub fn fleet(&self, filter: Filter) -> Result<(Condensed, HashSet<NodeId>)> {
        let mut nodes = match filter {
            Filter::Closed => Vec::new(),
            _ => self.forest_of(Filter::Live)?,
        };
        let mut closed = HashSet::new();
        if matches!(filter, Filter::All | Filter::Closed) {
            let archived = self.forest_of(Filter::Closed)?;
            for node in &archived {
                if node.kind == NodeKind::Work {
                    closed.insert(node.id.clone());
                }
            }
            nodes.extend(archived);
        }
        Ok((condense(&nodes, &closed), closed))
    }
}

/// The one line a node shows: what was last said, or which tool was last
/// called.
///
/// `role` is here for one case that used to be indistinguishable from a dead
/// row. When the newest message in a scope is the *instruction* — role `user` —
/// nothing has answered it yet, and printing it plainly makes the row read as
/// an agent sitting there ignoring what it was handed. That is exactly what a
/// resuming session looks like for its first couple of minutes: `claude
/// --resume` on a large transcript can take minutes to load before it emits a
/// token, and for the whole of that window the run's newest message is the
/// prompt. Marking it `starting…` says the true thing — handed this, has not
/// spoken yet — and keeps the instruction visible so the row still says what
/// the agent is about to do.
fn summarise(role: &str, text: &str, tool: Option<&str>) -> String {
    if let Some(tool) = tool {
        if !tool.is_empty() {
            return format!("{tool}…");
        }
    }
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat: String = flat.chars().take(80).collect();
    match role {
        "user" => format!("starting… {flat}"),
        _ => flat,
    }
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

        // Three rows, not two: pinning a main conversation gives Jod a row of
        // his own at the top, and the work follows it.
        let nodes = s.forest().unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].id, NodeId::main(&main));
        assert_eq!(nodes[1].id, NodeId::work(&work.id));
        assert_eq!(nodes[2].depth, 1);
        assert_eq!(nodes[2].parent, Some(NodeId::work(&work.id)));
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

    /// A run that has been handed an instruction and has not answered yet says
    /// so, instead of echoing the instruction back as though it were output.
    ///
    /// This is the state a resumed session sits in for its first minutes:
    /// `claude --resume` on a large transcript loads the whole thing before it
    /// emits a token, and for the whole of that window the newest message under
    /// the run is the prompt. Rendered plainly, the row is indistinguishable
    /// from an agent ignoring what it was given, which is how a working engineer
    /// gets read as dead and restarted.
    #[test]
    fn a_run_that_has_not_answered_its_instruction_yet_says_it_is_starting() {
        let s = store();
        let work = s.create_work("port the parser").unwrap();
        let lead = session(&s, &work.id, None, "lead");
        s.save_run(&StoredRun {
            id: "run-1".into(),
            name: "run run-1".into(),
            harness: "claude-code".into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            session_id: Some("sess-1".into()),
            pid: None,
            pgid: None,
            created_at_ms: 1,
            summary: serde_json::json!({}),
        })
        .unwrap();
        s.append_prompt(&lead, "run-1", "add the opponent cars").unwrap();

        let nodes = s.forest().unwrap();
        let run = nodes.iter().find(|n| n.kind == NodeKind::Run).unwrap();
        assert_eq!(run.summary, "starting… add the opponent cars");

        // And it stops saying so the moment it speaks.
        s.append_message(
            &lead,
            NewMessage::new(Role::Assistant, "right, one car is not a race").from_run("run-1"),
        )
        .unwrap();
        let nodes = s.forest().unwrap();
        let run = nodes.iter().find(|n| n.kind == NodeKind::Run).unwrap();
        assert_eq!(run.summary, "right, one car is not a race");
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

    /// A manager mid-instruction has to look different from an idle one.
    ///
    /// The row was a permanent leaf carrying `running: false`, so a manager that
    /// had been working for ten minutes drew exactly like one nobody had ever
    /// spoken to — and with nothing beneath it, there was nothing to open
    /// either. Both halves of that are checked here, because they had one cause.
    #[test]
    fn a_manager_carries_its_runs_and_says_when_one_is_live() {
        let s = store();
        let dir = format!("/tmp/jod-tree-mgr-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let (manager, _) = s
            .manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .unwrap();
        run_for(&s, &manager, "mgr-run", "running");

        let nodes = s.forest().unwrap();
        let at = nodes.iter().position(|n| n.kind == NodeKind::Manager).unwrap();

        assert!(nodes[at].running, "a manager with a live run is live: {nodes:?}");
        assert!(nodes[at].has_children, "and it has the run to show for it");

        let run = &nodes[at + 1];
        assert_eq!(run.kind, NodeKind::Run, "the run sits directly below: {nodes:?}");
        assert_eq!(run.id, NodeId::run("mgr-run"));
        assert_eq!(run.parent, Some(NodeId::manager(&manager)));
        assert_eq!(run.depth, 2, "one level under the manager");
        assert_eq!(run.status.as_deref(), Some("running"));

        // And it cascades: a project whose manager is working is working.
        let project_row = nodes.iter().find(|n| n.kind == NodeKind::Project).unwrap();
        assert!(project_row.running, "the project says so too: {nodes:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A repository Jod has been told about but not yet opened work in.
    ///
    /// The forest was built from works alone, so the whole project — heading,
    /// manager and the run the manager was in the middle of — appeared only
    /// once a work existed. That is precisely the stretch somebody watches to
    /// see whether the instruction was picked up.
    #[test]
    fn a_project_with_a_manager_and_no_work_yet_is_still_on_the_fleet() {
        let s = store();
        let dir = format!("/tmp/jod-tree-nowork-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let (manager, _) = s
            .manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .unwrap();

        let nodes = s.forest().unwrap();
        assert_eq!(nodes.len(), 2, "the project and its manager: {nodes:?}");
        assert_eq!(nodes[0].kind, NodeKind::Project);
        assert_eq!(nodes[1].id, NodeId::manager(&manager));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A repository nobody has said anything about has no chain to draw.
    #[test]
    fn a_tracked_project_with_neither_works_nor_a_manager_stays_off_the_fleet() {
        let s = store();
        let dir = format!("/tmp/jod-tree-bare-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        s.add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();

        assert_eq!(s.forest().unwrap(), Vec::new());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Jod is the row above every repository, and his runs hang from it.
    ///
    /// He is not their parent — he owns no checkout — so the projects stay at
    /// depth 0 beside him rather than underneath.
    #[test]
    fn jod_gets_the_first_row_and_his_runs_hang_from_it() {
        let s = store();
        let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        run_for(&s, &main, "main-run", "running");
        let work = s.create_work("a job").unwrap();
        session(&s, &work.id, None, "engineer");

        let nodes = s.forest().unwrap();

        assert_eq!(nodes[0].kind, NodeKind::Main, "{nodes:?}");
        assert_eq!(nodes[0].id, NodeId::main(&main));
        assert_eq!(nodes[0].label, "jod");
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].parent, None);
        assert!(nodes[0].running, "his run is live, so he is");

        assert_eq!(nodes[1].kind, NodeKind::Run);
        assert_eq!(nodes[1].id, NodeId::run("main-run"));
        assert_eq!(nodes[1].parent, Some(NodeId::main(&main)));
        assert_eq!(nodes[1].depth, 1);

        assert_eq!(nodes[2].kind, NodeKind::Work, "{nodes:?}");
        assert_eq!(nodes[2].depth, 0, "a work is beside Jod, not under him");
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

    // ─── the fold ────────────────────────────────────────────────────────────

    /// Jod survives the fold as a top-level row, and his runs fold into him.
    ///
    /// The fold drops every run row, so without an arm of his own he would be
    /// dropped too — the fleet would lose the row for the conversation every
    /// instruction arrives in, and the amber tier with it.
    #[test]
    fn the_fold_keeps_jod_and_reads_his_runs_onto_his_row() {
        let s = store();
        let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        run_for(&s, &main, "main-run", "running");

        let folded = condense(&s.forest().unwrap(), &HashSet::new());

        assert_eq!(folded.nodes.len(), 1, "one row, no run beneath: {:?}", folded.nodes);
        assert_eq!(folded.nodes[0].kind, NodeKind::Main);
        assert_eq!(folded.nodes[0].depth, 0);
        assert!(!folded.nodes[0].has_children);
        assert_eq!(
            folded.run_of.get(&NodeId::main(&main)).map(String::as_str),
            Some("main-run"),
            "his row answers for the run the fold removed"
        );
        assert!(folded.runs.contains("main-run"), "and the run is accounted for");
    }

    /// A manager's runs fold onto its row the same way, so the row that was
    /// unopenable before this branch stays openable after the fold.
    #[test]
    fn the_fold_keeps_a_manager_and_the_run_it_answers_for() {
        let s = store();
        let dir = format!("/tmp/jod-fold-mgr-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let (manager, _) = s
            .manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .unwrap();
        run_for(&s, &manager, "mgr-run", "running");

        let folded = condense(&s.forest().unwrap(), &HashSet::new());
        let row = folded
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Manager)
            .unwrap_or_else(|| panic!("no manager row: {:?}", folded.nodes));

        assert_eq!(row.depth, 1, "under its project");
        assert_eq!(
            folded.run_of.get(&NodeId::manager(&manager)).map(String::as_str),
            Some("mgr-run")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A work with no project keeps its own heading, and its agents stay under
    /// it.
    ///
    /// `forest_of` emits every project first and the works with a null
    /// `project_id` after all of them, so a fold that decides "does this work
    /// belong to a repository" by asking whether a project has been *seen* says
    /// yes for every loose work — and files its agents under whichever
    /// repository happened to come last. Read off the node instead.
    #[test]
    fn a_loose_work_keeps_its_own_heading_after_a_project() {
        let s = store();
        let dir = format!("/tmp/jod-fold-loose-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let owned = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        session(&s, &owned.id, None, "engineer");

        let loose = s.create_work("an old job").unwrap();
        s.set_work_title(&loose.id, "an old job").unwrap();
        let stray = session(&s, &loose.id, None, "stray");

        let folded = condense(&s.forest().unwrap(), &HashSet::new());
        let row = folded
            .nodes
            .iter()
            .find(|n| n.id == NodeId::session(&stray))
            .unwrap_or_else(|| panic!("no stray row: {:?}", folded.nodes));

        assert_eq!(
            row.parent,
            Some(NodeId::work(&loose.id)),
            "the stray agent hangs from its own work, not from tetris: {:?}",
            folded.nodes
        );
        assert_eq!(row.depth, 1, "and at the top level's child depth");

        std::fs::remove_dir_all(&dir).ok();
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

    /// Untracking a project takes it and everything under it off the fleet.
    ///
    /// The whole point of the verb. Before this, archiving took a repository
    /// off the catalog panel and left the tree drawing it unchanged, because
    /// the tree builds from `works` and reads the project afterwards only to
    /// find a heading — so the one screen you would look at to check it had
    /// worked was the one screen that said it had not.
    #[test]
    fn an_untracked_project_takes_its_whole_subtree_off_the_fleet() {
        let s = store();
        let dir = format!("/tmp/jod-tree-untrack-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        s.manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .unwrap();
        let work = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        session(&s, &work.id, None, "engineer");

        assert_eq!(s.forest().unwrap().len(), 4, "project, manager, work, session");

        s.set_project_state(&project.id, crate::projects::State::Archived)
            .unwrap();

        assert!(
            s.forest().unwrap().is_empty(),
            "the works go with the heading rather than being promoted to the top \
             level, which would move the rows up rather than take them off: {:?}",
            s.forest().unwrap()
        );

        // Nothing was deleted, so putting it back is one call and the subtree
        // returns whole.
        s.set_project_state(&project.id, crate::projects::State::Active)
            .unwrap();
        assert_eq!(s.forest().unwrap().len(), 4, "restore brings the subtree back");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pausing is not untracking, and the fleet is where the two would be
    /// easiest to conflate. `Paused` means dormant, and it sides with `Active`
    /// on every listing — only `Archived` comes off them.
    #[test]
    fn a_paused_project_stays_on_the_fleet() {
        let s = store();
        let dir = format!("/tmp/jod-tree-paused-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let project = s
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let work = s
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        session(&s, &work.id, None, "engineer");

        s.set_project_state(&project.id, crate::projects::State::Paused)
            .unwrap();

        let nodes = s.forest().unwrap();
        assert_eq!(nodes[0].kind, NodeKind::Project, "{nodes:?}");
        assert_eq!(nodes[0].label, "tetris");

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
