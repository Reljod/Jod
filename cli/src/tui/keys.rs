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
//! ## Why the verbs are on Alt and the editing keys are not
//!
//! Every global chord used to be Ctrl, which put Jod in a fight it cannot win:
//! a multiplexer sits between the terminal and this process and takes Ctrl
//! chords first — tmux's own prefix is `Ctrl-B`, which was Jod's delegate key,
//! so the binding simply never arrived. Alt is not contended that way, so the
//! *screen and verb* chords live there now.
//!
//! What did **not** move is the handful of Ctrl chords the terminal itself has
//! taught everyone: `Ctrl-C`/`Ctrl-D` quit, `Ctrl-U` clears the line, `Ctrl-W`
//! deletes a word, `Ctrl-A`/`Ctrl-E` go to the ends of it. Moving those would
//! break muscle memory that predates Jod by forty years to solve a problem
//! nobody has — no multiplexer steals them, because every shell needs them.
//! `Ctrl-A` in fact comes *back* to readline here: it used to open the fleet,
//! which was the one Ctrl collision Jod inflicted on itself.
//!
//! Where Ctrl had no readline meaning, the old spelling still works but is no
//! longer printed — `Ctrl-T` still toggles reasoning. Keeping the alias costs
//! nothing and stops the move being a re-learning tax; printing it would
//! advertise the chord tmux eats. Where Ctrl *does* have a readline meaning the
//! Alt spelling is the only one, so that `Ctrl-A` can never again be ambiguous.
//!
//! On macOS, Option only reaches this process as Alt when the terminal is told
//! to send it: iTerm2's "Esc+" for the left Option key, Terminal.app's "Use
//! Option as Meta key". Without that the terminal eats it to type `å`.

// Only the drift net turns a printed label back into a keypress, and that is
// test-only — the running program prints these strings and never reads them.
#[cfg(test)]
use crossterm::event::{KeyCode, KeyModifiers};

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
    k("x", "delete — confirms first"),
    k("S", "cycle sort"),
    k("1–9", "jump to a workspace"),
    k("Esc / q", "back one level"),
    k("?", "these keys"),
];

/// The chords that work everywhere, including in the middle of typing.
///
/// Alt for Jod's own verbs, Ctrl for the line editing every terminal already
/// does — see the module header for why the split falls exactly there. Only one
/// spelling per binding is printed: the Ctrl aliases still fire, but a keybar
/// that advertised them would be advertising the chords tmux intercepts.
pub const GLOBAL: &[Key] = &[
    k("Alt-K", "the workspace menu"),
    k("Alt-A", "fleet"),
    k("Alt-G", "team"),
    k("Alt-N", "the oldest thing unread"),
    k("Alt-J", "background shells"),
    // The rail's two chords. Both are chords rather than letters for one
    // reason, and it is the reason E2.S3 gives: the chat box owns every bare
    // key, so a rail verb on `c` would type a `c` into the sentence being
    // written. See [`RAIL`].
    k("Alt-R", "show or hide the rail"),
    k("Alt-C", "the rail's next card"),
    k("Alt-P", "add a directory to work in"),
    k("Alt-S", "search every transcript"),
    k("Alt-Y", "copy the last reply"),
    k("Alt-B", "delegate the typed line"),
    k("Alt-X", "stop the run being watched"),
    k("Alt-F", "the typed line in $EDITOR"),
    k("Alt-T", "show or hide reasoning"),
    k("Alt-O", "show or hide tool output"),
    k("Alt-L", "clear the transcript"),
    k("Alt-↑↓", "scroll the transcript"),
    k("Ctrl-A/Home", "start of the line"),
    k("Ctrl-E/End", "end of the line"),
    k("Ctrl-U", "clear the input line"),
    k("Ctrl-W", "delete the previous word"),
    k("Ctrl-C/D", "quit — twice while agents run"),
];

const CHAT: &[Key] = &[
    k("Alt-B", "delegate"),
    k("Alt-A", "fleet"),
    k("Alt-K", "menu"),
    k("/", "commands"),
    k("?", "keys"),
];

/// The fleet is the widest screen, because it is the only one that is both a
/// list of runs and a handle on the conversation graph behind them: `s r d a`
/// act on the run under the cursor, `c b u U g f t` act on its thread. `/` is
/// last because it is the spine's, not the fleet's — see the module header.
///
/// `u` undoes and `U` puts it back. Lower case is undo on every screen that has
/// one — memory's `u` is an undo too — because undo and redo are a verb and its
/// inverse, and that is the one case where a habit transferring between screens
/// does damage rather than nothing.
///
/// `c` says **conversations** rather than "threads" because the two are
/// different things here and the screen's own prose already draws the line:
/// `c` lists conversations ("no conversations yet — every run starts one"),
/// while `b` "opens the branches of the selected run's thread". A bar reading
/// `c threads · b branches` would say `b` drills into what `c` lists, which is
/// not what either key does.
///
/// `g` is spelled `go to #` because `#` is the exact token printed beside each
/// branch in the listing — the label names what is on screen rather than
/// describing it.
const FLEET: &[Key] = &[
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
    k("c", "conversations"),
    k("b", "branches"),
    k("u", "undo"),
    k("U", "redo"),
    k("g", "go to #"),
    k("f", "fork"),
    k("t", "retry"),
    // The tree's own verbs, in force once there is a work to draw. Below the
    // run verbs because those act on the row and these act on the shape, and
    // the row is what people come here for; above `/` because that one is the
    // spine's and means the same thing on every screen.
    k("→←", "in / out"),
    k("space", "expand / collapse"),
    k("E", "expand all"),
    k("C", "collapse all"),
    k("z", "closed works"),
    k("/", "filter"),
];

const MEMORY: &[Key] = &[
    k("g", "graph"),
    k("l", "link"),
    k("t", "type"),
    k("e", "edit"),
    k("n", "new"),
    k("x", "forget"),
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
    k("x", "delete"),
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
    k("x", "delete"),
];

const TASKS: &[Key] = &[
    k("⏎", "mark done"),
    k("d", "delegate"),
    k("c", "claim"),
    k("o", "open run"),
    k("n", "new"),
    k("x", "remove"),
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
/// keyboard — which `Alt-C` is what gives it, and `Esc` is what takes away.
///
/// **Why a focus rather than a chord per verb.** The chat input turns every
/// bare key into text, so the rail could either have a chord for each of its
/// eight verbs — eight more chords to find free, on a keymap that has already
/// had to move off Ctrl once — or one chord that hands it the keyboard. It has
/// the second. Getting *in* is free and safe mid-sentence (`Alt-C` never
/// touches `App::input`); once in, the keys are ordinary letters, and `Esc`
/// gives the keyboard back with the typed line exactly as it was.
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
        Workspace::Chat => "Alt-X stop · Ctrl-C quit",
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

/// The which-key overlay's keybar line, which has to name the leader it is
/// waiting on — an overlay that says only "waiting for a key" tells you it is
/// stuck without telling you what unstuck it.
///
/// `making` is the `n` submenu, which is one keypress deeper and so names the
/// two-key route rather than the leader alone.
///
/// This lives here rather than in `ui.rs` for one reason: `ui.rs` is prose the
/// drift test cannot see. Spelled here, `Alt-K` is scanned and pressed like
/// every other advertised chord, so it cannot go stale the next time the
/// keymap moves. That is exactly how these four strings were left saying
/// `Ctrl-K` after the keymap had already moved to Alt.
pub fn which_key_hint(making: bool) -> String {
    if making {
        "Alt-K n … s schedule · g goal · h hook · m memory · t task".to_string()
    } else {
        "Alt-K … waiting for a key".to_string()
    }
}

/// The which-key overlay's border title. Same reasoning as `which_key_hint`,
/// and the two must name the same chord — which is why they sit together.
pub fn which_key_title(making: bool) -> &'static str {
    if making {
        " Alt-K n · new… "
    } else {
        " Alt-K "
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
/// Exit hints are sentences (`"Alt-X stop · Ctrl-C quit"`), not table rows, so
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
    for (_, bindings) in rail_keymap() {
        found.extend(
            bindings
                .iter()
                .filter(|b| is_chord(b.key))
                .map(|b| b.key.to_string()),
        );
    }
    found.extend(chords_in(RAIL_EXIT));
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
/// - `↑↓` means both arrows, so `Alt-↑↓` is two presses.
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
            .any(|(_, bindings)| bindings.iter().any(|b| b.key == "Alt-R")));
        assert!(RAIL_EXIT.contains("Esc"));
    }

    /// The two chords are the whole reason the rail is usable mid-sentence, so
    /// they are advertised where every screen can see them.
    #[test]
    fn the_rails_chords_are_taught_alongside_the_other_global_ones() {
        let printed: Vec<&str> = GLOBAL.iter().map(|b| b.key).collect();
        assert!(printed.contains(&"Alt-R"), "{printed:?}");
        assert!(printed.contains(&"Alt-C"), "{printed:?}");
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
        assert!(all_documented_chords().iter().any(|c| c == "Alt-X"));
        assert!(all_documented_chords().iter().any(|c| c == "Ctrl-C/D"));
    }

    /// An overlay that says only "waiting for a key" tells you it is stuck
    /// without telling you what unstuck it, and the two halves disagreeing
    /// would be worse than either.
    #[test]
    fn the_which_key_overlay_names_the_leader_that_opened_it() {
        for making in [false, true] {
            assert!(
                which_key_hint(making).contains("Alt-K"),
                "hint, making={making}"
            );
            assert!(
                which_key_title(making).contains("Alt-K"),
                "title, making={making}"
            );
        }
    }

    /// The forward drift test presses what is printed — but `Ctrl-K` still
    /// fires, so a screen that quietly went back to printing it would pass.
    /// Nothing else would catch that, and it is exactly the state the four
    /// which-key strings were found in after the keymap had already moved.
    ///
    /// `GLOBAL` is the one table allowed to name Ctrl, because it is where the
    /// readline keys are taught. Every other screen names Jod's verbs, and
    /// those are Alt — plus `Ctrl-C`, which any screen may print because
    /// leaving must never depend on finding the right table.
    #[test]
    fn no_screen_outside_the_global_table_teaches_a_ctrl_verb() {
        let mut printed: Vec<String> = Vec::new();
        for ws in Workspace::ALL {
            printed.extend(
                local(ws)
                    .iter()
                    .filter(|b| is_chord(b.key))
                    .map(|b| b.key.to_string()),
            );
            printed.extend(chords_in(keybar_exit(ws)));
        }
        for making in [false, true] {
            printed.extend(chords_in(&which_key_hint(making)));
            printed.extend(chords_in(which_key_title(making)));
        }
        assert!(!printed.is_empty(), "the scan found nothing to check");
        for label in printed {
            assert!(
                label.starts_with("Alt-") || label == "Ctrl-C",
                "{label} is printed on a screen — Jod's verbs live on Alt, and only GLOBAL teaches Ctrl"
            );
        }
    }

    /// The move off Ctrl exists to stop a multiplexer eating Jod's verbs, and
    /// it is only half done if the keybar still teaches the old spelling.
    #[test]
    fn the_verbs_are_advertised_on_alt_and_the_editing_keys_on_ctrl() {
        let printed: Vec<&str> = GLOBAL.iter().map(|b| b.key).collect();
        for verb in ["Alt-K", "Alt-A", "Alt-G", "Alt-B", "Alt-X", "Alt-L"] {
            assert!(printed.contains(&verb), "{verb} is not on the global list");
        }
        for stale in [
            "Ctrl-K", "Ctrl-G", "Ctrl-B", "Ctrl-X", "Ctrl-T", "Ctrl-O", "Ctrl-L",
        ] {
            assert!(
                !printed.contains(&stale),
                "{stale} still works, but printing it advertises the chord tmux takes"
            );
        }
        // Readline's, not ours to move.
        for kept in ["Ctrl-U", "Ctrl-W"] {
            assert!(
                printed.contains(&kept),
                "{kept} must stay where every shell puts it"
            );
        }
    }
}
