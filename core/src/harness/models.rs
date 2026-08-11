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

    /// Claude Code's list is static, so the only thing that can go wrong with
    /// it is being empty.
    #[test]
    fn claude_code_has_a_list_without_being_asked() {
        let models = HarnessKind::ClaudeCode.models();
        assert!(models.iter().any(|m| m.id == "opus"));
        assert!(models.iter().any(|m| m.id == "claude-opus-5"));
    }
}
