//! Agent teams — several agents on one job, talking to each other.
//!
//! Every harness is growing a team feature of its own, and each one can only
//! ever contain that harness: OpenCode's teammates are OpenCode sessions,
//! AGY's subagents are AGY's. Jod owns the bus instead, which buys the thing
//! none of them can do alone — **one team whose lead runs on Claude Code and
//! whose teammates run on AGY and OpenCode**, coordinating through the same
//! inbox. Cross-harness teams are only possible for something that sits above
//! all the harnesses, which is the only thing Jod has ever been.
//!
//! The state lives in [`crate::store`] with everything else, so a team survives
//! the process, and the two contended operations — claiming a task and reading
//! an inbox — are single statements rather than read-then-write. That is the
//! same reasoning `claim_task` was already written with; teams reuse it rather
//! than inventing a second answer.
//!
//! Delivery is deliberately dumb: a message becomes a synthetic user turn in
//! the recipient's next prompt ([`Message::as_prompt`]). Because every harness
//! can resume a session by id, that works on all three without any harness
//! knowing teams exist.

use serde::{Deserialize, Serialize};

use crate::harness::HarnessKind;

/// A member's coarse lifecycle.
///
/// Deliberately small: recovery logic reasons about this, and the fine-grained
/// "where in the prompt loop is it" question is already answerable from the
/// agent's event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    /// Idle. A message addressed to it should wake it.
    Ready,
    /// A run is in flight; a message will be picked up on the next turn.
    Busy,
    /// Asked to stop once the current turn ends.
    ShutdownRequested,
    Shutdown,
    Error,
}

impl MemberStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemberStatus::Ready => "ready",
            MemberStatus::Busy => "busy",
            MemberStatus::ShutdownRequested => "shutdown_requested",
            MemberStatus::Shutdown => "shutdown",
            MemberStatus::Error => "error",
        }
    }

    /// Unknown text becomes `Error` rather than failing the read: a row written
    /// by a newer Jod must not make an older one unable to list its team.
    pub fn parse(s: &str) -> MemberStatus {
        match s {
            "ready" => MemberStatus::Ready,
            "busy" => MemberStatus::Busy,
            "shutdown_requested" => MemberStatus::ShutdownRequested,
            "shutdown" => MemberStatus::Shutdown,
            _ => MemberStatus::Error,
        }
    }
}

/// One teammate.
///
/// `agent_id` is the run currently embodying it, and `session_id` is the
/// harness-side conversation to resume. A member outlives any single run,
/// because each turn is a fresh spawn carrying the previous session id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub team: String,
    pub name: String,
    pub harness: HarnessKind,
    pub role: String,
    pub status: MemberStatus,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// A message on the bus, already addressed to exactly one recipient — a
/// broadcast is fanned out on send, so a reader never has to merge two sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub team: String,
    pub from: String,
    pub to: String,
    pub text: String,
    pub at_ms: i64,
}

impl Message {
    /// How a delivered message is handed to the receiving agent: as a synthetic
    /// user turn, because that is the only input channel every harness has.
    ///
    /// The sender is named in the text rather than trusted from anywhere else —
    /// a teammate reading this is being told who claims to have sent it, which
    /// is all Jod can honestly assert.
    pub fn as_prompt(&self) -> String {
        format!("[message from {}]\n{}", self.from, self.text)
    }
}

/// One item on a team's shared board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub owner: Option<String>,
    pub status: String,
}

impl TeamTask {
    pub fn is_done(&self) -> bool {
        self.status == "done"
    }

    pub fn is_claimed(&self) -> bool {
        self.owner.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivered_message_names_its_sender() {
        let m = Message {
            id: 1,
            team: "crew".into(),
            from: "lead".into(),
            to: "scout".into(),
            text: "look at the parser".into(),
            at_ms: 0,
        };
        assert_eq!(m.as_prompt(), "[message from lead]\nlook at the parser");
    }

    #[test]
    fn every_status_survives_a_round_trip_through_text() {
        for status in [
            MemberStatus::Ready,
            MemberStatus::Busy,
            MemberStatus::ShutdownRequested,
            MemberStatus::Shutdown,
            MemberStatus::Error,
        ] {
            assert_eq!(MemberStatus::parse(status.as_str()), status);
        }
    }

    /// A newer Jod may write a status this build has never heard of. Listing
    /// the team must still work.
    #[test]
    fn an_unknown_status_reads_as_error_rather_than_failing() {
        assert_eq!(MemberStatus::parse("from_the_future"), MemberStatus::Error);
        assert_eq!(MemberStatus::parse(""), MemberStatus::Error);
    }

    #[test]
    fn a_task_reports_whether_it_is_claimed_and_done() {
        let mut t = TeamTask {
            id: "t1".into(),
            title: "port the parser".into(),
            owner: None,
            status: "open".into(),
        };
        assert!(!t.is_claimed());
        assert!(!t.is_done());

        t.owner = Some("scout".into());
        t.status = "done".into();
        assert!(t.is_claimed());
        assert!(t.is_done());
    }
}
