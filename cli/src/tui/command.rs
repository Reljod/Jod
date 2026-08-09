//! Slash commands.
//!
//! Parsing is separated from doing, so the whole of "what did the user ask
//! for" is a pure function over a string and can be tested without a terminal,
//! a store or an agent.
//!
//! The set is deliberately smaller than OpenCode's. Every command here maps
//! onto something Jod can actually do; a command that would need a capability
//! the harness seam does not expose is *absent* rather than present and inert,
//! because a `/compact` that silently does nothing is worse than no `/compact`.
//! Unrecognised input is reported, never swallowed.

use jod_core::HarnessKind;

/// What a `/…` line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slash {
    Help,
    /// Use a different harness for the next turn.
    Harness(HarnessKind),
    /// Set the model, or clear it back to the harness default.
    Model(Option<String>),
    Thinking,
    /// Show or hide what tools gave back.
    Details,
    /// Start a fresh conversation, forgetting the session cursor.
    New,
    /// List conversations that can be resumed.
    Sessions,
    /// Continue a specific conversation by its harness-assigned id.
    Resume(String),
    Agents,
    Team,
    /// Clear the transcript on screen. The conversation is untouched.
    Clear,
    Exit,
    /// A `/word` nobody knows. Reported rather than sent to the agent.
    Unknown(String),
    /// A known command missing its argument.
    NeedsArgument(&'static str),
}

/// Parse a line as a slash command.
///
/// `None` means "this is not a command" — including a bare `/`, and anything
/// with leading whitespace, so a prompt that happens to start with a slash
/// (`/usr/bin/foo is missing`) still reaches the agent as long as it is a real
/// path rather than a single word.
pub fn parse(line: &str) -> Option<Slash> {
    let rest = line.strip_prefix('/')?;
    let mut parts = rest.split_whitespace();
    let name = parts.next()?.to_ascii_lowercase();
    let arg = parts.collect::<Vec<_>>().join(" ");
    let arg = arg.trim();

    Some(match name.as_str() {
        "help" | "?" => Slash::Help,
        "harness" | "agent" => match harness_from(arg) {
            Some(kind) => Slash::Harness(kind),
            None if arg.is_empty() => Slash::NeedsArgument("/harness <claude|opencode|agy>"),
            None => Slash::Unknown(format!("/harness {arg}")),
        },
        "model" | "models" => {
            if arg.is_empty() || arg == "default" || arg == "clear" {
                Slash::Model(None)
            } else {
                Slash::Model(Some(arg.to_string()))
            }
        }
        "thinking" | "reasoning" => Slash::Thinking,
        "details" | "output" => Slash::Details,
        "new" => Slash::New,
        "sessions" => Slash::Sessions,
        "resume" | "continue" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/resume <session-id>")
            } else {
                Slash::Resume(arg.to_string())
            }
        }
        "agents" => Slash::Agents,
        "team" => Slash::Team,
        "clear" => Slash::Clear,
        "exit" | "quit" | "q" => Slash::Exit,
        other => Slash::Unknown(format!("/{other}")),
    })
}

fn harness_from(name: &str) -> Option<HarnessKind> {
    match name.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" | "cc" => Some(HarnessKind::ClaudeCode),
        "opencode" | "open-code" | "open_code" | "oc" => Some(HarnessKind::OpenCode),
        "agy" | "antigravity" => Some(HarnessKind::Agy),
        _ => None,
    }
}

/// One line of `/help`, so the list and the parser cannot drift apart: every
/// command that appears here is one `parse` accepts.
pub const HELP: &[(&str, &str)] = &[
    ("/help", "this list"),
    ("/harness <name>", "claude, opencode or agy — takes effect next turn"),
    ("/model <name>", "set the model; no argument restores the default"),
    ("/thinking", "show or hide reasoning"),
    ("/details", "show or hide what tools returned"),
    ("/new", "start a fresh conversation"),
    ("/sessions", "conversations you can pick up"),
    ("/resume <id>", "continue one of them"),
    ("/agents", "the delegations panel (Ctrl-A)"),
    ("/team", "the team panel (Ctrl-G)"),
    ("/clear", "clear the transcript on screen"),
    ("/exit", "leave; running agents keep going"),
];

/// One thing the completion popup can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The whole line to put in the input if this is chosen.
    pub line: String,
    /// What is shown next to it.
    pub hint: &'static str,
}

/// What could complete the line being typed.
///
/// Empty means "no popup": either this is not a command, or it is already
/// finished. Completing arguments as well as names matters more than it looks
/// — `/harness ` is the point where a user has to remember three spellings.
pub fn completions(input: &str) -> Vec<Completion> {
    let Some(rest) = input.strip_prefix('/') else {
        return vec![];
    };

    // Still typing the command word: offer names.
    if !rest.contains(char::is_whitespace) {
        let typed = rest.to_ascii_lowercase();
        return HELP
            .iter()
            .filter(|(usage, _)| {
                usage
                    .split_whitespace()
                    .next()
                    .is_some_and(|name| name[1..].starts_with(&typed))
            })
            .map(|(usage, hint)| {
                let name = usage.split_whitespace().next().unwrap();
                let takes_argument = usage.contains('<');
                Completion {
                    // A command that takes an argument gets a trailing space,
                    // so accepting it leaves the cursor where the argument goes.
                    line: if takes_argument {
                        format!("{name} ")
                    } else {
                        name.to_string()
                    },
                    hint,
                }
            })
            .collect();
    }

    // Past the name: offer arguments for the commands that have a fixed set.
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let typed = parts.next().unwrap_or_default().trim_start().to_ascii_lowercase();
    if !matches!(name.as_str(), "harness" | "agent") {
        return vec![];
    }
    HarnessKind::ALL
        .into_iter()
        .filter(|k| k.id().replace('_', "").starts_with(&typed) || k.id().starts_with(&typed))
        .map(|k| Completion {
            line: format!("/{name} {}", short_name(k)),
            hint: k.label(),
        })
        .collect()
}

/// The spelling offered for a harness — the shortest one `parse` accepts.
fn short_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::ClaudeCode => "claude",
        HarnessKind::OpenCode => "opencode",
        HarnessKind::Agy => "agy",
    }
}

/// Whether pressing Enter should *run* the line rather than complete it.
///
/// True once what is typed is already a whole command, so `/help` + Enter runs
/// instead of demanding a second Enter — which is the difference between a
/// palette that helps and one that gets in the way.
pub fn is_complete(input: &str) -> bool {
    match parse(input) {
        None => true,
        Some(Slash::Unknown(_)) => false,
        Some(Slash::NeedsArgument(_)) => false,
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &str) -> Vec<String> {
        completions(input).into_iter().map(|c| c.line).collect()
    }

    #[test]
    fn a_plain_prompt_offers_no_completions() {
        assert!(completions("hello").is_empty());
        assert!(completions("").is_empty());
    }

    #[test]
    fn a_bare_slash_offers_everything() {
        assert_eq!(completions("/").len(), HELP.len());
    }

    #[test]
    fn typing_narrows_the_list() {
        let some = lines("/t");
        assert!(some.contains(&"/thinking".to_string()));
        assert!(some.contains(&"/team".to_string()));
        assert!(!some.contains(&"/help".to_string()));

        let one = lines("/th");
        assert_eq!(one, vec!["/thinking".to_string()]);
    }

    #[test]
    fn a_command_taking_an_argument_completes_with_a_trailing_space() {
        assert_eq!(lines("/harn"), vec!["/harness ".to_string()]);
        assert_eq!(lines("/hel"), vec!["/help".to_string()], "no argument, no space");
    }

    #[test]
    fn nonsense_completes_to_nothing() {
        assert!(completions("/zzzz").is_empty());
    }

    /// The bit that saves remembering three spellings.
    #[test]
    fn harness_arguments_are_completed_too() {
        let all = lines("/harness ");
        assert_eq!(all.len(), HarnessKind::ALL.len());
        assert!(all.contains(&"/harness claude".to_string()));
        assert!(all.contains(&"/harness agy".to_string()));

        assert_eq!(lines("/harness op"), vec!["/harness opencode".to_string()]);
    }

    /// Every offered harness spelling must be one the parser accepts, or the
    /// popup would suggest something that then fails.
    #[test]
    fn every_suggested_harness_parses() {
        for c in completions("/harness ") {
            assert!(
                matches!(parse(&c.line), Some(Slash::Harness(_))),
                "{} was suggested but does not parse",
                c.line
            );
        }
    }

    /// Likewise for command names: accepting a suggestion must never produce
    /// something the parser calls unknown.
    #[test]
    fn every_suggested_command_parses() {
        for c in completions("/") {
            let parsed = parse(c.line.trim());
            assert!(
                !matches!(parsed, Some(Slash::Unknown(_)) | None),
                "{} was suggested but parses as {parsed:?}",
                c.line
            );
        }
    }

    #[test]
    fn commands_that_take_an_argument_are_not_complete_until_it_is_given() {
        assert!(!is_complete("/harness"));
        assert!(is_complete("/harness claude"));
        assert!(!is_complete("/resume"));
        assert!(is_complete("/resume ses-1"));
    }

    #[test]
    fn a_finished_command_and_a_plain_prompt_are_both_complete() {
        assert!(is_complete("/help"));
        assert!(is_complete("/new"));
        assert!(is_complete("just a prompt"));
    }

    #[test]
    fn an_unknown_command_is_never_treated_as_complete() {
        assert!(!is_complete("/wibb"));
    }

    #[test]
    fn a_plain_prompt_is_not_a_command() {
        assert!(parse("hello there").is_none());
        assert!(parse("").is_none());
        assert!(parse("what about / this").is_none());
    }

    /// A bare slash is a typo, not a command, and must not be swallowed.
    #[test]
    fn a_bare_slash_is_not_a_command() {
        assert!(parse("/").is_none());
        assert!(parse("/   ").is_none());
    }

    #[test]
    fn help_answers_to_both_spellings() {
        assert_eq!(parse("/help"), Some(Slash::Help));
        assert_eq!(parse("/?"), Some(Slash::Help));
    }

    #[test]
    fn every_harness_can_be_named_including_its_short_forms() {
        for (text, expected) in [
            ("/harness claude", HarnessKind::ClaudeCode),
            ("/harness cc", HarnessKind::ClaudeCode),
            ("/harness opencode", HarnessKind::OpenCode),
            ("/harness oc", HarnessKind::OpenCode),
            ("/harness agy", HarnessKind::Agy),
            ("/harness antigravity", HarnessKind::Agy),
        ] {
            assert_eq!(parse(text), Some(Slash::Harness(expected)), "{text}");
        }
    }

    /// Every harness the build knows must be reachable by name, or a new one
    /// would be spawnable from the CLI and invisible from the TUI.
    #[test]
    fn no_harness_is_unreachable_from_the_tui() {
        for kind in HarnessKind::ALL {
            let by_id = parse(&format!("/harness {}", kind.id()));
            let by_label = parse(&format!("/harness {}", kind.label().replace(' ', "-")));
            assert!(
                by_id == Some(Slash::Harness(kind)) || by_label == Some(Slash::Harness(kind)),
                "{kind:?} cannot be selected: {by_id:?} / {by_label:?}"
            );
        }
    }

    #[test]
    fn an_unknown_harness_is_reported_rather_than_guessed() {
        assert_eq!(
            parse("/harness gpt"),
            Some(Slash::Unknown("/harness gpt".into()))
        );
    }

    #[test]
    fn harness_without_an_argument_says_what_it_wants() {
        assert_eq!(
            parse("/harness"),
            Some(Slash::NeedsArgument("/harness <claude|opencode|agy>"))
        );
    }

    #[test]
    fn model_takes_a_name_or_resets_to_the_default() {
        assert_eq!(
            parse("/model anthropic/claude-sonnet-5"),
            Some(Slash::Model(Some("anthropic/claude-sonnet-5".into())))
        );
        assert_eq!(parse("/model"), Some(Slash::Model(None)));
        assert_eq!(parse("/model default"), Some(Slash::Model(None)));
        assert_eq!(parse("/model clear"), Some(Slash::Model(None)));
    }

    #[test]
    fn resume_needs_an_id() {
        assert_eq!(parse("/resume ses-1"), Some(Slash::Resume("ses-1".into())));
        assert_eq!(parse("/continue ses-1"), Some(Slash::Resume("ses-1".into())));
        assert_eq!(
            parse("/resume"),
            Some(Slash::NeedsArgument("/resume <session-id>"))
        );
    }

    #[test]
    fn the_simple_commands_all_parse() {
        assert_eq!(parse("/thinking"), Some(Slash::Thinking));
        assert_eq!(parse("/details"), Some(Slash::Details));
        assert_eq!(parse("/output"), Some(Slash::Details));
        assert_eq!(parse("/reasoning"), Some(Slash::Thinking));
        assert_eq!(parse("/new"), Some(Slash::New));
        assert_eq!(parse("/sessions"), Some(Slash::Sessions));
        assert_eq!(parse("/agents"), Some(Slash::Agents));
        assert_eq!(parse("/team"), Some(Slash::Team));
        assert_eq!(parse("/clear"), Some(Slash::Clear));
        for text in ["/exit", "/quit", "/q"] {
            assert_eq!(parse(text), Some(Slash::Exit), "{text}");
        }
    }

    #[test]
    fn commands_are_case_insensitive_and_tolerate_spacing() {
        assert_eq!(parse("/HELP"), Some(Slash::Help));
        assert_eq!(parse("/Thinking"), Some(Slash::Thinking));
        assert_eq!(
            parse("/harness    OpenCode"),
            Some(Slash::Harness(HarnessKind::OpenCode))
        );
    }

    #[test]
    fn an_unknown_command_is_named_back_rather_than_sent_to_the_agent() {
        assert_eq!(parse("/wibble"), Some(Slash::Unknown("/wibble".into())));
        // The ones OpenCode has and Jod does not: reported, not silently inert.
        for missing in ["/compact", "/undo", "/share", "/themes"] {
            assert_eq!(parse(missing), Some(Slash::Unknown(missing.into())), "{missing}");
        }
    }

    /// `/help` must not list a command the parser rejects.
    #[test]
    fn every_documented_command_parses() {
        for (usage, _) in HELP {
            let word = usage.split_whitespace().next().unwrap();
            let parsed = parse(word).unwrap_or_else(|| panic!("{word} did not parse"));
            assert!(
                !matches!(parsed, Slash::Unknown(_)),
                "{usage} is documented but unknown to the parser"
            );
        }
    }
}
