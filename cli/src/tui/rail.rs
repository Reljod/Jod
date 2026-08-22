//! The decision rail's state: what it is showing, what the cursor is on, and
//! which stack of cards is on screen.
//!
//! Everything here is a pure transformation, in the same spirit as
//! [`super::workspace::ListState`] — the rail is the one part of the TUI whose
//! wrong behaviour would be *invisible* rather than merely ugly, so it has to
//! be testable without a terminal.
//!
//! ## The rail shows two facts about every card, never one
//!
//! [`jod_core::cards::Status`] is what the human did; [`Delivery`] is whether
//! the agent has heard about it yet. They are independent, and collapsing them
//! into a single "done" would be a lie the reader acts on: answering a card
//! while a turn is in flight *queues* the answer, and the turn carries on
//! untouched. Reljod can answer ten cards during one turn and all ten sit at
//! `answered, queued` until it comes up for air. See `core/src/delivery.rs` and
//! decision D2.
//!
//! ## Why the rail has a focus rather than a pair of bare cycle keys
//!
//! The chat input owns every bare letter, so a rail verb on `j` would type a
//! `j` into the sentence being written. Two ways out exist and the rail uses
//! both, at different costs:
//!
//! - `Ctrl-N` opens the rail and puts it away again, and `Ctrl-R` shows it and
//!   arms a digit — `Ctrl-R` then `3` accepts card three's recommendation —
//!   without taking the keyboard. Both are chords, so both are safe
//!   mid-sentence — that is the property E2.S3 asks for by name.
//! - `Ctrl-N` also *focuses* the rail, after which the bare keys are the rail's:
//!   `↑↓`/`jk` move, `⏎` expands, a digit answers, `x` dismisses, and `Esc`
//!   closes it with the typed line exactly as it was.
//!
//! `Ctrl-N` used to step to the next card instead of closing, which left the
//! rail with no way out on the key that opened it — the only one was `Ctrl-R`,
//! which the rail's own keybar never printed. Stepping is what `↑↓`/`jk` are
//! for, and they were already there.
//!
//! The focus is what makes answering a card cheap once you are in it, and the
//! chord is what makes getting in free. Neither ever touches `App::input`.

use jod_core::cards::{Card, CardKind, Delivery, Query, Sort, Status};

/// Which stacks the rail can show, in the order `t` walks through them.
///
/// Answered comes second because it is the one people go looking for: a card
/// leaves the open stack the moment it is answered, and "where did that go" is
/// the next question. Dismissed is last because nothing is waiting on it.
const STACKS: [Status; 3] = [Status::Open, Status::Answered, Status::Dismissed];

/// The kind filter's cycle. `None` — every kind — is first, because it is the
/// resting state and the key has to return to it.
const KINDS: [Option<CardKind>; 4] = [
    None,
    Some(CardKind::Decision),
    Some(CardKind::Question),
    Some(CardKind::Secret),
];

/// How many cards the rail asks for at once.
///
/// A cap rather than everything, because the query runs on the tick and the
/// rail is thirty columns of a terminal: nobody scrolls past fifty two-line
/// cards, and the ones that matter are at the top by construction — `Pressing`
/// puts blocking first.
pub const LIMIT: u32 = 50;

/// How many cards the stack draws at once, however many rows it has been given.
///
/// This used to be five, because a card was a bordered box four rows tall and
/// the sixth card cost twenty-four rows to reach. A card is one row now, so the
/// cap that protected the chat from the rail no longer has anything to protect
/// it from: nine cards cost nine rows plus their group headings, and the whole
/// rail usually fits. Nine rather than everything because nine is how many
/// carry a quick-answer digit — past that the rail is a list you scroll, and
/// the cap is what keeps it from becoming one silently.
pub const VISIBLE: usize = 9;

/// How many cards get a quick-answer digit.
///
/// One through nine, because those are the keys. A tenth card is still there,
/// still selectable and still answerable the long way; what it does not get is
/// a number, because there is no key to print beside it.
pub const QUICK: usize = 9;

/// Everything the rail remembers between frames.
///
/// The filter, the sort and both filters survive navigating away and coming
/// back, which E2.S5 asks for by name — they live here rather than being
/// rebuilt from the screen, so there is nowhere for them to be lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailState {
    /// Whether the column is drawn at all.
    pub shown: bool,
    /// Whether bare keys are the rail's rather than the chat's.
    pub focused: bool,
    /// Whether the selected card is shown in full rather than as two lines.
    pub expanded: bool,
    /// How many lines the expanded card has been scrolled down by.
    ///
    /// An expanded card can be taller than the pane holding it — a long body,
    /// nine options and five lines of provenance do not fit in twelve rows at
    /// the bottom of a phone screen — and before this there was no way to read
    /// the part underneath. It belongs to the card under the cursor, so moving
    /// the cursor or collapsing the card puts it back to zero: carrying an
    /// offset onto the next card would open it halfway down.
    pub scroll: u16,
    /// The cursor, held as a **card id** and never as a row index: the rail
    /// re-queries on the tick and re-sorts under the cursor, so an index would
    /// silently move the selection onto a different card the moment an agent
    /// raised one.
    pub selected: Option<i64>,
    /// `Some` once `/` has been pressed, even while still empty — an open but
    /// empty filter still owns the keyboard and `Esc` still has something to
    /// clear. Mirrors [`super::workspace::ListState`].
    pub filter: Option<String>,
    pub editing_filter: bool,
    /// An index into [`Sort::ALL`] rather than a `Sort`, so the key that cycles
    /// it cannot run out of orders.
    pub sort: usize,
    pub kind: usize,
    pub stack: usize,
    /// Whether the once-per-session auto-open has been spent.
    ///
    /// Once, deliberately. A rail that reopens itself every time a blocker
    /// arrives is a rail people turn off, and then the *next* blocker is
    /// invisible — the failure the auto-open exists to prevent.
    pub auto_opened: bool,
    /// The largest number of open blockers that has already been said out loud.
    ///
    /// Kept apart from [`RailState::auto_opened`] because opening the rail and
    /// saying something are different promises. Opening it twice fights the
    /// reader who just closed it, so that stays once per session; staying quiet
    /// about the second blocker is the failure this whole column exists to
    /// prevent, so the *sentence* is not rationed. A count rather than a flag,
    /// so three blockers arriving one at a time produce three lines and three
    /// blockers arriving together produce one. It falls back to zero when the
    /// last one is answered, which is what lets the next one speak again.
    pub announced: usize,
    /// Whether `Ctrl-R` has been pressed and the rail is waiting for a digit.
    ///
    /// The rail can be *shown* without being *focused*, and this is a third
    /// thing again: the keyboard still belongs to the chat, and exactly one
    /// keystroke is being watched for. Any key that is not a digit puts it back
    /// down and is handled normally, so an armed rail can never eat a sentence
    /// — the worst it costs is the one keypress that disarmed it, which is
    /// still typed.
    pub quick: bool,
    /// Whether the rail shows the whole subtree or only this conversation.
    ///
    /// On by default, and that is the orchestrator's whole case for existing:
    /// the sessions doing the work are the ones with questions, and a rail that
    /// showed only the conversation you happen to be looking at would hide
    /// every blocker in the fleet behind a keystroke nobody knew to press.
    ///
    /// Cascade is **upward only** — a parent sees its descendants' cards and
    /// never the reverse — which falls out of `Query::subtree_of` walking down
    /// from the root rather than up from each card.
    pub cascade: bool,
}

impl Default for RailState {
    fn default() -> RailState {
        RailState {
            // Hidden until there is something to say. The first blocker opens
            // it, and `Ctrl-R` opens it before that.
            shown: false,
            focused: false,
            expanded: false,
            scroll: 0,
            selected: None,
            filter: None,
            editing_filter: false,
            sort: 0,
            kind: 0,
            stack: 0,
            auto_opened: false,
            announced: 0,
            quick: false,
            cascade: true,
        }
    }
}

impl RailState {
    /// The order in force.
    pub fn sort_now(&self) -> Sort {
        Sort::ALL[self.sort % Sort::ALL.len()]
    }

    /// The kind on show, or `None` for every kind.
    pub fn kind_now(&self) -> Option<CardKind> {
        KINDS[self.kind % KINDS.len()]
    }

    /// Which stack is on show. Open is the resting state; the others are how
    /// an answered card is found again.
    pub fn stack_now(&self) -> Status {
        STACKS[self.stack % STACKS.len()]
    }

    /// What the rail asks the store for.
    ///
    /// The text goes into [`Query::text`], which is full-text search over the
    /// title, the body and the answer — not a filter applied in Rust
    /// afterwards. That matters twice: it is the same index `jod card ls` and
    /// the MCP tool go through, so a search here and a search on a phone find
    /// the same cards; and it runs per keystroke, which a linear scan over
    /// every card a busy fleet has raised would not survive.
    ///
    /// A filter that is open but empty is not a search — it hides nothing,
    /// exactly as `/` does on every list screen.
    pub fn query(&self, conversation_id: Option<String>) -> Query {
        // Exactly one of the two is ever set. Both together would be a filter
        // that says "in this subtree, and also only this one conversation",
        // which is the subtree scope silently doing nothing.
        let (conversation_id, subtree_of) = if self.cascade {
            (None, conversation_id)
        } else {
            (conversation_id, None)
        };
        Query {
            conversation_id,
            subtree_of,
            work_id: None,
            kind: self.kind_now(),
            status: Some(self.stack_now()),
            blocking_only: false,
            text: self
                .filter
                .as_ref()
                .filter(|f| !f.trim().is_empty())
                .cloned(),
            sort: self.sort_now(),
            limit: Some(LIMIT),
        }
    }

    /// Does the rail have a filter that hides cards?
    pub fn filtering(&self) -> bool {
        self.filter.as_deref().is_some_and(|f| !f.trim().is_empty())
    }

    pub fn cycle_sort(&mut self) -> Sort {
        self.sort = (self.sort + 1) % Sort::ALL.len();
        self.sort_now()
    }

    pub fn cycle_kind(&mut self) -> Option<CardKind> {
        self.kind = (self.kind + 1) % KINDS.len();
        self.kind_now()
    }

    pub fn cycle_stack(&mut self) -> Status {
        self.stack = (self.stack + 1) % STACKS.len();
        // A different stack is a different set of cards, and the expanded view
        // is about *one* of them. Collapsing rather than trying to keep the
        // cursor across the change is the honest move: the card that was
        // expanded is, by definition, not in the stack now on screen.
        self.collapse();
        self.stack_now()
    }

    /// Where the cursor is, as a row index into `ids`.
    pub fn index(&self, ids: &[i64]) -> usize {
        self.selected
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(0)
    }

    /// Keep the cursor on a card that still exists, preferring the one it was
    /// on. Called after every refresh and every filter change.
    pub fn reconcile(&mut self, ids: &[i64]) {
        if ids.is_empty() {
            self.selected = None;
            // Nothing to expand, so nothing may claim to be expanded — the
            // renderer would otherwise draw an empty full-card pane and leave
            // no way out of it but `Esc`.
            self.collapse();
            return;
        }
        if !self.selected.is_some_and(|id| ids.contains(&id)) {
            if let Some(first) = ids.first().copied() {
                self.look_at(first);
            }
        }
    }

    /// Move by `delta`, clamped at both ends rather than wrapping — the same
    /// rule the list screens follow, and for the same reason: in a stack that
    /// changes under you, overshooting lands somewhere unrelated.
    pub fn step(&mut self, delta: isize, ids: &[i64]) {
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let at = self.index(ids) as isize;
        let landed = (at + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.look_at(ids[landed]);
    }

    /// Put the cursor on one specific card — what a click on it means.
    ///
    /// Separate from [`RailState::step`] because a pointer names the card
    /// directly and never has to know where it sits in the stack, which is the
    /// one thing the sort keeps changing underneath.
    pub fn look_at(&mut self, id: i64) {
        if self.selected != Some(id) {
            // See `scroll`: the offset is the *card's*, so it does not travel.
            self.scroll = 0;
        }
        self.selected = Some(id);
    }

    /// Put the card back into the stack, at the top of its text.
    ///
    /// Every way out of the expanded card goes through here, so none of them
    /// can leave an offset behind for the next card to open on.
    pub fn collapse(&mut self) {
        self.expanded = false;
        self.scroll = 0;
    }

    /// Scroll the expanded card, stopping at both ends.
    ///
    /// `past` is how many lines of the card fall below the pane, measured by
    /// the last frame — scrolling into blank space below the last line reads as
    /// the card having been emptied.
    pub fn scroll_card(&mut self, delta: i16, past: u16) {
        let landed = (self.scroll as i32 + delta as i32).clamp(0, past as i32);
        self.scroll = landed as u16;
    }

    /// `Ctrl-N`: open the rail and take the keyboard, or put it away again.
    ///
    /// One key both ways, because the key that opened something is the key
    /// people press to close it — a view you open with `Ctrl-N` and close with
    /// a different chord is a view you leave open. The stack is walked with
    /// `↑↓`/`jk` once the rail has the keyboard, which is where it belonged: a
    /// second stepping key that also happened to be the only way in made the
    /// way *out* the thing nobody could find.
    ///
    /// Three states rather than two, because the rail can be on screen without
    /// holding the keyboard — [`RailState::auto_open`] puts it there when a
    /// blocker arrives, and `Ctrl-R` shows it without focusing it. From there
    /// this key takes the keyboard rather than closing a rail you have not
    /// read yet.
    pub fn toggle(&mut self, ids: &[i64]) {
        if self.shown && self.focused {
            self.close();
            return;
        }
        self.shown = true;
        self.focused = true;
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        if self.selected.is_some_and(|id| ids.contains(&id)) {
            return;
        }
        if let Some(first) = ids.first().copied() {
            self.look_at(first);
        }
    }

    /// `Ctrl-R`: put the rail on screen and watch for a digit, or put it away.
    ///
    /// This key used to be a plain visibility toggle. It is a prefix now,
    /// because showing a column you were not going to read is not worth a chord
    /// and answering the thing that stopped a run is. The old job survives in
    /// both directions: the first press still shows a hidden rail, and a second
    /// press still takes it away.
    ///
    /// It deliberately does **not** take the keyboard. `Ctrl-N` is the key that
    /// does that, and the whole value of this one is that it costs nothing —
    /// press it mid-sentence, read the numbered rows, press a digit or carry on
    /// typing.
    ///
    /// Returns whether it armed rather than disarmed, so the caller can say
    /// which of the two just happened.
    pub fn arm(&mut self) -> bool {
        if self.quick {
            self.close();
            return false;
        }
        self.shown = true;
        self.quick = true;
        true
    }

    /// Stop watching for a digit, leaving everything else as it was.
    ///
    /// Called for every key that is not a digit, and for every way out of the
    /// rail. An armed rail that stayed armed would turn the next `7` typed into
    /// a sentence into an answer to somebody's card.
    pub fn disarm(&mut self) {
        self.quick = false;
    }

    /// Take the rail off the screen and hand the keyboard back.
    ///
    /// Every way out ends here — the chord, `Esc`, and `Ctrl-R` hiding it —
    /// so none of them can leave the rail holding the bare keys with no rail on
    /// screen to spend them on.
    ///
    /// The filter and the sort are deliberately left alone. E2.S5 asks for them
    /// to be held in state so that leaving the rail and coming back finds it as
    /// you left it, and putting it away is leaving it. `Esc` clears the filter
    /// on its own first level, which is a different key saying a different
    /// thing: *undo the narrowing*, not *put the rail away*.
    pub fn close(&mut self) {
        self.shown = false;
        self.focused = false;
        self.quick = false;
        self.collapse();
    }

    /// Hand the keyboard back to whatever was underneath, leaving the typed
    /// line alone.
    ///
    /// One level at a time, like `Esc` everywhere else in this program: the
    /// filter first, then the expanded card, and then the rail itself. That
    /// last step used to stop at un-focusing and leave the rail on screen,
    /// which meant the only way to get the screen back was a chord — `Ctrl-R` —
    /// that nothing on the rail's own keybar named. `Esc` is what people press
    /// to leave a thing, so it now leaves it.
    pub fn back(&mut self) -> bool {
        if self.editing_filter || self.filter.is_some() {
            self.filter = None;
            self.editing_filter = false;
            return true;
        }
        if self.expanded {
            self.collapse();
            return true;
        }
        if self.focused || self.shown {
            self.close();
            return true;
        }
        false
    }

    /// Open the rail the first time a blocker appears, and never again.
    ///
    /// Returns whether this call is what opened it, so the caller can say so
    /// once rather than every tick.
    pub fn auto_open(&mut self, cards: &[Card]) -> bool {
        if self.auto_opened {
            return false;
        }
        if !cards.iter().any(|c| c.blocking && c.is_open()) {
            return false;
        }
        self.auto_opened = true;
        let was_hidden = !self.shown;
        self.shown = true;
        was_hidden
    }

    /// The line to say about blockers this tick, or `None` to stay quiet.
    ///
    /// Said whenever the number of open blockers **rises** past the highest
    /// already said, and reset to nothing once they are all gone. Before this,
    /// the sentence rode on [`RailState::auto_open`]'s once-per-session latch,
    /// so a session that had seen one blocker never mentioned another — you
    /// closed the rail, an agent stopped an hour later, and the only trace was
    /// a grey fragment in the middle of a run-on status line.
    ///
    /// `opened` is whether this same tick is what put the rail on screen, which
    /// changes what the reader has to do next: a rail already in front of them
    /// needs no instructions, and a hidden one does.
    pub fn announce(&mut self, cards: &[Card], opened: bool) -> Option<String> {
        let blocked = cards.iter().filter(|c| c.blocking && c.is_open()).count();
        if blocked == 0 {
            // Not a floor at the old count: answering everything and then being
            // blocked again is a fresh piece of news, not a repeat.
            self.announced = 0;
            return None;
        }
        if blocked <= self.announced {
            return None;
        }
        // How many are *new* since the last thing said, so the sentence is
        // about what just happened rather than about the whole backlog.
        let fresh = blocked - self.announced;
        self.announced = blocked;
        let subject = if fresh == 1 {
            "a run is blocked".to_string()
        } else {
            format!("{fresh} runs are blocked")
        };
        // The total only earns its place when it differs from what just
        // arrived; otherwise it says the same number twice.
        let total = if blocked > fresh {
            format!(" · {blocked} waiting on you")
        } else {
            String::new()
        };
        // One key either way: `Ctrl-N` shows the rail and takes the keyboard,
        // so it is the answer whether the column is already up or not.
        Some(if opened {
            format!("{subject}{total} — the rail is open; Ctrl-N answers, and closes it again")
        } else {
            format!("{subject}{total} — Ctrl-N opens the rail to answer")
        })
    }
}

/// What the rail says about a card's *delivery*, or `None` when there is
/// nothing to say.
///
/// Deliberately phrased as two facts joined — `answered, queued` — rather than
/// as one word. "Answered" alone reads as done, and the agent has not heard
/// yet; "queued" alone does not say what is queued. See decision D2.
pub fn delivery_note(card: &Card) -> Option<&'static str> {
    match (card.status, card.delivery) {
        (Status::Answered, Delivery::Queued) => Some("answered, queued"),
        (Status::Answered, Delivery::Delivered) => Some("answered, delivered"),
        // The session ended before it could be told. Reported rather than
        // dropped: an answer that vanished is worse than one that failed.
        (_, Delivery::Undeliverable) => Some("undelivered — the session ended"),
        (Status::Answered, Delivery::None) => Some("answered"),
        (Status::Dismissed, _) => Some("dismissed"),
        (Status::Open, _) => None,
    }
}

/// The same fact in one word, for a rail too narrow to hold the sentence.
///
/// [`delivery_note`] joins two facts — `answered, queued` — because "answered"
/// alone reads as done. This drops the half the reader already has: the stack's
/// own name is printed on the rail's header, so a row saying `queued` under a
/// header saying `answered` says both things exactly once between them, which is
/// what decision D2 is actually asking for. `undelivered` keeps its own meaning
/// with no header at all, which is why that one is not shortened to `session`.
///
/// Only ever used when the full sentence will not fit beside a readable title —
/// see `ui::answer_text`. A thirty-four column rail that spends sixteen of them
/// on `answered, queued` has four left for the card, and a row that says what
/// happened to a question nobody can read is not the honest option.
pub fn delivery_short(card: &Card) -> Option<&'static str> {
    match (card.status, card.delivery) {
        (Status::Answered, Delivery::Queued) => Some("queued"),
        (Status::Answered, Delivery::Delivered) => Some("delivered"),
        (_, Delivery::Undeliverable) => Some("undelivered"),
        (Status::Answered, Delivery::None) => Some("answered"),
        (Status::Dismissed, _) => Some("dismissed"),
        (Status::Open, _) => None,
    }
}

/// The word every blocking card carries, beside its coloured border.
///
/// A constant because two places print it — the collapsed card and the
/// expanded one — and because the epic's check greps for it. Colour is never
/// the only channel in this program, and this is the other one.
pub const BLOCKED: &str = "blocked";

/// The glyph that says what kind of card this is without relying on colour.
pub fn kind_glyph(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Decision => "◆",
        CardKind::Question => "?",
        CardKind::Secret => "✱",
    }
}

/// The glyph a collapsed row leads with, which is not always the kind's.
///
/// One column, three facts competing for it, resolved by which one the reader
/// needs first:
///
/// 1. **A secret keeps `✱`.** It is the one kind whose answer is not a
///    keystroke — it needs a typed value — so a row that hid that would offer a
///    digit for something no digit can do.
/// 2. **Anything else that blocks gets `!`.** A card that stopped a run outranks
///    what sort of card it is, and this is the non-colour channel for it: the
///    collapsed row no longer carries the word `blocked`, so the glyph has to.
///    The word itself survives on the rail's header, on every group heading
///    that has one, and in full on the expanded card.
/// 3. **Otherwise the kind's own glyph**, as before.
pub fn row_glyph(card: &Card) -> &'static str {
    if card.kind == CardKind::Secret {
        return kind_glyph(CardKind::Secret);
    }
    if card.blocking {
        return "!";
    }
    kind_glyph(card.kind)
}

/// What the rail's quick answer would send, and the word the row prints for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommended {
    /// The exact option text to answer with — matched by value against
    /// [`Card::options`], never by index. The hook that wrote the options is
    /// free to reorder them, and an index would silently start answering a
    /// different one the day it did.
    pub option: String,
    /// What the row prints in its right-hand column, short enough for a
    /// thirty-four column rail.
    pub label: String,
}

/// The one answer a card can be given without reading it, or `None`.
///
/// `None` is the honest and the common result, and the rail prints it as `—`:
/// most questions are questions precisely because nobody can guess them. Two
/// cases can be answered blind, and only two:
///
/// - **A permission request** — the agent was refused a tool call and is asking
///   to be let past. The recommendation is [`ONCE`](jod_core::approvals::ONCE)
///   and never `always allow`, because a two-keystroke chord must not be able
///   to write a standing grant. Widening what Jod may run unasked stays a
///   deliberate act: open the card and press the digit for it.
/// - **A decision already taken** — the agent chose, and the card exists so it
///   can be overruled. Accepting is agreeing with what has already happened, so
///   it changes nothing except that the card stops waiting.
///
/// A card that is not open has nothing to recommend: it has been answered or
/// put down, and offering a digit for it would answer it twice.
pub fn recommended(card: &Card) -> Option<Recommended> {
    if card.status != Status::Open {
        return None;
    }
    if jod_core::approvals::is_approval(card) {
        let once = card
            .options
            .iter()
            .find(|o| o.trim() == jod_core::approvals::ONCE)?;
        return Some(Recommended {
            option: once.clone(),
            label: "allow".to_string(),
        });
    }
    // Only when the option is still on offer. A `chosen` naming something that
    // is not in `options` is a card whose emitter has changed its mind about
    // the alternatives, and answering with text no option matches would be a
    // write nobody asked for.
    let chosen = card.chosen.as_ref()?;
    let option = card.options.iter().find(|o| *o == chosen)?;
    Some(Recommended {
        option: option.clone(),
        label: option.clone(),
    })
}

/// One project's cards — or one session's, when the cards belong to no work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The work id, or the conversation id when there is no work. Identity, not
    /// display: two works can be called the same thing.
    pub key: String,
    /// What the heading prints.
    pub label: String,
    /// How many of this group's cards stopped a run.
    pub blocked: usize,
    /// Indices into the list this was built from, in the order it had them.
    pub cards: Vec<usize>,
}

/// Gather the rail's cards under a heading each, without reordering them.
///
/// **The sort still decides everything.** Groups come out in the order their
/// first card appeared and cards keep their places inside one, so `Pressing`
/// still floats the group holding the most urgent card to the top. Grouping
/// that re-sorted would quietly undo the one ordering the rail exists to
/// provide.
///
/// `names` resolves an id to something a person recognises — a work's title,
/// which is a paraphrase of what that agent was asked to do. An id nobody has
/// looked up yet falls back to a short form of itself, so a heading is never
/// blank while the lookup catches up.
pub fn group(cards: &[Card], names: &std::collections::HashMap<String, String>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (at, card) in cards.iter().enumerate() {
        let (key, fallback) = match &card.work_id {
            Some(work) => (work.clone(), short_id(work)),
            None => (
                card.conversation_id.clone(),
                format!("session {}", short_id(&card.conversation_id)),
            ),
        };
        let found = groups.iter().position(|g| g.key == key);
        let at_group = match found {
            Some(index) => index,
            None => {
                groups.push(Group {
                    label: names.get(&key).cloned().unwrap_or(fallback),
                    key,
                    blocked: 0,
                    cards: Vec::new(),
                });
                groups.len() - 1
            }
        };
        groups[at_group].cards.push(at);
        if card.blocking && card.is_open() {
            groups[at_group].blocked += 1;
        }
    }
    groups
}

/// An id cut to something a heading can hold, when there is no name for it yet.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The cards in the order the rail draws them, as indices into the list the
/// groups were built from.
///
/// Grouping moves cards past each other — a second card for the first work is
/// drawn above a first card for the second one — so this is the order, and the
/// only one. Three things read it and all three must agree: the renderer, the
/// cursor that `↑↓` walks, and the digit the quick answer resolves. A digit
/// that counted rows while the cursor counted the query's order would answer
/// whichever card happened to sit at that number in the other list.
pub fn order(groups: &[Group]) -> Vec<usize> {
    groups.iter().flat_map(|g| g.cards.iter().copied()).collect()
}

/// The rail, reduced to a single line.
///
/// A narrow terminal gets the rail as a panel across the bottom rather than a
/// squeezed column, so this is what is left for the two cases even that cannot
/// serve: a rail with no cards in it, and a body too short to divide. It still
/// has to carry the one thing the rail exists for — that something is blocked —
/// and the key that opens it.
pub fn summary(cards: &[Card]) -> String {
    if cards.is_empty() {
        return String::new();
    }
    let blocking = cards.iter().filter(|c| c.blocking && c.is_open()).count();
    let noun = if cards.len() == 1 { "card" } else { "cards" };
    let mut line = format!("{} {noun}", cards.len());
    if blocking > 0 {
        line.push_str(&format!(" · {blocking} {BLOCKED}"));
    }
    let queued = cards.iter().filter(|c| c.is_waiting_to_deliver()).count();
    if queued > 0 {
        line.push_str(&format!(" · {queued} queued"));
    }
    // The cheapest way in rather than the most capable one. This line is what a
    // terminal too small for the rail gets, so the key it names should be the
    // one that costs least to press — `Ctrl-R` shows the stack and arms the
    // digits without taking the keyboard, and `Ctrl-N` is a keystroke further
    // on for anyone who wants to read a card rather than clear it.
    line.push_str(" · Ctrl-R then 1–9");
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::cards::{Importance, Source};

    fn card(id: i64, blocking: bool) -> Card {
        Card {
            id,
            conversation_id: "c".into(),
            work_id: None,
            run_id: None,
            kind: CardKind::Question,
            importance: Importance::Normal,
            blocking,
            status: Status::Open,
            delivery: Delivery::None,
            title: format!("card {id}"),
            body: String::new(),
            options: vec![],
            chosen: None,
            answer: None,
            secret_name: None,
            secret_scope: None,
            source: Source::Mcp,
            created_at_ms: 0,
            updated_at_ms: 0,
            answered_at_ms: None,
            delivered_at_ms: None,
            dedupe_key: None,
        }
    }

    /// A permission card as `jod approve-hook` actually raises one.
    fn approval(id: i64, pattern: &str) -> Card {
        let mut c = card(id, true);
        c.title = format!("Bash: {pattern}");
        c.options = vec![
            format!("{} `{pattern}*`", jod_core::approvals::ALWAYS),
            jod_core::approvals::ONCE.to_string(),
            jod_core::approvals::DENY.to_string(),
        ];
        c.dedupe_key = Some(format!("approval:Bash:{pattern}*"));
        c
    }

    /// The chord's whole safety property. Two keystrokes may let a call
    /// through; they may never widen what Jod is allowed to run from then on.
    #[test]
    fn the_quick_answer_on_a_permission_card_allows_once_and_never_always() {
        let c = approval(1, "cargo test ");
        let rec = recommended(&c).expect("a permission card can be accepted blind");
        assert_eq!(rec.option, jod_core::approvals::ONCE);
        assert_eq!(rec.label, "allow", "the row has room for one word");
        assert!(
            !rec.option.starts_with(jod_core::approvals::ALWAYS),
            "a standing grant is never two keystrokes away: {}",
            rec.option
        );
    }

    /// The recommendation is matched by value, so an emitter that reorders its
    /// options cannot silently move which one the digit sends.
    #[test]
    fn a_permission_card_whose_options_moved_still_resolves_to_the_same_text() {
        let mut c = approval(1, "gh pr ");
        c.options.reverse();
        assert_eq!(
            recommended(&c).map(|r| r.option),
            Some(jod_core::approvals::ONCE.to_string())
        );
    }

    /// A decision has already happened; accepting agrees with it. Anything else
    /// with options is a question, and a question answered blind is a guess.
    #[test]
    fn only_a_decision_already_taken_can_be_accepted_blind() {
        let mut taken = card(1, false);
        taken.kind = CardKind::Decision;
        taken.options = vec!["SQLite".into(), "Postgres".into()];
        taken.chosen = Some("SQLite".into());
        assert_eq!(
            recommended(&taken).map(|r| r.label),
            Some("SQLite".to_string())
        );

        let mut asked = card(2, false);
        asked.options = vec!["8080".into(), "3000".into()];
        assert_eq!(
            recommended(&asked),
            None,
            "an open question has no answer anybody can guess"
        );

        // A `chosen` naming something no longer on offer is an emitter that
        // changed its mind, and answering with text no option matches would be
        // a write nobody asked for.
        let mut stale = taken.clone();
        stale.chosen = Some("DuckDB".into());
        assert_eq!(recommended(&stale), None);
    }

    /// A card that is not open has nothing left to accept, or the digit beside
    /// it would answer it twice.
    #[test]
    fn an_answered_card_offers_no_quick_answer() {
        let mut c = approval(1, "cargo test ");
        c.status = Status::Answered;
        assert_eq!(recommended(&c), None);
        c.status = Status::Dismissed;
        assert_eq!(recommended(&c), None);
    }

    /// A secret needs a typed value, so no digit can finish it.
    #[test]
    fn a_secret_offers_no_quick_answer_and_keeps_its_own_glyph() {
        let mut c = card(1, true);
        c.kind = CardKind::Secret;
        assert_eq!(recommended(&c), None);
        assert_eq!(
            row_glyph(&c),
            kind_glyph(CardKind::Secret),
            "a blocking secret is still a secret: the digit cannot answer it"
        );
    }

    /// Blocking is never carried by colour alone. The word left the row, so the
    /// glyph column has to say it.
    #[test]
    fn a_blocking_row_is_marked_in_something_other_than_colour() {
        assert_eq!(row_glyph(&card(1, true)), "!");
        assert_eq!(row_glyph(&card(2, false)), kind_glyph(CardKind::Question));
    }

    /// Cards gather under the work they belong to, and fall back to the session
    /// when they belong to none.
    #[test]
    fn cards_group_by_work_and_fall_back_to_the_session() {
        let mut one = card(1, true);
        one.work_id = Some("work-aaaa1111".into());
        one.conversation_id = "conv-aaaa".into();
        let mut two = card(2, false);
        two.work_id = Some("work-bbbb2222".into());
        two.conversation_id = "conv-bbbb".into();
        let mut three = card(3, false);
        three.work_id = Some("work-aaaa1111".into());
        three.conversation_id = "conv-cccc".into();
        let mut loose = card(4, false);
        loose.conversation_id = "conv-dddd5555".into();

        let mut names = std::collections::HashMap::new();
        names.insert("work-aaaa1111".to_string(), "ship the rail".to_string());

        let groups = group(&[one, two, three, loose], &names);
        assert_eq!(groups.len(), 3, "two works and one loose session");
        assert_eq!(groups[0].label, "ship the rail", "the work's own title");
        assert_eq!(groups[0].cards, vec![0, 2], "both of that work's cards");
        assert_eq!(groups[0].blocked, 1);
        assert_eq!(
            groups[1].label, "work-bbb",
            "an id nobody has looked up yet still gets a heading"
        );
        assert_eq!(
            groups[2].label, "session conv-ddd",
            "a card belonging to no work is filed under the session"
        );
    }

    /// Grouping moves cards past each other, and everything that counts rows —
    /// the renderer, the cursor, the quick-answer digit — has to agree on the
    /// order it produced. A digit resolved against the query's order would
    /// answer a different card from the one it is printed beside.
    #[test]
    fn the_drawn_order_is_the_grouped_order_and_not_the_querys() {
        let mut first = card(1, false);
        first.work_id = Some("A".into());
        let mut middle = card(2, false);
        middle.work_id = Some("B".into());
        let mut last = card(3, false);
        last.work_id = Some("A".into());

        let cards = [first, middle, last];
        let groups = group(&cards, &std::collections::HashMap::new());
        assert_eq!(
            order(&groups),
            vec![0, 2, 1],
            "both of A's cards, then B's — not the order the query returned"
        );
    }

    /// The prefix shows the rail without taking the keyboard, and a second
    /// press puts the whole thing away.
    #[test]
    fn the_quick_prefix_arms_then_closes_and_never_holds_the_keyboard() {
        let mut rail = RailState::default();
        assert!(rail.arm(), "the first press arms");
        assert!(rail.shown, "and shows the rail");
        assert!(!rail.focused, "but never takes the bare keys");

        assert!(!rail.arm(), "the second press disarms");
        assert!(!rail.shown, "and takes the rail away with it");
        assert!(!rail.quick);
    }

    /// Anything that puts the rail down puts the prefix down too, or the next
    /// digit typed into a sentence answers somebody's card.
    #[test]
    fn closing_the_rail_disarms_the_prefix() {
        let mut rail = RailState::default();
        rail.arm();
        rail.close();
        assert!(!rail.quick);

        rail.arm();
        rail.disarm();
        assert!(!rail.quick, "and so does changing your mind");
        assert!(rail.shown, "which leaves the rail exactly as it was");
    }

    /// The whole point of D2: two independent facts, and the rail says both.
    /// An answered card that reads as done is a card the reader believes the
    /// agent has acted on.
    #[test]
    fn an_answered_card_reads_as_queued_until_it_is_delivered() {
        let mut c = card(1, false);
        c.status = Status::Answered;
        c.delivery = Delivery::Queued;
        assert_eq!(delivery_note(&c), Some("answered, queued"));
        c.delivery = Delivery::Delivered;
        assert_eq!(delivery_note(&c), Some("answered, delivered"));
    }

    #[test]
    fn an_open_card_has_no_delivery_to_report() {
        assert_eq!(delivery_note(&card(1, false)), None);
    }

    /// The gap this closes: the sentence used to ride on `auto_open`'s
    /// once-per-session latch, so a session that had already seen one blocker
    /// never mentioned another. You closed the rail, an agent stopped an hour
    /// later, and nothing said so.
    #[test]
    fn a_second_blocker_is_announced_even_though_the_rail_only_opens_once() {
        let mut rail = RailState::default();
        let one = vec![card(1, true)];
        assert!(rail.auto_open(&one), "the first blocker opens the rail");
        let first = rail.announce(&one, true).expect("and it says so");
        assert!(first.contains("a run is blocked"), "{first}");

        let two = vec![card(1, true), card(2, true)];
        assert!(!rail.auto_open(&two), "the rail is not opened a second time");
        let second = rail
            .announce(&two, false)
            .expect("but the news is said a second time");
        assert!(second.contains("2 waiting on you"), "{second}");
        assert!(
            second.contains("Ctrl-N"),
            "with the key that answers it: {second}"
        );
    }

    /// A line every tick is noise, and noise is what the reader learns to skip.
    #[test]
    fn nothing_new_is_said_while_the_same_cards_sit_there() {
        let mut rail = RailState::default();
        let one = vec![card(1, true)];
        assert!(rail.announce(&one, true).is_some(), "said once");
        assert!(rail.announce(&one, false).is_none(), "and not again");
        assert!(rail.announce(&one, false).is_none());
    }

    /// Two at once is one sentence. The count only earns its place when it
    /// differs from what just arrived; otherwise it prints the same number
    /// twice in one line.
    #[test]
    fn blockers_that_arrive_together_are_one_line_carrying_one_number() {
        let mut rail = RailState::default();
        let two = vec![card(1, true), card(2, true)];
        let said = rail.announce(&two, true).expect("said");
        assert!(said.contains("2 runs are blocked"), "{said}");
        assert!(!said.contains("waiting on you"), "and only once: {said}");
    }

    /// Answering everything and then being blocked again is fresh news, not a
    /// repeat — so the high-water mark falls back rather than holding.
    #[test]
    fn clearing_every_blocker_lets_the_next_one_speak_again() {
        let mut rail = RailState::default();
        assert!(rail.announce(&[card(1, true)], true).is_some());
        assert!(
            rail.announce(&[card(1, false)], false).is_none(),
            "nothing to say when nothing is blocked"
        );
        let again = rail
            .announce(&[card(2, true)], false)
            .expect("the next blocker is news again");
        assert!(again.contains("a run is blocked"), "{again}");
    }

    /// An answered card is not blocking anybody, so it must not keep the
    /// high-water mark up and silence the next real one.
    #[test]
    fn an_answered_card_does_not_count_as_a_blocker() {
        let mut rail = RailState::default();
        let mut answered = card(1, true);
        answered.status = Status::Answered;
        assert!(rail.announce(&[answered], true).is_none());
    }

    /// A session that ended owing an answer says so. Mail that vanishes is
    /// worse than mail that fails.
    #[test]
    fn an_answer_that_never_arrived_is_reported_rather_than_dropped() {
        let mut c = card(1, false);
        c.status = Status::Answered;
        c.delivery = Delivery::Undeliverable;
        assert_eq!(
            delivery_note(&c),
            Some("undelivered — the session ended")
        );
    }

    /// E4.S5: the main rail shows the whole subtree. Exactly one of the two
    /// scopes is ever set — both would be a subtree filter silently doing
    /// nothing, because a conversation filter on top of it narrows back to one.
    #[test]
    fn the_rail_asks_for_the_whole_subtree_and_narrows_to_one_session_on_a_toggle() {
        let mut rail = RailState::default();
        assert!(rail.cascade, "the orchestrator's rail is the point of it");

        let wide = rail.query(Some("conv".into()));
        assert_eq!(wide.subtree_of.as_deref(), Some("conv"));
        assert_eq!(wide.conversation_id, None, "both would cancel out");

        rail.cascade = false;
        let narrow = rail.query(Some("conv".into()));
        assert_eq!(narrow.conversation_id.as_deref(), Some("conv"));
        assert_eq!(narrow.subtree_of, None);
    }

    /// The text filter has to reach the store, or it is not full-text search —
    /// it is a scan someone will one day write in Rust because the query did
    /// not carry it.
    #[test]
    fn the_typed_filter_travels_to_the_store_as_a_search() {
        let mut rail = RailState::default();
        rail.filter = Some("sqlite".into());
        let q = rail.query(Some("conv".into()));
        assert_eq!(q.text.as_deref(), Some("sqlite"));
        // The scope the search runs over is the subtree by default — which of
        // the two id fields carries it is pinned by
        // `the_rail_asks_for_the_whole_subtree_and_narrows_to_one_session_on_a_toggle`.
        // What matters here is that the conversation reaches the query at all,
        // so a filter cannot silently search the whole database.
        assert_eq!(q.subtree_of.as_deref(), Some("conv"));
        assert_eq!(q.limit, Some(LIMIT));
    }

    /// An open-but-empty filter owns the keyboard without hiding anything, so
    /// pressing `/` never makes the rail appear to empty itself.
    #[test]
    fn an_open_but_empty_filter_searches_for_nothing() {
        let rail = RailState {
            filter: Some("   ".into()),
            ..Default::default()
        };
        assert!(!rail.filtering());
        assert_eq!(rail.query(None).text, None);
    }

    /// E2.S5 in one test: every one of the rail's settings is state, so
    /// nothing about walking to another screen and back can lose them.
    #[test]
    fn the_filter_sort_and_both_filters_survive_being_left_and_returned_to() {
        let mut rail = RailState::default();
        rail.filter = Some("db".into());
        let sort = rail.cycle_sort();
        let kind = rail.cycle_kind();
        let stack = rail.cycle_stack();

        // Nothing in this type is derived from the screen, so "navigating
        // away" cannot touch it — the clone stands in for the frames in
        // between.
        let later = rail.clone();
        assert_eq!(later.sort_now(), sort);
        assert_eq!(later.kind_now(), kind);
        assert_eq!(later.stack_now(), stack);
        assert_eq!(later.filter.as_deref(), Some("db"));
        let q = later.query(None);
        assert_eq!(q.sort, sort);
        assert_eq!(q.kind, kind);
        assert_eq!(q.status, Some(stack));
    }

    #[test]
    fn cycling_a_sort_reaches_every_order_and_comes_back() {
        let mut rail = RailState::default();
        let first = rail.sort_now();
        let mut seen = vec![first];
        for _ in 1..Sort::ALL.len() {
            seen.push(rail.cycle_sort());
        }
        assert_eq!(seen.len(), Sort::ALL.len());
        assert_eq!(rail.cycle_sort(), first, "the cycle closes");
    }

    #[test]
    fn cycling_a_kind_returns_to_showing_every_kind() {
        let mut rail = RailState::default();
        assert_eq!(rail.kind_now(), None);
        for _ in 0..KINDS.len() {
            rail.cycle_kind();
        }
        assert_eq!(rail.kind_now(), None);
    }

    /// Answered cards leave the stack and come back on a toggle — the second
    /// half of E2.S4.
    #[test]
    fn the_stack_toggle_reaches_the_answered_cards() {
        let mut rail = RailState::default();
        assert_eq!(rail.stack_now(), Status::Open);
        assert_eq!(rail.cycle_stack(), Status::Answered);
        assert_eq!(rail.cycle_stack(), Status::Dismissed);
        assert_eq!(rail.cycle_stack(), Status::Open);
    }

    /// The rail re-queries on the tick. Holding a row index would move the
    /// cursor onto a different card the moment an agent raised one.
    #[test]
    fn the_cursor_follows_the_card_when_a_new_one_arrives_above_it() {
        let mut rail = RailState::default();
        rail.reconcile(&[7, 8]);
        rail.step(1, &[7, 8]);
        assert_eq!(rail.selected, Some(8));

        // A blocker arrives and sorts to the top.
        let after = [9, 7, 8];
        rail.reconcile(&after);
        assert_eq!(rail.selected, Some(8), "still on the same card");
        assert_eq!(rail.index(&after), 2);
    }

    #[test]
    fn a_card_that_was_answered_away_leaves_the_cursor_on_the_top_of_the_stack() {
        let mut rail = RailState {
            selected: Some(42),
            ..Default::default()
        };
        rail.reconcile(&[7, 8]);
        assert_eq!(rail.selected, Some(7));
    }

    /// An expanded pane over nothing has no card in it and no obvious way out.
    #[test]
    fn emptying_the_stack_collapses_the_expanded_card() {
        let mut rail = RailState {
            expanded: true,
            selected: Some(1),
            ..Default::default()
        };
        rail.reconcile(&[]);
        assert!(!rail.expanded);
        assert_eq!(rail.selected, None);
    }

    /// The first `Ctrl-N` must not skip the most pressing card, which is the
    /// one `Sort::Pressing` deliberately put at the top.
    #[test]
    fn opening_the_rail_lands_on_the_top_card() {
        let mut rail = RailState::default();
        rail.toggle(&[3, 4, 5]);
        assert!(rail.shown && rail.focused);
        assert_eq!(rail.selected, Some(3));
    }

    /// The key that opened it is the key that closes it. Before this, `Ctrl-N`
    /// stepped to the next card instead and the only way out was `Ctrl-R`,
    /// which the rail's own keybar never printed.
    #[test]
    fn the_same_chord_puts_the_rail_away() {
        let mut rail = RailState::default();
        let ids = [3, 4, 5];
        rail.toggle(&ids);
        assert!(rail.shown && rail.focused);
        rail.toggle(&ids);
        assert!(!rail.shown, "the second press left it on screen");
        assert!(!rail.focused, "and left it holding the bare keys");
    }

    /// A rail that is on screen without holding the keyboard — which is where
    /// `auto_open` and `Ctrl-R` both leave it — is one you have not read yet.
    /// The chord takes the keyboard there rather than closing it.
    #[test]
    fn the_chord_focuses_a_rail_that_is_merely_showing() {
        let mut rail = RailState {
            shown: true,
            ..Default::default()
        };
        rail.toggle(&[3, 4, 5]);
        assert!(rail.shown, "a rail nobody has read was closed");
        assert!(rail.focused);
        assert_eq!(rail.selected, Some(3));
    }

    /// A cursor already on a card stays on it, so the chord is safe to press
    /// when you are not sure whether the rail has the keyboard.
    #[test]
    fn focusing_keeps_a_cursor_that_is_already_on_a_card() {
        let mut rail = RailState {
            shown: true,
            selected: Some(5),
            ..Default::default()
        };
        rail.toggle(&[3, 4, 5]);
        assert_eq!(rail.selected, Some(5));
    }

    /// `Esc` peels one layer at a time, and the last layer is the rail itself.
    /// It used to stop at un-focusing, which left the rail on screen with no
    /// way out that its own keybar named.
    #[test]
    fn escape_peels_one_layer_at_a_time_and_then_closes_the_rail() {
        let mut rail = RailState {
            shown: true,
            focused: true,
            expanded: true,
            filter: Some("db".into()),
            editing_filter: true,
            ..Default::default()
        };
        assert!(rail.back());
        assert_eq!(rail.filter, None, "the filter goes first");
        assert!(rail.back());
        assert!(!rail.expanded, "then the expanded card");
        assert!(rail.back());
        assert!(!rail.focused, "then the rail itself");
        assert!(!rail.shown, "and it leaves the screen with it");
        assert!(!rail.back(), "nothing left for this Esc to do");
    }

    /// Once per session. A rail that reopens itself on every blocker is a rail
    /// people turn off, and then the next blocker is invisible.
    #[test]
    fn the_first_blocker_opens_the_rail_and_the_second_does_not() {
        let mut rail = RailState::default();
        assert!(!rail.auto_open(&[card(1, false)]), "nothing is blocking");
        assert!(!rail.shown);

        assert!(rail.auto_open(&[card(1, false), card(2, true)]));
        assert!(rail.shown);

        rail.shown = false;
        assert!(
            !rail.auto_open(&[card(3, true)]),
            "the once-per-session open is spent"
        );
        assert!(!rail.shown, "and it stays where the user put it");
    }

    /// A blocker that has already been answered is not a blocker any more.
    #[test]
    fn an_answered_blocker_does_not_spend_the_auto_open() {
        let mut rail = RailState::default();
        let mut answered = card(1, true);
        answered.status = Status::Answered;
        assert!(!rail.auto_open(&[answered]));
        assert!(!rail.auto_opened);
    }

    /// The narrow-terminal line has one job: say that something is blocked,
    /// and say which key opens the rail.
    #[test]
    fn the_one_line_summary_names_the_blockers_and_the_key() {
        let line = summary(&[card(1, false), card(2, true), card(3, true)]);
        assert!(line.starts_with("3 cards"), "{line}");
        assert!(line.contains(&format!("2 {BLOCKED}")), "{line}");
        // The cheapest key in, which is the prefix rather than the focus chord:
        // this line is what a terminal too small to draw the rail gets.
        assert!(line.contains("Ctrl-R"), "{line}");
    }

    #[test]
    fn the_one_line_summary_counts_what_is_waiting_to_be_delivered() {
        let mut queued = card(2, false);
        queued.status = Status::Answered;
        queued.delivery = Delivery::Queued;
        let line = summary(&[card(1, false), queued]);
        assert!(line.contains("1 queued"), "{line}");
    }

    #[test]
    fn an_empty_rail_has_no_summary_line_at_all() {
        assert_eq!(summary(&[]), "");
    }
}
