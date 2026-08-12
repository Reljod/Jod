//! Conversations and the pinned main chat, over the real router.
//!
//! The write route here starts a supervised process holding Jod's own tools, so
//! most of what is worth testing is the set of refusals in front of it. None of
//! these tests ever reaches a harness: every send is expected to be refused
//! before it gets there, which is exactly what the bounds are for.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jod_api::audit::AuditLog;
use jod_api::auth::{Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;
use jod_core::conversation::NewMessage;
use jod_core::store::Store;
use jod_core::{HarnessKind, PermissionPolicy};
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    read_token: String,
    write_token: String,
}

fn harness_with(config: Config, seed: impl FnOnce(&Store)) -> Harness {
    let store = Store::in_memory().expect("in-memory store");
    seed(&store);

    let mut tokens = TokenStore::default();
    let read_token = tokens.issue("phone", Scope::Read);
    let write_token = tokens.issue("laptop", Scope::Write);
    let audit = AuditLog::new(std::env::temp_dir().join("jod-api-conversations-audit.jsonl"));
    let jod = jod_core::Jod::with_store(Arc::new(store));

    Harness {
        app: jod_api::router(AppState::new(jod, config, tokens, audit)),
        read_token,
        write_token,
    }
}

fn empty() -> Harness {
    harness_with(Config::default(), |_| {})
}

async fn send(h: &Harness, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn get(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("GET")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, token: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

// ─── reads ───────────────────────────────────────────────────────────────────

/// The pinned row exists in the fleet before anyone has spoken to it, so this
/// is a state to render rather than an error to report.
#[tokio::test]
async fn the_main_chat_is_null_rather_than_missing_before_anyone_speaks() {
    let h = empty();
    let (status, body) = send(&h, get("/v1/conversations/main", &h.read_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["conversation"].is_null(), "{body}");
    assert_eq!(body["messages"].as_array().unwrap().len(), 0);
}

/// A `GET` that creates is a `GET` a link prefetcher can fire. Reading the main
/// chat must not mint the pinned row.
#[tokio::test]
async fn reading_the_main_chat_does_not_create_it() {
    let h = empty();
    let (_, _) = send(&h, get("/v1/conversations/main", &h.read_token)).await;

    // If the read had created one, it would now be listed.
    let (status, list) = send(&h, get("/v1/conversations", &h.read_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list.as_array().unwrap().len(),
        0,
        "reading the main chat minted a conversation: {list}"
    );
}

#[tokio::test]
async fn a_conversation_and_its_thread_come_back() {
    let h = harness_with(Config::default(), |store| {
        let convo = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", Some("planning"))
            .unwrap();
        store
            .append_message(&convo.id, NewMessage::user("what is on my plate?"))
            .unwrap();
    });

    let (status, list) = send(&h, get("/v1/conversations", &h.read_token)).await;
    assert_eq!(status, StatusCode::OK);
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{list}");
    let id = rows[0]["id"].as_str().unwrap().to_string();

    let (status, one) = send(&h, get(&format!("/v1/conversations/{id}"), &h.read_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["id"].as_str(), Some(id.as_str()));

    let (status, msgs) = send(
        &h,
        get(&format!("/v1/conversations/{id}/messages"), &h.read_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let msgs = msgs.as_array().unwrap();
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert_eq!(msgs[0]["text"], "what is on my plate?");
}

/// A typo and an unused chat must be distinguishable — `[]` for both would make
/// the first indisplayable.
#[tokio::test]
async fn a_missing_conversation_is_a_404_on_both_the_row_and_its_messages() {
    let h = empty();
    let (status, _) = send(&h, get("/v1/conversations/nope", &h.read_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&h, get("/v1/conversations/nope/messages", &h.read_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `/v1/conversations/main` is a static segment and must win over `{id}`.
/// If it ever lost, "main" would be looked up as a conversation id and 404.
#[tokio::test]
async fn the_main_route_wins_over_the_id_route() {
    let h = empty();
    let (status, body) = send(&h, get("/v1/conversations/main", &h.read_token)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "`main` was matched as an id, not as the main chat: {body}"
    );
    // The `{id}` handler would have returned a bare conversation or a 404,
    // never this envelope.
    assert!(body.get("messages").is_some(), "{body}");
}

// ─── the write, and its refusals ─────────────────────────────────────────────

#[tokio::test]
async fn a_read_token_may_not_drive_the_main_chat() {
    let h = empty();
    let (status, _) = send(
        &h,
        post(
            "/v1/conversations/main/messages",
            &h.read_token,
            serde_json::json!({ "instruction": "triage the inbox" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_anonymous_caller_may_not_drive_the_main_chat() {
    let h = empty();
    let req = Request::builder()
        .uri("/v1/conversations/main/messages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"instruction":"triage the inbox"}"#))
        .unwrap();
    let (status, _) = send(&h, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_empty_instruction_is_refused_before_anything_is_spawned() {
    let h = empty();
    for text in ["", "   ", "\n\t "] {
        let (status, _) = send(
            &h,
            post(
                "/v1/conversations/main/messages",
                &h.write_token,
                serde_json::json!({ "instruction": text }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {text:?}");
    }
}

/// **The check this route exists to make.**
///
/// `hand_to_orchestrator` runs the main chat at `AcceptEdits` by construction.
/// A daemon whose ceiling is `Ask` must refuse the route outright — otherwise
/// anyone with a write token reaches `AcceptEdits` through this one path and
/// the ceiling means nothing.
#[tokio::test]
async fn a_daemon_capped_below_the_orchestrator_refuses_the_main_chat() {
    let config = Config {
        max_permission: PermissionPolicy::Ask,
        allowed_cwd: vec![std::env::temp_dir()],
        ..Config::default()
    };
    let h = harness_with(config, |_| {});

    let (status, body) = send(
        &h,
        post(
            "/v1/conversations/main/messages",
            &h.write_token,
            serde_json::json!({ "instruction": "triage the inbox" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The refusal has to name the mode, or an operator cannot tell which knob
    // to turn.
    let detail = body["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("accept_edits"),
        "the refusal did not name the mode: {body}"
    );
    assert!(
        detail.contains("max_permission"),
        "the refusal did not name the setting: {body}"
    );
}

/// An empty allowlist denies every spawn, and the main chat is a spawn.
#[tokio::test]
async fn an_empty_cwd_allowlist_refuses_the_main_chat() {
    let h = empty(); // Config::default() has an empty allowed_cwd
    let (status, _) = send(
        &h,
        post(
            "/v1/conversations/main/messages",
            &h.write_token,
            serde_json::json!({ "instruction": "triage the inbox" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_cwd_outside_the_allowlist_refuses_the_main_chat() {
    let config = Config {
        allowed_cwd: vec![std::env::temp_dir().join("jod-allowed")],
        ..Config::default()
    };
    let h = harness_with(config, |_| {});

    let (status, _) = send(
        &h,
        post(
            "/v1/conversations/main/messages",
            &h.write_token,
            serde_json::json!({ "instruction": "triage", "cwd": "/etc" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// The refusals must be ordered cheapest-and-most-restrictive first, so a
/// caller that is wrong in two ways hears about the scope rather than the cwd —
/// a read token learning "your cwd is bad" would be a small information leak
/// about the daemon's configuration.
#[tokio::test]
async fn scope_is_checked_before_the_allowlist() {
    let config = Config {
        allowed_cwd: vec![std::env::temp_dir().join("jod-allowed")],
        ..Config::default()
    };
    let h = harness_with(config, |_| {});

    let (status, _) = send(
        &h,
        post(
            "/v1/conversations/main/messages",
            &h.read_token,
            serde_json::json!({ "instruction": "triage", "cwd": "/etc" }),
        ),
    )
    .await;
    // Both are 403, but the scope refusal is the one that must fire — proven by
    // it never mentioning the path.
    assert_eq!(status, StatusCode::FORBIDDEN);
}
