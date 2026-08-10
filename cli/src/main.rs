//! `jod` — the command line over the agent harnesses.
//!
//! Jod does not answer prompts. It hands them to a harness (Claude Code,
//! OpenCode, AGY), runs that harness inside its own tmux session, and turns the
//! harness's output into one event stream that every command here renders.

mod mcp_cmd;
mod render;
mod render_time;
mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use jod_core::consolidate::{Consolidation, Provenance};
use jod_core::conversation::PortableMessage;
use jod_core::service::RunConversation;
use jod_core::store::{NewFact, Origin, Store};
use jod_core::team::MemberStatus;
use jod_core::{
    AgentEnvelope, AgentEvent, HarnessKind, Jod, PermissionPolicy, Resume, SpawnRequest,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "jod",
    about = "Delegate to an agent harness and watch it work.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Delegate a prompt to a harness and stream the result.
    Run {
        /// The prompt. Omit to read it from stdin.
        prompt: Option<String>,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        /// Name for this agent, shown in listings and the tmux session.
        #[arg(short, long)]
        name: Option<String>,
        /// Working directory for the agent.
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long, value_enum, default_value_t = PermissionArg::Ask)]
        permission: PermissionArg,
        /// Continue the most recent conversation instead of starting one.
        #[arg(short = 'C', long = "continue", conflicts_with = "session")]
        continue_last: bool,
        /// Continue one specific conversation by its harness session id.
        #[arg(short, long)]
        session: Option<String>,
        /// Return as soon as the agent is launched instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Emit raw event JSON, one per line, instead of formatted output.
        #[arg(long)]
        json: bool,
        /// Show the agent's thinking as it streams.
        #[arg(long)]
        thinking: bool,
    },
    /// List the agents this process knows about.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Show which harnesses are installed and usable.
    Harnesses {
        #[arg(long)]
        json: bool,
    },
    /// Follow a running agent, or replay a finished one.
    ///
    /// Reads the run out of the database, so it works for an agent this
    /// process never launched — including one still running from a session
    /// that has since been closed.
    Watch {
        id: String,
        #[arg(long)]
        json: bool,
        /// Show the agent's reasoning as well as its output.
        #[arg(long)]
        thinking: bool,
    },
    /// Stop an agent and everything it started.
    Kill { id: String },
    /// Counts and total spend across all agents.
    Report {
        #[arg(long)]
        json: bool,
    },
    /// Runs from earlier sessions, newest first.
    History {
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Teach Jod something it should remember.
    Remember {
        /// What the fact is about, e.g. "reljod".
        subject: String,
        /// The relation, e.g. "prefers".
        predicate: String,
        /// The value, e.g. "linear for tasks".
        object: String,
        /// Where this came from — a note path, a URL, a person.
        #[arg(long)]
        source: Option<String>,
        /// Which domain this belongs to. Scopes are hard partitions.
        #[arg(long, default_value = jod_core::store::DEFAULT_SCOPE)]
        scope: String,
        /// Who asserted it. Never inferred from the text itself.
        #[arg(long, value_enum, default_value_t = OriginArg::Owner)]
        origin: OriginArg,
    },
    /// Search what Jod remembers.
    Recall {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Restrict to one domain. Omit to search every scope.
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a conversation and write what it established into memory.
    ///
    /// Jod has no model client, so the reading is itself an agent run: the
    /// material goes out as a prompt, the answer comes back as JSON lines, and
    /// every trust decision — which scope, which origin, what may supersede
    /// what — is made here rather than by the agent. Without this, `facts` has
    /// no writer but a person typing `jod remember`.
    Consolidate {
        /// The conversation to read. A prefix of its id is enough.
        conversation: Option<String>,
        /// Read a run's event stream instead of a conversation.
        #[arg(long, conflicts_with = "conversation")]
        run: Option<String>,
        /// Where this material came from. Required, and never inferred: by the
        /// time Reljod's own chat and a page Jod fetched are both a string of
        /// text they look identical, so the caller — which does know — says.
        #[arg(long, value_enum)]
        provenance: ProvenanceArg,
        /// Which domain the facts belong to. Scopes are hard partitions, and a
        /// line naming another one is discarded rather than filed.
        #[arg(long, default_value = jod_core::store::DEFAULT_SCOPE)]
        scope: String,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        /// Run the extraction and print what it would write, writing nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently destroy a fact — every version of it, not just the current.
    Forget {
        subject: String,
        predicate: String,
        #[arg(long, default_value = jod_core::store::DEFAULT_SCOPE)]
        scope: String,
    },
    /// The full-screen interface: conversation, live agents, status.
    Tui {
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long, value_enum, default_value_t = PermissionArg::Ask)]
        permission: PermissionArg,
        /// Pick up the last conversation instead of starting a new one.
        #[arg(short = 'C', long = "continue")]
        continue_last: bool,
        /// Watch a team: Ctrl-G shows its members and its task board.
        #[arg(long)]
        team: Option<String>,
    },
    /// Agent teams: several agents on one job, talking to each other.
    ///
    /// A team can mix harnesses — a lead on Claude Code with teammates on AGY
    /// and OpenCode — which is the thing no single harness can do.
    Team {
        #[command(subcommand)]
        what: TeamCommand,
    },
    /// Run the scheduler: fire due schedules and advance goals, for ever.
    ///
    /// This is the process that makes a schedule mean anything. Without it the
    /// tick exists and is tested and nothing calls it, so `jod schedule ls`
    /// describes work that will never happen. Install it with
    /// `deploy/jod-daemon.service`.
    Daemon {
        /// Tick once and exit, rather than staying resident. For a systemd
        /// timer, or for checking the thing works before enabling the unit.
        #[arg(long)]
        once: bool,
    },
    /// Conversations Jod owns: list them, fork one, take one back.
    ///
    /// A conversation here is a tree, not a line. Two of the three harnesses
    /// can fork themselves and none can hand a thread to another, so the
    /// transcript that survives a harness switch has to be Jod's.
    Conv {
        #[command(subcommand)]
        what: ConvCommand,
    },
    /// Work that fires on the clock.
    ///
    /// A schedule is a prompt, a cron expression and a timezone. Everything
    /// else — what to do about a run that overran, or instants missed while
    /// Jod was down — is policy with a default that was chosen by measuring
    /// what happens without it.
    Schedule {
        #[command(subcommand)]
        what: ScheduleCommand,
    },
    /// Standing objectives, pursued until they are met.
    ///
    /// A goal differs from a schedule in having an end: it stops when it is
    /// satisfied, when it runs out of budget or iterations, or when it stops
    /// making progress. That last one matters most — a loop that keeps running
    /// while nothing changes looks exactly like a loop doing useful work.
    Goal {
        #[command(subcommand)]
        what: GoalCommand,
    },
    /// What Jod's memory connects to a thing.
    ///
    /// Recall answers "what do I know about X". This answers "what is X
    /// connected to", which no list of facts can — it walks the graph.
    Related {
        /// The entity to start from.
        subject: String,
        /// How many hops out. Capped: four undirected hops from a
        /// well-connected thing returns a fifth of everything Jod knows.
        #[arg(short = 'n', long, default_value_t = 2)]
        hops: u32,
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(long)]
        json: bool,
    },
    /// How two things in memory are connected, if they are.
    Path {
        from: String,
        to: String,
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(short = 'n', long, default_value_t = 5)]
        max: u32,
    },
    /// Hold a conversation on a plain terminal, without the full-screen UI.
    Chat {
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long, value_enum, default_value_t = PermissionArg::Ask)]
        permission: PermissionArg,
        /// Pick up the last conversation instead of starting a new one.
        #[arg(short = 'C', long = "continue")]
        continue_last: bool,
    },
    /// Serve Jod's own tools to a harness, as an MCP server over stdio.
    ///
    /// This is the seam the system turns on. Jod has no model client and never
    /// will; what it has is effects — delegating, scheduling, remembering,
    /// saying what is running. Both Claude Code (`--mcp-config`) and OpenCode
    /// (`opencode mcp add`) already speak MCP, so a run wired to this thinks
    /// *and* acts in one loop, with the harness supplying every judgement.
    ///
    /// Not a command to type: stdin and stdout carry the protocol. A harness is
    /// pointed at it by config, and `--access` says how much of Jod that
    /// particular agent gets.
    Mcp {
        /// How much of Jod the agent on the other end may reach. Fail-closed:
        /// an unset flag gets the read-only set, never the full one.
        /// Parsed by `jod_core::mcp::parse_access`, not by a `value_enum`, so
        /// there is exactly one spelling of a level in the system.
        ///
        /// Found the hard way. A derived enum accepts only `read-only`, while
        /// `ToolAccess::as_str()` writes `read_only` — so the MCP config Jod
        /// generated for a harness invoked the server with a flag the server
        /// rejected. It exited 2, the harness reported no tools, and the agent
        /// truthfully said Jod had none. Every layer was correct and the seam
        /// was closed by two spellings of one word.
        #[arg(long, default_value = "read_only", value_parser = parse_access_arg)]
        access: jod_core::harness::ToolAccess,
        /// The most permissive policy `delegate` may ask for — the same ceiling
        /// `jod-api` applies to a remote caller, and the same default.
        #[arg(long, value_enum, default_value_t = PermissionArg::AcceptEdits)]
        max_permission: PermissionArg,
    },
}

#[derive(Subcommand)]
enum ConvCommand {
    /// Every conversation, newest first.
    Ls {
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Read one, from its root to wherever its head currently points.
    Show {
        id: String,
        /// Only what a harness would still be sent, after compaction.
        #[arg(long)]
        live: bool,
    },
    /// Copy a conversation from a point, leaving the original untouched.
    ///
    /// Without `--at`, forks from the current head.
    Fork {
        id: String,
        #[arg(long)]
        at: Option<i64>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Move the head back to an earlier message.
    ///
    /// Non-destructive: the abandoned tail stays on disk and stays reachable,
    /// which is what makes this recoverable rather than a deletion.
    Revert { id: String, message: i64 },
    /// Point the head at any message sharing this conversation's root —
    /// including a branch abandoned earlier, which revert cannot reach because
    /// it is a cousin of the head rather than an ancestor.
    Goto { id: String, message: i64 },
    /// Search every conversation. Returns the match with the conversation's
    /// opening and closing around it, so a hit reads without the transcript.
    Search {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 5)]
        limit: usize,
    },
    /// Summarise a span out of the live window, keeping it on disk.
    Compact {
        id: String,
        /// The summary. Jod has no model client, so the text comes from
        /// whoever ran the summarising agent.
        summary: String,
    },
    /// What this conversation would be handed to another harness as.
    ///
    /// Prints the carrier rather than moving anything, because seeing what
    /// survives is the point — and for AGY the honest answer is that it
    /// becomes prompt text.
    Handoff {
        id: String,
        #[arg(short = 'H', long, value_enum)]
        to: HarnessArg,
    },
}

#[derive(Subcommand)]
enum ScheduleCommand {
    /// Every schedule, with when it next fires.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Arm a new one.
    Add {
        /// A short name, used everywhere else to refer to it.
        name: String,
        /// What to ask the agent to do.
        prompt: String,
        /// A cron expression: `0 2 * * *`, `@daily`, `*/15 * * * *`.
        #[arg(short, long)]
        cron: String,
        /// An IANA zone name — `Asia/Manila`, not `+08:00`. An offset is only
        /// correct until the next daylight-saving transition.
        #[arg(short = 'z', long, default_value = "UTC")]
        timezone: String,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        /// What to do about instants missed while Jod was not running.
        #[arg(long, default_value = "fire_once")]
        misfire: String,
        /// What to do when it comes due and the last run is still going.
        #[arg(long, default_value = "skip")]
        overlap: String,
    },
    /// Stop a schedule firing, without forgetting it.
    Pause { name: String },
    /// Arm it again. Also clears whatever failure count stopped it.
    Resume { name: String },
    /// Fire it now, through the ordinary tick so every policy still applies.
    Run { name: String },
    /// Forget a schedule and its history.
    Rm { name: String },
    /// What a schedule has done lately.
    Log {
        name: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum GoalCommand {
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Set a standing objective.
    Add {
        name: String,
        /// What the goal is, in a sentence.
        objective: String,
        /// How often to work on it.
        #[arg(short, long, default_value = "0 * * * *")]
        cron: String,
        #[arg(short = 'z', long, default_value = "UTC")]
        timezone: String,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// A command that decides "done". Deterministic, and consulted before
        /// anything is asked to judge progress.
        #[arg(long)]
        done_when: Option<String>,
        /// Stop after this many iterations, whatever the state.
        #[arg(long)]
        max_iterations: Option<i64>,
        /// Stop once this much has been spent.
        #[arg(long)]
        budget: Option<f64>,
        /// How many iterations may finish without progress before it is called
        /// stalled rather than left running.
        #[arg(long, default_value_t = 6)]
        stall_after: i64,
    },
    Pause { name: String },
    Resume { name: String },
    /// Run one iteration now.
    Run { name: String },
    Rm { name: String },
    /// What this goal has done, out of its own memory.
    ///
    /// A goal's progress lives in the fact store rather than in its columns,
    /// under a scope keyed by its id — which nobody can be expected to type.
    /// This is the way in.
    Log {
        name: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum TeamCommand {
    /// Put a member on a team, or update the one already there.
    Join {
        team: String,
        member: String,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(short, long, default_value = "")]
        role: String,
    },
    /// Put a task on the team's board.
    Task {
        team: String,
        id: String,
        title: Vec<String>,
    },
    /// Take ownership of a task. Reports whether this member won the race.
    Claim { id: String, member: String },
    /// Mark a task finished.
    Done { id: String },
    /// Send a message. Without --to it goes to every member but the sender.
    Msg {
        team: String,
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        to: Option<String>,
        text: Vec<String>,
    },
    /// Read a member's waiting messages and mark them delivered.
    Inbox {
        team: String,
        member: String,
        /// Look without consuming, so the next turn still sees them.
        #[arg(long)]
        peek: bool,
    },
    /// Deliver waiting messages: resume every idle member that has mail.
    ///
    /// Safe to run repeatedly — a member with nothing waiting, or one still
    /// working, is left alone.
    Wake {
        team: String,
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = PermissionArg::Ask)]
        permission: PermissionArg,
        /// Say what would happen without spawning anything.
        #[arg(long)]
        dry_run: bool,
        /// Return as soon as the agents are launched, instead of waiting.
        #[arg(short, long)]
        detach: bool,
    },
    /// Give a member its first turn, so it has a conversation to resume.
    ///
    /// A member has no session until it has run once; `wake` refuses to resume
    /// one it cannot identify, so this is how a teammate gets started.
    Start {
        team: String,
        member: String,
        prompt: Vec<String>,
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = PermissionArg::Ask)]
        permission: PermissionArg,
        /// Return as soon as the agent is launched, instead of waiting for it.
        #[arg(short, long)]
        detach: bool,
    },
    /// Who is on the team, and what is on its board.
    Show { team: String },
    /// Every team that has a member.
    List,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum HarnessArg {
    Claude,
    Opencode,
    Agy,
}

impl From<HarnessArg> for HarnessKind {
    fn from(a: HarnessArg) -> Self {
        match a {
            HarnessArg::Claude => HarnessKind::ClaudeCode,
            HarnessArg::Opencode => HarnessKind::OpenCode,
            HarnessArg::Agy => HarnessKind::Agy,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum OriginArg {
    /// Reljod said so. The default when a human types `jod remember`.
    Owner,
    /// An agent concluded it.
    Agent,
    /// Read from outside — a page, an email, a document.
    Untrusted,
    /// Jod itself recorded it.
    System,
}

impl From<OriginArg> for Origin {
    fn from(a: OriginArg) -> Self {
        match a {
            OriginArg::Owner => Origin::Owner,
            OriginArg::Agent => Origin::Agent,
            OriginArg::Untrusted => Origin::Untrusted,
            OriginArg::System => Origin::System,
        }
    }
}

/// Where consolidated material came from.
///
/// Deliberately not a superset of `OriginArg`: no provenance yields
/// `Origin::Owner`. An agent reading a transcript *concludes* things; only
/// Reljod asserts them, by typing `jod remember`.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ProvenanceArg {
    /// A conversation between Reljod and Jod.
    OwnerChat,
    /// Anything Jod ingested — a fetched page, a payload, an email. Untrusted,
    /// and excluded from recall by default.
    Ingested,
}

impl From<ProvenanceArg> for Provenance {
    fn from(a: ProvenanceArg) -> Self {
        match a {
            ProvenanceArg::OwnerChat => Provenance::OwnerChat,
            ProvenanceArg::Ingested => Provenance::Ingested,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum PermissionArg {
    /// Let the agent read — files and the web — and refuse everything that
    /// could change something.
    Ask,
    /// Let file edits through; still prompt for anything else.
    AcceptEdits,
    /// Auto-approve everything. Only sane in a throwaway directory.
    Bypass,
}

/// How much of Jod itself an agent may reach over MCP.
///
/// A separate axis from `PermissionArg`, which bounds what the agent may do to
/// the *machine*. An agent can be trusted to edit files and still have no
/// business arming a schedule that spends money every night at 2am.
///
/// Deliberately not a `ValueEnum`. The derive would define a second spelling of
/// every level beside `ToolAccess::as_str()`, and the two drifting apart is not
/// hypothetical — it silently disconnected Jod from every harness it had just
/// been wired to. One parser, in core, accepting both hyphen and underscore.
fn parse_access_arg(s: &str) -> Result<jod_core::harness::ToolAccess, String> {
    jod_core::mcp::parse_access(s).ok_or_else(|| {
        format!("`{s}` is not an access level — read_only, delegate or orchestrate")
    })
}

impl From<PermissionArg> for PermissionPolicy {
    fn from(a: PermissionArg) -> Self {
        match a {
            PermissionArg::Ask => PermissionPolicy::Ask,
            PermissionArg::AcceptEdits => PermissionPolicy::AcceptEdits,
            PermissionArg::Bypass => PermissionPolicy::Bypass,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Persistent by default: an assistant that forgets every run when the
    // process exits is a task runner, not an assistant.
    let jod = Jod::persistent().context("opening ~/.jod/jod.db")?;

    match cli.command {
        Command::Harnesses { json } => {
            let list = jod.harnesses();
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                render::harnesses(&list);
            }
        }

        Command::Run {
            prompt,
            harness,
            name,
            cwd,
            model,
            permission,
            continue_last,
            session,
            detach,
            json,
            thinking,
        } => {
            let prompt = match prompt {
                Some(p) => p,
                None => read_stdin().context("reading the prompt from stdin")?,
            };
            if prompt.trim().is_empty() {
                bail!("empty prompt — pass one as an argument or pipe it on stdin");
            }
            require_supervisor(&jod)?;

            let req = SpawnRequest {
                name: name.unwrap_or_else(|| default_name(&prompt)),
                harness: harness.into(),
                prompt,
                cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
                model,
                permission: permission.into(),
                resume: match session {
                    Some(id) => Resume::Session(id),
                    None if continue_last => Resume::Last,
                    None => Resume::Fresh,
                },
                // `jod run` is one task, not an orchestrator. Handing it Jod's
                // own verbs would let a one-liner create schedules that spend
                // money nightly, which is a decision that should be made on
                // purpose rather than inherited from a default.
                tools: None,
            };

            // Subscribe *before* spawning, so no early event is missed.
            let events = jod.subscribe();
            let agent = jod.spawn_agent(req).await?;

            if detach {
                render::launched(&agent);
                return Ok(());
            }
            render::launched_waiting(&agent);
            let code = render::stream(events, &agent.id, json, thinking).await;
            std::process::exit(code);
        }

        Command::Ls { json } => {
            // A fresh process knows nothing until it reads the database back.
            jod.rehydrate(200).await?;
            let agents = jod.agents().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&agents)?);
            } else {
                render::agents(&agents);
            }
        }

        Command::Watch { id, json, thinking } => {
            // Subscribe before rehydrating: rehydrate starts the followers that
            // produce the live events, and one that fired first would be lost.
            let events = jod.subscribe();
            jod.rehydrate(200).await?;
            let agent = jod.agent(&id).await?;

            // Everything that already happened, then everything that follows,
            // with no gap between the two — the same contract the SSE stream
            // gives a phone.
            let history = jod.events_since(&id, None).await?;
            let last_seen = history.last().map(|e| e.seq);
            for envelope in history {
                render::print_envelope(&envelope, json, thinking);
            }

            if agent.status != jod_core::AgentStatus::Running {
                return Ok(());
            }
            let code = render::stream_after(events, &id, last_seen, json, thinking).await;
            std::process::exit(code);
        }

        Command::Kill { id } => {
            jod.rehydrate(200).await?;
            jod.kill_agent(&id).await?;
            println!("killed {id}");
        }

        Command::Report { json } => {
            jod.rehydrate(200).await?;
            let report = jod.report().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render::report(&report);
            }
        }

        Command::History { limit, json } => {
            let runs = jod.history(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
            } else {
                render::history(&runs);
            }
        }

        Command::Remember {
            subject,
            predicate,
            object,
            source,
            scope,
            origin,
        } => {
            let store = jod.store().context("this command needs the database")?;
            let id = store.remember(NewFact {
                scope,
                subject,
                predicate,
                object,
                origin: origin.into(),
                source,
                valid_from: None,
            })?;
            println!("remembered #{id}");
        }

        Command::Related {
            subject,
            hops,
            scope,
            json,
        } => {
            let store = jod.store().context("this command needs the database")?;
            let now = chrono::Utc::now().timestamp_millis();
            let found = store.neighbourhood(&scope, &subject, hops, now)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else if found.is_empty() {
                println!("nothing connected to {subject}");
            } else {
                for n in &found {
                    // The hop count is the point: a thing two steps away is
                    // related differently from one Jod was told about directly.
                    println!("{:>2} hop  {}", n.hops, n.name);
                }
            }
        }

        Command::Path {
            from,
            to,
            scope,
            max,
        } => {
            let store = jod.store().context("this command needs the database")?;
            match store.path_between(&scope, &from, &to, max)? {
                Some(route) => println!("{}", route.join("  →  ")),
                None => println!("no path from {from} to {to} within {max} hops"),
            }
        }

        Command::Daemon { once } => {
            let daemon = jod_core::daemon::Daemon::persistent().await?;
            if once {
                let report = daemon.run_once().await?;
                println!(
                    "claimed {} · started {} · held {} · failed {}",
                    report.claimed, report.started, report.held, report.failed
                );
            } else {
                // Runs until SIGTERM, finishing the tick in flight rather than
                // being killed mid-claim — an abandoned claim is exactly the
                // case the lease exists to recover, and not creating one is
                // better than recovering from it.
                let report = daemon.run(jod_core::daemon::shutdown_signal()).await;
                println!(
                    "stopped after {} ticks · {} runs started · {} failed",
                    report.ticks, report.started, report.failed
                );
            }
        }

        Command::Conv { what } => conv_command(&jod, what)?,
        Command::Schedule { what } => schedule_command(&jod, what)?,
        Command::Goal { what } => goal_command(&jod, what)?,

        Command::Forget {
            subject,
            predicate,
            scope,
        } => {
            let store = jod.store().context("this command needs the database")?;
            let n = store.forget(&scope, &subject, &predicate)?;
            match n {
                0 => println!("nothing to forget"),
                1 => println!("forgot 1 version, permanently"),
                n => println!("forgot {n} versions, permanently"),
            }
        }

        Command::Consolidate {
            conversation,
            run,
            provenance,
            scope,
            harness,
            dry_run,
        } => {
            consolidate_command(&jod, conversation, run, provenance, scope, harness, dry_run)
                .await?;
        }

        Command::Recall {
            query,
            limit,
            scope,
            json,
        } => {
            let store = jod.store().context("this command needs the database")?;
            let facts = store.recall_in(scope.as_deref(), &query.join(" "), limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&facts)?);
            } else {
                render::facts(&facts);
            }
        }

        Command::Tui {
            harness,
            cwd,
            model,
            permission,
            continue_last,
            team,
        } => {
            require_supervisor(&jod)?;
            jod.rehydrate(200).await?;
            tui::run(
                jod,
                tui::Options {
                    harness: harness.into(),
                    team,
                    cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
                    model,
                    permission: permission.into(),
                    resume: if continue_last {
                        Resume::Last
                    } else {
                        Resume::Fresh
                    },
                },
            )
            .await?;
        }

        Command::Team { what } => {
            let store = jod.store().context("teams need the database")?;
            match what {
                TeamCommand::Join {
                    team,
                    member,
                    harness,
                    role,
                } => {
                    store.join_team(&team, &member, harness.into(), &role)?;
                    println!("{member} joined {team}");
                }
                TeamCommand::Task { team, id, title } => {
                    let title = title.join(" ");
                    let title = if title.is_empty() { id.clone() } else { title };
                    store.add_team_task(&team, &id, &title)?;
                    println!("{id} on {team}'s board");
                }
                TeamCommand::Claim { id, member } => {
                    // Refuse an id that is on no board. `claim_task` would
                    // otherwise invent it and report success, so a typo looked
                    // like a win and left a task nobody could see.
                    if !store.is_team_task(&id)? {
                        bail!("no task {id} on any team's board — `jod team show <team>` lists them");
                    }
                    // The exit code matters: a teammate scripting this needs to
                    // branch on whether it actually won.
                    if store.claim_task(&id, &member)? {
                        println!("{member} claimed {id}");
                    } else {
                        bail!("{id} is already owned by someone else");
                    }
                }
                TeamCommand::Done { id } => {
                    if !store.complete_task(&id)? {
                        bail!("no task {id} — `jod team show <team>` lists them");
                    }
                    println!("{id} done");
                }
                TeamCommand::Msg {
                    team,
                    from,
                    to,
                    text,
                } => {
                    let sent = store.send_team_message(
                        &team,
                        &from,
                        to.as_deref(),
                        &text.join(" "),
                    )?;
                    if sent.is_empty() {
                        println!("nobody on {team} to message");
                    } else {
                        println!("sent to {}", sent.join(", "));
                    }
                }
                TeamCommand::Inbox { team, member, peek } => {
                    let messages = if peek {
                        store.team_unread(&team, &member)?
                    } else {
                        store.drain_inbox(&team, &member)?
                    };
                    for m in &messages {
                        println!("{}", m.as_prompt());
                    }
                    if messages.is_empty() {
                        println!("nothing waiting for {member}");
                    }
                }
                TeamCommand::Wake {
                    team,
                    cwd,
                    permission,
                    dry_run,
                    detach,
                } => {
                    // A member marked busy whose run has since ended is idle
                    // again. Reconciling here rather than in a daemon keeps
                    // this command the only thing that has to run.
                    jod.rehydrate(200).await?;
                    let runs: std::collections::HashMap<String, (bool, Option<String>)> = jod
                        .agents()
                        .await
                        .into_iter()
                        .map(|a| {
                            (
                                a.id,
                                (a.status == jod_core::AgentStatus::Running, a.session_id),
                            )
                        })
                        .collect();
                    for m in store.team_members(&team)? {
                        let Some(run) = m.agent_id.as_deref().and_then(|id| runs.get(id)) else {
                            continue;
                        };
                        let (running, session) = run;
                        // Learn the conversation the harness assigned. This is
                        // the only place a member gets a session id, and
                        // without one it can never be woken — a run whose id we
                        // never recorded would be resumed into an empty
                        // context, which is worse than staying asleep.
                        if session.is_some() {
                            store.bind_member(&team, &m.name, m.agent_id.as_deref(), session.as_deref())?;
                        }
                        if !running && m.status == MemberStatus::Busy {
                            store.set_member_status(&team, &m.name, MemberStatus::Ready)?;
                        }
                    }

                    let cwd = cwd.unwrap_or_else(jod_core::service::default_cwd);
                    let mut woken = 0usize;
                    // Subscribe before any spawn, so no early event is missed.
                    let events = jod.subscribe();
                    let mut spawned: Vec<(String, String)> = Vec::new();
                    for m in store.team_members(&team)? {
                        let pending = store.team_unread(&team, &m.name)?;
                        let Some(order) = jod_core::team::wake_order(&m, &pending) else {
                            continue;
                        };
                        if dry_run {
                            println!(
                                "would wake {} on {} with {} message(s)",
                                order.member,
                                order.harness.label(),
                                order.messages
                            );
                            woken += 1;
                            continue;
                        }
                        let agent = jod
                            .spawn_agent(SpawnRequest {
                                name: format!("{team}-{}", order.member),
                                harness: order.harness,
                                prompt: order.prompt,
                                cwd: cwd.clone(),
                                model: None,
                                permission: permission.into(),
                                resume: Resume::Session(order.session_id),
                                tools: None,
                            })
                            .await?;
                        // Drain only once the spawn succeeded, so a failure
                        // leaves the mail waiting rather than losing it.
                        store.drain_inbox(&team, &m.name)?;
                        store.set_member_status(&team, &m.name, MemberStatus::Busy)?;
                        store.bind_member(&team, &m.name, Some(&agent.id), None)?;
                        println!(
                            "woke {} on {} ({} message(s)) as {}",
                            order.member,
                            order.harness.label(),
                            order.messages,
                            &agent.id[..agent.id.len().min(8)]
                        );
                        spawned.push((m.name.clone(), agent.id));
                        woken += 1;
                    }
                    if woken == 0 {
                        println!("nobody to wake");
                    } else if detach {
                        println!("detached — run `jod team wake {team}` again once they finish");
                    } else {
                        // Wait, then record what each run taught us. Without
                        // this the members stay busy for ever: the tailer lives
                        // in this process and dies with it.
                        wait_for_all(events, spawned.iter().map(|(_, id)| id.clone()).collect())
                            .await;
                        for (member, id) in &spawned {
                            settle_member(&jod, store, &team, member, id).await?;
                        }
                        println!("{woken} member(s) idle again");
                    }
                }
                TeamCommand::Start {
                    team,
                    member,
                    prompt,
                    cwd,
                    permission,
                    detach,
                } => {
                    let who = store
                        .team_members(&team)?
                        .into_iter()
                        .find(|m| m.name == member)
                        .with_context(|| format!("{member} is not on {team}"))?;
                    // Subscribe before spawning, so no early event is missed.
                    let events = jod.subscribe();
                    let agent = jod
                        .spawn_agent(SpawnRequest {
                            name: format!("{team}-{member}"),
                            harness: who.harness,
                            prompt: prompt.join(" "),
                            cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
                            model: None,
                            permission: permission.into(),
                            resume: Resume::Fresh,
                            tools: None,
                        })
                        .await?;
                    store.set_member_status(&team, &member, MemberStatus::Busy)?;
                    store.bind_member(&team, &member, Some(&agent.id), None)?;
                    println!(
                        "{member} started on {} as {}",
                        who.harness.label(),
                        &agent.id[..agent.id.len().min(8)]
                    );
                    if detach {
                        println!("detached — run `jod team wake {team}` once it finishes");
                    } else {
                        wait_for_all(events, [agent.id.clone()].into_iter().collect()).await;
                        settle_member(&jod, store, &team, &member, &agent.id).await?;
                        println!("{member} is idle again, ready to be woken");
                    }
                }
                TeamCommand::Show { team } => {
                    render::team(&store.team_members(&team)?, &store.team_tasks(&team)?);
                }
                TeamCommand::List => {
                    let teams = store.teams()?;
                    if teams.is_empty() {
                        println!("no teams yet");
                    }
                    for name in teams {
                        println!("{name}");
                    }
                }
            }
        }

        Command::Chat {
            harness,
            cwd,
            model,
            permission,
            continue_last,
        } => {
            require_supervisor(&jod)?;
            chat(jod, harness, cwd, model, permission, continue_last).await?;
        }

        Command::Mcp {
            access,
            max_permission,
        } => {
            mcp_cmd::run(jod, access, max_permission.into()).await?;
        }
    }

    Ok(())
}

/// Carry out a `jod conv …` subcommand.
///
/// An id prefix is enough everywhere one is taken — see
/// [`resolve_conversation`].
fn conv_command(jod: &Jod, what: ConvCommand) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    let resolve = |typed: &str| resolve_conversation(store, typed);

    match what {
        ConvCommand::Ls { limit, json } => {
            let all = store.conversations(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("no conversations yet");
            } else {
                render_time::conversations(&all, chrono::Utc::now().timestamp_millis());
            }
        }
        ConvCommand::Show { id, live } => {
            let id = resolve(&id)?;
            let thread = if live {
                store.live_window(&id)?
            } else {
                store.thread(&id)?
            };
            render_time::thread(&thread);
        }
        ConvCommand::Fork { id, at, title } => {
            let id = resolve(&id)?;
            let at = match at {
                Some(m) => m,
                None => store
                    .conversation(&id)?
                    .and_then(|c| c.head_id)
                    .context("that conversation has no messages to fork from")?,
            };
            let forked = store.fork_conversation(&id, at, title.as_deref())?;
            println!("forked to {}", forked.id);
            println!("the original is untouched — `jod conv ls` shows both");
        }
        ConvCommand::Revert { id, message } => {
            let id = resolve(&id)?;
            store.revert_to(&id, message)?;
            println!("head is back at message {message}");
            println!("nothing was deleted; `jod conv goto` returns to what was abandoned");
        }
        ConvCommand::Goto { id, message } => {
            let id = resolve(&id)?;
            store.move_head(&id, message)?;
            println!("head is now at message {message}");
        }
        ConvCommand::Search { query, limit } => {
            let hits = store.search_messages(&query.join(" "), limit)?;
            if hits.is_empty() {
                println!("nothing matched");
            } else {
                render_time::search(&hits);
            }
        }
        ConvCommand::Compact { id, summary } => {
            let id = resolve(&id)?;
            let live = store.live_window(&id)?;
            let (Some(first), Some(last)) = (live.first(), live.last()) else {
                bail!("nothing to compact");
            };
            let done = store.compact(&id, first.id, last.id, &summary, "manual")?;
            // Both numbers, because a compaction that freed nothing is a real
            // outcome and should be visible rather than repeated forever.
            println!(
                "compacted {} chars to {} — the originals stay on disk and stay searchable",
                done.before_chars, done.after_chars
            );
        }
        ConvCommand::Handoff { id, to } => {
            let id = resolve(&id)?;
            let carrier = store.handoff(&id, HarnessKind::from(to))?;
            if carrier.is_lossy() {
                // Said before the move, because that is when the loss is still
                // a decision rather than a discovery.
                eprintln!(
                    "note: this harness accepts no transcript, so the prior \
                     conversation travels as prompt text — structure is lost"
                );
            }
            // Printed in exactly the form the receiving harness takes, so this
            // is pipeable rather than merely informative: stream-json lines go
            // to `claude --input-format stream-json`, the document to
            // `opencode import`, the prefix into a prompt.
            match carrier {
                jod_core::conversation::Handoff::StreamJson { lines } => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                jod_core::conversation::Handoff::Import { document } => {
                    println!("{}", serde_json::to_string_pretty(&document)?);
                }
                jod_core::conversation::Handoff::PromptPrefix { text } => println!("{text}"),
            }
        }
    }
    Ok(())
}

/// Resolve a typed id prefix against the conversations that exist.
///
/// An ambiguous prefix is refused rather than guessed: reverting or
/// consolidating the wrong conversation is not undoable by the person who did
/// it.
fn resolve_conversation(store: &Store, typed: &str) -> Result<String> {
    let all = store.conversations(500)?;
    let hits: Vec<_> = all.iter().filter(|c| c.id.starts_with(typed)).collect();
    match hits.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => bail!("no conversation starts with {typed}"),
        many => bail!(
            "{typed} matches {} conversations — type more of it",
            many.len()
        ),
    }
}

/// Turn a conversation, or a run, into memory.
///
/// The shape of this command follows the module it drives: Jod builds the
/// prompt, an agent does the reading, and Jod reads the answer back under rules
/// the agent cannot reach. So the flow is spawn → wait → parse → write, and the
/// only thing the caller decides is *where the material came from*, which is
/// the one input that cannot be recovered from the text.
async fn consolidate_command(
    jod: &std::sync::Arc<Jod>,
    conversation: Option<String>,
    run: Option<String>,
    provenance: ProvenanceArg,
    scope: String,
    harness: HarnessArg,
    dry_run: bool,
) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;

    let (material, source) = match (conversation, run) {
        (Some(typed), _) => {
            let id = resolve_conversation(store, &typed)?;
            (
                transcript_material(&store.transcript(&id)?),
                format!("conversation:{id}"),
            )
        }
        (None, Some(id)) => {
            // Straight from the event log, so a run that was never placed in a
            // conversation — anything launched before this existed, or by a
            // process that has since gone — can still be read.
            let events = jod.events_since(&id, None).await?;
            (run_material(&id, &events), format!("run:{id}"))
        }
        (None, None) => bail!(
            "say which conversation to consolidate — `jod conv ls` lists them — \
             or pass --run <id>"
        ),
    };
    if material.trim().is_empty() {
        bail!("{source} has nothing in it to read");
    }

    let consolidation = Consolidation::new(scope, provenance.into(), material)
        .from(source.clone())
        .with_harness(harness.into());

    require_supervisor(jod)?;
    // Subscribe before spawning, so no early event is missed.
    let events = jod.subscribe();
    let agent = jod
        // Detached: the extraction's prompt *is* a transcript, and recording it
        // as a conversation would store the same text twice and index it twice
        // for search.
        .spawn_agent_in(consolidation.extraction_request(), RunConversation::Detached)
        .await?;
    eprintln!(
        "reading {source} with {} as {} …",
        agent.harness_label,
        &agent.id[..agent.id.len().min(8)]
    );
    let output = collect_output(events, &agent.id).await;

    settle_consolidation(store, &consolidation, &output, dry_run)
}

/// Read an extraction's output, report it, and write it unless this is a dry
/// run.
///
/// Split out from the command because "did the dry run write anything" is the
/// question worth a test here, and a test cannot spawn an agent.
fn settle_consolidation(
    store: &Store,
    consolidation: &Consolidation,
    output: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        let batch = consolidation.parse(output);
        if batch.facts.is_empty() && batch.dropped.is_empty() {
            println!("nothing durable in that material — a correct and common answer");
            return Ok(());
        }
        for fact in &batch.facts {
            println!(
                "would write  {} | {} | {}   ({})",
                fact.subject,
                fact.predicate,
                fact.object,
                would_do(store, fact)?
            );
        }
        report_dropped(&batch.dropped);
        if batch.truncated {
            println!("the agent offered more lines than the cap allows; the rest were not read");
        }
        println!(
            "nothing was written. The trust and loss rules are applied at write \
             time, so a real run may still refuse some of this."
        );
        return Ok(());
    }

    let outcome = consolidation.apply(store, output);
    if let Some(refusal) = &outcome.refused {
        // Nothing was written at all — a consolidation that would lose most of
        // what was known about a subject is not partially right.
        println!(
            "refused: this would retire {} of the {} things known about {} ({:.0}%, limit {:.0}%)",
            refusal.retiring,
            refusal.prior,
            refusal.subject,
            refusal.fraction * 100.0,
            refusal.limit * 100.0
        );
        println!("nothing was written");
        return Ok(());
    }
    println!(
        "wrote {} fact(s) · {} superseded a belief · {} already known",
        outcome.written.len(),
        outcome.superseded,
        outcome.restated
    );
    report_dropped(&outcome.dropped);
    if outcome.truncated {
        println!("the agent offered more lines than the cap allows; the rest were not read");
    }
    if let Some(error) = &outcome.error {
        // Reported, not raised: the conversation still happened.
        println!("the store stopped part-way through: {error}");
    }
    Ok(())
}

/// What writing one extracted line would do to what Jod already believes.
///
/// The same question `Consolidation::apply` asks — the current belief on this
/// subject, predicate and scope — asked read-only, so a dry run says something
/// more useful than "it parsed". It stops short of the trust ranking and the
/// loss guard, which are the store's to apply and are stated as such.
fn would_do(store: &Store, fact: &NewFact) -> Result<String> {
    let current = store
        .facts_about(&fact.subject)?
        .into_iter()
        .find(|f| f.scope == fact.scope && f.predicate == fact.predicate);
    Ok(match current {
        Some(f) if f.object.trim().eq_ignore_ascii_case(fact.object.trim()) => {
            "already known".to_string()
        }
        Some(f) => format!("supersedes “{}”", f.object),
        None => "new".to_string(),
    })
}

fn report_dropped(dropped: &[jod_core::consolidate::Dropped]) {
    if dropped.is_empty() {
        return;
    }
    println!("{} line(s) dropped:", dropped.len());
    for d in dropped {
        // The reason as the parser named it. Rendered debug rather than
        // translated, so a reason added later shows up here rather than
        // vanishing into a catch-all arm.
        println!("  {:?}  {}", d.reason, d.line);
    }
}

/// A conversation as the text an extraction reads.
///
/// Thinking is left out. It is one model's private reasoning — speculation,
/// options weighed and dropped — and a fact extracted from it is something
/// nobody ever asserted.
fn transcript_material(transcript: &[PortableMessage]) -> String {
    transcript
        .iter()
        .filter(|m| m.role != "thinking")
        .map(|m| match m.role.as_str() {
            "tool_call" => format!(
                "{} call: {}",
                m.tool_name.as_deref().unwrap_or("tool"),
                m.text
            ),
            "tool_result" => format!(
                "{} result: {}",
                m.tool_name.as_deref().unwrap_or("tool"),
                m.text
            ),
            role => format!("{role}: {}", m.text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A run as the text an extraction reads.
///
/// The prompt comes off disk rather than out of the events, because no harness
/// reports its own prompt back — and the prompt is usually the densest thing in
/// the run, since it is the part a person wrote.
fn run_material(run_id: &str, events: &[AgentEnvelope]) -> String {
    let mut lines = Vec::new();
    if let Ok(prompt) = std::fs::read_to_string(jod_core::paths::prompt_path(run_id)) {
        lines.push(format!("user: {}", prompt.trim()));
    }
    for envelope in events {
        match &envelope.event {
            AgentEvent::Message { text } => lines.push(format!("assistant: {text}")),
            AgentEvent::ToolResult {
                name,
                summary: Some(summary),
                ..
            } => lines.push(format!("{name} result: {summary}")),
            _ => {}
        }
    }
    lines.join("\n")
}

/// Wait for one run to finish, and return everything it said.
///
/// Every assistant message, not only the last: an agent asked for JSON lines
/// will preface them, apologise, or wrap them in a fence, and the parser passes
/// over prose in silence anyway — so keeping the lot costs nothing and loses no
/// facts to a stray "here you go".
async fn collect_output(
    mut events: jod_core::broadcast::Receiver<AgentEnvelope>,
    agent_id: &str,
) -> String {
    use jod_core::broadcast::error::RecvError;
    let mut said = String::new();
    loop {
        match events.recv().await {
            Ok(envelope) if envelope.agent_id == agent_id => match envelope.event {
                AgentEvent::Message { text } => {
                    said.push_str(&text);
                    said.push('\n');
                }
                // `Finished.text` repeats the last message, so it is not added.
                AgentEvent::Finished { .. } => return said,
                AgentEvent::Error { message } => eprintln!("[jod] {message}"),
                _ => {}
            },
            Ok(_) => {}
            // Nothing more is coming. Whatever was said is still worth reading.
            Err(RecvError::Closed) => return said,
            Err(RecvError::Lagged(_)) => continue,
        }
    }
}

/// Carry out a `jod schedule …` subcommand.
///
/// Every path here goes through the store rather than spawning anything: a
/// schedule is armed by writing a row, and the tick is what fires it. Even
/// `run` only brings the next instant forward, so a hand-started run picks up
/// the same overlap policy, failure count and fire record as a timed one.
fn schedule_command(jod: &Jod, what: ScheduleCommand) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        ScheduleCommand::Ls { json } => {
            let all = store.schedules()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("no schedules — `jod schedule add` arms one");
            } else {
                render_time::schedules(&all, now);
            }
        }
        ScheduleCommand::Add {
            name,
            prompt,
            cron,
            timezone,
            harness,
            cwd,
            model,
            misfire,
            overlap,
        } => {
            let s = jod_core::schedule::Schedule {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                prompt,
                harness: HarnessKind::from(harness).id().to_string(),
                cwd: cwd.unwrap_or(std::env::current_dir()?).display().to_string(),
                model,
                cron,
                timezone,
                state: jod_core::schedule::ScheduleState::Armed,
                misfire: misfire.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
                overlap: overlap.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
                grace_ms: 300_000,
                jitter_ms: 0,
                next_fire_at_ms: None,
                last_fire_at_ms: None,
                consecutive_failures: 0,
                created_at_ms: now,
            };
            store.add_schedule(&s)?;
            let armed = store.schedule_named(&name)?.and_then(|s| s.next_fire_at_ms);
            match armed {
                Some(at) => println!("{name} armed — next {}", render_time::when(at, now)),
                None => println!("{name} armed"),
            }
        }
        ScheduleCommand::Pause { name } => {
            let changed =
                store.set_schedule_state(&name, jod_core::schedule::ScheduleState::Paused)?;
            println!("{}", if changed { format!("{name} paused") } else { format!("no schedule {name}") });
        }
        ScheduleCommand::Resume { name } => {
            let changed =
                store.set_schedule_state(&name, jod_core::schedule::ScheduleState::Armed)?;
            println!("{}", if changed { format!("{name} armed") } else { format!("no schedule {name}") });
        }
        ScheduleCommand::Run { name } => {
            if store.run_schedule_now(&name, now)? {
                println!("{name} is due now — the next tick will fire it");
            } else {
                // Refused rather than forced: firing something paused or broken
                // silently would defeat the reason it was stopped.
                println!("{name} is not armed, so it was not brought forward");
            }
        }
        ScheduleCommand::Rm { name } => {
            let gone = store.delete_schedule(&name)?;
            println!("{}", if gone { format!("{name} forgotten") } else { format!("no schedule {name}") });
        }
        ScheduleCommand::Log { name, limit } => {
            let Some(s) = store.schedule_named(&name)? else {
                bail!("no schedule {name}");
            };
            let fires = store.fires(&s.id, limit)?;
            if fires.is_empty() {
                println!("{name} has not fired yet");
            } else {
                render_time::fires(&fires, now);
            }
        }
    }
    Ok(())
}

/// Carry out a `jod goal …` subcommand.
fn goal_command(jod: &Jod, what: GoalCommand) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        GoalCommand::Ls { json } => {
            let all = store.goals()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("no goals — `jod goal add` sets one");
            } else {
                render_time::goals(&all, now);
            }
        }
        GoalCommand::Add {
            name,
            objective,
            cron,
            timezone,
            harness,
            cwd,
            done_when,
            max_iterations,
            budget,
            stall_after,
        } => {
            let g = jod_core::schedule::Goal {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                objective,
                done_when,
                harness: HarnessKind::from(harness).id().to_string(),
                cwd: cwd.unwrap_or(std::env::current_dir()?).display().to_string(),
                model: None,
                cron,
                timezone,
                state: jod_core::schedule::GoalState::Running,
                iteration: 0,
                max_iterations,
                budget_usd: budget,
                spent_usd: 0.0,
                stall_after,
                no_progress: 0,
                next_fire_at_ms: None,
                created_at_ms: now,
            };
            store.add_goal(&g)?;
            println!("{name} is running");
        }
        GoalCommand::Pause { name } => {
            let changed = store.set_goal_state(&name, jod_core::schedule::GoalState::Paused)?;
            println!("{}", if changed { format!("{name} paused") } else { format!("no goal {name}") });
        }
        GoalCommand::Resume { name } => {
            let changed = store.set_goal_state(&name, jod_core::schedule::GoalState::Running)?;
            println!("{}", if changed { format!("{name} running") } else { format!("no goal {name}") });
        }
        GoalCommand::Run { name } => {
            if store.run_goal_now(&name, now)? {
                println!("{name} will iterate on the next tick");
            } else {
                println!("{name} is not running, so it was not brought forward");
            }
        }
        GoalCommand::Rm { name } => {
            let gone = store.delete_goal(&name)?;
            println!("{}", if gone { format!("{name} forgotten") } else { format!("no goal {name}") });
        }
        GoalCommand::Log { name, limit } => {
            if store.goal_named(&name)?.is_none() {
                bail!("no goal {name}");
            }
            // Keyed on the subject, which is derived from the name, precisely
            // so this does not need the id the scope is keyed on.
            let facts = store.facts_about(&format!("goal/{name}"))?;
            if facts.is_empty() {
                println!("{name} has not iterated yet");
                return Ok(());
            }
            for f in facts.iter().filter(|f| f.predicate == "pursuing") {
                println!("pursuing  {}", f.object);
            }
            for f in facts.iter().filter(|f| f.predicate == "ended") {
                println!("ended     {}", f.object);
            }
            let history: Vec<_> = facts.iter().filter(|f| f.predicate == "iteration").collect();
            if history.is_empty() {
                println!("no iteration has finished yet");
            }
            for f in history.iter().take(limit) {
                println!("  {}", f.object);
            }
        }
    }
    Ok(())
}

/// Refuse to start an agent when nothing could supervise it.
///
/// `jod-run` is what holds a run's output once the caller walks away; without
/// it a spawn would fail later and less clearly, after the run had a name.
fn require_supervisor(jod: &Jod) -> Result<()> {
    if !jod.supervisor_available() {
        bail!(
            "`jod-run` was not found — it supervises every agent and ships \
             alongside `jod`. Point at it with JOD_SUPERVISOR_BIN if it lives \
             somewhere unusual."
        );
    }
    Ok(())
}

/// One conversation, many turns.
///
/// Every turn after the first resumes the harness session the previous turn
/// reported, so context carries across turns — and every turn is recorded into
/// the *same* Jod conversation, which is a separate thing from the harness
/// session and outlives it. Both are needed: the session is what makes the next
/// turn cheap, the conversation is what survives the harness being swapped.
async fn chat(
    jod: std::sync::Arc<Jod>,
    harness: HarnessArg,
    cwd: Option<PathBuf>,
    model: Option<String>,
    permission: PermissionArg,
    continue_last: bool,
) -> Result<()> {
    use std::io::Write;

    let kind: HarnessKind = harness.into();
    let cwd = cwd.unwrap_or_else(jod_core::service::default_cwd);
    let mut resume = if continue_last {
        Resume::Last
    } else {
        Resume::Fresh
    };
    let mut conversation = RunConversation::New;

    eprintln!("jod chat · {} · Ctrl-D to leave", kind.label());
    loop {
        eprint!("\n› ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        // read_line returning 0 is EOF — the user pressed Ctrl-D.
        if std::io::stdin().read_line(&mut line)? == 0 {
            eprintln!();
            return Ok(());
        }
        let prompt = line.trim().to_string();
        if prompt.is_empty() {
            continue;
        }
        if prompt == "/exit" || prompt == "/quit" {
            return Ok(());
        }

        let events = jod.subscribe();
        let agent = jod
            .spawn_agent_in(
                SpawnRequest {
                    name: default_name(&prompt),
                    harness: kind,
                    prompt,
                    cwd: cwd.clone(),
                    model: model.clone(),
                    permission: permission.into(),
                    resume: resume.clone(),
                    tools: None,
                },
                conversation.clone(),
            )
            .await?;
        render::stream(events, &agent.id, false, false).await;

        // The rest of the chat lands in the conversation the first turn opened,
        // so `jod conv show` reads back as the conversation it was rather than
        // as one thread per line typed.
        if let Some(id) = jod.conversation_of(&agent.id).await {
            conversation = RunConversation::Existing(id);
        }

        // Prefer the id the harness reported; fall back to "continue the most
        // recent", which every harness also supports.
        resume = match jod.agent(&agent.id).await.ok().and_then(|a| a.session_id) {
            Some(id) => Resume::Session(id),
            None => Resume::Last,
        };
    }
}

fn read_stdin() -> std::io::Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

/// A short, human-recognisable name derived from the prompt's first words.
/// Wait until every one of `pending` has finished.
///
/// A team command that returned before its runs ended would leave the members
/// marked busy for ever: the tailer lives in *this* process, so nothing would
/// ever record that they stopped.
async fn wait_for_all(
    mut events: jod_core::broadcast::Receiver<jod_core::AgentEnvelope>,
    mut pending: std::collections::HashSet<String>,
) {
    use jod_core::broadcast::error::RecvError;
    while !pending.is_empty() {
        match events.recv().await {
            Ok(env) => {
                if matches!(env.event, jod_core::AgentEvent::Finished { .. }) {
                    pending.remove(&env.agent_id);
                }
            }
            // Nothing more is coming; stop rather than hang.
            Err(RecvError::Closed) => return,
            Err(RecvError::Lagged(_)) => continue,
        }
    }
}

/// Record what a finished run taught us: the conversation to resume next time,
/// and that the member is idle again.
async fn settle_member(
    jod: &std::sync::Arc<Jod>,
    store: &jod_core::store::Store,
    team: &str,
    member: &str,
    agent_id: &str,
) -> Result<()> {
    let session = jod.agent(agent_id).await.ok().and_then(|a| a.session_id);
    store.bind_member(team, member, Some(agent_id), session.as_deref())?;
    store.set_member_status(team, member, MemberStatus::Ready)?;
    Ok(())
}

fn default_name(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(5).collect();
    let name = words.join(" ");
    if name.is_empty() {
        "agent".to_string()
    } else if name.chars().count() > 48 {
        format!("{}…", name.chars().take(47).collect::<String>())
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn a_long_name_is_truncated_rather_than_left_unbounded() {
        let name = default_name(&"averyverylongword ".repeat(5));
        assert!(
            name.chars().count() <= 48,
            "got {} chars",
            name.chars().count()
        );
    }

    #[test]
    fn every_harness_arg_maps_to_a_distinct_kind() {
        let kinds: Vec<HarnessKind> = [HarnessArg::Claude, HarnessArg::Opencode, HarnessArg::Agy]
            .into_iter()
            .map(HarnessKind::from)
            .collect();
        assert_eq!(kinds.len(), 3);
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a, b, "two harness args mapped to the same kind");
            }
        }
    }

    #[test]
    fn the_cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    fn extraction() -> Consolidation {
        Consolidation::new("work", Provenance::OwnerChat, "…the conversation…")
    }

    const ONE_FACT: &str =
        r#"{"scope":"work","subject":"reljod","predicate":"prefers","object":"linear for tasks"}"#;

    #[test]
    fn a_dry_run_writes_nothing_at_all() {
        let store = Store::in_memory().unwrap();
        settle_consolidation(&store, &extraction(), ONE_FACT, true).unwrap();
        assert!(
            store.facts_about("reljod").unwrap().is_empty(),
            "a dry run that wrote anything would be the worst bug this command has"
        );
    }

    #[test]
    fn dropping_the_dry_run_flag_is_what_writes_the_facts() {
        let store = Store::in_memory().unwrap();
        settle_consolidation(&store, &extraction(), ONE_FACT, false).unwrap();
        let believed = store.facts_about("reljod").unwrap();
        assert_eq!(believed.len(), 1);
        assert_eq!(believed[0].object, "linear for tasks");
        // Read from a conversation, so it is an agent's conclusion — never the
        // owner's own word, whatever the transcript said.
        assert_eq!(believed[0].origin, Origin::Agent);
    }

    #[test]
    fn a_dry_run_says_which_lines_would_replace_a_belief_and_which_are_new() {
        let store = Store::in_memory().unwrap();
        store
            .remember(NewFact::new("reljod", "prefers", "notion for tasks").in_scope("work"))
            .unwrap();
        let batch = extraction().parse(ONE_FACT);
        assert_eq!(
            would_do(&store, &batch.facts[0]).unwrap(),
            "supersedes “notion for tasks”"
        );
        assert_eq!(
            would_do(&store, &NewFact::new("jod", "runs", "on a vps").in_scope("work")).unwrap(),
            "new"
        );
    }

    #[test]
    fn an_extraction_never_reads_another_models_thinking() {
        let material = transcript_material(&[
            PortableMessage {
                role: "user".into(),
                text: "where do tasks go?".into(),
                tool_name: None,
                tool_input: None,
                at_ms: 1,
            },
            PortableMessage {
                role: "thinking".into(),
                text: "maybe he uses notion".into(),
                tool_name: None,
                tool_input: None,
                at_ms: 2,
            },
            PortableMessage {
                role: "assistant".into(),
                text: "linear".into(),
                tool_name: None,
                tool_input: None,
                at_ms: 3,
            },
        ]);
        assert!(
            !material.contains("maybe he uses notion"),
            "reasoning is speculation, and a fact extracted from it was never asserted"
        );
        assert_eq!(material, "user: where do tasks go?\nassistant: linear");
    }
}
