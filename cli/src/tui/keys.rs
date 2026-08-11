//! What every screen says its keys are.
//!
//! Two places have to agree: the keybar that is always on screen, and the `?`
//! overlay that carries the long tail. They are generated from one table here
//! so they cannot drift — and because the same letter deliberately means
//! different things on different screens (`a` attaches in the fleet and answers
//! an escalation in goals), a screen whose verbs are *not* printed would be a
//! trap rather than a shortcut. That is the condition on the whole design, not
//! an afterthought.
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

const FLEET: &[Key] = &[
    k("⏎", "watch"),
    k("s", "stop"),
    k("r", "resume"),
    k("d", "delegate again"),
    k("a", "attach"),
    k("/", "filter"),
];

const MEMORY: &[Key] = &[
    k("g", "graph"),
    k("e", "edit"),
    k("n", "new"),
    k("l", "link"),
    k("x", "forget"),
    k("t", "type"),
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

const GOALS: &[Key] = &[
    k("⏎", "last iteration"),
    k("r", "run now"),
    k("p", "pause"),
    k("e", "edit"),
    k("n", "new"),
    k("a", "answer escalation"),
];

const HOOKS: &[Key] = &[
    k("⏎", "open run"),
    k("t", "test payload"),
    k("p", "pause"),
    k("e", "edit"),
    k("n", "new"),
    k("c", "copy URL"),
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

/// The keybar's left half: this screen's verbs, joined.
pub fn keybar(ws: Workspace) -> String {
    local(ws)
        .iter()
        .map(|b| format!("{} {}", b.key, b.what))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The keybar's right half: the way out, which never changes.
pub fn keybar_exit(ws: Workspace) -> &'static str {
    match ws {
        Workspace::Chat => "Alt-X stop · Ctrl-C quit",
        _ => "Esc back · ? keys",
    }
}

/// The footer printed inside a workspace's own border, shorter than the keybar
/// because it repeats only what acts on the selected row.
pub fn footer(ws: Workspace) -> String {
    let verbs = local(ws)
        .iter()
        .take(4)
        .map(|b| format!("{} {}", b.key, b.what))
        .collect::<Vec<_>>()
        .join(" · ");
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

/// Does this label name a chord, rather than a bare key like `⏎` or `n`?
pub fn is_chord(label: &str) -> bool {
    label.starts_with("Ctrl-") || label.starts_with("Alt-")
}

/// The chords named inside a line of on-screen prose.
///
/// Exit hints are sentences (`"Alt-X stop · Ctrl-C quit"`), not table rows, so
/// a chord can arrive there as a substring nobody registered anywhere.
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

    /// The same letter means different things on different screens, which is
    /// only safe because both are on the keybar at all times.
    #[test]
    fn every_workspace_prints_its_own_verbs() {
        for ws in Workspace::ALL {
            let bar = keybar(ws);
            assert!(!bar.is_empty(), "{ws:?} has an empty keybar");
            for binding in local(ws) {
                assert!(
                    bar.contains(binding.key),
                    "{ws:?} does not print {}",
                    binding.key
                );
            }
        }
    }

    /// `a` attaches in the fleet and answers an escalation in goals. That is
    /// allowed, and this test pins the condition that makes it allowed.
    #[test]
    fn a_letter_that_changes_meaning_is_printed_on_both_screens() {
        assert!(keybar(Workspace::Fleet).contains("a attach"));
        assert!(keybar(Workspace::Goals).contains("a answer escalation"));
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
