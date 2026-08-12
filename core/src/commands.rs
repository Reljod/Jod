//! Slash commands and skills a repository already defines.
//!
//! Reljod should not have to remember which harness knows about which command.
//! Jod scans every root and the user's own config, and offers what it finds in
//! its own palette, marked with where it came from.
//!
//! ## Forwarded, not reimplemented
//!
//! Jod sends the command line through to harnesses that expand it themselves,
//! and inlines the command's text for those that do not. Which harness does
//! which is **measured against the real binary before the code is written**,
//! not assumed — and if all three turn out to expand their own, the inlining
//! branch is deleted rather than kept just in case. That measurement is
//! deliberately scheduled early: it is an hour's work that decides whether a
//! whole branch of this module exists.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a command was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Under one of the conversation's roots — the repository's own.
    Root,
    /// The user's config directory. Available everywhere.
    User,
    /// Shipped by an installed plugin.
    Plugin,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Root => "root",
            Scope::User => "user",
            Scope::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Scope {
        match s {
            "user" => Scope::User,
            "plugin" => Scope::Plugin,
            _ => Scope::Root,
        }
    }
}

/// A command or a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Command,
    Skill,
}

/// One thing Jod's palette can offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovered {
    pub id: i64,
    /// The directory it was found under; empty for user-level config.
    pub root: PathBuf,
    pub scope: Scope,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Whose convention it follows; empty when every harness would find it.
    pub harness: String,
    /// The command's text, kept only for harnesses that cannot expand one
    /// themselves. Empty when nothing needs it.
    pub body: String,
    pub scanned_at_ms: i64,
}

/// What a harness does with `/name` in a prompt.
///
/// Measured, one value per harness, before anything depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expansion {
    /// The harness resolves the command itself. Jod forwards the line as
    /// typed and stays out of the way.
    Native,
    /// The harness treats it as ordinary text, so Jod substitutes the
    /// command's body before sending.
    Inline,
    /// Not yet measured. Nothing may branch on this value — it exists so that
    /// "we have not checked" is representable and cannot be mistaken for
    /// "it does not work".
    Unmeasured,
}
