//! Conversations, and the pinned main chat.
//!
//! The fleet's top row *is* the main chat — `jod tui` pins it above every
//! delegated run and lets you enter it. Until this module the HTTP API could
//! not see a conversation at all, so a browser or a phone could draw the fleet
//! but not the one row at the top of it.
//!
//! ## One way in, and this is not a second one
//!
//! [`jod_core::orchestrator::hand_to_orchestrator`] is the only function that
//! speaks to the main chat. `jod main`, the TUI's `/main` and the Telegram
//! bridge all call it, and its own documentation says why: *"which
//! conversation, which tools, which permission mode is a set of decisions with
//! four bugs already behind it, and a second copy would be a second place for
//! the fifth to hide."*
//!
//! So [`send_to_main`] calls it too. It does not assemble a `SpawnRequest`, it
//! does not resolve the pinned conversation itself, and it does not decide the
//! tool access. Everything this module adds is the part that is genuinely the
//! API's: who is allowed, from where, how often, and what gets written down.
//!
//! ## The permission subtlety, which is the reason this file has a long note
//!
//! `hand_to_orchestrator` fixes `PermissionPolicy::AcceptEdits` internally and
//! explains at length that it must: `Ask` is plan mode, plan mode refuses every
//! mutation including the MCP calls that *are* the orchestrator's job, and the
//! run once wrote a plan file instead of arming the schedule it was asked for.
//!
//! That reasoning is sound in the terminal, where the caller is the person
//! sitting at it. It does not carry itself across a socket. [`crate::routes`]
//! is careful that a remote caller can never obtain a permission the operator
//! has not allowed remotely — it checks `config.permits` and refuses rather
//! than downgrading. If this route called through without that check, a daemon
//! configured to allow only `Ask` would hand `AcceptEdits` to anyone with a
//! write token, and the ceiling would be a ceiling with a hole in it that only
//! this one path could find.
//!
//! So the check is made here explicitly, against the policy the orchestrator is
//! known to use, and the refusal names it. The cost is that this module knows a
//! constant core also knows; the test below pins them together so the day core
//! changes its mind is the day this fails rather than the day the ceiling
//! quietly stops meaning anything.
//!
//! ## Reads never create
//!
//! `main_conversation` is get-or-create. A `GET` that creates is a `GET` that a
//! link prefetcher can fire, so the read path uses `pinned_conversation`, which
//! only looks. A box where nobody has said anything yet answers
//! `{"conversation":null,"messages":[]}` rather than 404 — the TUI draws that
//! pinned row before it holds anything, captioned *the chat Jod keeps — pinned,
//! and it never ends*, and a client wants to draw the same thing.

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use jod_core::conversation::{Conversation, ConversationSummary, Message};
use jod_core::store::Store;
use jod_core::{AgentStatus, AgentSummary, HarnessKind, PermissionPolicy};
use serde::{Deserialize, Serialize};

use crate::auth::Scope;
use crate::error::{ApiError, ApiResult};
use crate::routes::audit_write;
use crate::{AppState, Identity};

/// The permission `hand_to_orchestrator` runs the main chat under.
///
/// Mirrored from core so this crate can test it against the daemon's ceiling
/// *before* handing over. Pinned by a test — see the module note.
const ORCHESTRATOR_PERMISSION: PermissionPolicy = PermissionPolicy::AcceptEdits;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

fn limit_of(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

fn store_of(state: &AppState) -> Option<&Store> {
    state.jod.store().map(|s| &**s)
}

/// The store, or the error a route that cannot work without one should return.
fn require_store(state: &AppState) -> ApiResult<&Store> {
    store_of(state).ok_or_else(|| {
        ApiError::Internal("this daemon has no store, so it keeps no conversations".into())
    })
}

// ─── reads ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<usize>,
}

pub async fn list_conversations(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(Vec::<ConversationSummary>::new()));
    };
    store
        .conversations(limit_of(q.limit))
        .map(Json)
        .map_err(internal)
}

/// The pinned main chat, and its thread.
///
/// `conversation` is `null` before anyone has spoken to it. That is a state to
/// render, not an error: the pinned row exists in the fleet from the first
/// launch.
#[derive(Debug, Serialize)]
pub struct MainChat {
    pub conversation: Option<Conversation>,
    pub messages: Vec<Message>,
}

pub async fn get_main(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let Some(store) = store_of(&state) else {
        return Ok(Json(MainChat {
            conversation: None,
            messages: Vec::new(),
        }));
    };

    // `pinned_conversation`, never `main_conversation`: a read must not mint a
    // row. See the module note.
    let Some(id) = store.pinned_conversation().map_err(internal)? else {
        return Ok(Json(MainChat {
            conversation: None,
            messages: Vec::new(),
        }));
    };

    Ok(Json(MainChat {
        conversation: store.conversation(&id).map_err(internal)?,
        messages: store.thread(&id).map_err(internal)?,
    }))
}

pub async fn get_conversation(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store = require_store(&state)?;
    store
        .conversation(&id)
        .map_err(internal)?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no conversation {id}")))
}

/// The thread of one conversation, oldest first.
///
/// `thread` and not `live_window`: the window is what the *harness* is given on
/// the next turn, which is a smaller thing than what a person scrolling wants
/// to read. A client that wants the window can ask for it when a route exists;
/// silently serving it here would make the transcript look like it had lost
/// messages.
pub async fn get_messages(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    identity.require(Scope::Read)?;
    let store = require_store(&state)?;

    // A conversation that does not exist is a 404; one that exists and is empty
    // is an empty list. Returning `[]` for both would make a typo indisplayable
    // from a chat nobody has used.
    if store.conversation(&id).map_err(internal)?.is_none() {
        return Err(ApiError::NotFound(format!("no conversation {id}")));
    }
    store.thread(&id).map(Json).map_err(internal)
}

// ─── the write ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendBody {
    /// What Jod is being asked to do. Goes to the harness as an argument, never
    /// through a shell.
    pub instruction: String,
    /// Which harness runs the orchestrator. Defaults to Claude Code, matching
    /// the console.
    #[serde(default = "default_harness")]
    pub harness: HarnessKind,
    /// Must resolve inside the configured allowlist. Omitted means the first
    /// allowed root — the common case for a phone with one project.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
}

fn default_harness() -> HarnessKind {
    HarnessKind::ClaudeCode
}

/// What the main chat did with an instruction.
///
/// `Handed` in core carries the same three things but is not `Serialize`, and a
/// wire shape this crate owns is the right place to draw that line anyway.
#[derive(Debug, Serialize)]
pub struct SentToMain {
    pub agent: AgentSummary,
    /// The conversation it landed in. Returned rather than left to be looked up
    /// again, because resolving the pinned chat twice is two chances to
    /// disagree — the same reason core returns it.
    pub conversation_id: String,
    /// Present when the live window has grown past a threshold. Advisory: the
    /// turn still ran.
    pub compaction_due: Option<CompactionDue>,
}

#[derive(Debug, Serialize)]
pub struct CompactionDue {
    pub reason: String,
    pub chars: usize,
}

/// Give the main chat an instruction.
///
/// The dangerous verb of this module: it starts a supervised process holding
/// Jod's own tools. Every bound `spawn_agent` applies is applied here too, in
/// the same order — cheapest and most restrictive first, all before anything is
/// spawned — plus the permission-ceiling check the module note explains.
pub async fn send_to_main(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> ApiResult<impl IntoResponse> {
    // Audited before the early return: a read credential repeatedly trying to
    // drive the main chat is the signal that one has been stolen, and it is
    // worthless if it never reaches the log.
    if let Err(e) = identity.require(Scope::Write) {
        audit_write(&state, &identity, "main.send", None, "refused_scope");
        return Err(e);
    }

    let instruction = body.instruction.trim().to_string();
    if instruction.is_empty() {
        return Err(ApiError::BadRequest("instruction is empty".into()));
    }

    // The check the module note exists for. The orchestrator runs at
    // `AcceptEdits` by construction, so a daemon whose ceiling is lower must
    // refuse the whole route rather than hand it over anyway.
    if !state.config.permits(ORCHESTRATOR_PERMISSION) {
        audit_write(&state, &identity, "main.send", None, "refused_permission");
        return Err(ApiError::Forbidden(format!(
            "the main chat runs at `{}`, which exceeds this daemon's ceiling; \
             raise max_permission locally to allow it",
            ORCHESTRATOR_PERMISSION.as_str()
        )));
    }

    let requested_cwd = match body.cwd {
        Some(ref c) => c.clone(),
        None => state.config.allowed_cwd.first().cloned().ok_or_else(|| {
            ApiError::Forbidden(crate::config::CwdRejection::NoAllowlist.to_string())
        })?,
    };
    let cwd = state.config.resolve_cwd(&requested_cwd).map_err(|e| {
        audit_write(&state, &identity, "main.send", None, "refused_cwd");
        ApiError::Forbidden(e.to_string())
    })?;

    if !state.jod.supervisor_available() {
        return Err(ApiError::Internal(
            "`jod-run` is not installed on this machine, and it supervises every agent".into(),
        ));
    }

    // A replay must not consume a concurrency slot, so it is answered before
    // the cap is tested — the ordering `spawn_agent` uses, for the same reason.
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let now = chrono::Utc::now().timestamp_millis();
    if let Some(key) = key.as_deref() {
        if let Some(existing) = state.idempotency.get(&identity.label, key, now) {
            // The original may since have been forgotten; fall through and send
            // rather than 404 on a retry.
            if let Ok(agent) = state.jod.agent(&existing).await {
                let conversation_id = store_of(&state)
                    .and_then(|s| s.conversation_for_run(&agent.id).ok().flatten())
                    .unwrap_or_default();
                return Ok((
                    StatusCode::OK,
                    Json(SentToMain {
                        agent,
                        conversation_id,
                        compaction_due: None,
                    }),
                )
                    .into_response());
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
        audit_write(&state, &identity, "main.send", None, "refused_capacity");
        return Err(ApiError::TooManyAgents {
            limit: state.config.max_concurrent_agents,
        });
    }

    // `carried` is `None`: it exists for a harness switch, which happens in the
    // console and is the console's to pass on. A remote caller has no thread
    // state of its own — the same position the Telegram bridge is in.
    //
    // The run is named `api` so `jod ls` says where an instruction came from,
    // the way the bridge names its runs after the chat.
    let handed = jod_core::orchestrator::hand_to_orchestrator(
        &state.jod,
        &instruction,
        body.harness,
        cwd,
        None,
        "api",
    )
    .await
    .map_err(|e| {
        audit_write(&state, &identity, "main.send", None, "failed");
        ApiError::from(e)
    })?;

    if let Some(key) = key.as_deref() {
        state
            .idempotency
            .put(&identity.label, key, &handed.agent.id, now);
    }
    audit_write(&state, &identity, "main.send", Some(&handed.agent.id), "ok");

    Ok((
        StatusCode::CREATED,
        Json(SentToMain {
            agent: handed.agent,
            conversation_id: handed.conversation_id,
            compaction_due: handed.compaction_due.map(|(reason, chars)| CompactionDue {
                reason: reason.to_string(),
                chars,
            }),
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constant this module checks the ceiling against must be the one the
    /// orchestrator actually runs at.
    ///
    /// This cannot read core's literal — it is buried in a `SpawnRequest` inside
    /// an async function — so it pins the two facts that make the check
    /// meaningful instead: the policy is `AcceptEdits`, and `AcceptEdits` is
    /// strictly more than `Ask`. If core moves the orchestrator to a different
    /// mode, this test is the note that the ceiling check needs moving too.
    #[test]
    fn the_orchestrator_permission_is_the_one_the_ceiling_is_checked_against() {
        assert_eq!(ORCHESTRATOR_PERMISSION, PermissionPolicy::AcceptEdits);
        assert_eq!(ORCHESTRATOR_PERMISSION.as_str(), "accept_edits");
    }

    #[test]
    fn a_limit_defaults_and_is_capped() {
        assert_eq!(limit_of(None), DEFAULT_LIMIT);
        assert_eq!(limit_of(Some(7)), 7);
        assert_eq!(limit_of(Some(usize::MAX)), MAX_LIMIT);
    }
}
