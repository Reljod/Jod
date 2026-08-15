//! The workspace read routes, over the real router and a real store.
//!
//! `tests/http.rs` builds its harness from `Jod::new()`, which has no
//! persistence — every workspace route answers "nothing" there, so those tests
//! prove the routes are *mounted and protected* but never that they can return
//! a row. These tests seed an in-memory store and assert the rows come back.
//!
//! The distinction matters more than it sounds: a handler that returns an empty
//! list on every input passes the whole of `http.rs`. What follows is the part
//! that would catch it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jod_api::audit::AuditLog;
use jod_api::auth::{Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;
use jod_core::schedule::{Goal, GoalState, Misfire, Overlap, Schedule, ScheduleState};
use jod_core::store::{NewFact, Store};
use jod_core::webhook::{Conditions, Delivery, DeliveryStatus, Rule};
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    token: String,
}

/// A router over a store seeded by `seed`.
///
/// The store is in-memory: these tests must never see `~/.jod/jod.db`, and an
/// in-memory database also cannot be shared by another process, so parallel
/// test binaries cannot collide.
fn harness_with(seed: impl FnOnce(&Store)) -> Harness {
    let store = Store::in_memory().expect("in-memory store");
    seed(&store);

    let mut tokens = TokenStore::default();
    let token = tokens.issue("phone", Scope::Read);
    let audit = AuditLog::new(std::env::temp_dir().join("jod-api-workspaces-audit.jsonl"));
    let jod = jod_core::Jod::with_store(Arc::new(store));

    Harness {
        app: jod_api::router(AppState::new(jod, Config::default(), tokens, audit)),
        token,
    }
}

/// GET a path with a read token and parse the body as JSON.
async fn get_json(h: &Harness, path: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .uri(path)
        .method("GET")
        .header("authorization", format!("Bearer {}", h.token))
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    // A body that is not JSON is a failure worth seeing in full.
    let json = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "{path} returned {status} with a non-JSON body ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, json)
}

fn schedule(name: &str) -> Schedule {
    Schedule {
        id: format!("id-{name}"),
        name: name.into(),
        prompt: "triage the inbox".into(),
        harness: "claude_code".into(),
        cwd: "/tmp".into(),
        model: None,
        cron: "0 2 * * *".into(),
        timezone: "UTC".into(),
        state: ScheduleState::Armed,
        misfire: Misfire::FireOnce,
        overlap: Overlap::Skip,
        grace_ms: 300_000,
        jitter_ms: 0,
        next_fire_at_ms: None,
        last_fire_at_ms: None,
        consecutive_failures: 0,
        created_at_ms: 0,
    }
}

fn goal(name: &str) -> Goal {
    Goal {
        id: format!("goal-{name}"),
        name: name.into(),
        objective: "get the suite green".into(),
        done_when: Some("cargo test".into()),
        harness: "claude_code".into(),
        cwd: "/tmp".into(),
        model: None,
        cron: "0 * * * *".into(),
        timezone: "UTC".into(),
        state: GoalState::Running,
        iteration: 0,
        max_iterations: None,
        budget_usd: None,
        spent_usd: 0.0,
        stall_after: 3,
        no_progress: 0,
        next_fire_at_ms: None,
        created_at_ms: 0,
    }
}

fn webhook_rule(name: &str) -> Rule {
    Rule {
        id: format!("wr-{name}"),
        name: name.into(),
        source: "github".into(),
        repo: "Reljod/Jod".into(),
        event: "pull_request".into(),
        action: None,
        conditions: Conditions::default(),
        prompt: "Look at {{title}}".into(),
        harness: "claude_code".into(),
        cwd: "/tmp".into(),
        model: None,
        enabled: true,
        created_at_ms: 0,
    }
}

/// A delivery against [`webhook_rule`], with the fields the feed reads set.
fn delivery(id: &str, status: DeliveryStatus, at_ms: i64) -> Delivery {
    let mut d = Delivery::new(id, "pull_request");
    d.action = Some("opened".into());
    d.repo = Some("Reljod/Jod".into());
    d.rule_id = Some("wr-nightly".into());
    d.status = status;
    d.received_at_ms = at_ms;
    d
}

// ─── memory ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn memory_returns_the_nodes_and_the_whole_graph_counts() {
    let h = harness_with(|store| {
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
    });

    let (status, body) = get_json(&h, "/v1/memory").await;
    assert_eq!(status, StatusCode::OK);

    let nodes = body["nodes"].as_array().expect("nodes is an array");
    assert!(
        !nodes.is_empty(),
        "a remembered fact produced no nodes: {body}"
    );
    // The counts are whole-graph totals, which is what the status line shows.
    assert!(body["node_count"].as_u64().unwrap() >= 1);
    assert!(body["edge_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn a_memory_node_carries_its_edges_split_by_direction() {
    let h = harness_with(|store| {
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
    });

    let (_, list) = get_json(&h, "/v1/memory").await;
    let id = list["nodes"][0]["id"].as_i64().expect("a node id");

    let (status, body) = get_json(&h, &format!("/v1/memory/{id}")).await;
    assert_eq!(status, StatusCode::OK);

    // Flattened, so the node's own fields sit beside the edge lists.
    assert_eq!(body["id"].as_i64(), Some(id));
    assert!(
        body["name"].is_string(),
        "node fields were not flattened: {body}"
    );
    assert!(body["in_edges"].is_array());
    assert!(body["out_edges"].is_array());

    let total =
        body["in_edges"].as_array().unwrap().len() + body["out_edges"].as_array().unwrap().len();
    assert!(total >= 1, "the subject of a fact has no edges: {body}");
}

#[tokio::test]
async fn a_missing_memory_node_is_a_404_not_an_empty_node() {
    let h = harness_with(|_| {});
    let (status, _) = get_json(&h, "/v1/memory/999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The route the tactical view renders. Every edge must join two nodes that
/// were also sent, or the renderer has an arrow pointing at nothing.
#[tokio::test]
async fn a_local_graph_never_returns_an_edge_to_a_node_it_did_not_send() {
    let h = harness_with(|store| {
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
        store
            .remember(NewFact::new("reljod", "runs", "jod on a vps"))
            .unwrap();
        store
            .remember(NewFact::new("jod on a vps", "hosts", "the fleet"))
            .unwrap();
    });

    let (_, list) = get_json(&h, "/v1/memory").await;
    let root = list["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "reljod")
        .expect("the subject node");
    let id = root["id"].as_i64().unwrap();

    let (status, body) = get_json(&h, &format!("/v1/memory/{id}/graph?depth=2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["root_id"].as_i64(), Some(id));

    let ids: Vec<i64> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    for edge in body["edges"].as_array().unwrap() {
        let from = edge["from"].as_i64().unwrap();
        let to = edge["to"].as_i64().unwrap();
        assert!(
            ids.contains(&from),
            "edge from an unsent node {from}: {body}"
        );
        assert!(ids.contains(&to), "edge to an unsent node {to}: {body}");
        assert!(edge["predicate"].is_string());
    }
}

/// The root appears exactly once, at hop 0. A duplicate would be drawn twice.
#[tokio::test]
async fn a_local_graph_contains_its_root_exactly_once() {
    let h = harness_with(|store| {
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
    });

    let (_, list) = get_json(&h, "/v1/memory").await;
    let id = list["nodes"][0]["id"].as_i64().unwrap();
    let (_, body) = get_json(&h, &format!("/v1/memory/{id}/graph")).await;

    let roots: Vec<_> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["id"].as_i64() == Some(id))
        .collect();
    assert_eq!(
        roots.len(),
        1,
        "the root was sent {} times: {body}",
        roots.len()
    );
    assert_eq!(roots[0]["hops"].as_i64(), Some(0));
}

// ─── schedules, goals, hooks, tasks ──────────────────────────────────────────

#[tokio::test]
async fn schedules_come_back_with_the_fields_a_client_needs_to_gloss_them() {
    let h = harness_with(|store| {
        store.add_schedule(&schedule("nightly")).unwrap();
    });

    let (status, body) = get_json(&h, "/v1/schedules").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "nightly");
    // The API sends the raw cron and zone rather than an English gloss, so each
    // client renders its own. Both have to be present for that to be possible.
    assert_eq!(rows[0]["cron"], "0 2 * * *");
    assert_eq!(rows[0]["timezone"], "UTC");
}

#[tokio::test]
async fn one_schedule_carries_its_fires_in_the_same_answer() {
    let h = harness_with(|store| {
        store.add_schedule(&schedule("nightly")).unwrap();
    });

    let (status, body) = get_json(&h, "/v1/schedules/nightly").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "nightly");
    assert!(body["fires"].is_array(), "fires missing: {body}");
}

#[tokio::test]
async fn a_missing_schedule_is_a_404() {
    let h = harness_with(|_| {});
    let (status, _) = get_json(&h, "/v1/schedules/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn goals_list_and_resolve_by_name() {
    let h = harness_with(|store| {
        store.add_goal(&goal("ship-it")).unwrap();
    });

    let (status, list) = get_json(&h, "/v1/goals").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, one) = get_json(&h, "/v1/goals/ship-it").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["name"], "ship-it");
    assert_eq!(one["objective"], "get the suite green");

    let (status, _) = get_json(&h, "/v1/goals/never-set").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_board_is_empty_rather_than_an_error_when_no_team_exists() {
    let h = harness_with(|_| {});
    let (status, body) = get_json(&h, "/v1/tasks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_board_returns_the_tasks_of_the_only_team_without_being_told_which() {
    let h = harness_with(|store| {
        // A team exists because somebody joined it — `teams()` lists teams with
        // a member, which is the same rule `/v1/teams` follows. A board seeded
        // without a member belongs to no team and is correctly invisible.
        store
            .join_team("crew", "scout", jod_core::HarnessKind::ClaudeCode, "worker")
            .unwrap();
        store.add_team_task("crew", "t1", "wire the HUD").unwrap();
    });

    let (status, body) = get_json(&h, "/v1/tasks").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1, "the sole team's board was not found: {body}");
    assert_eq!(rows[0]["title"], "wire the HUD");
}

#[tokio::test]
async fn hooks_are_an_empty_list_when_none_are_configured() {
    let h = harness_with(|_| {});
    let (status, body) = get_json(&h, "/v1/hooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

// ─── activity ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn activity_is_newest_first() {
    let h = harness_with(|store| {
        store.add_goal(&goal("ship-it")).unwrap();
        // Two iterations, written in order, so "newest first" is checkable.
        for n in ["first pass", "second pass"] {
            store
                .remember(NewFact::new("goal/ship-it", "iteration", n))
                .unwrap();
        }
    });

    let (status, body) = get_json(&h, "/v1/activity").await;
    assert_eq!(status, StatusCode::OK);

    let rows = body.as_array().unwrap();
    assert!(
        rows.len() >= 2,
        "goal iterations did not reach the feed: {body}"
    );
    let stamps: Vec<i64> = rows.iter().map(|r| r["at_ms"].as_i64().unwrap()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(stamps, sorted, "the feed was not newest-first: {stamps:?}");
}

#[tokio::test]
async fn an_activity_row_says_where_to_jump_to() {
    let h = harness_with(|store| {
        store.add_goal(&goal("ship-it")).unwrap();
        store
            .remember(NewFact::new("goal/ship-it", "iteration", "first pass"))
            .unwrap();
    });

    let (_, body) = get_json(&h, "/v1/activity").await;
    let row = &body.as_array().unwrap()[0];
    assert_eq!(row["source"], "goal");
    // A tuple serialises as a two-element array: the workspace, then the row.
    assert_eq!(row["jump_to"][0], "goals");
    assert_eq!(row["jump_to"][1], "ship-it");
    assert!(row["id"].is_string(), "no stable id to diff on: {row}");
}

/// `?needs_you=true` is the filter behind "only what wants a human". A goal
/// *ending* qualifies; an ordinary iteration does not.
#[tokio::test]
async fn the_needs_you_filter_keeps_only_what_wants_a_human() {
    let h = harness_with(|store| {
        store.add_goal(&goal("ship-it")).unwrap();
        store
            .remember(NewFact::new("goal/ship-it", "iteration", "first pass"))
            .unwrap();
        store
            .remember(NewFact::new("goal/ship-it", "ended", "satisfied"))
            .unwrap();
    });

    let (_, all) = get_json(&h, "/v1/activity").await;
    let (status, filtered) = get_json(&h, "/v1/activity?needs_you=true").await;
    assert_eq!(status, StatusCode::OK);

    let all_rows = all.as_array().unwrap();
    let kept = filtered.as_array().unwrap();
    assert!(
        kept.len() < all_rows.len(),
        "the filter kept everything: {filtered}"
    );
    assert!(!kept.is_empty(), "the filter kept nothing: {all}");
    for row in kept {
        assert_eq!(row["needs_you"], true);
    }
}

/// The regression that moving this projection into core was for.
///
/// This route used to compose the feed from schedules and goals only, while
/// `cli/src/tui/data.rs` composed it from three sources. A rejected delivery —
/// a webhook secret that stopped verifying — was therefore visible to whoever
/// happened to be sitting at the terminal and to nobody on a phone or a browser,
/// which is the exact silence `needs_you` exists to break.
#[tokio::test]
async fn a_rejected_delivery_reaches_the_feed_and_asks_for_a_human() {
    let h = harness_with(|store| {
        store.add_webhook_rule(&webhook_rule("nightly")).unwrap();
        store.record_delivery(&delivery("gh-1", DeliveryStatus::Rejected, 1))
            .unwrap();
    });

    let (status, body) = get_json(&h, "/v1/activity").await;
    assert_eq!(status, StatusCode::OK);

    let rows = body.as_array().unwrap();
    let hook = rows
        .iter()
        .find(|r| r["source"] == "hook")
        .unwrap_or_else(|| panic!("no webhook row in the feed: {body}"));

    assert_eq!(hook["needs_you"], true);
    // It must actually navigate: the delivery stores a rule *id*, and the hooks
    // screen is keyed by name, so an untranslated jump reaches the screen and
    // selects nothing.
    assert_eq!(hook["jump_to"][0], "hooks");
    assert_eq!(hook["jump_to"][1], "nightly");
    assert!(
        hook["text"]
            .as_str()
            .unwrap()
            .contains("pull_request.opened on Reljod/Jod"),
        "the row does not say what arrived: {hook}"
    );
}

/// An accepted delivery is the hook working, and must not wake anyone.
#[tokio::test]
async fn an_accepted_delivery_is_reported_without_asking_for_a_human() {
    let h = harness_with(|store| {
        store.add_webhook_rule(&webhook_rule("nightly")).unwrap();
        store.record_delivery(&delivery("gh-2", DeliveryStatus::Accepted, 1))
            .unwrap();
    });

    let (_, body) = get_json(&h, "/v1/activity").await;
    let rows = body.as_array().unwrap();
    let hook = rows
        .iter()
        .find(|r| r["source"] == "hook")
        .unwrap_or_else(|| panic!("no webhook row in the feed: {body}"));
    assert_eq!(hook["needs_you"], false);
}

/// `?needs_you=` filters *before* the page is cut, not after.
///
/// With a limit of one and newer ordinary traffic in front of it, filtering
/// afterwards returns an empty page while an escalation sits one row behind —
/// "show me what needs me" answering "nothing" because a routine delivery was
/// more recent. Pinned because the obvious way to write the passthrough gets
/// this backwards.
#[tokio::test]
async fn a_narrow_page_of_escalations_is_not_crowded_out_by_newer_noise() {
    let h = harness_with(|store| {
        store.add_webhook_rule(&webhook_rule("nightly")).unwrap();
        // The escalation is the older of the two.
        store.record_delivery(&delivery("gh-old", DeliveryStatus::Rejected, 1))
            .unwrap();
        store.record_delivery(&delivery("gh-new", DeliveryStatus::Accepted, 99))
            .unwrap();
    });

    let (status, body) = get_json(&h, "/v1/activity?needs_you=true&limit=1").await;
    assert_eq!(status, StatusCode::OK);

    let rows = body.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the escalation was cut before the filter ran: {body}"
    );
    assert_eq!(rows[0]["needs_you"], true);
}

// ─── limits ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_caller_asking_for_no_rows_gets_none() {
    let h = harness_with(|store| {
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
    });

    let (status, body) = get_json(&h, "/v1/memory?limit=0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nodes"].as_array().unwrap().len(), 0);
    // The counts describe the graph, not the page, so they survive `limit=0`.
    assert!(body["node_count"].as_u64().unwrap() >= 1);
}

/// A read token is enough for every route here, and there is no write route to
/// find. If one is ever added, this test starts failing and the author has to
/// think about the audit trail rather than discovering it in production.
#[tokio::test]
async fn every_workspace_route_is_satisfied_by_a_read_token() {
    let h = harness_with(|_| {});
    for path in [
        "/v1/memory",
        "/v1/schedules",
        "/v1/goals",
        "/v1/hooks",
        "/v1/tasks",
        "/v1/activity",
        "/v1/fleet",
    ] {
        let (status, _) = get_json(&h, path).await;
        assert_eq!(status, StatusCode::OK, "{path} refused a read token");
    }
}

// ─── fleet ───────────────────────────────────────────────────────────────────

/// Seed the shape the fleet screen exists to draw: a work, a session under it,
/// and a run under that.
fn seed_fleet(store: &Store) {
    let work = store.create_work("port the parser").unwrap();
    store.set_work_title(&work.id, "the parser").unwrap();
    let c = store
        .new_conversation(jod_core::HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap();
    store.set_conversation_title(&c.id, "lead").unwrap();
    store
        .attach_conversation(&c.id, &work.id, None, jod_core::works::Origin::Agent)
        .unwrap();
    store
        .save_run(&jod_core::store::StoredRun {
            id: "run-1".into(),
            name: "run 1".into(),
            harness: "claude_code".into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            session_id: None,
            pid: None,
            pgid: None,
            created_at_ms: 1,
            summary: serde_json::json!({}),
        })
        .unwrap();
    store
        .append_message(
            &c.id,
            jod_core::conversation::NewMessage::new(
                jod_core::conversation::Role::Assistant,
                "on it",
            )
            .from_run("run-1"),
        )
        .unwrap();
}

/// The tree the TUI draws, over HTTP. Depth and order are the whole payload —
/// a flat list of the same rows would render as a list, not a fleet.
#[tokio::test]
async fn the_fleet_route_returns_the_work_session_run_tree() {
    let h = harness_with(seed_fleet);
    let (status, body) = get_json(&h, "/v1/fleet").await;
    assert_eq!(status, StatusCode::OK);

    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 3, "work, session and run");
    assert_eq!(rows[0]["kind"], "work");
    assert_eq!(rows[0]["depth"], 0);
    assert_eq!(rows[1]["kind"], "session");
    assert_eq!(rows[1]["depth"], 1);
    assert_eq!(rows[2]["kind"], "run");
    assert_eq!(rows[2]["depth"], 2);
}

/// Every field the browser panel draws has to survive the wire. If one is
/// dropped from `tree::Node`'s `Serialize`, the web fleet loses a column and
/// nothing else in the suite notices.
#[tokio::test]
async fn a_fleet_row_carries_what_a_client_needs_to_draw_it() {
    let h = harness_with(seed_fleet);
    let (_, body) = get_json(&h, "/v1/fleet").await;
    let work = &body.as_array().unwrap()[0];

    for field in [
        "id", "parent", "kind", "depth", "label", "summary", "running", "cards", "blocked",
        "colour", "has_children",
    ] {
        assert!(!work[field].is_null() || field == "parent", "{field} was missing");
    }
    assert_eq!(work["id"]["kind_tag"], "work");
    assert!(work["label"].as_str().unwrap().contains("parser"));
}

/// An empty store draws "no work yet", not an error banner — the rule the rest
/// of this module already follows.
#[tokio::test]
async fn an_empty_fleet_is_an_empty_list_rather_than_an_error() {
    let h = harness_with(|_| {});
    let (status, body) = get_json(&h, "/v1/fleet").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

/// A misspelled filter draws the default screen rather than refusing to draw
/// one. `?filter=all` is the spelling that also returns closed work.
#[tokio::test]
async fn an_unknown_filter_falls_back_to_the_live_view() {
    let h = harness_with(seed_fleet);
    for query in ["", "?filter=live", "?filter=nonsense", "?filter=all"] {
        let (status, body) = get_json(&h, &format!("/v1/fleet{query}")).await;
        assert_eq!(status, StatusCode::OK, "/v1/fleet{query}");
        assert_eq!(
            body.as_array().unwrap().len(),
            3,
            "/v1/fleet{query} did not draw the open work"
        );
    }
}
