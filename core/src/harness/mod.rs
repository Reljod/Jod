//! The harness seam.
//!
//! Jod never talks to a model. It talks to a *harness* — an agent CLI that owns
//! its own context, tools and permissions. Adding a harness means implementing
//! this trait and nothing else.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::{AgentEvent, Usage};

pub mod agy;
pub mod claude;
pub mod models;
pub mod opencode;

pub use agy::Agy;
pub use claude::ClaudeCode;
pub use models::Model;
pub use opencode::OpenCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    ClaudeCode,
    OpenCode,
    Agy,
}

impl HarnessKind {
    pub const ALL: [HarnessKind; 3] = [
        HarnessKind::ClaudeCode,
        HarnessKind::OpenCode,
        HarnessKind::Agy,
    ];

    /// The inverse of [`HarnessKind::id`], for reading a kind back out of
    /// storage. Unknown text yields `None` rather than a guess.
    pub fn from_id(id: &str) -> Option<HarnessKind> {
        HarnessKind::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn id(&self) -> &'static str {
        match self {
            HarnessKind::ClaudeCode => "claude_code",
            HarnessKind::OpenCode => "open_code",
            HarnessKind::Agy => "agy",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            HarnessKind::ClaudeCode => "Claude Code",
            HarnessKind::OpenCode => "OpenCode",
            HarnessKind::Agy => "AGY",
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
            HarnessKind::Agy => crate::discovery::find_binary(
                "JOD_AGY_BIN",
                &["agy"],
                &[
                    "~/.local/bin/agy",
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
            HarnessKind::Agy => Box::new(Agy::default()),
        }
    }
}

/// Whether this delegation starts a new conversation or continues one.
///
/// Every harness supports both, spelled differently: Claude Code takes
/// `--continue` / `--resume <id>`, OpenCode `--continue` / `--session <id>`,
/// AGY `--continue` / `--conversation <id>`. Normalising it here is what lets
/// Jod hold a conversation rather than fire one-shot tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resume {
    /// Start a new conversation.
    #[default]
    Fresh,
    /// Continue the most recent conversation in this working directory.
    Last,
    /// Continue one specific conversation by its harness-assigned id.
    Session(String),
}

/// How much the agent may do without asking.
///
/// Four levels rather than three, because `Ask` was carrying two jobs that
/// pull in opposite directions. It mapped to Claude Code's `plan`, so asking
/// to be *consulted* silently became asking the model to *change nothing* —
/// and since `plan` was also the default, every run Jod started was a planning
/// run. That is why the agents kept describing work instead of doing it.
///
/// Split apart: [`Plan`](PermissionPolicy::Plan) is "think, do not act",
/// [`Ask`](PermissionPolicy::Ask) is "act, but check with me first".
///
/// The order of the variants is the ordering [`crate::mcp::permits`] ranks
/// them by — how much can happen with nobody watching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Read and reason; change nothing. Reading is allowed — the filesystem
    /// and the web — and anything that could change something is refused.
    Plan,
    /// Every sensitive call is put to a person first.
    ///
    /// Honest about the headless case: under `-p` there is nobody to answer,
    /// so a harness in this mode denies rather than blocks. That is the right
    /// behaviour and a poor default, which is why it is not the default.
    Ask,
    /// File edits go through; other sensitive calls still prompt.
    AcceptEdits,
    /// Everything is auto-approved — "auto". The default, because Jod's whole
    /// premise is work that happens while nobody is watching, and a mode that
    /// stops to ask an empty room is a mode that never finishes.
    #[default]
    Bypass,
}

impl PermissionPolicy {
    /// Every mode, in the order the TUI cycles them.
    pub const ALL: [PermissionPolicy; 4] = [
        PermissionPolicy::Plan,
        PermissionPolicy::Ask,
        PermissionPolicy::AcceptEdits,
        PermissionPolicy::Bypass,
    ];

    /// The stored spelling. Matches [`crate::mcp::parse_permission`].
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionPolicy::Plan => "plan",
            PermissionPolicy::Ask => "ask",
            PermissionPolicy::AcceptEdits => "accept_edits",
            PermissionPolicy::Bypass => "bypass",
        }
    }

    /// What a person calls it. `auto` rather than `bypass` for the top level,
    /// because that is the word every harness puts on the same setting.
    pub fn label(&self) -> &'static str {
        match self {
            PermissionPolicy::Plan => "plan",
            PermissionPolicy::Ask => "ask",
            PermissionPolicy::AcceptEdits => "edits",
            PermissionPolicy::Bypass => "auto",
        }
    }

    /// The next mode when cycling forward, wrapping at the top.
    pub fn next(self) -> PermissionPolicy {
        let at = PermissionPolicy::ALL.iter().position(|m| *m == self);
        PermissionPolicy::ALL[(at.unwrap_or(0) + 1) % PermissionPolicy::ALL.len()]
    }

    /// Whether this level lets the agent change anything at all.
    ///
    /// The one question the screen needs answered to colour the indicator, and
    /// the one a caller needs before handing over an unattended run.
    pub fn may_act(&self) -> bool {
        !matches!(self, PermissionPolicy::Plan)
    }
}

/// What the caller asked for. Harness-neutral on purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub name: String,
    pub harness: HarnessKind,
    pub prompt: String,
    /// Standing framing for this run: who it is, what it may not do, how to
    /// decide. Separate from `prompt` because it is not something anybody said.
    ///
    /// The distinction earns its place in the transcript. Folded into the
    /// prompt, the orchestrator's framing became the opening *user* turn of the
    /// main chat — so `jod main` showed a screen of instructions-to-itself
    /// before the one sentence the user actually typed. A system prompt is
    /// addressed to the model and belongs nowhere in the conversation a person
    /// reads back.
    ///
    /// Not every harness has the concept; [`Harness::args`] implementations
    /// that lack one prepend it to the prompt, which is the behaviour this
    /// field replaced and is still correct — just less tidy.
    pub system: Option<String>,
    /// Working directory for the agent. Defaults to the user's home.
    pub cwd: PathBuf,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission: PermissionPolicy,
    /// Whether to continue an existing conversation instead of starting one.
    #[serde(default)]
    pub resume: Resume,
    /// Give this agent Jod's own tools, over MCP.
    ///
    /// This is the seam the whole system turns on, so it belongs on *every*
    /// spawn rather than on one special conversation. Jod has no model client
    /// and never will; what it has is effects — delegating, scheduling,
    /// remembering, listing what is running — and MCP is how a harness reaches
    /// them. The harness supplies the judgement, Jod supplies the verbs, and
    /// neither has to become the other.
    ///
    /// Because it lives here rather than in the orchestrator, the same seam is
    /// available to a scheduled run, a goal iteration, a webhook-triggered
    /// agent and a teammate. An agent that can see what else is running can
    /// hand work sideways instead of duplicating it, which is the whole of
    /// agent-to-agent as far as Jod needs to care.
    ///
    /// **The seam is universal; the level is not.** An earlier draft of this
    /// comment said every spawn got "the same tools as the main chat", which
    /// was wrong and would have been dangerous: the main chat is you, present,
    /// watching. A schedule at 2am is nobody watching, and the thing you least
    /// want unattended is an agent that can create more unattended agents.
    /// See [`ToolAccess::unattended`].
    ///
    /// `None` means a plain agent with no access to Jod — the right default for
    /// anything untrusted, and the reason this is opt-in rather than automatic.
    #[serde(default)]
    pub tools: Option<ToolAccess>,
}

/// What an agent may do to Jod itself.
///
/// A capability set rather than a boolean, because "can see what is running" and
/// "can start another agent" are different amounts of trust, and a webhook-
/// triggered run started by a stranger's pull request should get the first
/// without the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    /// Read Jod: what is running, what is scheduled, what it remembers.
    /// Cannot spawn, cannot schedule, cannot write memory.
    #[default]
    ReadOnly,
    /// Everything read-only allows, plus delegating to and stopping agents.
    Delegate,
    /// The full set, including creating schedules and goals and writing
    /// memory. What the main chat gets, and what nothing reached from outside
    /// should.
    Orchestrate,
}

impl ToolAccess {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolAccess::ReadOnly => "read_only",
            ToolAccess::Delegate => "delegate",
            ToolAccess::Orchestrate => "orchestrate",
        }
    }

    /// Whether this level may start or stop agents.
    pub fn may_delegate(&self) -> bool {
        matches!(self, ToolAccess::Delegate | ToolAccess::Orchestrate)
    }

    /// Whether this level may create schedules and goals, or write memory.
    ///
    /// The distinction that matters: delegating spends money now and is
    /// visible; scheduling spends it every night at 2am whether or not anyone
    /// is watching, and a goal spends it until something stops it.
    pub fn may_orchestrate(&self) -> bool {
        matches!(self, ToolAccess::Orchestrate)
    }

    /// What an agent nobody is watching gets.
    ///
    /// Read-only, and the reason is compounding rather than caution. A
    /// scheduled run that could schedule is a schedule that can multiply
    /// while you sleep; a goal that could set goals has no bound at all, and
    /// the stall detector counts iterations of *one* goal, so it would not
    /// even notice. The failure is not one expensive night — it is that
    /// nothing in the design says when it stops.
    ///
    /// Reading is the half that pays for itself anyway: the point of tools on
    /// an unattended run is that it can see what else is going on and decline
    /// to duplicate it, which needs `list_agents` and nothing more.
    ///
    /// Raising this for a specific schedule is a per-schedule setting worth
    /// having, and deliberately not a default worth inheriting.
    pub fn unattended() -> ToolAccess {
        ToolAccess::ReadOnly
    }

    /// Clamp to what material from outside may ever reach.
    ///
    /// A capability has to be bounded by the *least* trusted thing in the
    /// chain, not the most. Without this, raising a schedule's level would
    /// quietly create a path: a webhook rule names a high-privilege schedule, a
    /// stranger opens a pull request that matches the rule, and their text is
    /// now steering an agent that can create schedules. Every step is
    /// individually reasonable, which is what makes it the dangerous shape.
    ///
    /// So the cap is applied at the point of use rather than trusted to the
    /// row, and it is the same rule `webhook.rs` already applies to a payload:
    /// [`crate::store::Origin::Untrusted`] means read-only, whatever anything
    /// else says.
    pub fn capped_for(self, origin: crate::store::Origin) -> ToolAccess {
        match origin {
            crate::store::Origin::Untrusted => ToolAccess::ReadOnly,
            _ => self,
        }
    }
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

    /// Whether [`SpawnRequest::system`] reaches this harness as a real system
    /// prompt rather than as text glued to the front of the user's.
    ///
    /// Defaults to `false`, which is both the safe answer and the true one for
    /// most CLIs: a harness that never learned the flag would otherwise drop
    /// the framing on the floor, and framing that silently vanishes is worse
    /// than framing in the wrong place. The runner folds it into the prompt for
    /// everyone who answers `false`.
    fn takes_system_prompt(&self) -> bool {
        false
    }

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
    use crate::store::Origin;

    /// The bug this split exists to fix. `Ask` used to mean Claude Code's
    /// `plan`, so every run Jod started could only describe work — and since
    /// `Ask` was the default, that was every run.
    #[test]
    fn planning_and_asking_are_no_longer_the_same_mode() {
        assert_ne!(PermissionPolicy::Plan, PermissionPolicy::Ask);
        assert!(!PermissionPolicy::Plan.may_act(), "plan changes nothing");
        assert!(PermissionPolicy::Ask.may_act(), "ask acts, having checked");
    }

    /// What the user asked for in so many words: the default is auto.
    #[test]
    fn the_default_is_auto_because_nobody_is_watching() {
        assert_eq!(PermissionPolicy::default(), PermissionPolicy::Bypass);
        assert_eq!(PermissionPolicy::default().label(), "auto");
    }

    /// Cycling is what the Tab key does, so it must reach every mode and come
    /// back — a cycle that stops at the top is a mode you can leave and not
    /// return to.
    #[test]
    fn cycling_reaches_every_mode_and_wraps() {
        let mut seen = vec![PermissionPolicy::Plan];
        let mut at = PermissionPolicy::Plan;
        for _ in 0..PermissionPolicy::ALL.len() - 1 {
            at = at.next();
            assert!(!seen.contains(&at), "{at:?} came round twice");
            seen.push(at);
        }
        assert_eq!(seen.len(), PermissionPolicy::ALL.len());
        assert_eq!(at.next(), PermissionPolicy::Plan, "and wraps");
    }

    /// Every mode must survive being written down and read back, or a stored
    /// conversation would silently reopen in a different one.
    #[test]
    fn every_mode_round_trips_through_its_stored_spelling() {
        for mode in PermissionPolicy::ALL {
            assert_eq!(
                crate::mcp::parse_permission(mode.as_str()),
                Some(mode),
                "{mode:?} does not read back from {:?}",
                mode.as_str()
            );
            // And by the name a person types, which is the harnesses' own.
            assert_eq!(crate::mcp::parse_permission(mode.label()), Some(mode));
        }
    }

    /// The ordering the ceiling check depends on.
    #[test]
    fn a_ceiling_permits_everything_at_or_below_it() {
        use crate::mcp::permits;
        assert!(permits(PermissionPolicy::Bypass, PermissionPolicy::Plan));
        assert!(permits(PermissionPolicy::Ask, PermissionPolicy::Plan));
        assert!(!permits(PermissionPolicy::Plan, PermissionPolicy::Ask));
        assert!(!permits(PermissionPolicy::AcceptEdits, PermissionPolicy::Bypass));
        for mode in PermissionPolicy::ALL {
            assert!(permits(mode, mode), "{mode:?} must permit itself");
        }
    }

    /// The escalation this exists to close: a webhook rule names a
    /// high-privilege schedule, a stranger opens a pull request that matches
    /// it, and their text is steering an agent that can create schedules.
    /// Every step is individually reasonable, which is what makes it dangerous.
    #[test]
    fn untrusted_material_can_never_reach_more_than_reading() {
        for granted in [
            ToolAccess::ReadOnly,
            ToolAccess::Delegate,
            ToolAccess::Orchestrate,
        ] {
            let capped = granted.capped_for(Origin::Untrusted);
            assert_eq!(capped, ToolAccess::ReadOnly, "{granted:?} escaped the cap");
            assert!(!capped.may_delegate());
            assert!(!capped.may_orchestrate());
        }
    }

    /// The cap bounds outside material, not Jod's own work. Capping everything
    /// would make the level pointless.
    #[test]
    fn what_jod_itself_started_keeps_the_level_it_was_given() {
        for origin in [Origin::Owner, Origin::Agent, Origin::System] {
            assert_eq!(
                ToolAccess::Orchestrate.capped_for(origin),
                ToolAccess::Orchestrate,
                "{origin:?}"
            );
        }
    }

    /// Unattended work reads and does not act. A goal that could set goals is
    /// bounded by nothing — the stall detector counts iterations of one goal.
    #[test]
    fn an_unattended_run_may_look_but_not_spawn() {
        let level = ToolAccess::unattended();
        assert!(!level.may_delegate());
        assert!(!level.may_orchestrate());
    }

    #[test]
    fn each_level_grants_strictly_more_than_the_one_below() {
        assert!(!ToolAccess::ReadOnly.may_delegate());
        assert!(ToolAccess::Delegate.may_delegate());
        assert!(!ToolAccess::Delegate.may_orchestrate());
        assert!(ToolAccess::Orchestrate.may_delegate());
        assert!(ToolAccess::Orchestrate.may_orchestrate());
    }

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
