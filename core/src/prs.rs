//! Pull requests a run opened.
//!
//! Detected two ways, on purpose. The event stream gives *immediacy* — a URL
//! appears the moment the agent prints it — and polling gives *authority*,
//! because only the forge knows whether it is still open. Neither alone is
//! enough: the stream cannot tell you it was merged an hour later, and polling
//! alone would leave the fleet blank for as long as the poll interval.
//!
//! Jod shows and opens. It never merges — that is `merge_pr.sh`'s job and the
//! charter is explicit that a script decides what merges unread.

use serde::{Deserialize, Serialize};

/// What the forge says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Draft,
    Open,
    Merged,
    Closed,
    /// Parsed out of a stream, never yet reconciled. Honest and common — a URL
    /// is not a status, and claiming one before asking would be inventing it.
    Unknown,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Draft => "draft",
            State::Open => "open",
            State::Merged => "merged",
            State::Closed => "closed",
            State::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "draft" => State::Draft,
            "open" => State::Open,
            "merged" => State::Merged,
            "closed" => State::Closed,
            _ => State::Unknown,
        }
    }
}

/// How Jod first heard about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Stream,
    Poll,
}

/// One pull request, as Jod knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: i64,
    pub work_id: Option<String>,
    pub conversation_id: Option<String>,
    /// Which lease's branch this came off, which is how a PR is attributed to
    /// the session that actually did the work.
    pub lease_id: Option<i64>,
    pub repo: String,
    pub number: Option<i64>,
    pub url: String,
    pub title: String,
    pub branch: String,
    pub state: State,
    pub source: Source,
    pub detected_at_ms: i64,
    pub reconciled_at_ms: Option<i64>,
}

/// Whether Jod opens a pull request by itself when a session's work looks
/// finished.
///
/// Off by default, and it opens a **draft** through the existing skill. Two
/// separate reasons: opening a PR is externally visible, and a draft is the
/// repo's own convention for one that is not asking to be read yet.
pub const AUTO_PR_SETTING: &str = "auto_pr";
