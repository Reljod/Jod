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

use serde::{Deserialize, Serialize};

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
}

/// Where a queued item has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Delivered,
    /// The session ended before it could be told. Recorded rather than
    /// deleted, because "it never arrived" is the answer to a question
    /// somebody will ask later.
    Undeliverable,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Delivered => "delivered",
            State::Undeliverable => "undeliverable",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "delivered" => State::Delivered,
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
