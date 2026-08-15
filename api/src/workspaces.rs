//! The read surface behind the TUI's workspaces.
//!
//! `jod tui` has nine screens; before this module the HTTP API could answer for
//! two of them (fleet and team). A browser or a phone asking for memory,
//! schedules, goals, webhooks, tasks or activity had nowhere to ask, so the
//! desktop and web HUDs could only ever render the fleet. These routes close
//! that gap.
//!
//! ## Store-faithful, not screen-faithful
//!
//! Every handler here is a read of [`jod_core::store::Store`] serialised more or
//! less as the store holds it. That is a deliberate line, and it is worth saying
//! why, because the TUI already has richer types.
//!
//! `cli/src/tui/data.rs` builds `ScheduleRow`, `GoalRow`, `HookRow` and friends,
//! which carry things like `gloss: "02:00 every day"`, `secret: "✓ verified 2m
//! ago"` and a seven-slot outcome sparkline. Those are *presentation*: a cron
//! gloss is a sentence in English, and a relative timestamp is only true for the
//! second in which it was rendered. Serving them over HTTP would push one
//! client's rendering choices onto every other client, and would stale the
//! moment the response sat in a cache. So the API sends `cron`, `timezone` and
//! `next_fire_at_ms`, and each client writes its own gloss.
//!
//! The rule this follows is the crate's own: *this crate adds no orchestration
//! logic of its own*. Where a screen needs two tables joined, the join belongs
//! in core and this module calls it — [`fleet`] hands back `Store::forest_of`
//! unchanged, and [`list_activity`] is a passthrough to `jod_core::activity`.
//! The activity feed is why that rule is worth stating twice: it used to be
//! composed here, in parallel with the terminal's copy, and the two had already
//! drifted far enough that this route was missing an entire source.
//!
//! What is still assembled here is only the shaping a single response needs —
//! [`crate::routes::TeamView`]'s argument, that one screen should not cost two
//! round trips that can tear against each other.
//!
//! ## Everything is `Scope::Read`
//!
//! No route in this module writes. Pausing a schedule, answering a goal, testing
//! a webhook payload and marking activity read are all *writes*, and they are
//! deliberately absent: the same reasoning as [`crate::routes::list_teams`] —
//! a remote client watches, it does not play. Adding them is a separate decision
//! with its own audit-trail obligations, not an afterthought to a read surface.
//!
//! ## An empty store is not an error
//!
//! A `Jod` with no store is legal — it means nothing is persisted — and a
//! freshly initialised database is mostly empty tables. Both answer with an
//! empty list, never a 404 and never a 500. "There are no goals" is a fact, and
//! the screen that asks wants to draw "no goals yet", not an error banner.

use axum::extract::{Extension, Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use jod_core::schedule::{Fire, Goal, Schedule};
use jod_core::store::{Edge, MemoryNode, Store};
use jod_core::team::TeamTask;
use jod_core::webhook::{Delivery, Rule};
use jod_core::works::Filter;
use serde::{Deserialize, Serialize};

use crate::auth::Scope;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, Identity};

/// How many rows a list route returns when the caller does not say.
///
/// Generous, because these tables are small in practice and a workspace draws
/// the whole list; bounded, because "small in practice" is not a guarantee and
/// an unbounded query is a denial of service with extra steps.
const DEFAULT_LIMIT: usize = 200;

/// The ceiling a caller cannot raise past, whatever `?limit=` says.
const MAX_LIMIT: usize = 1000;

/// Clamp a caller-supplied limit into something the store can be asked for.
///
/// `Some(0)` is honoured as zero rather than snapped to the default: asking for
/// no rows is a coherent request (a client that only wants the counts), and
/// silently returning 200 of them would be a surprise.
fn limit_of(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
}

/// Lift a store error into a 500 without leaking the SQL.
fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

/// The store, or `None` when this `Jod` has no persistence.
///
/// Handlers use this to answer "nothing" rather than to fail — see the module
/// note on empty stores.
fn store_of(state: &AppState) -> Option<&Store> {
    state.jod.store().map(|s| &**s)
}

// ─── memory ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MemoryQuery {
    /// Restrict to one memory scope. Omitted means every scope.
    pub scope: Option<String>,
    pub limit: Option<usize>,
}

/// The memory workspace's list, plus the two counts its status line shows.
///
/// The counts come back with the list because the TUI's footer reads
/// `2 nodes · 1 edge` beside the rows themselves. They are whole-graph totals,
/// deliberately not "how many did this page return" — a client that paginates
/// still wants to say how much memory exists.
#[derive(Debug, Serialize)]
pub struct MemoryPage {
    pub nodes: Vec<MemoryNode>,
    pub node_count: usize,
    pub edge_count: usize,
}

pub async fn list_memory(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<MemoryQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(MemoryPage {
            nodes: Vec::new(),
            node_count: 0,
            edge_count: 0,
        }));
    };

    let nodes = store
        .memory_nodes(q.scope.as_deref(), limit_of(q.limit))
        .map_err(internal)?;
    let (node_count, edge_count) = store.graph_size().map_err(internal)?;

    Ok(Json(MemoryPage {
        nodes,
        node_count,
        edge_count,
    }))
}

/// One memory node with its edges split by direction.
///
/// In and out are separate fields rather than one list with a flag because the
/// detail pane draws them as two headed sections (`▲ linked from`, `▼ links
/// to`), and `contradicts` read backwards is a different claim from
/// `contradicts` read forwards.
#[derive(Debug, Serialize)]
pub struct MemoryNodeView {
    #[serde(flatten)]
    pub node: MemoryNode,
    pub in_edges: Vec<Edge>,
    pub out_edges: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
pub struct NodeQuery {
    pub limit: Option<usize>,
}

pub async fn get_memory_node(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Query(q): Query<NodeQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store =
        store_of(&state).ok_or_else(|| ApiError::NotFound(format!("no memory node {id}")))?;

    let node =
        find_node(store, id)?.ok_or_else(|| ApiError::NotFound(format!("no memory node {id}")))?;
    let edges = store.edges_of(id, limit_of(q.limit)).map_err(internal)?;
    let (out_edges, in_edges): (Vec<Edge>, Vec<Edge>) = edges.into_iter().partition(|e| e.outgoing);

    Ok(Json(MemoryNodeView {
        node,
        in_edges,
        out_edges,
    }))
}

/// Look a node up by id.
///
/// The store exposes `memory_nodes` as a scan and no by-id read, so this filters
/// the scan. It is bounded by [`MAX_LIMIT`] rather than unbounded, which means a
/// node beyond that horizon reads as missing — acceptable while the graph is
/// thousands of rows, and the honest fix is a by-id query in core rather than an
/// unbounded scan here. Core's store is another lane's file, so this does not
/// reach into it.
fn find_node(store: &Store, id: i64) -> ApiResult<Option<MemoryNode>> {
    Ok(store
        .memory_nodes(None, MAX_LIMIT)
        .map_err(internal)?
        .into_iter()
        .find(|n| n.id == id))
}

#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    /// Hops out from the root. Core clamps this to its own ceiling.
    pub depth: Option<u32>,
    pub limit: Option<usize>,
}

/// A node in the local graph, and how far out it sits.
#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub id: i64,
    pub name: String,
    pub kind: String,
    /// 0 for the root itself.
    pub hops: i64,
}

/// A directed edge between two nodes that are both in [`LocalGraph::nodes`].
///
/// Direction is carried explicitly as `from`/`to` rather than as a flag on a
/// neighbour, so a renderer can draw an arrowhead without knowing which node it
/// asked about. `core/src/tui/graph.rs` refuses a node-link drawing because a
/// terminal cannot honestly place one; a canvas can, and this is the shape that
/// lets it.
#[derive(Debug, Serialize, PartialEq, Eq, Hash)]
pub struct GraphEdge {
    pub from: i64,
    pub to: i64,
    pub predicate: String,
}

#[derive(Debug, Serialize)]
pub struct LocalGraph {
    pub root_id: i64,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The neighbourhood around one node, as nodes plus directed edges.
///
/// Assembled from two store reads because neither answers alone:
/// `neighbourhood` walks the graph but returns only nodes and their hop count,
/// and `edges_of` returns edges but only for one node. So: walk to get the node
/// set, then ask each node for its edges and keep the ones whose other end is
/// also in the set. Edges to nodes outside the horizon are dropped rather than
/// dangling — an edge pointing at a node the client was not sent is something a
/// renderer cannot draw.
pub async fn memory_graph(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Query(q): Query<GraphQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store =
        store_of(&state).ok_or_else(|| ApiError::NotFound(format!("no memory node {id}")))?;

    let root =
        find_node(store, id)?.ok_or_else(|| ApiError::NotFound(format!("no memory node {id}")))?;
    let depth = q.depth.unwrap_or(1);
    let edge_limit = limit_of(q.limit);

    // `neighbourhood` is keyed by (scope, name) rather than by id.
    let reached = store
        .neighbourhood(
            &root.scope,
            &root.name,
            depth,
            chrono::Utc::now().timestamp_millis(),
        )
        .map_err(internal)?;

    let mut nodes = vec![GraphNode {
        id: root.id,
        name: root.name.clone(),
        kind: root.kind.clone(),
        hops: 0,
    }];
    for n in reached {
        // The walk can include the root at hop 0; keep exactly one copy of it.
        if n.id == root.id {
            continue;
        }
        nodes.push(GraphNode {
            id: n.id,
            name: n.name,
            kind: n.kind,
            hops: n.hops,
        });
    }

    let ids: std::collections::HashSet<i64> = nodes.iter().map(|n| n.id).collect();

    // A pair of nodes is reported by both ends, so the same edge arrives twice;
    // dedupe on the directed triple rather than on the unordered pair, because
    // A→B and B→A are two different claims and both may exist.
    let mut seen = std::collections::HashSet::new();
    let mut edges = Vec::new();
    for node in &nodes {
        for e in store.edges_of(node.id, edge_limit).map_err(internal)? {
            if !ids.contains(&e.other_id) {
                continue;
            }
            let edge = if e.outgoing {
                GraphEdge {
                    from: node.id,
                    to: e.other_id,
                    predicate: e.predicate,
                }
            } else {
                GraphEdge {
                    from: e.other_id,
                    to: node.id,
                    predicate: e.predicate,
                }
            };
            if seen.insert((edge.from, edge.to, edge.predicate.clone())) {
                edges.push(edge);
            }
        }
    }

    Ok(Json(LocalGraph {
        root_id: root.id,
        nodes,
        edges,
    }))
}

// ─── schedules ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub limit: Option<usize>,
}

pub async fn list_schedules(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<Schedule>::new()));
    };
    store.schedules().map(Json).map_err(internal)
}

/// A schedule and its recent fires — one screen, one request.
#[derive(Debug, Serialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    /// Most recent first, as the store orders them.
    pub fires: Vec<Fire>,
}

pub async fn get_schedule(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(name): Path<String>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store =
        store_of(&state).ok_or_else(|| ApiError::NotFound(format!("no schedule named {name}")))?;

    let schedule = store
        .schedule_named(&name)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("no schedule named {name}")))?;
    let fires = store
        .fires(&schedule.id, limit_of(q.limit))
        .map_err(internal)?;

    Ok(Json(ScheduleView { schedule, fires }))
}

// ─── goals ───────────────────────────────────────────────────────────────────

pub async fn list_goals(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<Goal>::new()));
    };
    store.goals().map(Json).map_err(internal)
}

pub async fn get_goal(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(name): Path<String>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store =
        store_of(&state).ok_or_else(|| ApiError::NotFound(format!("no goal named {name}")))?;
    store
        .goal_named(&name)
        .map_err(internal)?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no goal named {name}")))
}

// ─── webhooks ────────────────────────────────────────────────────────────────

/// A webhook rule with the deliveries that matched it.
///
/// The rule and its traffic travel together for the same reason the team view
/// bundles members and tasks: the screen shows both, and separately fetched
/// halves can disagree about the moment they describe.
#[derive(Debug, Serialize)]
pub struct HookView {
    #[serde(flatten)]
    pub rule: Rule,
    pub deliveries: Vec<Delivery>,
}

pub async fn list_hooks(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<HookView>::new()));
    };

    let rules = store.webhook_rules().map_err(internal)?;
    // One query for every rule's deliveries rather than one per rule: they share
    // a table, and this screen wants all of them. The same choice `data.rs`
    // makes, for the same reason.
    let deliveries = store.deliveries(limit_of(q.limit)).map_err(internal)?;

    Ok(Json(
        rules
            .into_iter()
            .map(|rule| {
                let mine = deliveries
                    .iter()
                    .filter(|d| d.rule_id.as_deref() == Some(rule.id.as_str()))
                    .cloned()
                    .collect();
                HookView {
                    rule,
                    deliveries: mine,
                }
            })
            .collect::<Vec<_>>(),
    ))
}

// ─── tasks ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TasksQuery {
    /// Which team's board. Omitted means the first team that exists, which is
    /// what a single-team box wants and what the TUI assumes.
    ///
    /// "Exists" means *has a member*, following `teams()` and the same rule
    /// [`crate::routes::list_teams`] uses. A board whose team nobody has joined
    /// is not reachable without naming the team explicitly — deliberate, since
    /// a team comes into being by someone joining it.
    pub team: Option<String>,
}

pub async fn list_tasks(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<TasksQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<TeamTask>::new()));
    };

    let team = match q.team {
        Some(t) => Some(t),
        // No team at all is an empty board, not an error: `jod tui` without
        // `--team` is the ordinary case.
        None => store.teams().map_err(internal)?.into_iter().next(),
    };
    let Some(team) = team else {
        return Ok(Json(Vec::new()));
    };

    store.team_tasks(&team).map(Json).map_err(internal)
}

// ─── activity ────────────────────────────────────────────────────────────────

/// The activity feed's types, which are core's.
///
/// Re-exported under their old names so this module's public surface is
/// unchanged, but they are no longer defined here: the projection moved to
/// [`jod_core::activity`] so that the terminal and this route cannot disagree
/// about what activity is. `needs_you` is still the field the screen exists for.
pub use jod_core::activity::{Item as ActivityItem, Jump, Source as ActivitySource};

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<usize>,
    /// Only lines that want a human. Default false — the feed is a feed.
    pub needs_you: Option<bool>,
}

/// The activity feed: schedule fires, goal iterations and webhook deliveries,
/// newest first.
///
/// A passthrough to [`jod_core::activity::feed`]. It used to compose the feed
/// itself, in parallel with `cli/src/tui/data.rs::activity`, and the drift that
/// arrangement invited had already happened: this route had no webhook source at
/// all, so a rejected signature or a rule that could not start its run was
/// visible only to whoever was sitting at the terminal. Those are precisely the
/// silences `needs_you` exists to surface. The rule now lives in one place and
/// every client gets the same three sources.
///
/// Unread state is still deliberately absent. Read state is a fact about a
/// person, not about an event, and there is nowhere to put it yet; inventing a
/// server-side notion of "read" would only make two clients disagree about it.
/// A client that wants unread tracks it locally.
pub async fn list_activity(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<ActivityItem>::new()));
    };

    let query = jod_core::activity::Query::with_limit(limit_of(q.limit))
        .needing_you(q.needs_you.unwrap_or(false));

    // Propagated rather than swallowed: a 200 carrying a short list reads as
    // "nothing happened", and a client cannot tell that from a store that would
    // not answer. The terminal makes the opposite call for its own good reasons.
    jod_core::activity::feed(store, query)
        .map(Json)
        .map_err(internal)
}

// ─── fleet ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FleetQuery {
    /// `live` (the default), `closed` or `all`. Anything else is `live` rather
    /// than a 400: a filter is a view preference, and refusing to draw the
    /// screen because a query string was misspelled is the wrong trade.
    pub filter: Option<String>,
}

/// The fleet tree: every work, its sessions and their runs, already flattened.
///
/// **The same forest the TUI draws.** `Store::forest_of` is one function in
/// `jod-core`, and this route hands its output over HTTP unchanged — no second
/// flatten, no API-side notion of what a work is. That is the whole point: the
/// fleet screen was terminal-only not because a browser could not draw a tree,
/// but because the tree was never on the wire.
///
/// It is a **query against the store**, so it says the same thing whichever
/// process asks and whoever started the runs. Unlike `/v1/agents` — which is
/// served from the answering process's own memory — nothing here depends on
/// this daemon having launched anything.
pub async fn fleet(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<FleetQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::new()));
    };
    store.forest_of(filter_of(q.filter.as_deref())).map(Json).map_err(internal)
}

/// Read a filter off the query string, defaulting rather than refusing.
///
/// Split out so the defaulting is pinned by a test without going through HTTP.
fn filter_of(requested: Option<&str>) -> Filter {
    match requested {
        Some("all") => Filter::All,
        Some("closed") => Filter::Closed,
        _ => Filter::Live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::schedule::FireOutcome;

    #[test]
    fn a_limit_defaults_when_absent_and_is_capped_when_absurd() {
        assert_eq!(limit_of(None), DEFAULT_LIMIT);
        assert_eq!(limit_of(Some(10)), 10);
        assert_eq!(limit_of(Some(usize::MAX)), MAX_LIMIT);
    }

    /// Zero is a coherent request, not a mistake to be corrected into 200.
    #[test]
    fn asking_for_no_rows_returns_no_rows() {
        assert_eq!(limit_of(Some(0)), 0);
    }

    /// The reason the activity screen exists. The predicate itself is pinned in
    /// `jod_core::activity`, which is now the only place it is written down —
    /// this asserts the route's re-export still reaches it, so a rename cannot
    /// quietly leave the API pointing at a second copy.
    #[test]
    fn only_silent_failures_ask_for_a_human() {
        use jod_core::activity::fire_needs_you;
        assert!(fire_needs_you(FireOutcome::SpawnFailed));
        assert!(fire_needs_you(FireOutcome::Abandoned));
        assert!(!fire_needs_you(FireOutcome::Ran));
        assert!(!fire_needs_you(FireOutcome::SkippedOverlap));
        assert!(!fire_needs_you(FireOutcome::Replaced));
    }

    /// The wire vocabulary a client matches on. `hook` is the variant this
    /// route gained when the projection moved to core, and the one an older
    /// client will not have seen.
    #[test]
    fn the_feed_can_name_all_three_of_its_sources() {
        assert_eq!(
            serde_json::to_string(&ActivitySource::Cron).unwrap(),
            "\"cron\""
        );
        assert_eq!(
            serde_json::to_string(&ActivitySource::Goal).unwrap(),
            "\"goal\""
        );
        assert_eq!(
            serde_json::to_string(&ActivitySource::Hook).unwrap(),
            "\"hook\""
        );
    }

    /// `jump_to` is a two-element array on the wire, and the first element is
    /// the screen name a client routes on. Moving it behind an enum must not
    /// have changed either.
    #[test]
    fn a_jump_still_serialises_as_screen_then_row() {
        let json = serde_json::to_string(&Some((Jump::Hooks, "nightly".to_string()))).unwrap();
        assert_eq!(json, "[\"hooks\",\"nightly\"]");
    }
}
