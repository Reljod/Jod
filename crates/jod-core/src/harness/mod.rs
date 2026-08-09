//! The harness seam.
//!
//! Jod never talks to a model. It talks to a *harness* — an agent CLI that owns
//! its own context, tools and permissions. Adding a harness means implementing
//! this trait and nothing else.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::{AgentEvent, Usage};

pub mod antigravity;
pub mod claude;
pub mod opencode;

pub use antigravity::Antigravity;
pub use claude::ClaudeCode;
pub use opencode::OpenCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    ClaudeCode,
    OpenCode,
    Antigravity,
}

impl HarnessKind {
    pub const ALL: [HarnessKind; 3] = [
        HarnessKind::ClaudeCode,
        HarnessKind::OpenCode,
        HarnessKind::Antigravity,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            HarnessKind::ClaudeCode => "claude_code",
            HarnessKind::OpenCode => "open_code",
            HarnessKind::Antigravity => "antigravity",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            HarnessKind::ClaudeCode => "Claude Code",
            HarnessKind::OpenCode => "OpenCode",
            HarnessKind::Antigravity => "Antigravity",
        }
    }

    /// Where this harness's binary lives, if it is installed.
    pub fn locate(&self) -> Option<PathBuf> {
        match self {
            HarnessKind::ClaudeCode => crate::discovery::find_binary(
                "JOD_CLAUDE_BIN",
                &["claude"],
                &[
                    "~/.nvm/versions/node/*/bin/claude",
                    "~/.claude/local/claude",
                    "/opt/homebrew/bin/claude",
                    "/usr/local/bin/claude",
                    "~/.bun/bin/claude",
                ],
            ),
            HarnessKind::OpenCode => crate::discovery::find_binary(
                "JOD_OPENCODE_BIN",
                &["opencode"],
                &[
                    "~/.opencode/bin/opencode",
                    "/opt/homebrew/bin/opencode",
                    "/usr/local/bin/opencode",
                    "~/.bun/bin/opencode",
                ],
            ),
            HarnessKind::Antigravity => crate::discovery::find_binary(
                "JOD_AGY_BIN",
                &["agy"],
                &[
                    "~/.local/bin/agy",
                    "~/.antigravity/bin/agy",
                    "/opt/homebrew/bin/agy",
                    "/usr/local/bin/agy",
                ],
            ),
        }
    }

    pub fn build(&self) -> Box<dyn Harness> {
        match self {
            HarnessKind::ClaudeCode => Box::new(ClaudeCode::default()),
            HarnessKind::OpenCode => Box::new(OpenCode::default()),
            HarnessKind::Antigravity => Box::new(Antigravity::default()),
        }
    }
}

/// How much the agent may do without asking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Tool calls that need approval are refused — safe for read-only prompts.
    #[default]
    Ask,
    /// File edits go through; other sensitive calls still prompt.
    AcceptEdits,
    /// Everything is auto-approved. Only sane inside a throwaway worktree.
    Bypass,
}

/// What the caller asked for. Harness-neutral on purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub name: String,
    pub harness: HarnessKind,
    pub prompt: String,
    /// Working directory for the agent. Defaults to the user's home.
    pub cwd: PathBuf,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission: PermissionPolicy,
    /// Continue an existing harness-side conversation instead of starting one.
    ///
    /// This is what makes a *conversation* out of one-shot headless runs: every
    /// harness can resume by id (`--resume` / `--session` / `--conversation`),
    /// so a follow-up turn is another spawn carrying the id the last one
    /// reported in `Started`. No pseudo-terminal, no long-lived child process.
    #[serde(default)]
    pub resume: Option<String>,
}

/// One argv entry. `Prompt` is a placeholder the runner substitutes with a
/// shell variable, so a prompt containing quotes or `$(...)` can never be
/// re-interpreted by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgPart {
    Literal(String),
    Prompt,
}

impl ArgPart {
    pub fn lit(s: impl Into<String>) -> Self {
        ArgPart::Literal(s.into())
    }
}

/// Running tally kept while a harness streams, so `finalize` can report the
/// final answer and cost even for harnesses that never emit a "done" record.
#[derive(Debug, Default)]
pub struct Accumulator {
    pub last_text: Option<String>,
    pub usage: Usage,
    pub errored: bool,
}

impl Accumulator {
    pub fn note_text(&mut self, text: &str) {
        if !text.trim().is_empty() {
            self.last_text = Some(text.to_string());
        }
    }

    /// Costs and cache counters accumulate across steps; context-window
    /// counters (input tokens) are reported per step, so we take the max rather
    /// than a sum that would double-count a re-sent prompt.
    pub fn add_usage(&mut self, other: &Usage) {
        fn sum(slot: &mut Option<u64>, add: Option<u64>) {
            if let Some(v) = add {
                *slot = Some(slot.unwrap_or(0) + v);
            }
        }
        fn max(slot: &mut Option<u64>, add: Option<u64>) {
            if let Some(v) = add {
                *slot = Some(slot.unwrap_or(0).max(v));
            }
        }
        max(&mut self.usage.input_tokens, other.input_tokens);
        sum(&mut self.usage.output_tokens, other.output_tokens);
        max(&mut self.usage.cache_read_tokens, other.cache_read_tokens);
        sum(&mut self.usage.cache_write_tokens, other.cache_write_tokens);
        // Reasoning tokens are generated per step, like output tokens.
        sum(&mut self.usage.thinking_tokens, other.thinking_tokens);
        if let Some(c) = other.cost_usd {
            self.usage.cost_usd = Some(self.usage.cost_usd.unwrap_or(0.0) + c);
        }
    }

    pub fn finish(&self, exit_code: Option<i32>) -> AgentEvent {
        let bad_exit = exit_code.is_some_and(|c| c != 0);
        AgentEvent::Finished {
            text: self.last_text.clone(),
            exit_code,
            is_error: self.errored || bad_exit,
            usage: self.usage.clone(),
        }
    }
}

/// A harness adapter: builds the command line, then turns that command's JSONL
/// back into `AgentEvent`s.
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;

    /// argv after the program name.
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;

    /// Translate one line of harness output. May yield zero events (noise) or
    /// several (one assistant message can carry thinking + text + a tool call).
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;

    /// Called once, when the process has exited. The runner owns "the run is
    /// over" so that harnesses without a terminal record still finish cleanly.
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_tokens_take_the_max_and_output_tokens_sum() {
        let mut acc = Accumulator::default();
        acc.add_usage(&Usage {
            input_tokens: Some(100),
            output_tokens: Some(10),
            cost_usd: Some(0.5),
            ..Default::default()
        });
        acc.add_usage(&Usage {
            input_tokens: Some(80),
            output_tokens: Some(5),
            cost_usd: Some(0.25),
            ..Default::default()
        });
        assert_eq!(acc.usage.input_tokens, Some(100));
        assert_eq!(acc.usage.output_tokens, Some(15));
        assert_eq!(acc.usage.cost_usd, Some(0.75));
    }

    #[test]
    fn blank_text_never_overwrites_a_real_answer() {
        let mut acc = Accumulator::default();
        acc.note_text("the answer");
        acc.note_text("   ");
        assert_eq!(acc.last_text.as_deref(), Some("the answer"));
    }

    #[test]
    fn a_nonzero_exit_marks_the_run_as_errored() {
        let acc = Accumulator::default();
        match acc.finish(Some(1)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn every_kind_reports_a_stable_id() {
        for kind in HarnessKind::ALL {
            assert!(!kind.id().is_empty());
            assert!(!kind.label().is_empty());
        }
    }
}
