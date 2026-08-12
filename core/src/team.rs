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

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::harness::HarnessKind;
use crate::store::Store;
use crate::works::{DEFAULT_MAX_DEPTH, DEFAULT_MESSAGE_BUDGET};

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
    ///
    /// **The id is not decoration.** It is the only thing a woken agent has to
    /// reply *into this thread* with: waking drains the inbox, so a follow-up
    /// read comes back empty and there is nowhere else to learn it. Without it
    /// the recipient can only send afresh, every answer starts a thread of its
    /// own at depth zero, and the depth bound — the one that stops two polite
    /// agents spending money in a loop — can never be reached. That was
    /// observed live: a question and its answer landing in two threads.
    pub fn as_prompt(&self) -> String {
        format!(
            "[message from {} · message #{}]\n{}",
            self.from, self.id, self.text
        )
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

// ---- threads, scopes and bounds -------------------------------------------

/// Which grouping a `team` column names.
///
/// One bus serves both. A work is an addressing scope whose members are its
/// sessions, joined to nothing; an explicit team is a standing crew that
/// outlives one intent. A second bus would mean a second drain, a second set of
/// tools and two places for a message to be lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Team,
    Work,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Team => "team",
            Scope::Work => "work",
        }
    }

    /// Anything unrecognised reads as `Team`, which is what every row written
    /// before works existed means.
    pub fn parse(s: &str) -> Scope {
        match s {
            "work" => Scope::Work,
            _ => Scope::Team,
        }
    }
}

/// What a message is doing, as opposed to what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    /// Transfers ownership of a task, rather than asking a question. Its own
    /// kind because ownership must not depend on both sides having read the
    /// same prose.
    Handoff,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Handoff => "handoff",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "handoff" => Kind::Handoff,
            _ => Kind::Message,
        }
    }
}

/// Where a message got to.
///
/// Richer than the `delivered` flag it sits beside, and the reason is A8: mail
/// to an agent that cannot receive it has to become visible rather than
/// silent. `Undeliverable` is a message that was never anybody's to read.
///
/// **The same type the card queue uses**, rather than a second enum spelling
/// the same four words. `team_messages.state` and `pending_deliveries.state`
/// are one vocabulary; they were briefly two, which is how a fifth word added
/// to one of them would have quietly meant nothing to the other.
pub use crate::delivery::State as MailState;

impl MailState {
    /// Whether this message counts against the work's budget. An attempt that
    /// was refused before it reached anybody must not also spend the allowance
    /// it was refused by, or hitting a bound once would spend the rest.
    ///
    /// Lives here rather than beside the type: the budget is the bus's
    /// question, and the card queue has no allowance to spend.
    pub fn counts_against_budget(&self) -> bool {
        !matches!(self, MailState::Undeliverable)
    }
}

/// One message and everything the thread around it needs.
///
/// [`Message`] is deliberately left as it was — it is what the delivery path
/// hands a harness, and it should carry no more than the recipient is being
/// told. This is the same row read by something that has to reason about the
/// conversation rather than take part in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(flatten)]
    pub message: Message,
    pub scope: Scope,
    /// Every message caused by one original shares this.
    pub thread_id: String,
    pub in_reply_to: Option<i64>,
    /// Hops from the thread's first message. A fresh question is zero.
    pub depth: i64,
    pub kind: Kind,
    pub state: MailState,
    /// Why it is in that state, when the state alone does not say.
    pub detail: Option<String>,
}

/// The three bounds on a conversation between agents.
///
/// Two agents in a polite loop are a way to spend money at machine speed, and
/// the failure is invisible because every individual message looks reasonable.
/// The defaults are generous enough that ordinary coordination never sees
/// them — see [`crate::works`], where the numbers live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The deepest hop allowed in one thread.
    pub max_depth: i64,
    /// How many messages this scope may exchange in total.
    pub message_budget: i64,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds {
            max_depth: DEFAULT_MAX_DEPTH,
            message_budget: DEFAULT_MESSAGE_BUDGET,
        }
    }
}

/// Which bound stopped a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bound {
    Depth,
    Budget,
}

impl Bound {
    pub fn as_str(&self) -> &'static str {
        match self {
            Bound::Depth => "depth",
            Bound::Budget => "budget",
        }
    }
}

/// Whether a thread may carry another message.
///
/// Derived rather than stored. A paused thread is one whose next hop would
/// cross a bound, and that is a question about the messages already on it — a
/// second column saying the same thing could disagree with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Open,
    /// The hops in this thread have reached the bound. **This thread** is
    /// paused — not the work, and not the sessions, which carry on with
    /// everything else they were doing.
    PausedDepth,
    /// The scope has spent its whole message budget, so every thread in it is
    /// paused until a human says otherwise.
    PausedBudget,
}

impl ThreadState {
    pub fn is_paused(&self) -> bool {
        !matches!(self, ThreadState::Open)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadState::Open => "open",
            ThreadState::PausedDepth => "paused_depth",
            ThreadState::PausedBudget => "paused_budget",
        }
    }
}

/// A message on its way onto the bus.
///
/// Note what is *not* here: nothing that names the sender's authority. `from`
/// is filled in by whatever resolved the caller — for an agent, from its run —
/// and is never an argument the agent supplies.
#[derive(Debug, Clone)]
pub struct Post<'a> {
    pub scope: Scope,
    pub team: &'a str,
    pub from: &'a str,
    /// `None` broadcasts to every other member of the scope.
    pub to: Option<&'a str>,
    pub text: &'a str,
    pub kind: Kind,
    /// The message this answers. What carries a thread — and its depth —
    /// forward.
    pub in_reply_to: Option<i64>,
}

impl<'a> Post<'a> {
    pub fn new(scope: Scope, team: &'a str, from: &'a str, text: &'a str) -> Post<'a> {
        Post {
            scope,
            team,
            from,
            to: None,
            text,
            kind: Kind::Message,
            in_reply_to: None,
        }
    }

    pub fn to(mut self, to: &'a str) -> Post<'a> {
        self.to = Some(to);
        self
    }

    pub fn replying_to(mut self, id: i64) -> Post<'a> {
        self.in_reply_to = Some(id);
        self
    }

    pub fn of_kind(mut self, kind: Kind) -> Post<'a> {
        self.kind = kind;
        self
    }
}

/// What became of a posted message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Sent {
    /// On the bus, waiting to be read.
    Queued {
        /// One id per recipient — a broadcast is fanned out on send.
        ids: Vec<i64>,
        thread_id: String,
        depth: i64,
        recipients: Vec<String>,
    },
    /// A bound stopped it. The attempt is recorded as an undeliverable message
    /// so the traffic log shows what was tried, and the thread is paused until
    /// a human answers.
    Bounded {
        bound: Bound,
        limit: i64,
        reached: i64,
        thread_id: String,
        /// The recorded attempt, so a card can quote it.
        id: i64,
    },
    /// Nobody could receive it. Recorded rather than dropped: mail that
    /// vanishes is worse than mail that fails.
    Undeliverable { detail: String, id: Option<i64> },
}

/// Who is addressable from here, and whether writing to them will reach
/// anybody soon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Addressee {
    pub name: String,
    pub role: String,
    pub harness: HarnessKind,
    pub status: MemberStatus,
    /// Idle, so a message to it will start a turn.
    pub idle: bool,
    /// False when there is no conversation to resume, which is the case where
    /// mail waits rather than being delivered into an amnesiac context.
    pub can_be_woken: bool,
    /// How much mail is already waiting for this member.
    pub waiting: usize,
}

/// A member with mail nobody has read yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub scope: Scope,
    pub team: String,
    pub member: Member,
    pub pending: Vec<Message>,
}

/// Which member a run is, resolved from the run itself.
///
/// The whole point of this type is that no field of it comes from an argument.
/// An agent that could name its own sender could send as anyone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Caller {
    pub run_id: String,
    pub conversation_id: Option<String>,
    pub scope: Scope,
    /// The team name or the work id, according to `scope`.
    pub team: String,
    pub name: String,
}

/// The columns every [`Envelope`] read selects, in the order
/// [`envelope_from_row`] expects them.
///
/// `thread_id` falls back to the message's own id, so a row written before
/// threads existed reads as a thread of one rather than as a broken chain.
const ENVELOPE_COLUMNS: &str = "id, team, sender, recipient, body, at_ms, scope, \
     COALESCE(thread_id, CAST(id AS TEXT)), in_reply_to, depth, kind, state, detail";

fn envelope_from_row(r: &rusqlite::Row) -> rusqlite::Result<Envelope> {
    let scope: String = r.get(6)?;
    let kind: String = r.get(10)?;
    let state: String = r.get(11)?;
    Ok(Envelope {
        message: Message {
            id: r.get(0)?,
            team: r.get(1)?,
            from: r.get(2)?,
            to: r.get(3)?,
            text: r.get(4)?,
            at_ms: r.get(5)?,
        },
        scope: Scope::parse(&scope),
        thread_id: r.get(7)?,
        in_reply_to: r.get(8)?,
        depth: r.get(9)?,
        kind: Kind::parse(&kind),
        state: MailState::parse(&state),
        detail: r.get(12)?,
    })
}

fn member_from_row(r: &rusqlite::Row) -> rusqlite::Result<Member> {
    let harness: String = r.get(2)?;
    let status: String = r.get(4)?;
    Ok(Member {
        team: r.get(0)?,
        name: r.get(1)?,
        harness: HarnessKind::from_id(&harness).unwrap_or(HarnessKind::ClaudeCode),
        role: r.get(3)?,
        status: MemberStatus::parse(&status),
        agent_id: r.get(5)?,
        session_id: r.get(6)?,
    })
}

const MEMBER_COLUMNS: &str = "team, name, harness, role, status, agent_id, session_id";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// How long a member is left alone after being resumed to read its mail.
///
/// One tick. Ten messages arriving together must become one turn carrying ten,
/// not ten turns: that is a cost control — every wake is a model call — and a
/// coherence one, because an agent reading its mail in one batch answers
/// better than one woken per line.
pub const WAKE_INTERVAL_MS: i64 = 60_000;

/// The longest a generated member name may be.
///
/// A name is typed by one agent to address another, and it appears in every
/// line of the traffic log. Past this it stops being an address and starts
/// being a sentence.
pub const MAX_NAME_CHARS: usize = 24;

/// Turn a session's short title into something addressable.
///
/// Lower case, hyphenated, no punctuation an agent would have to quote. A
/// title that yields nothing usable — empty, emoji, another alphabet — becomes
/// `session`, and the caller's uniquifier makes it `session-2` and so on. That
/// is deliberately boring: an unaddressable member is worse than a dull name.
pub fn member_name(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.chars().count() >= MAX_NAME_CHARS {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed
    }
}

impl Store {
    // ---- reading the bus -------------------------------------------------

    /// One message, with its thread.
    pub fn envelope(&self, id: i64) -> Result<Option<Envelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("SELECT {ENVELOPE_COLUMNS} FROM team_messages WHERE id = ?1"),
                params![id],
                envelope_from_row,
            )
            .optional()?)
    }

    /// Every message of one thread, oldest first.
    pub fn mail_thread(&self, thread_id: &str) -> Result<Vec<Envelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENVELOPE_COLUMNS} FROM team_messages
              WHERE COALESCE(thread_id, CAST(id AS TEXT)) = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![thread_id], envelope_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The thread metadata for messages already taken off the bus.
    ///
    /// Its own read rather than a richer drain, because the drain is the one
    /// statement that must stay exactly as it is: it marks delivery, and
    /// splitting that across two statements is how the same instruction ends up
    /// in two turns.
    pub fn envelopes(&self, ids: &[i64]) -> Result<Vec<Envelope>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().expect("store lock poisoned");
        let holes = vec!["?"; ids.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENVELOPE_COLUMNS} FROM team_messages WHERE id IN ({holes}) ORDER BY id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids), envelope_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The first answer to any of these messages, if one has arrived.
    ///
    /// What a bounded wait polls for. Keyed on `in_reply_to` rather than on the
    /// thread, so a peer's unrelated message in the same thread is not mistaken
    /// for the answer to this question.
    pub fn reply_to(&self, ids: &[i64]) -> Result<Option<Envelope>> {
        if ids.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().expect("store lock poisoned");
        let holes = vec!["?"; ids.len()].join(",");
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {ENVELOPE_COLUMNS} FROM team_messages
                      WHERE in_reply_to IN ({holes}) ORDER BY id LIMIT 1"
                ),
                rusqlite::params_from_iter(ids),
                envelope_from_row,
            )
            .optional()?)
    }

    /// Move a task from one owner to another, in one statement.
    ///
    /// Refuses a task somebody else holds, which is what makes a handoff a
    /// transfer rather than a claim: ownership of work is a claim on the board
    /// and never a sentence in a message, so this is the act and the message
    /// beside it is only the telling.
    pub fn hand_over_task(&self, task_id: &str, from: &str, to: &str) -> Result<bool> {
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE tasks SET owner = ?3, claimed_at = ?4
                  WHERE id = ?1 AND (owner = ?2 OR owner IS NULL)",
                params![task_id, from, to, now_ms()],
            )?;
            Ok(changed == 1)
        })
    }

    /// Every message in one scope, oldest first — the traffic log a work's
    /// screen is built from.
    pub fn traffic(&self, scope: Scope, team: &str) -> Result<Vec<Envelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENVELOPE_COLUMNS} FROM team_messages
              WHERE scope = ?1 AND team = ?2 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![scope.as_str(), team], envelope_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- the bounds ------------------------------------------------------

    /// What this scope may spend. A work may carry its own numbers; anything
    /// else gets the defaults, because a bound that only applies to some
    /// conversations is not a bound.
    pub fn bounds_for(&self, scope: Scope, team: &str) -> Result<Bounds> {
        if scope != Scope::Work {
            return Ok(Bounds::default());
        }
        let conn = self.conn.lock().expect("store lock poisoned");
        let row: Option<(Option<i64>, Option<i64>)> = conn
            .query_row(
                "SELECT message_budget, max_depth FROM works WHERE id = ?1",
                params![team],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (budget, depth) = row.unwrap_or((None, None));
        Ok(Bounds {
            max_depth: depth.unwrap_or(DEFAULT_MAX_DEPTH),
            message_budget: budget.unwrap_or(DEFAULT_MESSAGE_BUDGET),
        })
    }

    /// How many messages this scope has actually spent.
    ///
    /// Counted from the messages themselves rather than read off a column, so
    /// the number that decides whether to refuse cannot drift from the traffic
    /// it is counting. `works.messages_used` is kept in step for display.
    pub fn messages_used(&self, scope: Scope, team: &str) -> Result<i64> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM team_messages
              WHERE scope = ?1 AND team = ?2 AND state != 'undeliverable'",
            params![scope.as_str(), team],
            |r| r.get(0),
        )?)
    }

    /// Whether a thread may carry another hop, and if not, which bound says so.
    pub fn thread_state(&self, scope: Scope, team: &str, thread_id: &str) -> Result<ThreadState> {
        let bounds = self.bounds_for(scope, team)?;
        let used = self.messages_used(scope, team)?;
        if used >= bounds.message_budget {
            return Ok(ThreadState::PausedBudget);
        }
        let conn = self.conn.lock().expect("store lock poisoned");
        let deepest: i64 = conn.query_row(
            "SELECT COALESCE(MAX(depth), -1) FROM team_messages
              WHERE COALESCE(thread_id, CAST(id AS TEXT)) = ?1 AND state != 'undeliverable'",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(if deepest >= bounds.max_depth {
            ThreadState::PausedDepth
        } else {
            ThreadState::Open
        })
    }

    // ---- writing to the bus ----------------------------------------------

    /// Put a message on the bus, threaded and bounded.
    ///
    /// One write for the whole decision — reading the parent, counting the
    /// traffic, checking the bounds and inserting — because a bound checked in
    /// one transaction and enforced in another is a bound two agents can race
    /// past together.
    ///
    /// Every ending is recorded. A refused message and a message to nobody both
    /// leave a row, because the human reading the traffic afterwards needs to
    /// see the attempt, and the sender needs an answer rather than silence.
    pub fn post(&self, post: &Post) -> Result<Sent> {
        let bounds = self.bounds_for(post.scope, post.team)?;
        let at = now_ms();
        self.write(|tx| {
            // The thread this message belongs to. A reply inherits its parent's
            // thread and sits one hop deeper; anything else starts a thread of
            // its own.
            let parent: Option<(String, i64)> = match post.in_reply_to {
                Some(id) => tx
                    .query_row(
                        "SELECT COALESCE(thread_id, CAST(id AS TEXT)), depth
                           FROM team_messages WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
                None => None,
            };
            if post.in_reply_to.is_some() && parent.is_none() {
                return Ok(Sent::Undeliverable {
                    detail: format!(
                        "no message #{} to reply to",
                        post.in_reply_to.unwrap_or_default()
                    ),
                    id: None,
                });
            }
            let (thread_id, depth) = match &parent {
                Some((thread, depth)) => (thread.clone(), depth + 1),
                None => (uuid::Uuid::new_v4().to_string(), 0),
            };

            // Who this actually reaches. A broadcast is fanned out here rather
            // than at read time, so an inbox stays one query over one table.
            let members: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT name FROM team_members WHERE scope = ?1 AND team = ?2
                      ORDER BY joined_at_ms, name",
                )?;
                let rows = stmt.query_map(params![post.scope.as_str(), post.team], |r| r.get(0))?;
                rows.collect::<std::result::Result<Vec<String>, _>>()?
            };
            let recipients: Vec<String> = match post.to {
                Some(one) => {
                    if !members.iter().any(|m| m == one) {
                        let id = record(
                            tx,
                            post,
                            one,
                            &thread_id,
                            depth,
                            MailState::Undeliverable,
                            Some(&format!("`{one}` is not a member of this {}", post.scope.as_str())),
                            at,
                        )?;
                        return Ok(Sent::Undeliverable {
                            detail: format!(
                                "`{one}` is not addressable from here — the roster says who is"
                            ),
                            id: Some(id),
                        });
                    }
                    vec![one.to_string()]
                }
                None => members.into_iter().filter(|m| m != post.from).collect(),
            };
            if recipients.is_empty() {
                let id = record(
                    tx,
                    post,
                    "",
                    &thread_id,
                    depth,
                    MailState::Undeliverable,
                    Some("nobody else is in this scope"),
                    at,
                )?;
                return Ok(Sent::Undeliverable {
                    detail: "there is nobody else here to hear it".to_string(),
                    id: Some(id),
                });
            }

            // The bounds, in the order a person would ask about them: how deep
            // has this one argument gone, and how much has the whole work
            // spent.
            if depth > bounds.max_depth {
                let id = record(
                    tx,
                    post,
                    recipients.first().map(String::as_str).unwrap_or(""),
                    &thread_id,
                    depth,
                    MailState::Undeliverable,
                    Some(&format!(
                        "thread paused: {} hops is past the bound of {}",
                        depth, bounds.max_depth
                    )),
                    at,
                )?;
                return Ok(Sent::Bounded {
                    bound: Bound::Depth,
                    limit: bounds.max_depth,
                    reached: depth,
                    thread_id,
                    id,
                });
            }
            let used: i64 = tx.query_row(
                "SELECT COUNT(*) FROM team_messages
                  WHERE scope = ?1 AND team = ?2 AND state != 'undeliverable'",
                params![post.scope.as_str(), post.team],
                |r| r.get(0),
            )?;
            if used + recipients.len() as i64 > bounds.message_budget {
                let id = record(
                    tx,
                    post,
                    recipients.first().map(String::as_str).unwrap_or(""),
                    &thread_id,
                    depth,
                    MailState::Undeliverable,
                    Some(&format!(
                        "budget spent: {used} of {} messages used",
                        bounds.message_budget
                    )),
                    at,
                )?;
                return Ok(Sent::Bounded {
                    bound: Bound::Budget,
                    limit: bounds.message_budget,
                    reached: used,
                    thread_id,
                    id,
                });
            }

            let mut ids = Vec::with_capacity(recipients.len());
            for name in &recipients {
                ids.push(record(
                    tx,
                    post,
                    name,
                    &thread_id,
                    depth,
                    MailState::Queued,
                    None,
                    at,
                )?);
            }
            // Kept in step for anything that shows a work's remaining budget
            // without counting rows. Advisory, never the number a refusal is
            // decided from — see `messages_used`.
            if post.scope == Scope::Work {
                tx.execute(
                    "UPDATE works SET messages_used = messages_used + ?2, updated_at_ms = ?3
                      WHERE id = ?1",
                    params![post.team, recipients.len() as i64, at],
                )?;
            }
            Ok(Sent::Queued {
                ids,
                thread_id,
                depth,
                recipients,
            })
        })
    }

    // ---- who is here -----------------------------------------------------

    /// Add a member to a scope, or update what is known about one.
    ///
    /// The scope-aware sibling of `join_team`, which stays exactly as it was:
    /// everything `jod team` does today keeps doing it, and this is additive.
    /// A work's member carries the conversation it *is*, which is how a run
    /// with no member binding yet still resolves to a sender.
    pub fn join_scope(
        &self,
        scope: Scope,
        team: &str,
        name: &str,
        harness: HarnessKind,
        role: &str,
        conversation_id: Option<&str>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO team_members
                   (team, name, harness, role, status, joined_at_ms, scope, conversation_id)
                 VALUES (?1, ?2, ?3, ?4, 'ready', ?5, ?6, ?7)
                 ON CONFLICT(team, name) DO UPDATE SET
                   harness = ?3,
                   role = ?4,
                   scope = ?6,
                   conversation_id = COALESCE(?7, conversation_id)",
                params![
                    team,
                    name,
                    harness.id(),
                    role,
                    now_ms(),
                    scope.as_str(),
                    conversation_id
                ],
            )?;
            Ok(())
        })
    }

    /// Make a session a member of its work, with no join step.
    ///
    /// A work *is* an addressing scope — asking the sessions the orchestrator
    /// opened for one intent to join the thing they are already part of would
    /// be a tax on every delegation. So this runs when a conversation is
    /// attached to a work, and afterwards its siblings can address it by name.
    ///
    /// The name is assigned once and never changes. A member that renames
    /// itself when its conversation is retitled would be a message delivered to
    /// the wrong agent — or to nobody — halfway through a thread, and that
    /// failure is invisible from both ends.
    pub fn enrol_session(
        &self,
        work_id: &str,
        conversation_id: &str,
        title: &str,
        harness: HarnessKind,
        role: &str,
    ) -> Result<String> {
        if let Some(existing) = self.work_member_name(work_id, conversation_id)? {
            return Ok(existing);
        }
        let taken: Vec<String> = {
            let conn = self.conn.lock().expect("store lock poisoned");
            let mut stmt = conn.prepare("SELECT name FROM team_members WHERE team = ?1")?;
            let rows = stmt.query_map(params![work_id], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let base = member_name(title);
        // Uniqueness within the scope is not a nicety: an ambiguous name is a
        // message delivered to whichever agent the database happened to return
        // first, and the sender is told it was delivered.
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        self.join_scope(
            Scope::Work,
            work_id,
            &name,
            harness,
            role,
            Some(conversation_id),
        )?;
        Ok(name)
    }

    /// What a work's sessions address this conversation as.
    pub fn work_member_name(
        &self,
        work_id: &str,
        conversation_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT name FROM team_members
                  WHERE scope = 'work' AND team = ?1 AND conversation_id = ?2",
                params![work_id, conversation_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// One member of a scope.
    pub fn member_in(&self, scope: Scope, team: &str, name: &str) -> Result<Option<Member>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {MEMBER_COLUMNS} FROM team_members
                      WHERE scope = ?1 AND team = ?2 AND name = ?3"
                ),
                params![scope.as_str(), team, name],
                member_from_row,
            )
            .optional()?)
    }

    /// Everyone in a scope, in the order they joined.
    pub fn members_in(&self, scope: Scope, team: &str) -> Result<Vec<Member>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMBER_COLUMNS} FROM team_members
              WHERE scope = ?1 AND team = ?2 ORDER BY joined_at_ms, name"
        ))?;
        let rows = stmt.query_map(params![scope.as_str(), team], member_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Who an agent in this scope may write to, and what will happen if it
    /// does.
    ///
    /// `waiting` and `can_be_woken` are here because the alternative to
    /// answering them is an agent guessing: a peer holding no session will not
    /// read anything soon, and one already holding nine unread messages does
    /// not need a tenth to say the same thing.
    pub fn roster(&self, scope: Scope, team: &str, asking: &str) -> Result<Vec<Addressee>> {
        let members = self.members_in(scope, team)?;
        let mut out = Vec::with_capacity(members.len());
        for m in members.into_iter().filter(|m| m.name != asking) {
            let waiting = self.team_unread(team, &m.name)?.len();
            out.push(Addressee {
                idle: m.status == MemberStatus::Ready,
                can_be_woken: m.session_id.is_some(),
                name: m.name,
                role: m.role,
                harness: m.harness,
                status: m.status,
                waiting,
            });
        }
        Ok(out)
    }

    /// Which member a run is speaking as.
    ///
    /// Resolved from the run — its binding first, then the conversation it
    /// belongs to — and from nowhere else. This is the whole of sender
    /// identity: an agent that could name its own sender could send as anyone,
    /// so there is deliberately no argument by which it could.
    pub fn caller_for_run(&self, run_id: &str) -> Result<Option<Caller>> {
        let conversation = self.conversation_for_run(run_id)?;
        let conn = self.conn.lock().expect("store lock poisoned");
        let bound: Option<(String, String, String)> = conn
            .query_row(
                "SELECT scope, team, name FROM team_members WHERE agent_id = ?1",
                params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        // A work's members are its sessions, and a session outlives the run
        // that is currently embodying it — so the conversation answers where
        // the run binding has not been written yet.
        let found = match bound {
            Some(found) => Some(found),
            None => match &conversation {
                Some(id) => conn
                    .query_row(
                        "SELECT scope, team, name FROM team_members WHERE conversation_id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?,
                None => None,
            },
        };
        Ok(found.map(|(scope, team, name)| Caller {
            run_id: run_id.to_string(),
            conversation_id: conversation,
            scope: Scope::parse(&scope),
            team,
            name,
        }))
    }

    /// The run a process group belongs to.
    ///
    /// How an MCP server started by a harness knows which run is calling it
    /// without being told: it is a child of the harness, which the supervisor
    /// put in the run's own process group, so the group id *is* the run. A
    /// process cannot join another session's group, which is what makes this
    /// identity rather than a hint.
    ///
    /// Prefers a live run, because a process group id is a small integer the
    /// kernel reuses once the group is gone.
    pub fn run_by_pgid(&self, pgid: u32) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT id FROM runs WHERE pgid = ?1
                  ORDER BY (status = 'running') DESC, created_at_ms DESC LIMIT 1",
                params![pgid as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- waking ----------------------------------------------------------

    /// Every member holding mail nobody has read.
    ///
    /// One query for the addresses, then one member and one inbox each: the
    /// list is short — it is bounded by the number of agents on the box — and
    /// the alternative is a join that has to reproduce the inbox query.
    pub fn mail_waiting(&self) -> Result<Vec<Waiting>> {
        self.waiting(true)
    }

    /// Mail that is waiting and will not be delivered, because the work it was
    /// addressed into is over.
    ///
    /// Reported rather than delivered, and reported rather than deleted. A
    /// work's bus ends with the work: injecting a message into a session that
    /// is finishing would restart an agent that has just been allowed to stop,
    /// and dropping it silently would lose the last thing somebody said.
    pub fn mail_held(&self) -> Result<Vec<Waiting>> {
        self.waiting(false)
    }

    fn waiting(&self, deliverable: bool) -> Result<Vec<Waiting>> {
        Ok(self
            .all_waiting()?
            .into_iter()
            .filter(|w| self.scope_accepts_delivery(w.scope, &w.team).unwrap_or(true) == deliverable)
            .collect())
    }

    /// Whether mail into this scope still reaches anybody.
    ///
    /// An explicit team always does; a work does only while it is open.
    /// Closing is where this bites: the board is empty, the sessions are
    /// stopping, and waking one of them to read a message is the one thing
    /// that would give the work something new to do.
    fn scope_accepts_delivery(&self, scope: Scope, team: &str) -> Result<bool> {
        if scope != Scope::Work {
            return Ok(true);
        }
        let conn = self.conn.lock().expect("store lock poisoned");
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM works WHERE id = ?1",
                params![team],
                |r| r.get(0),
            )
            .optional()?;
        // A work that is gone accepts nothing, and mail addressed into one is
        // mail its own deletion should already have taken.
        Ok(state.as_deref() == Some("open"))
    }

    fn all_waiting(&self) -> Result<Vec<Waiting>> {
        let addresses: Vec<(String, String, String)> = {
            let conn = self.conn.lock().expect("store lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT DISTINCT scope, team, recipient FROM team_messages
                  WHERE delivered = 0 AND state = 'queued' ORDER BY team, recipient",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::new();
        for (scope, team, name) in addresses {
            let scope = Scope::parse(&scope);
            // Mail addressed to a name that is no longer a member is left
            // alone here rather than deleted. It is visible in the traffic log,
            // and deciding what to do about it is a person's call.
            let Some(member) = self.member_in(scope, &team, &name)? else {
                continue;
            };
            let pending = self.team_unread(&team, &name)?;
            if pending.is_empty() {
                continue;
            }
            out.push(Waiting {
                scope,
                team,
                member,
                pending,
            });
        }
        Ok(out)
    }

    /// Take the right to wake a member, at most once per interval.
    ///
    /// One statement, so two ticks racing produce one wake. Read-then-write
    /// here would be the same bug the inbox drain exists to avoid, spending a
    /// model call rather than repeating an instruction.
    ///
    /// A `last_woken_at_ms` in the future — a clock that went backwards, a VM
    /// restored from a snapshot — is treated as due rather than locking the
    /// member out until the clock catches up.
    pub fn claim_wake(
        &self,
        scope: Scope,
        team: &str,
        name: &str,
        now_ms: i64,
        interval_ms: i64,
    ) -> Result<bool> {
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE team_members SET last_woken_at_ms = ?4
                  WHERE scope = ?1 AND team = ?2 AND name = ?3
                    AND (last_woken_at_ms IS NULL
                         OR ?4 - last_woken_at_ms >= ?5
                         OR last_woken_at_ms > ?4)",
                params![scope.as_str(), team, name, now_ms, interval_ms],
            )?;
            Ok(changed == 1)
        })
    }

    /// Say, on the mail itself, why it is still sitting there.
    ///
    /// Only where nothing has been said yet, so a tick that finds the same
    /// stuck mail every minute does not rewrite it every minute. Returns how
    /// many messages this newly explained, which is what stops the caller
    /// logging the same silence for ever.
    pub fn note_mail_stuck(
        &self,
        scope: Scope,
        team: &str,
        name: &str,
        detail: &str,
    ) -> Result<usize> {
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE team_messages SET detail = ?4
                  WHERE scope = ?1 AND team = ?2 AND recipient = ?3
                    AND delivered = 0 AND detail IS NULL",
                params![scope.as_str(), team, name, detail],
            )?;
            Ok(changed)
        })
    }

    /// Take every waiting message for a member, and record that it went.
    ///
    /// **The only sanctioned way to take mail off the bus.** Both facts — the
    /// `delivered` flag that stops a message being injected into a second turn,
    /// and the `state` a traffic view reads — are written by one statement in
    /// one transaction, so there is no order of events in which a message is
    /// handed to an agent and still reports as waiting.
    ///
    /// It exists because the two-call version did not survive contact with its
    /// callers. `drain_inbox` sets the flag; [`Store::mark_mail_delivered`]
    /// sets the state beside it; and remembering the second one is a convention
    /// that was already half-followed — `jod team wake` and `jod team inbox`
    /// drain without it, so mail those paths delivered reported `queued` for
    /// ever afterwards. A message the system says it did not deliver when it
    /// did is worse than a duplicated line of SQL, and a wrapper that cannot be
    /// half-used beats a convention that can.
    ///
    /// The single-transaction drain underneath is unchanged and is still the
    /// reason the same instruction is never injected into two turns.
    pub fn take_mail(&self, team: &str, member: &str) -> Result<Vec<Message>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, team, sender, recipient, body, at_ms FROM team_messages
                  WHERE team = ?1 AND recipient = ?2 AND delivered = 0 ORDER BY id",
            )?;
            let taken: Vec<Message> = stmt
                .query_map(params![team, member], |r| {
                    Ok(Message {
                        id: r.get(0)?,
                        team: r.get(1)?,
                        from: r.get(2)?,
                        to: r.get(3)?,
                        text: r.get(4)?,
                        at_ms: r.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            // The same predicate the read used, inside the same transaction —
            // not the ids it returned. A message that arrived between the two
            // would otherwise be marked without being handed over.
            tx.execute(
                "UPDATE team_messages SET delivered = 1, state = ?3
                  WHERE team = ?1 AND recipient = ?2 AND delivered = 0",
                params![team, member, MailState::Delivered.as_str()],
            )?;
            Ok(taken)
        })
    }

    /// Mark drained messages as delivered in the richer vocabulary.
    ///
    /// The repair half of the old two-call sequence, kept for a caller that
    /// already drained — [`Store::take_mail`] is what new code wants, because
    /// it cannot be called without the drain it belongs to.
    pub fn mark_mail_delivered(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|tx| {
            for id in ids {
                tx.execute(
                    "UPDATE team_messages SET state = 'delivered', delivered = 1 WHERE id = ?1",
                    params![id],
                )?;
            }
            Ok(())
        })
    }
}

/// Insert one message row. Every ending of [`Store::post`] goes through here,
/// so a refused or undeliverable attempt is written with exactly the same shape
/// as a delivered one and shows up in the same log.
///
/// Anything that is not `Queued` is written already `delivered`, because
/// nothing should ever hand it to an agent.
#[allow(clippy::too_many_arguments)]
fn record(
    tx: &rusqlite::Transaction,
    post: &Post,
    to: &str,
    thread_id: &str,
    depth: i64,
    state: MailState,
    detail: Option<&str>,
    at_ms: i64,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO team_messages
           (team, sender, recipient, body, at_ms, delivered,
            scope, thread_id, in_reply_to, depth, kind, state, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            post.team,
            post.from,
            to,
            post.text,
            at_ms,
            i64::from(state != MailState::Queued),
            post.scope.as_str(),
            thread_id,
            post.in_reply_to,
            depth,
            post.kind.as_str(),
            state.as_str(),
            detail,
        ],
    )?;
    Ok(tx.last_insert_rowid())
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
        assert!(order.prompt.contains("[message from lead · message #1]"));
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
    fn a_delivered_message_names_its_sender_and_carries_the_id_to_reply_to() {
        let m = Message {
            id: 42,
            team: "crew".into(),
            from: "lead".into(),
            to: "scout".into(),
            text: "look at the parser".into(),
            at_ms: 0,
        };
        assert_eq!(
            m.as_prompt(),
            "[message from lead · message #42]\nlook at the parser"
        );
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

    // ---- threads, bounds and waking --------------------------------------

    use crate::conversation::NewMessage;
    use crate::store::Store;

    /// A team of two, both idle and both resumable.
    fn crew() -> Store {
        let s = Store::in_memory().unwrap();
        for (name, role) in [("lead", "planning"), ("scout", "research")] {
            s.join_scope(Scope::Team, "crew", name, HarnessKind::ClaudeCode, role, None)
                .unwrap();
            s.bind_member("crew", name, Some(&format!("run-{name}")), Some("ses-1"))
                .unwrap();
        }
        s
    }

    fn queued(sent: &Sent) -> (Vec<i64>, String, i64) {
        match sent {
            Sent::Queued {
                ids,
                thread_id,
                depth,
                ..
            } => (ids.clone(), thread_id.clone(), *depth),
            other => panic!("expected a queued message, got {other:?}"),
        }
    }

    #[test]
    fn a_reply_carries_its_thread_forward_so_the_exchange_reads_as_one() {
        let s = crew();
        let (first, thread, depth) = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "where is the parser?").to("scout"))
                .unwrap(),
        );
        assert_eq!(depth, 0, "a fresh question starts a thread");

        let (_, replied_thread, reply_depth) = queued(
            &s.post(
                &Post::new(Scope::Team, "crew", "scout", "in core")
                    .to("lead")
                    .replying_to(first[0]),
            )
            .unwrap(),
        );
        assert_eq!(replied_thread, thread, "a reply must join the thread it answers");
        assert_eq!(reply_depth, 1);
        assert_eq!(s.mail_thread(&thread).unwrap().len(), 2);
    }

    #[test]
    fn a_reply_to_a_reply_is_two_hops_deep() {
        let s = crew();
        let (first, _, _) = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "one").to("scout"))
                .unwrap(),
        );
        let (second, _, _) = queued(
            &s.post(
                &Post::new(Scope::Team, "crew", "scout", "two")
                    .to("lead")
                    .replying_to(first[0]),
            )
            .unwrap(),
        );
        let (_, _, depth) = queued(
            &s.post(
                &Post::new(Scope::Team, "crew", "lead", "three")
                    .to("scout")
                    .replying_to(second[0]),
            )
            .unwrap(),
        );
        assert_eq!(depth, 2);
    }

    /// The one this whole group exists for. Two agents told to keep replying
    /// stop on their own, without anybody watching and without the work or
    /// either session being killed.
    #[test]
    fn two_agents_conversing_without_end_stop_at_the_bound() {
        let s = crew();
        let bounds = s.bounds_for(Scope::Team, "crew").unwrap();

        let mut last = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "hop 0").to("scout"))
                .unwrap(),
        )
        .0[0];
        let mut hops = 0i64;
        // Deliberately pathological: each side answers the other for ever.
        for hop in 1..(bounds.max_depth + 20) {
            let (from, to) = if hop % 2 == 1 {
                ("scout", "lead")
            } else {
                ("lead", "scout")
            };
            let sent = s
                .post(
                    &Post::new(Scope::Team, "crew", from, &format!("hop {hop}"))
                        .to(to)
                        .replying_to(last),
                )
                .unwrap();
            match sent {
                Sent::Queued { ids, .. } => {
                    last = ids[0];
                    hops = hop;
                }
                Sent::Bounded { bound, limit, reached, .. } => {
                    assert_eq!(bound, Bound::Depth);
                    assert_eq!(limit, bounds.max_depth);
                    assert_eq!(reached, bounds.max_depth + 1);
                    assert_eq!(hops, bounds.max_depth, "it stopped at the bound, not before it");
                    return;
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
        }
        panic!("the conversation never stopped — it ran {hops} hops");
    }

    #[test]
    fn a_thread_at_its_depth_bound_is_paused_and_a_fresh_question_is_not() {
        let s = crew();
        let bounds = s.bounds_for(Scope::Team, "crew").unwrap();
        let (first, thread, _) = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "start").to("scout"))
                .unwrap(),
        );
        let mut last = first[0];
        for hop in 1..=bounds.max_depth {
            last = queued(
                &s.post(
                    &Post::new(Scope::Team, "crew", "lead", &format!("hop {hop}"))
                        .to("scout")
                        .replying_to(last),
                )
                .unwrap(),
            )
            .0[0];
        }
        assert_eq!(
            s.thread_state(Scope::Team, "crew", &thread).unwrap(),
            ThreadState::PausedDepth
        );

        // The pause is the thread's, not the team's: anybody may still start a
        // new subject, which is why a bound is not a kill switch.
        let (_, other, _) = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "different subject").to("scout"))
                .unwrap(),
        );
        assert_eq!(
            s.thread_state(Scope::Team, "crew", &other).unwrap(),
            ThreadState::Open
        );
    }

    #[test]
    fn a_refused_message_is_recorded_but_never_delivered_and_never_spends_budget() {
        let s = crew();
        let bounds = s.bounds_for(Scope::Team, "crew").unwrap();
        let mut last = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "start").to("scout"))
                .unwrap(),
        )
        .0[0];
        for hop in 1..=bounds.max_depth {
            last = queued(
                &s.post(
                    &Post::new(Scope::Team, "crew", "lead", &format!("hop {hop}"))
                        .to("scout")
                        .replying_to(last),
                )
                .unwrap(),
            )
            .0[0];
        }
        let spent_before = s.messages_used(Scope::Team, "crew").unwrap();
        let refused = s
            .post(
                &Post::new(Scope::Team, "crew", "scout", "one more")
                    .to("lead")
                    .replying_to(last),
            )
            .unwrap();
        let Sent::Bounded { id, .. } = refused else {
            panic!("the bound did not stop it: {refused:?}");
        };
        assert_eq!(
            s.messages_used(Scope::Team, "crew").unwrap(),
            spent_before,
            "a refusal must not also spend the allowance it was refused by"
        );
        let recorded = s.envelope(id).unwrap().expect("the attempt is on the record");
        assert_eq!(recorded.state, MailState::Undeliverable);
        assert!(recorded.detail.unwrap().contains("bound"));
        assert!(
            !s.team_unread("crew", "lead")
                .unwrap()
                .iter()
                .any(|m| m.text == "one more"),
            "a refused message reached an inbox"
        );
    }

    #[test]
    fn spending_the_whole_budget_pauses_the_scope() {
        let s = Store::in_memory().unwrap();
        // A work with a tiny budget, so the bound is reached in a test rather
        // than in two hundred messages.
        s.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO works (id, title, summary, instruction, colour, state,
                                    message_budget, messages_used, max_depth,
                                    created_at_ms, updated_at_ms)
                 VALUES ('w1', 't', 's', 'i', 'red', 'open', 2, 0, 20, 0, 0)",
                [],
            )
            .unwrap();
        for name in ["builder", "reviewer"] {
            s.join_scope(Scope::Work, "w1", name, HarnessKind::ClaudeCode, "", None)
                .unwrap();
        }
        let bounds = s.bounds_for(Scope::Work, "w1").unwrap();
        assert_eq!(bounds.message_budget, 2, "a work's own numbers win");

        for i in 0..2 {
            let sent = s
                .post(&Post::new(Scope::Work, "w1", "builder", &format!("{i}")).to("reviewer"))
                .unwrap();
            assert!(matches!(sent, Sent::Queued { .. }), "{sent:?}");
        }
        let sent = s
            .post(&Post::new(Scope::Work, "w1", "builder", "one too many").to("reviewer"))
            .unwrap();
        let Sent::Bounded { bound, limit, .. } = sent else {
            panic!("the budget did not stop it: {sent:?}");
        };
        assert_eq!(bound, Bound::Budget);
        assert_eq!(limit, 2);
        assert_eq!(
            s.thread_state(Scope::Work, "w1", "anything").unwrap(),
            ThreadState::PausedBudget
        );
    }

    /// So that a work's row can show what is left without counting rows —
    /// the card must never be the first time anybody hears about the budget.
    #[test]
    fn a_works_own_counter_keeps_step_with_the_traffic() {
        let s = Store::in_memory().unwrap();
        s.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO works (id, title, summary, instruction, colour, state,
                                    created_at_ms, updated_at_ms)
                 VALUES ('w1', 't', 's', 'i', 'red', 'open', 0, 0)",
                [],
            )
            .unwrap();
        for name in ["builder", "reviewer"] {
            s.join_scope(Scope::Work, "w1", name, HarnessKind::ClaudeCode, "", None)
                .unwrap();
        }
        s.post(&Post::new(Scope::Work, "w1", "builder", "hello").to("reviewer"))
            .unwrap();
        let used: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT messages_used FROM works WHERE id = 'w1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(used, 1);
        assert_eq!(used, s.messages_used(Scope::Work, "w1").unwrap());
    }

    /// Per A8: mail that vanishes is worse than mail that fails.
    #[test]
    fn a_message_to_somebody_who_is_not_here_is_recorded_rather_than_lost() {
        let s = crew();
        let sent = s
            .post(&Post::new(Scope::Team, "crew", "lead", "hello?").to("nobody"))
            .unwrap();
        let Sent::Undeliverable { detail, id } = sent else {
            panic!("a message to a stranger was accepted: {sent:?}");
        };
        assert!(detail.contains("nobody"), "{detail}");
        let recorded = s.envelope(id.unwrap()).unwrap().unwrap();
        assert_eq!(recorded.state, MailState::Undeliverable);
        assert!(s.team_unread("crew", "nobody").unwrap().is_empty());
    }

    #[test]
    fn a_broadcast_reaches_everyone_but_the_sender_under_one_thread() {
        let s = crew();
        s.join_scope(
            Scope::Team,
            "crew",
            "builder",
            HarnessKind::OpenCode,
            "code",
            None,
        )
        .unwrap();
        let (ids, thread, _) = queued(
            &s.post(&Post::new(Scope::Team, "crew", "lead", "standup in five"))
                .unwrap(),
        );
        assert_eq!(ids.len(), 2, "the sender does not get its own broadcast");
        let threads: Vec<String> = s
            .envelopes(&ids)
            .unwrap()
            .into_iter()
            .map(|e| e.thread_id)
            .collect();
        assert_eq!(threads, vec![thread.clone(), thread]);
        assert!(s.team_unread("crew", "lead").unwrap().is_empty());
        assert_eq!(s.team_unread("crew", "scout").unwrap().len(), 1);
    }

    #[test]
    fn a_handoff_is_a_named_move_rather_than_a_convention() {
        let s = crew();
        let (ids, _, _) = queued(
            &s.post(
                &Post::new(Scope::Team, "crew", "lead", "the parser is yours now")
                    .to("scout")
                    .of_kind(Kind::Handoff),
            )
            .unwrap(),
        );
        assert_eq!(s.envelope(ids[0]).unwrap().unwrap().kind, Kind::Handoff);
    }

    #[test]
    fn a_reply_to_a_message_that_never_existed_is_refused_rather_than_rethreaded() {
        let s = crew();
        let sent = s
            .post(
                &Post::new(Scope::Team, "crew", "lead", "as I was saying")
                    .to("scout")
                    .replying_to(4242),
            )
            .unwrap();
        assert!(matches!(sent, Sent::Undeliverable { .. }), "{sent:?}");
    }

    /// The rate limit, which is the whole of G2.S3: ten messages arriving
    /// together must produce one resumed turn carrying ten, not ten turns.
    #[test]
    fn a_member_is_resumed_at_most_once_per_interval() {
        let s = crew();
        for i in 0..10 {
            s.post(&Post::new(Scope::Team, "crew", "lead", &format!("{i}")).to("scout"))
                .unwrap();
        }
        let now = 1_000_000;
        assert!(s
            .claim_wake(Scope::Team, "crew", "scout", now, WAKE_INTERVAL_MS)
            .unwrap());
        assert!(
            !s.claim_wake(Scope::Team, "crew", "scout", now + 1, WAKE_INTERVAL_MS)
                .unwrap(),
            "a second wake inside the interval would be a second model call for the same mail"
        );
        assert!(s
            .claim_wake(
                Scope::Team,
                "crew",
                "scout",
                now + WAKE_INTERVAL_MS,
                WAKE_INTERVAL_MS
            )
            .unwrap());
    }

    /// A VM restored from a snapshot must not lock a member out of its mail
    /// until the clock catches up.
    #[test]
    fn a_clock_that_went_backwards_does_not_hold_a_member_asleep() {
        let s = crew();
        assert!(s
            .claim_wake(Scope::Team, "crew", "scout", 5_000_000, WAKE_INTERVAL_MS)
            .unwrap());
        assert!(s
            .claim_wake(Scope::Team, "crew", "scout", 1_000, WAKE_INTERVAL_MS)
            .unwrap());
    }

    #[test]
    fn every_member_holding_unread_mail_is_listed_with_it() {
        let s = crew();
        s.post(&Post::new(Scope::Team, "crew", "lead", "look at this").to("scout"))
            .unwrap();
        let waiting = s.mail_waiting().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].member.name, "scout");
        assert_eq!(waiting[0].scope, Scope::Team);
        assert_eq!(waiting[0].pending.len(), 1);

        s.drain_inbox("crew", "scout").unwrap();
        assert!(s.mail_waiting().unwrap().is_empty(), "drained mail is not waiting");
    }

    /// G2.S4: mail for a member with no session waits *visibly*. Delivering it
    /// into a fresh context would be answering with amnesia.
    #[test]
    fn mail_for_a_member_with_no_session_says_why_it_is_still_sitting_there() {
        let s = Store::in_memory().unwrap();
        for name in ["lead", "scout"] {
            s.join_scope(Scope::Team, "crew", name, HarnessKind::ClaudeCode, "", None)
                .unwrap();
        }
        s.post(&Post::new(Scope::Team, "crew", "lead", "hello").to("scout"))
            .unwrap();
        let waiting = s.mail_waiting().unwrap();
        assert!(wake_order(&waiting[0].member, &waiting[0].pending).is_none());

        let explained = s
            .note_mail_stuck(Scope::Team, "crew", "scout", "no session to resume")
            .unwrap();
        assert_eq!(explained, 1);
        assert_eq!(
            s.envelope(waiting[0].pending[0].id)
                .unwrap()
                .unwrap()
                .detail
                .as_deref(),
            Some("no session to resume")
        );
        // Said once, not once a minute for as long as it sits there.
        assert_eq!(
            s.note_mail_stuck(Scope::Team, "crew", "scout", "no session to resume")
                .unwrap(),
            0
        );
    }

    #[test]
    fn a_caller_is_resolved_from_the_run_that_is_asking() {
        let s = crew();
        let caller = s
            .caller_for_run("run-scout")
            .unwrap()
            .expect("the run is bound to a member");
        assert_eq!(caller.name, "scout");
        assert_eq!(caller.team, "crew");
        assert_eq!(caller.scope, Scope::Team);
        assert!(
            s.caller_for_run("some-other-run").unwrap().is_none(),
            "a run that is nobody's member must not resolve to a sender"
        );
    }

    /// A work's member is a session, so the conversation answers even before a
    /// run has been bound to it.
    #[test]
    fn a_work_member_is_resolved_through_its_conversation() {
        let s = Store::in_memory().unwrap();
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.append_message(&c.id, NewMessage::user("go").from_run("run-7"))
            .unwrap();
        s.join_scope(
            Scope::Work,
            "w1",
            "builder",
            HarnessKind::ClaudeCode,
            "",
            Some(&c.id),
        )
        .unwrap();

        let caller = s.caller_for_run("run-7").unwrap().expect("resolved");
        assert_eq!(caller.name, "builder");
        assert_eq!(caller.scope, Scope::Work);
        assert_eq!(caller.team, "w1");
        assert_eq!(caller.conversation_id.as_deref(), Some(c.id.as_str()));
    }

    #[test]
    fn the_roster_says_who_is_addressable_and_what_writing_to_them_would_do() {
        let s = crew();
        s.join_scope(
            Scope::Team,
            "crew",
            "builder",
            HarnessKind::OpenCode,
            "code",
            None,
        )
        .unwrap();
        s.set_member_status("crew", "builder", MemberStatus::Busy)
            .unwrap();
        s.post(&Post::new(Scope::Team, "crew", "lead", "waiting for you").to("scout"))
            .unwrap();

        let roster = s.roster(Scope::Team, "crew", "lead").unwrap();
        assert!(
            !roster.iter().any(|a| a.name == "lead"),
            "you are not addressable from your own seat"
        );
        let scout = roster.iter().find(|a| a.name == "scout").unwrap();
        assert!(scout.idle);
        assert!(scout.can_be_woken);
        assert_eq!(scout.waiting, 1);
        let builder = roster.iter().find(|a| a.name == "builder").unwrap();
        assert!(!builder.idle);
        assert!(
            !builder.can_be_woken,
            "a member with no session cannot be woken, and the roster must say so"
        );
        assert_eq!(builder.harness, HarnessKind::OpenCode);
    }

    /// One bus, two scopes: a team and a work may share a name without sharing
    /// a member list.
    #[test]
    fn a_work_and_a_team_are_separate_address_books() {
        let s = Store::in_memory().unwrap();
        s.join_scope(Scope::Team, "alpha", "lead", HarnessKind::ClaudeCode, "", None)
            .unwrap();
        assert_eq!(s.members_in(Scope::Team, "alpha").unwrap().len(), 1);
        assert!(s.members_in(Scope::Work, "alpha").unwrap().is_empty());
    }

    #[test]
    fn a_run_is_identified_by_the_process_group_the_supervisor_recorded() {
        let s = Store::in_memory().unwrap();
        s.save_run(&crate::store::StoredRun {
            id: "run-1".into(),
            name: "builder".into(),
            harness: "claude_code".into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            session_id: None,
            pid: None,
            pgid: None,
            created_at_ms: 1,
            summary: serde_json::Value::Null,
        })
        .unwrap();
        s.set_run_process("run-1", 4242, 4242).unwrap();
        assert_eq!(s.run_by_pgid(4242).unwrap().as_deref(), Some("run-1"));
        assert_eq!(s.run_by_pgid(9999).unwrap(), None);
    }

    #[test]
    fn a_teams_bounds_are_the_defaults_and_a_work_may_carry_its_own() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.bounds_for(Scope::Team, "crew").unwrap(), Bounds::default());
        // A work nobody has given numbers to gets the same defaults, so a bound
        // is never absent — only ever different.
        assert_eq!(s.bounds_for(Scope::Work, "w1").unwrap(), Bounds::default());
    }

    #[test]
    fn every_scope_kind_state_and_thread_state_survives_a_round_trip_through_text() {
        for scope in [Scope::Team, Scope::Work] {
            assert_eq!(Scope::parse(scope.as_str()), scope);
        }
        for kind in [Kind::Message, Kind::Handoff] {
            assert_eq!(Kind::parse(kind.as_str()), kind);
        }
        for state in [
            MailState::Queued,
            MailState::Delivered,
            MailState::Failed,
            MailState::Undeliverable,
        ] {
            assert_eq!(MailState::parse(state.as_str()), state);
        }
        assert_eq!(Scope::parse("from_the_future"), Scope::Team);
        assert_eq!(MailState::parse(""), MailState::Queued);
    }

    /// The bug this exists for was found by the live cross-harness run, not by
    /// a unit test: a reply that a real agent had already read still reported
    /// `queued`, because the path that delivered it drained the bus and forgot
    /// the second call that writes the state beside it.
    ///
    /// A message the system says it did not deliver, when it did, is worse than
    /// a duplicated line of SQL.
    #[test]
    fn taking_mail_records_both_facts_at_once() {
        let s = crew();
        s.post(&Post::new(Scope::Team, "crew", "lead", "start on the parser").to("scout"))
            .unwrap();

        let taken = s.take_mail("crew", "scout").unwrap();

        assert_eq!(taken.len(), 1);
        let after = &s.traffic(Scope::Team, "crew").unwrap()[0];
        assert_eq!(
            after.state,
            MailState::Delivered,
            "the state a traffic view reads has to move with the flag that stops \
             re-injection, or the bus reports mail it has already handed over as waiting"
        );
        assert!(
            s.team_unread("crew", "scout").unwrap().is_empty(),
            "and the flag moved too"
        );
    }

    /// The property the single transaction is for: two turns of the same agent
    /// must not both pick up one instruction.
    #[test]
    fn taking_mail_twice_takes_it_once() {
        let s = crew();
        s.post(&Post::new(Scope::Team, "crew", "lead", "only once").to("scout"))
            .unwrap();

        assert_eq!(s.take_mail("crew", "scout").unwrap().len(), 1);
        assert!(s.take_mail("crew", "scout").unwrap().is_empty());
    }

    /// The invariant, stated so a future path that drains without marking is a
    /// failing test rather than a wrong answer in a transcript weeks later.
    #[test]
    fn no_message_is_ever_handed_over_while_still_reporting_as_waiting() {
        let s = crew();
        for text in ["one", "two"] {
            s.post(&Post::new(Scope::Team, "crew", "lead", text).to("scout"))
                .unwrap();
        }
        s.take_mail("crew", "scout").unwrap();

        let stranded: i64 = {
            let conn = s.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM team_messages WHERE delivered = 1 AND state = 'queued'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            stranded, 0,
            "`delivered = 1` with `state = 'queued'` is the shape of the bug: handed to \
             an agent, and still reported as waiting"
        );
    }

    /// The two queues store the same four words in two tables. They were two
    /// enums that happened to agree; this asserts they are now one, so that a
    /// fifth word cannot be added to the bus and mean nothing to the card
    /// queue.
    #[test]
    fn the_bus_and_the_card_queue_speak_one_delivery_vocabulary() {
        assert_eq!(
            std::any::TypeId::of::<MailState>(),
            std::any::TypeId::of::<crate::delivery::State>(),
            "`MailState` is `delivery::State`, not a copy of it"
        );
        for word in ["queued", "delivered", "failed", "undeliverable"] {
            let from_the_bus = MailState::parse(word);
            let from_the_queue = crate::delivery::State::parse(word);
            assert_eq!(from_the_bus, from_the_queue);
            assert_eq!(from_the_bus.as_str(), word, "and it round-trips");
        }
    }

    /// The one thing the two queues do *not* share, and the reason it is a
    /// method here rather than beside the type: the card queue has no
    /// allowance to spend.
    #[test]
    fn a_refused_message_does_not_also_spend_the_budget_it_was_refused_by() {
        assert!(!MailState::Undeliverable.counts_against_budget());
        for state in [MailState::Queued, MailState::Delivered, MailState::Failed] {
            assert!(state.counts_against_budget());
        }
    }

    // ---- a work is a team (G3) -------------------------------------------

    /// A work, two sessions in it, and nobody joined anything.
    fn work_of_two(s: &Store) -> (String, String, String) {
        let work = s.create_work("port the parser").unwrap();
        let mut ids = Vec::new();
        for title in ["the lead", "the worker"] {
            let c = s
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            s.set_conversation_title(&c.id, title).unwrap();
            s.attach_conversation(&c.id, &work.id, None, crate::works::Origin::Agent)
                .unwrap();
            s.bind_member(&work.id, &member_name(title), Some(&format!("run-{title}")), Some("ses-1"))
                .unwrap();
            ids.push(c.id);
        }
        (work.id, ids.remove(0), ids.remove(0))
    }

    #[test]
    fn a_title_becomes_something_an_agent_can_address() {
        assert_eq!(member_name("The Parser!"), "the-parser");
        assert_eq!(member_name("  "), "session");
        assert_eq!(member_name("порт"), "session");
        assert!(member_name(&"long ".repeat(40)).chars().count() <= MAX_NAME_CHARS);
    }

    /// G3's check: two sessions the orchestrator opened for one work message
    /// each other by name, having never been joined to a team.
    #[test]
    fn two_sessions_of_one_work_message_each_other_with_no_join_step() {
        let s = Store::in_memory().unwrap();
        let (work, _lead, _worker) = work_of_two(&s);

        let roster = s.roster(Scope::Work, &work, "the-lead").unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].name, "the-worker");

        let sent = s
            .post(&Post::new(Scope::Work, &work, "the-lead", "take the lexer").to("the-worker"))
            .unwrap();
        assert!(matches!(sent, Sent::Queued { .. }), "got {sent:?}");
        let waiting = s.mail_waiting().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].scope, Scope::Work);
        assert_eq!(waiting[0].member.name, "the-worker");
    }

    /// An ambiguous name is a message delivered to whichever agent the database
    /// happened to return first, and the sender is told it arrived.
    #[test]
    fn two_sessions_with_the_same_title_get_different_names() {
        let s = Store::in_memory().unwrap();
        let work = s.create_work("a job").unwrap();
        let mut names = Vec::new();
        for _ in 0..3 {
            let c = s
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap();
            s.set_conversation_title(&c.id, "worker").unwrap();
            names.push(
                s.attach_conversation(&c.id, &work.id, None, crate::works::Origin::Agent)
                    .unwrap()
                    .name,
            );
        }
        assert_eq!(names, ["worker", "worker-2", "worker-3"]);
    }

    /// A name that moves is a message delivered to nobody halfway through a
    /// thread, and the failure is invisible from both ends.
    #[test]
    fn a_members_name_does_not_change_when_its_session_is_retitled() {
        let s = Store::in_memory().unwrap();
        let work = s.create_work("a job").unwrap();
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.set_conversation_title(&c.id, "first name").unwrap();
        s.attach_conversation(&c.id, &work.id, None, crate::works::Origin::Agent)
            .unwrap();

        s.set_conversation_title(&c.id, "something else").unwrap();
        let again = s
            .attach_conversation(&c.id, &work.id, None, crate::works::Origin::Agent)
            .unwrap();
        assert_eq!(again.name, "first-name");
        assert_eq!(s.members_in(Scope::Work, &work.id).unwrap().len(), 1);
    }

    /// G3.S6. Waking a session that has just been allowed to stop is the one
    /// thing that would give a closed work something new to do.
    #[test]
    fn closing_a_work_stops_delivery_into_it_and_reports_the_mail_instead() {
        let s = Store::in_memory().unwrap();
        let (work, _lead, _worker) = work_of_two(&s);
        s.post(&Post::new(Scope::Work, &work, "the-lead", "one more thing").to("the-worker"))
            .unwrap();
        assert_eq!(s.mail_waiting().unwrap().len(), 1);

        let task = s.work_tasks(&work).unwrap().remove(0);
        let closing = s.complete_work_task(&task.id).unwrap().unwrap();

        assert!(
            s.mail_waiting().unwrap().is_empty(),
            "a work's bus ends with the work"
        );
        let held = s.mail_held().unwrap();
        assert_eq!(held.len(), 1, "reported, not delivered and not dropped");
        assert_eq!(held[0].member.name, "the-worker");
        assert_eq!(
            closing.waiting_mail, 1,
            "and the closing card says so, because it is the last chance to"
        );
    }

    /// Read an id out of a prompt the way the recipient has to: from the text,
    /// because by the time it reads it there is nowhere else to look.
    fn id_in(prompt: &str) -> Option<i64> {
        ids_in(prompt).into_iter().next()
    }

    fn ids_in(prompt: &str) -> Vec<i64> {
        prompt
            .split("message #")
            .skip(1)
            .filter_map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// The defect this test exists for was found live: a question and its
    /// answer landed in two threads, because waking drains the inbox and the
    /// prompt carried no id, so the recipient could only send afresh. Every
    /// hop then starts at depth zero and the depth bound — the money bound —
    /// is never reached.
    ///
    /// Deliberately routed the long way round: the reply is sent with an id
    /// **parsed out of the rendered prompt**, never one the test happened to
    /// be holding. Passing `reply` an id directly is exactly how the unit
    /// tests missed this.
    #[test]
    fn a_woken_agent_can_reply_in_thread_using_only_what_its_prompt_carried() {
        let s = Store::in_memory().unwrap();
        let (work, _lead, _worker) = work_of_two(&s);
        let asked = s
            .post(
                &Post::new(Scope::Work, &work, "the-lead", "where does the parser live?")
                    .to("the-worker"),
            )
            .unwrap();
        let (ids, thread, _) = queued(&asked);

        let waiting = s.mail_waiting().unwrap();
        let held = &waiting[0];
        let order = wake_order(&held.member, &held.pending).expect("the worker is woken");
        // Exactly what the ticker does next, and the reason the id has nowhere
        // else to come from.
        s.drain_inbox(&held.team, &held.member.name).unwrap();
        assert!(
            s.team_unread(&work, "the-worker").unwrap().is_empty(),
            "the wake drained the inbox, so a second read tells the agent nothing"
        );

        let id = id_in(&order.prompt).expect("the prompt names the message to reply to");
        assert_eq!(id, ids[0]);

        let answered = s
            .post(
                &Post::new(Scope::Work, &work, "the-worker", "in core/src/harness")
                    .to("the-lead")
                    .replying_to(id),
            )
            .unwrap();
        let (_, reply_thread, depth) = queued(&answered);
        assert_eq!(
            reply_thread, thread,
            "a question and its answer are one thread, not two"
        );
        assert_eq!(depth, 1, "and the second hop counts, so the bound is reachable");
    }

    /// A batch of five with one id between them would leave four of them
    /// unrepliable, which is the same bug wearing a different hat.
    #[test]
    fn every_message_in_one_woken_batch_carries_its_own_id() {
        let s = Store::in_memory().unwrap();
        let (work, _lead, _worker) = work_of_two(&s);
        let mut sent = Vec::new();
        for text in ["first", "second", "third"] {
            let out = s
                .post(&Post::new(Scope::Work, &work, "the-lead", text).to("the-worker"))
                .unwrap();
            sent.extend(queued(&out).0);
        }

        let waiting = s.mail_waiting().unwrap();
        let order = wake_order(&waiting[0].member, &waiting[0].pending).expect("woken");

        assert_eq!(order.messages, 3, "one turn carrying three, not three turns");
        assert_eq!(
            ids_in(&order.prompt),
            sent,
            "each message in the batch is separately repliable"
        );
    }

    /// Everything `jod team` does today keeps doing it: this is additive.
    #[test]
    fn an_explicit_team_delivers_whatever_the_works_are_doing() {
        let s = crew();
        s.post(&Post::new(Scope::Team, "crew", "lead", "start").to("scout"))
            .unwrap();
        let waiting = s.mail_waiting().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].scope, Scope::Team);
        assert!(s.mail_held().unwrap().is_empty());
    }

    /// In the same transaction as the sessions, so no thread outlives its
    /// participants.
    #[test]
    fn deleting_a_work_takes_its_traffic_and_its_members_with_it() {
        let s = Store::in_memory().unwrap();
        let (work, _lead, _worker) = work_of_two(&s);
        s.post(&Post::new(Scope::Work, &work, "the-lead", "take the lexer").to("the-worker"))
            .unwrap();
        assert_eq!(s.traffic(Scope::Work, &work).unwrap().len(), 1);

        assert!(s.delete_work(&work, None).unwrap().happened());

        assert!(s.traffic(Scope::Work, &work).unwrap().is_empty());
        assert!(s.members_in(Scope::Work, &work).unwrap().is_empty());
        assert!(s.mail_waiting().unwrap().is_empty());
        assert!(s.mail_held().unwrap().is_empty());
    }
}
