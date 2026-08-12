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

use serde::{Deserialize, Serialize};

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
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Mcp => "mcp",
            Source::Lifted => "lifted",
        }
    }

    pub fn parse(s: &str) -> Source {
        match s {
            "lifted" => Source::Lifted,
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
    pub const ALL: &'static [Sort] = &[Sort::Pressing, Sort::Importance, Sort::Created, Sort::Updated];
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
