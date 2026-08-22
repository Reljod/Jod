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
//! - `Ctrl-N` opens the rail and puts it away again, and `Ctrl-R` shows or
//!   hides it without taking the keyboard. Both are chords, so both are safe
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
/// Five, rather than as many as fit. `Pressing` has already put the blocking
/// cards at the top, so the rows past the fifth are spent on cards nobody is
/// reading — and on the bottom panel, which is the phone case, they are spent
/// on rows the chat needed. The rest of the stack stays reachable: the cursor
/// keys and the wheel both move the selection, and the window follows it.
pub const VISIBLE: usize = 5;

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

/// The word every blocking card carries, beside its coloured border.
///
/// A constant because two places print it — the collapsed card and the
/// expanded one — and because the epic's check greps for it. Colour is never
/// the only channel in this program, and this is the other one.
pub const BLOCKED: &str = "blocked";

/// Which session raised a card, short enough for a thirty-four column rail.
///
/// Printed on **every** cascaded card, and it is not decoration: with the
/// subtree scope on, the rail holds cards from sessions all over the fleet, and
/// answering is a write against one specific agent. A card that did not say
/// whose question it was would make "answer the top one" a coin flip about
/// which agent gets unblocked.
pub fn raised_by(card: &Card) -> String {
    let session: String = card.conversation_id.chars().take(8).collect();
    match &card.run_id {
        Some(run) => format!("{session}·{}", run.chars().take(4).collect::<String>()),
        None => session,
    }
}

/// The work a card belongs to, as a short tag.
///
/// A tag rather than the tint E4.S5 asks for, because the colour belongs to
/// the *work* — `works::Work::colour` — and there is no store query returning a
/// work yet. **Integration point:** once lane A lands one, look the work up and
/// colour the row with it; the tag stays either way, because colour is never
/// the only channel in this program.
pub fn work_tag(card: &Card) -> Option<String> {
    card.work_id
        .as_ref()
        .map(|id| id.chars().take(6).collect::<String>())
}

/// The glyph that says what kind of card this is without relying on colour.
pub fn kind_glyph(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Decision => "◆",
        CardKind::Question => "?",
        CardKind::Secret => "✱",
    }
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
    line.push_str(" · Ctrl-N to answer");
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

    /// Answering writes against one specific agent, so a cascaded card that did
    /// not say whose question it was would make "answer the top one" a coin
    /// flip about which agent gets unblocked.
    #[test]
    fn a_cascaded_card_names_the_session_that_raised_it() {
        let mut c = card(1, false);
        c.conversation_id = "7c09d454-aaaa-bbbb".into();
        c.run_id = Some("3f2ab1c0".into());
        let said = raised_by(&c);
        assert!(said.starts_with("7c09d454"), "{said}");
        assert!(said.contains("3f2a"), "and which run of it: {said}");
    }

    #[test]
    fn a_card_with_no_run_still_names_its_session() {
        let mut c = card(1, false);
        c.conversation_id = "7c09d454-aaaa".into();
        c.run_id = None;
        assert_eq!(raised_by(&c), "7c09d454");
    }

    /// The work tag stands in for the tint until there is a query returning a
    /// work. Colour is never the only channel here, so the tag is what the
    /// design actually needs and the colour is the improvement on top.
    #[test]
    fn a_card_belonging_to_a_work_carries_its_tag() {
        let mut c = card(1, false);
        c.work_id = Some("w-abcdef-123".into());
        assert_eq!(work_tag(&c).as_deref(), Some("w-abcd"));
        assert_eq!(work_tag(&card(2, false)), None, "no work, no tag");
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
        assert!(line.contains("Ctrl-N"), "{line}");
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
