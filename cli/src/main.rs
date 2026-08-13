//! `jod` — the command line over the agent harnesses.
//!
//! Jod does not answer prompts. It hands them to a harness (Claude Code,
//! OpenCode, AGY), runs that harness inside its own tmux session, and turns the
//! harness's output into one event stream that every command here renders.

mod mcp_cmd;
mod render;
mod render_time;
mod tui;
mod update;
mod version;
mod voice;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use jod_core::consolidate::{Consolidation, Provenance};
use jod_core::conversation::PortableMessage;
// The main chat's one hand-off, shared with the TUI's `/main` and the Telegram
// bridge. It lives in `core` because the bridge lives there too.
pub(crate) use jod_core::orchestrator::hand_to_orchestrator;
use jod_core::service::RunConversation;
use jod_core::store::{NewFact, Origin, Store};
use jod_core::team::MemberStatus;
use jod_core::{
    AgentEnvelope, AgentEvent, HarnessKind, Jod, PermissionPolicy, Resume, SpawnRequest,
};
use std::path::PathBuf;

/// How many runs `jod ls` reads back out of the database before deciding what
/// to print. Deliberately larger than the row cap: the read-back is what makes
/// a run *visible at all*, so it has to reach past the rows a screenful shows.
const LS_READ_BACK: usize = 200;

/// "No limit", for the paths that take one anyway. SQLite receives a limit as a
/// signed integer, so `usize::MAX` would arrive as `-1`; this is the largest
/// value that survives the trip meaning what it says.
const ALL_ROWS: usize = i64::MAX as usize;

#[derive(Parser)]
#[command(
    name = "jod",
    about = "Delegate to an agent harness and watch it work.",
    // Not bare `version`: `jod 0.1.0` cannot tell a fresh build from a stale
    // copy on `$PATH`. → `version::LONG_VERSION`
    version = version::LONG_VERSION
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
        #[arg(short, long, value_parser = parse_permission_arg, default_value = "auto")]
        permission: PermissionPolicy,
        /// Continue the most recent conversation instead of starting one.
        #[arg(short = 'C', long = "continue", conflicts_with = "session")]
        continue_last: bool,
        /// Continue one specific conversation by its harness session id.
        #[arg(short, long)]
        session: Option<String>,
        /// Return as soon as the agent is launched instead of waiting for it.
        #[arg(long)]
        detach: bool,
        /// Watch this run for signs of life, and reap it if it wedges.
        ///
        /// For work measured in hours. A run that stops producing output for
        /// longer than its stall window is stopped and marked failed by the
        /// scheduler, rather than sitting there looking busy for ever. Needs
        /// `jod daemon` to be running — the sweep happens on its tick.
        #[arg(long)]
        watch: bool,
        /// How long this run may go silent before it counts as stalled.
        ///
        /// Minutes. Defaults to 20, which is deliberately generous: killing a
        /// slow run costs work somebody waited for, while noticing a wedged one
        /// late costs an idle process.
        #[arg(long, value_name = "MINUTES", requires = "watch")]
        stall_after: Option<i64>,
        /// Emit raw event JSON, one per line, instead of formatted output.
        #[arg(long)]
        json: bool,
        /// Show the agent's thinking as it streams.
        #[arg(long)]
        thinking: bool,
    },
    /// List the agents this process knows about, newest first.
    Ls {
        /// How many rows to print. The newest ones, because a run still going
        /// is at that end and it is the row worth reading.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Every run on the box, however many that is.
        #[arg(long)]
        all: bool,
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
        /// Which harness to open on. Defaults to your stored preference.
        //
        // The `Option` is load-bearing rather than decorative: the TUI stores a
        // preferred harness, and can only defer to it if it can tell "not
        // given" from "given the value that happens to be the default". Clap
        // collapses those two the moment a flag has a `default_value`, so the
        // flag has none and the default lives at the point of use.
        #[arg(short = 'H', long, value_enum)]
        harness: Option<HarnessArg>,
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        /// plan, ask, edits or auto. Also the ceiling Tab may not raise past.
        //
        // `Option` for the same reason as `--harness`, with more at stake: an
        // explicit flag is a ceiling and saying nothing is not, so "the user
        // said auto" and "the user said nothing" must not be one value.
        #[arg(short, long, value_parser = parse_permission_arg)]
        permission: Option<PermissionPolicy>,
        /// Pick up the last conversation instead of starting a new one.
        #[arg(short = 'C', long = "continue")]
        continue_last: bool,
        /// Watch a team: Ctrl-G w shows its members and its task board.
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
    /// The main chat — the one conversation that is always there.
    ///
    /// Send it an instruction and it decides who does the work: an agent
    /// already running, a fresh one, a schedule, or a goal. It never does the
    /// work itself, so it comes back to you immediately rather than when the
    /// job is finished — which is the point, because the moment you most want
    /// to ask for something else is while something is already running.
    ///
    /// With no instruction it shows the chat: what you last said, what it
    /// decided, and what that set in motion.
    Main {
        /// What you want done. Omit to read the chat instead.
        instruction: Vec<String>,
        /// Wait for the orchestrator's reply instead of returning at once.
        /// It still does not wait for the work it delegates.
        #[arg(long)]
        wait: bool,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// How many exchanges to show when reading the chat.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// The decision rail, from the command line.
    ///
    /// The same cards the terminal shows, answered the same way — which is the
    /// point: a blocker raised at midnight is answerable over SSH from a phone
    /// without opening the full-screen interface.
    Card {
        #[command(subcommand)]
        what: CardCommand,
    },
    /// The directories a conversation may work in.
    ///
    /// A session can be pointed at several repositories at once. Exactly one of
    /// them is ever writable — a worktree the session claimed — and the real
    /// checkout stays beside it, readable.
    Root {
        #[command(subcommand)]
        what: RootCommand,
    },
    /// Credentials an agent can use and cannot read.
    ///
    /// Values live outside every repository at owner-only permissions, are
    /// injected into the agent's environment at spawn, and are scrubbed out of
    /// everything it prints. The agent is told a *name*, so a missing key
    /// blocks one test rather than a session.
    Secret {
        #[command(subcommand)]
        what: SecretCommand,
    },
    /// The slash commands and skills the repositories on this box define.
    ///
    /// Jod reimplements none of them. It finds them, says which harness's
    /// convention each one follows, and forwards the name to a harness that can
    /// resolve it — measured per harness in `docs/harness-support.md`, never
    /// assumed.
    Commands {
        #[command(subcommand)]
        what: CommandsCommand,
    },
    /// Works: one intent, spanning several sessions.
    Work {
        #[command(subcommand)]
        what: WorkCommand,
    },
    /// The repositories work happens in — the catalog an instruction that
    /// names none is resolved against.
    ///
    /// Worth filling once by hand: until a repository is listed, saying "let's
    /// fix this" has nothing to resolve to and every instruction about it has
    /// to spell the path out.
    Project {
        #[command(subcommand)]
        what: ProjectCommand,
    },
    /// Dictation: which model transcribes you, and whether it runs here.
    Voice {
        #[command(subcommand)]
        what: VoiceCommand,
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
    /// Work that fires when GitHub says something happened.
    ///
    /// A rule is a match — source, repo, event, optional action and conditions
    /// — and a prompt template to run when it matches. The receiver, the
    /// signature check, the delivery ledger and the TUI's rule list were all
    /// built before anything could *create* one, so the table stayed empty and
    /// the whole path was unreachable. This is that missing verb.
    Webhook {
        #[command(subcommand)]
        what: WebhookCommand,
    },
    /// Jod on a phone.
    ///
    /// `HttpBot`, the poller, the rate-limit backoff, the allowlist and the
    /// refusal record were all built and tested, and `Bridge` — the piece that
    /// joins them to Jod — was never constructed anywhere. This is that
    /// missing entry point.
    Telegram {
        #[command(subcommand)]
        what: TelegramCommand,
    },
    /// Let a schedule decide for itself whether it is worth waking a model.
    ///
    /// A monitor is a probe and a hash attached to one schedule. Unchanged
    /// bytes suppress the run entirely; changed bytes run it with a diff in
    /// front of the prompt. `--no-agent` inverts the deal: the script *is* the
    /// job, its stdout is the result, and no model runs at all.
    ///
    /// The tick has asked the monitor since it was written. Nothing could
    /// write one — the same missing verb `jod webhook` was, one table over.
    Monitor {
        #[command(subcommand)]
        what: MonitorCommand,
    },
    /// Proof of what Jod owed people, and whether they got it.
    ///
    /// A run that finished and a reply that arrived are two different facts,
    /// and until this command the second was answerable only by opening
    /// SQLite. The ledger has recorded it all along.
    Ledger {
        #[command(subcommand)]
        what: LedgerCommand,
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
        #[arg(short, long, value_parser = parse_permission_arg, default_value = "auto")]
        permission: PermissionPolicy,
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
        /// `install` registers this server with the harnesses *you* launch.
        /// Absent, `jod mcp` is the server itself — which is how every config
        /// Jod writes invokes it, so this must stay optional.
        #[command(subcommand)]
        cmd: Option<McpCommand>,
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
        #[arg(long, value_parser = parse_permission_arg, default_value = "accept_edits")]
        max_permission: PermissionPolicy,
    },
    /// Update this machine's Jod binaries to the newest patch release.
    ///
    /// Rebuilds from the checkout `install.sh` left behind and renames the new
    /// binaries over the old ones, so the console can update itself while it
    /// is running. Patch-only by default: a minor or major move would change
    /// what the daemon and the TUI are, and is asked for explicitly with
    /// `--version`.
    Update {
        /// Say what an update would do, and change nothing.
        #[arg(long)]
        check: bool,
        /// Install a specific version or branch instead of the newest patch.
        #[arg(long)]
        version: Option<String>,
        /// Rebuild and reinstall even when already at the target commit.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Register Jod's MCP server with the harnesses on this machine, so a
    /// session *you* start holds Jod's tools too.
    ///
    /// Jod already hands its own spawned runs an MCP config on the command
    /// line. Nothing hands one to the `claude` you type in a repo, so that
    /// session cannot schedule, delegate or remember — which reads from the
    /// chair like the feature not existing rather than like a missing config.
    ///
    /// Safe to re-run: it rewrites its own entry, leaves every other server and
    /// setting alone, and refuses a config it cannot parse.
    Install {
        /// How much of Jod these sessions get. Defaults to the full set,
        /// because the session on the other end is one a person opened and is
        /// watching — the opposite of the unattended case, which is pinned to
        /// read-only where it is spawned and cannot be widened from here.
        #[arg(long, default_value = "orchestrate", value_parser = parse_access_arg)]
        access: jod_core::harness::ToolAccess,
        /// Just this harness, instead of every installed one.
        #[arg(short = 'H', long, value_enum)]
        harness: Option<HarnessArg>,
        /// Include harnesses that are not installed, which writes a config
        /// directory for a program that is not on this machine.
        #[arg(long)]
        all: bool,
        /// Say what would be written, and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CardCommand {
    /// Cards, most pressing first. Open ones only, unless you say otherwise.
    Ls {
        /// Only this conversation's. A prefix of its id is enough.
        #[arg(short, long)]
        conversation: Option<String>,
        /// This conversation's *and every session below it* — what the
        /// orchestrator's rail shows. Cascade is upward only.
        #[arg(long, conflicts_with = "conversation")]
        subtree: Option<String>,
        #[arg(short, long)]
        work: Option<String>,
        #[arg(short, long, value_enum)]
        kind: Option<KindArg>,
        #[arg(short, long, value_enum, default_value_t = StatusArg::Open)]
        status: StatusArg,
        /// Only the ones that stopped a run.
        #[arg(short, long)]
        blocking: bool,
        /// Full-text match over title, body and answer.
        #[arg(short, long)]
        text: Option<String>,
        #[arg(long, value_enum, default_value_t = SortArg::Pressing)]
        sort: SortArg,
        #[arg(short, long, default_value_t = 30)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// One card in full, with its options and where it came from.
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Answer it. The agent is told at the end of its current turn, never
    /// mid-turn — see `jod card show` for whether it has heard yet.
    ///
    /// For a secret card this asks for the value on the terminal and writes it
    /// straight through to the secret store; it never appears in an argument,
    /// in this database, or in the agent's context.
    Answer {
        id: i64,
        /// Pick one of the card's numbered options.
        #[arg(short = 'o', long)]
        option: Option<usize>,
        /// Answer in prose. Combined with --option when both are given.
        text: Vec<String>,
    },
    /// Read it and deliberately leave it unanswered. The agent is told nothing,
    /// which is the difference between this and an empty answer.
    Dismiss { id: i64 },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum KindArg {
    Decision,
    Question,
    Secret,
}

impl From<KindArg> for jod_core::cards::CardKind {
    fn from(a: KindArg) -> Self {
        use jod_core::cards::CardKind;
        match a {
            KindArg::Decision => CardKind::Decision,
            KindArg::Question => CardKind::Question,
            KindArg::Secret => CardKind::Secret,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum StatusArg {
    Open,
    Answered,
    Dismissed,
}

impl From<StatusArg> for jod_core::cards::Status {
    fn from(a: StatusArg) -> Self {
        use jod_core::cards::Status;
        match a {
            StatusArg::Open => Status::Open,
            StatusArg::Answered => Status::Answered,
            StatusArg::Dismissed => Status::Dismissed,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum SortArg {
    /// Blocking first, then importance, then newest.
    Pressing,
    Importance,
    Created,
    Updated,
}

impl From<SortArg> for jod_core::cards::Sort {
    fn from(a: SortArg) -> Self {
        use jod_core::cards::Sort;
        match a {
            SortArg::Pressing => Sort::Pressing,
            SortArg::Importance => Sort::Importance,
            SortArg::Created => Sort::Created,
            SortArg::Updated => Sort::Updated,
        }
    }
}

#[derive(Subcommand)]
enum RootCommand {
    /// Every directory this conversation may work in, in its own order.
    Ls {
        #[arg(short, long)]
        conversation: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Add a directory, read-only.
    ///
    /// Read-only is not a flag you can lift here: a root becomes writable only
    /// by a session claiming a worktree, which is what keeps a run's
    /// half-finished state off your checkout.
    Add {
        path: PathBuf,
        #[arg(short, long)]
        conversation: Option<String>,
    },
    /// Drop a directory. Its files are not touched.
    Rm {
        path: PathBuf,
        #[arg(short, long)]
        conversation: Option<String>,
    },
}

#[derive(Subcommand)]
enum SecretCommand {
    /// The names a run here would be given, and nothing else about them.
    ///
    /// With no scope, the globals. With `--work`, what a session on that work
    /// resolves — narrower scopes overriding wider ones by name, exactly as
    /// the spawn path resolves them.
    Ls {
        #[arg(short, long)]
        work: Option<String>,
        #[arg(short, long)]
        conversation: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Store a value.
    ///
    /// **The value is never an argument.** Anything on a command line is
    /// world-readable through `/proc` for as long as the process lives, and it
    /// is in your shell history for ever afterwards. This asks for it on the
    /// terminal with the echo off, or reads it from stdin when you pipe one in
    /// — `printf %s "$KEY" | jod secret set NAME --global`, with `printf` and
    /// not `echo`, because a trailing newline becomes part of the credential.
    Set {
        /// A legal environment variable name: a letter or underscore, then
        /// letters, digits and underscores.
        name: String,
        /// What it is for. Shown to the agent, which is how it knows which
        /// variable to reach for.
        #[arg(long, default_value = "")]
        hint: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    /// Forget a value. The file holding it is removed.
    Rm {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

/// Who a secret is for — asked for explicitly, every time.
///
/// Deliberately without a default. The scope is the blast radius if the value
/// leaks, and a default would make the widest choice the quiet one: `--global`
/// hands a key to every session on the box, and that should be a thing somebody
/// typed rather than a thing they omitted.
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct ScopeArgs {
    /// Every session on this machine.
    #[arg(long)]
    global: bool,
    /// One work. The scope to prefer: a key given for one project is not then
    /// handed to every session on the box.
    #[arg(long)]
    work: Option<String>,
    /// One conversation.
    #[arg(long)]
    conversation: Option<String>,
}

impl ScopeArgs {
    fn resolve(&self) -> (jod_core::secrets::Scope, String) {
        use jod_core::secrets::Scope;
        match (&self.work, &self.conversation) {
            (Some(work), _) => (Scope::Work, work.clone()),
            (_, Some(conversation)) => (Scope::Conversation, conversation.clone()),
            // The group is `required = true` and mutually exclusive, so this is
            // `--global` and clap has already refused every other shape.
            _ => (Scope::Global, String::new()),
        }
    }
}

#[derive(Subcommand)]
enum CommandsCommand {
    /// Every command and skill found under a conversation's roots and in your
    /// own config.
    ///
    /// Rescans by default, because a listing that answered from a stale cache
    /// would offer a command somebody deleted this morning. `--cached` reads
    /// what was last found instead, which is what the palette does on every
    /// keystroke.
    Ls {
        /// Whose roots to scan. Defaults to the main chat's.
        #[arg(short, long)]
        conversation: Option<String>,
        /// Scan these directories instead of a conversation's roots.
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        /// Only what this harness can resolve. A command is offered to the
        /// harness whose convention it follows and to no other — Jod does not
        /// forward one across conventions.
        #[arg(short = 'H', long, value_enum)]
        harness: Option<HarnessArg>,
        /// Answer from the cache rather than looking at the disk.
        #[arg(long)]
        cached: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum VoiceCommand {
    /// Is dictation ready, and what would it use?
    ///
    /// Answers the question `Ctrl-V` would otherwise answer by failing: whether
    /// there is a recorder, an engine and a model on this machine.
    Check,
    /// The models that can transcribe you, and which are downloaded.
    ///
    /// English-only builds are deliberately not offered — they cannot
    /// represent Tagalog, so they would delete half of what you said.
    Models,
    /// Download a model so transcription runs on this machine.
    ///
    /// Nothing leaves the laptop after this: no key, no network, no
    /// per-utterance cost.
    Download {
        /// Which one. Defaults to the recommended model.
        name: Option<String>,
    },
    /// Transcribe with this model from now on.
    Use { name: String },
    /// Go back to transcribing over the network.
    ///
    /// For a machine with no model on it. Needs `OPENROUTER_API_KEY`.
    Cloud,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// The catalog, most recently worked in first.
    Ls {
        /// Include finished and abandoned projects.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Put a repository in the catalog.
    ///
    /// Adding one that is already listed updates it rather than duplicating
    /// it, so this is also how you rename a project or extend its aliases.
    Add {
        /// The checkout. Defaults to the current directory.
        path: Option<PathBuf>,
        /// What you call it out loud. Defaults to the directory's name.
        #[arg(long)]
        name: Option<String>,
        /// Another thing you say for it — "the tetris thing", "my agent".
        /// Repeatable. These are what a dictated instruction is matched
        /// against, so they should be what you actually say, not what is tidy.
        #[arg(long = "alias")]
        aliases: Vec<String>,
        /// One line about it, carried into every main-chat turn. Keep it short.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Stop a project being inferred, without forgetting it.
    ///
    /// A paused or archived project can still be named explicitly; it just
    /// stops competing for an offhand mention. Nothing is deleted — the point
    /// of a catalog is to still answer "what was that repo called" later.
    Archive { name: String },
    /// Put an archived or paused project back in play.
    Restore { name: String },
}

#[derive(Subcommand)]
enum WorkCommand {
    /// Works, most recently touched first. Live ones by default.
    Ls {
        #[arg(long, conflicts_with = "closed")]
        all: bool,
        #[arg(long)]
        closed: bool,
        #[arg(long)]
        json: bool,
    },
    /// One work: its sessions, its board, its leases and its open cards.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// End it now, without waiting for its board to empty.
    ///
    /// Closing destroys nothing — the record, the tree and the worktrees all
    /// stay. A work whose sessions are still running becomes *finishing*
    /// rather than closed.
    Close { id: String },
    /// Remove the work and every session in it: transcripts, cards, bus
    /// traffic. Its worktrees and branches are left exactly where they are.
    Delete { id: String },
    /// The worktrees works have claimed, and what state each one is in.
    Leases {
        /// One work's. Omit for every lease Jod knows about.
        id: Option<String>,
        /// Only those whose work has been deleted — the ones nothing else will
        /// ever mention again.
        #[arg(long)]
        orphaned: bool,
        #[arg(long)]
        json: bool,
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
    /// Delete a conversation and everything it holds: its transcript, its
    /// cards, its roots, its queued answers.
    ///
    /// Refuses two things, and both refusals are the point. The main chat is
    /// the one conversation that is always there. And a session belonging to a
    /// work can only go when the work does — removing one on its own would
    /// leave a tree pointing at a session that is gone.
    Rm { id: String },
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
enum WebhookCommand {
    /// Every rule, and whether it is armed.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Add a rule.
    Add {
        /// A short name, used everywhere else to refer to it.
        name: String,
        /// What to ask the agent to do. `{{placeholders}}` — `{{title}}`,
        /// `{{body}}`, `{{author}}`, `{{branch}}`, `{{number}}` — are filled
        /// from the payload as *quoted JSON data*, never as bare text.
        prompt: String,
        /// `owner/repo`, or `*` for every repository the receiver hears from.
        #[arg(short, long, default_value = jod_core::webhook::ANY_REPO)]
        repo: String,
        /// The GitHub event name: `pull_request`, `issues`, `push`.
        #[arg(short, long)]
        event: String,
        /// One action of that event — `opened`, `closed`. Omitted matches all.
        #[arg(short, long)]
        action: Option<String>,
        /// Require *all* of these labels. Repeat the flag for each.
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        author: Option<String>,
        /// Match only drafts (`true`) or only non-drafts (`false`).
        #[arg(long)]
        draft: Option<bool>,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        /// Add it disarmed, to check what it would match first.
        #[arg(long)]
        paused: bool,
    },
    /// Arm a rule.
    Enable { name: String },
    /// Disarm a rule without deleting it.
    Disable { name: String },
    /// Delete a rule. Its past deliveries survive, with the rule id cleared.
    Rm { name: String },
    /// What has arrived, newest first.
    Deliveries {
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TelegramCommand {
    /// Poll Telegram and delegate what arrives. Runs until stopped.
    Serve {
        /// Where an agent launched from a chat runs.
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
    },
    /// Who the bot is, and who has tried to talk to it.
    ///
    /// Works *before* an allowlist exists, which is the only reason it can be
    /// used to build one: `serve` refuses to start without
    /// `JOD_TELEGRAM_ALLOWED_USERS`, and nothing else tells you the numeric id
    /// that belongs in it. Message the bot, run this, copy the id.
    Whoami,
}

/// The three questions a person actually has about a message Jod owed.
#[derive(Subcommand)]
enum LedgerCommand {
    /// What is still owed. Everything, with `--all`.
    Ls {
        /// Settled rows too — delivered and given up on.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// One message in full: what it said, who holds it, what became of it.
    Show {
        /// The `message_key` a listing prints, or the row id.
        what: String,
        #[arg(long)]
        json: bool,
    },
    /// What was given up on — the record that somebody was owed something
    /// they never got.
    Failed {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MonitorCommand {
    /// Every monitor, and what it has seen.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Attach a monitor to a schedule, replacing any it already had.
    Set {
        /// The schedule's name, as `jod schedule ls` shows it.
        schedule: String,
        /// A shell command whose stdout is what gets watched.
        #[arg(long, conflicts_with = "url")]
        command: Option<String>,
        /// A URL whose response body is what gets watched.
        #[arg(long)]
        url: Option<String>,
        /// Where a command runs. Defaults to the schedule's own directory, so
        /// a probe and the run it gates see the same tree.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// The script is the whole job: its stdout is the result and no model
        /// is ever woken. Empty stdout means stay quiet.
        #[arg(long)]
        no_agent: bool,
    },
    /// Detach the monitor, leaving the schedule to fire the ordinary way.
    Rm { schedule: String },
    /// Run the probe now and say what a tick would make of it.
    ///
    /// Records nothing and starts nothing, so the baseline a real tick compares
    /// against is left exactly where it was.
    Check {
        schedule: String,
        /// Keep what this check saw, making it the baseline the next one is
        /// compared against — "start watching from here".
        ///
        /// Without it an operator has no way to arm a baseline deliberately;
        /// they must wait for the daemon's first tick to set one. It still
        /// starts no agent: this records what was seen, never acts on it.
        #[arg(long)]
        record: bool,
    },
    /// What a monitor has actually seen, newest first.
    Log {
        schedule: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
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
        /// The team to join. A team exists once it has a member, so a name you
        /// have not used before starts a new one.
        team: String,
        /// What this teammate is called on the bus. Every other command spells
        /// it the same way — `--to`, `inbox`, `claim` — and its runs are named
        /// `<team>-<member>`.
        member: String,
        /// Which agent harness runs this member's turns.
        #[arg(short = 'H', long, value_enum, default_value_t = HarnessArg::Claude)]
        harness: HarnessArg,
        /// One line saying what this member is for. Free text, shown beside the
        /// name by `jod team show`.
        #[arg(short, long, default_value = "")]
        role: String,
    },
    /// Put a task on the team's board.
    Task {
        /// Whose board this goes on, as `jod team list` names it.
        team: String,
        /// The short slug that names this task from here on.
        ///
        /// You invent it — nothing generates one, and it need only be unique
        /// across the boards. It is what `jod team claim` and `jod team done`
        /// take, what teammates call the task in messages, and
        /// `jod team show <TEAM>` prints it back in full, which is the only
        /// place to read one you have forgotten. Re-using an id already on a
        /// board leaves that task exactly as it was.
        id: String,
        /// What the task is, in plain words.
        ///
        /// Everything after the id is joined with spaces, so quoting is
        /// optional. Left out, the id is used as the title.
        title: Vec<String>,
    },
    /// Take ownership of a task. Reports whether this member won the race.
    Claim {
        /// The task to take, as `jod team show <TEAM>` lists it. An id that is
        /// on no board is refused rather than created.
        id: String,
        /// Which teammate takes it, by the name it joined under. The first
        /// caller wins; a later one is refused and exits non-zero, which is
        /// how a script tells "mine" from "someone else got there".
        member: String,
    },
    /// Mark a task finished.
    Done {
        /// The task to close, as `jod team show <TEAM>` lists it. It does not
        /// have to have been claimed first.
        id: String,
    },
    /// Send a message. Without --to it goes to every member but the sender.
    Msg {
        /// Whose bus to put this on. Only that team's members can receive it.
        team: String,
        /// Which member is sending, by the name it joined under. The recipient
        /// is shown this name, and a broadcast skips it.
        #[arg(short, long)]
        from: String,
        /// Deliver to this one member. Left out, every other member gets a copy.
        #[arg(short, long)]
        to: Option<String>,
        /// What to say. Every word after the team is joined with spaces, so
        /// quoting is optional.
        text: Vec<String>,
    },
    /// Read a member's waiting messages and mark them delivered.
    Inbox {
        /// Whose bus to read from.
        team: String,
        /// Whose mail to read, by the name it joined under.
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
        /// Which team to sweep. Every idle member of it holding mail is resumed.
        team: String,
        /// Where the resumed agents run. Defaults to your home directory.
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        /// How much the resumed agents may do unattended: plan, ask, edits or
        /// auto.
        #[arg(short, long, value_parser = parse_permission_arg, default_value = "auto")]
        permission: PermissionPolicy,
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
        /// Which team the member is on.
        team: String,
        /// Which member to start, by the name it joined under. It must already
        /// be on the team — `jod team join` puts it there.
        member: String,
        /// The first thing to say to it. Joined with spaces, so quoting is
        /// optional.
        prompt: Vec<String>,
        /// Where the agent runs. Defaults to your home directory.
        #[arg(short, long)]
        cwd: Option<PathBuf>,
        /// How much the agent may do unattended: plan, ask, edits or auto.
        #[arg(short, long, value_parser = parse_permission_arg, default_value = "auto")]
        permission: PermissionPolicy,
        /// Return as soon as the agent is launched, instead of waiting for it.
        #[arg(short, long)]
        detach: bool,
    },
    /// Who is on the team, and what is on its board.
    Show {
        /// Which team to describe, as `jod team list` names it.
        team: String,
    },
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

/// How much an agent may do to the *machine*, parsed by core rather than
/// re-declared here.
///
/// This used to be a `ValueEnum` listing the levels a second time, and it
/// drifted exactly as the note on `parse_access_arg` below predicts its
/// neighbour would. A fourth mode was added to `PermissionPolicy` and this copy
/// never heard about it: `--permission plan` was not something you could ask
/// for, and the default stayed on the old `ask` after the real default had
/// moved to `auto`. So every `jod run` went out asking for an approval nobody
/// was there to give, and reported back that it was waiting for one.
///
/// One parser, in core, accepting every spelling — including the harnesses' own
/// (`manual`, `auto`, `bypass_permissions`).
fn parse_permission_arg(s: &str) -> Result<PermissionPolicy, String> {
    jod_core::mcp::parse_permission(s).ok_or_else(|| {
        let names: Vec<&str> = PermissionPolicy::ALL.iter().map(|m| m.label()).collect();
        format!("`{s}` is not a permission mode — {}", names.join(", "))
    })
}

/// How much of Jod itself an agent may reach over MCP.
///
/// A separate axis from `PermissionPolicy`, which bounds what the agent may do to
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

/// Read `.env` into the process environment, without overriding anything the
/// caller actually exported.
///
/// A real environment variable always wins. A file that could silently replace
/// what an operator typed on the command line would make `JOD_TELEGRAM_TOKEN=… jod …`
/// a lie, and the failure would look like the wrong token rather than the wrong
/// precedence.
///
/// Deliberately not a dependency. The format Jod needs is `KEY=value` lines with
/// `#` comments — `dotenvy` brings variable interpolation, multi-line values and
/// export syntax that nothing here uses, for a file that holds two secrets.
fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().strip_prefix("export ").unwrap_or(key.trim());
        // Quotes are stripped because writing a token bare and writing it
        // quoted are both ordinary, and a token carrying a literal `"` fails
        // authentication with a message that blames the token.
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if std::env::var_os(key).is_none() {
            // SAFETY: single-threaded, before any task is spawned.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv();
    let cli = Cli::parse();

    // Handled before the store is opened, deliberately: an update is how you
    // recover a build whose store it cannot open, and a command that needed a
    // working database to fix a broken one would be no use on the day it was
    // needed.
    if let Command::Update {
        check,
        version,
        force,
    } = cli.command
    {
        let outcome = update::run(check, version, force)?;
        if outcome.replaced {
            // Said here rather than by the installer, because only this
            // process knows it is *itself* the binary that just moved.
            println!("Anything already running is still the previous build — restart it.");
        }
        return Ok(());
    }

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
            watch,
            stall_after,
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

            let resume = match session {
                Some(id) => Resume::Session(id),
                None if continue_last => Resume::Last,
                None => Resume::Fresh,
            };
            // What this run may reach, folded in from the conversation it
            // continues — the same move `prefer_conversation_settings` makes
            // for the model and the permission, and for the same reason: these
            // are facts about the thread, not about the command line.
            //
            // Secret *names*, never values. The value is read at exec time by
            // the supervisor, out of a file only its owner can open; nothing in
            // this process, this argv or `spawn.json` ever holds one.
            let (roots, secrets) = match jod.store() {
                Some(store) => grants_for_run(store, &resume, harness.into())?,
                None => (Vec::new(), Vec::new()),
            };
            // Where the session *lives*, not where the command was typed.
            //
            // A resumed run in the wrong directory is not a cosmetic mistake.
            // OpenCode resolves its project from `--dir` and scopes sessions to
            // it, and `--session <id>` naming a session from another project
            // does not error — **it hangs, silently, for ever**. Measured:
            // `opencode run --format json --dir <other> --session <id> "…"`
            // emitted nothing at all on either stream and was still running
            // when killed at 90 seconds, while the identical command with the
            // session's own `--dir` answered in under a second. That is what
            // made a resumed OpenCode run in the parity suite produce one bare
            // `finished` event and never terminate — the directory, not the
            // session id, which was correct all along.
            //
            // Jod knows the answer, so it uses it. An explicit `--cwd` still
            // wins: somebody who names a directory means it, and being told
            // where their session lives is [`continuing_conversation`]'s job
            // rather than this one's.
            let cwd = match (cwd, jod.store()) {
                (Some(given), _) => given,
                (None, Some(store)) => session_cwd(store, &resume, harness.into())?
                    .unwrap_or_else(jod_core::service::default_cwd),
                (None, None) => jod_core::service::default_cwd(),
            };

            let req = SpawnRequest {
                name: name.unwrap_or_else(|| default_name(&prompt)),
                harness: harness.into(),
                prompt,
                system: None,
                cwd,
                model,
                permission: permission.into(),
                resume,
                roots,
                secrets,
                // `jod run` is one task, not an orchestrator. Handing it Jod's
                // own verbs would let a one-liner create schedules that spend
                // money nightly, which is a decision that should be made on
                // purpose rather than inherited from a default.
                tools: None,
                ..SpawnRequest::default()
            };

            // Subscribe *before* spawning, so no early event is missed.
            let events = jod.subscribe();
            let agent = jod.spawn_agent(req).await?;

            if watch {
                // Registered after the spawn, because a heartbeat for a run
                // that failed to start would be a row watching nothing — and
                // the cascade only cleans up rows whose run exists.
                let store = jod.store().context("watching a run needs a store")?;
                let mut hb = jod_core::heartbeat::Heartbeat::starting(
                    &agent.id,
                    jod_core::heartbeat::Watching::Run,
                    chrono::Utc::now().timestamp_millis(),
                );
                if let Some(minutes) = stall_after {
                    hb = hb.with_stall_ms(minutes.saturating_mul(60_000));
                }
                store.watch_run(&hb)?;
                render::watching(&agent.id, hb.stall_ms);
            }

            if detach {
                render::launched(&agent);
                return Ok(());
            }
            render::launched_waiting(&agent);
            let code = render::stream(events, &agent.id, json, thinking).await;
            std::process::exit(code);
        }

        Command::Ls { limit, all, json } => {
            // A fresh process knows nothing until it reads the database back.
            // Read back more than will be printed: a run started days ago and
            // still going sits far down the list, and dropping it is the whole
            // failure this cap exists to avoid.
            let read_back = if all { ALL_ROWS } else { LS_READ_BACK.max(limit) };
            jod.rehydrate(read_back).await?;
            let cap = if all { ALL_ROWS } else { limit };
            let (agents, known) = jod.recent_agents(cap).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&agents)?);
            } else {
                // The database knows about runs this process never read back,
                // so it gives the truthful "how many were hidden"; `known` is
                // the floor for a process running without one.
                render::agents(&agents, jod.run_count()?.max(known));
            }
        }

        Command::Watch { id, json, thinking } => {
            // Subscribe before rehydrating: rehydrate starts the followers that
            // produce the live events, and one that fired first would be lost.
            let events = jod.subscribe();
            jod.rehydrate(200).await?;
            let id = resolve_run(&jod, &id).await?;
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
            let id = resolve_run(&jod, &id).await?;
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
            // Here rather than inside `Daemon::run`, and the difference is not
            // stylistic. Registration writes to `~/.claude.json` and its
            // siblings — files this program does not own — so it must be an
            // effect of *someone starting the daemon*, never of constructing or
            // driving one. As a side effect of the library's run loop it also
            // fired from the test suite, which duly registered a `cargo test`
            // binary from `target/debug/deps/` as the machine's MCP server.
            // A long-running binary's entrypoint is a place tests do not reach.
            jod_core::mcp_install::ensure_registered();
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

        Command::Main {
            instruction,
            wait,
            harness,
            cwd,
            limit,
        } => {
            main_chat(&jod, instruction.join(" "), wait, harness, cwd, limit).await?;
        }
        Command::Card { what } => card_command(&jod, what)?,
        Command::Root { what } => root_command(&jod, what)?,
        Command::Secret { what } => secret_command(&jod, what)?,
        Command::Commands { what } => commands_command(&jod, what)?,
        Command::Work { what } => work_command(&jod, what)?,
        Command::Project { what } => project_command(&jod, what)?,
        Command::Voice { what } => voice_command(&jod, what).await?,
        Command::Conv { what } => conv_command(&jod, what)?,
        Command::Schedule { what } => schedule_command(&jod, what)?,
        Command::Webhook { what } => webhook_command(&jod, what)?,
        Command::Monitor { what } => monitor_command(&jod, what)?,
        Command::Ledger { what } => ledger_command(&jod, what)?,
        Command::Telegram { what } => telegram_command(jod, what).await?,
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
                    harness: harness.map(Into::into),
                    team,
                    cwd: console_cwd(cwd),
                    model,
                    permission,
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
                                system: None,
                                cwd: cwd.clone(),
                                model: None,
                                permission: permission.into(),
                                resume: Resume::Session(order.session_id),
                                tools: None,
                                ..SpawnRequest::default()
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
                            system: None,
                            cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
                            model: None,
                            permission: permission.into(),
                            resume: Resume::Fresh,
                            tools: None,
                            ..SpawnRequest::default()
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
            cmd:
                Some(McpCommand::Install {
                    access,
                    harness,
                    all,
                    dry_run,
                }),
            ..
        } => {
            mcp_cmd::install(access, harness.map(Into::into), all, dry_run)?;
        }

        Command::Mcp {
            cmd: None,
            access,
            max_permission,
        } => {
            mcp_cmd::run(jod, access, max_permission.into()).await?;
        }

        // Returned above, before the store was opened — an update must not
        // need a working database to fix a broken one.
        Command::Update { .. } => unreachable!("handled before the store is opened"),
    }

    Ok(())
}

/// Send an instruction to the main chat, or read it.
///
/// Non-blocking by default, and that is the feature rather than an
/// optimisation: a main chat that waited for the work would be unusable
/// exactly when you most want it. It returns as soon as the orchestrator has
/// been *handed* the instruction.
async fn main_chat(
    jod: &Jod,
    instruction: String,
    wait: bool,
    harness: HarnessArg,
    cwd: Option<PathBuf>,
    limit: usize,
) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    let kind: HarnessKind = harness.into();
    let cwd = cwd.unwrap_or_else(jod_core::service::default_cwd);
    let id = store.main_conversation(kind, &cwd.display().to_string())?;
    let now = chrono::Utc::now().timestamp_millis();

    if instruction.trim().is_empty() {
        return show_main_chat(jod, &id, limit, now);
    }

    // `None`: nothing to carry. A harness switch happens in the TUI, which
    // holds the summary on its thread and passes it on the next turn.
    let handed = hand_to_orchestrator(jod, &instruction, kind, cwd, None, "main").await?;
    if let Some((reason, chars)) = handed.compaction_due {
        println!("· the chat is due for compaction ({reason}) — {chars} chars in the live window");
        println!("  `jod conv compact {}` summarises it", &id[..8.min(id.len())]);
    }
    let agent = handed.agent;

    if !wait {
        println!("→ {} · {}", short_id(&agent.id), "handed to the orchestrator");
        println!("  `jod main` reads the chat · `jod watch {}` follows it", short_id(&agent.id));
        return Ok(());
    }
    wait_for_orchestrator(jod, &agent.id).await
}

/// Follow an orchestrator run to its end, for `jod main --wait`.
async fn wait_for_orchestrator(jod: &Jod, run_id: &str) -> Result<()> {
    let mut events = jod.subscribe();
    while let Ok(envelope) = events.recv().await {
        if envelope.agent_id != run_id {
            continue;
        }
        match &envelope.event {
            jod_core::AgentEvent::Message { text } => println!("{text}"),
            jod_core::AgentEvent::ToolCall { name, .. } => println!("  · {name}"),
            jod_core::AgentEvent::Finished { .. } => break,
            _ => {}
        }
    }
    Ok(())
}

/// Read the main chat: what was said, and what it set in motion.
fn show_main_chat(jod: &Jod, id: &str, limit: usize, now: i64) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    let thread = store.live_window(id)?;
    if thread.is_empty() {
        println!("the main chat is empty — `jod main \"<instruction>\"` starts it");
        return Ok(());
    }
    render_time::thread(&thread[thread.len().saturating_sub(limit)..]);

    // What the chat actually caused, which is the question a transcript alone
    // does not answer.
    let done = store.delegations(id, 10)?;
    if !done.is_empty() {
        println!("\nset in motion:");
        for d in done {
            // A run is named by its short id everywhere else in Jod, and this
            // column sat next to a schedule name — so a bare UUID here was both
            // inconsistent and the widest thing on the screen.
            let what = match (d.run_id, d.schedule_name, d.goal_name) {
                (Some(run), _, _) => short_id(&run),
                (_, Some(name), _) | (_, _, Some(name)) => name,
                _ => "—".into(),
            };
            println!(
                "  {} {:<12} {}",
                render_time::when(d.at_ms, now),
                d.kind,
                what
            );
        }
    }
    Ok(())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
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
        ConvCommand::Rm { id } => {
            let id = resolve(&id)?;
            // Unrecoverable, so it says what went rather than "ok". The counts
            // are read before the delete because afterwards there is nothing
            // left to count.
            let messages = store.thread(&id)?.len();
            let (open, _) = store.count_open_cards(&id, false)?;
            store.delete_conversation(&id)?;
            println!(
                "deleted {} — {messages} message(s), {open} unanswered card(s)",
                short_id(&id)
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

// ---- the rail, the roots, the secrets and the works ----------------------

/// Carry out a `jod card …` subcommand.
///
/// Everything here goes through the same [`jod_core::cards::Query`] the
/// terminal rail uses, so a card listed on a phone is the card that is on the
/// screen at home, sorted the same way. A second query builder here would drift
/// within a week — that is the whole reason the store takes a filter rather
/// than offering a function per caller.
fn card_command(jod: &Jod, what: CardCommand) -> Result<()> {
    use jod_core::cards::{Query, Sort};
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        CardCommand::Ls {
            conversation,
            subtree,
            work,
            kind,
            status,
            blocking,
            text,
            sort,
            limit,
            json,
        } => {
            let query = Query {
                conversation_id: conversation
                    .map(|c| resolve_conversation(store, &c))
                    .transpose()?,
                subtree_of: subtree.map(|c| resolve_conversation(store, &c)).transpose()?,
                work_id: work,
                kind: kind.map(Into::into),
                status: Some(status.into()),
                blocking_only: blocking,
                text,
                sort: Sort::from(sort),
                limit: Some(limit),
            };
            let cards = store.cards(&query)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&cards)?);
            } else if cards.is_empty() {
                println!("no cards");
            } else {
                render::cards(&cards, now);
            }
        }
        CardCommand::Show { id, json } => {
            let card = require_card(store, id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&card)?);
            } else {
                render::card(&card, now);
            }
        }
        CardCommand::Answer { id, option, text } => {
            let card = require_card(store, id)?;
            let chosen = match option {
                // One-based, because that is how the rail numbers them and how
                // the card prints them. Refused rather than clamped: answering
                // the wrong option is worse than being told to look again.
                Some(n) => Some(
                    card.options
                        .get(n.checked_sub(1).context("options are numbered from 1")?)
                        .with_context(|| {
                            format!(
                                "card #{id} has {} option(s); `jod card show {id}` lists them",
                                card.options.len()
                            )
                        })?
                        .clone(),
                ),
                None => None,
            };
            let text = text.join(" ");
            let answered = match card.kind {
                jod_core::cards::CardKind::Secret => answer_secret_card(store, &card)?,
                _ => store.answer_card(
                    id,
                    chosen.as_deref(),
                    Some(text.as_str()).filter(|t| !t.is_empty()),
                )?,
            };
            render::card(&answered, now);
            // The asynchrony is stated rather than implied. Somebody who
            // answered ten cards during one turn should not be waiting for
            // something to happen.
            println!(
                "\nqueued for {} — it reaches the agent at the end of its current turn, \
                 not now",
                short_id(&answered.conversation_id)
            );
        }
        CardCommand::Dismiss { id } => {
            store.dismiss_card(id)?;
            println!("card #{id} dismissed — the agent is told nothing");
        }
    }
    Ok(())
}

fn require_card(store: &Store, id: i64) -> Result<jod_core::cards::Card> {
    store
        .card(id)?
        .with_context(|| format!("no card #{id} — `jod card ls` lists them"))
}

/// Answer a secret card by storing the value, never by carrying it.
///
/// The value goes from the terminal to [`Store::put_secret`] and nowhere else.
/// What is written on the card is a confirmation — the name and the scope —
/// because that card is delivered to the agent, and the whole of D3 is that the
/// agent is told a name and never a value.
fn answer_secret_card(store: &Store, card: &jod_core::cards::Card) -> Result<jod_core::cards::Card> {
    use jod_core::secrets::{Scope, MIN_REDACTABLE_LEN};
    let name = card
        .secret_name
        .as_deref()
        .context("this secret card carries no variable name, so there is nothing to store")?;
    let scope = Scope::parse(card.secret_scope.as_deref().unwrap_or("work"));
    // The card records where the value *would* go; the id comes from the card's
    // own conversation and work, which is the only place it could honestly come
    // from — the agent that asked has no say in how widely it is shared.
    let scope_id = match scope {
        Scope::Global => String::new(),
        Scope::Work => card.work_id.clone().unwrap_or_default(),
        Scope::Conversation => card.conversation_id.clone(),
    };
    if scope != Scope::Global && scope_id.is_empty() {
        bail!(
            "card #{} asks for a {} secret and has no {} to attach it to — \
             `jod secret set {name} --global` stores it for every session instead",
            card.id,
            scope.as_str(),
            scope.as_str()
        );
    }

    let value = read_secret_value(&format!("value for {name}"))?;
    let meta = store.put_secret(name, scope, &scope_id, &value, &card.body)?;
    drop(value);
    if !meta.redactable {
        // Said out loud, because a silent exception here is a leak nobody was
        // told about: a value this short would match half of ordinary output,
        // so it is injected and not scrubbed.
        println!(
            "note: {name} is shorter than {MIN_REDACTABLE_LEN} characters, so it is injected \
             but NOT redacted from what the agent prints"
        );
    }
    Ok(store.answer_card(
        card.id,
        None,
        Some(&format!(
            "{name} is stored, {} scope. It is injected into the environment of the next run \
             — not the one in flight — and you will never be shown its value.",
            scope.as_str()
        )),
    )?)
}

/// Carry out a `jod root …` subcommand.
fn root_command(jod: &Jod, what: RootCommand) -> Result<()> {
    use jod_core::roots::NewRoot;
    let store = jod.store().context("this command needs the database")?;
    match what {
        RootCommand::Ls { conversation, json } => {
            let id = which_conversation(store, conversation)?;
            let roots = store.roots(&id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&roots)?);
            } else if roots.is_empty() {
                println!("no roots on {} — `jod root add <path>` sets one", short_id(&id));
            } else {
                render::roots(&roots);
            }
        }
        RootCommand::Add { path, conversation } => {
            let id = which_conversation(store, conversation)?;
            let root = store.add_root(&id, NewRoot::reading(&path))?;
            render::roots(&[root]);
        }
        RootCommand::Rm { path, conversation } => {
            let id = which_conversation(store, conversation)?;
            if store.remove_root(&id, &path)? {
                println!("{} is no longer a root of {}", path.display(), short_id(&id));
            } else {
                bail!("{} is not a root of {}", path.display(), short_id(&id));
            }
        }
    }
    Ok(())
}

/// The conversation a root command acts on: the one named, or the main chat.
///
/// The main chat is not created here if it is missing. `jod root ls` on a fresh
/// machine should say there is nothing rather than mint a conversation as a
/// side effect of looking.
fn which_conversation(store: &Store, typed: Option<String>) -> Result<String> {
    match typed {
        Some(typed) => resolve_conversation(store, &typed),
        None => store.pinned_conversation()?.context(
            "no conversation given and there is no main chat yet — pass --conversation, \
             or start one with `jod main \"…\"`",
        ),
    }
}

/// Carry out a `jod secret …` subcommand.
fn secret_command(jod: &Jod, what: SecretCommand) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    match what {
        SecretCommand::Ls {
            work,
            conversation,
            json,
        } => {
            // The resolution the spawn path performs, not a raw listing:
            // "which `OPENAI_API_KEY` would a run here actually get" is the
            // question somebody is asking, and two rows of the same name would
            // not answer it.
            let names = store.secrets_for(conversation.as_deref(), work.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&names)?);
            } else if names.is_empty() {
                println!("no secrets in scope — `jod secret set <NAME> --global` stores one");
            } else {
                render::secrets(&names);
            }
        }
        SecretCommand::Set { name, hint, scope } => {
            let (scope, scope_id) = scope.resolve();
            let value = read_secret_value(&format!("value for {name}"))?;
            let meta = store.put_secret(&name, scope, &scope_id, &value, &hint)?;
            drop(value);
            println!("{} stored, {} scope", meta.name, meta.scope.as_str());
            if !meta.redactable {
                println!(
                    "note: shorter than {} characters, so it is injected but NOT redacted \
                     from what an agent prints — redacting something this short would mangle \
                     ordinary output",
                    jod_core::secrets::MIN_REDACTABLE_LEN
                );
            }
            println!("it applies from the next spawn; runs already going were built without it");
        }
        SecretCommand::Rm { name, scope } => {
            let (scope, scope_id) = scope.resolve();
            if store.remove_secret(&name, scope, &scope_id)? {
                println!("{name} forgotten");
            } else {
                bail!("no {} secret named {name}", scope.as_str());
            }
        }
    }
    Ok(())
}

/// Read a credential from the terminal without echoing it, or from stdin.
///
/// Two paths because both are real. At a terminal the value is typed and must
/// not appear on screen or in the scrollback; in a script it arrives on stdin,
/// where there is nothing to echo. What neither path is, ever, is an argument:
/// `/proc/<pid>/cmdline` is world-readable for the life of the process, and the
/// shell keeps a copy in its history for ever after that.
///
/// Read exactly as given, with only a trailing newline removed — the one the
/// terminal adds when you press enter. Everything else is part of the value.
fn read_secret_value(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let piped = read_stdin()?;
        let value = piped.strip_suffix('\n').unwrap_or(&piped);
        if value.is_empty() {
            bail!("nothing arrived on stdin — pipe the value in, or run this at a terminal");
        }
        return Ok(value.to_string());
    }
    eprint!("{prompt}: ");
    std::io::Write::flush(&mut std::io::stderr()).ok();
    let value = read_without_echo();
    eprintln!();
    let value = value?;
    if value.trim().is_empty() {
        bail!("nothing typed");
    }
    Ok(value)
}

/// One line from the terminal with the echo off.
///
/// Raw mode rather than a crate: `crossterm` is already here for the
/// full-screen interface and this is a dozen lines against a dependency whose
/// whole job is to turn one flag off. Raw mode is disabled on every path out,
/// including the error ones — a terminal left raw is a terminal that stops
/// responding to Ctrl-C, which for a command that has just failed is a worse
/// outcome than the failure.
fn read_without_echo() -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    crossterm::terminal::enable_raw_mode().context("could not turn the terminal's echo off")?;
    let mut value = String::new();
    let outcome = loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent { code, modifiers, .. })) => match code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Backspace => {
                    value.pop();
                }
                // Ctrl-C at a password prompt means "stop", and it must not
                // leave a half-typed credential behind to be stored.
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear();
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow::anyhow!("could not read the value: {e}")),
        }
    };
    crossterm::terminal::disable_raw_mode().ok();
    outcome.map(|()| value)
}

/// Carry out a `jod commands …` subcommand.
///
/// The first thing in Jod that calls discovery at all. `scan`, `cache_discovered`
/// and `discovered` were written, tested, and reachable from nothing — the same
/// shape as the webhook rules and the Telegram bridge before them, where every
/// piece was green and the feature did not exist. This is the missing verb, and
/// it is deliberately the whole loop: scan the disk, write the cache, read it
/// back, so the path the palette will use is exercised rather than assumed.
fn commands_command(jod: &Jod, what: CommandsCommand) -> Result<()> {
    let store = jod.store().context("this command needs the database")?;
    match what {
        CommandsCommand::Ls {
            conversation,
            roots,
            harness,
            cached,
            json,
        } => {
            let kind = harness.map(HarnessKind::from);
            if !cached {
                // Roots from the flag, else the conversation's, else the main
                // chat's. A scan of nothing is not an error: it caches an empty
                // set, which correctly empties a palette whose repository has
                // been unmounted.
                let scanning: Vec<PathBuf> = if roots.is_empty() {
                    let id = which_conversation(store, conversation)?;
                    store.roots(&id)?.into_iter().map(|r| r.path).collect()
                } else {
                    roots
                };
                let found = jod_core::commands::scan(&scanning)?;
                store.cache_discovered(&scanning, &found)?;
            }

            let all = store.discovered(kind)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!(
                    "nothing found — Jod looks for `.claude/commands/`, `.claude/skills/`, \
                     `.opencode/command/` and `.agents/skills/` under each root"
                );
            } else {
                render::discovered(&all);
            }
        }
    }
    Ok(())
}

/// Carry out a `jod work …` subcommand.
async fn voice_command(jod: &Jod, what: VoiceCommand) -> Result<()> {
    use jod_voice_core::local;
    let store = jod.store().context("this command needs the database")?;
    let home = jod_core::paths::jod_home();

    match what {
        VoiceCommand::Check => {
            let s = voice::status(store, &home);
            match &s.recorder {
                Some(p) => println!("recorder   {p}"),
                None => println!(
                    "recorder   none — install one of: pw-record, arecord, rec, ffmpeg\n\
                     \x20          (on the machine running the console, which over SSH is the server)"
                ),
            }
            match &s.engine {
                Ok(e) => println!("engine     {}", e.label()),
                Err(why) => println!("engine     not ready — {why}"),
            }
            if s.installed.is_empty() {
                println!("models     none downloaded");
            } else {
                println!(
                    "models     {}",
                    s.installed
                        .iter()
                        .map(|m| m.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if s.recorder.is_some() && s.engine.is_ok() {
                println!("\nready — press Ctrl-V in the console and talk.");
            }
        }
        VoiceCommand::Models => {
            println!("Multilingual models only: an English-only build cannot represent Tagalog.\n");
            for m in local::CATALOG {
                let mark = if m.is_installed(&home) { "✓" } else { " " };
                let star = if m.name == local::RECOMMENDED { " ←" } else { "" };
                println!("{mark} {:<22} {:>5} MB  {}{star}", m.name, m.mb, m.note);
            }
            println!("\n`jod voice download <name>` fetches one. ✓ means already here.");
        }
        VoiceCommand::Download { name } => {
            let name = name.unwrap_or_else(|| local::RECOMMENDED.to_string());
            let m = local::model(&name).with_context(|| {
                format!("`{name}` is not a model this build knows — `jod voice models` lists them")
            })?;
            if m.is_installed(&home) {
                println!("{} is already downloaded.", m.name);
            } else {
                println!("downloading {} ({} MB) from the weights host…", m.name, m.mb);
                let mut last_pct = 0u64;
                let path = local::download(m, &home, |got, total| {
                    // Every whole percent, not every chunk: a progress line per
                    // 8 KB would be thousands of lines of scrollback.
                    if let Some(total) = total {
                        let pct = got * 100 / total.max(1);
                        if pct > last_pct {
                            last_pct = pct;
                            eprint!("\r  {pct}%");
                        }
                    }
                })
                .await
                .map_err(anyhow::Error::msg)?;
                eprintln!("\r  done");
                println!("installed at {}", path.display());
            }
            voice::set_model(store, m.name).map_err(anyhow::Error::msg)?;
            println!("transcription now runs on this machine, with {}.", m.name);
            if jod_voice_core::local::Whisper::detect().is_none() {
                println!(
                    "\nwhisper.cpp is not installed yet, so nothing can run the model.\n\
                     `brew install whisper-cpp`, or build it and point WHISPER_CLI at whisper-cli."
                );
            }
        }
        VoiceCommand::Use { name } => {
            voice::set_model(store, &name).map_err(anyhow::Error::msg)?;
            match voice::resolve(store, &home) {
                Ok(e) => println!("dictation uses {}", e.label()),
                Err(why) => println!("chosen, but not usable yet — {why}"),
            }
        }
        VoiceCommand::Cloud => {
            voice::set_cloud(store).map_err(anyhow::Error::msg)?;
            match voice::resolve(store, &home) {
                Ok(e) => println!("dictation uses {}", e.label()),
                Err(why) => println!("switched, but not usable yet — {why}"),
            }
        }
    }
    Ok(())
}

fn project_command(jod: &Jod, what: ProjectCommand) -> Result<()> {
    use jod_core::projects::{NewProject, State};
    let store = jod.store().context("this command needs the database")?;

    // Resolving by anything it is called, rather than by an id, for the same
    // reason the MCP tool does: these are things said out loud, and an id
    // would mean listing the catalog first just to translate a word you have.
    let find = |name: &str| -> Result<jod_core::projects::Project> {
        store
            .project_by_name(name)?
            .with_context(|| format!("no project called `{name}` — `jod project ls` lists them"))
    };

    match what {
        ProjectCommand::Ls { all, json } => {
            let projects = store.projects(all)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else if projects.is_empty() {
                println!(
                    "no projects yet — `jod project add .` catalogs the repository you are in"
                );
            } else {
                for p in &projects {
                    println!("{}", p.summary_line());
                }
            }
        }
        ProjectCommand::Add {
            path,
            name,
            aliases,
            notes,
        } => {
            let path = match path {
                Some(p) => p,
                None => std::env::current_dir().context("no path given and no current directory")?,
            };
            let mut new = NewProject::at(&path).with_aliases(aliases);
            if let Some(name) = name {
                new = new.named(name);
            }
            if let Some(notes) = notes {
                new = new.with_notes(notes);
            }
            let project = store.add_project(new)?;
            println!("{}", project.summary_line());
            println!(
                "  matched by: {}",
                project.spoken_forms().join(", ")
            );
        }
        ProjectCommand::Archive { name } => {
            let project = find(&name)?;
            store.set_project_state(&project.id, State::Archived)?;
            println!(
                "{} archived — it can still be named, but will not be inferred",
                project.name
            );
        }
        ProjectCommand::Restore { name } => {
            let project = find(&name)?;
            store.set_project_state(&project.id, State::Active)?;
            println!("{} is back in play", project.name);
        }
    }
    Ok(())
}

fn work_command(jod: &Jod, what: WorkCommand) -> Result<()> {
    use jod_core::works::{Deletion, Filter};
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        WorkCommand::Ls { all, closed, json } => {
            let filter = match (all, closed) {
                (true, _) => Filter::All,
                (_, true) => Filter::Closed,
                _ => Filter::Live,
            };
            let works = store.works(filter)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&works)?);
            } else if works.is_empty() {
                println!("no works — `jod main \"work on @repo, do X\"` opens one");
            } else {
                render::works(&works, now);
            }
        }
        WorkCommand::Show { id, json } => {
            let id = resolve_work(store, &id)?;
            let work = store.work(&id)?.expect("resolve_work found it a moment ago");
            let sessions = store.work_sessions(&id)?;
            let tasks = store.work_tasks(&id)?;
            let leases = store.work_leases(&id)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "work": work,
                        "sessions": sessions,
                        "tasks": tasks,
                        "leases": leases,
                    }))?
                );
            } else {
                // Cards per session, because "where are the questions" is the
                // reason to open a work at all and the tree is where they hide.
                let mut cards = Vec::new();
                for session in &sessions {
                    cards.push(store.count_open_cards(&session.conversation_id, false)?);
                }
                render::work(&work, &sessions, &cards, &tasks, &leases, now);
            }
        }
        WorkCommand::Close { id } => {
            let id = resolve_work(store, &id)?;
            let closing = store.close_work(&id)?;
            print!("{}", closing.summary());
            if let Some(card) = closing.card_id {
                println!("raised as card #{card}");
            }
        }
        WorkCommand::Delete { id } => {
            let id = resolve_work(store, &id)?;
            // `None` on both attempts, deliberately. The refusal arms a
            // confirmation in the database and the second call finds it, so
            // D8's "the same command, repeated" is literally the same command
            // — two processes sharing nothing but the file. Passing a
            // confirmation from here would put the expiry in the CLI's hands,
            // which is exactly what the store is refusing to allow.
            let armed = store.armed_deletion(&id)?.is_some();
            match store.delete_work(&id, None)? {
                Deletion::Refused { doomed, .. } => {
                    print!("{}", doomed.report());
                    // Said only when it is true. A refusal that promised a
                    // repeat would go through, when a lease cut in between had
                    // silently disarmed it, would teach somebody to type the
                    // command twice without reading it.
                    let seconds = store
                        .armed_deletion(&id)?
                        .map(|c| {
                            (c.expires_at_ms() - chrono::Utc::now().timestamp_millis()).max(0) / 1000
                        })
                        .unwrap_or(0);
                    bail!(
                        "refused: nothing was touched. Repeat the identical command within {seconds}s \
                         to go ahead — the worktrees and branches above are left on disk either way{}",
                        if armed {
                            ", and the lease set changed since the last attempt, so that \
                             confirmation no longer stands"
                        } else {
                            ""
                        }
                    );
                }
                Deletion::Done {
                    doomed,
                    worktrees_left,
                } => {
                    println!(
                        "deleted {} — {} session(s), {} transcript(s), {} unanswered card(s)",
                        doomed.title, doomed.sessions, doomed.transcripts, doomed.unanswered_cards
                    );
                    // Printed so nothing is orphaned silently: Jod's records
                    // are cheap to recreate and a branch with uncommitted work
                    // on it is not.
                    for path in &worktrees_left {
                        println!("  left on disk: {}", path.display());
                    }
                    if !worktrees_left.is_empty() {
                        println!("`jod work leases --orphaned` finds them again");
                    }
                }
            }
        }
        WorkCommand::Leases { id, orphaned, json } => {
            let leases = match (&id, orphaned) {
                (_, true) => store.orphaned_leases()?,
                (Some(id), _) => store.work_leases(&resolve_work(store, id)?)?,
                (None, _) => {
                    let mut all = Vec::new();
                    for work in store.works(Filter::All)? {
                        all.extend(store.work_leases(&work.id)?);
                    }
                    all.extend(store.orphaned_leases()?);
                    all
                }
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&leases)?);
                return Ok(());
            }
            if leases.is_empty() {
                println!("no worktrees claimed");
                return Ok(());
            }
            // Read from git now rather than from the row: a worktree that was
            // clean an hour ago says nothing about whether removing it today
            // would lose somebody's afternoon.
            let mut conditions = Vec::new();
            for lease in &leases {
                conditions.push(store.lease_condition(lease)?);
            }
            render::leases(&leases, &conditions, now);
        }
    }
    Ok(())
}

/// Resolve a typed work-id prefix, refusing an ambiguous one.
///
/// The same rule [`resolve_conversation`] follows, for a sharper reason:
/// `jod work delete` on the wrong work takes every transcript in it.
fn resolve_work(store: &Store, typed: &str) -> Result<String> {
    use jod_core::works::Filter;
    let all = store.works(Filter::All)?;
    if all.iter().any(|w| w.id == typed) {
        return Ok(typed.to_string());
    }
    let hits: Vec<_> = all.iter().filter(|w| w.id.starts_with(typed)).collect();
    match hits.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => bail!("no work starts with {typed} — `jod work ls` lists them"),
        many => bail!("{typed} matches {} works — type more of it", many.len()),
    }
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

/// Resolve a typed run-id prefix against the runs Jod knows about.
///
/// Every surface *shows* an eight-character id — `jod ls`, `jod main`, the run
/// summary printed after a spawn — and `jod watch` and `jod kill` then demanded
/// the full uuid. `jod main` printed ``jod watch 1f0fc870`` as a hint and that
/// hint did not work, which is the kind of detail that teaches someone the tool
/// is lying to them.
///
/// Ambiguity is refused rather than guessed, for the same reason as
/// [`resolve_conversation`]: `jod kill` on the wrong agent is not undoable.
/// An exact match wins outright, so a full uuid never has to be disambiguated
/// against itself.
async fn resolve_run(jod: &Jod, typed: &str) -> Result<String> {
    if jod.agent(typed).await.is_ok() {
        return Ok(typed.to_string());
    }
    let all = jod.agents().await;
    let hits: Vec<_> = all.iter().filter(|a| a.id.starts_with(typed)).collect();
    match hits.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => bail!("no agent with id `{typed}`"),
        many => bail!("{typed} matches {} agents — type more of it", many.len()),
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
/// Carry out a `jod webhook …` subcommand.
///
/// The verbs a rule needs to exist at all. Everything downstream of a rule —
/// the receiver, the HMAC check, the delivery ledger, the TUI's list with its
/// enable/disable/delete keys — was already built and tested against rules that
/// only ever existed inside test functions.
fn webhook_command(jod: &Jod, what: WebhookCommand) -> Result<()> {
    use jod_core::webhook::{Conditions, Rule};
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        WebhookCommand::Ls { json } => {
            let all = store.webhook_rules()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("no webhook rules — `jod webhook add` writes one");
            } else {
                render_time::webhook_rules(&all);
            }
        }
        WebhookCommand::Add {
            name,
            prompt,
            repo,
            event,
            action,
            labels,
            branch,
            author,
            draft,
            harness,
            cwd,
            model,
            paused,
        } => {
            if store.webhook_rule(&name)?.is_some() {
                bail!("a rule named `{name}` already exists — `jod webhook rm {name}` first");
            }
            let rule = Rule {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                source: "github".into(),
                repo,
                event,
                action,
                conditions: Conditions {
                    labels,
                    branch,
                    author,
                    draft,
                },
                prompt,
                harness: HarnessKind::from(harness).id().to_string(),
                cwd: cwd.unwrap_or(std::env::current_dir()?).display().to_string(),
                model,
                enabled: !paused,
                created_at_ms: now,
            };
            store.add_webhook_rule(&rule)?;
            println!(
                "{} {name} · {} {}",
                if paused { "○" } else { "●" },
                rule.event,
                rule.repo
            );
            if paused {
                println!("  disarmed — `jod webhook enable {name}` arms it");
            }
        }
        WebhookCommand::Enable { name } => set_rule_armed(&store, &name, true)?,
        WebhookCommand::Disable { name } => set_rule_armed(&store, &name, false)?,
        WebhookCommand::Rm { name } => {
            if store.delete_webhook_rule(&name)? {
                println!("deleted {name}");
            } else {
                bail!("no rule named `{name}`");
            }
        }
        WebhookCommand::Deliveries { limit, json } => {
            let all = store.deliveries(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("nothing has arrived yet");
            } else {
                render_time::deliveries(&all, now);
            }
        }
    }
    Ok(())
}

/// Arm or disarm a rule, reporting the name back rather than a silent success.
///
/// A missing name is an error and not a no-op: `jod webhook enable ci-faild` is
/// a typo, and a cheerful nothing would read as "armed".
fn set_rule_armed(store: &jod_core::store::Store, name: &str, armed: bool) -> Result<()> {
    if store.set_webhook_rule_enabled(name, armed)? {
        println!("{} {name}", if armed { "●" } else { "○" });
        Ok(())
    } else {
        bail!("no rule named `{name}`")
    }
}

/// The bot token, from the one name Jod documents or the one people write.
///
/// `JOD_TELEGRAM_TOKEN` is canonical and always wins. `TELEGRAM_BOT_API_KEY` is
/// accepted because it is what a person writes into a `.env` without consulting
/// anything, and refusing it teaches nothing — the alternative is a bot that is
/// silent for the one reason its own error message does not mention.
fn telegram_token() -> Result<String> {
    for name in ["JOD_TELEGRAM_TOKEN", "TELEGRAM_BOT_API_KEY"] {
        if let Ok(t) = std::env::var(name) {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
    }
    bail!("JOD_TELEGRAM_TOKEN is not set — a `.env` in this directory is read automatically")
}

/// Carry out a `jod telegram …` subcommand.
async fn telegram_command(jod: std::sync::Arc<Jod>, what: TelegramCommand) -> Result<()> {
    use jod_core::telegram::{Allowlist, Bridge, Config, HttpBot, Poller};
    match what {
        TelegramCommand::Whoami => {
            let bot = HttpBot::new(&telegram_token()?)?;
            // An empty allowlist refuses everybody, which is precisely what
            // makes this a bootstrap: every pending message becomes a Refusal
            // carrying the id that belongs in JOD_TELEGRAM_ALLOWED_USERS.
            // Reusing the real poller rather than curl means the ids printed
            // here are the ids `serve` will actually compare against.
            let poller = Poller::new(bot, Allowlist::default());
            let batch = poller
                .poll_once()
                .await
                .map_err(|e| anyhow::anyhow!("Telegram refused the poll: {e}"))?;
            println!("the token works — Telegram answered ({} update(s))", batch.len());
            let refusals = poller.refusals();
            if refusals.is_empty() {
                println!("nobody has messaged the bot yet — send it anything, then re-run");
                return Ok(());
            }
            // By person, not by message: somebody who sent three messages is
            // still one id to allow, and printing them three times reads as
            // three different people at a glance.
            println!("\nwho has messaged it:");
            let mut ids: Vec<i64> = Vec::new();
            for r in &refusals {
                if ids.contains(&r.user_id) {
                    continue;
                }
                ids.push(r.user_id);
                let who = r.username.as_deref().unwrap_or("(no username)");
                println!("  {} @{}", r.user_id, who);
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            println!("\nto let them in:\n  JOD_TELEGRAM_ALLOWED_USERS={list}");
            // Said out loud because this command consumes the backlog: the
            // messages just counted will not be redelivered to `serve`, and
            // silently eating the first thing somebody sent is the kind of
            // thing that reads as a broken bot.
            println!("\n\x1b[2m(these updates are now acknowledged and will not reach `serve`)\x1b[0m");
        }
        TelegramCommand::Serve { cwd, harness } => {
            // `from_parts` rather than `from_env`, purely so the token comes
            // from the same lookup `whoami` uses. Routing one of the two
            // through the alias and not the other is how this first shipped,
            // and the symptom was `whoami` proving the token while `serve`
            // insisted it was unset.
            //
            // Everything else `from_env` does is kept, including the refusal
            // of an empty allowlist at startup rather than at the first
            // message: a bot that silently answers nobody looks exactly like
            // a bot with a bad token.
            let mut config = Config::from_parts(
                Some(telegram_token()?),
                std::env::var("JOD_TELEGRAM_ALLOWED_USERS").ok(),
                jod_core::service::default_cwd(),
            )?;
            if let Some(dir) = cwd {
                config.cwd = dir;
            }
            config.harness = HarnessKind::from(harness);
            let bot = HttpBot::new(&config.token)?;
            let bridge = Bridge::new(bot, jod, &config);
            println!(
                "listening as a {} bridge in {} — Ctrl-C stops it",
                config.harness.label(),
                config.cwd.display()
            );
            bridge
                .run()
                .await
                .map_err(|e| anyhow::anyhow!("the bridge stopped: {e}"))?;
        }
    }
    Ok(())
}

/// Carry out a `jod monitor …` subcommand.
fn monitor_command(jod: &Jod, what: MonitorCommand) -> Result<()> {
    use jod_core::monitor::{Decision, LocalProbes, Mode, Monitor, Probe};
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    match what {
        MonitorCommand::Ls { json } => {
            let all = store.monitors()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else if all.is_empty() {
                println!("no monitors — `jod monitor set <schedule> --command …` attaches one");
            } else {
                // Every monitor is shown against its schedule's *name*. A
                // monitor whose schedule has been deleted falls back to the id
                // rather than vanishing: an orphan row is exactly the thing a
                // listing exists to reveal.
                let named: std::collections::HashMap<String, String> = store
                    .schedules()?
                    .into_iter()
                    .map(|s| (s.id, s.name))
                    .collect();
                let rows: Vec<_> = all
                    .into_iter()
                    .map(|m| {
                        let name = named
                            .get(&m.schedule_id)
                            .cloned()
                            .unwrap_or_else(|| format!("{} (no such schedule)", m.schedule_id));
                        (m, name)
                    })
                    .collect();
                render_time::monitors(&rows, now);
            }
        }
        MonitorCommand::Set {
            schedule,
            command,
            url,
            cwd,
            no_agent,
        } => {
            let s = store
                .schedule_named(&schedule)?
                .with_context(|| format!("no schedule named `{schedule}`"))?;
            let probe = match (command, url) {
                (Some(c), None) => Probe::Command(c),
                (None, Some(u)) => Probe::Url(u),
                // Refused rather than defaulted. Guessing which one was meant
                // would attach a monitor nobody described, and a monitor that
                // watches the wrong thing reports "unchanged" for ever.
                _ => bail!("give exactly one of --command or --url"),
            };
            if no_agent && matches!(probe, Probe::Url(_)) {
                // `no_agent` means "stdout is the result". For a URL that is
                // the whole page, reported in full on every single tick — a
                // notification firehose rather than a watchdog. Watch mode is
                // what a URL wants, and it is one flag away.
                bail!(
                    "--no-agent reports the probe's whole output every tick, which for a URL is \
                     the entire page — drop --no-agent to be told only when it changes"
                );
            }
            let mode = if no_agent { Mode::NoAgent } else { Mode::Watch };
            // The schedule's own directory by default, so the probe and the run
            // it gates look at the same tree. A monitor watching `git log` in
            // some other checkout is a bug that reads as a working monitor.
            let dir = cwd.map(|p| p.display().to_string()).unwrap_or(s.cwd.clone());
            let m = Monitor::new(&s.id, probe).in_dir(dir).with_mode(mode);
            let replacing = store.monitor(&s.id)?.is_some();
            store.set_monitor(&m)?;
            println!(
                "{} {schedule} · {} {}",
                if replacing { "↻" } else { "●" },
                m.probe.kind(),
                m.probe.target()
            );
            println!(
                "  {}",
                match mode {
                    Mode::NoAgent => "no agent runs — the script's stdout is the result",
                    Mode::Watch => "the next check sets a baseline and wakes nothing",
                }
            );
        }
        MonitorCommand::Rm { schedule } => {
            let s = store
                .schedule_named(&schedule)?
                .with_context(|| format!("no schedule named `{schedule}`"))?;
            if store.delete_monitor(&s.id)? {
                println!("{schedule} now fires on its cron alone");
            } else {
                bail!("{schedule} has no monitor");
            }
        }
        MonitorCommand::Check { schedule, record } => {
            let s = store
                .schedule_named(&schedule)?
                .with_context(|| format!("no schedule named `{schedule}`"))?;
            let m = store
                .monitor(&s.id)?
                .with_context(|| format!("{schedule} has no monitor"))?;
            // Dry by default, and `check` rather than `observe` for it: a dry
            // run that moved the baseline would consume the very change the
            // next real tick exists to notice, and "I tested it and then it
            // never fired" is the least debuggable outcome available.
            let decision = if record {
                let now = chrono::Utc::now().timestamp_millis();
                let (seen, decision) = jod_core::monitor::observe(&m, &LocalProbes);
                store.record_check(&s.id, &seen, &decision, now)?;
                decision
            } else {
                jod_core::monitor::check(&m, &LocalProbes)
            };
            match decision {
                Decision::Baseline => println!("baseline — nothing to compare against yet"),
                Decision::Suppress => println!("unchanged — a tick now would run nothing"),
                Decision::Run { diff } => {
                    println!("changed — a tick now would run the schedule with:");
                    println!("{diff}");
                }
                Decision::Report { text } => {
                    println!("reported — a tick now would deliver this and wake no model:");
                    println!("{text}");
                }
                Decision::Silent => println!("silent — the script said nothing, so nothing happens"),
                Decision::Failed { detail } => {
                    println!("failed — {detail}");
                }
            }
            println!(
                "\x1b[2m({})\x1b[0m",
                if record {
                    "recorded — the next check compares against this"
                } else {
                    "nothing was recorded; the baseline is where it was"
                }
            );
        }
        MonitorCommand::Log { schedule, limit } => {
            let s = store
                .schedule_named(&schedule)?
                .with_context(|| format!("no schedule named `{schedule}`"))?;
            let checks = store.monitor_checks(&s.id, limit)?;
            if checks.is_empty() {
                println!("{schedule}'s monitor has not been checked yet");
            } else {
                render_time::checks(&checks, now);
            }
        }
    }
    Ok(())
}

/// `jod ledger …` — the reader the delivery ledger never had.
///
/// The module has recorded every owed message since it was wired, and nothing
/// could show one. A ledger nobody can read proves things to nobody, which is a
/// slower version of not keeping one.
fn ledger_command(jod: &Jod, what: LedgerCommand) -> Result<()> {
    use jod_core::ledger::DeliveryState;
    let store = jod.store().context("this command needs the database")?;
    let now = chrono::Utc::now().timestamp_millis();
    // The ledger prunes itself to `MAX_ROWS`, so asking for that many is asking
    // for all of it. A smaller page would be a lie in the one view whose whole
    // job is "is anything outstanding" — an answer that silently omitted the
    // oldest unsettled row would be worse than no answer.
    let everything = jod_core::ledger::MAX_ROWS as usize;
    match what {
        LedgerCommand::Ls { all, json } => {
            let rows: Vec<_> = store
                .obligations(everything)?
                .into_iter()
                .filter(|o| all || !o.state.is_settled())
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                // Two different silences, and conflating them is the failure
                // this command exists to end: "nothing is owed" is the good
                // news, "nothing was ever recorded" means the ledger is not
                // running and the good news would look identical.
                let recorded = store.obligations(1)?.len();
                if all || recorded == 0 {
                    println!("nothing in the ledger — no message has been owed yet");
                } else {
                    println!("nothing outstanding — every message Jod owed has been settled");
                    println!("`jod ledger ls --all` shows the settled ones");
                }
            } else {
                render_time::obligations(&rows, now);
            }
        }
        LedgerCommand::Show { what, json } => {
            // By key first, then by row id. The key is what every listing
            // prints and what a person will have in hand; the id is what a
            // `--json` consumer has.
            let found = match store.obligation_by_key(&what)? {
                Some(o) => Some(o),
                None => match what.parse::<i64>() {
                    Ok(id) => store.obligation(id)?,
                    Err(_) => None,
                },
            };
            let o = found.with_context(|| format!("nothing in the ledger matches `{what}`"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&o)?);
            } else {
                render_time::obligation(&o, now);
            }
        }
        LedgerCommand::Failed { json } => {
            let rows: Vec<_> = store
                .obligations(everything)?
                .into_iter()
                .filter(|o| o.state == DeliveryState::Failed)
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("nothing has been given up on");
            } else {
                render_time::obligations(&rows, now);
            }
        }
    }
    Ok(())
}

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
                // A fire's outcome is decided when the run is *started*, so a
                // run that then failed still reads `ran`. Judged alone, a
                // schedule whose every run has failed shows a column of ticks —
                // in the one place you look to ask whether it works. So each
                // fire is paired with how its run actually ended.
                let outcomes: Vec<(jod_core::schedule::Fire, Option<String>)> = fires
                    .into_iter()
                    .map(|f| {
                        let ended = f
                            .run_id
                            .as_deref()
                            .and_then(|id| store.run(id).ok().flatten())
                            .map(|r| r.status);
                        (f, ended)
                    })
                    .collect();
                render_time::fires(&outcomes, now);
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

/// The roots and secret names a `jod run` should carry into its request.
///
/// Both are properties of the *thread*, not of the command line, so a run that
/// continues a conversation inherits what that conversation was given — exactly
/// as [`jod_core::service::prefer_conversation_settings`] does for the model and
/// the permission. Without this the two features are storage and nothing else:
/// `jod root add` records a directory no harness is ever granted, and
/// `jod secret set` records a name no run is ever given.
///
/// **Names, never values.** The list handed over is what the supervisor
/// resolves at exec, out of a file only its owner can read. A value in this
/// process would be a value in `spawn.json`, in `ps`, and in whatever logs the
/// launcher writes.
///
/// A fresh run has no conversation yet and therefore no roots — but it does get
/// the global secrets, because "every session on this box" is what global
/// means. Anything narrower would need a thread to be narrow *about*.
fn grants_for_run(
    store: &Store,
    resume: &Resume,
    harness: jod_core::HarnessKind,
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let Some(conversation) = continuing_conversation(store, resume, harness)? else {
        let names = store.secrets_for(None, None)?;
        return Ok((Vec::new(), names.into_iter().map(|s| s.name).collect()));
    };
    let roots = store
        .roots(&conversation)?
        .into_iter()
        .map(|r| r.path)
        .collect();
    // The work matters: a key given for one project is not handed to every
    // session on the box, and a session's work is where that scoping lives.
    let work = store.work_for_conversation(&conversation)?;
    let secrets = store
        .secrets_for(Some(&conversation), work.as_deref())?
        .into_iter()
        .map(|s| s.name)
        .collect();
    Ok((roots, secrets))
}

/// Where the console works when `--cwd` said nothing: the directory it was
/// launched in.
///
/// Not [`jod_core::service::default_cwd`], which answers `$HOME`, and the
/// difference is not cosmetic. A console is opened *inside* something — you
/// `cd` to a repository and type `jod tui` — so the home directory is almost
/// never the answer, and it was the wrong one in three places at once: every
/// turn's harness process started in `$HOME`, the band now printing the
/// directory would have printed `~`, and [`crate::tui::ensure_launch_root`]
/// would have handed the conversation the whole home directory to search.
///
/// `$HOME` remains the fallback for the case that has no launched-in
/// directory at all — a working directory that has been deleted out from under
/// the process, which is where `current_dir` fails.
///
/// This is the console only. `jod run` is a one-shot that may be fired from
/// anywhere by anything, and a batch job silently inheriting whatever directory
/// its caller happened to be in is the opposite of what a resumed run wants —
/// see [`session_cwd`], which looks the answer up rather than assuming it.
fn console_cwd(given: Option<PathBuf>) -> PathBuf {
    given
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(jod_core::service::default_cwd)
}

/// The directory a resumed session belongs to, when Jod knows it.
///
/// `None` for a fresh run, and for a session id Jod has never seen — somebody
/// resuming a session started outside Jod, where there is nothing to look up
/// and the caller's own directory is the only answer available.
///
/// Separate from [`grants_for_run`] rather than folded into it because the two
/// answer different questions and one of them can be wrong without the other
/// noticing: that one asks what this run may *reach*, this one asks where it
/// must *happen*. Sharing [`continuing_conversation`] keeps them agreeing about
/// which thread is being rejoined.
fn session_cwd(
    store: &Store,
    resume: &Resume,
    harness: jod_core::HarnessKind,
) -> Result<Option<PathBuf>> {
    let Some(id) = continuing_conversation(store, resume, harness)? else {
        return Ok(None);
    };
    Ok(store
        .conversation(&id)?
        .map(|c| c.cwd)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from))
}

/// Which conversation a `--continue` or `--session` run is rejoining.
///
/// `jod run` binds its *transcript* to a new conversation every time
/// ([`RunConversation::New`]), so this is not that question. It asks the other
/// one: which existing thread is the harness being resumed into, and therefore
/// whose roots and secrets apply.
fn continuing_conversation(
    store: &Store,
    resume: &Resume,
    harness: jod_core::HarnessKind,
) -> Result<Option<String>> {
    let recent = store.conversations(200)?;
    Ok(match resume {
        Resume::Fresh => None,
        // The harness's own id for the session, which is what `--session` takes.
        Resume::Session(id) => recent
            .into_iter()
            .find(|c| c.session_id.as_deref() == Some(id.as_str()))
            .map(|c| c.id),
        // Newest first, and filtered by harness: `--continue` resumes *this*
        // harness's last session, so picking the newest conversation of any
        // harness would hand one harness's roots to another.
        Resume::Last => recent
            .into_iter()
            .find(|c| c.harness == harness.id() && c.session_id.is_some())
            .map(|c| c.id),
    })
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
    permission: PermissionPolicy,
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
                    system: None,
                    cwd: cwd.clone(),
                    model: model.clone(),
                    permission: permission.into(),
                    resume: resume.clone(),
                    tools: None,
                    ..SpawnRequest::default()
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

    // ---- the rail, the roots, the secrets and the works ----

    fn arg_names(path: &[&str]) -> Vec<String> {
        use clap::CommandFactory;
        let mut command = Cli::command();
        for name in path {
            command = command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("no `{name}` subcommand"))
                .clone();
        }
        command
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .collect()
    }

    /// **The property `jod secret set` exists to have.** Anything on a command
    /// line is world-readable through `/proc` for the life of the process and
    /// in the shell's history for ever afterwards, so there must be no argument
    /// a value could be typed into — asserted, because adding one back would be
    /// a one-line convenience with no visible symptom.
    #[test]
    fn setting_a_secret_takes_no_argument_a_value_could_arrive_in() {
        let args = arg_names(&["secret", "set"]);
        for forbidden in ["value", "secret", "token", "key"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "`jod secret set` grew a `{forbidden}` argument: {args:?}"
            );
        }
        assert!(args.iter().any(|a| a == "name"));
    }

    /// The scope is the blast radius if a value leaks, so it is typed rather
    /// than defaulted — `--global` hands a key to every session on the box.
    #[test]
    fn storing_a_secret_without_saying_who_it_is_for_is_refused() {
        use clap::CommandFactory;
        let refused = Cli::command()
            .try_get_matches_from(["jod", "secret", "set", "STRIPE_API_KEY"])
            .is_err();
        assert!(refused, "a secret was stored without a scope being chosen");
        for scope in [
            ["jod", "secret", "set", "K", "--global"],
            ["jod", "secret", "set", "K", "--work=w1"],
            ["jod", "secret", "set", "K", "--conversation=c1"],
        ] {
            assert!(
                Cli::command().try_get_matches_from(scope).is_ok(),
                "{scope:?} was refused"
            );
        }
        // And never two at once: a value cannot be global *and* one work's.
        assert!(Cli::command()
            .try_get_matches_from(["jod", "secret", "set", "K", "--global", "--work=w1"])
            .is_err());
    }

    #[test]
    fn a_scope_flag_resolves_to_the_scope_it_names() {
        use jod_core::secrets::Scope;
        let global = ScopeArgs {
            global: true,
            work: None,
            conversation: None,
        };
        assert_eq!(global.resolve(), (Scope::Global, String::new()));
        let work = ScopeArgs {
            global: false,
            work: Some("w1".into()),
            conversation: None,
        };
        assert_eq!(work.resolve(), (Scope::Work, "w1".to_string()));
        let conversation = ScopeArgs {
            global: false,
            work: None,
            conversation: Some("c1".into()),
        };
        assert_eq!(conversation.resolve(), (Scope::Conversation, "c1".to_string()));
    }

    #[test]
    fn every_card_filter_maps_to_the_one_the_store_takes() {
        use jod_core::cards::{CardKind, Sort, Status};
        assert_eq!(CardKind::from(KindArg::Decision), CardKind::Decision);
        assert_eq!(CardKind::from(KindArg::Question), CardKind::Question);
        assert_eq!(CardKind::from(KindArg::Secret), CardKind::Secret);
        assert_eq!(Status::from(StatusArg::Open), Status::Open);
        assert_eq!(Status::from(StatusArg::Answered), Status::Answered);
        assert_eq!(Status::from(StatusArg::Dismissed), Status::Dismissed);
        // Every order the rail cycles through is one the command line can name,
        // or the two surfaces sort the same cards differently.
        let named: Vec<Sort> = [SortArg::Pressing, SortArg::Importance, SortArg::Created, SortArg::Updated]
            .into_iter()
            .map(Sort::from)
            .collect();
        assert_eq!(named, Sort::ALL.to_vec());
    }

    /// A temporary `JOD_HOME`, held for one test.
    ///
    /// `put_secret` writes the value to a file under `$JOD_HOME/secrets`, so a
    /// test that stores one without redirecting the variable writes a
    /// credential file into the developer's real Jod home — which is exactly
    /// what an orchestrator test did until this session, and it went unnoticed
    /// because the row it asserted on lived in an in-memory database while the
    /// file did not. `JOD_HOME` is process-wide, so the lock is what stops two
    /// of these from landing in each other's directory.
    struct TempHome {
        dir: PathBuf,
        previous: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl TempHome {
        fn new(tag: &str) -> TempHome {
            let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("JOD_HOME").ok();
            let dir = std::env::temp_dir().join(format!(
                "jod-cli-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("JOD_HOME", &dir);
            TempHome {
                dir,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            // Restored rather than unset: on the box Jod runs on `JOD_HOME` is
            // set, and clearing it would send every later reader to `~/.jod`.
            match self.previous.take() {
                Some(value) => std::env::set_var("JOD_HOME", value),
                None => std::env::remove_var("JOD_HOME"),
            }
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// A console is opened *inside* something. `jod tui` typed in a repository
    /// used to open a session whose every turn ran in `$HOME` — so the harness
    /// started somewhere the user was not, and the picker, which has always
    /// used the launched-in directory, disagreed with it.
    #[test]
    fn a_console_opens_where_it_was_launched_rather_than_at_home() {
        let here = std::env::current_dir().expect("a working directory");
        assert_eq!(console_cwd(None), here);
        // ...and somebody who names a directory means it.
        assert_eq!(
            console_cwd(Some(PathBuf::from("/tmp/elsewhere"))),
            PathBuf::from("/tmp/elsewhere")
        );
    }

    /// The bug this pins: `jod root add` and `jod secret set` both wrote rows
    /// that no run ever read, so the two features were storage and nothing
    /// else. It asserts the **request** — what the harness is actually handed —
    /// because a test that read the conversation back would have gone on
    /// passing for as long as the wiring was missing.
    #[test]
    fn a_continued_run_is_handed_the_conversation_s_roots_and_secret_names() {
        let _home = TempHome::new("grants");
        let store = Store::in_memory().unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id;
        store
            .set_conversation_session(&conversation, Some("sess-abc"))
            .unwrap();
        store
            .add_root(&conversation, jod_core::roots::NewRoot::reading("/tmp/repo"))
            .unwrap();
        store
            .add_root(&conversation, jod_core::roots::NewRoot::reading("/tmp/notes"))
            .unwrap();
        store
            .put_secret(
                "OPENAI_API_KEY",
                jod_core::secrets::Scope::Global,
                "",
                "a-value-long-enough-to-redact",
                "",
            )
            .unwrap();

        let (roots, secrets) = grants_for_run(
            &store,
            &Resume::Session("sess-abc".into()),
            HarnessKind::ClaudeCode,
        )
        .unwrap();
        assert_eq!(
            roots,
            vec![PathBuf::from("/tmp/repo"), PathBuf::from("/tmp/notes")],
            "both roots must reach the request, in the user's order"
        );
        assert_eq!(secrets, vec!["OPENAI_API_KEY".to_string()]);

        // `--continue` finds the same thread through the harness rather than
        // through an id, and must not reach across harnesses to do it.
        let (roots, _) = grants_for_run(&store, &Resume::Last, HarnessKind::ClaudeCode).unwrap();
        assert_eq!(roots.len(), 2);
        let (roots, secrets) = grants_for_run(&store, &Resume::Last, HarnessKind::OpenCode).unwrap();
        assert!(
            roots.is_empty(),
            "one harness's roots were handed to another: {roots:?}"
        );
        assert_eq!(
            secrets,
            vec!["OPENAI_API_KEY".to_string()],
            "a run with no thread still gets the globals — that is what global means"
        );
    }

    /// The rule the whole design rests on, asserted where it would be easiest
    /// to break: what leaves this process is a list of names.
    #[test]
    fn what_a_run_carries_is_names_and_never_values() {
        let _home = TempHome::new("names-only");
        let store = Store::in_memory().unwrap();
        let value = "sk-not-a-real-key-but-long-enough";
        store
            .put_secret(
                "STRIPE_API_KEY",
                jod_core::secrets::Scope::Global,
                "",
                value,
                "",
            )
            .unwrap();

        let (_, secrets) = grants_for_run(&store, &Resume::Fresh, HarnessKind::ClaudeCode).unwrap();
        assert_eq!(secrets, vec!["STRIPE_API_KEY".to_string()]);
        assert!(
            !secrets.iter().any(|s| s.contains(value)),
            "a value reached the spawn request, which is written to spawn.json"
        );
    }

    /// **A resumed run happens where its session lives.**
    ///
    /// Measured, not guessed: OpenCode scopes a session to the project it
    /// resolves from `--dir`, and `--session <id>` naming a session from
    /// another project neither errors nor starts fresh — it hangs silently for
    /// ever. So resuming in whatever directory the command was typed in is not
    /// a cosmetic mistake, and this is the lookup that prevents it.
    #[tokio::test]
    async fn resuming_a_session_uses_the_directory_that_session_belongs_to() {
        let store = Store::in_memory().unwrap();
        let conversation = store
            .new_conversation(HarnessKind::OpenCode, "/tmp/the-project", None)
            .unwrap();
        store
            .set_conversation_session(&conversation.id, Some("ses_abc"))
            .unwrap();

        assert_eq!(
            session_cwd(
                &store,
                &Resume::Session("ses_abc".into()),
                HarnessKind::OpenCode
            )
            .unwrap(),
            Some(PathBuf::from("/tmp/the-project"))
        );
        // A fresh run has no session to belong to, and a session Jod never saw
        // cannot be looked up — both fall back to the caller's directory rather
        // than inventing one.
        assert_eq!(
            session_cwd(&store, &Resume::Fresh, HarnessKind::OpenCode).unwrap(),
            None
        );
        assert_eq!(
            session_cwd(
                &store,
                &Resume::Session("ses_started_outside_jod".into()),
                HarnessKind::OpenCode
            )
            .unwrap(),
            None
        );
    }

    /// `--continue` resumes *this* harness's last session, so it must not hand
    /// one harness's directory to another — the same rule
    /// `continuing_conversation` already applies to roots and secrets.
    #[tokio::test]
    async fn continuing_takes_the_directory_of_that_harnesss_own_last_session() {
        let store = Store::in_memory().unwrap();
        let opencode = store
            .new_conversation(HarnessKind::OpenCode, "/tmp/opencode-project", None)
            .unwrap();
        store
            .set_conversation_session(&opencode.id, Some("ses_oc"))
            .unwrap();
        let claude = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/claude-project", None)
            .unwrap();
        store
            .set_conversation_session(&claude.id, Some("ses_cc"))
            .unwrap();

        assert_eq!(
            session_cwd(&store, &Resume::Last, HarnessKind::OpenCode).unwrap(),
            Some(PathBuf::from("/tmp/opencode-project"))
        );
        assert_eq!(
            session_cwd(&store, &Resume::Last, HarnessKind::ClaudeCode).unwrap(),
            Some(PathBuf::from("/tmp/claude-project"))
        );
    }

    /// Deleting the wrong work takes every transcript in it, so an ambiguous
    /// prefix is refused rather than guessed.
    #[test]
    fn a_work_prefix_that_matches_two_works_is_refused() {
        let store = Store::in_memory().unwrap();
        let first = store.create_work("port the parser").unwrap();
        let second = store.create_work("and the tests").unwrap();

        assert_eq!(resolve_work(&store, &first.id).unwrap(), first.id);
        assert_eq!(
            resolve_work(&store, &first.id[..8]).unwrap(),
            first.id,
            "a prefix long enough to be unique should resolve"
        );
        assert!(resolve_work(&store, "nothing-like-this").is_err());
        // Both uuids start with a hex digit, so *some* one-character prefix is
        // shared: whichever it is must be refused rather than picked.
        let shared: String = first.id.chars().take(1).collect();
        if second.id.starts_with(&shared) {
            assert!(resolve_work(&store, &shared).is_err());
        }
    }

    /// Both refusals are the point, and neither is the CLI's to relax: the main
    /// chat is the one conversation that is always there, and a session cut out
    /// of a work leaves a tree pointing at something that is gone.
    #[test]
    fn deleting_a_conversation_refuses_the_main_chat_and_anything_inside_a_work() {
        let store = Store::in_memory().unwrap();
        let main = store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp/repo")
            .unwrap();
        assert!(store.delete_conversation(&main).is_err());

        let work = store.create_work("port the parser").unwrap();
        let session = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id;
        store
            .attach_conversation(&session, &work.id, None, jod_core::works::Origin::Orchestrator)
            .unwrap();
        assert!(store.delete_conversation(&session).is_err());

        // An ordinary one goes.
        let loose = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id;
        store.delete_conversation(&loose).unwrap();
        assert!(store.conversation(&loose).unwrap().is_none());
    }

    /// D8: a work with nothing on disk deletes on the *first* command, because
    /// there is nothing to lose by it. The repeat exists to protect worktrees,
    /// and making every delete need two commands would teach people to type it
    /// twice without reading the first answer.
    // `Jod::with_store` starts the task that drains the event channel, so this
    // needs a runtime even though nothing here is awaited.
    #[tokio::test]
    async fn deleting_a_work_that_holds_no_worktree_goes_through_first_time() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let jod = Jod::with_store(store.clone());
        let work = store.create_work("port the parser").unwrap();

        work_command(&jod, WorkCommand::Delete { id: work.id.clone() }).unwrap();
        assert!(store.work(&work.id).unwrap().is_none());
        // And the id stops resolving, so a repeat says so rather than
        // reporting a second success.
        assert!(work_command(&jod, WorkCommand::Delete { id: work.id }).is_err());
    }

    /// Discovery was written, tested and reachable from nothing. This asserts
    /// the whole loop rather than any one piece: a real command file on disk is
    /// scanned, cached, and read back out — which is the path the palette will
    /// take, and the one that was missing.
    #[tokio::test]
    async fn listing_commands_scans_the_disk_caches_it_and_reads_it_back() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let jod = Jod::with_store(store.clone());
        let root = std::env::temp_dir().join(format!("jod-commands-{}", std::process::id()));
        let commands = root.join(".claude/commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(
            commands.join("ship-it.md"),
            "---\ndescription: open a draft PR\n---\nbody\n",
        )
        .unwrap();

        commands_command(
            &jod,
            CommandsCommand::Ls {
                conversation: None,
                roots: vec![root.clone()],
                harness: None,
                cached: false,
                json: false,
            },
        )
        .unwrap();

        let cached = store.discovered(None).unwrap();
        let found = cached
            .iter()
            .find(|d| d.name == "ship-it")
            .expect("the scan did not reach the cache");
        assert_eq!(found.description, "open a draft PR");
        // A command is offered to the harness whose convention it follows and
        // to no other: Jod never forwards one across conventions.
        assert_eq!(found.harness, HarnessKind::ClaudeCode.id());
        assert!(store
            .discovered(Some(HarnessKind::OpenCode))
            .unwrap()
            .iter()
            .all(|d| d.name != "ship-it"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_root_command_with_no_conversation_and_no_main_chat_says_so() {
        let store = Store::in_memory().unwrap();
        let refused = which_conversation(&store, None).unwrap_err().to_string();
        assert!(refused.contains("--conversation"), "{refused}");
    }

    /// A secret card whose scope has nothing to attach to must not quietly
    /// become a second global bucket — a key meant for one work handed to every
    /// session on the box is the failure the default scope exists to prevent.
    #[test]
    fn answering_a_work_scoped_secret_card_with_no_work_is_refused_before_anything_is_read() {
        use jod_core::cards::{CardKind, NewCard};
        let store = Store::in_memory().unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id;
        let card = store
            .raise_card(NewCard {
                conversation_id: conversation,
                kind: Some(CardKind::Secret),
                title: "STRIPE_API_KEY needed".into(),
                secret_name: Some("STRIPE_API_KEY".into()),
                secret_scope: Some("work".into()),
                ..NewCard::default()
            })
            .unwrap();

        let refused = answer_secret_card(&store, &card).unwrap_err().to_string();
        assert!(refused.contains("--global"), "{refused}");
        assert!(
            store.secret_names(jod_core::secrets::Scope::Global, "").unwrap().is_empty(),
            "a refusal still stored something"
        );
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

    // ---- the team tree's own help ----

    /// Every positional argument under `jod team` says what it is.
    ///
    /// `jod team task <TEAM> <ID> [TITLE]...` shipped with three blank
    /// `Arguments:` rows, and this is the agent board's own entry point — so
    /// the only way to learn what an `ID` was, or where one came from, was to
    /// read `TeamCommand`. Asserted rather than fixed once, because a tenth
    /// subcommand written the same way would reintroduce it silently: a bare
    /// `field: String` compiles, runs, and documents nothing.
    ///
    /// The second half is the part that keeps the text honest. `/// Id` is
    /// non-empty and answers nothing, so a description that only echoes the
    /// argument's own name counts as no description at all.
    #[test]
    fn every_positional_under_team_says_what_it_is() {
        use clap::CommandFactory;

        fn walk(command: &clap::Command, path: &str, blank: &mut Vec<String>) {
            for arg in command.get_arguments() {
                if !arg.is_positional() || arg.is_hide_set() {
                    continue;
                }
                let name = arg.get_id().to_string();
                let help = arg
                    .get_help()
                    .or_else(|| arg.get_long_help())
                    .map(|h| h.to_string())
                    .unwrap_or_default();
                let says_nothing = help.trim().is_empty()
                    || help
                        .trim()
                        .trim_end_matches('.')
                        .eq_ignore_ascii_case(&name.replace('_', " "));
                if says_nothing {
                    blank.push(format!("{path} <{}>", name.to_uppercase()));
                }
            }
            for sub in command.get_subcommands() {
                // clap writes `help` itself; its arguments are not ours to
                // document.
                if sub.get_name() == "help" {
                    continue;
                }
                walk(sub, &format!("{path} {}", sub.get_name()), blank);
            }
        }

        // Built first, so this reads the same tree `--help` prints from rather
        // than the derive's raw one.
        let mut cli = Cli::command();
        cli.build();
        let team = cli
            .find_subcommand("team")
            .expect("no `team` subcommand")
            .clone();
        let mut blank = Vec::new();
        walk(&team, "jod team", &mut blank);
        assert!(
            blank.is_empty(),
            "these positionals print an empty help row, so the only way to \
             learn what to type is to read the source: {blank:?}"
        );
    }
}
