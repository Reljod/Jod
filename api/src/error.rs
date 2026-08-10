//! Errors as [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) problem details.
//!
//! One machine-readable shape for every failure, so a mobile client can branch
//! on `type` instead of pattern-matching prose that changes.
//!
//! Two rules hold throughout:
//!
//! - **An authentication failure never explains itself.** "No such token",
//!   "expired", "wrong scope for this token" are all one opaque 401. Anything
//!   more is an oracle for guessing credentials.
//! - **A credential never appears in a body.** Not the token, not a prefix of
//!   it, not its hash.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Problem {
    /// A stable URI-ish identifier clients may branch on.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub title: &'static str,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("too many agents are already running")]
    TooManyAgents { limit: usize },
    #[error("internal error")]
    Internal(String),
}

impl ApiError {
    fn problem(&self) -> (StatusCode, Problem) {
        match self {
            // Deliberately detail-free: see the module note.
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Problem {
                    kind: "about:blank#unauthorized",
                    title: "Unauthorized",
                    status: 401,
                    detail: None,
                },
            ),
            ApiError::Forbidden(d) => (
                StatusCode::FORBIDDEN,
                Problem {
                    kind: "about:blank#forbidden",
                    title: "Forbidden",
                    status: 403,
                    detail: Some(d.clone()),
                },
            ),
            ApiError::NotFound(d) => (
                StatusCode::NOT_FOUND,
                Problem {
                    kind: "about:blank#not-found",
                    title: "Not Found",
                    status: 404,
                    detail: Some(d.clone()),
                },
            ),
            ApiError::BadRequest(d) => (
                StatusCode::BAD_REQUEST,
                Problem {
                    kind: "about:blank#bad-request",
                    title: "Bad Request",
                    status: 400,
                    detail: Some(d.clone()),
                },
            ),
            ApiError::TooManyAgents { limit } => (
                StatusCode::TOO_MANY_REQUESTS,
                Problem {
                    kind: "about:blank#too-many-agents",
                    title: "Too Many Agents",
                    status: 429,
                    detail: Some(format!(
                        "at most {limit} agents may run at once; kill one or raise max_concurrent_agents"
                    )),
                },
            ),
            ApiError::Internal(d) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Problem {
                    kind: "about:blank#internal",
                    title: "Internal Server Error",
                    status: 500,
                    detail: Some(d.clone()),
                },
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, problem) = self.problem();
        let mut response = (status, axum::Json(problem)).into_response();
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        // RFC 9457 says a 401 carries a challenge; without it a browser client
        // cannot tell "log in" from "you are logged in but blocked".
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

/// Map core's errors without leaking internals a client cannot act on.
impl From<jod_core::JodError> for ApiError {
    fn from(e: jod_core::JodError) -> Self {
        match e {
            jod_core::JodError::UnknownAgent(id) => ApiError::NotFound(format!("no agent `{id}`")),
            jod_core::JodError::HarnessNotFound(h) => {
                ApiError::BadRequest(format!("harness `{h}` is not installed on this machine"))
            }
            jod_core::JodError::SupervisorNotFound => ApiError::Internal(
                "`jod-run` is not installed on this machine, and it supervises every agent".into(),
            ),
            other => ApiError::Internal(other.to_string()),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(e: ApiError) -> (StatusCode, serde_json::Value) {
        let r = e.into_response();
        let status = r.status();
        let bytes = to_bytes(r.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn an_unauthorized_response_explains_nothing() {
        let (status, body) = body_of(ApiError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // A 401 that distinguishes "unknown token" from "expired token" is an
        // oracle. There must be no detail at all.
        assert!(body.get("detail").is_none(), "401 leaked a reason: {body}");
        assert_eq!(body["status"], 401);
    }

    #[tokio::test]
    async fn a_401_carries_a_bearer_challenge() {
        let r = ApiError::Unauthorized.into_response();
        assert_eq!(r.headers().get("www-authenticate").unwrap(), "Bearer");
    }

    #[tokio::test]
    async fn every_error_is_served_as_problem_json() {
        for e in [
            ApiError::Unauthorized,
            ApiError::Forbidden("x".into()),
            ApiError::NotFound("x".into()),
            ApiError::BadRequest("x".into()),
            ApiError::TooManyAgents { limit: 8 },
            ApiError::Internal("x".into()),
        ] {
            let r = e.into_response();
            assert_eq!(
                r.headers().get("content-type").unwrap(),
                "application/problem+json"
            );
        }
    }

    #[tokio::test]
    async fn the_status_field_matches_the_http_status() {
        let (status, body) = body_of(ApiError::TooManyAgents { limit: 3 }).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["status"], 429);
        assert!(body["detail"].as_str().unwrap().contains('3'));
    }

    #[tokio::test]
    async fn an_unknown_agent_becomes_a_404_not_a_500() {
        let e: ApiError = jod_core::JodError::UnknownAgent("abc".into()).into();
        let (status, _) = body_of(e).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_missing_harness_is_the_callers_fault_not_a_500() {
        let e: ApiError = jod_core::JodError::HarnessNotFound("agy".into()).into();
        let (status, _) = body_of(e).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
