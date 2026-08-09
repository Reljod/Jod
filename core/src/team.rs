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

/// What waking one member would take: resume its conversation with the
/// messages it has not seen.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeOrder {
    pub member: String,
    pub harness: HarnessKind,
    /// The conversation to continue, so the member keeps everything it knows.
    pub session_id: String,
    /// The pending messages, already formatted as one turn.
    pub prompt: String,
    /// How many messages this covers, so the caller drains exactly these.
    pub messages: usize,
}

/// Decide whether a member should be woken, and with what.
///
/// Separated from the spawning on purpose: *when to wake* is the part with all
/// the judgement in it, and keeping it pure means it can be tested without a
/// tmux server, a harness binary, or a running agent.
///
/// Returns `None` — deliberately, in each case — when:
///
/// - **There is nothing waiting.** Waking an agent to tell it nothing burns a
///   turn and a context window.
/// - **The member is not idle.** A busy member will read its inbox on its next
///   turn anyway; resuming a conversation that is mid-turn would fork it.
/// - **It is shutting down or has failed.** Waking it would undo the request.
/// - **There is no session to resume.** This is the important one: spawning
///   without a session id would silently start a *fresh* context, so the member
///   would answer having forgotten everything. Staying asleep and visibly
///   holding unread mail is better than answering with amnesia.
pub fn wake_order(member: &Member, pending: &[Message]) -> Option<WakeOrder> {
    if pending.is_empty() || member.status != MemberStatus::Ready {
        return None;
    }
    let session_id = member.session_id.clone()?;
    let prompt = pending
        .iter()
        .map(Message::as_prompt)
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(WakeOrder {
        member: member.name.clone(),
        harness: member.harness,
        session_id,
        prompt,
        messages: pending.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(status: MemberStatus, session: Option<&str>) -> Member {
        Member {
            team: "crew".into(),
            name: "scout".into(),
            harness: HarnessKind::OpenCode,
            role: "research".into(),
            status,
            agent_id: None,
            session_id: session.map(str::to_string),
        }
    }

    fn message(text: &str) -> Message {
        Message {
            id: 1,
            team: "crew".into(),
            from: "lead".into(),
            to: "scout".into(),
            text: text.into(),
            at_ms: 0,
        }
    }

    #[test]
    fn an_idle_member_with_mail_is_woken_on_its_own_conversation() {
        let order = wake_order(
            &member(MemberStatus::Ready, Some("ses-1")),
            &[message("start on the parser")],
        )
        .expect("should wake");

        assert_eq!(order.member, "scout");
        assert_eq!(order.session_id, "ses-1");
        assert_eq!(order.harness, HarnessKind::OpenCode);
        assert_eq!(order.messages, 1);
        assert!(order.prompt.contains("start on the parser"));
        assert!(order.prompt.contains("[message from lead]"));
    }

    #[test]
    fn several_pending_messages_become_one_turn() {
        let order = wake_order(
            &member(MemberStatus::Ready, Some("ses-1")),
            &[message("first"), message("second")],
        )
        .expect("should wake");

        assert_eq!(order.messages, 2);
        assert!(order.prompt.contains("first"));
        assert!(order.prompt.contains("second"));
        assert!(
            order.prompt.find("first") < order.prompt.find("second"),
            "messages keep their order"
        );
    }

    #[test]
    fn an_empty_inbox_wakes_nobody() {
        assert!(wake_order(&member(MemberStatus::Ready, Some("ses-1")), &[]).is_none());
    }

    /// A busy member picks its inbox up on its next turn. Resuming a
    /// conversation that is mid-turn would fork it.
    #[test]
    fn a_busy_member_is_left_alone() {
        assert!(wake_order(
            &member(MemberStatus::Busy, Some("ses-1")),
            &[message("hurry up")]
        )
        .is_none());
    }

    #[test]
    fn a_member_that_is_stopping_or_broken_is_not_restarted() {
        for status in [
            MemberStatus::ShutdownRequested,
            MemberStatus::Shutdown,
            MemberStatus::Error,
        ] {
            assert!(
                wake_order(&member(status, Some("ses-1")), &[message("hello")]).is_none(),
                "{status:?} must not be woken"
            );
        }
    }

    /// The one that matters most: without a session id, spawning would start a
    /// *fresh* context and the member would reply having forgotten everything.
    #[test]
    fn a_member_with_no_session_is_never_woken_into_an_empty_context() {
        assert!(wake_order(&member(MemberStatus::Ready, None), &[message("carry on")]).is_none());
    }

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
