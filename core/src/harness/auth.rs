//! Whether a harness can sign in, and what to do when it cannot.
//!
//! Jod does not own harness credentials and never will — that is the harness's
//! own configuration, and [`docs/harness-config.md`] says so. What Jod owns is
//! the *question*: a harness whose binary exists but whose account has expired
//! is not usable, and until this module existed nothing here could tell those
//! two states apart. `jod harnesses` called a harness ready because a file was
//! on disk, a run went out, and the whole of the failure a person saw was
//! `✗ failed · $0.0000 · 1s`.
//!
//! So there are three verbs here and no storage. Ask the harness what its
//! credentials look like ([`AuthState`]), hand a person to the harness's own
//! sign-in flow ([`HarnessKind::login_args`]), and when a run fails, work out
//! whether authentication is why ([`advice_for_failure`]).
//!
//! **Everything matched here was measured, not guessed.** Each string carries
//! the command that produced it, the same discipline `crate::prs` follows for
//! `gh`: a harness that changes its wording should make a test fail rather than
//! quietly stop being recognised.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::HarnessKind;

/// How long a harness gets to answer a question about its own credentials.
///
/// Bounded rather than trusted. `claude auth status` answers in well under a
/// second, but a harness that has never seen the subcommand reads the words as
/// a *prompt* — this is measured behaviour, `claude models` does exactly that
/// (→ `docs/harness-config.md`) — and a prompt does not exit. Without a
/// deadline, one stale install would hang `jod harnesses` for ever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// What a harness's own credentials look like right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthState {
    /// The harness answered, and it is signed in. `account` is whatever it
    /// names itself by — an email address for Claude Code, the list of
    /// configured providers for OpenCode — so a person can tell *which*
    /// account, which is the whole question when several exist.
    LoggedIn { account: Option<String> },
    /// The harness answered, and it is not signed in. A run started now fails.
    LoggedOut,
    /// Nothing could be asked. Either the harness is not installed, or it has
    /// no way of being asked at all — AGY has no `auth` subcommand — or the
    /// answer came back in a shape this module does not recognise.
    ///
    /// Deliberately not folded into [`LoggedOut`](AuthState::LoggedOut).
    /// "I could not find out" and "the answer is no" send a person to
    /// completely different places, and guessing the second would put a
    /// sign-in prompt in front of somebody whose credentials are fine.
    Unknown { why: String },
}

impl AuthState {
    fn unknown(why: impl Into<String>) -> AuthState {
        AuthState::Unknown { why: why.into() }
    }

    /// Whether a run started right now would get past authentication.
    ///
    /// `Unknown` answers `true`, because Jod must not stand in front of a
    /// harness it merely failed to interrogate.
    pub fn usable(&self) -> bool {
        !matches!(self, AuthState::LoggedOut)
    }

    /// One line for a person: what this state is, and who it is.
    pub fn describe(&self) -> String {
        match self {
            AuthState::LoggedIn { account: Some(a) } => format!("signed in as {a}"),
            AuthState::LoggedIn { account: None } => "signed in".to_string(),
            AuthState::LoggedOut => "not signed in".to_string(),
            AuthState::Unknown { why } => format!("unknown — {why}"),
        }
    }
}

impl HarnessKind {
    /// The harness's own "am I signed in?" command, after the program name.
    ///
    /// `None` means the harness has no way of being asked. That is a real
    /// answer and is reported as [`AuthState::Unknown`] rather than papered
    /// over — a check Jod cannot run is not a check Jod may claim to have run.
    ///
    /// Measured against the installs on this machine, 2026-08-21:
    /// `claude auth status --json` (Claude Code 2.1.231) and
    /// `opencode auth list` both print and exit 0 whether or not credentials
    /// exist. `agy help auth` answers `unknown subcommand: auth`.
    pub fn auth_status_args(&self) -> Option<&'static [&'static str]> {
        match self {
            HarnessKind::ClaudeCode => Some(&["auth", "status", "--json"]),
            HarnessKind::OpenCode => Some(&["auth", "list"]),
            HarnessKind::Agy => None,
        }
    }

    /// The harness's own sign-in command, after the program name.
    ///
    /// Jod runs this attached to the person's terminal and does nothing else:
    /// it does not read the credential, store it, or pass it on. The flow is
    /// the harness's, and so is the file it lands in.
    pub fn login_args(&self) -> Option<&'static [&'static str]> {
        match self {
            HarnessKind::ClaudeCode => Some(&["auth", "login"]),
            HarnessKind::OpenCode => Some(&["auth", "login"]),
            HarnessKind::Agy => None,
        }
    }

    /// Where this harness will look for the credentials it is about to use.
    ///
    /// This exists because of the bug that prompted the whole module, and it
    /// is the one line of output that explains it. Claude Code keeps its
    /// account in `$CLAUDE_CONFIG_DIR`, and a person who runs it through a
    /// shell alias that sets that variable has signed in to a directory Jod
    /// never sees — Jod spawns the binary with whatever environment it was
    /// started with, lands in the default `~/.claude`, and fails to
    /// authenticate against an account nobody logged in to.
    ///
    /// Naming the directory turns "OAuth session expired" into "you signed in
    /// somewhere else".
    pub fn profile_hint(&self) -> Option<String> {
        match self {
            HarnessKind::ClaudeCode => Some(match std::env::var("CLAUDE_CONFIG_DIR") {
                Ok(dir) if !dir.trim().is_empty() => format!("CLAUDE_CONFIG_DIR={dir}"),
                _ => "~/.claude (CLAUDE_CONFIG_DIR is not set)".to_string(),
            }),
            // Neither reads a per-profile directory from the environment, so
            // there is nothing here a person could be surprised by.
            HarnessKind::OpenCode | HarnessKind::Agy => None,
        }
    }

    /// Ask this harness whether it is signed in.
    ///
    /// Spawns a process, so it is not free — see the note on
    /// [`crate::service::Jod::harnesses`] about why this is not folded into
    /// the cheap availability listing.
    pub fn auth(&self) -> AuthState {
        let Some(bin) = self.locate() else {
            return AuthState::unknown(format!("{} is not installed", self.label()));
        };
        let Some(args) = self.auth_status_args() else {
            return AuthState::unknown(format!(
                "{} has no command that reports its sign-in state",
                self.label()
            ));
        };
        match run_bounded(&bin, args) {
            Ok(output) => self.read_auth_status(&output),
            Err(why) => AuthState::unknown(why),
        }
    }

    /// Turn the status command's output into a state.
    ///
    /// Split from [`auth`](HarnessKind::auth) so the parsing is testable
    /// against captured output without a harness installed.
    pub fn read_auth_status(&self, output: &str) -> AuthState {
        match self {
            HarnessKind::ClaudeCode => read_claude_status(output),
            HarnessKind::OpenCode => read_opencode_status(output),
            HarnessKind::Agy => AuthState::unknown("AGY does not report a sign-in state"),
        }
    }

    /// Lines this harness prints when authentication is what went wrong.
    ///
    /// One entry, because one is what has been observed. On 2026-08-21 a run
    /// through Claude Code printed `Failed to authenticate: OAuth session
    /// expired and could not be refreshed` and exited within a second, and the
    /// transcript carried the sentence with nothing to act on beside it.
    ///
    /// The list is meant to grow by measurement. Nothing is added here from
    /// memory of what a CLI "usually" says: a marker that matches the wrong
    /// failure sends a person to sign in over a problem sign-in cannot fix.
    fn auth_failure_markers(&self) -> &'static [&'static str] {
        match self {
            HarnessKind::ClaudeCode => &["Failed to authenticate"],
            HarnessKind::OpenCode | HarnessKind::Agy => &[],
        }
    }
}

/// Why a failed run failed, when the answer is "it was never signed in".
///
/// Called once, after a run has already failed. Two questions in order, cheap
/// one first:
///
/// 1. Did the harness say so? A measured marker in the run's own output is
///    proof, and it stays proof after the fact — a token that has since been
///    refreshed would make the second question answer "fine" about a run that
///    genuinely died of it.
/// 2. Failing that, is the harness signed out *now*? This costs one process
///    and is worth it only here, on a path that has already gone wrong. It
///    catches every wording nobody has captured yet, which is most of them.
///
/// `None` means authentication was not the problem, and the caller should say
/// nothing — an unrelated failure with a sign-in suggestion stapled to it is
/// worse than an unrelated failure.
pub fn advice_for_failure(kind: HarnessKind, output: &[String]) -> Option<String> {
    let said_so = output
        .iter()
        .any(|line| kind.auth_failure_markers().iter().any(|m| line.contains(m)));

    if !said_so && kind.auth().usable() {
        return None;
    }
    Some(advice(kind))
}

/// What to tell a person whose harness cannot authenticate.
pub fn advice(kind: HarnessKind) -> String {
    let mut message = format!(
        "{} could not authenticate, so nothing it was asked to do could run.",
        kind.label()
    );
    if let Some(profile) = kind.profile_hint() {
        message.push_str(&format!(" It is reading credentials from {profile}."));
    }
    match kind.login_args() {
        Some(_) => message.push_str(&format!(
            " Sign in with `jod login {}` and start this again.",
            kind.id().replace('_', "-")
        )),
        None => message.push_str(&format!(
            " {} has no sign-in command Jod can run for you — start it once by hand and sign in there.",
            kind.label()
        )),
    }
    message
}

/// Claude Code's `auth status --json`, captured 2026-08-21 from 2.1.231.
///
/// Signed in, the object carries `loggedIn`, `authMethod`, `email` and the
/// organisation; signed out it is three fields and no account. Both exit 0, so
/// the exit code says nothing and the body says everything.
fn read_claude_status(output: &str) -> AuthState {
    // The command prints JSON on stdout, but a Claude Code that distrusts the
    // working directory prints a warning line first, so the object is found
    // rather than assumed to start at byte zero.
    let Some(start) = output.find('{') else {
        return AuthState::unknown("Claude Code answered with something that was not JSON");
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&output[start..]) else {
        return AuthState::unknown("Claude Code's sign-in status could not be read");
    };
    match value.get("loggedIn").and_then(serde_json::Value::as_bool) {
        Some(true) => AuthState::LoggedIn {
            account: value
                .get("email")
                .or_else(|| value.get("authMethod"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        Some(false) => AuthState::LoggedOut,
        None => AuthState::unknown("Claude Code's sign-in status could not be read"),
    }
}

/// OpenCode's `auth list`, captured 2026-08-21.
///
/// It draws a box rather than printing a record, and the one durable fact in
/// it is the count on the last line: `0 credentials` or `1 credentials`. The
/// provider rows are read for the display only, so a change in how they are
/// drawn costs a name in the output and not the answer.
fn read_opencode_status(output: &str) -> AuthState {
    let plain = strip_ansi(output);
    let count = plain.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words
            .windows(2)
            .find(|pair| pair[1] == "credentials" || pair[1] == "credential")
            .and_then(|pair| pair[0].parse::<usize>().ok())
    });

    match count {
        Some(0) => AuthState::LoggedOut,
        Some(_) => {
            let providers: Vec<String> = plain
                .lines()
                .filter_map(|line| line.split_once('●'))
                .filter_map(|(_, rest)| rest.split_whitespace().next())
                .map(str::to_string)
                .collect();
            AuthState::LoggedIn {
                account: (!providers.is_empty()).then(|| providers.join(", ")),
            }
        }
        None => AuthState::unknown("OpenCode's credential list could not be read"),
    }
}

/// Drop ANSI colour so a drawn box can be read as text.
///
/// Small and local on purpose: this is the only place in the workspace that
/// has to read a harness's decorated output, and a dependency for it would be
/// a dependency for one function.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI sequences end at the first byte in `@`..`~`; anything else after
        // the escape is a two-character sequence we drop whole.
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// Run a command, with a deadline and no way to reach this process's stdin.
///
/// Both properties matter and neither is the default. `stdin` is closed
/// because a harness that does not recognise the subcommand may start an
/// interactive session, and one that inherits a terminal would sit there
/// holding it. The deadline is the backstop for the same failure when closing
/// stdin is not enough to make it exit.
fn run_bounded(program: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not answer within {} seconds",
                    program.display(),
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("could not wait for {}: {e}", program.display())),
        }
    }

    // Read after the exit rather than with `output()`, which would wait on the
    // pipes for ever and defeat the deadline above.
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut text);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `claude auth status --json` on 2.1.231 with a signed-in
    /// configuration directory.
    const CLAUDE_IN: &str = r#"{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "apiProvider": "firstParty",
  "email": "person@example.com",
  "orgId": "808e3507-7eab-4160-a4d0-ccf0d5d8ff37",
  "orgName": "person@example.com's Organization",
  "subscriptionType": "max"
}"#;

    /// The same command with `CLAUDE_CONFIG_DIR` pointed at a directory nobody
    /// has signed in to — which is exactly the state that produced the failed
    /// run this module was written for.
    const CLAUDE_OUT: &str = r#"{
  "loggedIn": false,
  "authMethod": "none",
  "apiProvider": "firstParty"
}"#;

    /// `opencode auth list`, one credential. The box drawing and the colour
    /// are part of what has to be parsed, so they are kept verbatim.
    const OPENCODE_IN: &str = "\u{1b}[0m\n\u{1b}[90m┌\u{1b}[39m  Credentials \u{1b}[90m~/.local/share/opencode/auth.json\n\u{1b}[90m│\u{1b}[39m\n\u{1b}[34m●\u{1b}[39m  OpenRouter \u{1b}[90mapi\n\u{1b}[90m│\u{1b}[39m\n\u{1b}[90m└\u{1b}[39m  1 credentials\n";

    /// The same command against an empty data directory.
    const OPENCODE_OUT: &str = "\u{1b}[0m\n\u{1b}[90m┌\u{1b}[39m  Credentials \u{1b}[90m~/opencode/auth.json\n\u{1b}[90m│\u{1b}[39m\n\u{1b}[90m└\u{1b}[39m  0 credentials\n";

    #[test]
    fn a_signed_in_claude_names_the_account_it_is_signed_in_as() {
        assert_eq!(
            HarnessKind::ClaudeCode.read_auth_status(CLAUDE_IN),
            AuthState::LoggedIn {
                account: Some("person@example.com".to_string())
            }
        );
    }

    /// The state Jod used to call "usable". A binary on disk and no account.
    #[test]
    fn a_signed_out_claude_is_not_usable() {
        let state = HarnessKind::ClaudeCode.read_auth_status(CLAUDE_OUT);
        assert_eq!(state, AuthState::LoggedOut);
        assert!(!state.usable(), "a signed-out harness cannot run anything");
    }

    /// Claude Code prints a trust warning before its JSON in any directory it
    /// has not been run in interactively, and that directory is the common
    /// case for a harness Jod spawns.
    #[test]
    fn a_warning_printed_before_the_json_does_not_hide_the_answer() {
        let noisy = format!("Ignoring 3 permissions.allow entries from .claude/settings.json: this workspace has not been trusted.\n{CLAUDE_IN}");
        assert!(matches!(
            HarnessKind::ClaudeCode.read_auth_status(&noisy),
            AuthState::LoggedIn { .. }
        ));
    }

    /// Output nobody can read is `Unknown`, never `LoggedOut`. Guessing here
    /// would put a sign-in prompt in front of a person whose account is fine.
    #[test]
    fn output_that_cannot_be_read_is_unknown_rather_than_signed_out() {
        for text in ["", "command not found", "{\"other\": 1}"] {
            let state = HarnessKind::ClaudeCode.read_auth_status(text);
            assert!(matches!(state, AuthState::Unknown { .. }), "{text:?}");
            assert!(state.usable(), "{text:?} must not block a run");
        }
    }

    #[test]
    fn opencode_reads_its_credential_count_through_the_box_drawing() {
        assert_eq!(
            HarnessKind::OpenCode.read_auth_status(OPENCODE_IN),
            AuthState::LoggedIn {
                account: Some("OpenRouter".to_string())
            }
        );
        assert_eq!(
            HarnessKind::OpenCode.read_auth_status(OPENCODE_OUT),
            AuthState::LoggedOut
        );
    }

    /// AGY has no `auth` subcommand — `agy help auth` answers `unknown
    /// subcommand: auth`. The honest report is that nothing was asked.
    #[test]
    fn agy_reports_that_it_cannot_be_asked() {
        assert_eq!(HarnessKind::Agy.auth_status_args(), None);
        assert_eq!(HarnessKind::Agy.login_args(), None);
        let state = HarnessKind::Agy.read_auth_status("");
        assert!(matches!(state, AuthState::Unknown { .. }));
        assert!(state.usable(), "a harness Jod cannot ask is not blocked");
    }

    /// The line the failed run actually printed, on 2026-08-21.
    #[test]
    fn the_observed_claude_failure_is_recognised_by_what_it_printed() {
        let output = vec![
            "Failed to authenticate: OAuth session expired and could not be refreshed".to_string(),
        ];
        let advice = advice_for_failure(HarnessKind::ClaudeCode, &output)
            .expect("an authentication failure should be recognised from the run's own output");
        assert!(advice.contains("jod login claude-code"), "{advice}");
    }

    /// A failure that is not about credentials gets no sign-in suggestion, and
    /// this must hold without asking the harness anything — so it is asserted
    /// against a harness that cannot be asked at all.
    #[test]
    fn an_unrelated_failure_gets_no_sign_in_suggestion() {
        let output = vec!["error: no such file or directory".to_string()];
        assert_eq!(advice_for_failure(HarnessKind::Agy, &output), None);
    }

    /// A harness with no sign-in command must not be told to run one.
    #[test]
    fn advice_for_a_harness_with_no_login_command_says_so() {
        let advice = advice(HarnessKind::Agy);
        assert!(!advice.contains("jod login"), "{advice}");
        assert!(advice.contains("by hand"), "{advice}");
    }

    /// The bug in one assertion: the directory Claude Code will read is named,
    /// so signing in somewhere else is visible rather than mysterious.
    #[test]
    fn claude_advice_names_the_configuration_directory_it_will_read() {
        let advice = advice(HarnessKind::ClaudeCode);
        assert!(advice.contains("CLAUDE_CONFIG_DIR") || advice.contains("~/.claude"), "{advice}");
    }

    #[test]
    fn ansi_escapes_are_removed_without_taking_the_text_with_them() {
        assert_eq!(strip_ansi("\u{1b}[90m└\u{1b}[39m  1 credentials"), "└  1 credentials");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
