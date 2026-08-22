//! The one place that decides when an agent is spoken to.
//!
//! Three different things want to interrupt a session: an answer to a card, a
//! message from another agent, and a nudge typed by a human. None of them may
//! be spliced into a turn already in flight — a prompt is assembled once, at
//! spawn, so anything arriving afterwards is either ignored or acted on twice.
//!
//! So all three enqueue here, and one handler decides when the queue is
//! drained into a real turn. Delivery itself stays what the bus already does:
//! a synthetic user turn in the session's next prompt, which works on every
//! harness because every harness can resume a session by id and none of them
//! has to know Jod has a queue at all.
//!
//! ## Why one queue and not three
//!
//! "Is this session ready to be spoken to" is the same question regardless of
//! who is speaking, and it is already written down once, correctly, in
//! [`crate::team::wake_order`]. A second copy of that judgement is a second
//! thing to keep right.
//!
//! ## Batching is a feature, not an optimisation
//!
//! Ten answers queued during one turn arrive as one turn carrying ten, not ten
//! turns. That is a cost control, and it is also the better answer: an agent
//! reading everything that changed in one go responds more coherently than one
//! woken ten times with a line each.
//!
//! ## What comes through here, and what does not
//!
//! Card answers and human nudges do: [`Ticker::tick_deliveries`] walks this
//! queue on every tick and injects what is waiting as one turn. **Agent mail
//! does not**, and a reader who assumes otherwise will look for a bug that is
//! not there. Mail reaches an agent through [`crate::ticker::Ticker::tick_mail`],
//! which asks [`crate::team::wake_order`] who may be woken and takes the mail
//! off the bus with [`Store::take_mail`]. This module is authoritative for
//! **card answers and human nudges**; [`crate::team`] is authoritative for
//! **agent mail**, including whether the recipient may be woken at all.
//!
//! The merge was examined in full and is being done in order rather than at
//! once. One of the three things in the way has been removed; two remain, and
//! both are cheap to state and expensive to discover:
//!
//! 1. **This queue addresses a conversation; a team member has not got one.**
//!    A member of a work *is* a conversation and would fit straight away. A
//!    member joined through `jod team join` holds a run and a harness-side
//!    session, and every wake mints a *fresh* Jod conversation
//!    ([`crate::service::RunConversation::New`]) — so there is no stable id to
//!    key a queue on, and one resolved at send time would name a conversation
//!    that is already finished by the time it is read. Mail sent to a member
//!    that has never run has no conversation at all, and today that mail waits
//!    on the bus, visibly, which is a property A8 asks for by name.
//!    Fixing it properly means letting the queue address a *member*, which is
//!    a schema change and therefore a stop-and-ask.
//! 2. **`wake_order` asks a question this module cannot.** `plan_injection`
//!    knows only whether a turn is in flight; `wake_order` also refuses a
//!    member that is shutting down, or that has no session to resume — where
//!    waking would start a fresh context and the agent would answer having
//!    forgotten the work. Until that judgement moves too, merging the queues
//!    would leave two decisions in two modules and only look unified.
//!
//! **Removed:** this queue used to have no caller at all, and that was the
//! largest of the three. [`Ticker::tick_deliveries`] now drains it on every
//! tick, so a card answered from the rail reaches its session in a turn
//! without anybody typing anything — E2.S7's other half, and the thing
//! Reljod asked for most directly. Before that, an answer nobody fetched over
//! MCP sat queued for ever and the rail said *queued* about answers the agent
//! already had.
//!
//! Also unified, because it cost nothing and the drift was real: both queues
//! speak one [`State`], so `pending_deliveries.state` and `team_messages.state`
//! cannot mean different things by the same word.
//!
//! The order to do the rest in: move `wake_order`'s eligibility here, so there
//! is one answer to "may this be spoken to"; then, with a migration, let this
//! queue address a member as well as a conversation. Mail moves last — it is
//! the path that works, and it should be the last thing asked to change.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{JodError, Result};
use crate::store::Store;

/// Who is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A card the human answered. See [`crate::cards`].
    CardAnswer,
    /// A message from another agent, over the bus.
    Mail,
    /// Reljod, typing into a running session.
    Human,
    /// Jod itself, from a tick loop rather than from any person or agent.
    ///
    /// Its own kind because the other three would each be a lie in the
    /// transcript Reljod reads to find out what happened: `Human` would render
    /// `[message from Reljod]` on a message he never sent, `CardAnswer` would
    /// claim the session raised a card it did not raise, and `Mail` would say
    /// another agent is speaking and have [`protocol_for`] tell the session to
    /// `reply` to one that does not exist.
    Jod,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::CardAnswer => "card_answer",
            Kind::Mail => "mail",
            Kind::Human => "human",
            Kind::Jod => "jod",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "mail" => Kind::Mail,
            "human" => Kind::Human,
            "jod" => Kind::Jod,
            _ => Kind::CardAnswer,
        }
    }

    /// The line that opens this item's block in an injected turn.
    ///
    /// One turn can carry an answer to a question the agent asked, a question
    /// from a peer session, and an instruction from Reljod, and those three
    /// want three different responses. An unframed batch invites exactly the
    /// wrong reading — a peer's question answered as though the human had
    /// already settled it, or a settled decision re-litigated with the peer.
    ///
    /// The frame comes from the kind rather than from the body, so it holds
    /// even when a caller queues plain text. What is *in* the block is the
    /// caller's, frozen at enqueue; how it is announced is Jod's, and is the
    /// same for every item of that kind.
    pub fn label(&self) -> &'static str {
        match self {
            Kind::CardAnswer => "[answer to a card you raised]",
            Kind::Mail => "[message from another agent]",
            Kind::Human => "[message from Reljod]",
            Kind::Jod => "[from Jod]",
        }
    }
}

/// Where something waiting to be said to an agent has got to.
///
/// **One vocabulary for both queues.** `pending_deliveries.state` and
/// `team_messages.state` hold the same four words, and until this type was
/// shared they were two enums with two `parse` functions that happened to
/// agree. Two spellings of one vocabulary is the kind of duplication that
/// looks harmless right up to the day one of them learns a fifth word:
/// [`crate::team::MailState`] is this type, not a copy of it.
///
/// The variants are deliberately about *the message*, never about the agent.
/// Whether a session may be spoken to at all is a different question, asked in
/// exactly one place — [`crate::team::wake_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    /// A doorman is reading this, working out whether it can wait for the turn
    /// in flight or has to stop it.
    ///
    /// Its own state rather than a flag on the row, because it is what stops a
    /// second doorman being started: a row being judged is not queued, so the
    /// next tick finds nothing waiting for that conversation and starts
    /// nothing. A verdict puts the row back to `Queued` with `reviewed_at_ms`
    /// stamped, and that stamp is what stops the same message being judged
    /// twice.
    Reviewing,
    Delivered,
    /// Handed over and something went wrong on the way — a spawn that failed,
    /// a run that died before its first turn. Distinct from `Undeliverable`
    /// because it is worth retrying and that one is not.
    ///
    /// Only the bus writes this today. It lives here because the two tables
    /// share one vocabulary, and a word one table cannot spell is a word the
    /// next reader has to check for.
    Failed,
    /// There was nobody to tell. The session ended, or the scope closed, before
    /// it could be said. Recorded rather than deleted, because "it never
    /// arrived" is the answer to a question somebody asks later, and a queue
    /// that silently drops is indistinguishable from one that works.
    Undeliverable,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Reviewing => "reviewing",
            State::Delivered => "delivered",
            State::Failed => "failed",
            State::Undeliverable => "undeliverable",
        }
    }

    /// Unknown text reads as `Queued` rather than failing: a row written by a
    /// newer Jod must not make an older one unable to read its own queue, and
    /// the safe reading of "I do not know what happened to this" is that it
    /// has not happened yet.
    pub fn parse(s: &str) -> State {
        match s {
            "reviewing" => State::Reviewing,
            "delivered" => State::Delivered,
            "failed" => State::Failed,
            "undeliverable" => State::Undeliverable,
            _ => State::Queued,
        }
    }
}

/// One thing waiting to be said to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub id: i64,
    pub conversation_id: String,
    pub kind: Kind,
    /// The card id or message id this came from, as text because the two
    /// sources number themselves independently.
    pub ref_id: String,
    /// Rendered when it was queued, not when it is delivered.
    ///
    /// What the human answered is a fact about the moment they answered it.
    /// Re-rendering at delivery time would let a card edited in between
    /// silently change what had already been promised to the agent.
    pub body: String,
    pub state: State,
    /// Which run finally carried it, so "did it actually arrive" stays
    /// answerable after the fact.
    pub run_id: Option<String>,
    pub detail: Option<String>,
    pub queued_at_ms: i64,
    pub delivered_at_ms: Option<i64>,
    /// When a doorman finished reading this, if one ever did.
    ///
    /// Set once and never cleared, because the question it answers is "has
    /// this already been judged", not "when was it last looked at". Without
    /// it, a message a doorman decided could wait would go back to `Queued`
    /// and be judged again on the next tick, and again after that, for as long
    /// as the turn it is waiting behind keeps running.
    pub reviewed_at_ms: Option<i64>,
}

/// What the handler decided to do for one conversation.
///
/// A value rather than an action, so the decision is testable without spawning
/// anything — the same shape [`crate::team::WakeOrder`] uses, and for the same
/// reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub conversation_id: String,
    /// Every queued item, batched into one turn.
    pub items: Vec<Pending>,
    /// The synthetic user turn to inject.
    pub prompt: String,
}

impl Injection {
    pub fn count(&self) -> usize {
        self.items.len()
    }
}

/// What should happen to a conversation's queue right now.
///
/// Three answers rather than two. `Option<Injection>` could say "speak" and
/// "do not speak yet", and for card answers and mail those are the only two
/// that exist — a card answer waits for the turn to end and loses nothing by
/// waiting. A message Reljod typed into a chat that is already working is
/// different: he is looking at the screen, and the reason he typed while it
/// was busy is often that the turn in flight is going the wrong way. Somebody
/// has to read it and say whether it can wait, and that somebody cannot be the
/// conversation itself, because the conversation is the thing that is busy.
///
/// So the third answer names the case and hands it on. All the judgement here
/// is still in *when*, and none of it is in a model: `Judge` is returned as a
/// value, the same as the other two, and what to do with it is the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to say, or nothing that may be said yet.
    Hold,
    /// Say all of this now.
    Speak(Injection),
    /// A turn is in flight and something is queued behind it that nobody has
    /// judged. Start a doorman on these rows.
    Judge {
        conversation_id: String,
        items: Vec<Pending>,
    },
}

impl Plan {
    /// What this plan says to say now, or `None` when it says anything else.
    ///
    /// Both other answers mean "not yet" to a caller that only wants to know
    /// whether to speak, and most callers are exactly that. The ones that have
    /// to tell a hold from a judgement match on the enum.
    pub fn speak(self) -> Option<Injection> {
        match self {
            Plan::Speak(injection) => Some(injection),
            Plan::Hold | Plan::Judge { .. } => None,
        }
    }

    /// Whether this plan says a doorman should be started.
    pub fn is_judge(&self) -> bool {
        matches!(self, Plan::Judge { .. })
    }
}

/// What a conversation's turn in flight is doing, for the doorman to read.
///
/// Deliberately small. The doorman is deciding one thing — can this wait — and
/// the whole of what it needs is what the turn was asked to do and the last
/// thing it said. Handing it the transcript would invite it to form an opinion
/// about the work, which is not its job and which it is not equipped for: it
/// runs on a cheap model in a conversation one second old.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    /// The run to stop, if stopping is the answer.
    pub run_id: String,
    /// What the run answers to in the fleet.
    pub name: String,
    /// The turn's own opening prompt — what it was asked to do.
    pub asked: Option<String>,
    /// The last thing it has said, which is as close as anything gets to
    /// "where has it got to".
    pub said: Option<String>,
}

/// The synthetic user turn a batch becomes.
///
/// One block per item, in the order they were queued, separated by a blank
/// line, each opened by [`Kind::label`] and followed by the body frozen at
/// enqueue.
///
/// The framing is per item rather than per batch because a batch is *mixed* in
/// the ordinary case: three answers to the agent's own cards and two questions
/// from a peer session arrive together, and they call for different responses.
/// A wrapper around the whole turn — "5 updates" — would say nothing about
/// which is which, and the agent would have to infer the source from the prose,
/// which is exactly the inference that goes wrong.
///
/// No count, header or footer around the blocks otherwise. A batch of one reads
/// like one framed message, which is what `wake_order` already delivers for
/// mail on its own.
pub fn render_injection(items: &[Pending]) -> String {
    let body = items
        .iter()
        .map(|p| format!("{}\n{}", p.kind.label(), p.body))
        .collect::<Vec<_>>()
        .join("\n\n");

    match protocol_for(items) {
        Some(reminder) => format!("{body}\n\n{reminder}"),
        None => body,
    }
}

/// What the recipient needs in order to *act* on what it has just been handed.
///
/// A turn like this arrives in a session whose framing is several turns back,
/// and an agent does not reliably reach for a verb it was told about once at
/// the start. Measured, not assumed: an answerer briefed at session start on
/// how to use the bus was asked a question some turns later, replied in prose,
/// and never touched the bus at all. Nothing failed loudly, because nothing
/// failed — an agent that has forgotten a protocol is an agent behaving
/// reasonably in the absence of one.
///
/// So the turn carries its own instructions. This is the same argument that
/// put the message id into the body of a delivered message rather than leaving
/// the recipient to look it up: **whatever is needed to respond travels with
/// the thing being responded to.**
///
/// Deliberately short, and only for the kinds that need a verb. A card answer
/// needs none — the agent asked a question and is being told the answer, which
/// it acts on by carrying on. Mail needs one, because replying is a tool call
/// the agent has to choose to make.
fn protocol_for(items: &[Pending]) -> Option<&'static str> {
    items.iter().any(|p| p.kind == Kind::Mail).then_some(
        "To answer any of the messages above, call `reply` with the message \
         number shown in its brackets — that is what keeps a reply in the same \
         thread as the question. Use `send_message` only to start something \
         new. Replying in prose here reaches nobody: this is a message from \
         another agent, not from a person reading your output.",
    )
}

// ---- the store ---------------------------------------------------------

impl Store {
    /// Queue something to be said to a session at the next safe moment.
    ///
    /// Every inbound thing goes through here — a card answer, a message from
    /// another agent, a nudge typed by a human — because "is this session ready
    /// to be spoken to" is one question, and answering it in three places is
    /// three places to get it wrong.
    ///
    /// `body` is the item's *content*, not its framing: the line announcing
    /// where it came from is added at render time from `kind`, so a caller
    /// queuing a peer's message can pass the message and its sender and does
    /// not have to remember to label it. `ref_id` is whatever identifies the
    /// thing on the caller's side — a card id, a message id — kept as text
    /// because those two sources number themselves independently.
    ///
    /// **This is the entry point for a caller that has no transaction of its
    /// own.** A caller that does — [`Store::answer_card`] queues inside the
    /// transaction that answers the card — goes through [`insert_pending`]
    /// instead, and must: an answered card with nothing queued to carry it
    /// would show as *queued* in the rail for ever and the agent would never be
    /// told. That is not two front doors; it is one insert, reachable with or
    /// without a transaction already open. Both write the same row and there is
    /// no third way in.
    ///
    /// It has no production caller yet, and the honest reason is that its two
    /// remaining sources do not exist: nothing produces [`Kind::Human`] until
    /// the terminal grows "nudge a session mid-turn", and [`Kind::Mail`] waits
    /// on the queue learning to address a member (see the module docs).
    pub fn enqueue_delivery(
        &self,
        conversation_id: &str,
        kind: Kind,
        ref_id: &str,
        body: &str,
    ) -> Result<Pending> {
        if body.trim().is_empty() {
            return Err(JodError::Invalid(
                "nothing to deliver: an empty body would spend a turn saying nothing".into(),
            ));
        }
        let at = now_ms();
        self.write(|tx| {
            let id = insert_pending(tx, conversation_id, kind, ref_id, body, at)?;
            require_pending(tx, id)
        })
    }

    /// What is still waiting for this session, oldest first.
    ///
    /// Insertion order, not answer order: two cards answered in the same
    /// millisecond still reach the agent in the order they were queued, and the
    /// agent reads a batch top to bottom.
    pub fn pending_for(&self, conversation_id: &str) -> Result<Vec<Pending>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let sql = format!(
            "SELECT {PENDING_COLUMNS} FROM pending_deliveries
              WHERE conversation_id = ?1 AND state = 'queued'
              ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![conversation_id], row_to_pending)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record that a batch actually reached the agent, in one transaction.
    ///
    /// Named for the queue rather than `mark_delivered`, which the outbound
    /// ledger already owns for a single notification row.
    ///
    /// Only rows still `queued` are touched, so replaying a delivery — a
    /// supervisor that crashed between the spawn and this call, and is being
    /// retried — cannot restamp an item with a later run and cannot mark a card
    /// delivered twice.
    ///
    /// The source card is flipped in the same transaction, because the rail
    /// reads the card and the handler reads the queue: if those two committed
    /// separately there is an instant where the agent has been told and the
    /// rail still says *queued*, and that is the state a person answers again.
    pub fn mark_deliveries_delivered(&self, ids: &[i64], run_id: Option<&str>) -> Result<()> {
        self.settle(ids, State::Delivered, run_id, None)
    }

    /// The session ended before it could be told.
    ///
    /// Recorded rather than deleted, and the card follows: "nobody ever heard
    /// this" is the answer to a question somebody asks later, and a queue that
    /// silently drops is indistinguishable from one that works.
    pub fn mark_deliveries_undeliverable(&self, ids: &[i64], reason: &str) -> Result<()> {
        self.settle(ids, State::Undeliverable, None, Some(reason))
    }

    fn settle(
        &self,
        ids: &[i64],
        state: State,
        run_id: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let at = now_ms();
        let delivered_at = (state == State::Delivered).then_some(at);
        self.write(|tx| {
            let placeholders = (2..2 + ids.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(State::Queued.as_str().into())];
            args.extend(ids.iter().map(|id| rusqlite::types::Value::Integer(*id)));

            // Read first, so the card flip below covers exactly the rows this
            // call moved — not the ones a previous attempt already settled.
            let sql = format!(
                "SELECT id, kind, ref_id FROM pending_deliveries
                  WHERE state = ?1 AND id IN ({placeholders})"
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    Kind::parse(&r.get::<_, String>(1)?),
                    r.get::<_, String>(2)?,
                ))
            })?;
            let moving = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);

            for (id, kind, ref_id) in moving {
                tx.execute(
                    "UPDATE pending_deliveries
                        SET state = ?2, run_id = coalesce(?3, run_id), detail = ?4,
                            delivered_at_ms = ?5
                      WHERE id = ?1",
                    params![id, state.as_str(), run_id, detail, delivered_at],
                )?;
                if kind != Kind::CardAnswer {
                    continue;
                }
                // The rail's card carries the same fact for the human. A ref_id
                // that is not a card id belongs to a caller that mislabelled its
                // row; there is no card to update and nothing to repair here.
                let Ok(card_id) = ref_id.parse::<i64>() else {
                    continue;
                };
                // The rail has three words, not five: to a person waiting on an
                // answer, "the spawn failed and will be retried", "an assistant
                // is reading it" and "it has not gone yet" are the same fact,
                // and a card that said `failed` would invite answering it
                // again. `Failed` is the bus's word for something it will try
                // once more, and `Reviewing` is a doorman's word for something
                // still on its way.
                let card_state = match state {
                    State::Delivered => "delivered",
                    State::Undeliverable => "undeliverable",
                    State::Queued | State::Failed | State::Reviewing => "queued",
                };
                tx.execute(
                    "UPDATE cards SET delivery = ?2, delivered_at_ms = ?3, updated_at_ms = ?4
                      WHERE id = ?1",
                    params![card_id, card_state, delivered_at, at],
                )?;
            }
            Ok(())
        })
    }

    /// Every conversation with something waiting to be said to it.
    ///
    /// The list the tick walks. Deliberately just the addresses: whether each
    /// one may be spoken to *now* is [`Store::plan_injection`]'s question, and
    /// answering it in the query would put the judgement in two places.
    ///
    /// Oldest queue first, so a session that has been waiting since before
    /// lunch is not overtaken by one queued a second ago.
    pub fn conversations_awaiting_delivery(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT conversation_id, MIN(id) AS first FROM pending_deliveries
              WHERE state = 'queued' GROUP BY conversation_id ORDER BY first",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether a turn of this conversation is in flight.
    ///
    /// The one fact [`Store::plan_injection`] cannot work out for itself, and
    /// the reason the whole queue exists: a prompt is assembled once, at spawn,
    /// so anything spliced in afterwards arrives in a context that was built
    /// before it existed.
    ///
    /// Read from the runs that wrote into this conversation, which is the same
    /// join `conversation_for_run` uses in the other direction — there is no
    /// column saying a conversation is busy, and a second one would be a fact
    /// that could disagree with the runs themselves.
    pub fn conversation_is_busy(&self, conversation_id: &str) -> Result<bool> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let busy: i64 = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages m JOIN runs r ON r.id = m.run_id
                             WHERE m.conversation_id = ?1 AND r.status = 'running')",
            params![conversation_id],
            |r| r.get(0),
        )?;
        Ok(busy != 0)
    }

    /// Decide what, if anything, to say to this conversation now.
    ///
    /// A value rather than an action, the same shape and for the same reason as
    /// [`crate::team::wake_order`]: *when* to speak is where all the judgement
    /// is, and keeping it out of the spawning means it can be tested without a
    /// harness binary, a tmux server, or a running agent.
    ///
    /// Returns [`Plan::Hold`] — deliberately, in each case — when:
    ///
    /// - **Nothing is queued.** Waking an agent to tell it nothing burns a turn
    ///   and a context window.
    /// - **A turn is in flight and everything queued behind it has already been
    ///   judged.** The running turn's prompt was assembled before any of this
    ///   arrived, so splicing it in produces an answer to a question the agent
    ///   has already moved past. It waits; nothing is lost.
    ///
    /// A turn in flight with something queued that *has not* been judged is
    /// [`Plan::Judge`] rather than a hold, and that is the whole of what
    /// changed here. It used to be a hold, and the cost of that was a message
    /// Reljod typed to stop a turn going the wrong way sitting in a queue until
    /// the turn it was trying to stop had finished.
    ///
    /// Everything queued goes into **one** injection. Ten cards answered while a
    /// run was mid-turn arrive as one turn carrying ten, not ten turns: cheaper,
    /// and the better answer as well, because an agent reading everything that
    /// changed in one go responds more coherently than one woken ten times with
    /// a line each.
    pub fn plan_injection(&self, conversation_id: &str, busy: bool) -> Result<Plan> {
        let items = self.pending_for(conversation_id)?;
        if items.is_empty() {
            return Ok(Plan::Hold);
        }
        if busy {
            // Two filters, and both of them save money.
            //
            // **Only what a person typed.** A card answer and a peer's message
            // have no reason to want a turn stopped: the agent asked a question
            // and is being told the answer, and it can be told when the turn
            // ends. Reading every queued row with a model would put an
            // assistant in front of every card answered while anything was
            // running.
            //
            // **Only what nobody has read yet.** A doorman that held a message
            // left its stamp on the row, and re-judging it would start a fresh
            // doorman every tick for as long as the turn ran — the same
            // message, the same verdict, paid for again each minute.
            let unjudged: Vec<Pending> = items
                .into_iter()
                .filter(|p| p.kind == Kind::Human && p.reviewed_at_ms.is_none())
                .collect();
            if unjudged.is_empty() {
                return Ok(Plan::Hold);
            }
            return Ok(Plan::Judge {
                conversation_id: conversation_id.to_string(),
                items: unjudged,
            });
        }
        Ok(Plan::Speak(Injection {
            conversation_id: conversation_id.to_string(),
            prompt: render_injection(&items),
            items,
        }))
    }

    /// Take these rows out of the queue while a doorman reads them.
    ///
    /// The claim is what makes "only one doorman at a time" true, so it is an
    /// atomic move off `queued` rather than a read followed by a write: two
    /// ticks overlapping — a daemon tick and a console that has just been typed
    /// into — would otherwise both see the same queued row and both pay for a
    /// model to read it.
    ///
    /// Returns how many rows it actually claimed. Zero means somebody else got
    /// there first, and the caller must start nothing.
    pub fn claim_for_review(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        self.write(|tx| {
            let placeholders = (2..2 + ids.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(State::Queued.as_str().into())];
            args.extend(ids.iter().map(|id| rusqlite::types::Value::Integer(*id)));
            let sql = format!(
                "UPDATE pending_deliveries SET state = 'reviewing'
                  WHERE state = ?1 AND id IN ({placeholders})"
            );
            Ok(tx.execute(&sql, params_from_iter(args))?)
        })
    }

    /// Put reviewed rows back in the queue, with the verdict stamped on them.
    ///
    /// Every path out of a review comes back through here — a doorman that held,
    /// a doorman that interrupted, and a doorman that died without saying
    /// anything.
    ///
    /// **The third case is not hypothetical and it was not covered.** This
    /// comment used to claim it was, and the code did not: every caller was a
    /// path where the *spawn* failed, so a doorman that started and then ended
    /// — held, crashed, or interrupted — left its rows in `Reviewing` for ever.
    /// [`Store::pending_for`] cannot see them, so the message was gone while the
    /// console went on saying an assistant was reading it. Found by an explorer
    /// session typing "STOP - urgent, forget the essay" into a busy chat and
    /// watching it never arrive. [`Store::release_stale_reviews`] is what closes
    /// it, and this is what it calls.
    pub fn finish_review(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let at = now_ms();
        self.write(|tx| {
            let placeholders = (3..3 + ids.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args: Vec<rusqlite::types::Value> = vec![
                rusqlite::types::Value::Text(State::Reviewing.as_str().into()),
                rusqlite::types::Value::Integer(at),
            ];
            args.extend(ids.iter().map(|id| rusqlite::types::Value::Integer(*id)));
            let sql = format!(
                "UPDATE pending_deliveries SET state = 'queued', reviewed_at_ms = ?2
                  WHERE state = ?1 AND id IN ({placeholders})"
            );
            tx.execute(&sql, params_from_iter(args))?;
            Ok(())
        })
    }

    /// Say which run is doing the reading, so the sweep knows whose review it is.
    ///
    /// Written after the spawn, because the run id does not exist before it.
    /// That leaves a gap — claimed, not yet stamped — and
    /// [`Store::release_stale_reviews`] deliberately treats a row in that gap as
    /// releasable. The two failure modes are not the same size: releasing early
    /// means a doorman's verdict is ignored and the message is delivered
    /// normally when the turn ends, and not releasing means the message is lost.
    ///
    /// Reuses `run_id` rather than adding a column. The column means "which run
    /// last had this", and a doorman reading it is a run having it; delivery
    /// overwrites it with the run that actually carried it.
    pub fn record_reviewer(&self, ids: &[i64], run_id: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|tx| {
            let placeholders = (3..3 + ids.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args: Vec<rusqlite::types::Value> = vec![
                rusqlite::types::Value::Text(State::Reviewing.as_str().into()),
                rusqlite::types::Value::Text(run_id.into()),
            ];
            args.extend(ids.iter().map(|id| rusqlite::types::Value::Integer(*id)));
            let sql = format!(
                "UPDATE pending_deliveries SET run_id = ?2
                  WHERE state = ?1 AND id IN ({placeholders})"
            );
            tx.execute(&sql, params_from_iter(args))?;
            Ok(())
        })
    }

    /// Put back every message whose doorman has stopped reading it.
    ///
    /// **A review ends when the run doing it ends.** Whatever the doorman
    /// decided, it has already done by the time its run is over: an interrupt is
    /// a tool call it has made, and a hold is a tool call it has decided not to
    /// make. Either way nobody is reading the message any more, so leaving it
    /// out of the queue can only lose it.
    ///
    /// That rule needs no timeout and no guess about what the verdict was, which
    /// is why it is this rule and not a stale-after-N-minutes one. It also does
    /// not care *why* the run ended — held, crashed, killed, or recorded
    /// `failed` by a harness that mislabels a successful run, which AGY
    /// currently does. All four mean the same thing here.
    ///
    /// A row with no reviewer recorded is released too. Either the spawn never
    /// happened or the process died between claiming and stamping, and in both
    /// cases nothing is reading it.
    ///
    /// Returns how many it put back, so a tick can say so out loud.
    pub fn release_stale_reviews(&self) -> Result<usize> {
        let at = now_ms();
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE pending_deliveries
                    SET state = 'queued', reviewed_at_ms = coalesce(reviewed_at_ms, ?1)
                  WHERE state = 'reviewing'
                    AND (run_id IS NULL
                         OR NOT EXISTS (SELECT 1 FROM runs r
                                         WHERE r.id = pending_deliveries.run_id
                                           AND r.status = 'running'))",
                params![at],
            )?)
        })
    }

    /// Everything a doorman was left holding, oldest first.
    ///
    /// Its own reader because [`Store::pending_for`] answers about the queue and
    /// these rows are deliberately not in it. The sweep that puts a crashed
    /// doorman's rows back needs to find them, and so does a test that wants to
    /// prove they were claimed.
    pub fn under_review_for(&self, conversation_id: &str) -> Result<Vec<Pending>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let sql = format!(
            "SELECT {PENDING_COLUMNS} FROM pending_deliveries
              WHERE conversation_id = ?1 AND state = 'reviewing'
              ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![conversation_id], row_to_pending)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The turn this conversation currently has in flight, if it has one.
    ///
    /// The other half of [`Store::conversation_is_busy`]: that answers whether
    /// there is one, and this says what it is. Read off the same join, so the
    /// two can never disagree about which run is meant.
    ///
    /// The newest running run wins if somehow there are two. That is not a
    /// state the console produces, but a delivery injected at the moment a turn
    /// was starting could, and stopping the older of the two would leave the
    /// one Reljod is actually watching alive.
    pub fn in_flight_turn(&self, conversation_id: &str) -> Result<Option<InFlight>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT r.id, r.name FROM runs r
                  WHERE r.status = 'running'
                    AND EXISTS (SELECT 1 FROM messages m
                                 WHERE m.run_id = r.id AND m.conversation_id = ?1)
                  ORDER BY r.created_at_ms DESC LIMIT 1",
                params![conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((run_id, name)) = found else {
            return Ok(None);
        };
        let text_of = |role: &str, order: &str| -> Option<String> {
            conn.query_row(
                &format!(
                    "SELECT text FROM messages
                      WHERE run_id = ?1 AND role = ?2 AND text <> ''
                      ORDER BY id {order} LIMIT 1"
                ),
                params![run_id, role],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        Ok(Some(InFlight {
            asked: text_of("user", "ASC"),
            said: text_of("assistant", "DESC"),
            run_id,
            name,
        }))
    }
}

// ---- helpers -----------------------------------------------------------

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const PENDING_COLUMNS: &str = "id, conversation_id, kind, ref_id, body, state, run_id, detail,
     queued_at_ms, delivered_at_ms, reviewed_at_ms";

/// The one insert, so the queue has one shape.
///
/// Takes a connection rather than doing its own transaction because
/// [`Store::answer_card`] queues inside the transaction that answers the card:
/// an answered card with nothing waiting to carry it would sit at *queued*
/// forever.
pub(crate) fn insert_pending(
    conn: &Connection,
    conversation_id: &str,
    kind: Kind,
    ref_id: &str,
    body: &str,
    at_ms: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pending_deliveries
           (conversation_id, kind, ref_id, body, state, queued_at_ms)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
        params![conversation_id, kind.as_str(), ref_id, body, at_ms],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_pending(r: &rusqlite::Row) -> rusqlite::Result<Pending> {
    Ok(Pending {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        kind: Kind::parse(&r.get::<_, String>(2)?),
        ref_id: r.get(3)?,
        body: r.get(4)?,
        state: State::parse(&r.get::<_, String>(5)?),
        run_id: r.get(6)?,
        detail: r.get(7)?,
        queued_at_ms: r.get(8)?,
        delivered_at_ms: r.get(9)?,
        reviewed_at_ms: r.get(10)?,
    })
}

fn require_pending(conn: &Connection, id: i64) -> Result<Pending> {
    let sql = format!("SELECT {PENDING_COLUMNS} FROM pending_deliveries WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_pending)
        .optional()?
        .ok_or_else(|| JodError::Invalid(format!("no pending delivery `{id}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::{Delivery, NewCard};
    use crate::harness::HarnessKind;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    /// A queued delivery follows the chat it was queued for.
    ///
    /// Main compacts itself when its context fills, opening a fresh
    /// conversation and moving the pin. A delivery queued just before that
    /// still named the thread that was compacted away — so `tick_deliveries`
    /// would resume the *old* session and inject Reljod's answer into a
    /// conversation the console no longer shows, where he would never see the
    /// reply.
    ///
    /// The fourth thing found holding a stale conversation id after a
    /// compaction, and filed here for the same reason as the others.
    #[test]
    fn a_queued_delivery_follows_the_main_chat_through_a_compaction() {
        let s = store();
        let main = s
            .main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        for turn in 0..3 {
            s.append_prompt(&main, &format!("run-{turn}"), "go").unwrap();
        }
        s.enqueue_delivery(&main, Kind::CardAnswer, "card-1", "yes, go ahead")
            .unwrap();

        s.continue_as_new(&main, "so far", "full").unwrap();
        let now = s.pinned_conversation().unwrap().unwrap();
        assert_ne!(now, main, "the pin moved, which is the premise");

        assert_eq!(
            s.conversations_awaiting_delivery().unwrap(),
            vec![now],
            "the answer is owed to whichever conversation is main now",
        );
    }

    fn conversation(s: &Store) -> String {
        s.new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .expect("conversation")
            .id
    }

    /// A run row in a given state, which is the whole of what
    /// [`Store::release_stale_reviews`] reads to decide whether anybody is
    /// still reading a message.
    fn run_with_status(s: &Store, id: &str, status: &str) {
        s.save_run(&crate::store::StoredRun {
            id: id.into(),
            name: format!("doorman {id}"),
            harness: "agy".into(),
            status: status.into(),
            cwd: "/tmp".into(),
            session_id: None,
            pid: None,
            pgid: None,
            created_at_ms: 0,
            summary: serde_json::Value::Null,
        })
        .expect("a run row");
    }

    /// A card raised and answered, which is the ordinary way something reaches
    /// this queue.
    fn answered_card(s: &Store, conversation_id: &str, title: &str, answer: &str) -> i64 {
        let card = s
            .raise_card(NewCard {
                conversation_id: conversation_id.into(),
                title: title.into(),
                ..NewCard::default()
            })
            .expect("raise");
        s.answer_card(card.id, None, Some(answer)).expect("answer");
        card.id
    }

    // ---- the queue -----------------------------------------------------

    #[test]
    fn a_queued_item_keeps_everything_it_was_queued_with() {
        let s = store();
        let c = conversation(&s);
        let queued = s
            .enqueue_delivery(
                &c,
                Kind::Mail,
                "17",
                "[message from lead]\nstart on the parser",
            )
            .unwrap();

        assert_eq!(queued.conversation_id, c);
        assert_eq!(queued.kind, Kind::Mail);
        assert_eq!(queued.ref_id, "17");
        assert_eq!(queued.state, State::Queued);
        assert_eq!(queued.run_id, None);
        assert_eq!(queued.delivered_at_ms, None);
        assert!(queued.body.contains("start on the parser"));
        assert_eq!(s.pending_for(&c).unwrap(), vec![queued]);
    }

    /// Insertion order, not answer order: the agent reads a batch top to
    /// bottom, and two answers given in one millisecond still arrive in the
    /// order they were given.
    #[test]
    fn the_queue_comes_back_oldest_first() {
        let s = store();
        let c = conversation(&s);
        for text in ["first", "second", "third"] {
            s.enqueue_delivery(&c, Kind::Mail, "1", text).unwrap();
        }
        let bodies: Vec<_> = s
            .pending_for(&c)
            .unwrap()
            .into_iter()
            .map(|p| p.body)
            .collect();
        assert_eq!(bodies, vec!["first", "second", "third"]);
    }

    #[test]
    fn an_empty_body_is_refused_rather_than_spending_a_turn_on_nothing() {
        let s = store();
        let c = conversation(&s);
        assert!(matches!(
            s.enqueue_delivery(&c, Kind::Human, "", "   \n "),
            Err(JodError::Invalid(_))
        ));
        assert!(s.pending_for(&c).unwrap().is_empty());
    }

    #[test]
    fn one_conversations_queue_is_not_anothers() {
        let s = store();
        let (a, b) = (conversation(&s), conversation(&s));
        s.enqueue_delivery(&a, Kind::Mail, "1", "for a").unwrap();

        assert_eq!(s.pending_for(&a).unwrap().len(), 1);
        assert!(s.pending_for(&b).unwrap().is_empty());
        assert!(s.plan_injection(&b, false).unwrap().speak().is_none());
    }

    // ---- the judgement -------------------------------------------------

    /// The rule the whole module exists for. The running turn's prompt was
    /// assembled before any of this arrived; splicing it in produces an answer
    /// to a question the agent has already moved past.
    #[test]
    fn a_turn_in_flight_is_never_interrupted() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "1", "answer me")
            .unwrap();

        assert!(s.plan_injection(&c, true).unwrap().speak().is_none());
        assert_eq!(
            s.pending_for(&c).unwrap().len(),
            1,
            "holding is not dropping: it is still queued"
        );
        assert!(s.plan_injection(&c, false).unwrap().speak().is_some());
    }

    /// The one thing that is allowed to want a turn stopped, and what happens
    /// to it. Reljod typing into a chat that is already working is the only
    /// case where waiting is the wrong answer often enough to be worth reading,
    /// so it is planned as `Judge` and everything else still holds.
    #[test]
    fn a_message_typed_into_a_busy_chat_is_judged_once_and_then_waits() {
        let s = store();
        let c = conversation(&s);
        let queued = s
            .enqueue_delivery(&c, Kind::Human, "typed", "no, the other repo")
            .unwrap();

        let plan = s.plan_injection(&c, true).unwrap();
        let Plan::Judge { items, .. } = &plan else {
            panic!("a message nobody has read yet is judged, not held: {plan:?}");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, queued.id);

        // Claimed, so a second tick finds nothing to start a second doorman on.
        assert_eq!(s.claim_for_review(&[queued.id]).unwrap(), 1);
        assert!(s.pending_for(&c).unwrap().is_empty());
        assert_eq!(s.under_review_for(&c).unwrap().len(), 1);
        assert_eq!(
            s.plan_injection(&c, true).unwrap(),
            Plan::Hold,
            "a queue somebody is already reading is not a queue to read again"
        );
        assert_eq!(
            s.claim_for_review(&[queued.id]).unwrap(),
            0,
            "and a second claim takes nothing"
        );

        // The doorman held, so the message goes back to waiting — stamped, so
        // the next tick does not pay to have it read all over again.
        s.finish_review(&[queued.id]).unwrap();
        let back = s.pending_for(&c).unwrap();
        assert_eq!(back.len(), 1);
        assert!(back[0].reviewed_at_ms.is_some());
        assert_eq!(
            s.plan_injection(&c, true).unwrap(),
            Plan::Hold,
            "read once is read: it waits for the turn to end like anything else"
        );

        // And when the turn ends it is delivered, exactly as before.
        let injection = s.plan_injection(&c, false).unwrap().speak().expect("speaks");
        assert_eq!(injection.count(), 1);
        assert!(injection.prompt.contains("no, the other repo"));
    }

    /// A card answer or a peer's message is not what this is for. Only a person
    /// typing has any reason to want a turn stopped, and reading every queued
    /// row with a model would put an assistant in front of every card answered
    /// while something was running.
    #[test]
    fn only_a_message_from_reljod_is_worth_reading_mid_turn() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::CardAnswer, "1", "yes, go ahead")
            .unwrap();
        assert_eq!(s.plan_injection(&c, true).unwrap(), Plan::Hold);

        s.enqueue_delivery(&c, Kind::Mail, "2", "a peer asks something")
            .unwrap();
        assert_eq!(s.plan_injection(&c, true).unwrap(), Plan::Hold);
    }

    /// Everything queued behind the turn goes to one doorman, not one doorman
    /// each. Three lines typed in quick succession are one thought most of the
    /// time, and reading them separately would let two of them disagree.
    #[test]
    fn everything_typed_behind_one_turn_is_judged_together() {
        let s = store();
        let c = conversation(&s);
        for n in 0..3 {
            s.enqueue_delivery(&c, Kind::Human, "typed", &format!("line {n}"))
                .unwrap();
        }
        let Plan::Judge { items, .. } = s.plan_injection(&c, true).unwrap() else {
            panic!("three lines behind one turn are judged");
        };
        assert_eq!(items.len(), 3);
    }

    /// **A review ends when the run doing it ends**, whatever the verdict was
    /// and whatever the harness recorded about how the run finished.
    ///
    /// The regression this pins is the one that made the whole assistant tier
    /// worse than not having it: every release path was a *spawn failure*, so a
    /// doorman that started and then ended — held, crashed, or recorded
    /// `failed` by a harness that mislabels success — left the message in
    /// `Reviewing`, where `pending_for` cannot see it. The console went on
    /// saying an assistant was reading it and the message was gone.
    #[test]
    fn a_message_goes_back_in_the_queue_when_its_doorman_stops_reading_it() {
        for ended_as in ["completed", "failed", "killed"] {
            let s = store();
            let c = conversation(&s);
            let queued = s
                .enqueue_delivery(&c, Kind::Human, "typed", "STOP — urgent")
                .unwrap();
            s.claim_for_review(&[queued.id]).unwrap();
            run_with_status(&s, "doorman-1", "running");
            s.record_reviewer(&[queued.id], "doorman-1").unwrap();

            assert_eq!(
                s.release_stale_reviews().unwrap(),
                0,
                "a doorman still reading it keeps it: {ended_as}"
            );
            assert!(s.pending_for(&c).unwrap().is_empty());

            run_with_status(&s, "doorman-1", ended_as);
            assert_eq!(s.release_stale_reviews().unwrap(), 1, "{ended_as}");

            let back = s.pending_for(&c).unwrap();
            assert_eq!(back.len(), 1, "{ended_as}");
            assert!(
                back[0].reviewed_at_ms.is_some(),
                "read once and not read again: {ended_as}"
            );
            assert!(s
                .plan_injection(&c, false)
                .unwrap()
                .speak()
                .is_some_and(|i| i.prompt.contains("STOP — urgent")));
        }
    }

    /// A stuck review must not stop the *next* message being read.
    ///
    /// Checked because it was reported as the worst part of the stranding bug —
    /// one failure disabling the assistant for that chat for good, with the
    /// counter going up and nothing ever reading any of it. It is not what this
    /// code does: a claim is per row, not per conversation, so a message nobody
    /// is reading any more does not hold the door shut on the ones behind it.
    ///
    /// Worth a test either way. The claim was plausible enough to be believed,
    /// and "one failure and the tier is dead" and "one message is late" call for
    /// very different fixes.
    #[test]
    fn a_message_stuck_under_review_does_not_stop_the_next_one_being_read() {
        let s = store();
        let c = conversation(&s);
        let stuck = s.enqueue_delivery(&c, Kind::Human, "typed", "STOP - urgent").unwrap();
        s.claim_for_review(&[stuck.id]).unwrap();
        run_with_status(&s, "doorman-1", "failed");
        s.record_reviewer(&[stuck.id], "doorman-1").unwrap();

        // Two more typed behind it while the first is still stranded.
        s.enqueue_delivery(&c, Kind::Human, "typed", "seriously, stop").unwrap();
        s.enqueue_delivery(&c, Kind::Human, "typed", "are you listening").unwrap();

        let Plan::Judge { items, .. } = s.plan_injection(&c, true).unwrap() else {
            panic!("a stranded row must not hold the door shut on the ones behind it");
        };
        assert_eq!(items.len(), 2);

        // And the sweep brings the stranded one back to join them.
        assert_eq!(s.release_stale_reviews().unwrap(), 1);
        assert_eq!(s.pending_for(&c).unwrap().len(), 3);
    }

    /// A row claimed by a process that died before it could say which run was
    /// reading it. Nothing is reading it, so it goes back.
    ///
    /// Released rather than held, deliberately: releasing early costs a
    /// doorman's verdict being ignored and the message arriving when the turn
    /// ends, and holding costs the message.
    #[test]
    fn a_review_nobody_owns_is_not_a_review() {
        let s = store();
        let c = conversation(&s);
        let queued = s
            .enqueue_delivery(&c, Kind::Human, "typed", "still here")
            .unwrap();
        s.claim_for_review(&[queued.id]).unwrap();
        assert!(s.under_review_for(&c).unwrap()[0].run_id.is_none());

        assert_eq!(s.release_stale_reviews().unwrap(), 1);
        assert_eq!(s.pending_for(&c).unwrap().len(), 1);
    }

    /// And a reviewer that was never a run at all — a row naming a run this
    /// database has no record of — is not something to wait on for ever.
    #[test]
    fn a_reviewer_that_does_not_exist_releases_the_message() {
        let s = store();
        let c = conversation(&s);
        let queued = s.enqueue_delivery(&c, Kind::Human, "typed", "hello").unwrap();
        s.claim_for_review(&[queued.id]).unwrap();
        s.record_reviewer(&[queued.id], "a-run-that-never-was").unwrap();

        assert_eq!(s.release_stale_reviews().unwrap(), 1);
        assert_eq!(s.pending_for(&c).unwrap().len(), 1);
    }

    /// A doorman that died without saying anything must not take the message
    /// with it. `finish_review` is the one way out of `Reviewing`, and it is
    /// reached from the verdict, from a failed spawn, and from a sweep.
    #[test]
    fn a_message_left_under_review_can_always_be_put_back() {
        let s = store();
        let c = conversation(&s);
        let queued = s
            .enqueue_delivery(&c, Kind::Human, "typed", "still here")
            .unwrap();
        s.claim_for_review(&[queued.id]).unwrap();
        assert!(s.pending_for(&c).unwrap().is_empty());

        s.finish_review(&[queued.id]).unwrap();
        assert_eq!(s.under_review_for(&c).unwrap().len(), 0);
        assert_eq!(s.pending_for(&c).unwrap().len(), 1);
    }

    /// Waking an agent to tell it nothing burns a turn and a context window.
    #[test]
    fn an_idle_session_with_an_empty_queue_is_left_alone() {
        let s = store();
        let c = conversation(&s);
        assert!(s.plan_injection(&c, false).unwrap().speak().is_none());
    }

    /// The batching guarantee, stated exactly as the spec does: ten answers
    /// given during one turn arrive as one turn carrying ten, not ten turns.
    #[test]
    fn ten_answers_queued_during_one_turn_arrive_as_one_turn_carrying_ten() {
        let s = store();
        let c = conversation(&s);
        for n in 0..10 {
            answered_card(&s, &c, &format!("question {n}"), &format!("answer {n}"));
        }
        // Nothing while it is busy...
        assert!(s.plan_injection(&c, true).unwrap().speak().is_none());

        let injection = s.plan_injection(&c, false).unwrap().speak().expect("one injection");
        assert_eq!(injection.count(), 10);
        assert_eq!(injection.conversation_id, c);
        for n in 0..10 {
            assert!(
                injection.prompt.contains(&format!("answer {n}")),
                "answer {n} must be in the one prompt"
            );
        }
        assert!(
            injection.prompt.find("answer 0") < injection.prompt.find("answer 9"),
            "the batch keeps the order they were answered in"
        );
    }

    /// One road, not two: a card answer and a message from another agent are
    /// both "something to say to this session", and they arrive together.
    #[test]
    fn a_card_answer_and_agent_mail_travel_in_the_same_turn() {
        let s = store();
        let c = conversation(&s);
        answered_card(&s, &c, "chat DB", "sqlite");
        s.enqueue_delivery(&c, Kind::Mail, "9", "from lead: hurry up")
            .unwrap();

        let injection = s.plan_injection(&c, false).unwrap().speak().expect("one injection");
        assert_eq!(injection.count(), 2);
        assert!(injection.prompt.contains("chat DB"));
        assert!(injection.prompt.contains("hurry up"));
    }

    /// The case most likely to be got wrong: a queue holding both sources at
    /// once. Five items become one turn, in the order they were queued, and
    /// each block says where it came from — otherwise the agent answers the
    /// peer's question as though Reljod had already settled it.
    #[test]
    fn a_mixed_queue_becomes_one_turn_in_which_every_block_names_its_source() {
        let s = store();
        let c = conversation(&s);
        answered_card(&s, &c, "chat DB", "sqlite");
        s.enqueue_delivery(&c, Kind::Mail, "9", "from scout: which port do you want")
            .unwrap();
        answered_card(&s, &c, "retry budget", "three attempts");
        s.enqueue_delivery(&c, Kind::Human, "", "stop and show me the diff")
            .unwrap();
        answered_card(&s, &c, "log format", "json lines");

        let injection = s.plan_injection(&c, false).unwrap().speak().expect("one injection");
        assert_eq!(
            injection.count(),
            5,
            "one turn carrying five, not five turns"
        );

        let answers = injection.prompt.matches(Kind::CardAnswer.label()).count();
        assert_eq!(answers, 3, "each answer framed as an answer");
        assert_eq!(injection.prompt.matches(Kind::Mail.label()).count(), 1);
        assert_eq!(injection.prompt.matches(Kind::Human.label()).count(), 1);

        // Queue order, regardless of source.
        let at = |needle: &str| injection.prompt.find(needle).expect("in the prompt");
        assert!(at("chat DB") < at("which port"));
        assert!(at("which port") < at("retry budget"));
        assert!(at("retry budget") < at("stop and show me"));
        assert!(at("stop and show me") < at("log format"));
    }

    /// Three sources, three frames. If any two shared a label the agent could
    /// not tell a settled decision from a peer still asking.
    #[test]
    fn every_kind_frames_its_block_differently() {
        let labels: Vec<&str> = [Kind::CardAnswer, Kind::Mail, Kind::Human]
            .iter()
            .map(Kind::label)
            .collect();
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
    }

    #[test]
    fn each_block_is_its_kinds_label_then_the_body_it_was_queued_with() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "1", "from lead: first")
            .unwrap();
        s.enqueue_delivery(&c, Kind::Human, "", "second").unwrap();

        let injection = s.plan_injection(&c, false).unwrap().speak().unwrap();
        assert!(
            injection.prompt.starts_with(
                "[message from another agent]\nfrom lead: first\n\n[message from Reljod]\nsecond"
            ),
            "{}",
            injection.prompt
        );
    }

    /// A turn carrying mail says how to answer it; a turn carrying only an
    /// answer does not.
    ///
    /// The reminder exists because a session's framing is several turns back by
    /// the time mail arrives, and an agent does not reliably reach for a verb
    /// it was told about once at the start — measured, in a real cross-harness
    /// run, as an answerer replying in prose and never touching the bus.
    ///
    /// Both halves are asserted. Adding it everywhere would be its own noise: a
    /// card answer needs no verb, because the agent asked a question and is
    /// being told the answer, which it acts on by carrying on.
    #[test]
    fn a_turn_carrying_mail_says_how_to_answer_it_and_one_carrying_an_answer_does_not() {
        let s = store();

        let with_mail = conversation(&s);
        s.enqueue_delivery(&with_mail, Kind::Mail, "1", "which port?")
            .unwrap();
        let mail = s.plan_injection(&with_mail, false).unwrap().speak().unwrap();
        assert!(
            mail.prompt.contains("call `reply`"),
            "mail arrived with no way to answer it: {}",
            mail.prompt
        );

        let with_answer = conversation(&s);
        s.enqueue_delivery(&with_answer, Kind::CardAnswer, "2", "use SQLite")
            .unwrap();
        let answer = s.plan_injection(&with_answer, false).unwrap().speak().unwrap();
        assert!(
            !answer.prompt.contains("call `reply`"),
            "an answer to the agent's own question told it to reply to somebody: {}",
            answer.prompt
        );
    }

    /// A batch of one is one framed block — no count, nothing wrapped around
    /// the turn, so it reads like a message delivered on its own.
    #[test]
    fn a_batch_of_one_is_that_item_and_nothing_else() {
        let s = store();
        let c = conversation(&s);
        let queued = s
            .enqueue_delivery(&c, Kind::Human, "", "stop and show me")
            .unwrap();

        let injection = s.plan_injection(&c, false).unwrap().speak().unwrap();
        assert_eq!(
            injection.prompt,
            format!("{}\n{}", Kind::Human.label(), queued.body)
        );
        assert_eq!(render_injection(&[]), "");
    }

    /// The frame comes from the kind, so an item queued as plain text is still
    /// announced. A caller that forgets to label its own body cannot produce a
    /// block the agent will read as its own instruction.
    #[test]
    fn a_body_with_no_label_of_its_own_is_still_framed() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "4", "which port do you want")
            .unwrap();

        let injection = s.plan_injection(&c, false).unwrap().speak().unwrap();
        assert!(injection.prompt.starts_with(Kind::Mail.label()));
        assert!(injection.prompt.contains("which port do you want"));
    }

    // ---- settling ------------------------------------------------------

    #[test]
    fn marking_delivered_empties_the_queue_and_records_the_run() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "1", "one").unwrap();
        s.enqueue_delivery(&c, Kind::Mail, "2", "two").unwrap();
        let injection = s.plan_injection(&c, false).unwrap().speak().unwrap();
        let ids: Vec<i64> = injection.items.iter().map(|p| p.id).collect();

        s.mark_deliveries_delivered(&ids, Some("run-7")).unwrap();

        assert!(s.pending_for(&c).unwrap().is_empty());
        assert!(s.plan_injection(&c, false).unwrap().speak().is_none());
        let settled = s.write(|tx| require_pending(tx, ids[0])).unwrap();
        assert_eq!(settled.state, State::Delivered);
        assert_eq!(settled.run_id.as_deref(), Some("run-7"));
        assert!(settled.delivered_at_ms.is_some());
    }

    /// The rail reads the card and the handler reads the queue. If those two
    /// committed separately there is an instant where the agent has been told
    /// and the rail still says *queued* — which is the state a person answers
    /// again.
    #[test]
    fn marking_delivered_flips_the_card_the_rail_is_showing() {
        let s = store();
        let c = conversation(&s);
        let card_id = answered_card(&s, &c, "chat DB", "sqlite");
        assert_eq!(s.card(card_id).unwrap().unwrap().delivery, Delivery::Queued);

        let ids: Vec<i64> = s.pending_for(&c).unwrap().iter().map(|p| p.id).collect();
        s.mark_deliveries_delivered(&ids, Some("run-7")).unwrap();

        let card = s.card(card_id).unwrap().unwrap();
        assert_eq!(card.delivery, Delivery::Delivered);
        assert!(card.delivered_at_ms.is_some());
    }

    /// A supervisor that crashed between the spawn and the mark is retried.
    /// Replaying must not restamp the row with a later run, and must not mark a
    /// card delivered twice.
    #[test]
    fn settling_an_already_settled_item_changes_nothing() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "1", "one").unwrap();
        let ids: Vec<i64> = s.pending_for(&c).unwrap().iter().map(|p| p.id).collect();

        s.mark_deliveries_delivered(&ids, Some("run-7")).unwrap();
        let first = s.write(|tx| require_pending(tx, ids[0])).unwrap();
        s.mark_deliveries_delivered(&ids, Some("run-8")).unwrap();
        let again = s.write(|tx| require_pending(tx, ids[0])).unwrap();

        assert_eq!(again, first);
        assert_eq!(again.run_id.as_deref(), Some("run-7"));
    }

    /// Mail that vanishes is worse than mail that fails: the row and the card
    /// both keep the fact that nobody ever heard this.
    #[test]
    fn an_undeliverable_item_keeps_its_row_its_reason_and_its_card() {
        let s = store();
        let c = conversation(&s);
        let card_id = answered_card(&s, &c, "chat DB", "sqlite");
        let ids: Vec<i64> = s.pending_for(&c).unwrap().iter().map(|p| p.id).collect();

        s.mark_deliveries_undeliverable(&ids, "the session ended first")
            .unwrap();

        assert!(s.pending_for(&c).unwrap().is_empty());
        let row = s.write(|tx| require_pending(tx, ids[0])).unwrap();
        assert_eq!(row.state, State::Undeliverable);
        assert_eq!(row.detail.as_deref(), Some("the session ended first"));
        assert_eq!(row.delivered_at_ms, None);
        assert_eq!(
            s.card(card_id).unwrap().unwrap().delivery,
            Delivery::Undeliverable
        );
    }

    #[test]
    fn settling_an_empty_list_is_a_no_op() {
        let s = store();
        let c = conversation(&s);
        s.enqueue_delivery(&c, Kind::Mail, "1", "one").unwrap();

        s.mark_deliveries_delivered(&[], Some("run-7")).unwrap();
        s.mark_deliveries_undeliverable(&[], "no reason").unwrap();

        assert_eq!(s.pending_for(&c).unwrap().len(), 1);
    }

    /// `ref_id` numbers itself independently per source, so a message id that
    /// happens to equal a card id must not settle that card.
    #[test]
    fn mail_never_touches_a_card_even_when_its_ref_id_looks_like_one() {
        let s = store();
        let c = conversation(&s);
        let card_id = answered_card(&s, &c, "chat DB", "sqlite");
        let mail = s
            .enqueue_delivery(&c, Kind::Mail, &card_id.to_string(), "unrelated mail")
            .unwrap();

        s.mark_deliveries_delivered(&[mail.id], Some("run-7"))
            .unwrap();

        assert_eq!(
            s.card(card_id).unwrap().unwrap().delivery,
            Delivery::Queued,
            "the card's own answer is still waiting"
        );
    }

    // ---- the vocabulary ------------------------------------------------

    #[test]
    fn every_delivery_enum_survives_a_round_trip_through_text() {
        for kind in [Kind::CardAnswer, Kind::Mail, Kind::Human] {
            assert_eq!(Kind::parse(kind.as_str()), kind);
        }
        for state in [State::Queued, State::Delivered, State::Undeliverable] {
            assert_eq!(State::parse(state.as_str()), state);
        }
    }

    /// A row written by a newer Jod reads as the safe end of each scale rather
    /// than failing: still queued, still something to say.
    #[test]
    fn an_unknown_kind_or_state_reads_as_the_conservative_one() {
        assert_eq!(Kind::parse("telepathy"), Kind::CardAnswer);
        assert_eq!(State::parse("posted"), State::Queued);
    }
}
