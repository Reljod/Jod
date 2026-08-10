//! What every screen says its keys are.
//!
//! Two places have to agree: the keybar that is always on screen, and the `?`
//! overlay that carries the long tail. They are generated from one table here
//! so they cannot drift — and because the same letter deliberately means
//! different things on different screens (`a` attaches in the fleet and answers
//! an escalation in goals), a screen whose verbs are *not* printed would be a
//! trap rather than a shortcut. That is the condition on the whole design, not
//! an afterthought.

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
pub const GLOBAL: &[Key] = &[
    k("Ctrl-K", "the workspace menu"),
    k("Ctrl-A", "fleet"),
    k("Ctrl-G", "team"),
    k("Ctrl-B", "delegate the typed line"),
    k("Ctrl-X", "stop the run being watched"),
    k("Ctrl-T", "show or hide reasoning"),
    k("Ctrl-O", "show or hide tool output"),
    k("Ctrl-L", "clear the transcript"),
    k("Ctrl-U", "clear the input line"),
    k("Ctrl-W", "delete the previous word"),
    k("Ctrl-↑↓", "scroll the transcript"),
    k("Ctrl-C", "quit — twice while agents run"),
];


const CHAT: &[Key] = &[
    k("Ctrl-B", "delegate"),
    k("Ctrl-A", "fleet"),
    k("Ctrl-K", "menu"),
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

const TEAM: &[Key] = &[
    k("⏎", "mark done"),
    k("↑↓", "pick"),
    k("/", "filter"),
];

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
        Workspace::Chat => "Ctrl-X stop · Ctrl-C quit",
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
    #[test]
    fn no_documented_chord_is_one_the_terminal_owns() {
        let stolen = [
            "Ctrl-S", "Ctrl-Q", "Ctrl-Z", "Ctrl-H", "Ctrl-I", "Ctrl-J", "Ctrl-M",
        ];
        for binding in GLOBAL {
            assert!(
                !stolen.contains(&binding.key),
                "{} is the terminal's, not ours",
                binding.key
            );
        }
    }
}
