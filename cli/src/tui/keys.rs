//! What every screen says its keys are.
//!
//! Two places have to agree: the keybar that is always on screen, and the `?`
//! overlay that carries the long tail. They are generated from one table here
//! so they cannot drift.
//!
//! ## What the bar guarantees, and what it does not
//!
//! **The way out is always printed. The verbs are printed as far as they fit.**
//!
//! The rule used to be that every one of a screen's verbs had to be on the bar,
//! because the same letter deliberately means different things on different
//! screens — `a` attaches in the fleet and answers an escalation in goals — so
//! a verb that was not printed would be a trap rather than a shortcut.
//!
//! That rule was already false when it was written down. `ui::two_ends`
//! reserved room for the verbs and dropped the right-hand half when none was
//! left, and the right-hand half is the way out: at eighty columns, chat, fleet
//! and memory all stopped saying `Esc back · ? keys`. Every render test in the
//! suite used a hundred and fifty columns, so nothing ever saw it.
//!
//! The argument order was simply backwards. Being stranded is the trap the
//! condition was written against; a terse bar is not. So the exit is reserved
//! first, `keybar` spends whatever is left, and it drops **whole** verbs —
//! half a chord teaches a key that does not exist — saying `? more` when it
//! does, so a short bar reads as short rather than as complete. `?` then opens
//! the overlay, which lists the screen's own verbs before anything else.
//!
//! ## Where a new verb goes in its table
//!
//! The budget drops from the end, so the order of a table decides what a narrow
//! terminal loses. Order every screen's verbs:
//!
//! 1. `⏎` — the primary action, always first.
//! 2. Verbs **unique to this screen**, most important first. Where two are
//!    equally important, prefer the one whose letter means something *else* on
//!    another screen: `a` attaches here and answers an escalation in goals, and
//!    printing only one half of that pair teaches a habit the other screen
//!    breaks. That is a tie-break and not a rule — importance wins. `s stop`
//!    stays above `r resume` on the fleet even though `r` is the collided
//!    letter, because stopping a run matters more than the tidiness does.
//! 3. Verbs that also appear in [`SPINE`].
//!
//! The point is that the bar should print what only this screen can teach you.
//! `n`, `e`, `x` and `/` mean the same thing on all ten screens and have their
//! own section in the overlay, so losing them off the bar costs nothing — the
//! meaning transfers. `a answer` exists on exactly one screen and can be
//! learned nowhere else, so it must outrank `e edit` even though `e` was typed
//! into the table first.
//!
//! This is why the rule is an ordering and not a special case: the budget then
//! drops the cheapest thing available by construction, on every screen and
//! every screen added later.
//! `no_verb_the_spine_already_teaches_sits_above_one_only_this_screen_has`
//! keeps it that way.
//!
//! Only step 3 is enforced by a test, deliberately. An attempt to pin step 2's
//! tie-break — "a collided letter must be printed on every screen that defines
//! it, or on none" — was written, run, and deleted: the fleet has thirteen
//! verbs, room for five at eighty columns, and seven collided letters among
//! them (`r a c u g f t`). No ordering satisfies it, so the guarantee could only
//! have been met by deleting verbs. A weaker form — collided letters first
//! within step 2 — was also tried and also deleted, because it demanded
//! `r resume` outrank `s stop` on the fleet, which is the tie-break overruling
//! importance rather than settling it. What survives is the part that is both
//! true and checkable; the rest is judgement, and is written down as judgement.
//!
//! ## Why the verbs are on Ctrl, and why six letters are missing from them
//!
//! The verbs were briefly on Alt, to get out of the way of a multiplexer that
//! takes Ctrl chords before this process ever sees them — tmux's default prefix
//! is `Ctrl-B`, which was Jod's delegate key, so the binding never arrived.
//!
//! That fixed the wrong half of the problem. On macOS, Option does not send Alt
//! at all unless the terminal is specially told to — iTerm2's "Esc+" for the
//! left Option key, Terminal.app's "Use Option as Meta key" — and without it
//! the terminal eats the keypress to type `å`. So the chords did not merely
//! collide, they could not be typed. A binding nobody can press is worse than
//! one a multiplexer eats, because the second at least has a workaround.
//!
//! The verbs are therefore Ctrl again, minus the letters something else is
//! already holding. tmux here is prefixed on `Ctrl-A`, with `Ctrl-H/J/K/L` for
//! panes and `Ctrl-S` for sessions; the terminal has always owned `Ctrl-Q` and
//! `Ctrl-Z`, and `Ctrl-I`/`Ctrl-M` are Tab and Enter. Take those away, then
//! readline's `Ctrl-C/D/E/U/W`, and eleven letters are left:
//! **B F G N O P R T V X Y**.
//!
//! ## Eighteen verbs, eleven letters
//!
//! They do not fit, so which verb keeps a chord is decided by what a chord is
//! *for*. The chat box turns every bare key into text, so a chord buys exactly
//! one thing: a verb you can reach **without stopping the sentence you are
//! typing, or looking away from the run you are watching**. Delegate the line,
//! stop the run, copy the reply, show the reasoning, answer the rail, start
//! dictating. Those get the letters.
//!
//! Everything else is a *destination*, and destinations go behind the leader:
//! `Ctrl-G` opens the menu, one more letter lands you anywhere. That is not a
//! consolation prize for the verbs that lost the draw — it is the job the menu
//! was built for, and it now covers all nine screens rather than the seven that
//! happened to have no chord. `Ctrl-F` is the one destination that kept one,
//! because the fleet is where a delegated run goes and `Ctrl-B` `Ctrl-F` is a
//! single thought.
//!
//! **The eleven are now spent.** `Ctrl-V` was held back so the next verb would
//! have somewhere to land that is not someone else's key, and dictation took it
//! within the week — which is the argument for having kept it, not against.
//! The projects panel arrived in the same batch and went to `Ctrl-G d`, because
//! `Ctrl-D` is quit.
//!
//! So the next verb after this one has no letter at all, and that is the
//! decision to make deliberately rather than by taking `Ctrl-L` back off tmux:
//! either it is a destination and goes behind the leader, or something already
//! holding a letter is demoted to make room. The menu is the pressure valve and
//! it has nine free letters left.
//!
//! **The first demotion has now happened, and it is the shape the rule
//! predicted.** The projects panel spent a year on `Ctrl-G d` and turned out to
//! be pressed constantly — it is the box that answers *which repository does my
//! next sentence land in*, which is a question asked between one instruction and
//! the next rather than twice a day. The directory picker on `Ctrl-P` is the
//! opposite: a destination, opened when a new tree has to be added and not
//! again. So they swapped. Nothing was added and nothing was lost; what changed
//! is that the chord went to the key that is actually pressed, and the letter
//! behind the leader now stands for something — `d` for directory.
//!
//! What did **not** move is the handful of Ctrl chords the terminal itself has
//! taught everyone: `Ctrl-C`/`Ctrl-D` quit, `Ctrl-U` clears the line, `Ctrl-W`
//! deletes a word, `Ctrl-A`/`Ctrl-E` go to the ends of it. Moving those would
//! break muscle memory that predates Jod by forty years to solve a problem
//! nobody has — no multiplexer steals them, because every shell needs them.
//! `Ctrl-A` is the one that costs anything under a prefix-on-`Ctrl-A` tmux, and
//! what it costs is a second press rather than the binding.
//!
//! `no_verb_sits_on_a_chord_a_multiplexer_takes` is what keeps the six letters
//! clear, and it exempts those readline rows by name rather than by pattern —
//! they are the terminal's convention, printed here because Jod answers them,
//! not verbs Jod chose to put there.

// Only the drift net turns a printed label back into a keypress, and that is
// test-only — the running program prints these strings and never reads them.
#[cfg(test)]
use crossterm::event::{KeyCode, KeyModifiers};

use super::app::Layer;
use super::workspace::Workspace;

/// One binding, as it appears on the keybar and in the `?` overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub key: &'static str,
    pub what: &'static str,
}

const fn k(key: &'static str, what: &'static str) -> Key {
    Key { key, what }
}

/// The verbs that mean the same thing on every workspace. Learn them once.
pub const SPINE: &[Key] = &[
    k("↑↓ / jk", "move the cursor"),
    k("PgUp/PgDn", "move one screen"),
    k("Home/End", "first / last"),
    k("⏎", "the one obvious thing"),
    k("/", "filter this list"),
    k("n", "new"),
    k("e", "edit"),
    // Deliberately silent about confirmation. Four screens confirm and the
    // fleet's `x` does not — untracking a project deletes nothing — so the
    // promise lives on the screens that keep it, not on the row they share.
    k("x", "delete"),
    k("S", "cycle sort"),
    k("1–9", "jump to a workspace"),
    k("Esc / q", "back one level"),
    k("?", "these keys"),
];

/// The chords that work everywhere, including in the middle of typing.
///
/// Ctrl throughout, because Alt is unpressable on a stock macOS terminal — see
/// the module header, which also says why six Ctrl letters are missing and
/// where the verbs that wanted them went. The Alt spelling of each of these
/// still fires and is deliberately not printed: it costs nothing to keep for
/// anyone who learned it, and printing it would advertise a chord that does not
/// exist on the keyboard this is aimed at.
///
/// The last four rows are readline's rather than Jod's, and are the exemption
/// `no_verb_sits_on_a_chord_a_multiplexer_takes` names.
///
/// One row is not a chord at all — `Shift-Tab` — and it is here for the same
/// reason everything else is: it works everywhere, including mid-sentence. It
/// spends none of the eleven letters that
/// `every_free_letter_is_spent_so_the_next_verb_is_a_decision` counts, because
/// the terminal sends it as `BackTab` rather than as a letter.
pub const GLOBAL: &[Key] = &[
    // The label is load-bearing on its *length*: `draw_keymap` sizes a column
    // from the widest row, and at 100×30 a `what` longer than 33 characters
    // costs the `?` overlay its second column and hides twenty rows. Say the
    // menu takes another key, in under that.
    k("Ctrl-G", "the workspace menu — then a key"),
    // The one destination that kept a chord. See the module header.
    k("Ctrl-F", "fleet"),
    // The rail's two chords. Both are chords rather than letters for one
    // reason, and it is the reason E2.S3 gives: the chat box owns every bare
    // key, so a rail verb on `c` would type a `c` into the sentence being
    // written. See [`RAIL`].
    k("Ctrl-R", "the rail, then 1–9 to accept"),
    k("Ctrl-N", "the cards — and away again"),
    // The side panel, which is where the projects, the sessions, the mode, the
    // harness, the spend and the context left are drawn — a large fraction of
    // what the program knows, behind one key.
    //
    // It is written down here because until now it was written down *only* on
    // the panel's own bottom border (`Shift-Tab closes`), which you can read
    // only once you have already found the key. An overlay that calls itself
    // the whole keymap and omits the way into a sixth of the program sends the
    // reader to the source, which is where this key was in fact found.
    //
    // Not caught by the drift net either, and that is why the row carries its
    // own test in `ui`: `is_chord` recognises a Ctrl or Alt prefix, and this
    // arrives as `BackTab` carrying neither, so nothing replays it.
    k("Shift-Tab", "show or hide the side panel"),
    // The catalog inside that panel, and the keyboard with it. `p` for
    // projects, which is the letter the box's own title asks for; it was
    // `Ctrl-G d`, where `d` stood for nothing and had been chosen only because
    // `Ctrl-D` is quit. The directory picker that held this chord is a
    // destination opened about twice a day and went behind the leader, which is
    // what the leader is for.
    k("Ctrl-P", "the projects — and away again"),
    // A switch, not a button: it stays on, and everything said streams into
    // the box until it is switched off. Saying "go ahead" sends, "stop
    // listening" switches off — the keyboard is optional once it is on, which
    // is the point.
    //
    // This is what the spare letter was being kept for, and `v` is the one it
    // would have asked for anyway. It is also the clearest case yet of the rule
    // above: dictation is *only* useful without stopping what you are doing.
    // The projects panel that arrived beside it is a destination and went to
    // `Ctrl-G d` — `Ctrl-D` is quit and there was no letter left to give it.
    k("Ctrl-V", "listen, and keep listening"),
    k("Ctrl-Y", "copy the last reply"),
    k("Ctrl-B", "delegate the typed line"),
    k("Ctrl-X", "stop the run being watched"),
    k("Ctrl-T", "show or hide reasoning"),
    // The steps of the turn being watched are streamed by `/details`; this key
    // is about the ones already over, which the transcript folds away so that
    // scrolling back reads as the conversation rather than as a log.
    k("Ctrl-O", "show or hide the steps taken"),
    k("Ctrl-↑↓", "scroll the transcript"),
    // One row for the pair, for the same reason `uU` above is one row.
    //
    // The `?` overlay promises to be complete at 100×30, and this branch and
    // `main` each added chords to it — the rail's two, the picker, search,
    // yank, and background shells — which together cost it one line more than
    // it had. Something had to give, and a verb with its inverse gives up a
    // row without giving up a verb: both keys still fire and both are still
    // advertised.
    //
    // This pair rather than another because start-of-line and end-of-line are
    // read as one idea by anyone who already knows them from a shell, and
    // guessed as a pair by anyone who does not.
    // All four spellings, because all four are dispatched. `press_of` splits on
    // `/` and carries the modifier along, so this one row still advertises
    // Ctrl-A, Ctrl-E, Ctrl-Home and Ctrl-End — and the drift net, which replays
    // every printed label as a real keypress, is what caught the version of
    // this that quietly dropped Home and End while still answering them.
    k("Ctrl-A/E/Home/End", "start / end of the line"),
    k("Ctrl-U", "clear the input line"),
    k("Ctrl-W", "delete the previous word"),
    k("Ctrl-C/D", "quit — twice while agents run"),
];

/// Two of these are printed nowhere else. `Tab` cycles the permission mode —
/// the side panel says "Tab cycles" and no keymap said what it cycled — and
/// `@` is the only way to reach the file picker. Both sit above `/` and `?`
/// because those two are the spine's and this screen's own verbs must not be
/// what a narrow terminal drops.
const CHAT: &[Key] = &[
    k("Ctrl-B", "delegate"),
    k("Ctrl-F", "fleet"),
    k("Ctrl-G", "menu"),
    k("Tab", "cycle the permission mode"),
    k("@", "a file from this session's folders"),
    k("/", "commands"),
    k("?", "keys"),
];

/// The fleet is the widest screen, because it is the only one that is both a
/// list of runs and a handle on the conversation graph behind them: `s r d a`
/// act on the run under the cursor, `b u U g f t` act on its thread. `/` is
/// last because it is the spine's, not the fleet's — see the module header.
///
/// `u` undoes and `U` puts it back. Lower case is undo on every screen that has
/// one — memory's `u` is an undo too — because undo and redo are a verb and its
/// inverse, and that is the one case where a habit transferring between screens
/// does damage rather than nothing.
///
/// `⇥` is first because it is the key that makes the screen navigable: the
/// fleet draws three panes and one of them is the runs that belong to no work,
/// which is otherwise only reachable by walking the cursor past every row of
/// every project above it.
///
/// `g` is spelled `go to #` because `#` is the exact token printed beside each
/// branch in the listing — the label names what is on screen rather than
/// describing it.
const FLEET: &[Key] = &[
    k("⇥", "next pane"),
    k("⏎", "watch"),
    k("s", "stop"),
    k("r", "resume"),
    // Above `d` because `a` is the collided letter — it answers an escalation
    // on goals, which does print it. See the ordering rule in the header.
    k("a", "attach"),
    // `delegate`, word for word as the task board spells it, because it is the
    // same `Action::Delegate` on the selected row. Two spellings of one verb
    // read as a collision and are not one: `d` is the one letter here that
    // transfers between screens intact, and it only does so while both screens
    // call it the same thing. Six characters shorter is why it now survives
    // eighty columns, but matching the board is the reason.
    k("d", "delegate"),
    // The work's bus. High in the table because it is the only verb here that
    // answers "what are these agents saying to each other", and a work whose
    // traffic cannot be read is a work you can only watch spend money — see
    // `tui::traffic`. Capital because `t` already retries a run on this screen.
    k("T", "traffic"),
    k("b", "branches"),
    // One row for the pair, the way `→←` below is one row for two arrows.
    //
    // Both keys still fire and both are still advertised; what changed is that
    // they cost the `?` overlay one line instead of two. The overlay is two
    // columns of twenty-eight rows at the design size, and the fleet's own
    // section plus the spine plus the global chords came to exactly one line
    // more than that when `T traffic` arrived — so a screen that had promised
    // to be complete at 100×30 started saying `1 more — widen the window`.
    // Undo and redo are a verb and its inverse and read as one thing anyway,
    // which is why this pair is the one that gives way rather than a verb that
    // would have had to be dropped.
    k("uU", "undo / redo"),
    k("g", "go to #"),
    k("f", "fork"),
    k("t", "retry"),
    // The tree's own verbs, in force once there is a work to draw. Below the
    // run verbs because those act on the row and these act on the shape, and
    // the row is what people come here for; above `/` because that one is the
    // spine's and means the same thing on every screen.
    k("→←", "in / out"),
    k("space", "expand / collapse"),
    // One row for the pair, the way `uU` and `→←` above are. Both keys still
    // fire and both are still advertised; what it buys is the row that `x`
    // needed. The `?` overlay promises to be complete at 100×30 and was exactly
    // at that limit — see the note on `uU` — so a new verb here had to come
    // from somewhere, and expand-all with its inverse is the same trade that
    // note describes: a row given up without a verb given up.
    k("EC", "expand / collapse all"),
    k("z", "closed works"),
    // Last of the tree's own verbs, and the only one on this screen that
    // changes the catalog rather than the shape of what is drawn from it.
    //
    // `x` because it is what the letter already means on memory, schedules,
    // goals and hooks — take this row off this list. Those four go through the
    // shared `x` and its "this cannot be undone" confirmation; this one does
    // not, and deliberately: untracking deletes nothing and `jod project
    // restore` puts the whole subtree back, so the notice naming that command
    // is the friction that fits. A confirmation titled "cannot be undone" in
    // front of something that can is how people learn to dismiss them.
    k("x", "untrack project"),
    k("/", "filter"),
];

/// The traffic log, opened from the tree with `T`.
///
/// `T` is capital because lower-case `t` is already *retry* on the fleet, and a
/// letter that retried a run on one press and opened a screen on the next would
/// be the worst kind of collision — one of the two is destructive. `E`, `C`,
/// `U` and `S` set that pattern on this screen already: when the letter is
/// spoken for, the verb goes to the capital rather than to an unrelated key
/// nobody can guess.
///
/// `f` is the state cycle, spelled and ordered exactly as the rail's `f` is,
/// because G5.S5 asks for one way to narrow a list in this program rather than
/// a second idiom for the same job. `/` and `S` are the spine's and go last.
const TRAFFIC: &[Key] = &[
    k("⏎", "the message in full"),
    k("f", "every / failed / waiting / delivered"),
    k("/", "filter"),
    k("S", "cycle sort"),
];

const MEMORY: &[Key] = &[
    k("g", "graph"),
    k("l", "link"),
    k("t", "type"),
    k("e", "edit"),
    k("n", "new"),
    k("x", "forget — confirms first"),
    k("/", "filter"),
];

const MEMORY_GRAPH: &[Key] = &[
    k("⏎", "re-centre"),
    k("↑↓", "neighbour"),
    k("Backspace", "back"),
    k("h", "hops"),
    k("f", "edge kind"),
    k("g", "list"),
];

const SCHEDULES: &[Key] = &[
    k("⏎", "last run"),
    k("r", "run now"),
    k("p", "pause/resume"),
    k("e", "edit"),
    k("n", "new"),
    k("x", "delete — confirms first"),
    k("/", "filter"),
];

/// `a answer` is fourth rather than last because it is the verb that unblocks a
/// stuck loop and exists on no other screen, while `e` and `n` are the spine's
/// and mean the same thing everywhere.
///
/// It is also `answer` rather than `answer escalation`, and that is one decision
/// rather than two. Ordering alone was **not** enough here — this is the only
/// screen where it was not. At nineteen characters the full phrase did not fit
/// eighty columns even in fourth place, so the reorder reserved a slot it could
/// not use and dropped `e edit` for nothing: strictly fewer verbs than before.
/// At eight it fits, and a shorter label still teaches the key, which is the
/// whole reason for printing one. `draw_goals` already prints `needs you` in
/// bold above the bar, so the screen says *that* a goal is stuck; the keybar
/// only owes you the key.
const GOALS: &[Key] = &[
    k("⏎", "last iteration"),
    k("r", "run now"),
    k("p", "pause"),
    k("a", "answer"),
    k("e", "edit"),
    k("n", "new"),
];

const HOOKS: &[Key] = &[
    k("⏎", "open run"),
    k("t", "test payload"),
    k("p", "pause"),
    k("c", "copy URL"),
    k("e", "edit"),
    k("n", "new"),
    k("x", "delete — confirms first"),
];

const TASKS: &[Key] = &[
    k("⏎", "mark done"),
    k("d", "delegate"),
    k("c", "claim"),
    k("o", "open run"),
    k("n", "new"),
    k("x", "remove — confirms first"),
    k("/", "filter"),
];

const ACTIVITY: &[Key] = &[
    k("⏎", "jump to it"),
    k("m", "mark read"),
    k("M", "mark all"),
    k("u", "unread only"),
    k("f", "filter source"),
];

const TEAM: &[Key] = &[k("⏎", "mark done"), k("↑↓", "pick"), k("/", "filter")];

/// The decision rail's own verbs, in force only while the rail has the
/// keyboard — which `Ctrl-N` is what gives it, and `Esc` is what takes away.
///
/// **Why a focus rather than a chord per verb.** The chat input turns every
/// bare key into text, so the rail could either have a chord for each of its
/// eight verbs — eight more free letters, on a keymap with exactly one to spare
/// — or one chord that hands it the keyboard. It has the second. Getting *in*
/// is free and safe mid-sentence (`Ctrl-N` never touches `App::input`); once
/// in, the keys are ordinary letters, and `Esc` gives the keyboard back with
/// the typed line exactly as it was.
///
/// `1–9` answers the numbered option under the cursor rather than jumping to a
/// workspace. That collision is safe precisely *because* focus is explicit: the
/// digits mean the rail's thing only while the rail is drawn, highlighted and
/// named on the bar, and a workspace jump is one `Esc` away.
///
/// `t` cycles which stack is on show — open, then answered, then dismissed —
/// which is how an answered card is toggled back into view once it has left the
/// stack.
pub const RAIL: &[Key] = &[
    k("⏎", "expand / collapse"),
    k("1–9", "answer by option"),
    k("a", "answer in prose"),
    k("x", "dismiss"),
    k("t", "open / answered / dismissed"),
    k("c", "this session / everything below"),
    k("f", "kind"),
    k("/", "filter"),
    k("S", "sort"),
];

/// What the rail's keybar says on its right-hand half.
pub const RAIL_EXIT: &str = "Esc back to the chat · ? keys";

/// The same, for a rail that was opened over the catalog.
///
/// `Esc` closes the newest view first and hands the keyboard to whatever was
/// open underneath it, so from here it lands on the projects rather than on the
/// chat. A keybar that named the wrong destination would be worse than naming
/// none — see [`super::app::App::focus`].
pub const RAIL_EXIT_TO_CATALOG: &str = "Esc back to the projects · ? keys";

/// The project catalog's own verbs, in force only while it has the keyboard —
/// which `Ctrl-P`, or a click on the box, is what gives it.
///
/// Four rows, and that is the whole screen. The catalog is a list of
/// repositories; the verbs that *change* one already have homes that are better
/// at it — `/project add` and `/project ls` from the chat box, and `x` on the
/// fleet, which unlike a thirty-column panel can tell two projects of the same
/// name apart. What did not exist anywhere was moving a cursor down the box and
/// opening what it is on, so that is what this is and it stops there.
///
/// `⏎` says `manager` rather than `open` because that is the thing it opens: the
/// conversation that owns the repository's work. A label saying "open" would
/// leave the reader to guess what a project opens *into*.
pub const CATALOG: &[Key] = &[
    k("⏎", "manager"),
    k("↑↓ / jk", "move the cursor"),
    k("Home/End", "first / last"),
    k("Ctrl-P", "put it away"),
];

/// What the catalog's keybar says on its right-hand half.
pub const CATALOG_EXIT: &str = "Esc back to the chat · ? keys";

/// The same, for a catalog that was opened over the rail. See
/// [`RAIL_EXIT_TO_CATALOG`], which is the same rule the other way around.
pub const CATALOG_EXIT_TO_RAIL: &str = "Esc back to the cards · ? keys";

/// What `Esc` says it does, given what is open underneath the view that has the
/// keyboard.
///
/// One function for both views, because there is one rule: `Esc` closes the
/// newest thing and the bar names whatever that uncovers.
pub fn exit_beneath(here: Layer, beneath: Option<Layer>) -> &'static str {
    match (here, beneath) {
        (Layer::Rail, Some(Layer::Catalog)) => RAIL_EXIT_TO_CATALOG,
        (Layer::Rail, _) => RAIL_EXIT,
        (Layer::Catalog, Some(Layer::Rail)) => CATALOG_EXIT_TO_RAIL,
        (Layer::Catalog, _) => CATALOG_EXIT,
    }
}

/// The footer printed inside the expanded card's border. Same relationship to
/// [`RAIL`] that [`footer`] has to [`local`], and fitted the same way at the
/// call site.
pub fn rail_footer() -> String {
    let verbs = items(RAIL)
        .into_iter()
        .take(4)
        .collect::<Vec<_>>()
        .join(SEP);
    format!(" {verbs} ")
}

/// This screen's own verbs, in keybar order.
///
/// Fleet's `s`, `a` and `r` are exactly what they are today. `S` is capital
/// precisely because lowercase `s` is spoken for there.
pub fn local(ws: Workspace) -> &'static [Key] {
    match ws {
        Workspace::Chat => CHAT,
        Workspace::Fleet => FLEET,
        Workspace::Memory => MEMORY,
        Workspace::MemoryGraph => MEMORY_GRAPH,
        Workspace::Schedules => SCHEDULES,
        Workspace::Goals => GOALS,
        Workspace::Hooks => HOOKS,
        Workspace::Tasks => TASKS,
        Workspace::Activity => ACTIVITY,
        Workspace::Team => TEAM,
        Workspace::Traffic => TRAFFIC,
    }
}

/// The keybar's left half: this screen's verbs, as many as fit beside the way
/// out. See the module header for why the exit wins the argument.
pub fn keybar(ws: Workspace, width: u16) -> String {
    fit_bar(local(ws), verb_budget(ws, width))
}

/// The keybar while the decision rail has the keyboard.
///
/// A bar of its own rather than the screen's, because the screen's verbs are
/// not the ones in force: printing `s stop` while `x` dismisses a card would
/// teach a key that does something else entirely. Same fitting rule — the way
/// out is reserved first — for the reason the module header gives.
pub fn rail_keybar(width: u16) -> String {
    fit_bar(RAIL, budget(RAIL_EXIT, width))
}

/// The keybar while the project catalog has the keyboard. Same argument as
/// [`rail_keybar`]'s, one box over.
pub fn catalog_keybar(width: u16) -> String {
    fit_bar(CATALOG, budget(CATALOG_EXIT, width))
}

fn fit_bar(bindings: &'static [Key], budget: usize) -> String {
    let verbs = items(bindings);

    let whole = verbs.join(SEP);
    if whole.chars().count() <= budget {
        return whole;
    }

    // The marker's own width comes out of the budget before any verb does.
    // Adding it afterwards would let the announcement be the thing that
    // overflowed — the bar would drop the exit hint in order to say it had
    // dropped a verb.
    let mut used = MORE.chars().count();
    let mut shown: Vec<String> = Vec::new();
    for verb in verbs {
        let cost = verb.chars().count() + SEP.chars().count();
        if used + cost > budget {
            break;
        }
        used += cost;
        shown.push(verb);
    }
    shown.push(MORE.to_string());
    shown.join(SEP)
}

/// What the left half may spend before it starts eating the way out.
///
/// Mirrors `ui::two_ends`, which reserves the right half first and hands the
/// remainder to the verbs: a space of margin at each end and at least one
/// between the halves, so three columns beyond the exit text itself.
///
/// Two files therefore have to agree on that number, and **nothing in this
/// module can check it**. `the_way_out_fits_beside_the_verbs_at_every_realistic_width`
/// looks like it does and does not: it measures a bar this function already
/// budgeted, so it is self-consistent by construction and stays green however
/// wrong the shared number is. Worse, `keybar` drops *whole* verbs, so it
/// usually lands well under budget and quietly absorbs a disagreement of a
/// column or two — until the one screen whose verbs end exactly on the
/// boundary, which is the day that screen loses its entire left half rather
/// than one verb.
///
/// The real guard is `ui::tests::two_ends_accepts_a_left_half_of_exactly_the_budgeted_width`,
/// which calls this function, builds a left half of precisely that width, and
/// asserts `two_ends` prints it. Public for exactly that reason: the number
/// lives here, and the test that pins it against the renderer calls it rather
/// than repeating it.
pub fn verb_budget(ws: Workspace, width: u16) -> usize {
    budget(keybar_exit(ws), width)
}

fn budget(exit: &str, width: u16) -> usize {
    (width as usize).saturating_sub(exit.chars().count() + 3)
}

fn items(bindings: &'static [Key]) -> Vec<String> {
    bindings
        .iter()
        .map(|b| format!("{} {}", b.key, b.what))
        .collect()
}

const SEP: &str = " · ";

/// What a bar says when it has dropped something. `?` is the overlay, which
/// leads with this screen's own verbs — so the marker is a destination, not an
/// apology.
const MORE: &str = "? more";

/// The keybar's right half: the way out, which never changes.
pub fn keybar_exit(ws: Workspace) -> &'static str {
    match ws {
        Workspace::Chat => "Ctrl-X stop · Ctrl-C quit",
        _ => "Esc back · ? keys",
    }
}

/// The footer printed inside a workspace's own border, shorter than the keybar
/// because it repeats only what acts on the selected row.
///
/// Unlike [`keybar`], this takes no width and does no fitting — `ui::fit_verbs`
/// does it at the call site, against the pane's own rect. The split is
/// deliberate: the keybar has to reserve room for the way out and say `? more`
/// when it drops something, which is keymap policy, whereas the footer carries
/// no marker and nothing not already on the bar. What is left is "make text fit
/// a box", which belongs to the renderer.
///
/// So do not add a budget here. Two of them would fight, and the second would
/// be invisible — the string this returns is already whole, and clipping it
/// twice looks exactly like clipping it once.
pub fn footer(ws: Workspace) -> String {
    let verbs = items(local(ws))
        .into_iter()
        .take(4)
        .collect::<Vec<_>>()
        .join(SEP);
    format!(" ↑↓ pick · {verbs} ")
}

/// The `?` overlay, screen-aware: this screen's verbs first, then the spine
/// every screen shares, then the global chords.
///
/// Screen-first is the point — help that omits the focused screen's verbs sends
/// you to the source.
pub fn keymap(ws: Workspace) -> Vec<(String, &'static [Key])> {
    let mut sections: Vec<(String, &'static [Key])> = Vec::new();
    sections.push((format!("{} — this screen", ws.title()), local(ws)));
    if ws.is_list() {
        sections.push(("every workspace".to_string(), SPINE));
    }
    sections.push(("anywhere".to_string(), GLOBAL));
    sections
}

/// The `?` overlay while the rail has the keyboard.
///
/// The screen's own verbs are deliberately **not** here, and neither is the
/// list spine. Both are the same argument as [`rail_keybar`]'s: while the rail
/// holds the keyboard those keys are not in force, and help that lists a key
/// which currently does something else is worse than help that omits it. The
/// two chords that got you here, and the way out, are in [`GLOBAL`], which
/// stays.
pub fn rail_keymap() -> Vec<(String, &'static [Key])> {
    vec![
        ("the rail — this has the keyboard".to_string(), RAIL),
        ("anywhere".to_string(), GLOBAL),
    ]
}

/// The `?` overlay while the project catalog has the keyboard. Same shape and
/// same argument as [`rail_keymap`].
pub fn catalog_keymap() -> Vec<(String, &'static [Key])> {
    vec![
        ("the projects — this has the keyboard".to_string(), CATALOG),
        ("anywhere".to_string(), GLOBAL),
    ]
}

/// The which-key overlay's keybar line, which has to name the leader it is
/// waiting on — an overlay that says only "waiting for a key" tells you it is
/// stuck without telling you what unstuck it.
///
/// `making` is the `n` submenu, which is one keypress deeper and so names the
/// two-key route rather than the leader alone.
///
/// This lives here rather than in `ui.rs` for one reason: `ui.rs` is prose the
/// drift test cannot see. Spelled here, `Ctrl-G` is scanned and pressed like
/// every other advertised chord, so it cannot go stale the next time the
/// keymap moves. That is exactly how these four strings were left saying
/// `Ctrl-K` after the keymap had already moved to Alt.
pub fn which_key_hint(making: bool) -> String {
    if making {
        "Ctrl-G n … s schedule · g goal · h hook · m memory · t task".to_string()
    } else {
        "Ctrl-G … waiting for a key".to_string()
    }
}

/// The which-key overlay's border title. Same reasoning as `which_key_hint`,
/// and the two must name the same chord — which is why they sit together.
pub fn which_key_title(making: bool) -> &'static str {
    if making {
        " Ctrl-G n · new… "
    } else {
        " Ctrl-G "
    }
}

// ---- keeping the printed keymap and the dispatch honest -------------------
//
// This table is display-only; the `match` in `mod.rs` is what actually runs.
// Nothing but attention has ever kept the two in step, and the failure is
// silent in the worst possible way — the keybar keeps promising a chord that
// stopped working. So the labels are made *machine-readable* here, and
// `mod.rs`'s tests press every one of them. A binding added to either side
// alone now fails the build instead of shipping.
//
// Everything below is `cfg(test)`: the running program never reads a label back
// out, only prints it, so these would be dead code in the binary — and a
// standing dead-code warning is how a real one gets missed.

/// Does this label name a chord, rather than a bare key like `⏎` or `n`?
#[cfg(test)]
pub fn is_chord(label: &str) -> bool {
    label.starts_with("Ctrl-") || label.starts_with("Alt-")
}

/// The chords named inside a line of on-screen prose.
///
/// Exit hints are sentences (`"Ctrl-X stop · Ctrl-C quit"`), not table rows, so
/// a chord can arrive there as a substring nobody registered anywhere.
#[cfg(test)]
pub fn chords_in(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|token| is_chord(token))
        .map(|token| token.to_string())
        .collect()
}

/// Every chord this module puts on a screen, once each.
///
/// `keymap` already folds in each workspace's own verbs, the shared spine and
/// the global chords, and the keybar and footer are rendered from that same
/// data — so this plus the exit hints and the which-key overlay is genuinely
/// everything Jod advertises.
///
/// Anything printed *outside* this module is outside the net. That is the
/// argument for moving a chord-bearing string in here rather than spelling it
/// at the call site.
#[cfg(test)]
pub fn all_documented_chords() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for ws in Workspace::ALL {
        for (_, bindings) in keymap(ws) {
            found.extend(
                bindings
                    .iter()
                    .filter(|b| is_chord(b.key))
                    .map(|b| b.key.to_string()),
            );
        }
        found.extend(chords_in(keybar_exit(ws)));
    }
    for sections in [rail_keymap(), catalog_keymap()] {
        for (_, bindings) in sections {
            found.extend(
                bindings
                    .iter()
                    .filter(|b| is_chord(b.key))
                    .map(|b| b.key.to_string()),
            );
        }
    }
    found.extend(chords_in(RAIL_EXIT));
    found.extend(chords_in(CATALOG_EXIT));
    for making in [false, true] {
        found.extend(chords_in(&which_key_hint(making)));
        found.extend(chords_in(which_key_title(making)));
    }
    found.sort();
    found.dedup();
    found
}

/// The keypresses a printed label stands for — the whole point being that a
/// test can press what the screen says.
///
/// Two shorthands the labels use, because the overlay has twelve columns for a
/// key and a row per binding is a row the reader has to scan:
///
/// - `↑↓` means both arrows, so `Ctrl-↑↓` is two presses.
/// - a `/` continuation inherits the modifier to its left, so `Ctrl-A/Home` is
///   `Ctrl-A` and `Ctrl-Home` — not `Ctrl-A` and a bare `Home`.
///
/// An empty result means the label is not pressable, which the drift test
/// treats as a bug in the label rather than as a row to skip. Silently
/// skipping is how a table drifts.
#[cfg(test)]
pub fn press_of(label: &str) -> Vec<(KeyCode, KeyModifiers)> {
    let mut presses = Vec::new();
    let mut carried: Option<KeyModifiers> = None;
    for part in label.split('/') {
        let part = part.trim();
        let (modifier, rest) = if let Some(rest) = part.strip_prefix("Ctrl-") {
            (KeyModifiers::CONTROL, rest)
        } else if let Some(rest) = part.strip_prefix("Alt-") {
            (KeyModifiers::ALT, rest)
        } else {
            match carried {
                Some(modifier) => (modifier, part),
                // A label that starts with a continuation names no modifier at
                // all, so there is nothing to press.
                None => return Vec::new(),
            }
        };
        carried = Some(modifier);
        let codes = codes_of(rest);
        if codes.is_empty() {
            return Vec::new();
        }
        presses.extend(codes.into_iter().map(|code| (code, modifier)));
    }
    presses
}

/// The key codes a label's tail names, once the modifier is stripped off.
#[cfg(test)]
fn codes_of(rest: &str) -> Vec<KeyCode> {
    match rest {
        "↑↓" => vec![KeyCode::Up, KeyCode::Down],
        "↑" => vec![KeyCode::Up],
        "↓" => vec![KeyCode::Down],
        "Home" => vec![KeyCode::Home],
        "End" => vec![KeyCode::End],
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                // Printed in capitals because that is how a chord reads; sent
                // by the terminal in lowercase, which is what must be matched.
                (Some(c), None) if c.is_ascii_alphanumeric() => {
                    vec![KeyCode::Char(c.to_ascii_lowercase())]
                }
                _ => Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The realistic terminal widths. Eighty is the one that matters: it is
    /// where the exit hint was being dropped, and every render test in the
    /// suite used a hundred and fifty, which is why nobody saw it for so long.
    const WIDTHS: [u16; 4] = [80, 100, 120, 150];

    /// The invariant. A screen showing three of its five verbs is merely
    /// terse; a screen with no way out is one you have to kill the terminal to
    /// leave.
    ///
    /// Note what this does **not** prove: it measures a bar `verb_budget`
    /// already sized, so it cannot see the padding here disagreeing with the
    /// padding in `ui::two_ends`. That coupling is pinned by
    /// `ui::tests::two_ends_accepts_a_left_half_of_exactly_the_budgeted_width`,
    /// which builds the boundary case by hand. See `verb_budget`.
    #[test]
    fn the_way_out_fits_beside_the_verbs_at_every_realistic_width() {
        for ws in Workspace::ALL {
            for width in WIDTHS {
                let left = keybar(ws, width).chars().count();
                let right = keybar_exit(ws).chars().count();
                assert!(
                    left + right + 3 <= width as usize,
                    "{ws:?} at {width} columns wants {} before the way out fits",
                    left + right + 3
                );
            }
        }
    }

    /// The budget drops from the end, so a table's order decides what a narrow
    /// terminal loses. Shared verbs go last so that what it loses is the thing
    /// the spine already teaches everywhere, rather than the one verb that
    /// exists on this screen alone.
    ///
    /// Without this, the order is whatever someone happened to type, and the
    /// casualty is whatever that accident put at the end — which is how goals
    /// came to hide `a answer` at eighty columns while keeping
    /// `e edit`, a verb printed on six other screens.
    #[test]
    fn no_verb_the_spine_already_teaches_sits_above_one_only_this_screen_has() {
        for ws in Workspace::ALL {
            let mut shared_so_far: Option<&Key> = None;
            for binding in local(ws) {
                // The primary action leads every screen, spine or not.
                if binding.key == "⏎" {
                    continue;
                }
                let shared = SPINE.iter().any(|s| s.key == binding.key);
                match (shared, shared_so_far) {
                    (true, None) => shared_so_far = Some(binding),
                    (false, Some(earlier)) => panic!(
                        "{ws:?} lists `{} {}` after `{} {}`. The second is the spine's and means \
                         the same thing on every screen; the first exists only here. The budget \
                         drops from the end, so this order loses the one that cannot be learned \
                         anywhere else.",
                        binding.key, binding.what, earlier.key, earlier.what
                    ),
                    _ => {}
                }
            }
        }
    }

    /// The rule is only worth anything if it changes what a narrow terminal
    /// shows, so both screens where it does are pinned here.
    ///
    /// Hooks is the clean case: the reorder alone surfaces `c copy URL` and
    /// drops `e edit`, where before the bar spent its last slot on the spine's
    /// verb and hid the only way to get a webhook's address out of Jod.
    ///
    /// Goals is the case that needed a label change as well, and is the reason
    /// this test exists in both halves — ordering `a` above `e` reserved a slot
    /// that nineteen characters could not fit, so until `answer escalation`
    /// became `answer` the reorder made this width strictly worse rather than
    /// better. A rule that is necessary but not sufficient looks identical to a
    /// rule that works, right up until someone measures.
    #[test]
    fn a_screen_specific_verb_outlives_a_spine_verb_at_eighty_columns() {
        for (ws, kept, dropped) in [
            (Workspace::Hooks, "c copy URL", "e edit"),
            (Workspace::Goals, "a answer", "e edit"),
        ] {
            let bar = keybar(ws, 80);
            assert!(bar.contains(kept), "{ws:?} must keep `{kept}`: {bar}");
            assert!(!bar.contains(dropped), "{ws:?} must drop `{dropped}`: {bar}");
            assert!(bar.contains(MORE), "{ws:?} must say it dropped something: {bar}");
        }
    }

    /// The bar is allowed to be terse. It is not allowed to be terse *and* look
    /// complete — a dropped verb has to be announced, and still be findable.
    #[test]
    fn a_bar_that_drops_verbs_says_so_and_the_overlay_still_has_them() {
        for ws in Workspace::ALL {
            let in_overlay: Vec<&str> = keymap(ws)
                .into_iter()
                .flat_map(|(_, bindings)| bindings.iter().map(|b| b.key))
                .collect();
            for width in WIDTHS {
                let bar = keybar(ws, width);
                assert!(!bar.is_empty(), "{ws:?} at {width} has an empty keybar");
                for (binding, item) in local(ws).iter().zip(items(local(ws))) {
                    assert!(
                        in_overlay.contains(&binding.key),
                        "{ws:?} hides {} from the ? overlay, which is where the rest lives",
                        binding.key
                    );
                    if !bar.contains(&item) {
                        assert!(
                            bar.contains(MORE),
                            "{ws:?} at {width} dropped `{item}` without saying so: {bar}"
                        );
                    }
                }
            }
        }
    }

    /// A verb is never cut in half. `a att` is a key that does not exist, and
    /// a bar that prints one is worse than a bar that prints fewer.
    #[test]
    fn a_dropped_verb_goes_whole_rather_than_being_cut() {
        // Narrow enough that the fleet's twelve verbs cannot all fit.
        let bar = keybar(Workspace::Fleet, 80);
        assert!(bar.contains(MORE), "expected the fleet to be truncated at 80: {bar}");
        for item in bar.split(SEP) {
            assert!(
                item == MORE || items(local(Workspace::Fleet)).iter().any(|v| v == item),
                "`{item}` is not a whole verb: {bar}"
            );
        }
    }

    /// `a` attaches in the fleet and answers an escalation in goals. Both are
    /// on the bar when there is room, and both are always in the overlay —
    /// which is what makes the collision safe now that the bar can be partial.
    #[test]
    fn a_letter_that_changes_meaning_is_reachable_on_both_screens() {
        assert!(keybar(Workspace::Fleet, 150).contains("a attach"));
        assert!(keybar(Workspace::Goals, 150).contains("a answer"));
        for (ws, what) in [
            (Workspace::Fleet, "attach"),
            (Workspace::Goals, "answer"),
        ] {
            assert!(
                keymap(ws)
                    .into_iter()
                    .any(|(_, bs)| bs.iter().any(|b| b.key == "a" && b.what == what)),
                "{ws:?} does not carry `a {what}` in the overlay"
            );
        }
    }

    /// The rail's bar obeys the same invariant as every screen's: terse is
    /// allowed, stranded is not. It is fitted against the *full* terminal
    /// width rather than the rail's own thirty columns, because it is the
    /// bottom bar and not something drawn inside the column.
    #[test]
    fn the_rails_way_out_fits_beside_its_verbs_at_every_realistic_width() {
        for width in WIDTHS {
            let left = rail_keybar(width).chars().count();
            assert!(
                left + RAIL_EXIT.chars().count() + 3 <= width as usize,
                "the rail at {width} columns wants {}",
                left + RAIL_EXIT.chars().count() + 3
            );
        }
    }

    /// A rail with the keyboard must say how to give it back, and must not
    /// print the screen's verbs while the rail's are the ones in force.
    #[test]
    fn the_rail_names_its_own_verbs_and_the_way_out_of_them() {
        let bar = rail_keybar(150);
        assert!(bar.contains("x dismiss"), "{bar}");
        assert!(bar.contains("a answer in prose"), "{bar}");
        assert!(RAIL_EXIT.contains("Esc"), "{RAIL_EXIT}");
        assert!(
            !bar.contains("s stop"),
            "the fleet's verbs are not in force here: {bar}"
        );
    }

    /// `?` while the rail has the keyboard lists the rail's verbs and the
    /// global chords, and deliberately not the screen's own — those letters are
    /// not in force, and help that names a key which currently does something
    /// else is worse than help that omits it.
    #[test]
    fn the_rails_overlay_lists_its_verbs_and_not_the_screens() {
        let sections = rail_keymap();
        assert!(sections[0].0.contains("rail"));
        assert!(sections[0].1.iter().any(|b| b.what == "dismiss"));
        assert!(sections.iter().any(|(name, _)| name == "anywhere"));
        assert!(
            !sections
                .iter()
                .any(|(_, bindings)| bindings.iter().any(|b| b.what == "stop")),
            "the fleet's verbs are not in force while the rail holds the keyboard"
        );
    }

    /// The way back to the screen's own keys is a chord, so it has to be in the
    /// one section the rail's overlay does carry.
    #[test]
    fn the_rails_overlay_still_names_the_way_out() {
        assert!(rail_keymap()
            .into_iter()
            .any(|(_, bindings)| bindings.iter().any(|b| b.key == "Ctrl-R")));
        assert!(RAIL_EXIT.contains("Esc"));
    }

    /// The two chords are the whole reason the rail is usable mid-sentence, so
    /// they are advertised where every screen can see them.
    #[test]
    fn the_rails_chords_are_taught_alongside_the_other_global_ones() {
        let printed: Vec<&str> = GLOBAL.iter().map(|b| b.key).collect();
        assert!(printed.contains(&"Ctrl-R"), "{printed:?}");
        assert!(printed.contains(&"Ctrl-N"), "{printed:?}");
    }

    #[test]
    fn every_screen_says_how_to_leave_it() {
        for ws in Workspace::ALL {
            let exit = keybar_exit(ws);
            if ws == Workspace::Chat {
                assert!(exit.contains("quit"));
            } else {
                assert!(exit.contains("Esc back"), "{ws:?}");
                assert!(exit.contains("? keys"), "{ws:?}");
            }
        }
    }

    /// Help that lists only global keys sends you to the source.
    #[test]
    fn the_keymap_overlay_leads_with_the_screen_you_are_on() {
        let sections = keymap(Workspace::Schedules);
        assert!(sections[0].0.contains("schedules"));
        assert!(sections[0].1.iter().any(|b| b.what == "run now"));
        assert!(sections.iter().any(|(name, _)| name == "anywhere"));
    }

    /// Chat has no list, so the list spine would be a promise it cannot keep.
    #[test]
    fn the_chat_keymap_does_not_promise_list_keys() {
        let sections = keymap(Workspace::Chat);
        assert!(!sections.iter().any(|(name, _)| name == "every workspace"));
    }

    #[test]
    fn a_workspace_footer_names_the_cursor_and_its_first_verbs() {
        let footer = footer(Workspace::Fleet);
        assert!(footer.contains("↑↓ pick"));
        assert!(footer.contains("⏎ watch"));
    }

    /// The chords the terminal owns must never appear as a binding: `Ctrl-S`
    /// and `Ctrl-Q` are XON/XOFF, `Ctrl-Z` is job control, and `Ctrl-H`,
    /// `Ctrl-I`, `Ctrl-J`, `Ctrl-M` are aliases of Backspace, Tab and Enter.
    ///
    /// Checked against the *parsed* press rather than the label, so a chord
    /// smuggled in as the tail of a `/` continuation is caught too.
    #[test]
    fn no_documented_chord_is_one_the_terminal_owns() {
        let stolen = ['s', 'q', 'z', 'h', 'i', 'j', 'm'];
        for label in all_documented_chords() {
            for (code, modifier) in press_of(&label) {
                if modifier != KeyModifiers::CONTROL {
                    continue;
                }
                if let KeyCode::Char(c) = code {
                    assert!(
                        !stolen.contains(&c),
                        "Ctrl-{c} in {label} is the terminal's, not ours"
                    );
                }
            }
        }
    }

    /// Nothing may be printed that cannot be pressed — a label the parser
    /// cannot read is a label the drift test would silently skip, which is
    /// exactly how the two tables came apart in the first place.
    #[test]
    fn every_printed_chord_parses_into_a_press() {
        let chords = all_documented_chords();
        assert!(
            !chords.is_empty(),
            "no chords found at all — the scan is broken"
        );
        for label in chords {
            assert!(
                !press_of(&label).is_empty(),
                "{label} is printed but cannot be pressed"
            );
        }
    }

    /// The two shorthands the overlay's twelve-column key field forces on us.
    #[test]
    fn a_label_expands_both_arrows_and_inherits_across_a_slash() {
        assert_eq!(
            press_of("Alt-↑↓"),
            vec![
                (KeyCode::Up, KeyModifiers::ALT),
                (KeyCode::Down, KeyModifiers::ALT)
            ]
        );
        assert_eq!(
            press_of("Ctrl-A/Home"),
            vec![
                (KeyCode::Char('a'), KeyModifiers::CONTROL),
                (KeyCode::Home, KeyModifiers::CONTROL)
            ],
            "the tail inherits Ctrl rather than becoming a bare Home"
        );
        assert!(
            press_of("Home/End").is_empty(),
            "no modifier means no chord"
        );
    }

    /// Exit hints are sentences, so a chord can hide in them as a substring.
    #[test]
    fn a_chord_named_only_inside_an_exit_hint_is_still_found() {
        assert!(all_documented_chords().iter().any(|c| c == "Ctrl-X"));
        assert!(all_documented_chords().iter().any(|c| c == "Ctrl-C/D"));
    }

    /// An overlay that says only "waiting for a key" tells you it is stuck
    /// without telling you what unstuck it, and the two halves disagreeing
    /// would be worse than either.
    #[test]
    fn the_which_key_overlay_names_the_leader_that_opened_it() {
        for making in [false, true] {
            assert!(
                which_key_hint(making).contains("Ctrl-G"),
                "hint, making={making}"
            );
            assert!(
                which_key_title(making).contains("Ctrl-G"),
                "title, making={making}"
            );
        }
    }

    /// The letters a multiplexer holds. tmux here is prefixed on `Ctrl-A`, with
    /// `Ctrl-H/J/K/L` for panes and `Ctrl-S` for sessions — a chord on any of
    /// them is taken before this process is even asked, which is the failure
    /// the whole keymap is arranged around.
    const MULTIPLEXER: [char; 6] = ['a', 's', 'h', 'j', 'k', 'l'];

    /// The rows that are readline's convention rather than Jod's choice, named
    /// one by one rather than matched by a pattern — the point of an exemption
    /// list is that adding to it has to be a decision.
    ///
    /// `Ctrl-A` is here and nowhere else. Under a prefix-on-`Ctrl-A` tmux it
    /// needs pressing twice, and that is the price of it also meaning
    /// start-of-line in every shell ever written. A *verb* may not pay that
    /// price, because a verb has somewhere else to go.
    const READLINE: [&str; 4] = ["Ctrl-A/E/Home/End", "Ctrl-U", "Ctrl-W", "Ctrl-C/D"];

    /// The reason this keymap has the shape it has. A verb printed on a letter
    /// tmux is holding is a verb that never arrives — silently, with the keybar
    /// still promising it, which is the exact failure mode the drift net exists
    /// to make loud.
    ///
    /// Checked against the *parsed* press rather than the label, so a letter
    /// smuggled in as the tail of a `/` continuation is caught too.
    #[test]
    fn no_verb_sits_on_a_chord_a_multiplexer_takes() {
        let mut checked = 0;
        for label in all_documented_chords() {
            if READLINE.contains(&label.as_str()) {
                continue;
            }
            for (code, modifier) in press_of(&label) {
                if modifier != KeyModifiers::CONTROL {
                    continue;
                }
                checked += 1;
                if let KeyCode::Char(c) = code {
                    assert!(
                        !MULTIPLEXER.contains(&c),
                        "Ctrl-{c} in {label} is tmux's — it never reaches this process, so a \
                         keybar printing it promises a key that does nothing"
                    );
                }
            }
        }
        assert!(checked > 0, "the scan found no Ctrl verbs to check");
    }

    /// Alt is unpressable on a stock macOS terminal, so a keybar that teaches
    /// it teaches a key the reader does not have. The aliases still fire — see
    /// `on_chord` — but nothing may advertise them.
    #[test]
    fn no_screen_teaches_an_alt_chord() {
        let printed = all_documented_chords();
        assert!(!printed.is_empty(), "the scan found nothing to check");
        for label in printed {
            assert!(
                !label.starts_with("Alt-"),
                "{label} is printed, but Option does not send Alt unless the terminal is \
                 configured to — the Ctrl spelling is the one that can be typed"
            );
        }
    }

    /// The verbs that must stay one keypress away, and the readline keys that
    /// were never Jod's to move. Everything else is a destination and lives
    /// behind the leader — see the module header for why the line falls there.
    #[test]
    fn the_verbs_that_work_mid_sentence_are_the_ones_with_a_chord() {
        let printed: Vec<&str> = GLOBAL.iter().map(|b| b.key).collect();
        for verb in [
            "Ctrl-G", "Ctrl-F", "Ctrl-B", "Ctrl-X", "Ctrl-R", "Ctrl-N", "Ctrl-Y", "Ctrl-T",
            "Ctrl-O", "Ctrl-P", "Ctrl-V",
        ] {
            assert!(verb_of(verb).is_some(), "{verb} is not on the global list");
        }
        // Readline's, not ours to move.
        for kept in ["Ctrl-U", "Ctrl-W"] {
            assert!(
                printed.contains(&kept),
                "{kept} must stay where every shell puts it"
            );
        }
    }

    /// All eleven free letters are now spent — `Ctrl-V` was the last, and
    /// dictation took it. This is not a failure state, but it *is* the fact
    /// that decides what happens to the next verb, so it is asserted rather
    /// than left to be rediscovered by whoever adds one.
    ///
    /// When this test fails, the keymap is full and the choice is deliberate:
    /// the new verb is a destination and goes behind the leader, or something
    /// already holding a letter is demoted to make room. It is **not** a
    /// licence to take a letter back off the multiplexer —
    /// `no_verb_sits_on_a_chord_a_multiplexer_takes` still refuses that.
    #[test]
    fn every_free_letter_is_spent_so_the_next_verb_is_a_decision() {
        let free = ['b', 'f', 'g', 'n', 'o', 'p', 'r', 't', 'v', 'x', 'y'];
        for letter in free {
            let chord = format!("Ctrl-{}", letter.to_ascii_uppercase());
            assert!(
                verb_of(&chord).is_some(),
                "{chord} is free again — that is room for a verb, not a gap to leave. \
                 See the module header before spending it."
            );
        }
        assert_eq!(
            GLOBAL.iter().filter(|b| is_chord(b.key)).count(),
            free.len() + 5,
            "the global table is the eleven letters plus the arrows and readline's four"
        );
    }

    fn verb_of(chord: &str) -> Option<&'static Key> {
        GLOBAL.iter().find(|b| b.key == chord)
    }
}
