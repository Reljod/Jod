//! The harness seam.
//!
//! Jod never talks to a model. It talks to a *harness* — an agent CLI that owns
//! its own context, tools and permissions. Adding a harness means implementing
//! this trait and nothing else.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::{AgentEvent, Usage};

pub mod agy;
pub mod auth;
pub mod claude;
pub mod grants;
pub mod models;
pub mod opencode;

pub use agy::Agy;
pub use auth::AuthState;
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

/// One layer of the chain of command, and the key of its row in `roles`.
///
/// Six, because six is what the chain has: main hands over to an assistant, the
/// assistant hands to a manager or to a scratch session, a manager opens
/// engineers, and housekeeping hangs off the side because nothing delegates to
/// it. The spellings here are the primary keys in the `roles` table
/// ([`crate::store::RoleRow`]), so they are written down once and read back
/// with [`Role::parse`].
///
/// This is a *tag on a spawn*, not a property of it. It says which row to
/// consult for the harness, model, effort and permission the caller did not
/// name itself; [`crate::service::apply_role`] is the only thing that reads it,
/// and by the time the request reaches a harness the tag has already been spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Main,
    Assistant,
    Manager,
    Engineer,
    Scratch,
    Housekeeping,
}

impl Role {
    /// Every layer, in the order the chain of command lists them. Not a tree —
    /// which role sits under which is a shape the roles panel draws, and this
    /// array deliberately does not encode it.
    pub const ALL: [Role; 6] = [
        Role::Main,
        Role::Assistant,
        Role::Manager,
        Role::Engineer,
        Role::Scratch,
        Role::Housekeeping,
    ];

    /// The stored spelling — the `roles.role` primary key.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Assistant => "assistant",
            Role::Manager => "manager",
            Role::Engineer => "engineer",
            Role::Scratch => "scratch",
            Role::Housekeeping => "housekeeping",
        }
    }

    /// The inverse of [`Role::as_str`], for reading a role back out of the
    /// table. Unknown text yields `None` rather than a guess: a row naming a
    /// layer this build does not have is a row to leave alone, not one to map
    /// onto whichever role looks closest.
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// How hard the model should think, when somebody has said.
///
/// A level rather than a token budget, because a level is what the harnesses
/// actually take. Claude Code and AGY both spell it `--effort <level>` and both
/// accept the same first three words; OpenCode's `--variant <string>` sits in
/// the same slot. All three were read off the installed binaries — Claude Code
/// 2.1.220 and AGY 1.1.18 — rather than assumed, and no environment variable is
/// involved anywhere.
///
/// There is no `None` variant, on purpose. "Do not set this" is `Option::None`
/// on [`SpawnRequest::effort`], which passes no flag at all and leaves the
/// harness on its own default. That is what keeps an empty `roles` table
/// producing exactly the argv it produced before any of this existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    /// Claude Code only — see [`Effort::flag_value`].
    #[serde(rename = "xhigh")]
    XHigh,
    /// Claude Code only — see [`Effort::flag_value`].
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The stored spelling, which is also the word the flag takes.
    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// The inverse of [`Effort::as_str`], for reading `roles.thinking` back.
    pub fn parse(s: &str) -> Option<Effort> {
        Effort::ALL.into_iter().find(|e| e.as_str() == s)
    }

    /// What to pass to this harness's effort flag, or `None` when the harness
    /// has no spelling for the level asked for.
    ///
    /// Per harness, because the three do not agree on the range:
    ///
    /// - **Claude Code** takes all five. `claude --help` on 2.1.220 spells it
    ///   `--effort <level>` and names `low, medium, high, xhigh, max`.
    /// - **AGY** takes the first three, `low|medium|high`, which
    ///   `docs/harness-config.md` already documented. `xhigh` and `max` have no
    ///   spelling there and come back `None`, so the adapter passes nothing
    ///   rather than the nearest level AGY does accept. Rounding `max` down to
    ///   `high` would be a setting that quietly did something other than what
    ///   it says, and the person who set it would have no way to find out. The
    ///   refusal is *reported* instead, at the one place that knows which role
    ///   asked for it — [`crate::service::apply_role`].
    /// - **OpenCode** has nothing to check against. `--variant` is handed
    ///   through to whichever provider the model comes from, so the valid words
    ///   are that provider's rather than OpenCode's — its own help names
    ///   `high, max, minimal`. Every level therefore goes through verbatim, and
    ///   Jod does not pretend to know which ones the provider will take.
    pub fn flag_value(&self, harness: HarnessKind) -> Option<&'static str> {
        match harness {
            HarnessKind::ClaudeCode | HarnessKind::OpenCode => Some(self.as_str()),
            HarnessKind::Agy => {
                matches!(self, Effort::Low | Effort::Medium | Effort::High).then_some(self.as_str())
            }
        }
    }

    /// Whether this level reaches that harness at all. The question a caller
    /// asks *before* setting it, so it can say why it did not.
    pub fn accepted_by(&self, harness: HarnessKind) -> bool {
        self.flag_value(harness).is_some()
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
    /// How hard to think, when a caller or a role has said.
    ///
    /// `None` is not "think as little as possible" — it means nobody said, so
    /// no flag is emitted and the harness uses its own default. Every adapter
    /// treats it that way, which is what lets this field exist without changing
    /// a single spawn on a machine that never configures a role.
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub permission: PermissionPolicy,
    /// Which layer of the chain of command this spawn is, if the caller knows.
    ///
    /// Set it and the role's row in `roles` fills in the harness, model, effort
    /// and permission that the caller and the conversation both left unsaid.
    /// Leave it `None` — the default, and what every existing call site does —
    /// and no row is read and nothing changes.
    ///
    /// **A caller that names its own harness should leave this unset.**
    /// [`harness`](Self::harness) is a `HarnessKind` rather than an `Option`,
    /// so "the caller asked for Claude Code" and "the caller asked for nothing
    /// and got the fallback" arrive here as the same value, and the roles table
    /// sits exactly between those two cases: below a harness somebody named,
    /// above the fallback. Since the request cannot tell them apart, the caller
    /// does — `role: args.harness.is_none().then_some(Role::Scratch)` is the
    /// shape, and it keeps an explicit argument winning without adding a second
    /// field that every future call site would have to remember to set.
    #[serde(default)]
    pub role: Option<Role>,
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
    /// Directories this run may read, beyond [`cwd`](Self::cwd).
    ///
    /// A conversation can be pointed at several repositories at once, and the
    /// one the process happens to start in is not the only one it may look at.
    /// Each harness spells this differently and one of them cannot spell it at
    /// all, so [`Harness::args`] translates and the gap is documented rather
    /// than pretended away — a directory a harness will not accept is a
    /// directory Jod must not claim to have granted.
    ///
    /// Not a sandbox. Passing a root grants reading; withholding one does not
    /// prevent it.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    /// Ordinary, non-secret environment variables for the harness process.
    ///
    /// Safe to read, log and write into the run's `spawn.json`, because
    /// nothing confidential is permitted here. Credentials go in
    /// [`secrets`](Self::secrets), which is a different field for exactly that
    /// reason.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// A repository command to invoke, named rather than pasted.
    ///
    /// Only OpenCode needs this, and it needs it because of a measured quirk:
    /// alone of the three, it does *not* expand `/name` written into the
    /// message. Given one it hands the literal text to the model, which — in
    /// the run that found this — went looking with `ls` and `cat`, happened to
    /// find the file in the working directory, and answered correctly. Right
    /// answer, wrong mechanism, and it would have failed the moment the command
    /// lived anywhere else. `opencode run --command <name>` resolves it
    /// properly, in one step.
    ///
    /// Claude Code and AGY leave this `None` and put `/name` in the prompt,
    /// which they expand themselves. So this is not a general "run a command"
    /// verb — it is one harness's spelling of a thing the other two say in the
    /// prompt, and [`crate::commands::Discovered::invoke`] is what decides
    /// which spelling a given command gets.
    ///
    /// **With a command set, `prompt` is the command's *arguments*, not a
    /// message.** Measured: `--command jodargs "hello world"` reached the
    /// command as `$ARGUMENTS` = `["hello world"]`. That matters beside
    /// [`system`](Self::system) — a harness that answers `false` to
    /// [`Harness::takes_system_prompt`] has its framing prepended to the
    /// prompt by the runner, and under a command that framing would arrive as
    /// argument text. Setting both on one OpenCode spawn is therefore a caller
    /// error rather than a supported combination, and it is written down here
    /// because nothing at the type level stops it.
    #[serde(default)]
    pub command: Option<String>,
    /// Names of secrets to inject, and *only* the names.
    ///
    /// This is how a credential reaches an agent's tools without reaching the
    /// agent's context. The value is never here, never in `spawn.json`, and
    /// never in this process: the supervisor looks each name up in the
    /// owner-only secret file at exec time, puts it in the child's
    /// environment, and uses the same values to scrub the child's output
    /// before anything is parsed or stored.
    ///
    /// Carrying names rather than values is the whole safety property. A plan
    /// is written to disk so a person can read it afterwards; a value in it
    /// would be a second copy of the credential at ordinary permissions, which
    /// is the leak the design exists to prevent. The model, meanwhile, is told
    /// the name — enough to use the variable, never enough to print it.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The run's id — filled in by the launcher, never by the caller.
    ///
    /// Every other field on this struct is a *request*: something the caller
    /// asked for. This one is an answer, and it is here for one reason —
    /// [`Harness::args`] needs it to hand the harness a per-run MCP config, and
    /// `args` is given nothing but this struct.
    ///
    /// It carries the run's identity to Jod's own MCP server, which is how the
    /// messaging tools know which member is calling. A caller that set this
    /// itself would be naming a run it does not own, so [`crate::runner::launch`]
    /// overwrites whatever is here.
    #[serde(default)]
    pub run_id: Option<String>,
}

impl Default for SpawnRequest {
    /// Exists so a caller that cares about three fields does not have to name
    /// nine. Every field added here after the fact is one that would otherwise
    /// have to be threaded through every construction site in the workspace.
    fn default() -> Self {
        SpawnRequest {
            name: String::new(),
            harness: HarnessKind::ClaudeCode,
            prompt: String::new(),
            system: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            model: None,
            effort: None,
            permission: PermissionPolicy::default(),
            role: None,
            resume: Resume::default(),
            tools: None,
            roots: Vec::new(),
            env: Vec::new(),
            command: None,
            secrets: Vec::new(),
            run_id: None,
        }
    }
}

/// A short, human-recognisable name from the prompt's first words.
///
/// Fills [`SpawnRequest::name`] when the caller does not pass one, which every
/// entry point has to do: `jod run` without `--name`, `POST /v1/agents` without
/// `"name"`, the TUI's delegate, and the `delegate` MCP tool.
///
/// It lives here because it was written four times — once in `cli/src/main.rs`,
/// once in `api/src/routes.rs`, once privately in `core/src/mcp.rs`, and once
/// more in `cli/examples/screens.rs` — each with a comment promising it matched
/// the others. It did, but only because nobody had touched it yet. The activity
/// feed made the same promise across two files and had already broken it. An
/// agent should be called the same thing whether it was started from a terminal,
/// a phone or another agent, and that is a property worth having the compiler
/// keep rather than a comment.
///
/// Five words, because a name is a label in a list and not a summary. The 48
/// character bound is on *characters* rather than bytes: a prompt is whatever
/// the user typed, and slicing bytes through a multi-byte character panics.
pub fn default_name(prompt: &str) -> String {
    let name = prompt
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ");

    if name.is_empty() {
        // A run with no name is a row nobody can point at. "agent" is a poor
        // name and an honest one; an empty string is neither.
        "agent".to_string()
    } else if name.chars().count() > 48 {
        format!("{}…", name.chars().take(47).collect::<String>())
    } else {
        name
    }
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

    #[test]
    fn a_name_is_derived_from_the_first_words_of_the_prompt() {
        assert_eq!(
            default_name("summarise the inbox please now ok"),
            "summarise the inbox please now"
        );
    }

    #[test]
    fn an_empty_prompt_still_yields_a_usable_name() {
        assert_eq!(default_name("   "), "agent");
        assert_eq!(default_name(""), "agent");
    }

    #[test]
    fn a_long_name_is_truncated_rather_than_left_unbounded() {
        let name = default_name(&"averyverylongword ".repeat(5));
        assert!(
            name.chars().count() <= 48,
            "got {} chars: {name}",
            name.chars().count()
        );
        assert!(name.ends_with('…'), "a truncated name should say so: {name}");
    }

    /// The bound is on characters, not bytes. Slicing a prompt mid-character
    /// panics, and a prompt is whatever the user typed — which for this repo's
    /// own users includes plenty that is not ASCII.
    #[test]
    fn a_long_multibyte_prompt_is_truncated_without_panicking() {
        let name = default_name(&"日本語のとても長い単語 ".repeat(5));
        assert!(name.chars().count() <= 48, "got {name}");
    }

    /// Whitespace is a separator, not content: a prompt that arrives with
    /// newlines and runs of spaces must not turn them into part of the name.
    #[test]
    fn runs_of_whitespace_collapse_to_single_spaces() {
        assert_eq!(default_name("summarise\n\n  the   inbox"), "summarise the inbox");
    }

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

    /// A role is written into `roles.role` and read back out of it, so the
    /// spelling has to survive the trip. A round trip that failed would leave a
    /// configured layer silently inheriting.
    #[test]
    fn every_role_round_trips_through_its_stored_spelling() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()), Some(role), "{role:?}");
        }
        assert_eq!(Role::ALL.len(), 6, "the chain of command has six layers");
        assert_eq!(Role::parse("ceo"), None, "an unknown layer is not guessed at");
    }

    /// The same for effort, and with the JSON spelling checked beside the
    /// stored one: the launcher writes a `SpawnRequest` into `spawn.json` and
    /// the supervisor reads it back, so a level whose serde name disagreed with
    /// `as_str` would mean one thing in the database and another on the wire.
    #[test]
    fn every_effort_level_round_trips_through_its_stored_spelling() {
        for level in Effort::ALL {
            assert_eq!(Effort::parse(level.as_str()), Some(level), "{level:?}");
            assert_eq!(
                serde_json::to_string(&level).unwrap(),
                format!("\"{}\"", level.as_str()),
                "{level:?} is spelled differently in JSON"
            );
        }
        assert_eq!(Effort::parse("none"), None, "there is no `none` level");
        assert_eq!(Effort::parse("xhigh"), Some(Effort::XHigh));
    }

    /// The two levels only Claude Code has. AGY's `--effort` takes three words
    /// and nothing else, so asking it for `max` must produce no flag rather
    /// than the nearest one it does know — a role that quietly ran at `high`
    /// when it was set to `max` would be a setting nobody could check.
    #[test]
    fn a_level_a_harness_has_no_word_for_is_never_passed_to_it() {
        for level in [Effort::Low, Effort::Medium, Effort::High] {
            assert_eq!(level.flag_value(HarnessKind::Agy), Some(level.as_str()));
        }
        for level in [Effort::XHigh, Effort::Max] {
            assert_eq!(level.flag_value(HarnessKind::Agy), None, "{level:?}");
            assert!(!level.accepted_by(HarnessKind::Agy));
            assert!(level.accepted_by(HarnessKind::ClaudeCode), "{level:?}");
        }
    }

    /// OpenCode's `--variant` reaches the provider, not OpenCode, so there is
    /// no list here to check a value against. Everything goes through verbatim.
    #[test]
    fn opencode_takes_every_level_verbatim_because_the_provider_decides() {
        for level in Effort::ALL {
            assert_eq!(
                level.flag_value(HarnessKind::OpenCode),
                Some(level.as_str()),
                "{level:?}"
            );
        }
    }

    /// The promise the roles table is built on: a request nobody has configured
    /// carries no role, no effort and nothing in its environment, so it is the
    /// request this code produced before any of that existed.
    #[test]
    fn a_request_nobody_has_configured_is_the_request_it_always_was() {
        let req = SpawnRequest::default();
        assert_eq!(req.role, None);
        assert_eq!(req.effort, None);
        assert!(req.env.is_empty());
    }

    /// SPEC check 29b. The effort level is a flag on the command line and must
    /// never have been routed through the environment instead — so no spawn
    /// anywhere in the workspace may put anything in `SpawnRequest::env`.
    ///
    /// Asserted against the source because the field is public and the next
    /// person to want a per-role setting will look for the easiest place to put
    /// it. `env` reaches the child process for real (`runner.rs` →
    /// `supervisor`), so it would work, and it would be invisible.
    #[test]
    fn no_spawn_in_the_workspace_puts_anything_in_the_environment() {
        fn rs_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits inside the workspace")
            .to_path_buf();
        let mut files = Vec::new();
        for crate_dir in ["core", "cli", "api", "supervisor"] {
            rs_files(&root.join(crate_dir).join("src"), &mut files);
        }
        assert!(files.len() > 10, "found almost no source to read");

        // The field's name is assembled a character at a time so this test does
        // not find itself, the way `rank.rs` does with the picker binaries.
        let field: String = ['e', 'n', 'v'].iter().collect();
        // The three shapes that put something in it: a struct literal with a
        // non-empty list, an assignment, and a push. An empty `Vec::new()` is
        // the state being defended, so it is not one of them.
        let literal = format!("{field}: vec![");
        let assigned = format!(".{field} = ");
        let pushed = format!(".{field}.push(");

        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                let fills = (line.starts_with(&literal) && !line.starts_with(&format!("{literal}]")))
                    || line.contains(&assigned)
                    || line.contains(&pushed);
                assert!(
                    !fills,
                    "{}:{} fills a spawn's environment: {line}",
                    file.display(),
                    n + 1
                );
            }
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
