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
pub mod webhook;
pub mod workspaces;

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
        // Read-only: a phone watches a team, it does not join one.
        .route("/v1/teams", get(routes::list_teams))
        .route("/v1/teams/{team}", get(routes::get_team))
        // The rest of the TUI's workspaces, all reads. → [`workspaces`]
        .route("/v1/memory", get(workspaces::list_memory))
        .route("/v1/memory/{id}", get(workspaces::get_memory_node))
        .route("/v1/memory/{id}/graph", get(workspaces::memory_graph))
        .route("/v1/schedules", get(workspaces::list_schedules))
        .route("/v1/schedules/{name}", get(workspaces::get_schedule))
        .route("/v1/goals", get(workspaces::list_goals))
        .route("/v1/goals/{name}", get(workspaces::get_goal))
        .route("/v1/hooks", get(workspaces::list_hooks))
        .route("/v1/tasks", get(workspaces::list_tasks))
        .route("/v1/activity", get(workspaces::list_activity))
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
        // Outside the authenticated group on purpose: GitHub holds no bearer
        // token, and its HMAC signature is the credential instead. → [`webhook`]
        .merge(webhook::routes_from_env())
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
}
