//! The left rail's contents: what an agent decided, what it wants to ask, and
//! what credential it is missing.
//!
//! This module owns the *shape* of a card. Three separate consumers read it —
//! the terminal rail, the CLI, and Jod's MCP tools — and the whole reason they
//! share one type and one query builder is that a card answered on a phone and
//! a card answered in the terminal must be the same card, sorted the same way.
//!
//! ## Nothing here blocks a run
//!
//! Raising a card is a write and a return. Answering one is a write and a
//! *queue* — see [`crate::delivery`]. A turn already in flight had its prompt
//! assembled before the answer existed, so splicing one in mid-turn produces
//! either a silent no-op or a double action. The rail therefore has two
//! independent facts about every card: what the human did ([`Status`]) and
//! whether the agent has heard about it yet ([`Delivery`]).

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::delivery::{self, Kind};
use crate::error::{JodError, Result};
use crate::store::{fts_query, Store};

/// What a card is asking of the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// The agent already chose, and is telling you so you can overrule it.
    /// Carries the alternatives it chose between, which is what makes
    /// "switch it out" a keystroke rather than a conversation.
    Decision,
    /// The agent wants an answer it cannot derive.
    Question,
    /// The agent needs a credential. The *value* never passes through this
    /// type — see [`crate::secrets`].
    Secret,
}

impl CardKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CardKind::Decision => "decision",
            CardKind::Question => "question",
            CardKind::Secret => "secret",
        }
    }

    /// Unknown text becomes a question rather than failing.
    ///
    /// A card that cannot be parsed is still a card somebody needs to see, and
    /// the kind only decides its colour. Refusing to load it would hide the
    /// message to protect a label.
    pub fn parse(s: &str) -> CardKind {
        match s {
            "decision" => CardKind::Decision,
            "secret" => CardKind::Secret,
            _ => CardKind::Question,
        }
    }
}

/// How much the agent thinks this matters.
///
/// Deliberately *not* the same axis as [`Card::blocking`]. Importance is a
/// judgement about consequence; blocking is a fact about whether a run can
/// continue. A trivial question can block, and a weighty decision usually
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Low,
    Normal,
    High,
}

impl Importance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Importance::Low => "low",
            Importance::Normal => "normal",
            Importance::High => "high",
        }
    }

    pub fn parse(s: &str) -> Importance {
        match s {
            "low" => Importance::Low,
            "high" => Importance::High,
            _ => Importance::Normal,
        }
    }

    /// Sort weight, highest first. Named rather than derived so the rail's
    /// ordering is one decision in one place.
    pub fn rank(&self) -> u8 {
        match self {
            Importance::High => 0,
            Importance::Normal => 1,
            Importance::Low => 2,
        }
    }
}

/// What the human has done about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Answered,
    /// Read and deliberately not answered. Distinct from answered, because a
    /// dismissed question must not be delivered to the agent as though it had
    /// been given an answer.
    Dismissed,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Answered => "answered",
            Status::Dismissed => "dismissed",
        }
    }

    pub fn parse(s: &str) -> Status {
        match s {
            "answered" => Status::Answered,
            "dismissed" => Status::Dismissed,
            _ => Status::Open,
        }
    }
}

/// Whether the answer has reached the agent.
///
/// The rail shows this because an answer is asynchronous and pretending
/// otherwise would be a lie the user acts on. Answer ten cards while a turn is
/// running and all ten sit at [`Delivery::Queued`] until it comes up for air.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Nothing to deliver — the card is unanswered, or was dismissed.
    None,
    /// Answered, waiting for a safe moment to be injected.
    Queued,
    /// In a prompt the agent actually received.
    Delivered,
    /// The session ended before it could be told. Reported rather than
    /// dropped: mail that vanishes is worse than mail that fails.
    Undeliverable,
}

impl Delivery {
    pub fn as_str(&self) -> &'static str {
        match self {
            Delivery::None => "none",
            Delivery::Queued => "queued",
            Delivery::Delivered => "delivered",
            Delivery::Undeliverable => "undeliverable",
        }
    }

    pub fn parse(s: &str) -> Delivery {
        match s {
            "queued" => Delivery::Queued,
            "delivered" => Delivery::Delivered,
            "undeliverable" => Delivery::Undeliverable,
            _ => Delivery::None,
        }
    }
}

/// Where a card came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The agent called Jod's MCP tool. The supported path, identical on every
    /// harness.
    Mcp,
    /// Jod recognised the harness's own question in its output stream. The
    /// fallback for a run launched without Jod's MCP server, de-duplicated
    /// against the tool path so a harness that does both produces one card.
    Lifted,
    /// Jod itself raised it, about the run rather than for it.
    ///
    /// Distinct from both of the above because the agent did not participate:
    /// it neither called a tool nor said anything Jod lifted. It is Jod
    /// noticing something the run could not — that every file it wrote landed
    /// outside the directories it was given, say — and the reader deserves to
    /// know the observation is Jod's rather than the agent's.
    Jod,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Mcp => "mcp",
            Source::Lifted => "lifted",
            Source::Jod => "jod",
        }
    }

    pub fn parse(s: &str) -> Source {
        match s {
            "lifted" => Source::Lifted,
            "jod" => Source::Jod,
            _ => Source::Mcp,
        }
    }
}

/// One row in the rail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: i64,
    pub conversation_id: String,
    /// Denormalised so the orchestrator's cascading query is one index scan,
    /// and so a card keeps its colour after its session is gone.
    pub work_id: Option<String>,
    /// Which run raised it — the first thing a blocking card provokes somebody
    /// to ask.
    pub run_id: Option<String>,
    pub kind: CardKind,
    pub importance: Importance,
    /// The agent said it cannot proceed past this. Gets the coloured border
    /// and the word `blocked`.
    pub blocking: bool,
    pub status: Status,
    pub delivery: Delivery,
    pub title: String,
    pub body: String,
    /// The alternatives a decision chose between, or the options a question
    /// offers. Answerable by digit in the rail.
    pub options: Vec<String>,
    pub chosen: Option<String>,
    pub answer: Option<String>,
    /// `kind == Secret` only: the environment variable's name. Never a value.
    pub secret_name: Option<String>,
    pub secret_scope: Option<String>,
    pub source: Source,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub answered_at_ms: Option<i64>,
    pub delivered_at_ms: Option<i64>,
    /// What this card is *about*, structurally, for the emitters that need to
    /// act on an answer rather than merely deliver it — see
    /// [`crate::approvals::CARD_KEY`]. De-duplication was its first job and is
    /// no longer its only one, which is why it is readable here and not just
    /// write-only on [`NewCard`].
    pub dedupe_key: Option<String>,
}

impl Card {
    /// Still wants something from a human.
    pub fn is_open(&self) -> bool {
        self.status == Status::Open
    }

    /// Answered but not yet in front of the agent. The state the rail must be
    /// able to render, because it is the ordinary one for a busy session.
    pub fn is_waiting_to_deliver(&self) -> bool {
        self.delivery == Delivery::Queued
    }

    /// How an answer is put to the agent that raised the card.
    ///
    /// Rendered at answer time and stored on the queued row, never re-rendered
    /// at delivery: what the human answered is a fact about the moment they
    /// answered it.
    ///
    /// This is the *content* of one queued item; the line that says an answer
    /// is what this is belongs to [`crate::delivery::Kind::label`], which frames
    /// every item in a batch the same way whatever its source. So the body
    /// starts with the card's own identity instead: the agent may have raised
    /// several, and on a resumed session may have compacted away the turn it
    /// raised them in, so a bare "sqlite" arriving with no anchor is an answer
    /// to nothing.
    ///
    /// Never carries a credential: a secret card's `answer` holds a
    /// confirmation, and the value lives outside this database entirely.
    ///
    /// Use [`Card::answer_body_over`] wherever the card's own earlier choice is
    /// still to hand. This spelling cannot tell a human confirming the agent's
    /// decision from a human replacing it, because by the time it is called the
    /// one field that held both has held only one of them.
    pub fn answer_body(&self) -> String {
        self.answer_body_over(None)
    }

    /// The same, told against what the agent itself had already chosen.
    ///
    /// **The distinction this exists for is the whole of the overrule path.**
    /// A [`CardKind::Decision`] is raised *after* the agent has acted: it says
    /// "I picked one engineer and started them" and offers the alternatives so
    /// the choice can be taken back. Answering it writes the human's choice
    /// into the same `chosen` field, so the delivered text read
    /// `chosen: 2 engineers` whether Reljod had switched the decision or agreed
    /// with it — two opposite events spelled identically. An agent reading the
    /// agreeing one should carry on; an agent reading the other has to undo
    /// something. It could not tell which it had.
    ///
    /// `agent_chose` is what `chosen` held before the answer overwrote it.
    /// `None` means the card carried no choice of its own — every question and
    /// every secret — and those read exactly as they did before.
    ///
    /// Only a *named* alternative counts as an overrule. A decision answered in
    /// prose, with no option picked, leaves the agent's choice standing and is
    /// delivered as an ordinary answer: Jod cannot read a sentence and decide
    /// it contradicts a decision, and guessing wrong here would send an agent
    /// to undo work nobody objected to.
    ///
    /// The instruction to reconcile is in the body rather than in
    /// [`crate::delivery::protocol_for`], where the rest of Jod's per-turn
    /// prompting lives, because that function frames a whole *batch* and this
    /// is true of one item in it. One turn routinely carries an overrule beside
    /// four answers that changed nothing, and a batch-level "undo what you
    /// started" would be wrong about four of the five.
    pub fn answer_body_over(&self, agent_chose: Option<&str>) -> String {
        let mut out = format!("card #{} — {}", self.id, self.title);
        let overruled = self.overrules(agent_chose);
        if let Some(was) = overruled {
            out.push_str(&format!("\nyou chose: {was}"));
        }
        if let Some(chosen) = &self.chosen {
            out.push_str(&format!("\nchosen: {chosen}"));
        }
        if let Some(answer) = &self.answer {
            out.push_str(&format!("\nanswer: {answer}"));
        }
        if overruled.is_some() {
            out.push_str(
                "\n\nReljod overruled you. Whatever you set in motion on the strength of your \
                 own choice is now the wrong thing to be doing, so reconcile it before you \
                 carry on: stop or redirect what is doing the old thing, and start whatever \
                 the new answer asks for. Answering in prose changes nothing — the work is \
                 already running.",
            );
        }
        out
    }

    /// What the agent had chosen, when the human replaced it with something
    /// else. `None` when the two agree, or when there was nothing to replace.
    ///
    /// Compared on trimmed text rather than on identity, because the two ways
    /// of answering a decision do not produce the same string: the rail answers
    /// by pressing the digit of an option, and the command line answers by
    /// typing one out. "sqlite" and "sqlite " are one decision, and a card that
    /// claimed to have been overruled by its own choice would send an agent off
    /// to undo work nobody objected to.
    pub fn overrules<'a>(&self, agent_chose: Option<&'a str>) -> Option<&'a str> {
        let was = agent_chose.map(str::trim).filter(|s| !s.is_empty())?;
        let now = self.chosen.as_deref().map(str::trim)?;
        (was != now).then_some(was)
    }
}

/// How the rail, the CLI and the MCP tool ask for cards.
///
/// One filter type rather than three query functions, so a sort order added
/// for the terminal is automatically available to `jod card ls` and cannot
/// drift out of step with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Cards raised by this conversation.
    pub conversation_id: Option<String>,
    /// Cards raised by this conversation *or any of its descendants*. This is
    /// what makes the orchestrator's rail show the whole fleet's questions.
    /// Cascade is upward only — a child never sees its parent's cards.
    pub subtree_of: Option<String>,
    pub work_id: Option<String>,
    pub kind: Option<CardKind>,
    /// Unset means "open only", which is the rail's default: answered cards
    /// leave the stack and come back on a toggle.
    pub status: Option<Status>,
    pub blocking_only: bool,
    /// Full-text match over title, body and answer.
    pub text: Option<String>,
    pub sort: Sort,
    pub limit: Option<u32>,
}

/// The rail's orderings. Reljod asked for importance and both timestamps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// Blocking first, then importance, then newest. The default, because the
    /// thing that stopped a run outranks the thing that merely matters.
    #[default]
    Pressing,
    Created,
    Updated,
    Importance,
}

impl Sort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sort::Pressing => "pressing",
            Sort::Created => "created",
            Sort::Updated => "updated",
            Sort::Importance => "importance",
        }
    }

    /// Cycled by a key in the rail and named on the command line, so the two
    /// agree on what the orders are called.
    pub const ALL: &'static [Sort] = &[
        Sort::Pressing,
        Sort::Importance,
        Sort::Created,
        Sort::Updated,
    ];
}

/// A card being raised, before it has an id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewCard {
    pub conversation_id: String,
    pub work_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: Option<CardKind>,
    pub importance: Option<Importance>,
    pub blocking: bool,
    pub title: String,
    pub body: String,
    pub options: Vec<String>,
    pub chosen: Option<String>,
    pub secret_name: Option<String>,
    pub secret_scope: Option<String>,
    pub source: Option<Source>,
    /// What the MCP path and the lifted path agree on, so a harness doing both
    /// produces one card. `None` disables de-duplication for this card.
    pub dedupe_key: Option<String>,
}

// ---- the store ---------------------------------------------------------

impl Store {
    /// Raise a card, or hand back the one that already says this.
    ///
    /// De-duplication is the whole reason this is not a plain insert. A harness
    /// that both calls Jod's MCP tool *and* prints its own question produces two
    /// emissions of one question — the tool path and the passive lifter — and
    /// two cards in the rail for one decision is worse than none, because
    /// answering one leaves the other open forever. So a repeat under the same
    /// `(conversation_id, dedupe_key)` returns the existing card rather than
    /// erroring: the caller wanted a card for this question and there is one.
    ///
    /// The read and the insert share one immediate transaction, so the MCP path
    /// and the lifter racing on the same key cannot both get past the check.
    /// `ux_cards_dedupe` is the backstop under that.
    pub fn raise_card(&self, new: NewCard) -> Result<Card> {
        let title = new.title.trim().to_string();
        if title.is_empty() {
            return Err(JodError::Invalid(
                "a card needs a title: the rail's collapsed row is title-only, and a blank one is unanswerable".into(),
            ));
        }
        let options = serde_json::to_string(&new.options)?;
        let kind = new.kind.unwrap_or(CardKind::Question);
        let importance = new.importance.unwrap_or(Importance::Normal);
        let source = new.source.unwrap_or(Source::Mcp);
        let at = now_ms();

        self.write(|tx| {
            // Named rather than left to the foreign key: a card raised against a
            // conversation that has gone is a wiring bug in the emitter, and
            // `FOREIGN KEY constraint failed` does not say which id was wrong.
            let known: Option<String> = tx
                .query_row(
                    "SELECT id FROM conversations WHERE id = ?1",
                    params![new.conversation_id],
                    |r| r.get(0),
                )
                .optional()?;
            if known.is_none() {
                return Err(JodError::Invalid(format!(
                    "no conversation `{}` to raise a card against",
                    new.conversation_id
                )));
            }

            if let Some(key) = &new.dedupe_key {
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM cards WHERE conversation_id = ?1 AND dedupe_key = ?2",
                        params![new.conversation_id, key],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = existing {
                    return require_card(tx, id);
                }
            }

            tx.execute(
                "INSERT INTO cards
                   (conversation_id, work_id, run_id, kind, importance, blocking, status,
                    delivery, title, body, options, chosen, secret_name, secret_scope,
                    source, dedupe_key, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', 'none', ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?15)",
                params![
                    new.conversation_id,
                    new.work_id,
                    new.run_id,
                    kind.as_str(),
                    importance.as_str(),
                    new.blocking as i64,
                    title,
                    new.body,
                    options,
                    new.chosen,
                    new.secret_name,
                    new.secret_scope,
                    source.as_str(),
                    new.dedupe_key,
                    at,
                ],
            )?;
            require_card(tx, tx.last_insert_rowid())
        })
    }

    pub fn card(&self, id: i64) -> Result<Option<Card>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        read_card(&conn, id)
    }

    /// The one query the rail, the CLI and the MCP tool all go through.
    ///
    /// Deliberately one function taking a filter rather than a query per
    /// caller. The three surfaces answer the *same* cards, and the moment they
    /// have their own SQL they drift: a sort added for the terminal is missing
    /// from `jod card ls`, a status filter is subtly different over MCP, and the
    /// card you answered on your phone is not the card you were looking at. So
    /// the SQL is assembled here from [`Query`] and nowhere else.
    pub fn cards(&self, q: &Query) -> Result<Vec<Card>> {
        // Text with nothing indexable in it matches nothing, rather than
        // quietly degrading into "everything" — the call `search_messages`
        // already makes, and the one a filter box wants: typing `??` should
        // empty the rail, not fill it.
        let expr = match q.text.as_deref() {
            Some(text) => match fts_query(text) {
                Some(expr) => Some(expr),
                None => return Ok(vec![]),
            },
            None => None,
        };

        let mut args: Vec<Value> = Vec::new();
        let mut sql = String::new();
        let mut wheres: Vec<String> = Vec::new();

        if let Some(root) = &q.subtree_of {
            args.push(Value::Text(root.clone()));
            sql.push_str(&subtree_cte(args.len()));
            wheres.push("c.conversation_id IN (SELECT id FROM subtree)".into());
        }

        sql.push_str(&format!("SELECT {CARD_COLUMNS} FROM cards c"));
        if let Some(expr) = expr {
            // Through the index, not a LIKE scan: this runs on every keystroke
            // of the rail's filter box.
            sql.push_str(" JOIN cards_fts ON cards_fts.rowid = c.id");
            args.push(Value::Text(expr));
            wheres.push(format!("cards_fts MATCH ?{}", args.len()));
        }
        if let Some(conversation_id) = &q.conversation_id {
            args.push(Value::Text(conversation_id.clone()));
            wheres.push(format!("c.conversation_id = ?{}", args.len()));
        }
        if let Some(work_id) = &q.work_id {
            args.push(Value::Text(work_id.clone()));
            wheres.push(format!("c.work_id = ?{}", args.len()));
        }
        if let Some(kind) = q.kind {
            args.push(Value::Text(kind.as_str().into()));
            wheres.push(format!("c.kind = ?{}", args.len()));
        }
        // An unset status means open, which is the rail's resting state:
        // answering a card takes it out of the stack, and the toggle that brings
        // answered ones back asks for them by name.
        args.push(Value::Text(
            q.status.unwrap_or(Status::Open).as_str().into(),
        ));
        wheres.push(format!("c.status = ?{}", args.len()));
        if q.blocking_only {
            wheres.push("c.blocking = 1".into());
        }

        if !wheres.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        sql.push_str(&format!(" ORDER BY {}", order_by(q.sort)));
        if let Some(limit) = q.limit {
            args.push(Value::Integer(limit as i64));
            sql.push_str(&format!(" LIMIT ?{}", args.len()));
        }

        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_card)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Answer a card: record what the human said, and *queue* it.
    ///
    /// The queue is the point. A turn's prompt is assembled once, at spawn, so
    /// an answer spliced into a running turn arrives in a context assembled
    /// before it existed — the agent either ignores it or acts on it twice.
    /// Both are worse than waiting. See [`crate::delivery`].
    ///
    /// The update and the queued row are written in one transaction, because
    /// the state this must never reach is an answered card with nothing waiting
    /// to carry it: the rail would show *queued* forever and the agent would
    /// never be told.
    ///
    /// Answering twice is refused rather than queued twice. A second delivery
    /// of the same decision reads to the agent as a second instruction, and the
    /// work gets done again.
    ///
    /// **An overrule is queued as an overrule.** Answering a decision with a
    /// different option than the agent picked is the one case where the agent
    /// has to undo something rather than carry on, and the queued row says so
    /// in as many words — see [`Card::answer_body_over`].
    ///
    /// For a secret card, what is passed here is a confirmation — the value
    /// goes to the secret store and never through this function. Nothing in
    /// this database ever holds one.
    pub fn answer_card(&self, id: i64, chosen: Option<&str>, answer: Option<&str>) -> Result<Card> {
        let chosen = chosen.map(str::trim).filter(|s| !s.is_empty());
        let answer = answer.map(str::trim).filter(|s| !s.is_empty());
        if chosen.is_none() && answer.is_none() {
            return Err(JodError::Invalid(format!(
                "card `{id}` needs a chosen option or some text: an empty answer would wake the agent to tell it nothing"
            )));
        }
        let at = now_ms();

        self.write(|tx| {
            let card = require_card(tx, id)?;
            if card.status != Status::Open {
                return Err(JodError::Invalid(format!(
                    "card `{id}` is already {} and cannot be answered again",
                    card.status.as_str()
                )));
            }

            // Read before the update overwrites it, because `chosen` holds two
            // different people's answers at two different times: the agent's
            // own decision until this moment, and the human's from here on.
            // Once the row is written there is no way to tell an overrule from
            // an agreement, and those are the two things the agent most needs
            // told apart — see [`Card::answer_body_over`].
            let agent_chose = card.chosen.clone();

            // `coalesce`, not a plain assignment. A decision card arrives with
            // the agent's own choice in `chosen`, and answering one in prose —
            // no option picked, just a sentence — used to overwrite it with
            // null: the rail was left showing a decision that had chosen
            // nothing, and the record of what the agent actually decided was
            // gone. Answering a decision without naming a different option
            // leaves that decision standing, which is what the words on the
            // card already say.
            tx.execute(
                "UPDATE cards
                    SET status = 'answered', delivery = 'queued',
                        chosen = coalesce(?2, chosen), answer = ?3,
                        answered_at_ms = ?4, updated_at_ms = ?4
                  WHERE id = ?1",
                params![id, chosen, answer, at],
            )?;
            let answered = require_card(tx, id)?;
            // An approval answered "always" becomes a standing grant *here*,
            // in the same transaction as the answer.
            //
            // It used to be written by the hook that raised the card, while
            // that hook sat waiting for it — which meant the grant only
            // persisted if somebody answered within the wait. Answer it a
            // minute later, from the rail or a phone, and the hook had already
            // gone: the card said "every session from now on runs it without
            // asking" and no grant existed. The promise on the card is kept by
            // whoever answers it, so it belongs to the answer.
            crate::approvals::grant_from_answer(tx, &answered, at)?;
            delivery::insert_pending(
                tx,
                &answered.conversation_id,
                Kind::CardAnswer,
                &id.to_string(),
                &answered.answer_body_over(agent_chose.as_deref()),
                at,
            )?;
            Ok(answered)
        })
    }

    /// Read, and deliberately not answered.
    ///
    /// `delivery` stays `none` on purpose: nothing is queued, so the agent is
    /// never told anything. A dismissal that reached the agent as a delivery
    /// would be indistinguishable from an answer, and it would act on a
    /// decision nobody made.
    pub fn dismiss_card(&self, id: i64) -> Result<()> {
        let at = now_ms();
        self.write(|tx| {
            let card = require_card(tx, id)?;
            if card.status != Status::Open {
                return Err(JodError::Invalid(format!(
                    "card `{id}` is already {}",
                    card.status.as_str()
                )));
            }
            tx.execute(
                "UPDATE cards SET status = 'dismissed', updated_at_ms = ?2 WHERE id = ?1",
                params![id, at],
            )?;
            Ok(())
        })
    }

    /// `(open, blocking)` for one node of the fleet tree.
    ///
    /// Two numbers rather than one because they answer different questions: the
    /// open count says where the questions are, and the blocking count says
    /// which of them stopped a run. A tree that showed only the total would put
    /// a node holding six idle decisions above one holding a single blocker.
    ///
    /// `subtree` is what the orchestrator's row uses — its own cards plus every
    /// descendant's, cascading upward only.
    pub fn count_open_cards(&self, conversation_id: &str, subtree: bool) -> Result<(usize, usize)> {
        let counts = "SELECT count(*), coalesce(sum(c.blocking), 0) FROM cards c
                       WHERE c.status = 'open'";
        let sql = if subtree {
            format!(
                "{}{counts} AND c.conversation_id IN (SELECT id FROM subtree)",
                subtree_cte(1)
            )
        } else {
            format!("{counts} AND c.conversation_id = ?1")
        };
        let conn = self.conn.lock().expect("store lock poisoned");
        let (open, blocking): (i64, i64) = conn.query_row(&sql, params![conversation_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok((open as usize, blocking as usize))
    }
}

// ---- helpers -----------------------------------------------------------

const CARD_COLUMNS: &str = "c.id, c.conversation_id, c.work_id, c.run_id, c.kind, c.importance,
     c.blocking, c.status, c.delivery, c.title, c.body, c.options, c.chosen, c.answer,
     c.secret_name, c.secret_scope, c.source, c.created_at_ms, c.updated_at_ms,
     c.answered_at_ms, c.delivered_at_ms, c.dedupe_key";

/// Importance in the order a human means it, which is not the order SQLite
/// sorts the text in — alphabetically `high` sorts before `low` before
/// `normal`, so `ix_cards_open`'s `importance` column cannot be trusted to
/// order the rail. The index still earns its keep on the `status, blocking`
/// prefix; this expression is what makes the rest right.
///
/// Mirrors [`Importance::rank`], and the two are in one file so they stay that
/// way.
const IMPORTANCE_RANK: &str =
    "CASE c.importance WHEN 'high' THEN 0 WHEN 'normal' THEN 1 ELSE 2 END";

/// Every conversation at or below `?n`, for the orchestrator's rail.
///
/// The cascade is upward only — a parent sees its descendants' cards and never
/// the reverse — which falls out of walking down from the root rather than up
/// from each card.
///
/// `UNION` rather than `UNION ALL`: a parent edge that somehow closed a cycle
/// would spin here forever, and duplicate ids would multiply every card by the
/// number of paths to it. Cycles are refused when they are written, but a query
/// that hangs the rail is not the place to find out that refusal had a hole.
fn subtree_cte(n: usize) -> String {
    format!(
        "WITH RECURSIVE subtree(id) AS (
           SELECT id FROM conversations WHERE id = ?{n}
           UNION
           SELECT k.id FROM conversations k JOIN subtree ON k.parent_conversation_id = subtree.id
         ) "
    )
}

/// Ties break on `id DESC` in every order, so two cards raised in the same
/// millisecond — which is what a burst of MCP calls looks like — come back in a
/// stable order rather than shuffling between reads of the same rail.
fn order_by(sort: Sort) -> String {
    match sort {
        Sort::Pressing => {
            format!("c.blocking DESC, {IMPORTANCE_RANK}, c.created_at_ms DESC, c.id DESC")
        }
        Sort::Importance => format!("{IMPORTANCE_RANK}, c.created_at_ms DESC, c.id DESC"),
        Sort::Created => "c.created_at_ms DESC, c.id DESC".into(),
        Sort::Updated => "c.updated_at_ms DESC, c.id DESC".into(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn row_to_card(r: &rusqlite::Row) -> rusqlite::Result<Card> {
    Ok(Card {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        work_id: r.get(2)?,
        run_id: r.get(3)?,
        kind: CardKind::parse(&r.get::<_, String>(4)?),
        importance: Importance::parse(&r.get::<_, String>(5)?),
        blocking: r.get::<_, i64>(6)? != 0,
        status: Status::parse(&r.get::<_, String>(7)?),
        delivery: Delivery::parse(&r.get::<_, String>(8)?),
        title: r.get(9)?,
        body: r.get(10)?,
        // Options that no longer parse read back as none rather than failing
        // the query, the same call `row_to_message` makes for a tool payload:
        // one bad row must not cost you the rail around it. The card is still
        // answerable in prose.
        options: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
        chosen: r.get(12)?,
        answer: r.get(13)?,
        secret_name: r.get(14)?,
        secret_scope: r.get(15)?,
        source: Source::parse(&r.get::<_, String>(16)?),
        created_at_ms: r.get(17)?,
        updated_at_ms: r.get(18)?,
        answered_at_ms: r.get(19)?,
        delivered_at_ms: r.get(20)?,
        dedupe_key: r.get(21)?,
    })
}

fn read_card(conn: &Connection, id: i64) -> Result<Option<Card>> {
    let sql = format!("SELECT {CARD_COLUMNS} FROM cards c WHERE c.id = ?1");
    Ok(conn.query_row(&sql, params![id], row_to_card).optional()?)
}

fn require_card(conn: &Connection, id: i64) -> Result<Card> {
    read_card(conn, id)?.ok_or_else(|| JodError::Invalid(format!("no card `{id}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::State;
    use crate::harness::HarnessKind;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn conversation(s: &Store) -> String {
        s.new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .expect("conversation")
            .id
    }

    /// A session another session spawned. `parent_conversation_id` is written
    /// directly because works and delegation land in a later slice; the column
    /// is the whole contract the cascade reads.
    fn child(s: &Store, parent: &str) -> String {
        let id = conversation(s);
        s.write(|tx| {
            tx.execute(
                "UPDATE conversations SET parent_conversation_id = ?2 WHERE id = ?1",
                params![id, parent],
            )?;
            Ok(())
        })
        .expect("link child");
        id
    }

    fn new_card(conversation_id: &str, title: &str) -> NewCard {
        NewCard {
            conversation_id: conversation_id.into(),
            title: title.into(),
            ..NewCard::default()
        }
    }

    /// Timestamps come from the wall clock, so several cards raised inside one
    /// test share a millisecond. A test about ordering says which order it
    /// means rather than hoping the clock ticks between two inserts.
    fn backdate(s: &Store, id: i64, at_ms: i64) {
        s.write(|tx| {
            tx.execute(
                "UPDATE cards SET created_at_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
                params![id, at_ms],
            )?;
            Ok(())
        })
        .expect("backdate");
    }

    fn titles(cards: &[Card]) -> Vec<&str> {
        cards.iter().map(|c| c.title.as_str()).collect()
    }

    // ---- raising -------------------------------------------------------

    #[test]
    fn a_raised_card_comes_back_open_undelivered_and_whole() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                kind: Some(CardKind::Decision),
                importance: Some(Importance::High),
                blocking: true,
                body: "sqlite needs no server".into(),
                options: vec!["sqlite".into(), "postgres".into()],
                chosen: Some("sqlite".into()),
                run_id: Some("run-1".into()),
                work_id: Some("work-1".into()),
                source: Some(Source::Lifted),
                ..new_card(&c, "chat DB")
            })
            .unwrap();

        assert_eq!(card.title, "chat DB");
        assert_eq!(card.body, "sqlite needs no server");
        assert_eq!(card.kind, CardKind::Decision);
        assert_eq!(card.importance, Importance::High);
        assert!(card.blocking);
        assert_eq!(card.status, Status::Open);
        assert_eq!(card.delivery, Delivery::None);
        assert_eq!(card.options, vec!["sqlite", "postgres"]);
        assert_eq!(card.chosen.as_deref(), Some("sqlite"));
        assert_eq!(card.run_id.as_deref(), Some("run-1"));
        assert_eq!(card.work_id.as_deref(), Some("work-1"));
        assert_eq!(card.source, Source::Lifted);
        assert_eq!(card.answered_at_ms, None);
        assert!(card.is_open());
        assert!(!card.is_waiting_to_deliver());
    }

    #[test]
    fn an_unset_kind_importance_and_source_take_the_ordinary_defaults() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "why is the build red")).unwrap();

        assert_eq!(card.kind, CardKind::Question);
        assert_eq!(card.importance, Importance::Normal);
        assert_eq!(card.source, Source::Mcp);
    }

    #[test]
    fn a_card_with_no_options_round_trips_as_an_empty_list() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "no options")).unwrap();
        assert!(card.options.is_empty());
        assert_eq!(
            s.card(card.id).unwrap().unwrap().options,
            Vec::<String>::new()
        );
    }

    #[test]
    fn options_survive_a_round_trip_through_json_including_awkward_text() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                options: vec!["a \"quoted\" one".into(), "one, with a comma".into()],
                ..new_card(&c, "pick")
            })
            .unwrap();
        assert_eq!(
            s.card(card.id).unwrap().unwrap().options,
            vec!["a \"quoted\" one", "one, with a comma"]
        );
    }

    /// The rail's collapsed row is the title. A blank one is a card nobody can
    /// act on, and it would sit in the stack forever.
    #[test]
    fn a_card_with_no_title_is_refused() {
        let s = store();
        let c = conversation(&s);
        assert!(matches!(
            s.raise_card(new_card(&c, "   ")),
            Err(JodError::Invalid(_))
        ));
    }

    #[test]
    fn raising_against_an_unknown_conversation_names_the_id() {
        let s = store();
        let err = s.raise_card(new_card("ghost", "hello")).unwrap_err();
        assert!(
            format!("{err}").contains("ghost"),
            "the refusal should say which conversation: {err}"
        );
    }

    #[test]
    fn card_is_none_for_an_id_that_does_not_exist() {
        assert_eq!(store().card(404).unwrap(), None);
    }

    // ---- de-duplication ------------------------------------------------

    /// The reason `dedupe_key` exists: a harness that both calls Jod's MCP tool
    /// and prints its own question emits one question twice, and two rail cards
    /// for one decision means answering one and leaving the other open forever.
    #[test]
    fn raising_twice_with_one_dedupe_key_returns_the_existing_card() {
        let s = store();
        let c = conversation(&s);
        let first = s
            .raise_card(NewCard {
                dedupe_key: Some("ask:chat-db".into()),
                source: Some(Source::Mcp),
                ..new_card(&c, "chat DB")
            })
            .unwrap();
        let second = s
            .raise_card(NewCard {
                dedupe_key: Some("ask:chat-db".into()),
                source: Some(Source::Lifted),
                ..new_card(&c, "chat DB (printed)")
            })
            .unwrap();

        assert_eq!(second.id, first.id, "the second raise must not mint a card");
        assert_eq!(second.title, "chat DB", "the first card is not rewritten");
        assert_eq!(second.source, Source::Mcp);
        assert_eq!(
            s.cards(&Query {
                conversation_id: Some(c),
                ..Query::default()
            })
            .unwrap()
            .len(),
            1
        );
    }

    /// De-duplication is per conversation: two sessions asking the same
    /// question are two questions, answered by different agents.
    #[test]
    fn two_conversations_may_share_a_dedupe_key() {
        let s = store();
        let (a, b) = (conversation(&s), conversation(&s));
        let first = s
            .raise_card(NewCard {
                dedupe_key: Some("ask:chat-db".into()),
                ..new_card(&a, "chat DB")
            })
            .unwrap();
        let second = s
            .raise_card(NewCard {
                dedupe_key: Some("ask:chat-db".into()),
                ..new_card(&b, "chat DB")
            })
            .unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn cards_without_a_dedupe_key_are_never_merged() {
        let s = store();
        let c = conversation(&s);
        let first = s.raise_card(new_card(&c, "same title")).unwrap();
        let second = s.raise_card(new_card(&c, "same title")).unwrap();
        assert_ne!(first.id, second.id);
    }

    // ---- the query builder ---------------------------------------------

    #[test]
    fn the_default_query_shows_open_cards_only() {
        let s = store();
        let c = conversation(&s);
        let open = s.raise_card(new_card(&c, "still open")).unwrap();
        let answered = s.raise_card(new_card(&c, "answered")).unwrap();
        let dismissed = s.raise_card(new_card(&c, "dismissed")).unwrap();
        s.answer_card(answered.id, None, Some("yes")).unwrap();
        s.dismiss_card(dismissed.id).unwrap();

        let q = Query {
            conversation_id: Some(c),
            ..Query::default()
        };
        let rows = s.cards(&q).unwrap();
        assert_eq!(titles(&rows), vec!["still open"]);
        assert_eq!(rows[0].id, open.id);

        let answered_rows = s
            .cards(&Query {
                status: Some(Status::Answered),
                ..q.clone()
            })
            .unwrap();
        assert_eq!(titles(&answered_rows), vec!["answered"]);

        let dismissed_rows = s
            .cards(&Query {
                status: Some(Status::Dismissed),
                ..q
            })
            .unwrap();
        assert_eq!(titles(&dismissed_rows), vec!["dismissed"]);
    }

    /// Blocking outranks importance, because the thing that stopped a run
    /// outranks the thing that merely matters.
    #[test]
    fn pressing_sorts_blocking_first_then_importance_then_newest() {
        let s = store();
        let c = conversation(&s);
        let low = s
            .raise_card(NewCard {
                importance: Some(Importance::Low),
                ..new_card(&c, "low")
            })
            .unwrap();
        let high = s
            .raise_card(NewCard {
                importance: Some(Importance::High),
                ..new_card(&c, "high")
            })
            .unwrap();
        let normal_old = s.raise_card(new_card(&c, "normal, older")).unwrap();
        let normal_new = s.raise_card(new_card(&c, "normal, newer")).unwrap();
        let blocker = s
            .raise_card(NewCard {
                blocking: true,
                importance: Some(Importance::Low),
                ..new_card(&c, "blocked on a key")
            })
            .unwrap();
        backdate(&s, low.id, 1_000);
        backdate(&s, high.id, 2_000);
        backdate(&s, normal_old.id, 3_000);
        backdate(&s, normal_new.id, 4_000);
        backdate(&s, blocker.id, 500);

        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(
            titles(&rows),
            vec![
                "blocked on a key",
                "high",
                "normal, newer",
                "normal, older",
                "low"
            ]
        );
    }

    /// Importance is a judgement about consequence; blocking is a fact about a
    /// run. Sorting by one must not smuggle in the other.
    #[test]
    fn sorting_by_importance_ignores_whether_a_card_blocks() {
        let s = store();
        let c = conversation(&s);
        let blocking_low = s
            .raise_card(NewCard {
                blocking: true,
                importance: Some(Importance::Low),
                ..new_card(&c, "blocking but trivial")
            })
            .unwrap();
        let high = s
            .raise_card(NewCard {
                importance: Some(Importance::High),
                ..new_card(&c, "weighty")
            })
            .unwrap();
        backdate(&s, blocking_low.id, 2_000);
        backdate(&s, high.id, 1_000);

        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                sort: Sort::Importance,
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["weighty", "blocking but trivial"]);
    }

    #[test]
    fn created_sorts_newest_first_and_updated_follows_the_last_touch() {
        let s = store();
        let c = conversation(&s);
        let old = s.raise_card(new_card(&c, "older")).unwrap();
        let new = s.raise_card(new_card(&c, "newer")).unwrap();
        backdate(&s, old.id, 1_000);
        backdate(&s, new.id, 2_000);

        let q = Query {
            conversation_id: Some(c),
            sort: Sort::Created,
            ..Query::default()
        };
        assert_eq!(titles(&s.cards(&q).unwrap()), vec!["newer", "older"]);

        // Touching the older card moves it to the front of `updated` and
        // leaves `created` where it was.
        s.write(|tx| {
            tx.execute(
                "UPDATE cards SET updated_at_ms = 9000 WHERE id = ?1",
                params![old.id],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(titles(&s.cards(&q).unwrap()), vec!["newer", "older"]);
        assert_eq!(
            titles(
                &s.cards(&Query {
                    sort: Sort::Updated,
                    ..q
                })
                .unwrap()
            ),
            vec!["older", "newer"]
        );
    }

    /// Two cards raised in the same millisecond — what a burst of MCP calls
    /// looks like — must not shuffle between two reads of the same rail.
    #[test]
    fn cards_sharing_a_timestamp_keep_a_stable_order() {
        let s = store();
        let c = conversation(&s);
        for title in ["first", "second", "third"] {
            let card = s.raise_card(new_card(&c, title)).unwrap();
            backdate(&s, card.id, 5_000);
        }
        let q = Query {
            conversation_id: Some(c),
            ..Query::default()
        };
        let once = titles(&s.cards(&q).unwrap()).join(",");
        let twice = titles(&s.cards(&q).unwrap()).join(",");
        assert_eq!(once, "third,second,first");
        assert_eq!(once, twice);
    }

    #[test]
    fn the_kind_work_and_blocking_filters_compose() {
        let s = store();
        let c = conversation(&s);
        s.raise_card(NewCard {
            kind: Some(CardKind::Question),
            blocking: true,
            work_id: Some("w1".into()),
            ..new_card(&c, "wanted")
        })
        .unwrap();
        s.raise_card(NewCard {
            kind: Some(CardKind::Question),
            work_id: Some("w1".into()),
            ..new_card(&c, "not blocking")
        })
        .unwrap();
        s.raise_card(NewCard {
            kind: Some(CardKind::Decision),
            blocking: true,
            work_id: Some("w1".into()),
            ..new_card(&c, "wrong kind")
        })
        .unwrap();
        s.raise_card(NewCard {
            kind: Some(CardKind::Question),
            blocking: true,
            work_id: Some("w2".into()),
            ..new_card(&c, "other work")
        })
        .unwrap();

        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                kind: Some(CardKind::Question),
                work_id: Some("w1".into()),
                blocking_only: true,
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["wanted"]);
    }

    #[test]
    fn limit_caps_the_rows_without_changing_the_order() {
        let s = store();
        let c = conversation(&s);
        for title in ["a", "b", "c"] {
            let card = s.raise_card(new_card(&c, title)).unwrap();
            backdate(&s, card.id, 1_000 + card.id);
        }
        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                sort: Sort::Created,
                limit: Some(2),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["c", "b"]);
    }

    // ---- text search ---------------------------------------------------

    #[test]
    fn text_matches_the_title_and_the_body_through_the_index() {
        let s = store();
        let c = conversation(&s);
        s.raise_card(NewCard {
            body: "no server to run".into(),
            ..new_card(&c, "sqlite for the chat store")
        })
        .unwrap();
        s.raise_card(NewCard {
            body: "sqlite was rejected".into(),
            ..new_card(&c, "queue choice")
        })
        .unwrap();
        s.raise_card(new_card(&c, "unrelated")).unwrap();

        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                text: Some("sqlite".into()),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(titles(&rows).contains(&"queue choice"));
        assert!(titles(&rows).contains(&"sqlite for the chat store"));
    }

    /// The answer is indexed too, which is what makes an answered card findable
    /// months later — the point of keeping them at all.
    #[test]
    fn text_finds_an_answered_card_by_what_was_answered() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "which port")).unwrap();
        s.answer_card(card.id, None, Some("use 8443 everywhere"))
            .unwrap();

        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                status: Some(Status::Answered),
                text: Some("8443".into()),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["which port"]);
    }

    /// Typing punctuation into the filter box empties the rail rather than
    /// filling it, and never becomes an FTS5 syntax error.
    #[test]
    fn text_with_nothing_searchable_in_it_matches_nothing() {
        let s = store();
        let c = conversation(&s);
        s.raise_card(new_card(&c, "anything")).unwrap();
        let rows = s
            .cards(&Query {
                conversation_id: Some(c),
                text: Some("?? \"".into()),
                ..Query::default()
            })
            .unwrap();
        assert!(rows.is_empty());
    }

    // ---- the cascade ---------------------------------------------------

    #[test]
    fn a_parent_sees_every_descendants_cards_three_levels_down() {
        let s = store();
        let root = conversation(&s);
        let mid = child(&s, &root);
        let leaf = child(&s, &mid);
        s.raise_card(new_card(&root, "root card")).unwrap();
        s.raise_card(new_card(&mid, "mid card")).unwrap();
        s.raise_card(new_card(&leaf, "leaf card")).unwrap();

        let from_root = s
            .cards(&Query {
                subtree_of: Some(root.clone()),
                ..Query::default()
            })
            .unwrap();
        let mut seen = titles(&from_root);
        seen.sort_unstable();
        assert_eq!(seen, vec!["leaf card", "mid card", "root card"]);

        let below_mid = s
            .cards(&Query {
                subtree_of: Some(mid),
                ..Query::default()
            })
            .unwrap();
        let mut from_mid = titles(&below_mid);
        from_mid.sort_unstable();
        assert_eq!(from_mid, vec!["leaf card", "mid card"]);
    }

    /// Upward only. A child asking for its own subtree must not be handed its
    /// parent's questions — an answer landing on the wrong agent is worse than
    /// no answer at all.
    #[test]
    fn the_cascade_never_runs_downward() {
        let s = store();
        let root = conversation(&s);
        let mid = child(&s, &root);
        let leaf = child(&s, &mid);
        s.raise_card(new_card(&root, "root card")).unwrap();
        s.raise_card(new_card(&leaf, "leaf card")).unwrap();

        let rows = s
            .cards(&Query {
                subtree_of: Some(leaf),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["leaf card"]);
    }

    #[test]
    fn a_siblings_cards_are_not_in_the_subtree() {
        let s = store();
        let root = conversation(&s);
        let left = child(&s, &root);
        let right = child(&s, &root);
        s.raise_card(new_card(&left, "left card")).unwrap();
        s.raise_card(new_card(&right, "right card")).unwrap();

        let rows = s
            .cards(&Query {
                subtree_of: Some(left),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["left card"]);
    }

    #[test]
    fn a_subtree_query_still_honours_the_other_filters() {
        let s = store();
        let root = conversation(&s);
        let leaf = child(&s, &root);
        s.raise_card(NewCard {
            blocking: true,
            ..new_card(&leaf, "leaf blocker")
        })
        .unwrap();
        s.raise_card(new_card(&leaf, "leaf aside")).unwrap();
        s.raise_card(new_card(&root, "root aside")).unwrap();

        let rows = s
            .cards(&Query {
                subtree_of: Some(root),
                blocking_only: true,
                ..Query::default()
            })
            .unwrap();
        assert_eq!(titles(&rows), vec!["leaf blocker"]);
    }

    // ---- answering and dismissing --------------------------------------

    /// Answering writes the card *and* queues the delivery in one transaction:
    /// an answered card with nothing waiting to carry it would show `queued`
    /// forever and the agent would never be told.
    #[test]
    fn answering_a_card_queues_a_delivery_rather_than_delivering_it() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                options: vec!["sqlite".into(), "postgres".into()],
                ..new_card(&c, "chat DB")
            })
            .unwrap();

        let answered = s
            .answer_card(card.id, Some("sqlite"), Some("keep it simple"))
            .unwrap();
        assert_eq!(answered.status, Status::Answered);
        assert_eq!(answered.delivery, Delivery::Queued);
        assert!(answered.is_waiting_to_deliver());
        assert_eq!(answered.chosen.as_deref(), Some("sqlite"));
        assert_eq!(answered.answer.as_deref(), Some("keep it simple"));
        assert!(answered.answered_at_ms.is_some());
        assert_eq!(answered.delivered_at_ms, None);

        let queued = s.pending_for(&c).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, Kind::CardAnswer);
        assert_eq!(queued[0].ref_id, card.id.to_string());
        assert_eq!(queued[0].state, State::Queued);
        assert!(queued[0].body.contains("chat DB"));
        assert!(queued[0].body.contains("sqlite"));
    }

    /// A second delivery of the same decision reads to the agent as a second
    /// instruction, and the work gets done twice.
    #[test]
    fn answering_an_answered_card_is_refused_rather_than_queued_again() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "chat DB")).unwrap();
        s.answer_card(card.id, None, Some("sqlite")).unwrap();

        let err = s.answer_card(card.id, None, Some("postgres")).unwrap_err();
        assert!(matches!(err, JodError::Invalid(_)), "{err}");
        assert_eq!(s.pending_for(&c).unwrap().len(), 1);
        assert_eq!(
            s.card(card.id).unwrap().unwrap().answer.as_deref(),
            Some("sqlite"),
            "the first answer stands"
        );
    }

    #[test]
    fn answering_with_neither_a_choice_nor_any_text_is_refused() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "chat DB")).unwrap();

        assert!(matches!(
            s.answer_card(card.id, None, None),
            Err(JodError::Invalid(_))
        ));
        assert!(matches!(
            s.answer_card(card.id, Some("  "), Some("\n")),
            Err(JodError::Invalid(_))
        ));
        assert_eq!(s.card(card.id).unwrap().unwrap().status, Status::Open);
        assert!(s.pending_for(&c).unwrap().is_empty());
    }

    #[test]
    fn answering_a_card_that_does_not_exist_is_refused() {
        assert!(matches!(
            store().answer_card(404, None, Some("hello")),
            Err(JodError::Invalid(_))
        ));
    }

    /// A dismissal that reached the agent would be indistinguishable from an
    /// answer, and it would act on a decision nobody made.
    #[test]
    fn a_dismissed_card_queues_nothing_and_keeps_delivery_none() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "not now")).unwrap();
        s.dismiss_card(card.id).unwrap();

        let dismissed = s.card(card.id).unwrap().unwrap();
        assert_eq!(dismissed.status, Status::Dismissed);
        assert_eq!(dismissed.delivery, Delivery::None);
        assert!(!dismissed.is_open());
        assert!(s.pending_for(&c).unwrap().is_empty());
    }

    #[test]
    fn a_dismissed_card_cannot_then_be_answered() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "not now")).unwrap();
        s.dismiss_card(card.id).unwrap();

        assert!(matches!(
            s.answer_card(card.id, None, Some("actually, yes")),
            Err(JodError::Invalid(_))
        ));
        assert!(s.pending_for(&c).unwrap().is_empty());
    }

    #[test]
    fn dismissing_twice_is_refused_rather_than_silently_repeated() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "not now")).unwrap();
        s.dismiss_card(card.id).unwrap();
        assert!(matches!(s.dismiss_card(card.id), Err(JodError::Invalid(_))));
    }

    // ---- the counts the tree draws -------------------------------------

    #[test]
    fn count_open_cards_separates_the_blockers_from_the_rest() {
        let s = store();
        let c = conversation(&s);
        s.raise_card(new_card(&c, "one")).unwrap();
        s.raise_card(NewCard {
            blocking: true,
            ..new_card(&c, "two")
        })
        .unwrap();
        s.raise_card(NewCard {
            blocking: true,
            ..new_card(&c, "three")
        })
        .unwrap();

        assert_eq!(s.count_open_cards(&c, false).unwrap(), (3, 2));
    }

    #[test]
    fn count_open_cards_ignores_answered_and_dismissed_ones() {
        let s = store();
        let c = conversation(&s);
        let answered = s.raise_card(new_card(&c, "answered")).unwrap();
        let dismissed = s.raise_card(new_card(&c, "dismissed")).unwrap();
        s.raise_card(NewCard {
            blocking: true,
            ..new_card(&c, "open")
        })
        .unwrap();
        s.answer_card(answered.id, None, Some("done")).unwrap();
        s.dismiss_card(dismissed.id).unwrap();

        assert_eq!(s.count_open_cards(&c, false).unwrap(), (1, 1));
    }

    #[test]
    fn count_open_cards_covers_the_subtree_only_when_asked() {
        let s = store();
        let root = conversation(&s);
        let mid = child(&s, &root);
        let leaf = child(&s, &mid);
        s.raise_card(new_card(&root, "root card")).unwrap();
        s.raise_card(NewCard {
            blocking: true,
            ..new_card(&leaf, "leaf blocker")
        })
        .unwrap();

        assert_eq!(s.count_open_cards(&root, false).unwrap(), (1, 0));
        assert_eq!(s.count_open_cards(&root, true).unwrap(), (2, 1));
    }

    #[test]
    fn a_conversation_with_no_cards_counts_zero_rather_than_failing() {
        let s = store();
        let c = conversation(&s);
        assert_eq!(s.count_open_cards(&c, false).unwrap(), (0, 0));
        assert_eq!(s.count_open_cards(&c, true).unwrap(), (0, 0));
        assert_eq!(s.count_open_cards("ghost", true).unwrap(), (0, 0));
    }

    // ---- what the agent is told ----------------------------------------

    #[test]
    fn an_answer_body_names_the_card_and_carries_both_halves() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "chat DB")).unwrap();
        let answered = s
            .answer_card(card.id, Some("sqlite"), Some("keep it simple"))
            .unwrap();

        assert_eq!(
            answered.answer_body(),
            format!(
                "card #{} — chat DB\nchosen: sqlite\nanswer: keep it simple",
                card.id
            )
        );
    }

    #[test]
    fn an_answer_body_omits_the_half_that_was_not_given() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "chat DB")).unwrap();
        let answered = s.answer_card(card.id, Some("sqlite"), None).unwrap();

        assert!(answered.answer_body().contains("chosen: sqlite"));
        assert!(!answered.answer_body().contains("answer:"));
    }

    /// The bug Reljod reported, at the level the text is rendered.
    ///
    /// He asked for the work to be split between two engineers instead of one,
    /// and the manager carried on with one. The reason is here: the agent's
    /// choice and the human's live in the same column, so once the answer was
    /// written the delivery said `chosen: 2 engineers` — which is exactly what
    /// it would have said if he had agreed with one engineer and the manager
    /// had never been contradicted at all.
    #[test]
    fn an_overruled_decision_says_it_was_overruled_and_what_it_replaced() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                kind: Some(CardKind::Decision),
                options: vec!["1 engineer".into(), "2 engineers".into()],
                chosen: Some("1 engineer".into()),
                ..new_card(&c, "how to split the cards fix")
            })
            .unwrap();

        s.answer_card(card.id, Some("2 engineers"), None).unwrap();

        let told = &s.pending_for(&c).unwrap()[0].body;
        assert!(
            told.contains("you chose: 1 engineer"),
            "the agent is not told what its own choice was, so it cannot know it changed: \
             {told}"
        );
        assert!(told.contains("chosen: 2 engineers"), "{told}");
        assert!(told.contains("Reljod overruled you"), "{told}");
    }

    /// The other half, and the one that keeps the first honest. Agreeing with
    /// an agent must not read as overruling it: an agent told to undo work
    /// nobody objected to costs a turn and a running engineer.
    #[test]
    fn agreeing_with_a_decision_is_not_delivered_as_an_overrule() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                kind: Some(CardKind::Decision),
                options: vec!["1 engineer".into(), "2 engineers".into()],
                chosen: Some("1 engineer".into()),
                ..new_card(&c, "how to split the cards fix")
            })
            .unwrap();

        // Answered by pressing the digit of the option already in force, which
        // is how the rail confirms one.
        s.answer_card(card.id, Some(" 1 engineer "), None).unwrap();

        let told = &s.pending_for(&c).unwrap()[0].body;
        assert!(!told.contains("overruled"), "{told}");
        assert!(!told.contains("you chose:"), "{told}");
    }

    /// A question was never decided by the agent, so there is nothing of its
    /// own to contradict and nothing to undo. Answering one reads as it always
    /// has.
    #[test]
    fn an_answered_question_is_not_dressed_up_as_an_overrule() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "which port?")).unwrap();

        s.answer_card(card.id, Some("8443"), None).unwrap();

        let told = &s.pending_for(&c).unwrap()[0].body;
        assert_eq!(told, &format!("card #{} — which port?\nchosen: 8443", card.id));
    }

    /// Answering a decision in prose leaves the decision standing rather than
    /// erasing it.
    ///
    /// The update used to write the human's `chosen` unconditionally, so an
    /// answer that named no option wrote null over the agent's own choice: the
    /// rail was left showing a decision that had decided nothing, and what the
    /// agent had actually settled on was gone from the only place it was
    /// recorded. Nothing was overruled here — a sentence is not a different
    /// option — so the card keeps its choice and the delivery carries both.
    #[test]
    fn answering_a_decision_in_prose_does_not_erase_what_the_agent_chose() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                kind: Some(CardKind::Decision),
                options: vec!["1 engineer".into(), "2 engineers".into()],
                chosen: Some("1 engineer".into()),
                ..new_card(&c, "how to split the cards fix")
            })
            .unwrap();

        let answered = s
            .answer_card(card.id, None, Some("fine, but keep an eye on the tests"))
            .unwrap();

        assert_eq!(answered.chosen.as_deref(), Some("1 engineer"));
        let told = &s.pending_for(&c).unwrap()[0].body;
        assert!(told.contains("chosen: 1 engineer"), "{told}");
        assert!(
            told.contains("answer: fine, but keep an eye on the tests"),
            "{told}"
        );
        assert!(!told.contains("overruled"), "{told}");
    }

    // ---- the vocabulary ------------------------------------------------

    #[test]
    fn every_card_enum_survives_a_round_trip_through_text() {
        for kind in [CardKind::Decision, CardKind::Question, CardKind::Secret] {
            assert_eq!(CardKind::parse(kind.as_str()), kind);
        }
        for importance in [Importance::Low, Importance::Normal, Importance::High] {
            assert_eq!(Importance::parse(importance.as_str()), importance);
        }
        for status in [Status::Open, Status::Answered, Status::Dismissed] {
            assert_eq!(Status::parse(status.as_str()), status);
        }
        for delivery in [
            Delivery::None,
            Delivery::Queued,
            Delivery::Delivered,
            Delivery::Undeliverable,
        ] {
            assert_eq!(Delivery::parse(delivery.as_str()), delivery);
        }
        for source in [Source::Mcp, Source::Lifted] {
            assert_eq!(Source::parse(source.as_str()), source);
        }
    }

    /// A row written by a newer Jod must not make the rail unreadable: the kind
    /// only decides a colour, and hiding the message to protect a label is the
    /// wrong trade.
    #[test]
    fn an_unknown_kind_reads_as_a_question_rather_than_failing_the_row() {
        let s = store();
        let c = conversation(&s);
        let card = s.raise_card(new_card(&c, "from the future")).unwrap();
        s.write(|tx| {
            tx.execute(
                "UPDATE cards SET kind = 'telepathy', importance = 'urgent',
                                  status = 'pondering', delivery = 'posted', source = 'osmosis'
                  WHERE id = ?1",
                params![card.id],
            )?;
            Ok(())
        })
        .unwrap();

        let read = s.card(card.id).unwrap().unwrap();
        assert_eq!(read.kind, CardKind::Question);
        assert_eq!(read.importance, Importance::Normal);
        assert_eq!(read.status, Status::Open);
        assert_eq!(read.delivery, Delivery::None);
        assert_eq!(read.source, Source::Mcp);
    }

    /// Options that no longer parse cost the options, not the card: it is still
    /// answerable in prose.
    #[test]
    fn unparseable_options_read_back_as_none_rather_than_failing_the_query() {
        let s = store();
        let c = conversation(&s);
        let card = s
            .raise_card(NewCard {
                options: vec!["one".into()],
                ..new_card(&c, "pick")
            })
            .unwrap();
        s.write(|tx| {
            tx.execute(
                "UPDATE cards SET options = 'not json' WHERE id = ?1",
                params![card.id],
            )?;
            Ok(())
        })
        .unwrap();

        let read = s.card(card.id).unwrap().unwrap();
        assert!(read.options.is_empty());
        assert_eq!(read.title, "pick");
    }

    #[test]
    fn the_sort_names_are_the_ones_the_rail_and_the_cli_cycle_through() {
        assert_eq!(Sort::default(), Sort::Pressing);
        assert_eq!(Sort::ALL.len(), 4);
        for sort in Sort::ALL {
            assert!(!sort.as_str().is_empty());
        }
        assert!(Importance::High.rank() < Importance::Normal.rank());
        assert!(Importance::Normal.rank() < Importance::Low.rank());
    }
}
