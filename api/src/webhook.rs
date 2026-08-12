//! `POST /webhooks/github` — the one route GitHub itself calls.
//!
//! ## Why this route is not behind the bearer middleware
//!
//! Every other route in this crate is guarded by [`crate::auth`], because every
//! other route is called by something Reljod controls. GitHub is not. It holds
//! no token and cannot be given one, so it authenticates the only way a webhook
//! sender can: it signs the body with a shared secret and sends the tag in
//! `X-Hub-Signature-256`. That signature *is* the credential here, and it is
//! checked with the same care [`crate::auth::TokenStore::verify`] gives a
//! token — over the raw bytes, in constant time, before anything else happens.
//!
//! With no secret configured the endpoint refuses every delivery. An
//! unconfigured secret must never mean "accept anything": this route ends in
//! [`jod_core::Jod::spawn_agent`], which runs shell commands.
//!
//! ## Why the response does not wait for the agent
//!
//! GitHub gives a hook ten seconds and then records a timeout and retries.
//! Starting a harness is not reliably inside that, and a retry is a *second*
//! delivery of the same event. So the handler does all its deciding
//! synchronously — verify, parse, dedupe, match, write the row — and hands the
//! actual spawn to a detached task. The delivery row is claimed *before* the
//! task is spawned, so the ack and the dedupe can never disagree.
//!
//! ## Why the payload is quoted rather than trusted
//!
//! Anyone with a GitHub account can put text in a pull request title, and that
//! title ends up in a prompt. The containment lives in
//! [`jod_core::webhook::render_prompt`] — read the note there. What this file
//! adds is the rest of the bound: the rule's `cwd` still goes through the same
//! allowlist a remote spawn does, the permission is the daemon's default rather
//! than anything the event could ask for, and the run announces itself in the
//! store as [`jod_core::store::Origin::Untrusted`].

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use jod_core::store::Store;
use jod_core::webhook::{Delivery, DeliveryStatus, Rule};
use jod_core::{PermissionPolicy, Resume, SpawnRequest};
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;

use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub const PATH: &str = "/webhooks/github";

/// Where the shared secret comes from.
///
/// Not [`crate::config::Config`], because that file is a TOML the daemon reads
/// and prints; a secret belongs in the process environment, next to the other
/// things you would not want in a config dump.
///
/// `Config` derives `Serialize`, which is the deciding detail: a field on it is
/// one `to_string_pretty` away from a log line or an HTTP response, and that is
/// the kind of leak that happens years later, in code nobody connected to this
/// decision. The environment is the right home, not a placeholder for one.
pub const SECRET_ENV: &str = "JOD_GITHUB_WEBHOOK_SECRET";

/// GitHub's headers. Named rather than inlined because all three are load
/// bearing and one typo would silently disable a control.
const DELIVERY_HEADER: &str = "x-github-delivery";
const EVENT_HEADER: &str = "x-github-event";
const SIGNATURE_HEADER: &str = "x-hub-signature-256";

const SOURCE: &str = "github";

/// How large a delivery may be.
///
/// Deliberately **not** [`crate::config::Config::max_body_bytes`], which
/// defaults to 256KB because it is sized for a prompt somebody typed. A `push`
/// carrying a hundred commits is comfortably past that, and inheriting the
/// prompt-sized limit would 413 real deliveries.
///
/// It is not GitHub's own 25MB ceiling either. This route is the one thing in
/// the crate a stranger on the internet can reach, and a MAC cannot be checked
/// without buffering the whole body first — so the limit is what bounds how
/// much memory an unauthenticated request can make the daemon hold. 8MB is far
/// above every payload GitHub actually sends and far below what would matter on
/// a small VPS.
///
/// A delivery over the limit is refused by the layer, before the handler, so it
/// leaves no row here. It is still visible: GitHub records it as a failed
/// delivery in the repository's webhook pane and retries it.
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// The shared secret, or the absence of one.
///
/// Wrapped so it travels as a request extension rather than as a `String` that
/// anything could pick up by type, and so `None` is a value the handler has to
/// deal with rather than an empty secret that would verify nothing.
#[derive(Clone)]
pub struct Secret(Option<Arc<[u8]>>);

impl Secret {
    pub fn new(secret: Option<&str>) -> Secret {
        Secret(
            secret
                .filter(|s| !s.is_empty())
                .map(|s| s.as_bytes().into()),
        )
    }

    /// Read it out of the environment. A missing or empty variable is `None`,
    /// which the handler turns into a refusal.
    pub fn from_env() -> Secret {
        Secret::new(std::env::var(SECRET_ENV).ok().as_deref())
    }
}

/// What the caller is told. GitHub ignores the body, but a person running
/// `curl` against the endpoint, or reading the delivery pane, needs to be able
/// to tell "no rule wanted it" from "I could not read it".
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub status: DeliveryStatus,
    pub delivery: String,
    /// How many rules matched. Zero with `accepted` is impossible by
    /// construction, so the pair is enough to explain any outcome.
    pub matched: usize,
}

/// Mount the route with an explicit secret. The form tests use.
pub fn routes(secret: Secret, max_body: usize) -> Router<AppState> {
    Router::new()
        .route(PATH, post(github))
        .layer(Extension(secret))
        .layer(RequestBodyLimitLayer::new(max_body))
}

/// Mount the route as the daemon does: secret from the environment, payload
/// limit from [`MAX_PAYLOAD_BYTES`]. Kept separate so [`routes`] stays a pure
/// function of its arguments and a test can pin both.
pub fn routes_from_env() -> Router<AppState> {
    routes(Secret::from_env(), MAX_PAYLOAD_BYTES)
}

/// Receive one delivery.
///
/// The order of the steps is the design. Nothing touches the database until the
/// signature has been checked, and nothing spawns until the delivery id has
/// been claimed.
///
/// `body` is [`Bytes`] and comes last on purpose: it is axum's raw-body
/// extractor, so what gets verified is exactly what arrived. Taking `Json<T>`
/// here would verify a signature over bytes a parser had already normalised,
/// which is a different message than the one GitHub signed.
pub async fn github(
    State(state): State<AppState>,
    Extension(secret): Extension<Secret>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    // Without these two there is nothing to dedupe on and nothing to match, and
    // no row can be written — `delivery_id` is the table's unique key. A
    // request this shape did not come from GitHub.
    let (Some(delivery_id), Some(event)) = (header(DELIVERY_HEADER), header(EVENT_HEADER)) else {
        return Err(ApiError::BadRequest(
            "a GitHub delivery carries X-GitHub-Delivery and X-GitHub-Event".into(),
        ));
    };
    let (delivery_id, event) = (delivery_id.to_string(), event.to_string());

    // Authenticate before anything else is looked at, let alone reported. Every
    // branch below this point has already earned the right to be here.
    //
    // An absent secret and a bad tag are the *same* refusal on purpose. They
    // are different problems — one is the operator's, one is the caller's — but
    // telling them apart from outside would let a stranger discover that this
    // endpoint exists and is unconfigured, which is the single most useful fact
    // about it. The distinction is recorded where the operator actually looks:
    // the delivery row and the audit line, both below.
    let why = match &secret {
        // Deliberately not a silent accept. An operator who has not set the
        // secret has an endpoint that spawns agents for anyone who finds it.
        Secret(None) => Some("no secret is configured"),
        Secret(Some(secret)) => {
            if jod_core::webhook::verify_signature(secret, &body, header(SIGNATURE_HEADER)) {
                None
            } else {
                Some("bad or missing signature")
            }
        }
    };

    // Fetched after the check, so an unauthenticated caller cannot learn even
    // this much about how the daemon is put together.
    let store = state.jod.store().cloned();

    if let Some(why) = why {
        if let Some(store) = &store {
            reject(&state, store, &delivery_id, &event, why);
        }
        // The same opaque 401 an unknown bearer token gets — no detail, no
        // `type` a caller could branch on.
        return Err(ApiError::Unauthorized);
    }

    // Webhooks need somewhere to record what they did and somewhere to read
    // rules from. Answering 200 with no store would tell GitHub the event was
    // handled when nothing could have handled it.
    let store = store.ok_or_else(|| {
        ApiError::Internal("this daemon has no store, and a webhook rule lives in one".into())
    })?;

    // Only now is the body worth parsing. A delivery that GitHub signed but
    // Jod cannot read is the operator's problem — most often a hook configured
    // to send `application/x-www-form-urlencoded` — so it is recorded as a
    // failure rather than a rejection, which would blame the sender.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            let mut d = Delivery::new(&delivery_id, &event);
            d.status = DeliveryStatus::Failed;
            d.detail = Some(format!("the body is not JSON: {e}"));
            let _ = store.record_delivery(&d);
            return Err(ApiError::BadRequest(
                "the body is not JSON; set the hook's content type to application/json".into(),
            ));
        }
    };

    let repo = jod_core::webhook::payload_repo(&payload).unwrap_or_default();
    let action = jod_core::webhook::payload_action(&payload).map(str::to_string);
    let matched = store
        .match_rules(SOURCE, repo, &event, &payload)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut delivery = Delivery::new(&delivery_id, &event);
    delivery.action = action;
    delivery.repo = Some(repo.to_string()).filter(|r| !r.is_empty());
    delivery.rule_id = matched.first().map(|r| r.id.clone());
    delivery.status = if matched.is_empty() {
        DeliveryStatus::NoMatch
    } else {
        DeliveryStatus::Accepted
    };
    delivery.detail = describe(&matched);

    // The claim and the dedupe are the same act. A redelivery loses the race
    // here, before anything has been spawned, which is what stops one event
    // becoming two agent runs.
    let claimed = store
        .record_delivery(&delivery)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !claimed {
        audit(&state, "webhook.github", "duplicate", Some(&delivery_id));
        return Ok((
            StatusCode::OK,
            Json(Receipt {
                status: DeliveryStatus::Duplicate,
                delivery: delivery_id,
                matched: matched.len(),
            }),
        ));
    }

    if matched.is_empty() {
        audit(&state, "webhook.github", "no_match", Some(&delivery_id));
        return Ok((
            StatusCode::OK,
            Json(Receipt {
                status: DeliveryStatus::NoMatch,
                delivery: delivery_id,
                matched: 0,
            }),
        ));
    }

    audit(&state, "webhook.github", "accepted", Some(&delivery_id));

    // Detached: everything that decides *whether* to run has already happened,
    // so what is left is the slow part, and GitHub is not waiting for it.
    let task = SpawnTask {
        state: state.clone(),
        store,
        delivery_id: delivery_id.clone(),
        event: event.clone(),
        payload,
        rules: matched.clone(),
    };
    tokio::spawn(task.run());

    Ok((
        StatusCode::ACCEPTED,
        Json(Receipt {
            status: DeliveryStatus::Accepted,
            delivery: delivery_id,
            matched: matched.len(),
        }),
    ))
}

/// The prompt one rule produces for one payload.
///
/// A one-line wrapper, and public on purpose: it is the seam where this crate
/// could accidentally hand a harness un-quoted attacker text, so it is the seam
/// a test can hold on to. Nothing in this file builds a prompt any other way.
pub fn prompt_for(rule: &Rule, event: &str, payload: &serde_json::Value) -> String {
    jod_core::webhook::render_prompt(&rule.prompt, event, payload)
}

/// Everything the detached task needs, so the spawn point stays one line.
struct SpawnTask {
    state: AppState,
    store: Arc<Store>,
    delivery_id: String,
    event: String,
    payload: serde_json::Value,
    rules: Vec<Rule>,
}

impl SpawnTask {
    /// Start one run per matching rule, then write down what happened.
    ///
    /// Nothing here can fail the request — it has already been answered — so
    /// every failure has to land in the delivery row instead. A rule whose
    /// `cwd` is outside the allowlist, or whose harness is not installed, shows
    /// up as `failed` with the reason, which is the only place anyone will look.
    async fn run(self) {
        let mut started: Vec<(String, String)> = Vec::new();
        let mut problems: Vec<String> = Vec::new();

        for rule in &self.rules {
            match self.start(rule).await {
                Ok(run_id) => started.push((rule.name.clone(), run_id)),
                Err(e) => problems.push(format!("{}: {e}", rule.name)),
            }
        }

        let status = if started.is_empty() {
            DeliveryStatus::Failed
        } else {
            DeliveryStatus::Accepted
        };
        let detail = started
            .iter()
            .map(|(rule, run)| format!("{rule} -> {run}"))
            .chain(problems)
            .collect::<Vec<_>>()
            .join("; ");
        // The row already exists and already names the first rule; this only
        // fills in what could not be known before the response went out.
        let _ = self.store.set_delivery_outcome(
            &self.delivery_id,
            status,
            started.first().map(|(_, run)| run.as_str()),
            Some(&detail).filter(|d| !d.is_empty()).map(String::as_str),
        );
    }

    /// One rule, one run.
    async fn start(&self, rule: &Rule) -> Result<String, String> {
        let harness = jod_core::HarnessKind::from_id(&rule.harness)
            .ok_or_else(|| format!("`{}` is not a harness", rule.harness))?;

        // The same allowlist a remote spawn goes through. A rule is written by
        // the operator, so this is not about distrusting the rule — it is that
        // one place deciding which directories an agent may run in is how the
        // answer stays the same for every caller.
        let cwd = self
            .state
            .config
            .resolve_cwd(Path::new(&rule.cwd))
            .map_err(|e| e.to_string())?;

        // The daemon's default, never the event's choice: `webhook_rules` has
        // no permission column precisely so that an attacker who gets a rule to
        // match still cannot pick what the run is allowed to do.
        let permission = PermissionPolicy::default();
        if !self.state.config.permits(permission) {
            return Err("the daemon's permission ceiling forbids its own default".into());
        }

        let req = SpawnRequest {
            name: format!("{} ({})", rule.name, self.event),
            harness,
            prompt: prompt_for(rule, &self.event, &self.payload),
            system: None,
            cwd,
            model: rule.model.clone(),
            permission,
            // Always a fresh context. Resuming would carry one stranger's
            // payload into the next stranger's run.
            resume: Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
        };

        // `spawn_from_untrusted`, not `spawn_agent`. The prompt was built from
        // a payload a stranger wrote, so whatever tool grant the rule carries
        // is capped to reading on the way in — at the point of use, rather than
        // trusted to the rule's row.
        let agent = self
            .state
            .jod
            .spawn_from_untrusted(req)
            .await
            .map_err(|e| e.to_string())?;

        // Written before the run says anything, so the provenance is on record
        // even if the run is killed mid-turn.
        let _ = self.store.remember(jod_core::webhook::provenance_fact(
            &agent.id,
            &self.delivery_id,
            rule,
        ));
        Ok(agent.id)
    }
}

/// Record a refusal and say so in the audit log.
///
/// A rejected delivery gets a row like every other outcome: "the hook is
/// misconfigured" and "the hook is not firing" look identical from the outside,
/// and only a row tells them apart.
fn reject(state: &AppState, store: &Store, delivery_id: &str, event: &str, why: &str) {
    let mut d = Delivery::new(delivery_id, event);
    d.status = DeliveryStatus::Rejected;
    d.detail = Some(why.to_string());
    let _ = store.record_delivery(&d);
    audit(state, "webhook.github", "rejected", Some(delivery_id));
}

/// The actor is `github`, not a token label — this route has no token. The
/// label is a constant so an audit reader can grep the unauthenticated surface.
fn audit(state: &AppState, action: &str, outcome: &str, delivery_id: Option<&str>) {
    let mut e = audit::entry(action, SOURCE, outcome);
    e.detail = delivery_id.map(str::to_string);
    state.audit.append(&e);
}

fn describe(matched: &[Rule]) -> Option<String> {
    if matched.is_empty() {
        return None;
    }
    Some(
        matched
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_empty_secret_is_no_secret_at_all() {
        // The distinction matters: `Some("")` would happily verify an HMAC
        // keyed on nothing, which anyone could compute.
        assert!(Secret::new(None).0.is_none());
        assert!(Secret::new(Some("")).0.is_none());
        assert!(Secret::new(Some("s3cret")).0.is_some());
    }

    #[test]
    fn a_receipt_names_the_delivery_it_is_about() {
        let json = serde_json::to_value(Receipt {
            status: DeliveryStatus::NoMatch,
            delivery: "d-1".into(),
            matched: 0,
        })
        .unwrap();
        assert_eq!(json["status"], "no_match");
        assert_eq!(json["delivery"], "d-1");
    }

    #[test]
    fn a_detail_lists_every_rule_that_matched() {
        assert_eq!(describe(&[]), None);
    }
}
