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
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::CardAnswer => "card_answer",
            Kind::Mail => "mail",
            Kind::Human => "human",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "mail" => Kind::Mail,
            "human" => Kind::Human,
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
                // The rail has three words, not four: to a person waiting on an
                // answer, "the spawn failed and will be retried" and "it has
                // not gone yet" are the same fact, and a card that said
                // `failed` would invite answering it again. `Failed` is the
                // bus's word for something it will try once more.
                let card_state = match state {
                    State::Delivered => "delivered",
                    State::Undeliverable => "undeliverable",
                    State::Queued | State::Failed => "queued",
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
    /// Returns `None` — deliberately, in each case — when:
    ///
    /// - **A turn is in flight.** This is the rule the whole module exists for.
    ///   The running turn's prompt was assembled before any of this arrived, so
    ///   splicing it in produces an answer to a question the agent has already
    ///   moved past. It waits; nothing is lost.
    /// - **Nothing is queued.** Waking an agent to tell it nothing burns a turn
    ///   and a context window.
    ///
    /// Everything queued goes into **one** injection. Ten cards answered while a
    /// run was mid-turn arrive as one turn carrying ten, not ten turns: cheaper,
    /// and the better answer as well, because an agent reading everything that
    /// changed in one go responds more coherently than one woken ten times with
    /// a line each.
    pub fn plan_injection(&self, conversation_id: &str, busy: bool) -> Result<Option<Injection>> {
        if busy {
            return Ok(None);
        }
        let items = self.pending_for(conversation_id)?;
        if items.is_empty() {
            return Ok(None);
        }
        Ok(Some(Injection {
            conversation_id: conversation_id.to_string(),
            prompt: render_injection(&items),
            items,
        }))
    }
}

// ---- helpers -----------------------------------------------------------

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const PENDING_COLUMNS: &str = "id, conversation_id, kind, ref_id, body, state, run_id, detail,
     queued_at_ms, delivered_at_ms";

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
        assert!(s.plan_injection(&b, false).unwrap().is_none());
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

        assert!(s.plan_injection(&c, true).unwrap().is_none());
        assert_eq!(
            s.pending_for(&c).unwrap().len(),
            1,
            "holding is not dropping: it is still queued"
        );
        assert!(s.plan_injection(&c, false).unwrap().is_some());
    }

    /// Waking an agent to tell it nothing burns a turn and a context window.
    #[test]
    fn an_idle_session_with_an_empty_queue_is_left_alone() {
        let s = store();
        let c = conversation(&s);
        assert!(s.plan_injection(&c, false).unwrap().is_none());
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
        assert!(s.plan_injection(&c, true).unwrap().is_none());

        let injection = s.plan_injection(&c, false).unwrap().expect("one injection");
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

        let injection = s.plan_injection(&c, false).unwrap().expect("one injection");
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

        let injection = s.plan_injection(&c, false).unwrap().expect("one injection");
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

        let injection = s.plan_injection(&c, false).unwrap().unwrap();
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
        let mail = s.plan_injection(&with_mail, false).unwrap().unwrap();
        assert!(
            mail.prompt.contains("call `reply`"),
            "mail arrived with no way to answer it: {}",
            mail.prompt
        );

        let with_answer = conversation(&s);
        s.enqueue_delivery(&with_answer, Kind::CardAnswer, "2", "use SQLite")
            .unwrap();
        let answer = s.plan_injection(&with_answer, false).unwrap().unwrap();
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

        let injection = s.plan_injection(&c, false).unwrap().unwrap();
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

        let injection = s.plan_injection(&c, false).unwrap().unwrap();
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
        let injection = s.plan_injection(&c, false).unwrap().unwrap();
        let ids: Vec<i64> = injection.items.iter().map(|p| p.id).collect();

        s.mark_deliveries_delivered(&ids, Some("run-7")).unwrap();

        assert!(s.pending_for(&c).unwrap().is_empty());
        assert!(s.plan_injection(&c, false).unwrap().is_none());
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
