//! Whether the reply a run owed somebody ever actually arrived.
//!
//! The fleet screen knows a great deal about a run and one thing not at all:
//! that it finished, said its piece, and the person it was for heard nothing.
//! `core::ledger` has recorded exactly that since it was wired, and until now
//! the only way to read it was `jod ledger` — which means leaving the program
//! that is telling you the run is `completed`. A ledger consulted in another
//! room answers its question too late to change what anybody does.
//!
//! **This is deliberately not a second `jod ledger`.** That command answers
//! "show me the ledger"; the fleet asks a narrower question about one row —
//! *is anything wrong with this run's delivery* — and the honest answer is
//! usually one line. Most runs owe nobody anything: a run started from the TUI
//! reports into the transcript you are already looking at, and only a Telegram
//! turn puts Jod under an obligation to reach somebody who is not here.
//!
//! The join is free. `NewMessage::about_run` records the agent id on the
//! obligation, and a fleet row *is* an agent id, so nothing new was needed in
//! the store to ask this.

use jod_core::ledger::{DeliveryState, Obligation};
use jod_core::store::Store;

use super::app::short_duration;

/// How many rows are searched for the selected run's messages.
///
/// The ledger prunes itself to `MAX_ROWS`, so this is "all of it". A page would
/// mean a run whose reply failed last week reading as though it never owed one,
/// which is the single answer this must never give by accident.
fn everything() -> usize {
    jod_core::ledger::MAX_ROWS as usize
}

/// What the ledger has to say about one run, in the order of how much it
/// matters.
///
/// Ordered rather than merely collected, because the screen shows the first
/// line loudest and the caller should not have to decide which of three
/// obligations is the bad one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Somebody was owed something and never got it. The one this exists for.
    Lost,
    /// Still owed — either untouched or in flight right now.
    Owed,
    /// It arrived, but it was resent after a crash, so they may hold two.
    Twice,
    /// It arrived, once, cleanly.
    Fine,
    /// This run owed nobody anything, which is the common case and is *not*
    /// the same as "fine". Saying "delivered" about a message that never
    /// existed is how a reader learns to distrust the good news.
    ///
    /// The default, so a row built before its ledger is consulted claims
    /// nothing rather than claiming success. Every other variant is an
    /// assertion about a message; this one is the absence of one.
    #[default]
    Nothing,
}

impl Verdict {
    /// The verdict for one row, from the two facts that decide it.
    fn of(o: &Obligation) -> Verdict {
        match o.state {
            DeliveryState::Failed => Verdict::Lost,
            DeliveryState::Pending | DeliveryState::Attempting => Verdict::Owed,
            // `may_be_a_duplicate` rather than a bare null check, so this stays
            // true to the rule the ledger states rather than re-deriving it.
            DeliveryState::Delivered if o.may_be_a_duplicate() => Verdict::Twice,
            DeliveryState::Delivered => Verdict::Fine,
        }
    }

    /// Whether anything here is worth saying out loud when somebody asks.
    ///
    /// Includes [`Verdict::Owed`], because "your reply has not gone yet" is a
    /// real answer to a question somebody just asked. It is **not** the rule
    /// for the fleet row — see [`Verdict::marks_a_row`], which is narrower and
    /// deliberately so.
    pub fn is_trouble(self) -> bool {
        matches!(self, Verdict::Lost | Verdict::Owed | Verdict::Twice)
    }

    /// Whether a fleet row should wear a mark for this, unasked.
    ///
    /// Narrower than [`Verdict::is_trouble`] by exactly one variant, and the
    /// difference is the whole design of a passive marker. `Owed` covers a
    /// reply **in flight right now**, which is the ordinary state of every
    /// Telegram run for the seconds between the answer existing and the
    /// transport confirming it. A glyph drawn from that appears routinely, and
    /// a marker that appears routinely is one people stop seeing — the same
    /// argument that keeps `Nothing` unmarked, applied to the other end.
    ///
    /// So the row marks only what is already wrong and will not fix itself:
    /// a message nobody got, or one somebody may hold twice.
    pub fn marks_a_row(self) -> bool {
        matches!(self, Verdict::Lost | Verdict::Twice)
    }

    /// The mark for this verdict, so a second surface does not invent its own.
    ///
    /// `Lost` is `⊘` rather than the obvious `✗` because of where these are
    /// drawn: on a fleet row, two cells from `ui.rs`'s own status glyph, whose
    /// set is `● ✓ ✗ ■ ○`. A failed run whose reply was also lost would have
    /// rendered `✗ ✗` — two identical marks meaning different things, which is
    /// worse than no marker at all. `⊘` reads as "nothing got through" and
    /// collides with neither that set nor the marks reserved elsewhere in the
    /// TUI (`!` for a contradicted memory, `⚑` for unread).
    ///
    /// `Owed`, `Fine` and `Nothing` do collide with that set, and it does not
    /// matter: [`Verdict::marks_a_row`] means they never reach a row. They
    /// appear only in this module's own answer, where nothing else is nearby.
    pub fn glyph(self) -> &'static str {
        match self {
            Verdict::Lost => "⊘",
            Verdict::Owed => "○",
            Verdict::Twice => "♻",
            Verdict::Fine => "●",
            Verdict::Nothing => "·",
        }
    }
}

/// One message this run owed, judged.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub verdict: Verdict,
    pub target: String,
    pub attempts: i64,
    pub detail: Option<String>,
    pub recovered_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

/// Everything the ledger holds about one run, worst first.
///
/// Errors are swallowed the way every loader in `data.rs` swallows them: a
/// locked database costs one stale answer, never the session.
pub fn replies_for(store: &Store, run_id: &str) -> Vec<Reply> {
    let mut rows: Vec<Reply> = store
        .obligations(everything())
        .unwrap_or_default()
        .into_iter()
        .filter(|o| o.run_id.as_deref() == Some(run_id))
        .map(|o| Reply {
            verdict: Verdict::of(&o),
            target: format!("{}→{}", o.channel, o.target),
            attempts: o.attempts,
            detail: o.detail,
            recovered_at_ms: o.recovered_at_ms,
            updated_at_ms: o.updated_at_ms,
        })
        .collect();
    // Worst first. `Verdict` is ordered so that `Lost` sorts before `Fine`,
    // which is the whole reason it is an enum rather than a string.
    rows.sort_by_key(|r| (r.verdict, std::cmp::Reverse(r.updated_at_ms)));
    rows
}

/// One run's verdict, in one call, for a loader that has a row to fill.
///
/// The shape `list_agents` needs: it holds a run id and has room for a single
/// value, not a list. A run can owe more than one message, and the worst of
/// them is what the row must show — `Lost` beside `Fine` is still `Lost` to
/// somebody looking at that row, and a marker that averaged them would be a
/// marker that hides the only case worth drawing.
pub fn verdict_of_run(store: &Store, run_id: &str) -> Verdict {
    verdict_for(&replies_for(store, run_id))
}

/// The single verdict for a run, for a caller that already has the replies.
pub fn verdict_for(replies: &[Reply]) -> Verdict {
    replies
        .iter()
        .map(|r| r.verdict)
        .min()
        .unwrap_or(Verdict::Nothing)
}

/// What the fleet screen says when asked about the selected run.
///
/// Answers in whole sentences rather than a table, because this is a question
/// asked once about one row — not a listing to be scanned. `jod ledger` is the
/// listing.
pub fn about_run(store: &Store, run_id: &str, now_ms: i64) -> Vec<String> {
    let replies = replies_for(store, run_id);
    let short: String = run_id.chars().take(8).collect();

    // Asked as "what is this run's verdict" rather than "is the list empty",
    // because those are the same test only by accident today: the day a run can
    // owe something the ledger declines to judge, `is_empty` would call it
    // fine and this says what it means.
    if verdict_for(&replies) == Verdict::Nothing {
        // Deliberately not "delivered" and deliberately not silence. A run that
        // owed nothing is a different fact from a run whose message arrived,
        // and a key that did nothing visible would read as a key that failed.
        return vec![format!(
            "{short} owed nobody a message — nothing was promised outside this screen"
        )];
    }

    // One owed message is a sentence about the run. Several are a *list*, and
    // repeating "run-9's reply" above each of them says they are the same
    // reply — which is exactly the case where they are not, and the case where
    // one of them went missing and the other did not.
    let mut out = Vec::new();
    if replies.len() > 1 {
        out.push(summary(&short, &replies));
    }
    for r in &replies {
        out.push(match replies.len() {
            1 => format!(
                "{} {} · {}",
                r.verdict.glyph(),
                alone(r.verdict, &short),
                r.target
            ),
            _ => format!("{} {} · {}", r.verdict.glyph(), among(r.verdict), r.target),
        });
        for note in notes(r, now_ms) {
            out.push(format!("  {note}"));
        }
    }
    out
}

/// The heading over several owed messages: how many, and how many went wrong.
///
/// Counted rather than left to be inferred, because the reason to show a list
/// at all is that its rows disagree — a run that reached one person and not
/// another is a run with a problem, and a reader who stops at the first line
/// should already know that.
fn summary(short: &str, replies: &[Reply]) -> String {
    let trouble = replies.iter().filter(|r| r.verdict.is_trouble()).count();
    let owed = format!("{short} owed {} messages", replies.len());
    match trouble {
        0 => format!("{owed} — all arrived"),
        n if n == replies.len() => format!("{owed} — none of them arrived cleanly"),
        n => format!("{owed}, and {n} did not arrive cleanly"),
    }
}

/// The whole answer, when there is only one message to answer about.
fn alone(verdict: Verdict, short: &str) -> String {
    match verdict {
        Verdict::Lost => format!("{short}'s reply never arrived"),
        Verdict::Owed => format!("{short}'s reply has not gone yet"),
        Verdict::Twice => format!("{short}'s reply arrived, possibly twice"),
        Verdict::Fine => format!("{short}'s reply arrived"),
        Verdict::Nothing => format!("{short} owed nobody a message"),
    }
}

/// One row of several. The run is named once, above; these describe the
/// message, and the recipient after the dot is what tells them apart.
fn among(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Lost => "never arrived",
        Verdict::Owed => "not gone yet",
        Verdict::Twice => "arrived, possibly twice",
        Verdict::Fine => "arrived",
        Verdict::Nothing => "nothing owed",
    }
}

/// The second line, and only when there is something to say on it.
fn notes(r: &Reply, now_ms: i64) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(at) = r.recovered_at_ms {
        notes.push(format!(
            "resent after a crash {} ago — they may hold two copies",
            short_duration(now_ms.saturating_sub(at))
        ));
    }
    // The reason, on the verdict that has one. `jod ledger failed` learned this
    // the hard way: a failure without its reason sends the reader somewhere
    // else to find out, which is the trip this whole surface exists to save.
    if let Some(detail) = &r.detail {
        notes.push(one_line(detail));
    }
    if r.attempts > 1 {
        notes.push(format!("{} attempts", r.attempts));
    }
    notes
}

/// One line, bounded. A transport's error can be a paragraph.
fn one_line(s: &str) -> String {
    const WIDTH: usize = 88;
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= WIDTH {
        return flat;
    }
    format!("{}…", flat.chars().take(WIDTH - 1).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::ledger::{NewMessage as Owed, Owner};

    fn store() -> Store {
        Store::in_memory().expect("an in-memory store")
    }

    fn owner() -> Owner {
        Owner::new("jod-cloud", 4821)
    }

    /// One obligation for `run`, left in `state`.
    fn owed(store: &Store, key: &str, run: &str, body: &str) -> i64 {
        store
            .record_obligation(
                &Owed::new(key, "telegram", "7", body).about_run(run),
                &owner(),
                1_000,
            )
            .expect("an obligation")
    }

    /// The common case, and the one a careless answer gets wrong. Most runs
    /// report into the transcript and owe nobody anything; calling that
    /// "delivered" would teach a reader to distrust the good news.
    #[test]
    fn a_run_that_owed_nobody_says_so_rather_than_claiming_success() {
        let s = store();
        let said = about_run(&s, "run-7", 5_000);

        assert_eq!(said.len(), 1);
        assert!(said[0].contains("owed nobody"), "{said:?}");
        assert!(
            !said[0].contains("arrived"),
            "silence must not read as delivery: {said:?}"
        );
        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Nothing);
    }

    /// The question the whole surface exists to answer, from the screen that
    /// otherwise says `completed` and nothing else.
    #[test]
    fn a_run_whose_reply_was_lost_says_so_and_says_why() {
        let s = store();
        let id = owed(&s, "telegram:7:1", "run-7", "the nightly digest");
        // Out of attempts, so the row settles as failed with its reason.
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            s.mark_attempting(id, &owner(), 2_000).unwrap();
            s.mark_failed(id, "Unauthorized: bot was removed from the chat", 2_000)
                .unwrap();
        }

        let said = about_run(&s, "run-7", 5_000);

        assert!(said[0].contains("never arrived"), "{said:?}");
        assert!(
            said[0].starts_with(Verdict::Lost.glyph()),
            "the mark, not a hard-coded copy of it: {said:?}"
        );
        assert!(
            said.iter().any(|l| l.contains("bot was removed")),
            "the reason has to be here, not one command away: {said:?}"
        );
        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Lost);
    }

    #[test]
    fn a_reply_still_waiting_is_not_reported_as_delivered() {
        let s = store();
        owed(&s, "telegram:7:1", "run-7", "the nightly digest");

        let said = about_run(&s, "run-7", 5_000);

        assert!(said[0].contains("has not gone yet"), "{said:?}");
        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Owed);
    }

    /// A delivered message that was resent after a crash is not the same fact
    /// as one that went once, and the person holding two copies is the one
    /// asking.
    #[test]
    fn a_reply_that_was_resent_after_a_crash_says_it_may_be_a_duplicate() {
        let s = store();
        let id = owed(&s, "telegram:7:1", "run-7", "the nightly digest");
        s.mark_attempting(id, &owner(), 2_000).unwrap();
        // Interrupted, swept by a later process, then it landed.
        s.sweep_recoverable(
            &Owner::new("jod-cloud", 9001),
            &jod_core::ledger::LocalProcesses {
                machine: "jod-cloud".into(),
            },
            "telegram",
            3_000,
        )
        .unwrap();
        s.mark_delivered(id, 4_000).unwrap();

        let said = about_run(&s, "run-7", 5_000);

        assert!(said[0].contains("possibly twice"), "{said:?}");
        assert!(said.iter().any(|l| l.contains("two copies")), "{said:?}");
        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Twice);
    }

    #[test]
    fn a_reply_that_simply_arrived_says_so_in_one_line() {
        let s = store();
        let id = owed(&s, "telegram:7:1", "run-7", "the nightly digest");
        s.mark_attempting(id, &owner(), 2_000).unwrap();
        s.mark_delivered(id, 3_000).unwrap();

        let said = about_run(&s, "run-7", 5_000);

        assert_eq!(said.len(), 1, "nothing to add: {said:?}");
        assert!(said[0].contains("arrived"), "{said:?}");
        assert!(!said[0].contains("twice"), "{said:?}");
        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Fine);
    }

    /// A run that owed two people and reached one of them is a run with a
    /// problem. The bad news goes first and decides the run's verdict, because
    /// a reader who stops after one line must not stop after the good one.
    #[test]
    fn the_worst_news_about_a_run_is_the_first_thing_said() {
        let s = store();
        let fine = owed(&s, "telegram:7:1", "run-7", "went fine");
        s.mark_attempting(fine, &owner(), 2_000).unwrap();
        s.mark_delivered(fine, 2_000).unwrap();

        let lost = owed(&s, "telegram:8:1", "run-7", "never made it");
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            s.mark_attempting(lost, &owner(), 2_000).unwrap();
            s.mark_failed(lost, "chat not found", 2_000).unwrap();
        }

        let replies = replies_for(&s, "run-7");
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].verdict,
            Verdict::Lost,
            "the good news came first"
        );
        assert_eq!(
            verdict_for(&replies),
            Verdict::Lost,
            "one lost reply makes the run's answer `lost`"
        );
        // The heading comes first when there are several, and it says the run
        // has a problem before any individual row does.
        let said = about_run(&s, "run-7", 5_000);
        assert!(said[0].contains("owed 2 messages"), "{said:?}");
        assert!(said[0].contains("1 did not arrive"), "{said:?}");
        assert!(said[1].contains("never arrived"), "{said:?}");
        assert!(
            !said[1].contains("run-7's reply"),
            "naming the run on every row says they are the same reply: {said:?}"
        );
    }

    /// Another run's trouble is not this run's trouble. Obvious, and the kind
    /// of filter that is wrong in exactly one direction the day it breaks.
    #[test]
    fn one_runs_lost_reply_does_not_show_up_against_another() {
        let s = store();
        let lost = owed(&s, "telegram:7:1", "run-other", "somebody else's");
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            s.mark_attempting(lost, &owner(), 2_000).unwrap();
            s.mark_failed(lost, "chat not found", 2_000).unwrap();
        }
        owed(&s, "telegram:8:1", "run-7", "mine");

        assert_eq!(verdict_for(&replies_for(&s, "run-7")), Verdict::Owed);
        assert_eq!(verdict_for(&replies_for(&s, "run-other")), Verdict::Lost);
    }

    #[test]
    fn only_the_verdicts_worth_interrupting_somebody_about_are_trouble() {
        assert!(Verdict::Lost.is_trouble());
        assert!(Verdict::Owed.is_trouble());
        assert!(Verdict::Twice.is_trouble());
        assert!(!Verdict::Fine.is_trouble());
        assert!(
            !Verdict::Nothing.is_trouble(),
            "most runs owe nobody anything, and a marker on every row is a \
             marker nobody reads"
        );
    }

    /// The passive marker is narrower than "worth saying", by exactly one
    /// variant, and the one it drops is the reason the marker is worth having.
    #[test]
    fn a_reply_merely_in_flight_does_not_earn_a_mark_on_the_row() {
        assert!(Verdict::Lost.marks_a_row(), "nobody got it");
        assert!(Verdict::Twice.marks_a_row(), "somebody may hold two");
        assert!(
            !Verdict::Owed.marks_a_row(),
            "every Telegram run is `Owed` for a few seconds, and a glyph that \
             appears routinely is one people stop seeing"
        );
        assert!(!Verdict::Fine.marks_a_row());
        assert!(!Verdict::Nothing.marks_a_row());
    }

    /// Two marks meaning different things must not be the same character on
    /// one row.
    ///
    /// `ui.rs::run_glyph` is private and in a file this module does not own, so
    /// its set is copied here rather than called. That makes this a guard on
    /// *this* side of the seam: it fails if a verdict moves onto a taken glyph,
    /// and it cannot see a sixth status arriving over there. Named so the next
    /// person knows which half is covered.
    #[test]
    fn a_row_mark_never_collides_with_the_run_status_beside_it() {
        // `ui.rs::run_glyph`, as of this test: running, completed, failed,
        // killed, and anything unrecognised.
        const RUN_GLYPHS: [&str; 5] = ["●", "✓", "✗", "■", "○"];
        for v in [
            Verdict::Lost,
            Verdict::Owed,
            Verdict::Twice,
            Verdict::Fine,
            Verdict::Nothing,
        ] {
            if !v.marks_a_row() {
                continue;
            }
            assert!(
                !RUN_GLYPHS.contains(&v.glyph()),
                "{v:?} draws `{}` two cells from a run status using the same \
                 character",
                v.glyph()
            );
        }
    }

    /// The call `list_agents` will make: a run id in, one verdict out.
    #[test]
    fn a_run_reduces_to_the_worst_of_what_it_owed() {
        let s = store();
        let fine = owed(&s, "telegram:7:1", "run-7", "went fine");
        s.mark_attempting(fine, &owner(), 2_000).unwrap();
        s.mark_delivered(fine, 2_000).unwrap();
        let lost = owed(&s, "telegram:8:1", "run-7", "never made it");
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            s.mark_attempting(lost, &owner(), 2_000).unwrap();
            s.mark_failed(lost, "chat not found", 2_000).unwrap();
        }

        assert_eq!(verdict_of_run(&s, "run-7"), Verdict::Lost);
        assert!(verdict_of_run(&s, "run-7").marks_a_row());
        assert_eq!(
            verdict_of_run(&s, "run-never-heard-of"),
            Verdict::Nothing,
            "a run that owed nothing is the common case and wears no mark"
        );
        assert!(!verdict_of_run(&s, "run-never-heard-of").marks_a_row());
    }
}
