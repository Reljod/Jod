//! The handlers. Each one is a thin translation of an HTTP request into a
//! [`jod_core::Jod`] call — the orchestration lives there, not here.

use std::path::PathBuf;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use jod_core::team::{Member, TeamTask};
use jod_core::{
    AgentEnvelope, AgentStatus, AgentSummary, HarnessKind, PermissionPolicy, Resume, SpawnRequest,
};
use serde::{Deserialize, Serialize};

use crate::audit;
use crate::auth::Scope;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, Identity};

/// Liveness only.
///
/// Unauthenticated, and deliberately says nothing else — no version, no agent
/// count, no hostname. A health check that leaks inventory is a reconnaissance
/// endpoint.
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    /// Returned so a browser can grey out actions it cannot perform, rather
    /// than offering a form that will eat a 403.
    pub scope: Scope,
    pub expires_at_ms: i64,
}

/// Trade a bearer token for a browser session cookie.
///
/// Authenticates itself rather than sitting behind the shared middleware: you
/// cannot bootstrap a session from a session, so only a bearer works here.
pub async fn start_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let presented = crate::auth::bearer_from_header(
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    )
    .map_err(|_| ApiError::Unauthorized)?;

    let (label, scope) = {
        let tokens = state.tokens.read().await;
        let record = tokens.verify(presented).ok_or(ApiError::Unauthorized)?;
        (record.label.clone(), record.scope)
    };

    let ttl = state.config.session_ttl_ms();
    let now = chrono::Utc::now().timestamp_millis();
    let id = state.sessions.create(&label, scope, now, ttl);

    let mut e = audit::entry("session.start", &label, "ok");
    e.tailnet_user = headers
        .get("tailscale-user-login")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    state.audit.append(&e);

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::SET_COOKIE,
            crate::session::set_cookie_value(&id, ttl),
        )],
        Json(SessionInfo {
            scope,
            expires_at_ms: now.saturating_add(ttl),
        }),
    ))
}

/// Sign a browser out. Idempotent — clearing a cookie that is already gone is
/// still success, so a client can always reach a signed-out state.
pub async fn end_session(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    if let Some(id) = crate::session::session_from_cookie_header(
        headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok()),
    ) {
        state.sessions.revoke(id);
    }
    state
        .audit
        .append(&audit::entry("session.end", &identity.label, "ok"));
    Ok((
        StatusCode::NO_CONTENT,
        [(
            axum::http::header::SET_COOKIE,
            crate::session::clear_cookie_value(),
        )],
    ))
}

pub async fn harnesses(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    Ok(Json(state.jod.harnesses()))
}

pub async fn list_agents(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    Ok(Json(state.jod.agents().await))
}

pub async fn get_agent(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    Ok(Json(state.jod.agent(&id).await?))
}

pub async fn report(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    Ok(Json(state.jod.report().await))
}

/// A team, as a client sees it.
///
/// Members and tasks together, in one answer, because they are one screen: the
/// TUI's `Ctrl-G` panel draws both and two round trips would let it render a
/// board from one moment against a roster from another.
#[derive(Debug, Serialize)]
pub struct TeamView {
    pub team: String,
    pub members: Vec<Member>,
    pub tasks: Vec<TeamTask>,
}

/// Every team that has a member.
///
/// Read-only, and deliberately so: joining, claiming and messaging are how a
/// *teammate* participates, and a teammate is an agent on the box with a tmux
/// session — not a phone. A remote client watches the board; it does not play
/// on it.
pub async fn list_teams(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    // No store means no persistence, so there is nowhere a team could exist.
    // An empty list is the honest answer, not an error: the question was
    // "which teams are there", and the answer is "none".
    let Some(store) = state.jod.store() else {
        return Ok(Json(Vec::<String>::new()));
    };
    store
        .teams()
        .map(Json)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// One team's roster and board.
///
/// A team nobody has joined is a 404 rather than an empty view, so a mistyped
/// name is distinguishable from a team that exists and is idle — the panel
/// shows very different things for the two.
pub async fn get_team(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(team): Path<String>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store = state
        .jod
        .store()
        .ok_or_else(|| ApiError::NotFound(format!("no team named {team}")))?;
    let members = store
        .team_members(&team)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let tasks = store
        .team_tasks(&team)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // A board can legitimately be empty while a roster is not, but neither
    // being present means nobody has ever joined under this name.
    if members.is_empty() && tasks.is_empty() {
        return Err(ApiError::NotFound(format!("no team named {team}")));
    }
    Ok(Json(TeamView {
        team,
        members,
        tasks,
    }))
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// The last `seq` the client saw. **Omitted means "everything"** — it is
    /// deliberately not `0`, because `seq` starts at 0 and core's
    /// `events_since` is strictly exclusive, so a `0` default would silently
    /// swallow the first event of every run. That first event is `started`,
    /// which carries `session_id` and `model`.
    pub after_seq: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct EventsPage {
    pub events: Vec<AgentEnvelope>,
    /// Highest `seq` in this page — what to send as `after_seq` next time.
    pub last_seq: Option<u64>,
}

pub async fn agent_events(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let mut events = state.jod.events_since(&id, q.after_seq).await?;
    if let Some(limit) = q.limit {
        events.truncate(limit);
    }
    let last_seq = events.last().map(|e| e.seq);
    Ok(Json(EventsPage { events, last_seq }))
}

/// What a client sends to delegate a task.
#[derive(Debug, Deserialize)]
pub struct SpawnBody {
    pub prompt: String,
    #[serde(default = "default_harness")]
    pub harness: HarnessKind,
    #[serde(default)]
    pub name: Option<String>,
    /// Must resolve inside the configured allowlist. Omitted means the first
    /// allowed root, which is the common case for a phone with one project.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub model: Option<String>,
    /// Pinned to `Ask` rather than following [`PermissionPolicy`]'s own
    /// default, and the difference is the trust boundary.
    ///
    /// The process-wide default is `Bypass`, because Jod's whole premise is
    /// work that happens with nobody watching. That reasoning does not reach
    /// here: this field is filled in by *whatever is on the other end of a
    /// socket*, and a caller who omits it has not asked for anything. Letting
    /// an omission mean "auto-approve everything" would put the most dangerous
    /// setting one forgotten JSON key away, on the one surface Jod does not
    /// control the callers of.
    ///
    /// A remote caller that genuinely wants it says so, and is still capped by
    /// its token's ceiling.
    #[serde(default = "default_permission")]
    pub permission: PermissionPolicy,
    #[serde(default)]
    pub resume: Resume,
}

fn default_harness() -> HarnessKind {
    HarnessKind::ClaudeCode
}

fn default_permission() -> PermissionPolicy {
    PermissionPolicy::Ask
}

/// Delegate a prompt to a harness.
///
/// This is the dangerous verb — it starts a process that runs shell commands —
/// so it is where every bound lives: scope, permission ceiling, cwd allowlist,
/// concurrency cap. The checks run *before* anything is spawned, cheapest and
/// most restrictive first.
pub async fn spawn_agent(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Json(body): Json<SpawnBody>,
) -> ApiResult<impl IntoResponse> {
    // Audited before the early return: a read credential repeatedly trying to
    // spawn is precisely the signal that one has been stolen and is being
    // probed, and it is worthless if it never reaches the log.
    if let Err(e) = identity.require(Scope::Write) {
        audit_write(&state, &identity, "spawn", None, "refused_scope");
        return Err(e);
    }

    let prompt = body.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ApiError::BadRequest("prompt is empty".into()));
    }

    // A permission the operator has not allowed remotely is refused outright,
    // never silently downgraded — a client that asked for `bypass` and got
    // `ask` would misreport what it did.
    if !state.config.permits(body.permission) {
        audit_write(&state, &identity, "spawn", None, "refused_permission");
        return Err(ApiError::Forbidden(format!(
            "permission `{}` exceeds this daemon's ceiling; raise max_permission locally to allow it",
            permission_id(body.permission)
        )));
    }

    let requested_cwd = match body.cwd {
        Some(ref c) => c.clone(),
        None => state.config.allowed_cwd.first().cloned().ok_or_else(|| {
            ApiError::Forbidden(crate::config::CwdRejection::NoAllowlist.to_string())
        })?,
    };
    let cwd = state.config.resolve_cwd(&requested_cwd).map_err(|e| {
        audit_write(&state, &identity, "spawn", None, "refused_cwd");
        ApiError::Forbidden(e.to_string())
    })?;

    if !state.jod.supervisor_available() {
        return Err(ApiError::Internal(
            "`jod-run` is not installed on this machine, and it supervises every agent".into(),
        ));
    }

    // An idempotent replay must not consume a concurrency slot, so it is
    // answered before the cap is tested.
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let now = chrono::Utc::now().timestamp_millis();
    if let Some(key) = key.as_deref() {
        if let Some(existing) = state.idempotency.get(&identity.label, key, now) {
            // The original may since have been forgotten; fall through and spawn
            // rather than 404 on a retry.
            if let Ok(agent) = state.jod.agent(&existing).await {
                return Ok((StatusCode::OK, location(&agent), Json(agent)).into_response());
            }
        }
    }

    let running = state
        .jod
        .agents()
        .await
        .iter()
        .filter(|a| a.status == AgentStatus::Running)
        .count();
    if running >= state.config.max_concurrent_agents {
        audit_write(&state, &identity, "spawn", None, "refused_capacity");
        return Err(ApiError::TooManyAgents {
            limit: state.config.max_concurrent_agents,
        });
    }

    let req = SpawnRequest {
        name: body.name.unwrap_or_else(|| default_name(&prompt)),
        harness: body.harness,
        prompt,
        system: None,
        cwd,
        model: body.model,
        permission: body.permission,
        resume: body.resume,
        tools: None,
    };

    let agent = state.jod.spawn_agent(req).await.map_err(|e| {
        audit_write(&state, &identity, "spawn", None, "failed");
        ApiError::from(e)
    })?;

    if let Some(key) = key.as_deref() {
        state.idempotency.put(&identity.label, key, &agent.id, now);
    }
    audit_write(&state, &identity, "spawn", Some(&agent.id), "ok");

    Ok((StatusCode::CREATED, location(&agent), Json(agent)).into_response())
}

/// Stop an agent, and everything it started.
///
/// Killing an already-finished agent is not an error: the session outlives the
/// agent, so this also serves as "reclaim the session".
pub async fn kill_agent(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if let Err(e) = identity.require(Scope::Write) {
        audit_write(&state, &identity, "kill", Some(&id), "refused_scope");
        return Err(e);
    }
    state.jod.kill_agent(&id).await.map_err(|e| {
        audit_write(&state, &identity, "kill", Some(&id), "failed");
        ApiError::from(e)
    })?;
    audit_write(&state, &identity, "kill", Some(&id), "ok");
    Ok(StatusCode::NO_CONTENT)
}

fn location(agent: &AgentSummary) -> [(axum::http::HeaderName, String); 1] {
    [(
        axum::http::header::LOCATION,
        format!("/v1/agents/{}", agent.id),
    )]
}

/// The spelling `parse_permission` reads back, from the one definition of it.
fn permission_id(p: PermissionPolicy) -> &'static str {
    p.as_str()
}

fn audit_write(
    state: &AppState,
    identity: &Identity,
    action: &str,
    agent_id: Option<&str>,
    outcome: &str,
) {
    let mut e = audit::entry(action, &identity.label, outcome);
    e.agent_id = agent_id.map(str::to_string);
    e.tailnet_user = identity.tailnet_user.clone();
    state.audit.append(&e);
}

/// A short, human-recognisable name from the prompt's first words — the same
/// rule the CLI uses, so an agent is called the same thing in both places.
pub fn default_name(prompt: &str) -> String {
    let name = prompt
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        "agent".to_string()
    } else if name.chars().count() > 48 {
        format!("{}…", name.chars().take(47).collect::<String>())
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_derived_from_the_first_words_of_the_prompt() {
        assert_eq!(
            default_name("summarise the inbox please now ok"),
            "summarise the inbox please now"
        );
    }

    #[test]
    fn an_empty_prompt_still_yields_a_usable_name() {
        assert_eq!(default_name("   "), "agent");
    }

    #[test]
    fn a_long_name_is_truncated_rather_than_left_unbounded() {
        let name = default_name(&"averyverylongword ".repeat(5));
        assert!(name.chars().count() <= 48);
    }

    #[test]
    fn a_spawn_body_needs_only_a_prompt() {
        let body: SpawnBody = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
        assert_eq!(body.harness, HarnessKind::ClaudeCode);
        assert_eq!(body.permission, PermissionPolicy::Ask);
        assert_eq!(body.resume, Resume::Fresh);
        assert!(body.cwd.is_none());
    }

    #[test]
    fn resume_accepts_the_three_documented_forms() {
        let fresh: SpawnBody = serde_json::from_str(r#"{"prompt":"h","resume":"fresh"}"#).unwrap();
        assert_eq!(fresh.resume, Resume::Fresh);
        let last: SpawnBody = serde_json::from_str(r#"{"prompt":"h","resume":"last"}"#).unwrap();
        assert_eq!(last.resume, Resume::Last);
        let session: SpawnBody =
            serde_json::from_str(r#"{"prompt":"h","resume":{"session":"s-1"}}"#).unwrap();
        assert_eq!(session.resume, Resume::Session("s-1".into()));
    }

    #[test]
    fn every_harness_id_is_accepted_in_a_spawn_body() {
        for id in ["claude_code", "open_code", "agy"] {
            let body: SpawnBody =
                serde_json::from_str(&format!(r#"{{"prompt":"h","harness":"{id}"}}"#)).unwrap();
            assert_eq!(body.harness.id(), id);
        }
    }

    #[test]
    fn an_unknown_harness_is_rejected_rather_than_defaulted() {
        let r: Result<SpawnBody, _> = serde_json::from_str(r#"{"prompt":"h","harness":"gpt"}"#);
        assert!(r.is_err(), "an unknown harness silently became the default");
    }

    #[test]
    fn permission_ids_match_the_wire_spelling_core_uses() {
        for p in [
            PermissionPolicy::Ask,
            PermissionPolicy::AcceptEdits,
            PermissionPolicy::Bypass,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", permission_id(p)));
        }
    }
}
