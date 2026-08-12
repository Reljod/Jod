//! Collecting a credential in the rail, without it ever becoming part of the
//! UI.
//!
//! This is the most security-sensitive file in the terminal, so the rules it
//! keeps are written down rather than left to be inferred.
//!
//! ## The value takes one path and leaves no copies
//!
//! Typed → [`Typed`] inside the overlay → moved out on `⏎` → `put_secret` →
//! dropped. That is the whole route. In particular the value **never**:
//!
//! - reaches `App::input`, so it cannot be recalled with `↑`, cannot be
//!   completed against, and cannot be sent to an agent by pressing enter twice;
//! - reaches `App::transcript` or any notice, so it is not in the scrollback,
//!   not in a rendered frame, and not in anything a test dumps;
//! - reaches `App::history`, which is the copy people forget about;
//! - survives its own `Action`, which moves rather than clones it;
//! - appears in a `Debug` rendering, because `Action` derives `Debug` and one
//!   stray `tracing::debug!` on a dispatched action would put a live credential
//!   into a log file. [`Typed`]'s `Debug` prints its length and nothing else.
//!
//! ## What the card says, and why it says it before the value is asked for
//!
//! A person about to paste a production token deserves to know where it is
//! going *first*. So the expanded card explains the destination, the
//! permissions, and the fact that the agent is told a name and never a value —
//! and only then offers the field.
//!
//! Two things are said afterwards and both are corrections to a comfortable
//! assumption: injection applies from the **next** spawn, not to the turn
//! already running; and a value under [`MIN_REDACTABLE_LEN`] is injected but
//! **not** scrubbed from output. A silent exception there is a leak nobody was
//! told about.

use jod_core::secrets::{Scope, SecretMeta, MIN_REDACTABLE_LEN};

/// A credential on its way to the store, and nowhere else.
///
/// A newtype rather than a `String` for one reason, and it is worth the file:
/// `Action` derives `Debug`, overlays derive `Debug`, and every ordinary
/// diagnostic in this codebase prints one or the other. A bare `String` would
/// be one `{:?}` away from a token in a log. This cannot be printed by
/// accident — the only way to see the value is to call [`Typed::reveal`], which
/// is named to be conspicuous in review.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Typed(String);

impl Typed {
    pub fn new() -> Typed {
        Typed(String::new())
    }

    pub fn push(&mut self, c: char) {
        self.0.push(c);
    }

    pub fn pop(&mut self) {
        self.0.pop();
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many characters have been typed, for the masked field.
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    /// The value itself. **The store is the only legitimate caller.**
    ///
    /// Not a rule the compiler can hold — the same shape `read_secret_value`
    /// uses on the other side — so it is stated here and kept true by review.
    /// A second caller is a second place a credential can escape from.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// Take the value out, leaving an empty one behind.
    ///
    /// Moving rather than cloning is the point: after this the overlay holds
    /// nothing, so a frame drawn between the keypress and the write has no
    /// credential in it to draw.
    pub fn take(&mut self) -> Typed {
        Typed(std::mem::take(&mut self.0))
    }
}

/// Length only. See the type's own note — this is what stops a `{:?}` on an
/// `Action` or an `Overlay` becoming a leak.
impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Typed(<{} chars>)", self.len())
    }
}

/// What the field shows while it is being typed into.
///
/// Never the characters. A shoulder, a screen share and a recorded terminal
/// are all ordinary, and a credential field that echoes is the one part of
/// this flow a user cannot undo after the fact.
pub fn masked(typed: &Typed) -> String {
    "•".repeat(typed.len())
}

/// Where the value will live, in the order a person needs to hear it.
///
/// Said *before* the field is offered, because the moment to learn where a
/// production token is about to be written is not after pasting it.
pub fn destination(name: &str, scope: Scope) -> Vec<String> {
    vec![
        format!("`{name}` will be stored outside every repository, in Jod's own"),
        format!("secrets directory, at owner-only permissions ({SECRET_MODE})."),
        String::new(),
        format!("Scope: {}. {}", scope.as_str(), scope_note(scope)),
        String::new(),
        "The agent is told the *name* only, and reads the value as an".to_string(),
        "environment variable. Nothing here puts it in a prompt, a".to_string(),
        "transcript, or the database.".to_string(),
    ]
}

/// The permissions the store sets, quoted so the card and the store agree.
const SECRET_MODE: &str = "0600 in a 0700 directory";

fn scope_note(scope: Scope) -> &'static str {
    match scope {
        // Named as the risk it is, not as a feature. Global is the setting
        // that hands one project's key to every agent on the box.
        Scope::Global => "every session on this box can use it.",
        Scope::Work => "only this work's sessions can use it.",
        Scope::Conversation => "only this conversation can use it.",
    }
}

/// What the rail says once a value has been stored.
///
/// Two corrections to comfortable assumptions, and both are the point of the
/// sentence rather than decoration:
///
/// 1. **Injection is from the next spawn.** A turn already running was given
///    its environment at exec and cannot be handed another variable. Somebody
///    who stores a key to unblock the run in front of them needs to know it
///    will not take effect until that run is restarted.
/// 2. **A short value is injected but not redacted.** Below
///    [`MIN_REDACTABLE_LEN`] the scrubber would replace half of ordinary output
///    with the marker, so it stands aside — which means an agent that echoes
///    this variable *will* put it in the transcript. That is a decision the
///    owner has to be told about at the moment they make it.
pub fn stored_note(meta: &SecretMeta) -> String {
    let mut said = format!(
        "{} stored ({} scope) — injected from the next spawn, not into a run already going",
        meta.name,
        meta.scope.as_str()
    );
    if !meta.redactable {
        said.push_str(&format!(
            ". Warning: {} characters is under the {MIN_REDACTABLE_LEN}-character floor, so it is \
             injected but NOT redacted from output — an agent that echoes it will put it in the \
             transcript",
            meta.length
        ));
    }
    said
}

/// What the card shows in place of an answer once the value is stored.
///
/// A name and a scope. Deliberately not a masked value, not a length, not a
/// prefix — a card that showed `sk-live-••••` would be telling anyone reading
/// over your shoulder which key it is and where it is from.
pub fn stored_summary(name: &str, scope: Scope) -> String {
    format!("stored {name} ({} scope)", scope.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, length: usize, redactable: bool) -> SecretMeta {
        SecretMeta {
            id: 1,
            name: name.into(),
            scope: Scope::Work,
            scope_id: "w1".into(),
            hint: "the deploy key".into(),
            length,
            redactable,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// The reason this is a newtype at all. `Action` and `Overlay` both derive
    /// `Debug`, and one `{:?}` in a diagnostic would otherwise be a credential
    /// in a log file.
    #[test]
    fn a_typed_value_never_prints_itself() {
        let mut typed = Typed::new();
        for c in "sk-live-abcdef123456".chars() {
            typed.push(c);
        }
        let printed = format!("{typed:?}");
        assert!(!printed.contains("sk-live"), "{printed}");
        assert!(!printed.contains("abcdef"), "{printed}");
        assert_eq!(printed, "Typed(<20 chars>)");
    }

    /// The field echoes a count, never the characters. A screen share, a
    /// shoulder and a recorded terminal are all ordinary.
    #[test]
    fn the_field_shows_dots_and_not_the_value() {
        let mut typed = Typed::new();
        for c in "hunter2".chars() {
            typed.push(c);
        }
        assert_eq!(masked(&typed), "•••••••");
        assert!(!masked(&typed).contains('h'));
    }

    /// Taking the value moves it, so a frame drawn between the keypress and
    /// the write has nothing left to draw.
    #[test]
    fn taking_the_value_leaves_the_overlay_holding_nothing() {
        let mut typed = Typed::new();
        for c in "abc".chars() {
            typed.push(c);
        }
        let taken = typed.take();
        assert_eq!(taken.reveal(), "abc");
        assert!(typed.is_empty(), "the overlay kept a copy");
        assert_eq!(masked(&typed), "");
    }

    #[test]
    fn backspace_shortens_the_value_rather_than_clearing_it() {
        let mut typed = Typed::new();
        for c in "abcd".chars() {
            typed.push(c);
        }
        typed.pop();
        assert_eq!(typed.reveal(), "abc");
        assert_eq!(typed.len(), 3);
    }

    /// Said before the field is offered: the moment to learn where a
    /// production token is going is not after pasting it.
    #[test]
    fn the_card_says_where_the_value_will_live_before_asking_for_it() {
        let said = destination("GITHUB_TOKEN", Scope::Work).join(" ");
        assert!(said.contains("GITHUB_TOKEN"), "{said}");
        assert!(said.contains("outside every repository"), "{said}");
        assert!(said.contains("0600"), "{said}");
        assert!(said.contains("environment variable"), "{said}");
        assert!(
            said.contains("only this work's sessions"),
            "the scope has to be stated as who can use it: {said}"
        );
    }

    /// Global is the setting that hands one project's key to every agent on
    /// the box, so it is named as the risk it is.
    #[test]
    fn a_global_scope_says_how_wide_it_is() {
        let said = destination("OPENAI_API_KEY", Scope::Global).join(" ");
        assert!(said.contains("every session on this box"), "{said}");
    }

    /// The correction that matters most: somebody storing a key to unblock the
    /// run in front of them needs to know it will not reach that run.
    #[test]
    fn storing_says_injection_starts_at_the_next_spawn() {
        let said = stored_note(&meta("GITHUB_TOKEN", 40, true));
        assert!(said.contains("next spawn"), "{said}");
        assert!(said.contains("not into a run already going"), "{said}");
    }

    /// A silent exception here is a leak nobody was told about.
    #[test]
    fn a_value_too_short_to_redact_says_so_loudly() {
        let said = stored_note(&meta("PIN", 4, false));
        assert!(said.contains("NOT redacted"), "{said}");
        assert!(
            said.contains(&MIN_REDACTABLE_LEN.to_string()),
            "it has to name the floor: {said}"
        );
        assert!(
            said.contains("echoes it will put it in the transcript"),
            "and the consequence, not just the fact: {said}"
        );
    }

    /// A long value earns no warning, so the warning keeps its meaning.
    #[test]
    fn an_ordinary_value_gets_no_redaction_warning() {
        let said = stored_note(&meta("GITHUB_TOKEN", 40, true));
        assert!(!said.contains("NOT redacted"), "{said}");
    }

    /// A name and a scope. `sk-live-••••` would tell a passer-by which key it
    /// is and where it came from.
    #[test]
    fn the_stored_card_shows_a_name_and_a_scope_and_nothing_else() {
        let said = stored_summary("GITHUB_TOKEN", Scope::Work);
        assert_eq!(said, "stored GITHUB_TOKEN (work scope)");
        assert!(!said.contains('•'), "not even a masked value: {said}");
    }
}
