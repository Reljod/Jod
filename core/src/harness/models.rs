//! What models a harness will accept.
//!
//! `/model <name>` is only usable if you already know the name, and none of the
//! three harnesses spell their models the same way: `opus` is a Claude Code
//! alias, OpenCode wants `opencode/claude-opus-5`, AGY wants
//! `claude-opus-4-6-thinking`. A typo is not rejected at the prompt — it is
//! handed to the harness, which fails the whole turn. So the list has to come
//! from the harness itself wherever the harness will produce one.
//!
//! Two of them will: `opencode models` and `agy models` both print one model
//! per line. Claude Code has no such subcommand — `claude models` is read as a
//! *prompt* and hangs waiting for an answer — so its list is the static one
//! below, which is why this module has both a catalogue and a parser.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::HarnessKind;

/// One model a harness will accept, and something to say about it.
///
/// `id` is passed to `--model` verbatim: it is the harness's own spelling, not
/// a normalised one. Normalising would defeat the point — the reason this
/// exists is that the harnesses disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub label: String,
}

impl Model {
    fn new(id: impl Into<String>, label: impl Into<String>) -> Model {
        Model {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// How long a harness gets to print its model list.
///
/// AGY fetches the list over the network, so this is a network timeout rather
/// than a process one. Generous, because it runs once per harness off the
/// render path and a list that arrives late is still better than no list.
const TIMEOUT: Duration = Duration::from_secs(15);

impl HarnessKind {
    /// Every model this harness will accept, best effort.
    ///
    /// Empty when the binary is missing, the subcommand fails, or it takes
    /// longer than [`TIMEOUT`] — never an error, because the caller is a
    /// completion popup and a popup that reports a failure is worse than one
    /// that does not appear. `/model <anything>` still works: the list is an
    /// aid, not a gate.
    pub fn models(&self) -> Vec<Model> {
        match self {
            // No `models` subcommand: `claude models` is taken as a prompt and
            // opens a session. The list below is what `--model` documents plus
            // the full ids of the models those aliases point at.
            HarnessKind::ClaudeCode => claude_models(),
            HarnessKind::OpenCode | HarnessKind::Agy => ask(*self).unwrap_or_default(),
        }
    }
}

/// Ask a harness what it accepts. `None` at the first thing that does not work.
fn ask(kind: HarnessKind) -> Option<Vec<Model>> {
    let bin = kind.locate()?;
    let out = capture(&bin, "models")?;
    Some(parse(kind, &out))
}

/// The models Claude Code accepts, which nothing on the machine can be asked.
///
/// Aliases first: they are what a person types, and they follow the latest
/// model of each family without this list having to be edited. The full ids
/// below them are for pinning one version — the thing an alias cannot do.
fn claude_models() -> Vec<Model> {
    vec![
        Model::new("opus", "Claude Opus 5 — the latest Opus"),
        Model::new("sonnet", "Claude Sonnet 5 — the latest Sonnet"),
        Model::new("haiku", "Claude Haiku 4.5 — fastest, cheapest"),
        Model::new("fable", "Claude Fable 5 — the most capable"),
        Model::new("opus[1m]", "Claude Opus 5 with a 1M-token context"),
        Model::new("sonnet[1m]", "Claude Sonnet 5 with a 1M-token context"),
        Model::new("claude-opus-5", "pinned — Claude Opus 5"),
        Model::new("claude-opus-4-8", "pinned — Claude Opus 4.8"),
        Model::new("claude-sonnet-5", "pinned — Claude Sonnet 5"),
        Model::new("claude-haiku-4-5", "pinned — Claude Haiku 4.5"),
        Model::new("claude-fable-5", "pinned — Claude Fable 5"),
    ]
}

/// Read a harness's model list off its stdout.
///
/// Its own function, and a pure one, because the two formats are the part that
/// can silently drift when a harness changes its output — and a parser that can
/// only be exercised by installing the harness is a parser nobody re-checks.
pub fn parse(kind: HarnessKind, stdout: &str) -> Vec<Model> {
    stdout
        .lines()
        .filter_map(|line| match kind {
            // `id<TAB>Human Name`. The tab is what makes a row a row: AGY
            // prints "Fetching available models..." first, and a line without
            // one is that, not a model.
            HarnessKind::Agy => {
                let (id, label) = line.split_once('\t')?;
                let (id, label) = (id.trim(), label.trim());
                (!id.is_empty()).then(|| Model::new(id, label))
            }
            // `provider/model`, one per line, nothing else on the line. The
            // provider becomes the label: with sixty-odd models from a dozen
            // providers, *whose* model this is is the only thing the id does
            // not already say.
            HarnessKind::OpenCode => {
                let id = line.trim();
                if id.is_empty() || id.contains(char::is_whitespace) {
                    return None;
                }
                let label = id.split_once('/').map(|(p, _)| p).unwrap_or_default();
                Some(Model::new(id, label))
            }
            // Never asked — `claude_models` is the whole list.
            HarnessKind::ClaudeCode => None,
        })
        .collect()
}

/// Whether a harness's own list has this name in it.
///
/// Only ask this of a list that came back non-empty. An empty list means the
/// harness could not be asked — the binary is missing, the subcommand failed,
/// it timed out — and "not in an empty list" says nothing about the name.
pub fn accepts(name: &str, models: &[Model]) -> bool {
    models.iter().any(|m| m.id == name)
}

/// The ids in this list that a name the harness does not have was reaching for.
///
/// Two rules, because there are two ways to get a real model's name wrong and
/// both of them happen. Dropping the provider is the common one: OpenCode
/// spells Opus `opencode/claude-opus-5`, Claude Code spells it `claude-opus-5`,
/// and a name carried over from one to the other is right about the model and
/// wrong about nothing else. So a listed id whose tail matches the typed tail
/// is the first suggestion — which also covers reaching for the right model
/// under the wrong provider, `anthropic/claude-opus-5` for OpenCode's own.
///
/// The second is a missing suffix, which is how AGY differs: it has
/// `claude-opus-4-6-thinking` and no bare `claude-opus-4-6`. Any listed id
/// containing the typed name catches that.
///
/// Three at most. This exists to put the right name on the screen next to the
/// wrong one, and a list of ten is the model list again, which the person can
/// already see.
pub fn nearest(name: &str, models: &[Model]) -> Vec<String> {
    let wanted = tail(name).to_lowercase();
    if wanted.is_empty() {
        return vec![];
    }
    let mut found: Vec<String> = models
        .iter()
        .filter(|m| tail(&m.id).eq_ignore_ascii_case(&wanted))
        .map(|m| m.id.clone())
        .collect();
    for m in models {
        if found.len() >= 3 {
            break;
        }
        if m.id.to_lowercase().contains(&wanted) && !found.contains(&m.id) {
            found.push(m.id.clone());
        }
    }
    found.truncate(3);
    found
}

/// A model id without its provider — `opencode/claude-opus-5` is Opus 5, and so
/// is a bare `claude-opus-5`.
fn tail(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Run `<bin> <arg>` and hand back its stdout, or nothing.
///
/// Killed at [`TIMEOUT`] rather than waited on: this is called from a blocking
/// task, and a harness that hangs on `models` would hold that thread for the
/// rest of the session. Stdin is closed so a subcommand that turns out to be
/// interactive ends rather than waits.
fn capture(bin: &Path, arg: &str) -> Option<String> {
    let mut child = Command::new(bin)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }

    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `agy models` prints, header line and all.
    #[test]
    fn agy_rows_are_tab_separated_and_the_header_is_not_one() {
        let out = "Fetching available models...\n\
                   gemini-3.6-flash-high\tGemini 3.6 Flash (High)\n\
                   claude-sonnet-4-6\tClaude Sonnet 4.6 (Thinking)\n";
        assert_eq!(
            parse(HarnessKind::Agy, out),
            vec![
                Model::new("gemini-3.6-flash-high", "Gemini 3.6 Flash (High)"),
                Model::new("claude-sonnet-4-6", "Claude Sonnet 4.6 (Thinking)"),
            ]
        );
    }

    /// The shape `opencode models` prints. The provider is the label because
    /// the id already carries the model name.
    #[test]
    fn opencode_rows_are_bare_ids_labelled_by_provider() {
        let out = "opencode/claude-opus-5\nopencode/gemini-3.1-pro\n\n";
        assert_eq!(
            parse(HarnessKind::OpenCode, out),
            vec![
                Model::new("opencode/claude-opus-5", "opencode"),
                Model::new("opencode/gemini-3.1-pro", "opencode"),
            ]
        );
    }

    /// Anything a harness prints around its list — progress lines, warnings,
    /// a trailing blank — is not a model, and a model list with prose in it
    /// would be handed to `--model` as a name.
    #[test]
    fn prose_lines_are_not_models() {
        assert!(parse(HarnessKind::OpenCode, "no models configured\n").is_empty());
        assert!(parse(HarnessKind::Agy, "not logged in\n").is_empty());
    }

    /// The list OpenCode actually prints on Reljod's box, cut down to the rows
    /// these tests need.
    fn opencode_list() -> Vec<Model> {
        parse(
            HarnessKind::OpenCode,
            "opencode/claude-opus-5\n\
             opencode/claude-sonnet-5\n\
             opencode/hy3-free\n",
        )
    }

    /// The failure this was written for. Reljod asked main "what's the weather
    /// today", the run died two seconds in with `UnknownError: Unexpected
    /// server error. Check server logs for details.`, and the cause was the
    /// conversation holding `claude-opus-5` — Claude Code's spelling — against
    /// an OpenCode harness that only has `opencode/claude-opus-5`. The name is
    /// not in the list, and the id it was reaching for is.
    #[test]
    fn a_claude_code_spelling_is_not_a_model_opencode_has() {
        let list = opencode_list();
        assert!(!accepts("claude-opus-5", &list));
        assert_eq!(
            nearest("claude-opus-5", &list),
            vec!["opencode/claude-opus-5"]
        );
    }

    /// Right model, wrong provider. The tail is what identifies the model, so
    /// the provider being wrong is the same mistake as the provider being
    /// absent and gets the same answer.
    #[test]
    fn the_right_model_under_the_wrong_provider_still_finds_the_right_id() {
        assert_eq!(
            nearest("anthropic/claude-opus-5", &opencode_list()),
            vec!["opencode/claude-opus-5"]
        );
    }

    /// AGY's names carry a suffix Claude Code's do not, so the tails never
    /// match and only the substring rule can find these.
    #[test]
    fn a_name_missing_its_suffix_finds_the_ids_that_extend_it() {
        let list = parse(
            HarnessKind::Agy,
            "claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n\
             claude-opus-4-6-fast\tClaude Opus 4.6 (Fast)\n\
             gemini-3.6-flash-high\tGemini 3.6 Flash (High)\n",
        );
        assert_eq!(
            nearest("claude-opus-4-6", &list),
            vec!["claude-opus-4-6-thinking", "claude-opus-4-6-fast"]
        );
    }

    /// A name the harness does have is not a mistake, and nothing should be
    /// suggested in place of it.
    #[test]
    fn a_name_the_harness_has_is_accepted() {
        let list = opencode_list();
        assert!(accepts("opencode/hy3-free", &list));
    }

    /// An empty list is a harness that could not be asked, not a harness with
    /// no models. Suggesting nothing is the only honest answer, and the caller
    /// must not read it as "this name is wrong".
    #[test]
    fn an_unavailable_list_suggests_nothing() {
        assert!(nearest("claude-opus-5", &[]).is_empty());
    }

    /// A name with nothing recognisable in it matches nothing, rather than
    /// matching everything through an empty substring.
    #[test]
    fn a_name_with_no_tail_matches_nothing() {
        assert!(nearest("/", &opencode_list()).is_empty());
        assert!(nearest("", &opencode_list()).is_empty());
    }

    /// Three at most, however many the list could offer. `claude` matches
    /// plenty and a wall of them is the model list, which `/model` already
    /// shows.
    #[test]
    fn no_more_than_three_suggestions() {
        let list = parse(
            HarnessKind::OpenCode,
            "opencode/claude-opus-5\nopencode/claude-opus-4-8\n\
             opencode/claude-sonnet-5\nopencode/claude-haiku-4-5\n\
             opencode/claude-fable-5\n",
        );
        assert_eq!(nearest("claude", &list).len(), 3);
    }

    /// Claude Code's list is static, so the only thing that can go wrong with
    /// it is being empty.
    #[test]
    fn claude_code_has_a_list_without_being_asked() {
        let models = HarnessKind::ClaudeCode.models();
        assert!(models.iter().any(|m| m.id == "opus"));
        assert!(models.iter().any(|m| m.id == "claude-opus-5"));
    }
}
