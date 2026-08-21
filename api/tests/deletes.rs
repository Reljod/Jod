//! Removing things over HTTP: runs, conversations and works.
//!
//! Three routes, one theme. Every one of them is a write, every one is audited,
//! and every one refuses in a way the caller can act on rather than doing
//! something surprising. The refusals are the reason this file exists — a
//! delete that quietly stopped a live agent, or quietly removed a session out
//! of a tree that still points at it, would pass a test that only checked the
//! happy path.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jod_api::audit::AuditLog;
use jod_api::auth::{Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;
use jod_core::conversation::{NewMessage, Role};
use jod_core::store::{Store, StoredRun};
use jod_core::works::Origin;
use jod_core::HarnessKind;
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    read_token: String,
    write_token: String,
    store: Arc<Store>,
}

fn harness(seed: impl FnOnce(&Store)) -> Harness {
    let store = Store::in_memory().expect("in-memory store");
    seed(&store);
    let store = Arc::new(store);

    let mut tokens = TokenStore::default();
    let read_token = tokens.issue("phone", Scope::Read);
    let write_token = tokens.issue("laptop", Scope::Write);
    let audit = AuditLog::new(std::env::temp_dir().join("jod-api-deletes-audit.jsonl"));
    let jod = jod_core::Jod::with_store(store.clone());

    Harness {
        app: jod_api::router(AppState::new(jod, Config::default(), tokens, audit)),
        read_token,
        write_token,
        store,
    }
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

fn delete(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("DELETE")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn run(id: &str, status: &str) -> StoredRun {
    StoredRun {
        id: id.into(),
        name: format!("run {id}"),
        harness: "claude_code".into(),
        status: status.into(),
        cwd: "/tmp".into(),
        session_id: Some(format!("sess-{id}")),
        pid: None,
        pgid: None,
        created_at_ms: 1,
        summary: serde_json::json!({ "id": id }),
    }
}

// ─── runs ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn deleting_a_finished_run_removes_the_row_and_its_events() {
    let h = harness(|store| {
        store.save_run(&run("done", "completed")).unwrap();
        store.save_run(&run("other", "completed")).unwrap();
    });

    let (status, _) = send(&h, delete("/v1/runs/done", &h.write_token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(h.store.run("done").unwrap().is_none());
    assert!(
        h.store.run("other").unwrap().is_some(),
        "the delete reached a run nobody named"
    );
}

/// Deleting the row does not stop the process group, and the row is the only
/// thing left holding its pgid. A refusal is what keeps the agent reachable.
#[tokio::test]
async fn deleting_a_running_run_is_refused_rather_than_stopping_it() {
    let h = harness(|store| {
        store.save_run(&run("live", "running")).unwrap();
    });

    let (status, body) = send(&h, delete("/v1/runs/live", &h.write_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("still running"),
        "the refusal does not say why: {body}"
    );
    assert!(
        h.store.run("live").unwrap().is_some(),
        "a refused delete removed the run anyway"
    );
}

#[tokio::test]
async fn deleting_a_run_that_never_existed_is_a_404() {
    let h = harness(|_| {});
    let (status, _) = send(&h, delete("/v1/runs/ghost", &h.write_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_read_token_cannot_delete_a_run() {
    let h = harness(|store| {
        store.save_run(&run("done", "completed")).unwrap();
    });

    let (status, _) = send(&h, delete("/v1/runs/done", &h.read_token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.store.run("done").unwrap().is_some());
}

/// `DELETE /v1/agents/{id}` and `DELETE /v1/runs/{id}` are different verbs on
/// purpose. Killing keeps the record; this removes it. If they ever collapse
/// into one path, stopping a run would start erasing history.
#[tokio::test]
async fn killing_a_run_keeps_the_record_that_deleting_it_removes() {
    let h = harness(|store| {
        store.save_run(&run("done", "completed")).unwrap();
    });

    // Kill is served from the daemon's memory, which never held this run — the
    // point being only that it is not the same route and does not delete.
    let (_, _) = send(&h, delete("/v1/agents/done", &h.write_token)).await;
    assert!(
        h.store.run("done").unwrap().is_some(),
        "a kill removed the stored run"
    );

    let (status, _) = send(&h, delete("/v1/runs/done", &h.write_token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(h.store.run("done").unwrap().is_none());
}

// ─── conversations ───────────────────────────────────────────────────────────

#[tokio::test]
async fn deleting_a_loose_conversation_takes_its_messages() {
    let h = harness(|store| {
        let c = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        store
            .append_message(&c.id, NewMessage::user("what is on my plate?"))
            .unwrap();
    });

    let id = h.store.conversations(10).unwrap()[0].id.clone();
    let (status, _) = send(&h, delete(&format!("/v1/conversations/{id}"), &h.write_token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(h.store.conversations(10).unwrap().is_empty());
}

/// The desk everything else was opened from. Deleting it frees nothing and
/// loses the thread.
#[tokio::test]
async fn the_main_chat_cannot_be_deleted() {
    let h = harness(|store| {
        let id = store.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        store.append_message(&id, NewMessage::user("hello")).unwrap();
    });

    let id = h.store.pinned_conversation().unwrap().unwrap();
    let (status, body) = send(&h, delete(&format!("/v1/conversations/{id}"), &h.write_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(h.store.pinned_conversation().unwrap().is_some());
}

/// Its siblings still name it as a parent and its cards still carry its work,
/// so removing one session out of a tree leaves the tree pointing at nothing.
/// Deleting the work is the sanctioned way, and the refusal says so.
#[tokio::test]
async fn a_session_inside_a_work_is_refused_and_told_to_delete_the_work() {
    let h = harness(|store| {
        let work = store.create_work("port the parser").unwrap();
        let c = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        store
            .attach_conversation(&c.id, &work.id, None, Origin::Agent)
            .unwrap();
    });

    let id = h.store.conversations(10).unwrap()[0].id.clone();
    let (status, body) = send(&h, delete(&format!("/v1/conversations/{id}"), &h.write_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("delete the work"),
        "the refusal does not name the way through: {body}"
    );
}

/// A missing row and a refusal are different answers, and a client that shows
/// "cannot delete that" for a typo is a client that sends people looking for a
/// rule that does not exist.
#[tokio::test]
async fn deleting_a_conversation_that_is_not_there_is_a_404_not_a_refusal() {
    let h = harness(|_| {});
    let (status, _) = send(&h, delete("/v1/conversations/ghost", &h.write_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_read_token_cannot_delete_a_conversation() {
    let h = harness(|store| {
        store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
    });

    let id = h.store.conversations(10).unwrap()[0].id.clone();
    let (status, _) = send(&h, delete(&format!("/v1/conversations/{id}"), &h.read_token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(h.store.conversations(10).unwrap().len(), 1);
}

// ─── works ───────────────────────────────────────────────────────────────────

fn seed_work(store: &Store) -> String {
    let work = store.create_work("port the parser").unwrap();
    store.set_work_title(&work.id, "the parser").unwrap();
    let c = store
        .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
        .unwrap();
    store
        .attach_conversation(&c.id, &work.id, None, Origin::Agent)
        .unwrap();
    store
        .append_message(&c.id, NewMessage::new(Role::Assistant, "on it"))
        .unwrap();
    work.id
}

/// A work holding no git worktrees has nothing that cannot be recreated, so
/// there is nothing to confirm and it goes in one call.
#[tokio::test]
async fn deleting_a_work_with_no_worktrees_takes_it_and_its_sessions() {
    let mut id = String::new();
    let h = harness(|store| id = seed_work(store));

    let (status, body) = send(&h, delete(&format!("/v1/works/{id}"), &h.write_token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"].as_bool(), Some(true), "{body}");
    assert_eq!(body["doomed"]["sessions"].as_u64(), Some(1), "{body}");
    assert!(h.store.work(&id).unwrap().is_none());
    assert!(
        h.store.conversations(10).unwrap().is_empty(),
        "the work went but its sessions stayed"
    );
}

/// The counts are the confirmation dialog. A refusal that only said "refused"
/// would leave a client with nothing to put in front of a person.
#[tokio::test]
async fn a_deleted_work_reports_what_it_took() {
    let mut id = String::new();
    let h = harness(|store| id = seed_work(store));

    let (_, body) = send(&h, delete(&format!("/v1/works/{id}"), &h.write_token)).await;
    assert_eq!(body["doomed"]["title"].as_str(), Some("the parser"), "{body}");
    assert_eq!(body["doomed"]["transcripts"].as_u64(), Some(1), "{body}");
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("the parser"),
        "{body}"
    );
}

#[tokio::test]
async fn deleting_a_work_that_is_not_there_says_so() {
    let h = harness(|_| {});
    let (status, body) = send(&h, delete("/v1/works/ghost", &h.write_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_read_token_cannot_delete_a_work() {
    let mut id = String::new();
    let h = harness(|store| id = seed_work(store));

    let (status, _) = send(&h, delete(&format!("/v1/works/{id}"), &h.read_token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.store.work(&id).unwrap().is_some());
}
