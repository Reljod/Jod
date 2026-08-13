//! Slash commands.
//!
//! Parsing is separated from doing, so the whole of "what did the user ask
//! for" is a pure function over a string and can be tested without a terminal,
//! a store or an agent.
//!
//! The set is deliberately smaller than OpenCode's. Every command here maps
//! onto something Jod can actually do; a command that would need a capability
//! the harness seam does not expose is *absent* rather than present and inert,
//! because a `/compact` that silently does nothing is worse than no `/compact`.
//! Unrecognised input is reported, never swallowed.

use jod_core::{HarnessKind, PermissionPolicy};

use super::config;
use super::workspace::Workspace;

/// What a `/…` line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slash {
    Help,
    /// Use a different harness for the next turn.
    Harness(HarnessKind),
    /// Set the model, or clear it back to the harness default.
    Model(Option<String>),
    /// Set how much the agent may do without asking, or — with no argument —
    /// move to the next mode, which is what Tab does.
    Mode(Option<PermissionPolicy>),
    Thinking,
    /// Show or hide what tools gave back.
    Details,
    /// Read or change a preference that outlives the session.
    Config(config::Request),
    /// The directories this conversation may work in.
    ///
    /// One command with three shapes rather than three commands, because they
    /// are one subject and the palette is already long: `/root` lists, `/root
    /// add` opens the picker, `/root rm <path>` removes.
    Root(RootCmd),
    /// The repositories an instruction that names none is resolved against.
    ///
    /// Shaped like [`Slash::Root`] and for the same reason: one subject, two
    /// verbs, one word to remember. It is deliberately the *narrow* half of
    /// `jod project` — see [`ProjectCmd`].
    Project(ProjectCmd),
    /// Start a fresh conversation, forgetting the session cursor.
    New,
    /// List conversations that can be resumed.
    Sessions,
    /// Continue a specific conversation by its harness-assigned id.
    Resume(String),
    /// Go to a workspace. One variant for all nine, because the palette and the
    /// which-key menu must reach the same set — a screen you can open one way
    /// and not the other is a screen half the users never find.
    Open(Workspace),
    /// Go to a workspace and land the cursor on a named row.
    OpenNamed(Workspace, String),
    /// The memory list, optionally with the filter already typed in.
    Memory(Option<String>),
    /// `/new schedule|goal|hook|memory|task` — start making one.
    NewKind(Workspace),
    Pause(String),
    Unpause(String),
    /// Fire a schedule, or run one iteration of a goal, now.
    Run(String),
    /// Assert one fact. A triple rather than a sentence, because that is what
    /// `Store::remember` stores and splitting a sentence into three would be
    /// Jod guessing which word is the relation.
    Remember {
        subject: String,
        predicate: String,
        object: String,
    },
    Forget(String),
    /// Start an agent that runs without taking over the screen.
    Delegate(String),
    /// Hand an instruction to the orchestrator — the pinned main chat.
    ///
    /// Distinct from [`Slash::Delegate`], which starts one agent on one prompt.
    /// This decides *what kind of thing* the instruction is: continue an agent
    /// already holding the context, start a new one, arm a schedule, or set a
    /// goal. The screen gets the decision and the reason for it.
    Main(String),
    /// `/main` with nothing after it — go and live in the main chat rather than
    /// send one instruction to it. The same destination as `⏎` on the fleet's
    /// pinned row.
    EnterMain,
    /// Stop an agent, by an id prefix or its name.
    Stop(String),
    /// Put an agent's output on screen.
    Watch(String),
    /// Keep a heartbeat on an agent, so a run that wedges is reaped.
    ///
    /// Deliberately *not* spelled `/watch`, which already means "put this on
    /// screen". The two are opposites in the way that matters: `/watch` is for
    /// a run you are looking at, and this is for one you are going to walk away
    /// from for a few hours.
    Heartbeat { which: String, on: bool },
    /// Say how to attach to an agent's tmux session.
    Attach(String),
    /// Put a task on the watched team's board.
    Todo(String),
    /// Mark one of those tasks finished.
    Done(String),
    /// Clear the transcript on screen. The conversation is untouched.
    Clear,
    /// The background shells this console started, running and finished.
    Jobs,
    /// Restart the console into whatever `jod` is on disk now.
    Reload,
    /// Update the binaries this console is running from.
    ///
    /// `check` is the whole difference between "tell me" and "do it", and it
    /// is a separate word rather than a separate command because the two are
    /// the same question asked with different consequences — a user who typed
    /// the wrong one should be one word away from the right one.
    Update {
        check: bool,
    },
    /// Install the newest *release* of the binaries this console runs from,
    /// downloaded prebuilt rather than rebuilt from a checkout.
    ///
    /// A separate variant rather than an alias of [`Slash::Update`], because
    /// at a shell the two words already name two different acts — `jod update`
    /// rebuilds within the installed MAJOR.MINOR, `jod upgrade` downloads the
    /// newest release whatever its major and minor. A console where `/upgrade`
    /// quietly did the first would make the same word mean two things
    /// depending on where it was typed.
    Upgrade {
        check: bool,
    },
    Exit,
    /// A `/word` nobody knows. Reported rather than sent to the agent.
    Unknown(String),
    /// A known command missing its argument.
    NeedsArgument(&'static str),
    /// A command understood, and not carried out, with the reason already
    /// written — a preference asked for a value it does not take, say.
    ///
    /// Distinct from [`Slash::Unknown`], which is a command nobody has heard
    /// of, and from [`Slash::NeedsArgument`], which is one whose argument is
    /// simply absent. Here the argument was present and wrong, and the useful
    /// sentence names what *would* have worked — which only the thing that
    /// rejected it knows.
    Refused(String),
}

/// Parse a line as a slash command.
///
/// `None` means "this is not a command" — including a bare `/`, and anything
/// with leading whitespace, so a prompt that happens to start with a slash
/// (`/usr/bin/foo is missing`) still reaches the agent as long as it is a real
/// path rather than a single word.
/// What `/root` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootCmd {
    List,
    /// `None` opens the picker; a path adds it without one, which is what a
    /// script or a paste wants.
    Add(Option<String>),
    /// Open the picker somewhere other than the directory `jod` was launched
    /// in. Distinct from [`RootCmd::Add`] with a path, which adds that exact
    /// directory and offers no choice — here the path says *where to browse*,
    /// which is the only way to reach a tree the launch directory does not
    /// contain.
    AddFrom(String),
    Remove(String),
}

/// What `/project` was asked to do.
///
/// Two verbs where `jod project` has four. `archive` and `restore` are catalog
/// housekeeping — they need a name you can only have got by listing first, and
/// neither is what stops an instruction resolving. The gap this closes is that
/// a catalog cannot be *started* from the console, so the two verbs that fill
/// it are the two that are here; the rest stay at the shell, where the flags
/// they take (`--name`, `--alias`, `--notes`, `--json`) belong. Prose
/// arguments only, for the reason `/heartbeat` takes `off` rather than
/// `--off`: a single flag in a set that has none is a spelling nobody guesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCmd {
    List,
    /// `None` catalogs the directory the console was launched in, exactly as
    /// `jod project add` with no path catalogs the shell's working directory.
    Add(Option<String>),
}

pub fn parse(line: &str) -> Option<Slash> {
    let rest = line.strip_prefix('/')?;
    let mut parts = rest.split_whitespace();
    let name = parts.next()?.to_ascii_lowercase();
    let arg = parts.collect::<Vec<_>>().join(" ");
    let arg = arg.trim();

    Some(match name.as_str() {
        "help" | "?" => Slash::Help,
        "harness" | "agent" => match harness_named(arg) {
            Some(kind) => Slash::Harness(kind),
            None if arg.is_empty() => Slash::NeedsArgument("/harness <claude|opencode|agy>"),
            None => Slash::Unknown(format!("/harness {arg}")),
        },
        "model" | "models" => {
            if arg.is_empty() || arg == "default" || arg == "clear" {
                Slash::Model(None)
            } else {
                Slash::Model(Some(arg.to_string()))
            }
        }
        // `/mode` with no argument cycles, so the command and the Tab key mean
        // the same thing rather than being two ways to reach one setting that
        // disagree about what "no argument" means.
        "mode" | "permission" | "permissions" => match jod_core::mcp::parse_permission(arg) {
            Some(mode) => Slash::Mode(Some(mode)),
            None if arg.is_empty() => Slash::Mode(None),
            None => Slash::Unknown(format!("/mode {arg}")),
        },
        "thinking" | "reasoning" => Slash::Thinking,
        "details" | "output" => Slash::Details,
        // The whole argument is handed to `config`, which owns what a
        // preference is called and what it takes. A refusal comes back as the
        // sentence to show rather than as a bare "no", because the useful part
        // is which values *are* accepted.
        "config" | "prefs" | "preferences" | "settings" => match config::request(arg) {
            Ok(request) => Slash::Config(request),
            Err(said) => Slash::Refused(said),
        },
        // `/new` alone is still a fresh conversation, which is what it has
        // always meant — and now also the way out of the main chat, since both
        // are the one binding. `/new schedule` is the form ladder's front door.
        "new" => match kind_from(arg) {
            Some(ws) => Slash::NewKind(ws),
            None if arg.is_empty() => Slash::New,
            None => Slash::Unknown(format!("/new {arg}")),
        },
        "sessions" => Slash::Sessions,
        "root" | "roots" => {
            let mut words = arg.split_whitespace();
            match words.next() {
                None | Some("ls") | Some("list") => Slash::Root(RootCmd::List),
                // The picker with no argument, a literal path with one. Both
                // are "add", because which of the two you meant is obvious from
                // whether you typed a path.
                Some("add") => {
                    let path = words.collect::<Vec<_>>().join(" ");
                    Slash::Root(RootCmd::Add((!path.is_empty()).then_some(path)))
                }
                Some("rm") | Some("remove") => {
                    let path = words.collect::<Vec<_>>().join(" ");
                    if path.is_empty() {
                        Slash::Unknown("/root rm needs a path".into())
                    } else {
                        Slash::Root(RootCmd::Remove(path))
                    }
                }
                Some(other) => Slash::Unknown(format!("/root {other}")),
            }
        }
        // The folder-first spelling, and the one people arrive with from other
        // consoles. `/root` is a noun you have to know Jod uses; `/add-dir` is
        // the thing you are trying to do, so it is the name in `/help`.
        //
        // Bare, it is exactly `/root add` — the same picker, at the same base.
        // With an argument it is [`RootCmd::AddFrom`], because somebody who
        // names a directory here is nearly always naming the *parent* of the
        // one they want, and `.` is the picker's first row so "this one,
        // exactly" is still a single `⏎`.
        "add-dir" | "adddir" | "add_dir" => {
            if arg.is_empty() {
                Slash::Root(RootCmd::Add(None))
            } else {
                Slash::Root(RootCmd::AddFrom(arg.to_string()))
            }
        }
        // Bare `/project` lists, for the reason bare `/root` does: half the
        // people who type it mean "show me" and the other half will reach for
        // the subcommand out of habit. Anything that is neither verb is named
        // back rather than read as a path — `/project ~/Jod` is a person
        // guessing at the shape, and cataloguing on a guess writes a row.
        "project" | "projects" | "repo" | "repos" => {
            let mut words = arg.split_whitespace();
            match words.next() {
                None | Some("ls") | Some("list") => Slash::Project(ProjectCmd::List),
                Some("add") => {
                    let path = words.collect::<Vec<_>>().join(" ");
                    Slash::Project(ProjectCmd::Add((!path.is_empty()).then_some(path)))
                }
                Some(other) => Slash::Unknown(format!("/project {other}")),
            }
        }
        "memory" | "memories" => {
            if arg.is_empty() {
                Slash::Memory(None)
            } else {
                Slash::Memory(Some(arg.to_string()))
            }
        }
        "graph" => Slash::Open(Workspace::Memory),
        "schedules" | "cron" => Slash::Open(Workspace::Schedules),
        "schedule" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/schedule <name>")
            } else {
                Slash::OpenNamed(Workspace::Schedules, arg.to_string())
            }
        }
        "goals" => Slash::Open(Workspace::Goals),
        "goal" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/goal <name>")
            } else {
                Slash::OpenNamed(Workspace::Goals, arg.to_string())
            }
        }
        "hooks" | "webhooks" => Slash::Open(Workspace::Hooks),
        "hook" | "webhook" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/hook <name>")
            } else {
                Slash::OpenNamed(Workspace::Hooks, arg.to_string())
            }
        }
        "tasks" | "board" => Slash::Open(Workspace::Tasks),
        "activity" | "inbox" => Slash::Open(Workspace::Activity),
        "pause" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/pause <name>")
            } else {
                Slash::Pause(arg.to_string())
            }
        }
        "unpause" | "resume-schedule" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/unpause <name>")
            } else {
                Slash::Unpause(arg.to_string())
            }
        }
        "run" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/run <name>")
            } else {
                Slash::Run(arg.to_string())
            }
        }
        "remember" => match triple(arg) {
            Some((subject, predicate, object)) => Slash::Remember {
                subject,
                predicate,
                object,
            },
            None if arg.is_empty() => Slash::NeedsArgument(REMEMBER_USAGE),
            None => Slash::Refused(format!(
                "“{arg}” is a sentence, and memory holds triples — {REMEMBER_USAGE}"
            )),
        },
        "forget" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/forget <name>")
            } else {
                Slash::Forget(arg.to_string())
            }
        }
        "resume" | "continue" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/resume <session-id>")
            } else {
                Slash::Resume(arg.to_string())
            }
        }
        "agents" | "fleet" => Slash::Open(Workspace::Fleet),
        "team" => Slash::Open(Workspace::Team),
        "delegate" | "bg" | "spawn" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/delegate <prompt>")
            } else {
                Slash::Delegate(arg.to_string())
            }
        }
        // `/main` and `/jod` both, because the second is what people type when
        // they mean "you decide" and the first is what the CLI verb is called.
        //
        // Bare `/main` is not a missing argument. It mirrors the CLI, where
        // `jod main "…"` sends and `jod main` opens the chat — and it is the
        // keyboard's way to the same place `⏎` on the fleet's top row goes.
        "main" | "jod" => {
            if arg.is_empty() {
                Slash::EnterMain
            } else {
                Slash::Main(arg.to_string())
            }
        }
        "stop" | "kill" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/stop <id>")
            } else {
                Slash::Stop(arg.to_string())
            }
        }
        "watch" | "focus" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/watch <id>")
            } else {
                Slash::Watch(arg.to_string())
            }
        }
        // `/heartbeat <id>` arms it, `/heartbeat <id> off` disarms it. A
        // trailing word rather than a flag, because the TUI's commands take
        // prose arguments and `--off` would be the only flag in the set.
        "heartbeat" | "hb" => {
            let (which, tail) = match arg.split_once(char::is_whitespace) {
                Some((head, rest)) => (head, rest.trim()),
                None => (arg, ""),
            };
            if which.is_empty() {
                Slash::NeedsArgument("/heartbeat <id> [off]")
            } else {
                match tail {
                    "" | "on" => Slash::Heartbeat {
                        which: which.to_string(),
                        on: true,
                    },
                    "off" | "stop" | "no" => Slash::Heartbeat {
                        which: which.to_string(),
                        on: false,
                    },
                    other => Slash::Refused(format!(
                        "{other} is not on or off — /heartbeat <id> [off]"
                    )),
                }
            }
        }
        "attach" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/attach <id>")
            } else {
                Slash::Attach(arg.to_string())
            }
        }
        "todo" | "task" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/todo <title>")
            } else {
                Slash::Todo(arg.to_string())
            }
        }
        "done" | "finish" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/done <task-id>")
            } else {
                Slash::Done(arg.to_string())
            }
        }
        "clear" => Slash::Clear,
        // Not `bg`: that already means `/delegate`, and a word that means
        // "start one" on one line and "list them" on the next is a trap.
        "jobs" | "shells" => Slash::Jobs,
        "reload" | "restart" => Slash::Reload,
        "update" => match arg.trim_start_matches("--") {
            "" => Slash::Update { check: false },
            "check" | "dry-run" | "n" => Slash::Update { check: true },
            other => Slash::Refused(format!(
                "“{other}” is not something /update takes — /update installs the \
                 newest patch, /update check says what it would install"
            )),
        },
        // Still deliberately not `/upgrade <version>`. Naming a version is how
        // you land on a release nobody is on by accident, and the console
        // mid-session is the wrong place to decide that — `jod upgrade
        // --version` at a shell is. Taking the newest published release is a
        // different act, and it is the one this offers.
        "upgrade" => match arg.trim_start_matches("--") {
            "" => Slash::Upgrade { check: false },
            "check" | "dry-run" | "n" => Slash::Upgrade { check: true },
            other => Slash::Refused(format!(
                "“{other}” is not something /upgrade takes — /upgrade installs the \
                 newest release, /upgrade check says what it would install. To land \
                 on a specific one, run `jod upgrade --version {other}` at a shell"
            )),
        },
        "exit" | "quit" | "q" => Slash::Exit,
        other => Slash::Unknown(format!("/{other}")),
    })
}

/// How a fact is typed. Shown in `/help`, in the refusal and in the overlay, so
/// there is one spelling of the shape to learn.
pub const REMEMBER_USAGE: &str = "/remember <subject> | <predicate> | <object>";

/// Split a typed fact into its three parts.
///
/// A pipe rather than whitespace, because every part is a phrase: `reljod |
/// prefers | linear for tasks` is three fields and `reljod prefers linear for
/// tasks` is a sentence that only a model could split. `None` for anything that
/// is not exactly three non-empty parts — refused, never guessed at, because a
/// fact filed under the wrong subject is worse than one never filed.
pub(super) fn triple(arg: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = arg.split('|').map(str::trim).collect();
    match parts.as_slice() {
        [subject, predicate, object]
            if !subject.is_empty() && !predicate.is_empty() && !object.is_empty() =>
        {
            Some((
                subject.to_string(),
                predicate.to_string(),
                object.to_string(),
            ))
        }
        _ => None,
    }
}

/// What `/new <kind>` is asking to make. Named after the singular of the
/// screen, because that is the word on the screen you just came from.
fn kind_from(name: &str) -> Option<Workspace> {
    Some(match name.to_ascii_lowercase().as_str() {
        "schedule" | "cron" | "timer" => Workspace::Schedules,
        "goal" => Workspace::Goals,
        "hook" | "webhook" => Workspace::Hooks,
        "memory" | "fact" | "belief" => Workspace::Memory,
        "task" | "todo" => Workspace::Tasks,
        _ => return None,
    })
}

/// The harness a typed word names, in every spelling `/harness` accepts —
/// including each kind's stored `id()`, so a value read back out of the
/// database parses here too. `pub(super)` because [`super::config`] must accept
/// exactly the same words: two parsers for one setting is how `claude-code`
/// ends up working in one place and not the other.
pub(super) fn harness_named(name: &str) -> Option<HarnessKind> {
    match name.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" | "cc" => Some(HarnessKind::ClaudeCode),
        "opencode" | "open-code" | "open_code" | "oc" => Some(HarnessKind::OpenCode),
        "agy" | "antigravity" => Some(HarnessKind::Agy),
        _ => None,
    }
}

/// One line of `/help`, so the list and the parser cannot drift apart: every
/// command that appears here is one `parse` accepts.
pub const HELP: &[(&str, &str)] = &[
    ("/help", "this list"),
    (
        "/harness <name>",
        "claude, opencode or agy — takes effect next turn",
    ),
    (
        "/model <name>",
        "set the model for this conversation; no argument restores the default",
    ),
    (
        "/mode [name]",
        "plan, ask, edits or auto; no argument cycles (Tab)",
    ),
    ("/thinking", "show or hide reasoning — remembered"),
    ("/details", "show or hide what tools returned — remembered"),
    (
        "/config [key] [value]",
        "preferences that outlive the session",
    ),
    (
        "/new [kind]",
        "a fresh conversation, or a new schedule/goal/hook/task",
    ),
    (
        "/add-dir [where]",
        "pick a folder this session can work in and `@` — a path browses there, not here",
    ),
    (
        "/root [add|rm]",
        "the directories this session works in (Ctrl-P picks one)",
    ),
    (
        "/project [add]",
        "the repositories an instruction resolves against; `add [path]` catalogs one",
    ),
    ("/sessions", "conversations you can pick up"),
    ("/resume <id>", "continue one of them"),
    ("/delegate <prompt>", "run it in the background (Ctrl-B)"),
    ("/main", "go into the main chat — the pinned one"),
    (
        "/main <instruction>",
        "send it one instruction and stay where you are",
    ),
    ("/agents", "the fleet (Ctrl-F, Ctrl-G f)"),
    ("/watch <id>", "put an agent's output on screen"),
    (
        "/heartbeat <id> [off]",
        "reap it if it goes silent — for runs you leave alone for hours",
    ),
    ("/stop <id>", "stop an agent and close its session"),
    ("/attach <id>", "how to attach to its tmux session"),
    ("/memory [query]", "what Jod remembers (Ctrl-G m)"),
    ("/schedules", "cron-triggered runs (Ctrl-G s)"),
    ("/schedule <name>", "open one of them"),
    ("/goals", "looping objectives (Ctrl-G g)"),
    ("/goal <name>", "open one of them"),
    ("/hooks", "webhook rules (Ctrl-G h)"),
    ("/hook <name>", "open one of them"),
    ("/tasks", "the board as a screen (Ctrl-G t)"),
    ("/activity", "what happened while you were away (Ctrl-G a)"),
    ("/run <name>", "fire a schedule or a goal iteration now"),
    ("/pause <name>", "stop a schedule or goal firing"),
    ("/unpause <name>", "arm it again"),
    (
        "/remember <s> | <p> | <o>",
        "assert one fact — subject, relation, value",
    ),
    ("/forget <name>", "drop a memory node"),
    ("/team", "the team panel (Ctrl-G w)"),
    ("/todo <title>", "put a task on the team's board"),
    ("/done <task-id>", "mark one of those tasks finished"),
    ("/clear", "clear the transcript on screen"),
    ("/jobs", "background shells — what is building (Ctrl-G j)"),
    (
        "/reload",
        "restart this console into the jod that is on disk now",
    ),
    (
        "/update",
        "rebuild and install the newest patch of Jod; 'check' just says what it would do",
    ),
    (
        "/upgrade",
        "download and install the newest release of Jod; 'check' just says what it would do",
    ),
    ("/exit", "leave; running agents keep going"),
];

/// The kinds `/new` accepts, offered rather than remembered.
const KINDS: [&str; 5] = ["schedule", "goal", "hook", "memory", "task"];

/// One thing the completion popup can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The whole line to put in the input if this is chosen.
    pub line: String,
    /// What is shown next to it.
    pub hint: String,
}

impl Completion {
    fn new(line: impl Into<String>, hint: impl Into<String>) -> Completion {
        Completion {
            line: line.into(),
            hint: hint.into(),
        }
    }
}

/// What could complete the line being typed.
///
/// Empty means "no popup": either this is not a command, or it is already
/// finished. Completing arguments as well as names matters more than it looks
/// — `/harness ` is the point where a user has to remember three spellings, and
/// the commands that take an agent id are otherwise a UUID-retyping exercise,
/// so the live fleet is offered there.
/// The repository's own commands, as palette rows.
///
/// **Marked with their source**, and that is not decoration: `/review` from
/// Jod and `/review` from the checkout you happen to be in are different
/// things, and a palette that showed them identically would make which one
/// fired a matter of ordering. The mark says `repo` or `user`, and `skill` or
/// `command`, because those are the two facts that decide what it will do.
///
/// `app.discovered` is already filtered to the harness on screen — see
/// `data::discovered` — so nothing here can offer a command that would not
/// resolve.
fn repo_commands(typed: &str, app: &crate::tui::App) -> Vec<Completion> {
    app.discovered
        .iter()
        .filter(|found| found.name.to_ascii_lowercase().starts_with(typed))
        .map(|found| {
            // `Root` is the repository's own; the rest come from the user's
            // config or a plugin and are available everywhere. Which of the
            // two it is decides whether the command travels with the checkout,
            // which is the fact worth a column.
            let source = match found.scope {
                jod_core::commands::Scope::Root => "repo",
                jod_core::commands::Scope::User => "user",
                jod_core::commands::Scope::Plugin => "plugin",
            };
            let what = if found.description.trim().is_empty() {
                String::new()
            } else {
                format!(" · {}", found.description.trim())
            };
            Completion::new(
                format!("/{} ", found.name),
                format!("{source} {}{what}", found.kind.as_str()),
            )
        })
        .collect()
}

/// The repository command a typed line names, and how to send it to `harness`.
///
/// `None` when the line names none, which leaves every existing path
/// untouched: an unknown `/word` is still prose, as it was.
///
/// The spelling comes from [`Discovered::invoke`] rather than from anything
/// here. Claude Code and AGY expand `/name` straight out of the prompt;
/// OpenCode needs the name in `run --command <name>`. That was measured once,
/// lives in `commands.rs`, and reimplementing the branch at this call site is
/// how the two copies would drift.
pub fn repo_invocation(
    line: &str,
    app: &crate::tui::App,
) -> Option<(String, jod_core::commands::Invocation)> {
    let rest = line.trim().strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next()?;
    let args = parts.next().unwrap_or("").trim();
    let found = app
        .discovered
        .iter()
        .find(|found| found.name.eq_ignore_ascii_case(name))?;
    // A refusal here means the harness moved between the palette being built
    // and the line being sent. Dropping to `None` puts it back on the ordinary
    // prose path rather than sending a spelling the harness cannot resolve.
    let invocation = found.invoke(app.harness, args).ok()?;
    Some((found.name.clone(), invocation))
}

pub fn completions(input: &str, app: &crate::tui::App) -> Vec<Completion> {
    let agents = &app.agents;
    let Some(rest) = input.strip_prefix('/') else {
        return vec![];
    };

    // Still typing the command word: offer names — Jod's own first, then
    // whatever this repository brought.
    if !rest.contains(char::is_whitespace) {
        let typed = rest.to_ascii_lowercase();
        let mut offered: Vec<Completion> = HELP
            .iter()
            .filter(|(usage, _)| {
                usage
                    .split_whitespace()
                    .next()
                    .is_some_and(|name| name[1..].starts_with(&typed))
            })
            .map(|(usage, hint)| {
                let name = usage.split_whitespace().next().unwrap();
                let takes_argument = usage.contains('<');
                // A command that takes an argument gets a trailing space, so
                // accepting it leaves the cursor where the argument goes.
                let line = if takes_argument {
                    format!("{name} ")
                } else {
                    name.to_string()
                };
                Completion::new(line, *hint)
            })
            .collect();
        offered.extend(repo_commands(&typed, app));
        return offered;
    }

    // Past the name: offer arguments for the commands that have a fixed set.
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let typed = parts
        .next()
        .unwrap_or_default()
        .trim_start()
        .to_ascii_lowercase();

    match name.as_str() {
        "harness" | "agent" => HarnessKind::ALL
            .into_iter()
            .filter(|k| k.id().replace('_', "").starts_with(&typed) || k.id().starts_with(&typed))
            .map(|k| Completion::new(format!("/{name} {}", short_name(k)), k.label()))
            .collect(),
        // Whichever agents are still worth naming. `/stop` only offers the ones
        // that could actually be stopped, so the list answers "what can I do"
        // rather than merely "what exists".
        // `/heartbeat` completes over running agents only, for the same reason
        // `/stop` does: watching a finished run is a row that retires on the
        // next sweep having done nothing.
        "stop" | "kill" | "watch" | "focus" | "attach" | "heartbeat" | "hb" => agents
            .iter()
            .filter(|a| {
                !matches!(name.as_str(), "stop" | "kill" | "heartbeat" | "hb") || a.is_running()
            })
            .filter(|a| a.id.starts_with(&typed))
            .map(|a| {
                let id: String = a.id.chars().take(8).collect();
                Completion::new(
                    format!("/{name} {id}"),
                    format!("{} · {}", a.status, a.name),
                )
            })
            .collect(),
        // Four spellings nobody should have to remember, offered with what
        // each one actually costs you.
        "mode" | "permission" | "permissions" => PermissionPolicy::ALL
            .into_iter()
            .filter(|m| m.label().starts_with(&typed))
            .map(|m| {
                let what = match m {
                    PermissionPolicy::Plan => "read and reason; change nothing",
                    PermissionPolicy::Ask => "check with me first — denies when nobody answers",
                    PermissionPolicy::AcceptEdits => "edits go through; the rest asks",
                    PermissionPolicy::Bypass => "everything auto-approved",
                };
                Completion::new(format!("/{name} {}", m.label()), what)
            })
            .collect(),
        // Whatever this harness said it accepts, in its own spelling. This is
        // the one completion list where getting it wrong costs a turn: a model
        // name is not validated at the prompt, it is handed to `--model`, and a
        // harness that does not recognise it fails the run.
        //
        // Matched anywhere in the id rather than at the front, because the
        // useful half of `opencode/claude-sonnet-5` is the half a prefix match
        // cannot reach — nobody types the provider first.
        "model" | "models" => {
            let mut out = Vec::new();
            // Offered first and always, because it is the one answer no
            // harness lists: not a model, the absence of one.
            if "default".starts_with(&typed) {
                out.push(Completion::new(
                    format!("/{name} default"),
                    "whatever the harness picks itself",
                ));
            }
            out.extend(
                app.models
                    .iter()
                    .filter(|m| m.id.to_ascii_lowercase().contains(&typed))
                    .map(|m| Completion::new(format!("/{name} {}", m.id), m.label.clone())),
            );
            out
        }
        // The two verbs, offered rather than remembered. Without this the only
        // way to learn that `/project` takes `add` is the one-line hint in
        // `/help`, which scrolls — and a catalog you cannot start is the whole
        // of the bug this command exists to close.
        "project" | "projects" | "repo" | "repos" => [
            (
                "add ",
                "catalog a repository — no path means the one Jod was launched in",
            ),
            ("ls", "the catalog, most recently worked in first"),
        ]
        .into_iter()
        .filter(|(verb, _)| verb.trim_end().starts_with(&typed))
        .map(|(verb, what)| Completion::new(format!("/{name} {verb}"), what))
        .collect(),
        "new" => KINDS
            .iter()
            .filter(|kind| kind.starts_with(&typed))
            .map(|kind| Completion::new(format!("/new {kind}"), format!("a new {kind}")))
            .collect(),
        "config" | "prefs" | "preferences" | "settings" => config_completions(&name, &typed, app),
        // The same reasoning as the agent ids: retyping a name off the screen
        // above is not a user interface. A schedule and a goal are both things
        // you pause, run and un-pause, so both are offered on those verbs.
        "schedule" => named(
            &name,
            &typed,
            app.schedules.iter().map(|s| (&s.name, &s.gloss)),
        ),
        "goal" => named(
            &name,
            &typed,
            app.goals.iter().map(|g| (&g.name, &g.cadence)),
        ),
        "hook" | "webhook" => named(&name, &typed, app.hooks.iter().map(|h| (&h.name, &h.repo))),
        "forget" => named(&name, &typed, app.memory.iter().map(|n| (&n.name, &n.body))),
        "pause" | "unpause" | "run" => {
            let schedules = app.schedules.iter().map(|s| (&s.name, &s.gloss));
            let goals = app.goals.iter().map(|g| (&g.name, &g.cadence));
            named(&name, &typed, schedules.chain(goals))
        }
        _ => vec![],
    }
}

/// Preference names, and then that preference's values once one is named.
///
/// The values matter more than the names here. `on`/`off` is guessable and
/// `plan | ask | edits | auto` is not, and a preference whose spelling you
/// cannot recall is one that stays at its default for ever. `model` is the
/// extreme of that: nobody recalls `opencode/claude-opus-5`, which is why this
/// takes the app — the live list is the only thing that makes the preference
/// usable, and it is the same list `/model` offers rather than a second copy.
fn config_completions(command: &str, typed: &str, app: &crate::tui::App) -> Vec<Completion> {
    match typed.split_once(char::is_whitespace) {
        // Still on the key. A trailing space, because every preference takes a
        // value and the cursor should land where it goes.
        None => config::Pref::ALL
            .into_iter()
            .filter(|p| p.name().starts_with(typed))
            .map(|p| Completion::new(format!("/{command} {} ", p.name()), p.what()))
            .collect(),
        Some((name, value)) => {
            let Some(pref) = config::Pref::named(name) else {
                return vec![];
            };
            let value = value.trim_start().to_ascii_lowercase();
            let mut out: Vec<Completion> = pref
                .choices()
                .into_iter()
                .filter(|choice| choice.starts_with(&value))
                .map(|choice| {
                    Completion::new(format!("/{command} {} {choice}", pref.name()), pref.what())
                })
                .collect();
            // The half of `model`'s list that no pure function can hold. Matched
            // anywhere in the id and labelled by the harness, for the reasons
            // `/model`'s own arm gives — this is that list, reached through a
            // different command, so it behaves the same way.
            if pref == config::Pref::Model {
                out.extend(
                    app.models
                        .iter()
                        .filter(|m| m.id.to_ascii_lowercase().contains(&value))
                        .map(|m| {
                            Completion::new(
                                format!("/{command} {} {}", pref.name(), m.id),
                                m.label.clone(),
                            )
                        }),
                );
            }
            out
        }
    }
}

/// Offer live names for a command that takes one.
fn named<'a>(
    command: &str,
    typed: &str,
    rows: impl Iterator<Item = (&'a String, &'a String)>,
) -> Vec<Completion> {
    rows.filter(|(name, _)| name.to_ascii_lowercase().starts_with(typed))
        .map(|(name, hint)| Completion::new(format!("/{command} {name}"), hint.clone()))
        .collect()
}

/// The spelling offered for a harness — the shortest one `parse` accepts.
fn short_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::ClaudeCode => "claude",
        HarnessKind::OpenCode => "opencode",
        HarnessKind::Agy => "agy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completions read live rows off the app, so a fixture app is what the
    /// tests hand them. Only the fleet varies here; the other lists are empty
    /// until their loaders land.
    fn fleet(agents: &[crate::tui::AgentLine]) -> crate::tui::App {
        let mut app = crate::tui::App::new(HarnessKind::ClaudeCode, None, jod_core::Resume::Fresh);
        app.agents = agents.to_vec();
        app
    }

    fn lines(input: &str) -> Vec<String> {
        completions(input, &fleet(&[]))
            .into_iter()
            .map(|c| c.line)
            .collect()
    }

    fn agent(id: &str, status: &str) -> crate::tui::AgentLine {
        crate::tui::AgentLine {
            delivery: crate::tui::delivery::Verdict::Nothing,
            id: id.into(),
            name: "port the parser".into(),
            harness: "Claude Code".into(),
            status: status.into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            cwd: "/srv/reljod/repo".into(),
            last: None,
        }
    }

    /// An app carrying a model list, the way the loader leaves one.
    fn with_models(ids: &[(&str, &str)]) -> crate::tui::App {
        let mut app = fleet(&[]);
        app.models = ids
            .iter()
            .map(|(id, label)| jod_core::Model {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect();
        app.models_for = Some(HarnessKind::ClaudeCode);
        app
    }

    fn model_lines(input: &str, app: &crate::tui::App) -> Vec<String> {
        completions(input, app)
            .into_iter()
            .map(|c| c.line)
            .collect()
    }

    /// The whole point: `/model ` shows what this harness accepts rather than
    /// nothing, so the name does not have to be remembered.
    #[test]
    fn model_offers_the_harnesss_own_list() {
        let app = with_models(&[("opus", "the latest Opus"), ("haiku", "fastest")]);
        assert_eq!(
            model_lines("/model ", &app),
            vec!["/model default", "/model opus", "/model haiku"]
        );
    }

    /// Typing filters, and it filters on any part of the id — a provider-first
    /// id like `opencode/claude-sonnet-5` is unreachable by prefix, because
    /// what you remember is the model, not who serves it.
    #[test]
    fn typing_filters_the_list_anywhere_in_the_name() {
        let app = with_models(&[
            ("opencode/claude-sonnet-5", "opencode"),
            ("opencode/gemini-3.1-pro", "opencode"),
        ]);
        assert_eq!(
            model_lines("/model sonnet", &app),
            vec!["/model opencode/claude-sonnet-5"]
        );
        // And the filter is not case-sensitive, because the ids are lowercase
        // and the shift key is not a search term.
        assert_eq!(
            model_lines("/model GEMINI", &app),
            vec!["/model opencode/gemini-3.1-pro"]
        );
    }

    /// `default` is offered by name because it is the one answer no harness
    /// The preference and the command offer one list, because they set the same
    /// kind of thing and a second copy is how the two drift apart.
    #[test]
    fn the_model_preference_offers_the_harnesss_own_list_too() {
        let app = with_models(&[
            ("opencode/claude-sonnet-5", "opencode"),
            ("opencode/gemini-3.1-pro", "opencode"),
        ]);
        assert_eq!(
            model_lines("/config model ", &app),
            vec![
                "/config model default",
                "/config model opencode/claude-sonnet-5",
                "/config model opencode/gemini-3.1-pro",
            ]
        );
        // Matched anywhere in the id, for the reason `/model` is: nobody types
        // the provider first.
        assert_eq!(
            model_lines("/config model gemini", &app),
            vec!["/config model opencode/gemini-3.1-pro"]
        );
    }

    /// The other preferences must not pick up model names on their way past the
    /// new branch.
    #[test]
    fn a_preference_that_is_not_the_model_offers_only_its_own_values() {
        let app = with_models(&[("opus", "the latest Opus")]);
        assert_eq!(
            model_lines("/config mode ", &app),
            vec![
                "/config mode plan",
                "/config mode ask",
                "/config mode edits",
                "/config mode auto",
            ]
        );
    }

    /// Every preference is reachable by name, the new one included.
    #[test]
    fn config_offers_the_model_preference_among_the_keys() {
        assert!(
            lines("/config mod").iter().any(|l| l == "/config model "),
            "{:?}",
            lines("/config mod")
        );
    }

    /// lists, and it is what `parse` reads as "clear the model".
    #[test]
    fn default_is_offered_and_means_clear() {
        let app = with_models(&[("opus", "the latest Opus")]);
        assert_eq!(model_lines("/model def", &app), vec!["/model default"]);
        assert_eq!(parse("/model default"), Some(Slash::Model(None)));
    }

    /// Before the list arrives — or when the harness is not installed — the
    /// only thing offered is the one option that is always true. A popup that
    /// claimed the harness had no models would be a lie.
    #[test]
    fn an_unloaded_list_still_offers_the_default() {
        assert_eq!(model_lines("/model ", &fleet(&[])), vec!["/model default"]);
    }

    #[test]
    fn a_plain_prompt_offers_no_completions() {
        assert!(completions("hello", &fleet(&[])).is_empty());
        assert!(completions("", &fleet(&[])).is_empty());
    }

    #[test]
    fn a_bare_slash_offers_everything() {
        assert_eq!(completions("/", &fleet(&[])).len(), HELP.len());
    }

    #[test]
    fn typing_narrows_the_list() {
        let some = lines("/t");
        assert!(some.contains(&"/thinking".to_string()));
        assert!(some.contains(&"/team".to_string()));
        assert!(!some.contains(&"/help".to_string()));

        let one = lines("/th");
        assert_eq!(one, vec!["/thinking".to_string()]);
    }

    #[test]
    fn a_command_taking_an_argument_completes_with_a_trailing_space() {
        assert_eq!(lines("/harn"), vec!["/harness ".to_string()]);
        assert_eq!(
            lines("/hel"),
            vec!["/help".to_string()],
            "no argument, no space"
        );
    }

    #[test]
    fn nonsense_completes_to_nothing() {
        assert!(completions("/zzzz", &fleet(&[])).is_empty());
    }

    /// The bit that saves remembering three spellings.
    #[test]
    fn harness_arguments_are_completed_too() {
        let all = lines("/harness ");
        assert_eq!(all.len(), HarnessKind::ALL.len());
        assert!(all.contains(&"/harness claude".to_string()));
        assert!(all.contains(&"/harness agy".to_string()));

        assert_eq!(lines("/harness op"), vec!["/harness opencode".to_string()]);
    }

    /// Every offered harness spelling must be one the parser accepts, or the
    /// popup would suggest something that then fails.
    #[test]
    fn every_suggested_harness_parses() {
        for c in completions("/harness ", &fleet(&[])) {
            assert!(
                matches!(parse(&c.line), Some(Slash::Harness(_))),
                "{} was suggested but does not parse",
                c.line
            );
        }
    }

    /// Likewise for command names: accepting a suggestion must never produce
    /// something the parser calls unknown.
    #[test]
    fn every_suggested_command_parses() {
        for c in completions("/", &fleet(&[])) {
            let parsed = parse(c.line.trim());
            assert!(
                !matches!(parsed, Some(Slash::Unknown(_)) | None),
                "{} was suggested but parses as {parsed:?}",
                c.line
            );
        }
    }

    /// A command needing an argument completes to itself plus a space, which is
    /// how Enter used to be swallowed instead of running the command.
    #[test]
    fn a_command_needing_an_argument_completes_to_itself() {
        assert_eq!(lines("/resume"), vec!["/resume "]);
        assert_eq!(lines("/harness"), vec!["/harness "]);
        // Trimmed, the only suggestion is what is already typed — so there is
        // nothing to accept and Enter must run it.
        for input in ["/resume", "/harness"] {
            let only = &completions(input, &fleet(&[]))[0].line;
            assert_eq!(only.trim_end(), input.trim_end());
        }
    }

    #[test]
    fn a_plain_prompt_is_not_a_command() {
        assert!(parse("hello there").is_none());
        assert!(parse("").is_none());
        assert!(parse("what about / this").is_none());
    }

    /// A bare slash is a typo, not a command, and must not be swallowed.
    #[test]
    fn a_bare_slash_is_not_a_command() {
        assert!(parse("/").is_none());
        assert!(parse("/   ").is_none());
    }

    #[test]
    fn help_answers_to_both_spellings() {
        assert_eq!(parse("/help"), Some(Slash::Help));
        assert_eq!(parse("/?"), Some(Slash::Help));
    }

    #[test]
    fn every_harness_can_be_named_including_its_short_forms() {
        for (text, expected) in [
            ("/harness claude", HarnessKind::ClaudeCode),
            ("/harness cc", HarnessKind::ClaudeCode),
            ("/harness opencode", HarnessKind::OpenCode),
            ("/harness oc", HarnessKind::OpenCode),
            ("/harness agy", HarnessKind::Agy),
            ("/harness antigravity", HarnessKind::Agy),
        ] {
            assert_eq!(parse(text), Some(Slash::Harness(expected)), "{text}");
        }
    }

    /// Every harness the build knows must be reachable by name, or a new one
    /// would be spawnable from the CLI and invisible from the TUI.
    #[test]
    fn no_harness_is_unreachable_from_the_tui() {
        for kind in HarnessKind::ALL {
            let by_id = parse(&format!("/harness {}", kind.id()));
            let by_label = parse(&format!("/harness {}", kind.label().replace(' ', "-")));
            assert!(
                by_id == Some(Slash::Harness(kind)) || by_label == Some(Slash::Harness(kind)),
                "{kind:?} cannot be selected: {by_id:?} / {by_label:?}"
            );
        }
    }

    #[test]
    fn an_unknown_harness_is_reported_rather_than_guessed() {
        assert_eq!(
            parse("/harness gpt"),
            Some(Slash::Unknown("/harness gpt".into()))
        );
    }

    #[test]
    fn harness_without_an_argument_says_what_it_wants() {
        assert_eq!(
            parse("/harness"),
            Some(Slash::NeedsArgument("/harness <claude|opencode|agy>"))
        );
    }

    #[test]
    fn model_takes_a_name_or_resets_to_the_default() {
        assert_eq!(
            parse("/model anthropic/claude-sonnet-5"),
            Some(Slash::Model(Some("anthropic/claude-sonnet-5".into())))
        );
        assert_eq!(parse("/model"), Some(Slash::Model(None)));
        assert_eq!(parse("/model default"), Some(Slash::Model(None)));
        assert_eq!(parse("/model clear"), Some(Slash::Model(None)));
    }

    /// Every mode has to be nameable, or a mode would exist that Tab can reach
    /// and nobody can ask for directly.
    #[test]
    fn every_mode_can_be_named_and_no_argument_means_cycle() {
        for mode in PermissionPolicy::ALL {
            assert_eq!(
                parse(&format!("/mode {}", mode.label())),
                Some(Slash::Mode(Some(mode))),
                "{} is not accepted",
                mode.label()
            );
        }
        assert_eq!(parse("/mode"), Some(Slash::Mode(None)), "bare /mode cycles");
        // The harnesses' own spellings, since that is what a person has read.
        assert_eq!(
            parse("/mode manual"),
            Some(Slash::Mode(Some(PermissionPolicy::Ask)))
        );
        assert_eq!(
            parse("/mode bypass"),
            Some(Slash::Mode(Some(PermissionPolicy::Bypass)))
        );
    }

    #[test]
    fn an_unknown_mode_is_reported_rather_than_guessed() {
        assert_eq!(
            parse("/mode yolo"),
            Some(Slash::Unknown("/mode yolo".into()))
        );
    }

    /// Offering a mode the parser rejects would be a popup that suggests a
    /// mistake.
    #[test]
    fn every_suggested_mode_parses() {
        let offered = completions("/mode ", &fleet(&[]));
        assert_eq!(offered.len(), PermissionPolicy::ALL.len());
        for c in offered {
            assert!(
                matches!(parse(&c.line), Some(Slash::Mode(Some(_)))),
                "{} was suggested but does not parse",
                c.line
            );
        }
    }

    #[test]
    fn resume_needs_an_id() {
        assert_eq!(parse("/resume ses-1"), Some(Slash::Resume("ses-1".into())));
        assert_eq!(
            parse("/continue ses-1"),
            Some(Slash::Resume("ses-1".into()))
        );
        assert_eq!(
            parse("/resume"),
            Some(Slash::NeedsArgument("/resume <session-id>"))
        );
    }

    #[test]
    fn the_simple_commands_all_parse() {
        assert_eq!(parse("/thinking"), Some(Slash::Thinking));
        assert_eq!(parse("/details"), Some(Slash::Details));
        assert_eq!(parse("/output"), Some(Slash::Details));
        assert_eq!(parse("/reasoning"), Some(Slash::Thinking));
        assert_eq!(parse("/new"), Some(Slash::New));
        assert_eq!(parse("/sessions"), Some(Slash::Sessions));
        // One command, three shapes. Bare and `ls` both list, because half the
        // people who type `/root` mean "show me" and the other half will type
        // the subcommand out of habit.
        assert_eq!(parse("/root"), Some(Slash::Root(RootCmd::List)));
        assert_eq!(parse("/roots"), Some(Slash::Root(RootCmd::List)));
        assert_eq!(parse("/root ls"), Some(Slash::Root(RootCmd::List)));
        assert_eq!(parse("/root add"), Some(Slash::Root(RootCmd::Add(None))));
        assert_eq!(
            parse("/root add /home/reljod/repo/Jod"),
            Some(Slash::Root(RootCmd::Add(Some("/home/reljod/repo/Jod".into()))))
        );
        assert_eq!(
            parse("/root rm /home/reljod/repo/Jod"),
            Some(Slash::Root(RootCmd::Remove("/home/reljod/repo/Jod".into())))
        );
        // A removal with nothing to remove is refused by name rather than
        // silently becoming a list, which would look like the key did nothing.
        assert!(matches!(parse("/root rm"), Some(Slash::Unknown(_))));
    }

    /// Bare `/add-dir` is `/root add` and nothing else — the same picker, so
    /// the folder-first name is a name and not a second implementation.
    #[test]
    fn add_dir_with_nothing_after_it_is_the_picker_where_you_are() {
        assert_eq!(parse("/add-dir"), Some(Slash::Root(RootCmd::Add(None))));
        assert_eq!(parse("/add-dir"), parse("/root add"));
        // Three spellings, because the hyphen is the part nobody remembers.
        assert_eq!(parse("/adddir"), parse("/add-dir"));
        assert_eq!(parse("/add_dir"), parse("/add-dir"));
        assert_eq!(parse("/ADD-DIR"), parse("/add-dir"), "and the case is not");
    }

    /// The argument says *where to browse*, which is the whole reason the
    /// command exists: without it no tree outside the launch directory is
    /// reachable at all.
    #[test]
    fn add_dir_with_a_path_browses_there_rather_than_adding_it_blind() {
        assert_eq!(
            parse("/add-dir ~/Developer"),
            Some(Slash::Root(RootCmd::AddFrom("~/Developer".into())))
        );
        // Deliberately *not* `RootCmd::Add`: `/root add <path>` adds exactly
        // that directory, and these two must not collapse into each other.
        assert_ne!(
            parse("/add-dir /home/reljod/repo"),
            parse("/root add /home/reljod/repo")
        );
    }

    /// A folder with a space in its name is a folder, not a subcommand and an
    /// argument — `parse` rejoins what `split_whitespace` took apart.
    #[test]
    fn a_directory_name_with_a_space_survives_parsing() {
        assert_eq!(
            parse("/add-dir ~/My Projects"),
            Some(Slash::Root(RootCmd::AddFrom("~/My Projects".into())))
        );
        // `/agents` and `/team` now name workspaces rather than panels, which
        // is what lets one variant cover all nine screens.
        assert_eq!(parse("/agents"), Some(Slash::Open(Workspace::Fleet)));
        assert_eq!(parse("/team"), Some(Slash::Open(Workspace::Team)));
        assert_eq!(parse("/clear"), Some(Slash::Clear));
        for text in ["/exit", "/quit", "/q"] {
            assert_eq!(parse(text), Some(Slash::Exit), "{text}");
        }
    }

    #[test]
    fn commands_are_case_insensitive_and_tolerate_spacing() {
        assert_eq!(parse("/HELP"), Some(Slash::Help));
        assert_eq!(parse("/Thinking"), Some(Slash::Thinking));
        assert_eq!(
            parse("/harness    OpenCode"),
            Some(Slash::Harness(HarnessKind::OpenCode))
        );
    }

    #[test]
    fn an_unknown_command_is_named_back_rather_than_sent_to_the_agent() {
        assert_eq!(parse("/wibble"), Some(Slash::Unknown("/wibble".into())));
        // The ones OpenCode has and Jod does not: reported, not silently inert.
        for missing in ["/compact", "/undo", "/share", "/themes"] {
            assert_eq!(
                parse(missing),
                Some(Slash::Unknown(missing.into())),
                "{missing}"
            );
        }
    }

    #[test]
    fn the_agent_management_commands_all_parse() {
        assert_eq!(
            parse("/delegate audit the deps"),
            Some(Slash::Delegate("audit the deps".into()))
        );
        assert_eq!(
            parse("/bg audit the deps"),
            Some(Slash::Delegate("audit the deps".into()))
        );
        assert_eq!(parse("/stop abc123"), Some(Slash::Stop("abc123".into())));
        assert_eq!(parse("/kill abc123"), Some(Slash::Stop("abc123".into())));
        assert_eq!(parse("/watch abc123"), Some(Slash::Watch("abc123".into())));
        assert_eq!(parse("/focus abc123"), Some(Slash::Watch("abc123".into())));
        assert_eq!(
            parse("/heartbeat abc123"),
            Some(Slash::Heartbeat {
                which: "abc123".into(),
                on: true
            })
        );
        assert_eq!(
            parse("/hb abc123 off"),
            Some(Slash::Heartbeat {
                which: "abc123".into(),
                on: false
            })
        );
        assert_eq!(
            parse("/attach abc123"),
            Some(Slash::Attach("abc123".into()))
        );
        assert_eq!(
            parse("/todo port the parser"),
            Some(Slash::Todo("port the parser".into()))
        );
        assert_eq!(
            parse("/done port-the-parser"),
            Some(Slash::Done("port-the-parser".into()))
        );
    }

    /// `/main` is not `/delegate`. `/delegate` starts one agent on one prompt;
    /// `/main` hands the instruction over and lets the orchestrator decide
    /// whether it is a continuation, a new agent, a schedule or a goal.
    #[test]
    fn the_orchestrator_is_reachable_from_the_chat() {
        assert_eq!(
            parse("/main every weekday at 8am, sweep the PRs"),
            Some(Slash::Main("every weekday at 8am, sweep the PRs".into()))
        );
        assert_eq!(
            parse("/jod do the thing"),
            Some(Slash::Main("do the thing".into()))
        );
        // Bare `/main` is the other verb, not a missing argument: it goes into
        // the chat rather than sending to it, mirroring `jod main` with no
        // words. Refusing it left the pinned chat with no keyboard route in.
        assert_eq!(parse("/main"), Some(Slash::EnterMain));
        assert_eq!(parse("/jod"), Some(Slash::EnterMain));
    }

    /// `/watch` puts a run on screen; `/heartbeat` is what you arm on the run
    /// you are about to stop looking at. Two commands with adjacent names and
    /// opposite purposes, so the parse must never collapse one into the other.
    #[test]
    fn watching_a_run_and_keeping_a_heartbeat_on_it_are_different_commands() {
        assert_eq!(parse("/watch abc"), Some(Slash::Watch("abc".into())));
        assert_eq!(
            parse("/heartbeat abc"),
            Some(Slash::Heartbeat {
                which: "abc".into(),
                on: true
            })
        );
    }

    /// Anything other than on/off is refused rather than guessed at. Reading a
    /// stray word as "on" would arm a heartbeat somebody was trying to disarm.
    #[test]
    fn a_heartbeat_argument_that_is_neither_on_nor_off_is_refused() {
        assert!(matches!(
            parse("/heartbeat abc maybe"),
            Some(Slash::Refused(_))
        ));
        for text in ["/heartbeat abc off", "/heartbeat abc stop", "/heartbeat abc no"] {
            assert_eq!(
                parse(text),
                Some(Slash::Heartbeat {
                    which: "abc".into(),
                    on: false
                }),
                "{text}"
            );
        }
    }

    /// Each of these does something irreversible or unguessable without its
    /// argument, so a bare one must ask rather than act.
    #[test]
    fn the_agent_management_commands_all_want_an_argument() {
        for (text, usage) in [
            ("/delegate", "/delegate <prompt>"),
            ("/stop", "/stop <id>"),
            ("/watch", "/watch <id>"),
            ("/heartbeat", "/heartbeat <id> [off]"),
            ("/attach", "/attach <id>"),
            ("/todo", "/todo <title>"),
            ("/done", "/done <task-id>"),
        ] {
            assert_eq!(parse(text), Some(Slash::NeedsArgument(usage)), "{text}");
        }
    }

    /// Retyping a UUID is not a user interface.
    #[test]
    fn the_live_agents_complete_the_commands_that_name_one() {
        let agents = [
            agent("abcdef1234", "running"),
            agent("99887766", "completed"),
        ];
        let offered = completions("/watch ", &fleet(&agents))
            .into_iter()
            .map(|c| c.line)
            .collect::<Vec<_>>();
        assert_eq!(offered, vec!["/watch abcdef12", "/watch 99887766"]);

        assert_eq!(
            completions("/watch abc", &fleet(&agents))[0].line,
            "/watch abcdef12",
            "typing narrows it"
        );
    }

    /// `/stop` offers what could actually be stopped, so the list answers "what
    /// can I do" rather than merely "what exists".
    #[test]
    fn stopping_only_offers_the_agents_that_are_still_running() {
        let agents = [
            agent("abcdef1234", "running"),
            agent("99887766", "completed"),
        ];
        let offered = completions("/stop ", &fleet(&agents))
            .into_iter()
            .map(|c| c.line)
            .collect::<Vec<_>>();
        assert_eq!(offered, vec!["/stop abcdef12"]);
    }

    /// The hint is what tells you which of two eight-character ids is which.
    #[test]
    fn an_offered_agent_is_described_by_its_status_and_name() {
        let agents = [agent("abcdef1234", "running")];
        assert_eq!(
            completions("/watch ", &fleet(&agents))[0].hint,
            "running · port the parser"
        );
    }

    #[test]
    fn naming_an_agent_completes_to_nothing_when_there_are_none() {
        assert!(completions("/stop ", &fleet(&[])).is_empty());
    }

    // ---- /config ----

    use crate::tui::config::{Pref, Request, Value};

    #[test]
    fn config_with_no_argument_asks_for_the_whole_list() {
        assert_eq!(parse("/config"), Some(Slash::Config(Request::List)));
        assert_eq!(parse("/settings"), Some(Slash::Config(Request::List)));
        assert_eq!(parse("/prefs"), Some(Slash::Config(Request::List)));
    }

    #[test]
    fn config_shows_one_preference_and_sets_one() {
        assert_eq!(
            parse("/config thinking"),
            Some(Slash::Config(Request::Show(Pref::Thinking)))
        );
        assert_eq!(
            parse("/config thinking off"),
            Some(Slash::Config(Request::Set(
                Pref::Thinking,
                Value::Flag(false)
            )))
        );
        assert_eq!(
            parse("/config mode plan"),
            Some(Slash::Config(Request::Set(
                Pref::Mode,
                Value::Mode(PermissionPolicy::Plan)
            )))
        );
    }

    /// The rule the module doc states at the top of this file: never silently
    /// accepted. A preference that looks set and is not is the worst outcome.
    #[test]
    fn an_unknown_preference_is_refused_with_the_real_ones_named() {
        let Some(Slash::Refused(said)) = parse("/config colour green") else {
            panic!(
                "an unknown key was not refused: {:?}",
                parse("/config colour green")
            );
        };
        assert!(said.contains("colour"), "{said}");
        assert!(said.contains("thinking"), "{said}");
    }

    #[test]
    fn a_value_a_preference_does_not_take_is_refused_rather_than_stored() {
        let Some(Slash::Refused(said)) = parse("/config mode yolo") else {
            panic!("a bad value was not refused");
        };
        assert!(said.contains("yolo") && said.contains("plan"), "{said}");
    }

    /// Offering a preference or a value the parser then rejects would be a
    /// popup that teaches a mistake — the same rule `/harness` and `/mode` keep.
    #[test]
    fn every_suggested_preference_and_value_parses() {
        let keys = completions("/config ", &fleet(&[]));
        assert_eq!(keys.len(), Pref::ALL.len());
        for c in keys {
            let parsed = parse(c.line.trim());
            assert!(
                matches!(parsed, Some(Slash::Config(_))),
                "{} was suggested but parses as {parsed:?}",
                c.line
            );
        }
        for pref in Pref::ALL {
            let offered = completions(&format!("/config {} ", pref.name()), &fleet(&[]));
            assert!(!offered.is_empty(), "{} offers no values", pref.name());
            for c in offered {
                // `Set` or `Clear`. Every value used to be a `Set`, because
                // every preference had a closed list of values and none of them
                // was spelled `default`. `model` has no closed list, so the one
                // value it can always offer is the word that means "no name" —
                // and that word is the same one `/config <key> default` reads as
                // giving the choice up. Both are the popup being obeyed rather
                // than rejected, which is what this test is for; insisting on
                // `Set` here would only force `model` to hide its one
                // universally-valid answer.
                let parsed = parse(&c.line);
                assert!(
                    matches!(
                        parsed,
                        Some(Slash::Config(Request::Set(_, _) | Request::Clear(_)))
                    ),
                    "{} was suggested but parses as {parsed:?}",
                    c.line
                );
            }
        }
    }

    #[test]
    fn typing_narrows_the_preferences_and_their_values() {
        assert_eq!(lines("/config th"), vec!["/config thinking "]);
        assert_eq!(
            lines("/config thinking o"),
            vec!["/config thinking on", "/config thinking off"]
        );
        assert!(completions("/config nonsense ", &fleet(&[])).is_empty());
    }

    // ---- /remember ----

    /// A fact is three fields. Typing a sentence and having Jod pick the
    /// relation out of it is a guess, and a fact filed under the wrong subject
    /// is worse than one never filed.
    #[test]
    fn remember_takes_a_triple() {
        assert_eq!(
            parse("/remember reljod | prefers | linear for tasks"),
            Some(Slash::Remember {
                subject: "reljod".into(),
                predicate: "prefers".into(),
                object: "linear for tasks".into(),
            })
        );
    }

    #[test]
    fn remember_refuses_a_sentence_and_says_what_it_wants() {
        let Some(Slash::Refused(said)) = parse("/remember linear is the system of record") else {
            panic!("a sentence was accepted as a fact");
        };
        assert!(said.contains(REMEMBER_USAGE), "{said}");

        // Two fields is the near miss, and it must not become subject-predicate
        // with an empty value.
        assert!(matches!(
            parse("/remember reljod | prefers"),
            Some(Slash::Refused(_))
        ));
        assert!(matches!(
            parse("/remember reljod |  | linear"),
            Some(Slash::Refused(_))
        ));
        assert_eq!(
            parse("/remember"),
            Some(Slash::NeedsArgument(REMEMBER_USAGE))
        );
    }

    // ---- /project ----

    /// The catalog could be filled by `jod project add` and by nothing inside
    /// the console: `/project` did not parse, so the only way to make the
    /// panel non-empty was to quit or open a second terminal.
    #[test]
    fn a_project_can_be_catalogued_and_listed_from_the_chat_box() {
        for text in ["/project", "/projects", "/project ls", "/project list"] {
            assert_eq!(
                parse(text),
                Some(Slash::Project(ProjectCmd::List)),
                "{text}"
            );
        }
        // Bare `add` is the directory Jod was launched in, which is the same
        // default `jod project add` takes from the shell.
        assert_eq!(
            parse("/project add"),
            Some(Slash::Project(ProjectCmd::Add(None)))
        );
        assert_eq!(
            parse("/project add /home/reljod/repo/Jod"),
            Some(Slash::Project(ProjectCmd::Add(Some(
                "/home/reljod/repo/Jod".into()
            ))))
        );
        // A checkout with a space in its name is one path, not a verb and an
        // argument — the same rejoin `/add-dir` does.
        assert_eq!(
            parse("/project add ~/My Projects/Jod"),
            Some(Slash::Project(ProjectCmd::Add(Some(
                "~/My Projects/Jod".into()
            ))))
        );
        // The name is case-insensitive like every other command's. The verb is
        // not, and deliberately: `/root` treats its subcommand the same way,
        // and one command tolerating a shouted verb while its twin does not is
        // worse than both being strict.
        assert_eq!(parse("/REPOS"), parse("/project"), "the name, at least");
        // Neither verb. Named back rather than read as a path: cataloguing on
        // a guess writes a row that has to be found again to be undone.
        assert!(matches!(parse("/project ~/Jod"), Some(Slash::Unknown(_))));
    }

    /// Typeable is not the same as findable, and the complaint was that `/`
    /// offered no route to the catalog at all.
    #[test]
    fn the_slash_list_offers_project_and_both_its_verbs() {
        assert!(
            lines("/").contains(&"/project".to_string()),
            "{:?}",
            lines("/")
        );
        assert_eq!(lines("/proj"), vec!["/project".to_string()]);

        let offered = lines("/project ");
        assert_eq!(offered, vec!["/project add ", "/project ls"]);
        // Nothing may be offered that the parser then calls unknown — the rule
        // `/harness`, `/mode` and `/config` each keep.
        for line in offered {
            let parsed = parse(line.trim_end());
            assert!(
                matches!(parsed, Some(Slash::Project(_))),
                "{line} was suggested but parses as {parsed:?}"
            );
        }
        assert_eq!(lines("/project a"), vec!["/project add "], "typing narrows");
    }

    /// `/help` must not list a command the parser rejects.
    #[test]
    fn every_documented_command_parses() {
        for (usage, _) in HELP {
            let word = usage.split_whitespace().next().unwrap();
            let parsed = parse(word).unwrap_or_else(|| panic!("{word} did not parse"));
            assert!(
                !matches!(parsed, Slash::Unknown(_)),
                "{usage} is documented but unknown to the parser"
            );
        }
    }
}
