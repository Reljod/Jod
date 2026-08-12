//! Credentials an agent can use and cannot read.
//!
//! The model this converged on is the one GitHub Actions, Doppler, Infisical
//! and `op run` all landed on independently: **inject at exec, mask on output,
//! reference by name.** The agent is told a variable exists; the value reaches
//! its tools through the process environment and never through its context.
//!
//! Three rules, and every one of them is load-bearing:
//!
//! 1. **The value lives outside every repository**, in a file at owner-only
//!    permissions, verified on read. Not in the database — a value in SQLite
//!    is a value in every backup, every `jod conv show`, and every screen
//!    share.
//! 2. **The value is injected by the supervisor at spawn**, which is the only
//!    process that sees both the child's environment and its output. It is not
//!    in the prompt, the transcript, or `spawn.json`.
//! 3. **The value is scrubbed back out of the output** before anything is
//!    parsed or stored. Redaction is the belt to injection's braces: an agent
//!    that echoes the variable still cannot get the value into the record.
//!
//! ## What this is not
//!
//! Not a keychain, and not a permission system. A missing key blocks one test,
//! not a session — which is the point. The agent is told to treat an absent
//! credential as a *blocked* ending rather than a reason to invent one.

use serde::{Deserialize, Serialize};

/// Who a secret is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every session on the box.
    Global,
    /// One work. The default, so a key given for one project is not handed to
    /// every agent Reljod runs.
    Work,
    /// One conversation.
    Conversation,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Work
    }
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Work => "work",
            Scope::Conversation => "conversation",
        }
    }

    pub fn parse(s: &str) -> Scope {
        match s {
            "global" => Scope::Global,
            "conversation" => Scope::Conversation,
            _ => Scope::Work,
        }
    }
}

/// Everything about a secret that is safe to show anyone.
///
/// Note what is absent, permanently: the value. This type is what the rail,
/// the CLI, the MCP tools and the agent's preamble are allowed to see. If a
/// field is ever added here that could reconstruct a value, the design has
/// been broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub id: i64,
    /// The environment variable's name. Validated to be a legal one, because
    /// a name that cannot be exported is a secret that silently never arrives.
    pub name: String,
    pub scope: Scope,
    /// The work or conversation id; empty for global.
    pub scope_id: String,
    /// What it is for, in the owner's words. Shown to the agent so it knows
    /// which variable to reach for.
    pub hint: String,
    /// Length only — never content. The scrubber needs it to decide what is
    /// too short to redact safely, and asking for the length must not require
    /// reading the value.
    pub length: usize,
    /// Whether this value is long enough to redact without mangling ordinary
    /// output. A four-character secret would match half of everything, so it
    /// is injected and *not* redacted — and the rail says so when it is
    /// stored, because a silent exception here is a leak nobody was told about.
    pub redactable: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Below this, a value is injected but not redacted.
///
/// Short strings appear in ordinary output constantly; redacting them would
/// replace legitimate text with the marker and make transcripts unreadable,
/// which is its own kind of failure. The threshold is named here so the rail
/// and the scrubber cannot disagree about it.
pub const MIN_REDACTABLE_LEN: usize = 8;

/// Whether `name` is a legal environment variable name.
///
/// Deliberately strict — leading letter or underscore, then letters, digits
/// and underscores. A name outside this set may be silently dropped by a shell
/// or a harness somewhere down the line, and the failure would look like "the
/// secret did not work" rather than "the name was invalid".
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_a_shell_would_reject_is_refused_here_first() {
        assert!(is_valid_name("OPENAI_API_KEY"));
        assert!(is_valid_name("_private"));
        assert!(!is_valid_name("2FA_TOKEN"), "cannot start with a digit");
        assert!(!is_valid_name("MY-KEY"), "hyphens are not exportable");
        assert!(!is_valid_name(""), "the empty name is not a name");
        assert!(!is_valid_name("HAS SPACE"));
    }

    #[test]
    fn the_redaction_floor_is_shared_so_the_rail_and_the_scrubber_agree() {
        // Not a behaviour test so much as a tripwire: if this constant moves,
        // both the "stored, but too short to redact" warning and the scrubber
        // move with it, and neither can drift alone.
        assert!(MIN_REDACTABLE_LEN >= 8);
    }
}
