//! End-to-end tests over the real router.
//!
//! The unit tests prove each piece in isolation; these prove the pieces are
//! actually *wired together* — that a route really is behind the middleware,
//! that a read token really is refused at the spawn endpoint. A security
//! control that is implemented but not mounted is worse than none, because it
//! reads as present.
//!
//! No agent is ever spawned here: every spawn is expected to be refused before
//! it reaches a harness, which is exactly what the bounds are for.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jod_api::audit::AuditLog;
use jod_api::auth::{Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    read_token: String,
    write_token: String,
}

fn harness(config: Config) -> Harness {
    let mut tokens = TokenStore::default();
    let read_token = tokens.issue("phone", Scope::Read);
    let write_token = tokens.issue("laptop", Scope::Write);

    let audit = AuditLog::new(std::env::temp_dir().join("jod-api-test-audit.jsonl"));
    // `Jod::new()` is the non-persistent form: these tests must not touch the
    // real ~/.jod store.
    let state = AppState::new(jod_core::Jod::new(), config, tokens, audit);
    Harness {
        app: jod_api::router(state),
        read_token,
        write_token,
    }
}

fn default_harness() -> Harness {
    harness(Config::default())
}

async fn send(
    app: &axum::Router,
    req: Request<Body>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body, headers)
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri(path).method("GET");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

fn post_json(path: &str, token: Option<&str>, body: serde_json::Value) -> Request<Body> {
    let mut b = Request::builder()
        .uri(path)
        .method("POST")
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// Every route that is not `/v1/health`. Used to assert the middleware is
/// mounted on all of them rather than most of them.
const PROTECTED_GETS: &[&str] = &[
    "/v1/agents",
    "/v1/harnesses",
    "/v1/report",
    "/v1/agents/some-id",
    "/v1/agents/some-id/events",
    "/v1/agents/some-id/stream",
    "/v1/events",
];

#[tokio::test]
async fn health_needs_no_credential() {
    // A liveness probe that needs a token fails when the token rotates.
    let h = default_harness();
    let (status, body, _) = send(&h.app, get("/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn health_leaks_no_inventory() {
    let h = default_harness();
    let (_, body, _) = send(&h.app, get("/v1/health", None)).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(
        obj.len(),
        1,
        "health returned more than a status: {json} — that is a recon endpoint"
    );
}

#[tokio::test]
async fn every_protected_route_refuses_an_anonymous_request() {
    let h = default_harness();
    for path in PROTECTED_GETS {
        let (status, _, _) = send(&h.app, get(path, None)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} was reachable anonymously"
        );
    }
}

#[tokio::test]
async fn spawning_anonymously_is_refused() {
    let h = default_harness();
    let (status, _, _) = send(
        &h.app,
        post_json("/v1/agents", None, serde_json::json!({"prompt": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_invalid_token_is_refused() {
    let h = default_harness();
    let (status, _, _) = send(&h.app, get("/v1/agents", Some("jod_not_a_real_token"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_401_is_problem_json_with_a_challenge_and_no_reason() {
    let h = default_harness();
    let (status, body, headers) = send(&h.app, get("/v1/agents", Some("jod_wrong"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers.get("content-type").unwrap(),
        "application/problem+json"
    );
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json.get("detail").is_none(),
        "the 401 explained itself: {json}"
    );
}

#[tokio::test]
async fn a_valid_read_token_may_list_agents() {
    let h = default_harness();
    let (status, body, _) = send(&h.app, get("/v1/agents", Some(&h.read_token))).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array(), "expected an array of agents, got {json}");
}

#[tokio::test]
async fn a_read_token_may_not_spawn() {
    // The single most important assertion in this file: the credential most
    // likely to be carried around cannot execute code.
    let h = default_harness();
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/v1/agents",
            Some(&h.read_token),
            serde_json::json!({"prompt": "hi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_read_token_may_not_kill() {
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/agents/some-id")
        .method("DELETE")
        .header("authorization", format!("Bearer {}", h.read_token))
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_empty_allowlist_refuses_a_spawn_even_with_a_write_token() {
    // Failing closed on an unset control is the whole point: a forgotten config
    // line must not become an open shell.
    let h = default_harness();
    assert!(Config::default().allowed_cwd.is_empty());
    let (status, body, _) = send(
        &h.app,
        post_json(
            "/v1/agents",
            Some(&h.write_token),
            serde_json::json!({"prompt": "hi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["detail"].as_str().unwrap().contains("allowed"));
}

#[tokio::test]
async fn a_permission_above_the_ceiling_is_refused_not_downgraded() {
    let config = Config {
        allowed_cwd: vec![std::env::temp_dir()],
        ..Default::default()
    };
    let h = harness(config);
    let (status, body, _) = send(
        &h.app,
        post_json(
            "/v1/agents",
            Some(&h.write_token),
            serde_json::json!({"prompt": "hi", "permission": "bypass"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bypass was not refused");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["detail"].as_str().unwrap().contains("bypass"));
}

#[tokio::test]
async fn a_cwd_outside_the_allowlist_is_refused() {
    let root = std::env::temp_dir().join("jod-api-allowed-root");
    std::fs::create_dir_all(&root).unwrap();
    let h = harness(Config {
        allowed_cwd: vec![root],
        ..Default::default()
    });
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/v1/agents",
            Some(&h.write_token),
            serde_json::json!({"prompt": "hi", "cwd": "/etc"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_empty_prompt_is_a_400_not_a_spawn() {
    let h = harness(Config {
        allowed_cwd: vec![std::env::temp_dir()],
        ..Default::default()
    });
    let (status, _, _) = send(
        &h.app,
        post_json(
            "/v1/agents",
            Some(&h.write_token),
            serde_json::json!({"prompt": "   "}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_agent_is_a_404_in_problem_json() {
    let h = default_harness();
    let (status, _, headers) = send(&h.app, get("/v1/agents/nope", Some(&h.read_token))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers.get("content-type").unwrap(),
        "application/problem+json"
    );
}

#[tokio::test]
async fn streaming_an_unknown_agent_fails_fast_rather_than_hanging() {
    // A stream that opens and never yields looks identical to a working stream
    // that is simply quiet, which is a miserable thing to debug on a phone.
    let h = default_harness();
    let (status, _, _) = send(&h.app, get("/v1/agents/nope/stream", Some(&h.read_token))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_report_endpoint_answers_a_read_token() {
    let h = default_harness();
    let (status, body, _) = send(&h.app, get("/v1/report", Some(&h.read_token))).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["running"], 0);
    assert_eq!(json["total_cost_usd"], 0.0);
}

#[tokio::test]
async fn harnesses_are_listed_with_availability() {
    let h = default_harness();
    let (status, body, _) = send(&h.app, get("/v1/harnesses", Some(&h.read_token))).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let ids: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    for expected in ["claude_code", "open_code", "agy"] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
}

// ---------------------------------------------------------------------------
// Browser sessions
// ---------------------------------------------------------------------------

fn cookie_from(headers: &axum::http::HeaderMap) -> String {
    let set = headers
        .get("set-cookie")
        .expect("no Set-Cookie header")
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn a_bearer_token_can_be_traded_for_a_hardened_cookie() {
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.read_token))
        .body(Body::empty())
        .unwrap();
    let (status, body, headers) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::CREATED);

    let set = headers.get("set-cookie").unwrap().to_str().unwrap();
    for flag in ["HttpOnly", "Secure", "SameSite=Strict"] {
        assert!(set.contains(flag), "cookie missing {flag}: {set}");
    }

    // The web client greys out actions it cannot perform rather than eating a
    // 403, so the scope must come back with the session.
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["scope"], "read");
    assert!(json["expires_at_ms"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn a_session_cookie_authenticates_a_request() {
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.write_token))
        .body(Body::empty())
        .unwrap();
    let (_, _, headers) = send(&h.app, req).await;
    let cookie = cookie_from(&headers);

    let req = Request::builder()
        .uri("/v1/agents")
        .method("GET")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::OK, "the cookie did not authenticate");
}

#[tokio::test]
async fn a_session_carries_the_scope_of_the_token_that_made_it() {
    // A read token must not be launderable into a write session.
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.read_token))
        .body(Body::empty())
        .unwrap();
    let (_, _, headers) = send(&h.app, req).await;
    let cookie = cookie_from(&headers);

    let req = Request::builder()
        .uri("/v1/agents")
        .method("POST")
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"prompt":"hi"}"#))
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a read session could spawn");
}

#[tokio::test]
async fn a_forged_cookie_is_refused() {
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/agents")
        .method("GET")
        .header("cookie", "jod_session=jod_madeup")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_session_cannot_be_minted_from_a_session() {
    // Otherwise a stolen cookie renews itself forever.
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.read_token))
        .body(Body::empty())
        .unwrap();
    let (_, _, headers) = send(&h.app, req).await;
    let cookie = cookie_from(&headers);

    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_invalid_bearer_is_not_rescued_by_a_valid_cookie() {
    // Presenting a bad bearer is a refusal, not a fallback to whatever cookie
    // happened to ride along.
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.write_token))
        .body(Body::empty())
        .unwrap();
    let (_, _, headers) = send(&h.app, req).await;
    let cookie = cookie_from(&headers);

    let req = Request::builder()
        .uri("/v1/agents")
        .method("GET")
        .header("authorization", "Bearer jod_wrong")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signing_out_clears_the_cookie_and_stops_it_working() {
    let h = default_harness();
    let req = Request::builder()
        .uri("/v1/session")
        .method("POST")
        .header("authorization", format!("Bearer {}", h.read_token))
        .body(Body::empty())
        .unwrap();
    let (_, _, headers) = send(&h.app, req).await;
    let cookie = cookie_from(&headers);

    let req = Request::builder()
        .uri("/v1/session")
        .method("DELETE")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, headers) = send(&h.app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));

    let req = Request::builder()
        .uri("/v1/agents")
        .method("GET")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&h.app, req).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked cookie still worked"
    );
}
