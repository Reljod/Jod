//! The agent-to-agent traffic screen: which scope is on show, which messages
//! are, and how a flat list of rows becomes a conversation you can read.
//!
//! Everything here is a pure transformation, in the same spirit as
//! [`super::rail`] and [`super::fleet`] — the shaping is the part with the
//! judgement in it, and it has to be testable without a terminal or a store.
//!
//! ## Why the messages are threaded rather than listed
//!
//! [`jod_core::team::Store::traffic`] returns one scope's messages oldest
//! first, which is the order they were written and *not* the order they were
//! said in. Two agents holding three conversations at once interleave, so a
//! flat log reads as six unrelated sentences; the thread id and the depth are
//! already on every row precisely so a reader can put the reply back under the
//! question. That is G5.S1's "in order, **threaded**", and it is why the depth
//! bound in G4 was worth counting in the first place.
//!
//! ## Why the newest thread is at the top
//!
//! A work's bus is append-only and unbounded until the budget stops it, so the
//! interesting end is the recent one. Threads are ordered newest-activity-first
//! by default and the messages *inside* a thread stay oldest-first, so the
//! conversation reads downwards while the screen still opens on what just
//! happened. `S` cycles to plain chronological order for reading a work from
//! the beginning.
//!
//! ## Why a failure prints its reason rather than its state
//!
//! A8: a message to an agent that cannot receive it becomes a card, never a
//! silence. On this screen the same rule is that it becomes a *sentence*.
//! `undeliverable` alone tells you the machine noticed and tells you nothing
//! about what to do; the store already wrote down `` `nobody-here` is not a
//! member of this work ``, and that is the half a person can act on. See
//! [`trouble`].

use std::collections::{HashMap, HashSet};

use jod_core::team::{Envelope, MailState, Scope, ThreadState};

/// Which scope's traffic the screen is showing.
///
/// Held on [`super::App`] rather than inside [`Log`], because the log is
/// rebuilt from the store on every tick and the *request* has to outlive it —
/// a scope kept only on the loaded data would be forgotten by the first
/// refresh after opening the screen.
///
/// **Integration point.** `scope` is a [`Scope`] rather than a work id because
/// one bus serves both scopes (G3.S2), and `super::data::traffic_from` reads
/// either. Nothing opens a *team's* traffic today for one reason only: the
/// fleet tree is what this screen hangs off, and a team has no node in it.
/// Giving the team screen its own `T` is the whole of that change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watching {
    pub scope: Scope,
    /// The work id or the team name, according to `scope`.
    pub id: String,
}

impl Watching {
    pub fn work(id: impl Into<String>) -> Watching {
        Watching {
            scope: Scope::Work,
            id: id.into(),
        }
    }
}

/// What the `f` key narrows the log to, in the order it walks through them.
///
/// `Everything` is first because it is the resting state and a cycle key has to
/// return to it — the same shape [`super::rail`]'s kind filter has, so there is
/// one thing to learn. `Problems` is second because it is what people come to
/// this screen for: G5.S4 exists so that a message nobody could receive is
/// findable rather than buried three threads down in a working conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Everything,
    /// Failed and undeliverable — everything that did not reach anybody.
    Problems,
    /// On the bus and not yet read.
    Waiting,
    Delivered,
}

impl Shown {
    pub const ALL: [Shown; 4] = [
        Shown::Everything,
        Shown::Problems,
        Shown::Waiting,
        Shown::Delivered,
    ];

    pub fn next(self) -> Shown {
        let at = Shown::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Shown::ALL[(at + 1) % Shown::ALL.len()]
    }

    /// What the screen says it is showing. A filter nobody can see is a screen
    /// that looks empty for no reason.
    pub fn label(self) -> &'static str {
        match self {
            Shown::Everything => "every message",
            Shown::Problems => "failed and undelivered",
            Shown::Waiting => "waiting to be read",
            Shown::Delivered => "delivered",
        }
    }

    fn admits(self, state: MailState) -> bool {
        match self {
            Shown::Everything => true,
            Shown::Problems => matches!(state, MailState::Failed | MailState::Undeliverable),
            Shown::Waiting => state == MailState::Queued,
            Shown::Delivered => state == MailState::Delivered,
        }
    }
}

/// One scope's traffic, and everything the screen says about it beyond the
/// messages themselves.
///
/// Loaded on the tick like every other screen's rows, because agents write to
/// this bus from other processes — an in-memory copy could never be
/// authoritative.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Log {
    /// What the title bar calls this scope: a work's title, or a team's name.
    pub title: String,
    /// The work's colour, so one work's traffic is distinguishable from
    /// another's at a glance — the same tint the fleet tree gives its rows.
    /// Empty for a team, which has no colour.
    pub colour: String,
    /// Messages that have spent the budget, and the budget they spent.
    ///
    /// Both are read from the store rather than off `works.messages_used`,
    /// because [`jod_core::team::Store::bounds_for`] is the function that
    /// actually refuses a message — a second number on the screen that could
    /// disagree with it would be worse than no number.
    pub used: i64,
    pub budget: i64,
    /// Oldest first, exactly as the store returned them.
    pub messages: Vec<Envelope>,
    /// Which threads may not carry another hop, by thread id.
    ///
    /// G4.S3 pauses a *thread* and never the work, so this is per thread and
    /// not a flag on the log.
    pub paused: HashMap<String, ThreadState>,
    /// Messages that are waiting and will never be delivered, because the work
    /// they were addressed into is over.
    ///
    /// `queued` is the honest state for these — nothing failed — but a reader
    /// who saw only that word would be waiting for something that is never
    /// coming. [`jod_core::team::Store::mail_held`] is the one place that knows
    /// the difference.
    pub held: HashSet<i64>,
}

impl Log {
    /// Messages this scope may still exchange.
    ///
    /// G4.S5: the budget is on screen *before* it is spent, because two agents
    /// in a polite loop spend money at machine speed and the escalation card is
    /// far too late to be the first time anybody hears about it.
    pub fn budget_left(&self) -> i64 {
        (self.budget - self.used).max(0)
    }

    /// How many messages did not reach anybody. The one count worth putting in
    /// a title.
    pub fn troubled(&self) -> usize {
        self.messages
            .iter()
            .filter(|e| trouble(e, self.held.contains(&e.message.id)).is_some())
            .count()
    }

    pub fn threads(&self) -> usize {
        self.messages
            .iter()
            .map(|e| e.thread_id.as_str())
            .collect::<HashSet<_>>()
            .len()
    }
}

/// What went wrong with one message, in the words the store recorded.
///
/// `None` for a message that arrived or is on its way — those are not news, and
/// a screen that annotated every row equally would be one where the annotation
/// stops being read.
///
/// The `detail` is the point rather than the decoration. `undeliverable` says
/// the machine noticed; `` `nobody-here` is not a member of this work `` says
/// what to do about it, and that sentence is already in the row.
pub fn trouble(envelope: &Envelope, held: bool) -> Option<String> {
    match envelope.state {
        MailState::Undeliverable => Some(format!(
            "undeliverable — {}",
            reason(envelope, "nobody could receive it")
        )),
        MailState::Failed => Some(format!(
            "failed — {}",
            reason(envelope, "it was handed over and never arrived")
        )),
        // Nothing failed here: the work closed under a message that was already
        // on the bus. Reported rather than left reading as ordinary mail,
        // because "queued" promises a delivery that is not coming.
        MailState::Queued if held => {
            Some("held — this work is closed, so it will not be delivered".to_string())
        }
        _ => None,
    }
}

fn reason<'a>(envelope: &'a Envelope, fallback: &'a str) -> &'a str {
    match envelope.detail.as_deref() {
        Some(detail) if !detail.trim().is_empty() => detail,
        _ => fallback,
    }
}

/// The one word beside every message, whatever state it is in.
///
/// Colour is never the only channel in this program, and neither is a glyph —
/// this is the third, and the one that survives a screenshot pasted into a
/// ticket.
pub fn state_word(envelope: &Envelope, held: bool) -> &'static str {
    match envelope.state {
        MailState::Delivered => "delivered",
        MailState::Queued if held => "held",
        MailState::Queued => "waiting",
        MailState::Failed => "failed",
        MailState::Undeliverable => "undeliverable",
    }
}

/// The glyph in the gutter, so a failure is visible before anything is read.
pub fn glyph(envelope: &Envelope, held: bool) -> &'static str {
    match envelope.state {
        MailState::Delivered => "·",
        MailState::Queued if held => "⏸",
        MailState::Queued => "⋯",
        MailState::Failed | MailState::Undeliverable => "✗",
    }
}

/// How far a reply is indented past the question it answers.
///
/// Capped, because the depth bound allows twelve hops and twenty-four cells of
/// indent at eighty columns would leave nothing for the message. Past the cap
/// the row still says its depth in figures — see [`depth_marker`] — so a deep
/// thread is legible rather than merely narrow.
pub const DEEPEST_INDENT: i64 = 5;

pub fn indent(depth: i64) -> String {
    "  ".repeat(depth.clamp(0, DEEPEST_INDENT) as usize)
}

/// The figure printed beside a reply the indent could no longer show.
pub fn depth_marker(depth: i64) -> String {
    if depth > DEEPEST_INDENT {
        format!("+{depth} ")
    } else {
        String::new()
    }
}

/// The messages on screen, threaded and in the order they are drawn.
///
/// Three passes, and the order matters:
///
/// 1. **Which messages match** — the state cycle and the `/` filter.
/// 2. **Plus every ancestor of a match.** The same rule the fleet tree follows,
///    for the same reason: a reply at depth three floating with nothing above
///    it reads as a rendering fault rather than as a filter, and the question
///    it answers is usually what you were looking for anyway.
/// 3. **Grouped into threads**, threads ordered by `sort`, messages inside a
///    thread always oldest first — a reply belongs under what it answers
///    whichever way the threads themselves are stacked.
pub fn rows<'a>(
    messages: &'a [Envelope],
    held: &HashSet<i64>,
    shown: Shown,
    needle: Option<&str>,
    sort: usize,
) -> Vec<&'a Envelope> {
    let keep = matching(messages, held, shown, needle);
    if keep.is_empty() {
        return Vec::new();
    }

    // Threads in encounter order, so a sort that ties leaves them chronological
    // rather than in whatever order a hash map happened to hold them.
    let mut order: Vec<&str> = Vec::new();
    let mut threads: HashMap<&str, Vec<&Envelope>> = HashMap::new();
    for (at, envelope) in messages.iter().enumerate() {
        if !keep.contains(&at) {
            continue;
        }
        let thread = envelope.thread_id.as_str();
        if !threads.contains_key(thread) {
            order.push(thread);
        }
        threads.entry(thread).or_default().push(envelope);
    }

    order.sort_by_key(|thread| key(&threads[thread], sort));
    order
        .into_iter()
        .flat_map(|thread| threads.remove(thread).unwrap_or_default())
        .collect()
}

/// What orders one thread against another, for the sort in force.
///
/// A tuple rather than a comparator so the tie-break is written once: every
/// order ends with the thread's newest message descending, which is what makes
/// two threads by the same sender read newest first rather than arbitrarily.
fn key(thread: &[&Envelope], sort: usize) -> (i64, String, std::cmp::Reverse<i64>) {
    let newest = thread.iter().map(|e| e.message.id).max().unwrap_or(0);
    let oldest = thread.iter().map(|e| e.message.id).min().unwrap_or(0);
    let started_by = thread
        .first()
        .map(|e| e.message.from.clone())
        .unwrap_or_default();
    let troubled = thread
        .iter()
        .any(|e| matches!(e.state, MailState::Failed | MailState::Undeliverable));
    let newest_first = std::cmp::Reverse(newest);
    match sort % SORTS.len() {
        // "oldest": the work read from the beginning.
        1 => (oldest, String::new(), newest_first),
        // "sender": whose conversations these are.
        2 => (0, started_by, newest_first),
        // "problems first": the messages that did not arrive, then the rest.
        3 => (i64::from(!troubled), String::new(), newest_first),
        // "newest": what just happened, without scrolling for it.
        _ => (0, String::new(), newest_first),
    }
}

/// The orders `S` cycles through, in the order it cycles them.
///
/// Declared here rather than only in [`super::workspace::Workspace::sorts`]
/// because [`key`] indexes into the same list, and two copies of an ordering
/// are two things that can disagree about which number means "sender".
pub const SORTS: [&str; 4] = ["newest", "oldest", "sender", "problems first"];

/// Which messages survive the filters — every hit, plus every ancestor of one.
fn matching(
    messages: &[Envelope],
    held: &HashSet<i64>,
    shown: Shown,
    needle: Option<&str>,
) -> HashSet<usize> {
    let needle = needle.filter(|n| !n.trim().is_empty());
    let mut keep: HashSet<usize> = HashSet::new();
    let mut at_of: HashMap<i64, usize> = HashMap::new();
    for (at, envelope) in messages.iter().enumerate() {
        at_of.insert(envelope.message.id, at);
    }
    for (at, envelope) in messages.iter().enumerate() {
        if !shown.admits(envelope.state) {
            continue;
        }
        if let Some(needle) = needle {
            // The reason a message failed is searchable too. It is the half of
            // the row a person actually reads on a failure, so a filter that
            // could not find it would send them back to `sqlite3`.
            let haystack = format!(
                "{} {} {} {}",
                envelope.message.from,
                envelope.message.to,
                envelope.message.text,
                trouble(envelope, held.contains(&envelope.message.id)).unwrap_or_default()
            );
            if !super::workspace::matches(needle, &haystack) {
                continue;
            }
        }
        keep.insert(at);
        // Climb by `in_reply_to` rather than by scanning backwards within the
        // thread: a thread can branch, and the previous message in it is not
        // always the one this answers.
        let mut parent = envelope.in_reply_to;
        while let Some(id) = parent {
            let Some(up) = at_of.get(&id).copied() else {
                break;
            };
            if !keep.insert(up) {
                // Already kept, so its ancestors are too.
                break;
            }
            parent = messages[up].in_reply_to;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::team::{Kind, Message};

    fn envelope(id: i64, from: &str, to: &str, text: &str, thread: &str, depth: i64) -> Envelope {
        Envelope {
            message: Message {
                id,
                team: "w1".into(),
                from: from.into(),
                to: to.into(),
                text: text.into(),
                at_ms: id * 1_000,
            },
            scope: Scope::Work,
            thread_id: thread.into(),
            in_reply_to: None,
            depth,
            kind: Kind::Message,
            state: MailState::Delivered,
            detail: None,
        }
    }

    /// One thread of three, and a second thread started later.
    fn conversation() -> Vec<Envelope> {
        let mut question = envelope(1, "asker", "answerer", "where is the lexer?", "t1", 0);
        question.state = MailState::Delivered;
        let mut answer = envelope(2, "answerer", "asker", "in core", "t1", 1);
        answer.in_reply_to = Some(1);
        let mut thanks = envelope(3, "asker", "answerer", "thank you", "t1", 2);
        thanks.in_reply_to = Some(2);
        let later = envelope(4, "scribe", "asker", "the docs are done", "t2", 0);
        vec![question, answer, thanks, later]
    }

    fn nothing() -> HashSet<i64> {
        HashSet::new()
    }

    /// G5.S1: a reply sits under what it answers, whichever way the threads
    /// themselves are stacked.
    #[test]
    fn a_thread_stays_in_order_even_when_the_newest_thread_is_on_top() {
        let messages = conversation();
        let rows = rows(&messages, &nothing(), Shown::Everything, None, 0);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(
            ids,
            vec![4, 1, 2, 3],
            "the newest thread leads, and its own messages stay oldest first"
        );
    }

    #[test]
    fn the_oldest_order_reads_the_work_from_the_beginning() {
        let messages = conversation();
        let rows = rows(&messages, &nothing(), Shown::Everything, None, 1);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn sorting_by_sender_groups_a_threads_opener() {
        let messages = conversation();
        let rows = rows(&messages, &nothing(), Shown::Everything, None, 2);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4], "`asker` sorts before `scribe`");
    }

    /// The order that answers "did anything not arrive", which is the question
    /// this screen exists for.
    #[test]
    fn problems_first_lifts_the_thread_that_failed_above_the_newer_one() {
        let mut messages = conversation();
        messages[0].state = MailState::Undeliverable;
        messages[0].detail = Some("`answerer` is not a member of this work".into());
        let rows = rows(&messages, &nothing(), Shown::Everything, None, 3);
        assert_eq!(rows[0].message.id, 1, "the broken thread leads");
        assert_eq!(rows.last().unwrap().message.id, 4);
    }

    /// The whole of G5.S4 in one assertion: the row says *why*, in the words
    /// the store wrote, and not merely that something went wrong.
    #[test]
    fn an_undeliverable_message_carries_the_reason_it_was_refused() {
        let mut refused = envelope(3, "asker", "reljod", "are you there?", "t3", 0);
        refused.state = MailState::Undeliverable;
        refused.detail = Some("`reljod` is not a member of this team".into());
        let said = trouble(&refused, false).expect("a refusal is news");
        assert!(said.starts_with("undeliverable — "), "{said}");
        assert!(
            said.contains("`reljod` is not a member of this team"),
            "the state word alone is not something anybody can act on: {said}"
        );
        assert_eq!(state_word(&refused, false), "undeliverable");
        assert_eq!(glyph(&refused, false), "✗");
    }

    /// A refusal with no detail must still say something, or the row is a bare
    /// word after all.
    #[test]
    fn a_refusal_with_nothing_written_down_still_says_what_happened() {
        let mut refused = envelope(1, "asker", "ghost", "hello", "t1", 0);
        refused.state = MailState::Undeliverable;
        assert_eq!(
            trouble(&refused, false).as_deref(),
            Some("undeliverable — nobody could receive it")
        );
    }

    #[test]
    fn a_failed_handover_reads_as_failed_rather_than_as_refused() {
        let mut lost = envelope(1, "asker", "answerer", "hello", "t1", 0);
        lost.state = MailState::Failed;
        lost.detail = Some("the session died before its first turn".into());
        let said = trouble(&lost, false).expect("a failure is news");
        assert!(said.starts_with("failed — "), "{said}");
        assert!(said.contains("died before its first turn"), "{said}");
    }

    /// `queued` is the true state and the misleading word: nothing failed, and
    /// nothing is coming either.
    #[test]
    fn mail_held_by_a_closed_work_says_so_rather_than_reading_as_ordinary_queue() {
        let waiting = envelope(7, "asker", "answerer", "one more thing", "t1", 0);
        let mut waiting = waiting;
        waiting.state = MailState::Queued;
        assert_eq!(state_word(&waiting, false), "waiting");
        assert_eq!(trouble(&waiting, false), None, "an ordinary queue is not news");

        assert_eq!(state_word(&waiting, true), "held");
        let said = trouble(&waiting, true).expect("mail nobody will read is news");
        assert!(said.contains("will not be delivered"), "{said}");
    }

    #[test]
    fn a_delivered_message_has_nothing_to_report() {
        let arrived = envelope(1, "asker", "answerer", "hello", "t1", 0);
        assert_eq!(trouble(&arrived, false), None);
        assert_eq!(state_word(&arrived, false), "delivered");
    }

    /// The state cycle narrows to what went wrong, and comes back.
    #[test]
    fn the_state_filter_reaches_the_failures_and_returns_to_everything() {
        let mut messages = conversation();
        messages[1].state = MailState::Undeliverable;

        let all = rows(&messages, &nothing(), Shown::Everything, None, 1);
        assert_eq!(all.len(), 4);

        let problems = rows(&messages, &nothing(), Shown::Problems, None, 1);
        let ids: Vec<i64> = problems.iter().map(|e| e.message.id).collect();
        assert_eq!(
            ids,
            vec![1, 2],
            "the refusal, and the question it was answering"
        );

        let mut shown = Shown::Everything;
        for _ in 0..Shown::ALL.len() {
            shown = shown.next();
        }
        assert_eq!(shown, Shown::Everything, "the cycle closes");
    }

    /// The tree's rule, applied to a conversation: a reply kept without the
    /// question it answers floats at a depth with nothing above it.
    #[test]
    fn filtering_keeps_every_message_a_hit_was_answering() {
        let messages = conversation();
        let rows = rows(&messages, &nothing(), Shown::Everything, Some("thank"), 1);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "the whole chain down to the hit");
    }

    #[test]
    fn a_filter_finds_a_sender_as_well_as_a_word_in_the_message() {
        let messages = conversation();
        let rows = rows(&messages, &nothing(), Shown::Everything, Some("scribe"), 1);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![4]);
    }

    /// The reason is the half of a failed row anybody reads, so it has to be
    /// searchable — otherwise finding one means leaving the program.
    #[test]
    fn a_filter_searches_the_reason_a_message_was_refused() {
        let mut messages = conversation();
        messages[3].state = MailState::Undeliverable;
        messages[3].detail = Some("`nobody-here` is not a member of this work".into());
        let rows = rows(&messages, &nothing(), Shown::Everything, Some("nobody-here"), 1);
        let ids: Vec<i64> = rows.iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![4]);
    }

    #[test]
    fn an_open_but_empty_filter_hides_nothing() {
        let messages = conversation();
        assert_eq!(
            rows(&messages, &nothing(), Shown::Everything, Some("  "), 1).len(),
            4
        );
    }

    #[test]
    fn a_filter_matching_nothing_empties_the_log() {
        let messages = conversation();
        assert!(rows(&messages, &nothing(), Shown::Everything, Some("zzzz"), 1).is_empty());
    }

    /// Twelve hops of indent would leave nothing for the message at eighty
    /// columns, so the indent stops and the figure takes over.
    #[test]
    fn a_deep_reply_stops_indenting_and_says_its_depth_instead() {
        assert_eq!(indent(0), "");
        assert_eq!(indent(2), "    ");
        assert_eq!(
            indent(11).chars().count(),
            indent(DEEPEST_INDENT).chars().count(),
            "the indent is capped"
        );
        assert_eq!(depth_marker(2), "", "a shallow reply needs no figure");
        assert_eq!(depth_marker(11), "+11 ");
    }

    /// G4.S5: the number is on screen before it is spent, not after.
    #[test]
    fn the_log_reports_what_is_left_of_the_budget() {
        let log = Log {
            used: 12,
            budget: 200,
            ..Default::default()
        };
        assert_eq!(log.budget_left(), 188);
    }

    /// A budget already overspent must read as nothing left rather than as a
    /// negative number, which looks like a bug in the screen.
    #[test]
    fn a_spent_budget_bottoms_out_at_nothing_left() {
        let log = Log {
            used: 220,
            budget: 200,
            ..Default::default()
        };
        assert_eq!(log.budget_left(), 0);
    }

    #[test]
    fn the_log_counts_its_threads_and_its_failures() {
        let mut messages = conversation();
        messages[3].state = MailState::Undeliverable;
        let log = Log {
            messages,
            ..Default::default()
        };
        assert_eq!(log.threads(), 2);
        assert_eq!(log.troubled(), 1);
    }

    /// Mail held by a closed work counts as trouble too — it is the case that
    /// looks fine in the state column and is not.
    #[test]
    fn held_mail_counts_among_the_failures_the_title_reports() {
        let mut messages = conversation();
        messages[3].state = MailState::Queued;
        let log = Log {
            messages,
            held: [4].into_iter().collect(),
            ..Default::default()
        };
        assert_eq!(log.troubled(), 1);
    }

    /// The sort names and the sort keys are indexed by the same number, so a
    /// screen offering four orders and a key handling three would silently give
    /// two of them the same behaviour.
    #[test]
    fn every_named_sort_order_is_one_the_key_function_implements() {
        let messages = conversation();
        let mut seen: Vec<Vec<i64>> = Vec::new();
        for sort in 0..SORTS.len() {
            let ids: Vec<i64> = rows(&messages, &nothing(), Shown::Everything, None, sort)
                .iter()
                .map(|e| e.message.id)
                .collect();
            assert_eq!(ids.len(), 4, "{} dropped a message", SORTS[sort]);
            seen.push(ids);
        }
        assert_ne!(seen[0], seen[1], "newest and oldest cannot be the same order");
    }
}
