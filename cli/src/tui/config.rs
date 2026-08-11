//! Preferences that outlive the session.
//!
//! `/thinking` used to be a bool on `App` and nothing else, so the answer to
//! "do I want to watch the model reason" had to be given again every time the
//! TUI started — and a setting you have to re-choose on every launch is one
//! nobody keeps. This is the layer that remembers it: a named set of
//! preferences, each with a key in `settings`, a built-in default, and a parser
//! for what a person may type at it.
//!
//! Pure on purpose. Everything here is a string going into the store or coming
//! back out, so what `/config` accepts, refuses and prints is testable against
//! `Store::in_memory()` with no terminal — the same separation of parsing from
//! doing that [`super::command`] keeps.
//!
//! **Keys are namespaced by who *decides*, not by who reads.** `tui.*` is how
//! this screen looks, which nothing outside the terminal could want. `default.*`
//! is what Jod picks when nobody says, which the daemon, a webhook rule and
//! `jod run` have as much claim to as the TUI does. Calling the harness
//! preference `tui.harness` would have made the first non-TUI reader either
//! wrong or a second key meaning the same thing.
//!
//! **Unset is not the same as set-to-the-default.** `Store::setting` gives back
//! `None` for "no opinion", and an opinionless preference follows the built-in
//! default when the built-in changes, where a chosen one does not. `/config`
//! prints which of the two you are looking at, and `/config <key> default` is
//! how a choice is given up again.

use jod_core::error::Result;
use jod_core::store::Store;
use jod_core::{HarnessKind, PermissionPolicy};

use super::command::harness_named;

/// One thing a person can decide once and have Jod remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pref {
    /// Whether the harness's reasoning reaches the transcript.
    Thinking,
    /// Whether what tools gave back reaches it too.
    Details,
    /// Which harness a conversation starts on.
    Harness,
    /// How much a turn may do without asking.
    Mode,
}

impl Pref {
    /// Every preference, in the order `/config` lists them: the two that change
    /// what you see first, then the two that change what happens.
    pub const ALL: [Pref; 4] = [Pref::Thinking, Pref::Details, Pref::Harness, Pref::Mode];

    /// What you type at `/config`. Deliberately the same word as the command
    /// that toggles it — `/thinking` and `/config thinking` are one setting
    /// reached two ways, not two settings that drift.
    pub fn name(self) -> &'static str {
        match self {
            Pref::Thinking => "thinking",
            Pref::Details => "details",
            Pref::Harness => "harness",
            Pref::Mode => "mode",
        }
    }

    /// Where it is stored. See the module doc for why the two namespaces.
    pub fn key(self) -> &'static str {
        match self {
            Pref::Thinking => "tui.show_thinking",
            Pref::Details => "tui.show_tool_output",
            Pref::Harness => "default.harness",
            Pref::Mode => "default.permission",
        }
    }

    /// One line about what it does, for the list.
    pub fn what(self) -> &'static str {
        match self {
            Pref::Thinking => "show the harness reasoning",
            Pref::Details => "show what tools returned",
            Pref::Harness => "which harness a new conversation starts on",
            Pref::Mode => "how much a turn may do without asking",
        }
    }

    /// What you get having said nothing.
    pub fn fallback(self) -> Value {
        match self {
            // Both on, for the same reason: the point of sitting in front of a
            // harness is watching it work. Turning them off is a choice about
            // noise, and choices are what this file stores.
            Pref::Thinking | Pref::Details => Value::Flag(true),
            Pref::Harness => Value::Harness(HarnessKind::ClaudeCode),
            Pref::Mode => Value::Mode(PermissionPolicy::default()),
        }
    }

    /// Read a typed value, or `None` if this preference does not take it.
    /// Never a guess: an unrecognised word is refused and named back.
    pub fn parse(self, text: &str) -> Option<Value> {
        match self {
            Pref::Thinking | Pref::Details => parse_flag(text).map(Value::Flag),
            Pref::Harness => harness_named(text).map(Value::Harness),
            Pref::Mode => jod_core::mcp::parse_permission(text).map(Value::Mode),
        }
    }

    /// Everything this preference accepts, for the completion popup and for the
    /// sentence a refused value gets. One list, so the popup cannot offer
    /// something `parse` then rejects.
    pub fn choices(self) -> Vec<String> {
        match self {
            Pref::Thinking | Pref::Details => vec!["on".into(), "off".into()],
            Pref::Harness => HarnessKind::ALL
                .into_iter()
                .map(|k| Value::Harness(k).label())
                .collect(),
            Pref::Mode => PermissionPolicy::ALL
                .into_iter()
                .map(|m| Value::Mode(m).label())
                .collect(),
        }
    }

    /// The preference that word names, if any.
    pub fn named(name: &str) -> Option<Pref> {
        let name = name.trim().to_ascii_lowercase();
        Pref::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// What one preference is set to.
///
/// Typed rather than a bare string so a caller gets a `PermissionPolicy` back
/// and not something it has to parse a second time — the second parse being
/// where two spellings of one setting come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Flag(bool),
    Harness(HarnessKind),
    Mode(PermissionPolicy),
}

impl Value {
    /// The spelling written to the database.
    ///
    /// The canonical one in each case — `claude_code`, `accept_edits` — so a
    /// row here reads the same as the harness column of a schedule, and so
    /// `parse` reads back exactly what `store` wrote.
    pub fn stored(&self) -> String {
        match self {
            Value::Flag(true) => "on".into(),
            Value::Flag(false) => "off".into(),
            Value::Harness(k) => k.id().to_string(),
            Value::Mode(m) => m.as_str().to_string(),
        }
    }

    /// The spelling shown on screen.
    ///
    /// Must be one `Pref::parse` accepts: what `/config` prints is what a
    /// person types back at it, and a list showing `Claude Code` next to a
    /// parser wanting `claude` is a list that teaches the wrong word.
    pub fn label(&self) -> String {
        match self {
            Value::Flag(true) => "on".into(),
            Value::Flag(false) => "off".into(),
            Value::Harness(HarnessKind::ClaudeCode) => "claude".into(),
            Value::Harness(HarnessKind::OpenCode) => "opencode".into(),
            Value::Harness(HarnessKind::Agy) => "agy".into(),
            Value::Mode(m) => m.label().to_string(),
        }
    }

    /// The flag this is, for a caller that already knows which preference it
    /// asked for. `None` when the preference is not a flag at all.
    pub fn flag(&self) -> Option<bool> {
        match self {
            Value::Flag(on) => Some(*on),
            _ => None,
        }
    }
}

/// `on`, `off`, and the words people reach for instead.
///
/// Not a `bool::from_str`: `true`/`false` is what a programmer types and
/// `on`/`off`/`yes`/`no` is what everyone else does, and refusing the second
/// group would make the command feel broken for no gain.
fn parse_flag(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "y" | "1" | "show" | "shown" => Some(true),
        "off" | "false" | "no" | "n" | "0" | "hide" | "hidden" => Some(false),
        _ => None,
    }
}

/// What a preference is worth right now, and who decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub pref: Pref,
    pub value: Value,
    /// Whether somebody chose this, as against it being the built-in default.
    pub chosen: bool,
    /// Text in the store this build cannot read. `value` is then the fallback,
    /// and the line has to say so — a preference silently ignored is exactly
    /// the failure `/config` exists to prevent.
    pub unreadable: Option<String>,
}

impl Current {
    /// One line for the list: name, value, who decided, what it does.
    pub fn line(&self) -> String {
        let origin = if self.chosen { "chosen" } else { "default" };
        let mut line = format!(
            "{:<9} {:<9} {:<8} {}",
            self.pref.name(),
            self.value.label(),
            origin,
            self.pref.what()
        );
        if let Some(junk) = &self.unreadable {
            line.push_str(&format!(
                " — the stored “{junk}” is not one I understand, so the default is in force"
            ));
        }
        line
    }
}

/// What one preference is currently worth.
pub fn read(store: &Store, pref: Pref) -> Result<Current> {
    let raw = store.setting(pref.key())?;
    Ok(match raw {
        None => Current {
            pref,
            value: pref.fallback(),
            chosen: false,
            unreadable: None,
        },
        Some(text) => match pref.parse(&text) {
            Some(value) => Current {
                pref,
                value,
                chosen: true,
                unreadable: None,
            },
            // A value written by a newer build, or by hand. The default is used
            // and the text is carried up so the screen can name it, rather than
            // being dropped and leaving a preference that appears never to have
            // been set.
            None => Current {
                pref,
                value: pref.fallback(),
                chosen: false,
                unreadable: Some(text),
            },
        },
    })
}

/// Every preference, in `Pref::ALL` order.
pub fn read_all(store: &Store) -> Result<Vec<Current>> {
    Pref::ALL.into_iter().map(|p| read(store, p)).collect()
}

pub fn write(store: &Store, pref: Pref, value: &Value) -> Result<()> {
    store.set_setting(pref.key(), &value.stored())
}

/// Give up a choice, so the built-in default wins — and keeps winning if it
/// changes. Not the same as writing the default's current value.
pub fn clear(store: &Store, pref: Pref) -> Result<bool> {
    store.clear_setting(pref.key())
}

/// What a `/config` line asked for. Only ever a request this build can carry
/// out: an unknown key or an impossible value is refused during parsing, so
/// nothing downstream has to handle "and what if it is nonsense".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `/config` — every preference and its value.
    List,
    /// `/config <key>` — one of them.
    Show(Pref),
    /// `/config <key> <value>` — set one.
    Set(Pref, Value),
    /// `/config <key> default` — forget the choice.
    Clear(Pref),
}

/// Read the argument of a `/config` line.
///
/// `Err` is the whole sentence to put on screen. An unknown key is reported and
/// never silently accepted: a preference that appears to be set and is not is
/// worse than one that refuses you, because you only find out weeks later that
/// the thing you turned off has been on the whole time.
pub fn request(arg: &str) -> std::result::Result<Request, String> {
    let mut parts = arg.split_whitespace();
    let Some(name) = parts.next() else {
        return Ok(Request::List);
    };
    let rest = parts.collect::<Vec<_>>().join(" ");

    let Some(pref) = Pref::named(name) else {
        return Err(format!(
            "{name} is not a preference — there are {}",
            joined(&Pref::ALL.map(|p| p.name().to_string()))
        ));
    };
    if rest.is_empty() {
        return Ok(Request::Show(pref));
    }
    // The same three words `/model` takes to mean "back to the built-in", so
    // "how do I undo this" has one answer across the whole command set.
    if matches!(
        rest.to_ascii_lowercase().as_str(),
        "default" | "clear" | "unset"
    ) {
        return Ok(Request::Clear(pref));
    }
    match pref.parse(&rest) {
        Some(value) => Ok(Request::Set(pref, value)),
        None => Err(format!(
            "{} does not take “{rest}” — it takes {}",
            pref.name(),
            joined(&pref.choices())
        )),
    }
}

/// Carry out a request and say what happened, one transcript line per line.
///
/// Every path returns something. A store that refuses the write says so rather
/// than leaving the toggle looking as though it stuck — the choice has already
/// been applied to the screen by then, and a person who is not told it was not
/// recorded will find out at the next launch.
pub fn apply(store: &Store, request: &Request) -> Vec<String> {
    match request {
        Request::List => match read_all(store) {
            Ok(all) => all.iter().map(Current::line).collect(),
            Err(e) => vec![format!("could not read the preferences: {e}")],
        },
        Request::Show(pref) => match read(store, *pref) {
            Ok(current) => vec![current.line()],
            Err(e) => vec![format!("could not read {}: {e}", pref.name())],
        },
        Request::Set(pref, value) => match write(store, *pref, value) {
            Ok(()) => vec![format!(
                "{} is {} — remembered for next time",
                pref.name(),
                value.label()
            )],
            Err(e) => vec![format!(
                "{} is {} for this session — it was not recorded: {e}",
                pref.name(),
                value.label()
            )],
        },
        Request::Clear(pref) => match clear(store, *pref) {
            Ok(true) => vec![format!(
                "{} follows the default again, which is {}",
                pref.name(),
                pref.fallback().label()
            )],
            Ok(false) => vec![format!(
                "{} was never set, so it is already the default, which is {}",
                pref.name(),
                pref.fallback().label()
            )],
            Err(e) => vec![format!("could not forget {}: {e}", pref.name())],
        },
    }
}

/// A human list: `a, b or c`. Used in every refusal, so being told what *is*
/// accepted costs the same as being told what is not.
fn joined(items: &[String]) -> String {
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory().expect("an in-memory store")
    }

    #[test]
    fn a_preference_nobody_set_falls_back_to_the_built_in() {
        let store = store();
        let current = read(&store, Pref::Thinking).unwrap();
        assert_eq!(
            current.value,
            Value::Flag(true),
            "thinking is on by default"
        );
        assert!(!current.chosen, "and nobody chose it");
        assert_eq!(current.unreadable, None);
    }

    /// The distinction the whole file rests on: "no opinion" follows a changed
    /// default and "I chose this" does not, so the two cannot be one bool.
    #[test]
    fn choosing_the_default_value_is_still_a_choice() {
        let store = store();
        write(&store, Pref::Thinking, &Value::Flag(true)).unwrap();
        let current = read(&store, Pref::Thinking).unwrap();
        assert_eq!(current.value, Value::Flag(true));
        assert!(
            current.chosen,
            "the value matches the default but was chosen"
        );
    }

    #[test]
    fn every_preference_round_trips_through_the_store() {
        let store = store();
        for (pref, value) in [
            (Pref::Thinking, Value::Flag(false)),
            (Pref::Details, Value::Flag(false)),
            (Pref::Harness, Value::Harness(HarnessKind::OpenCode)),
            (Pref::Mode, Value::Mode(PermissionPolicy::Plan)),
        ] {
            write(&store, pref, &value).unwrap();
            let back = read(&store, pref).unwrap();
            assert_eq!(
                back.value,
                value,
                "{} did not survive the store",
                pref.name()
            );
            assert!(back.chosen, "{} came back as unchosen", pref.name());
        }
    }

    /// Every mode and every harness, not just the two above: a value that
    /// cannot be written and read back is a setting that silently reverts.
    #[test]
    fn no_harness_or_mode_is_lost_on_the_way_to_the_database() {
        let store = store();
        for kind in HarnessKind::ALL {
            let value = Value::Harness(kind);
            write(&store, Pref::Harness, &value).unwrap();
            assert_eq!(
                read(&store, Pref::Harness).unwrap().value,
                value,
                "{kind:?}"
            );
        }
        for mode in PermissionPolicy::ALL {
            let value = Value::Mode(mode);
            write(&store, Pref::Mode, &value).unwrap();
            assert_eq!(read(&store, Pref::Mode).unwrap().value, value, "{mode:?}");
        }
    }

    /// What `/config` prints has to be what you can type back at it.
    #[test]
    fn every_value_a_preference_shows_can_be_typed_back_in() {
        for pref in Pref::ALL {
            for offered in pref.choices() {
                let parsed = pref.parse(&offered);
                assert!(
                    parsed.is_some(),
                    "{} offers {offered} and does not accept it",
                    pref.name()
                );
                assert_eq!(
                    parsed.unwrap().label(),
                    offered,
                    "{} does not print back what it was given",
                    pref.name()
                );
            }
        }
        // And the fallbacks, which are printed before anything is ever set.
        for pref in Pref::ALL {
            let shown = pref.fallback().label();
            assert_eq!(
                pref.parse(&shown),
                Some(pref.fallback()),
                "{}'s default prints as {shown}, which it will not take back",
                pref.name()
            );
        }
    }

    #[test]
    fn giving_up_a_choice_returns_the_preference_to_the_default() {
        let store = store();
        write(&store, Pref::Details, &Value::Flag(false)).unwrap();
        assert!(clear(&store, Pref::Details).unwrap());

        let current = read(&store, Pref::Details).unwrap();
        assert_eq!(current.value, Value::Flag(true));
        assert!(
            !current.chosen,
            "and it is no opinion again, not a chosen true"
        );
    }

    /// A value from a newer build must not read as "never set". The screen has
    /// to be able to say what is in there and that it is being ignored.
    #[test]
    fn a_stored_value_this_build_cannot_read_is_named_rather_than_dropped() {
        let store = store();
        store.set_setting(Pref::Mode.key(), "yolo").unwrap();

        let current = read(&store, Pref::Mode).unwrap();
        assert_eq!(
            current.value,
            Pref::Mode.fallback(),
            "the default is in force"
        );
        assert!(!current.chosen);
        assert_eq!(current.unreadable.as_deref(), Some("yolo"));
        assert!(current.line().contains("yolo"), "{}", current.line());
    }

    #[test]
    fn no_argument_asks_for_the_whole_list() {
        assert_eq!(request(""), Ok(Request::List));
        assert_eq!(request("   "), Ok(Request::List));
    }

    #[test]
    fn a_key_on_its_own_asks_for_that_one() {
        assert_eq!(request("thinking"), Ok(Request::Show(Pref::Thinking)));
        assert_eq!(request("MODE"), Ok(Request::Show(Pref::Mode)));
    }

    #[test]
    fn a_key_and_a_value_set_it() {
        assert_eq!(
            request("thinking off"),
            Ok(Request::Set(Pref::Thinking, Value::Flag(false)))
        );
        assert_eq!(
            request("harness opencode"),
            Ok(Request::Set(
                Pref::Harness,
                Value::Harness(HarnessKind::OpenCode)
            ))
        );
        assert_eq!(
            request("mode auto"),
            Ok(Request::Set(
                Pref::Mode,
                Value::Mode(PermissionPolicy::Bypass)
            ))
        );
    }

    #[test]
    fn the_words_that_give_a_choice_up_all_work() {
        for word in ["default", "clear", "unset"] {
            assert_eq!(
                request(&format!("details {word}")),
                Ok(Request::Clear(Pref::Details)),
                "{word}"
            );
        }
    }

    /// The rule the module doc states: never silently accepted.
    #[test]
    fn an_unknown_key_is_refused_and_the_real_ones_named() {
        let said = request("colour green").unwrap_err();
        assert!(said.contains("colour is not a preference"), "{said}");
        for pref in Pref::ALL {
            assert!(
                said.contains(pref.name()),
                "{said} does not mention {}",
                pref.name()
            );
        }
    }

    #[test]
    fn a_value_a_preference_does_not_take_is_refused_with_the_ones_it_does() {
        let said = request("mode yolo").unwrap_err();
        assert!(said.contains("yolo"), "{said}");
        assert!(said.contains("plan"), "{said}");
        assert!(said.contains("auto"), "{said}");

        let said = request("thinking maybe").unwrap_err();
        assert!(said.contains("on") && said.contains("off"), "{said}");
    }

    #[test]
    fn setting_one_says_it_was_remembered_and_it_was() {
        let store = store();
        let said = apply(&store, &Request::Set(Pref::Thinking, Value::Flag(false)));
        assert_eq!(said.len(), 1);
        assert!(said[0].contains("thinking is off"), "{}", said[0]);
        assert_eq!(
            store.setting("tui.show_thinking").unwrap().as_deref(),
            Some("off")
        );
    }

    #[test]
    fn listing_shows_every_preference_and_whether_it_was_chosen() {
        let store = store();
        write(&store, Pref::Thinking, &Value::Flag(false)).unwrap();

        let lines = apply(&store, &Request::List);
        assert_eq!(lines.len(), Pref::ALL.len());
        assert!(
            lines[0].contains("thinking") && lines[0].contains("chosen"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("details") && lines[1].contains("default"),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn showing_one_shows_only_that_one() {
        let store = store();
        let lines = apply(&store, &Request::Show(Pref::Harness));
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("harness") && lines[0].contains("claude"),
            "{}",
            lines[0]
        );
    }

    /// Two preferences must not share a key, or setting one would silently
    /// change the other.
    #[test]
    fn no_two_preferences_share_a_key_or_a_name() {
        let mut keys: Vec<&str> = Pref::ALL.iter().map(|p| p.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "two preferences share a key");

        let mut names: Vec<&str> = Pref::ALL.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two preferences share a name");
    }

    #[test]
    fn a_list_of_choices_reads_as_a_sentence() {
        assert_eq!(joined(&[]), "");
        assert_eq!(joined(&["on".into()]), "on");
        assert_eq!(joined(&["on".into(), "off".into()]), "on or off");
        assert_eq!(
            joined(&["plan".into(), "ask".into(), "auto".into()]),
            "plan, ask or auto"
        );
    }
}
