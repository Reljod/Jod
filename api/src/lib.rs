//! # jod-api
//!
//! The authenticated HTTP surface over [`jod_core::Jod`], so a phone or a
//! browser can do what the `jod` CLI does.
//!
//! This crate adds **no orchestration logic of its own**. Every route is a
//! method call on the same `Jod` struct the CLI and the desktop app drive. The
//! moment the API grows its own idea of what an agent is, three clients start
//! disagreeing about it.
//!
//! ## The security posture in one paragraph
//!
//! A credential for this API is arbitrary code execution on the box — spawning
//! an agent harness *is* running shell commands. So the daemon binds loopback
//! only and is reached over a Tailscale tailnet ([`config::DEFAULT_BIND`]);
//! every route but `/v1/health` needs a bearer token that is stored hashed and
//! compared in constant time ([`auth`]); and a valid token is still bounded by
//! a permission ceiling, a working-directory allowlist and a concurrency cap
//! ([`config::Config`]), because credentials leak.
//!
//! → `docs/jod-api.md`

pub mod audit;
pub mod auth;
pub mod config;
pub mod error;
pub mod idempotency;
pub mod routes;
pub mod session;
pub mod sse;

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::RwLock;
use tower_http::limit::RequestBodyLimitLayer;

use crate::audit::AuditLog;
use crate::auth::{Scope, TokenStore};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::idempotency::IdempotencyCache;

/// The header `tailscale serve` injects with the caller's tailnet login.
///
/// Used for the audit trail and nothing else. It is an ordinary HTTP header, so
/// anything that can reach the service can set it; treating it as an
/// authorisation input is a known footgun. Identity a client can assert is not
/// authorisation. → `docs/jod-api.md`
const TAILSCALE_USER_HEADER: &str = "tailscale-user-login";

#[derive(Clone)]
pub struct AppState {
    pub jod: Arc<jod_core::Jod>,
    pub config: Arc<Config>,
    pub tokens: Arc<RwLock<TokenStore>>,
    pub sessions: Arc<crate::session::SessionStore>,
    pub idempotency: Arc<IdempotencyCache>,
    pub audit: Arc<AuditLog>,
}

impl AppState {
    pub fn new(
        jod: Arc<jod_core::Jod>,
        config: Config,
        tokens: TokenStore,
        audit: AuditLog,
    ) -> Self {
        Self {
            jod,
            config: Arc::new(config),
            tokens: Arc::new(RwLock::new(tokens)),
            sessions: Arc::new(crate::session::SessionStore::new()),
            idempotency: Arc::new(IdempotencyCache::new()),
            audit: Arc::new(audit),
        }
    }
}

/// Who is making this request, established by the auth middleware.
#[derive(Clone, Debug)]
pub struct Identity {
    /// The token's label — safe to log, unlike the token.
    pub label: String,
    pub scope: Scope,
    pub tailnet_user: Option<String>,
}

impl Identity {
    /// Gate a route on a scope. `read` tokens cannot spawn or kill.
    pub fn require(&self, needed: Scope) -> ApiResult<()> {
        if self.scope.allows(needed) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(
                "this token is read-only; a write-scoped token is required".into(),
            ))
        }
    }
}

/// Assemble the router.
///
/// `/v1/health` is mounted outside the authenticated group on purpose: a
/// liveness probe that needs a credential is a liveness probe that fails when
/// the credential rotates.
pub fn router(state: AppState) -> Router {
    let max_body = state.config.max_body_bytes;

    let protected = Router::new()
        .route("/v1/harnesses", get(routes::harnesses))
        .route(
            "/v1/agents",
            get(routes::list_agents).post(routes::spawn_agent),
        )
        .route(
            "/v1/agents/{id}",
            get(routes::get_agent).delete(routes::kill_agent),
        )
        .route("/v1/agents/{id}/events", get(routes::agent_events))
        .route("/v1/agents/{id}/stream", get(sse::agent_stream))
        .route("/v1/events", get(sse::all_agents_stream))
        .route("/v1/report", get(routes::report))
        .route("/v1/session", axum::routing::delete(routes::end_session))
        // Layers apply to the routes declared above them. The state is captured
        // by the closure rather than extracted, which keeps the middleware's
        // extractor list empty and unambiguous.
        // The `(Request,)` type argument is axum's extractor tuple for this
        // middleware: `Request` is itself the trailing extractor. Inference
        // cannot pick an arity on its own here, so it is named.
        .layer(axum::middleware::from_fn::<_, (Request,)>({
            let state = state.clone();
            move |req: Request, next: Next| authenticate(state.clone(), req, next)
        }))
        .layer(RequestBodyLimitLayer::new(max_body));

    // Minting a session must present a *bearer* token — you cannot bootstrap a
    // session from a session, so this route authenticates itself.
    let session = Router::new()
        .route("/v1/session", axum::routing::post(routes::start_session))
        .layer(RequestBodyLimitLayer::new(max_body));

    Router::new()
        .route("/v1/health", get(routes::health))
        .merge(session)
        .merge(protected)
        .with_state(state)
}

/// Establish [`Identity`], or refuse.
///
/// Accepts **either** a bearer token or a session cookie. Bearer is the
/// primary: curl, the CLI and native mobile clients use it and never touch a
/// cookie. The cookie exists because `EventSource` cannot set headers.
///
/// Every failure here is the same opaque 401 — "no such token", "malformed
/// header", "expired session" are indistinguishable to the caller, because a
/// 401 that explains itself is an oracle for guessing credentials.
async fn authenticate(state: AppState, mut req: Request, next: Next) -> Response {
    // Copy the three headers out before any `.await`. A borrow of `Request`
    // held across an await point makes the whole future non-Send, because the
    // body is `dyn HttpBody + Send` and so `&Request` is not `Send`.
    let (auth_header, cookie, tailnet_user) = {
        let headers = req.headers();
        let owned = |name: axum::http::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        (
            owned(AUTHORIZATION),
            owned(axum::http::header::COOKIE),
            headers
                .get(TAILSCALE_USER_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        )
    };

    let resolved = resolve_identity(
        &state,
        auth_header.as_deref(),
        cookie.as_deref(),
        tailnet_user,
    )
    .await;
    let Some(identity) = resolved else {
        return ApiError::Unauthorized.into_response();
    };

    req.extensions_mut().insert(identity);
    next.run(req).await
}

/// Bearer first, then cookie. Returns `None` for every kind of failure so the
/// caller cannot tell them apart.
async fn resolve_identity(
    state: &AppState,
    auth_header: Option<&str>,
    cookie: Option<&str>,
    tailnet_user: Option<String>,
) -> Option<Identity> {
    if let Ok(presented) = auth::bearer_from_header(auth_header) {
        let tokens = state.tokens.read().await;
        if let Some(record) = tokens.verify(presented) {
            return Some(Identity {
                label: record.label.clone(),
                scope: record.scope,
                tailnet_user,
            });
        }
        // A presented-but-invalid bearer is a refusal, not an invitation to
        // fall back to a cookie that happened to ride along.
        return None;
    }

    let id = crate::session::session_from_cookie_header(cookie)?;
    let now = chrono::Utc::now().timestamp_millis();
    let s = state.sessions.get(id, now)?;
    Some(Identity {
        label: s.label,
        scope: s.scope,
        tailnet_user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_token_may_read_but_not_write() {
        let id = Identity {
            label: "phone".into(),
            scope: Scope::Read,
            tailnet_user: None,
        };
        assert!(id.require(Scope::Read).is_ok());
        assert!(id.require(Scope::Write).is_err());
    }

    #[test]
    fn a_write_token_may_do_both() {
        let id = Identity {
            label: "laptop".into(),
            scope: Scope::Write,
            tailnet_user: None,
        };
        assert!(id.require(Scope::Read).is_ok());
        assert!(id.require(Scope::Write).is_ok());
    }

    #[test]
    fn a_scope_refusal_never_names_the_token() {
        let id = Identity {
            label: "phone".into(),
            scope: Scope::Read,
            tailnet_user: None,
        };
        let msg = id.require(Scope::Write).unwrap_err().to_string();
        assert!(
            !msg.contains("phone"),
            "the refusal named the credential: {msg}"
        );
    }

    // --- the wire ---------------------------------------------------------
    //
    // These drive the assembled router, so the middleware, the scope gates and
    // the status codes are exercised together. A handler that is correct in
    // isolation but mounted behind the wrong layer is exactly the bug a
    // unit test cannot see.

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    struct Harness {
        router: Router,
        write: String,
        read: String,
        _dir: std::path::PathBuf,
    }

    fn harness_with(config: Config) -> Harness {
        let dir = std::env::temp_dir().join(format!("jod-api-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let mut tokens = TokenStore::default();
        let write = tokens.issue("laptop", Scope::Write);
        let read = tokens.issue("phone", Scope::Read);

        let state = AppState::new(
            jod_core::Jod::new(),
            config,
            tokens,
            AuditLog::new(dir.join("audit.jsonl")),
        );
        Harness {
            router: router(state),
            write,
            read,
            _dir: dir,
        }
    }

    fn harness() -> Harness {
        harness_with(Config::default())
    }

    impl Harness {
        async fn send(&self, req: Request<Body>) -> (StatusCode, String) {
            let res = self.router.clone().oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).to_string())
        }

        async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, String) {
            let mut b = Request::builder().uri(uri);
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            self.send(b.body(Body::empty()).unwrap()).await
        }

        async fn post_json(&self, uri: &str, token: &str, body: &str) -> (StatusCode, String) {
            self.send(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }
    }

    /// A liveness probe that needs a credential fails when the credential
    /// rotates, so this one is mounted outside the authenticated group.
    #[tokio::test]
    async fn health_needs_no_credential_and_leaks_no_inventory() {
        let (status, body) = harness().get("/v1/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ok"));
        for leak in ["version", "hostname", "agents"] {
            assert!(!body.contains(leak), "health leaked {leak}: {body}");
        }
    }

    #[tokio::test]
    async fn every_other_route_refuses_an_anonymous_caller() {
        let h = harness();
        for uri in ["/v1/harnesses", "/v1/agents", "/v1/report", "/v1/agents/x"] {
            let (status, _) = h.get(uri, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} was reachable");
        }
    }

    #[tokio::test]
    async fn a_token_that_was_never_issued_is_refused() {
        let h = harness();
        let (status, _) = h.get("/v1/agents", Some("jod_not-a-real-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// Every 401 must look the same, or the difference is an oracle for
    /// guessing credentials.
    #[tokio::test]
    async fn every_refusal_looks_identical_from_outside() {
        let h = harness();
        let (_, missing) = h.get("/v1/agents", None).await;
        let (_, wrong) = h.get("/v1/agents", Some("jod_wrong")).await;
        let (_, malformed) = h
            .send(
                Request::builder()
                    .uri("/v1/agents")
                    .header("authorization", "Basic abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(missing, wrong);
        assert_eq!(wrong, malformed);
    }

    #[tokio::test]
    async fn a_valid_token_reaches_the_read_routes() {
        let h = harness();
        for uri in ["/v1/harnesses", "/v1/agents", "/v1/report"] {
            let (status, _) = h.get(uri, Some(&h.write)).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_read_token_may_look_but_not_spawn() {
        let h = harness();
        assert_eq!(h.get("/v1/agents", Some(&h.read)).await.0, StatusCode::OK);

        let (status, _) = h
            .post_json("/v1/agents", &h.read, r#"{"prompt":"do it"}"#)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_read_token_may_not_kill_either() {
        let h = harness();
        let (status, _) = h
            .send(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/agents/whatever")
                    .header("authorization", format!("Bearer {}", h.read))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_agent_nobody_has_heard_of_is_a_404() {
        let h = harness();
        assert_eq!(
            h.get("/v1/agents/nope", Some(&h.write)).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.get("/v1/agents/nope/events", Some(&h.write)).await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn an_empty_prompt_is_refused_before_anything_is_started() {
        let h = harness();
        let (status, body) = h
            .post_json("/v1/agents", &h.write, r#"{"prompt":"   "}"#)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("prompt"), "{body}");
    }

    /// `bypass` over an API is a remote shell. Raising the ceiling is a local
    /// act; a request must never do it to itself.
    #[tokio::test]
    async fn a_permission_above_the_daemons_ceiling_is_refused_not_downgraded() {
        let h = harness();
        let (status, body) = h
            .post_json(
                "/v1/agents",
                &h.write,
                r#"{"prompt":"do it","permission":"bypass"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("ceiling"), "{body}");
    }

    /// The default config has an empty allowlist, which denies every spawn.
    #[tokio::test]
    async fn with_no_allowlist_there_is_nowhere_to_spawn() {
        let h = harness();
        let (status, _) = h
            .post_json("/v1/agents", &h.write, r#"{"prompt":"do it"}"#)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_directory_outside_the_allowlist_is_refused() {
        let h = harness_with(Config {
            allowed_cwd: vec![std::path::PathBuf::from("/tmp/jod-allowed")],
            ..Config::default()
        });
        let (status, _) = h
            .post_json(
                "/v1/agents",
                &h.write,
                r#"{"prompt":"do it","cwd":"/etc"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_unknown_harness_is_rejected_as_a_bad_request() {
        let h = harness();
        let (status, _) = h
            .post_json(
                "/v1/agents",
                &h.write,
                r#"{"prompt":"do it","harness":"gpt"}"#,
            )
            .await;
        assert!(
            status.is_client_error(),
            "an unknown harness must not be accepted: {status}"
        );
    }

    // --- sessions ---------------------------------------------------------

    #[tokio::test]
    async fn a_bearer_token_can_be_traded_for_a_cookie() {
        let h = harness();
        let res = h
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session")
                    .header("authorization", format!("Bearer {}", h.write))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // 201: minting a session creates a resource, so it is not a plain 200.
        assert_eq!(res.status(), StatusCode::CREATED);
        let cookie = res
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .expect("a session must set a cookie");
        let cookie = cookie.to_str().unwrap();
        assert!(cookie.contains("HttpOnly"), "cookie must be HttpOnly: {cookie}");

        let body = res.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("scope"), "the client is told what it may do: {body}");
    }

    /// You cannot bootstrap a session from a session — that route takes a
    /// bearer and nothing else.
    #[tokio::test]
    async fn minting_a_session_without_a_bearer_is_refused() {
        let h = harness();
        let (status, _) = h
            .send(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// `EventSource` cannot set headers, which is the whole reason the cookie
    /// exists — so it has to actually authenticate.
    #[tokio::test]
    async fn a_minted_cookie_authenticates_a_later_request() {
        let h = harness();
        let res = h
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session")
                    .header("authorization", format!("Bearer {}", h.write))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = res
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let (status, _) = h
            .send(
                Request::builder()
                    .uri("/v1/agents")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn ending_a_session_clears_the_cookie() {
        let h = harness();
        let res = h
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/session")
                    .header("authorization", format!("Bearer {}", h.write))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let cookie = res
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .expect("the cookie must be cleared")
            .to_str()
            .unwrap();
        assert!(
            cookie.contains("Max-Age=0") || cookie.contains("Expires"),
            "the cookie must actually expire: {cookie}"
        );
    }

    // --- the whole lifecycle over HTTP ------------------------------------

    /// Tests that set `JOD_*` must hold this: the environment is process-wide
    /// and Rust runs tests as threads of one process.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fake tmux and a fake harness, so a spawn can actually succeed here
    /// without a tmux server, a real Claude install, or any risk of touching
    /// the developer's own `jod-*` sessions.
    struct Spawnable {
        dir: std::path::PathBuf,
    }

    impl Spawnable {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("jod-api-spawn-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("work")).unwrap();

            let sessions = dir.join("sessions");
            std::fs::write(&sessions, b"").unwrap();
            write_exec(
                &dir.join("tmux"),
                &format!(
                    "#!/bin/bash\nPATH=/usr/bin:/bin\nexport PATH\nS={:?}\n\
                     c=\"${{1:-}}\"; shift || true\n\
                     case \"$c\" in\n\
                       has-session) grep -qxF \"$2\" \"$S\" ;;\n\
                       new-session) while [ $# -gt 0 ]; do case \"$1\" in -s) printf '%s\\n' \"$2\" >> \"$S\"; shift 2 ;; *) shift ;; esac; done ;;\n\
                       kill-session) grep -vxF \"$2\" \"$S\" > \"$S.t\" || true; mv \"$S.t\" \"$S\" ;;\n\
                       list-sessions) cat \"$S\" ;;\n\
                       *) : ;;\n\
                     esac\n",
                    sessions.to_string_lossy()
                ),
            );
            write_exec(&dir.join("claude"), "#!/bin/bash\nexit 0\n");

            std::env::set_var("JOD_TMUX_BIN", dir.join("tmux"));
            std::env::set_var("JOD_CLAUDE_BIN", dir.join("claude"));
            std::env::set_var("JOD_HOME", dir.join("home"));
            Self { dir }
        }

        fn work(&self) -> std::path::PathBuf {
            self.dir.join("work")
        }
    }

    impl Drop for Spawnable {
        fn drop(&mut self) {
            for k in ["JOD_TMUX_BIN", "JOD_CLAUDE_BIN", "JOD_HOME"] {
                std::env::remove_var(k);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn write_exec(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Delegate, read it back, read its events, then reclaim it — the whole
    /// point of the API, over the wire, through the real router.
    #[tokio::test]
    async fn an_agent_can_be_spawned_read_back_and_killed() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = Spawnable::new();
        let h = harness_with(Config {
            allowed_cwd: vec![env.work()],
            ..Config::default()
        });

        let res = h
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agents")
                    .header("authorization", format!("Bearer {}", h.write))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"summarise the repo"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::CREATED);
        let location = res
            .headers()
            .get(axum::http::header::LOCATION)
            .expect("a created agent must say where it lives")
            .to_str()
            .unwrap()
            .to_string();

        let body = res.into_body().collect().await.unwrap().to_bytes();
        let agent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = agent["id"].as_str().unwrap().to_string();
        assert_eq!(location, format!("/v1/agents/{id}"));
        assert_eq!(agent["status"], "running");
        assert_eq!(
            agent["name"], "summarise the repo",
            "an unnamed agent is named from its prompt"
        );

        let (status, listed) = h.get("/v1/agents", Some(&h.write)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(listed.contains(&id));

        let (status, one) = h.get(&location, Some(&h.write)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(one.contains(&id));

        let (status, _) = h
            .get(&format!("/v1/agents/{id}/events"), Some(&h.write))
            .await;
        assert_eq!(status, StatusCode::OK);

        let (status, report) = h.get("/v1/report", Some(&h.write)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(report.contains("running"));

        let (status, _) = h
            .send(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/v1/agents/{id}"))
                    .header("authorization", format!("Bearer {}", h.write))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, after) = h.get(&location, Some(&h.write)).await;
        assert!(
            after.contains("killed") || after.contains("session_closed\":true"),
            "the kill must be visible afterwards: {after}"
        );
    }

    /// A retried POST must not start a second agent — that is the difference
    /// between a flaky network and a duplicated shell session.
    #[tokio::test]
    async fn replaying_a_request_with_the_same_idempotency_key_returns_the_first_agent() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = Spawnable::new();
        let h = harness_with(Config {
            allowed_cwd: vec![env.work()],
            ..Config::default()
        });

        let send = || {
            h.router.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agents")
                    .header("authorization", format!("Bearer {}", h.write))
                    .header("content-type", "application/json")
                    .header("idempotency-key", "abc-123")
                    .body(Body::from(r#"{"prompt":"do it once"}"#))
                    .unwrap(),
            )
        };

        let first = send().await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let first_id: serde_json::Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();

        let second = send().await.unwrap();
        assert_eq!(
            second.status(),
            StatusCode::OK,
            "a replay is not a fresh creation"
        );
        let second_id: serde_json::Value =
            serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes()).unwrap();

        assert_eq!(first_id["id"], second_id["id"]);
        let (_, listed) = h.get("/v1/agents", Some(&h.write)).await;
        assert_eq!(
            listed.matches("\"id\"").count(),
            1,
            "the replay started a second agent: {listed}"
        );
    }

    /// The cap exists so a stolen credential cannot fork-bomb the box.
    #[tokio::test]
    async fn the_concurrency_cap_refuses_the_agent_that_would_exceed_it() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = Spawnable::new();
        let h = harness_with(Config {
            allowed_cwd: vec![env.work()],
            max_concurrent_agents: 1,
            ..Config::default()
        });

        let spawn = |prompt: &str| {
            h.router.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agents")
                    .header("authorization", format!("Bearer {}", h.write))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"prompt":"{prompt}"}}"#)))
                    .unwrap(),
            )
        };

        assert_eq!(spawn("first").await.unwrap().status(), StatusCode::CREATED);
        assert_eq!(
            spawn("second").await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    /// Identity a client asserts is not authorisation — the header is recorded
    /// for the audit trail and must not grant anything on its own.
    #[tokio::test]
    async fn the_tailnet_header_alone_authenticates_nothing() {
        let h = harness();
        let (status, _) = h
            .send(
                Request::builder()
                    .uri("/v1/agents")
                    .header(TAILSCALE_USER_HEADER, "someone@example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
