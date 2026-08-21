//! Deleting a work over HTTP.
//!
//! A module of its own rather than a handler bolted onto [`crate::workspaces`],
//! which states at the top of the file that every route in it is a read and
//! that adding a write is "a separate decision with its own audit-trail
//! obligations". This is that decision, made separately, with the audit call in
//! it.
//!
//! ## The two-step confirmation is core's, and it is not re-implemented here
//!
//! [`jod_core::store::Store::delete_work`] refuses the first time a work holds
//! git worktrees, hands back everything the delete would take, and arms a
//! confirmation in the database that the *same* request repeated within its
//! window will satisfy. The TUI holds the returned value between two keystrokes;
//! `jod work delete` is two processes and relies on the armed row. HTTP is a
//! third caller with the same shape as the second — every request is its own
//! process, as far as the store is concerned — so it relies on the armed row
//! too, and adds nothing.
//!
//! What this route must not do is offer a `?force=true`. The refusal exists
//! because a worktree can hold uncommitted work, and a flag that skips it is
//! the same delete with the warning turned off.
//!
//! ## Why the refusal is a 409 with a body rather than problem+json
//!
//! The client cannot act on "refused". It can act on "four sessions, 312
//! transcript messages and two dirty worktrees, one of them unmerged" — that is
//! the confirmation dialog. RFC 9457's `detail` is one string, so the counts
//! ride as their own fields and `detail` carries the sentence for a client that
//! reads nothing else. The status is still 4xx, so a client that checks only
//! `res.ok` cannot mistake a refusal for a delete.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use jod_core::store::Store;
use jod_core::works::{Deletion, Doomed};
use serde::Serialize;

use crate::auth::Scope;
use crate::error::{ApiError, ApiResult};
use crate::routes::audit_write;
use crate::{AppState, Identity};

fn require_store(state: &AppState) -> ApiResult<&Store> {
    state
        .jod
        .store()
        .map(|s| &**s)
        .ok_or_else(|| ApiError::Internal("this daemon has no store, so it keeps no works".into()))
}

/// One git worktree the delete would leave behind, and the state it is in.
#[derive(Debug, Serialize)]
pub struct LeaseView {
    pub worktree_path: String,
    pub branch: String,
    pub dirty: bool,
    pub merged: bool,
    /// The directory is already gone from disk — somebody removed it by hand.
    pub missing: bool,
}

/// What a delete would take, counted before anything is taken.
///
/// The API's own shape rather than `Doomed` serialised: core's type carries a
/// `PathBuf` and is not on the wire anywhere else, and pinning the JSON here
/// means a field core renames does not silently rename itself for every client.
#[derive(Debug, Serialize)]
pub struct DoomedView {
    pub work_id: String,
    pub title: String,
    pub sessions: usize,
    pub transcripts: usize,
    pub unanswered_cards: usize,
    pub mail: usize,
    /// Runs whose last transcript this takes. Their rows and costs are kept.
    pub orphaned_runs: usize,
    pub leases: Vec<LeaseView>,
}

impl From<&Doomed> for DoomedView {
    fn from(d: &Doomed) -> Self {
        DoomedView {
            work_id: d.work_id.clone(),
            title: d.title.clone(),
            sessions: d.sessions,
            transcripts: d.transcripts,
            unanswered_cards: d.unanswered_cards,
            mail: d.mail,
            orphaned_runs: d.orphaned_runs,
            leases: d
                .leases
                .iter()
                .map(|c| LeaseView {
                    worktree_path: c.worktree_path.display().to_string(),
                    branch: c.branch.clone(),
                    dirty: c.dirty,
                    merged: c.merged,
                    missing: c.missing,
                })
                .collect(),
        }
    }
}

/// The answer to a delete, whichever way it went.
#[derive(Debug, Serialize)]
pub struct DeleteWorkResponse {
    /// False means nothing was touched and the same request will now go
    /// through. Never assume from the presence of a body.
    pub deleted: bool,
    /// One sentence, for a client that renders nothing else.
    pub detail: String,
    pub doomed: DoomedView,
    /// Worktree paths left on disk. Jod's records are cheap to recreate and a
    /// branch with uncommitted work on it is not, so they are never removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub worktrees_left: Vec<String>,
    /// When the armed confirmation stops working, on a refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_before_ms: Option<i64>,
}

/// Delete a work, and every session in it.
///
/// Repeat the request to confirm when it comes back refused. → the module note.
pub async fn delete_work(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if let Err(e) = identity.require(Scope::Write) {
        audit_write(&state, &identity, "delete_work", Some(&id), "refused_scope");
        return Err(e);
    }
    let store = require_store(&state)?;

    // `None`: the confirmation this route relies on is the one core armed in
    // the database on the previous refusal. See the module note.
    let outcome = store.delete_work(&id, None).map_err(|e| {
        audit_write(&state, &identity, "delete_work", Some(&id), "failed");
        ApiError::from(e)
    })?;

    match outcome {
        Deletion::Refused {
            doomed,
            confirmation,
        } => {
            audit_write(&state, &identity, "delete_work", Some(&id), "refused");
            Ok((
                StatusCode::CONFLICT,
                Json(DeleteWorkResponse {
                    deleted: false,
                    detail: refusal_sentence(&doomed),
                    doomed: DoomedView::from(&*doomed),
                    worktrees_left: Vec::new(),
                    confirm_before_ms: Some(confirmation.expires_at_ms()),
                }),
            )
                .into_response())
        }
        Deletion::Done {
            doomed,
            worktrees_left,
        } => {
            audit_write(&state, &identity, "delete_work", Some(&id), "ok");
            Ok((
                StatusCode::OK,
                Json(DeleteWorkResponse {
                    deleted: true,
                    detail: format!(
                        "deleted `{}` and its {} session(s)",
                        doomed.title, doomed.sessions
                    ),
                    doomed: DoomedView::from(&*doomed),
                    worktrees_left: worktrees_left
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect(),
                    confirm_before_ms: None,
                }),
            )
                .into_response())
        }
    }
}

/// The refusal in one line: what is at stake, and what to do about it.
///
/// Split out so a test can read it without going through HTTP, and written to
/// be shown verbatim in a confirmation dialog — it names the worktrees because
/// those are the part that cannot be recreated.
fn refusal_sentence(doomed: &Doomed) -> String {
    let dirty = doomed.leases.iter().filter(|c| c.dirty).count();
    let unmerged = doomed.leases.iter().filter(|c| !c.merged).count();
    let mut sentence = format!(
        "Deleting `{}` takes {} session(s) and {} transcript message(s), and leaves {} \
         worktree(s) on disk",
        doomed.title,
        doomed.sessions,
        doomed.transcripts,
        doomed.leases.len(),
    );
    if dirty > 0 {
        sentence.push_str(&format!(" — {dirty} of them with uncommitted changes"));
    }
    if unmerged > 0 {
        sentence.push_str(&format!(" — {unmerged} on an unmerged branch"));
    }
    sentence.push_str(". Repeat the request to confirm.");
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::leases::Condition;
    use std::path::PathBuf;

    fn doomed(leases: Vec<Condition>) -> Doomed {
        Doomed {
            work_id: "w1".into(),
            title: "ship the thing".into(),
            sessions: 4,
            transcripts: 312,
            unanswered_cards: 1,
            mail: 0,
            orphaned_runs: 2,
            leases,
        }
    }

    fn lease(dirty: bool, merged: bool) -> Condition {
        Condition {
            worktree_path: PathBuf::from("/tmp/wt"),
            branch: "feat/x".into(),
            dirty,
            merged,
            missing: false,
        }
    }

    #[test]
    fn the_refusal_names_what_cannot_be_recreated() {
        let sentence = refusal_sentence(&doomed(vec![lease(true, false)]));
        assert!(sentence.contains("4 session(s)"), "{sentence}");
        assert!(sentence.contains("uncommitted changes"), "{sentence}");
        assert!(sentence.contains("unmerged branch"), "{sentence}");
        assert!(sentence.contains("Repeat the request"), "{sentence}");
    }

    /// A clean, merged worktree is still lost work in the sense that matters —
    /// it is a directory Jod stops tracking — but it must not be described as
    /// dirty, or the warning stops meaning anything.
    #[test]
    fn a_clean_lease_is_not_described_as_dirty() {
        let sentence = refusal_sentence(&doomed(vec![lease(false, true)]));
        assert!(sentence.contains("1 worktree(s)"), "{sentence}");
        assert!(!sentence.contains("uncommitted"), "{sentence}");
        assert!(!sentence.contains("unmerged"), "{sentence}");
    }

    #[test]
    fn the_view_carries_every_count_the_dialog_needs() {
        let view = DoomedView::from(&doomed(vec![lease(true, false)]));
        assert_eq!(view.sessions, 4);
        assert_eq!(view.transcripts, 312);
        assert_eq!(view.orphaned_runs, 2);
        assert_eq!(view.leases.len(), 1);
        assert_eq!(view.leases[0].branch, "feat/x");
        assert!(view.leases[0].dirty);
    }
}
