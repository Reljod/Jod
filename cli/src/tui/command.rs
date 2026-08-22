//! Slash commands.
//!
//! Parsing is separated from doing, so the whole of "what did the user ask
//! for" is a pure function over a string and can be tested without a terminal,
//! a store or an agent.
//!
//! The set is deliberately smaller than OpenCode's. Every command here maps
//! onto something Jod can actually do; a command that would need a capability
//! the harness seam does not expose is *absent* rather than present and inert,
//! because a command that silently does nothing is worse than no command.
//! Unrecognised input is reported, never swallowed.
//!
//! `/compact` was the standing example of that rule and is now the example of
//! how it lifts: it stayed out while nothing behind the seam could shorten a
//! context, and arrived the day `Store::continue_as_new` could. The rule is
//! "earn the command", not "never grow".

use jod_core::{HarnessKind, PermissionPolicy};

use super::config;
use super::workspace::Workspace;

/// What a `/…` line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slash {
    Help,
    /// Use a different harness for the next turn.
    Harness(HarnessKind),
    /// Sign in to a harness, through the harness's own flow.
    ///
    /// `None` means the harness this conversation is on, which is the one that
    /// just refused to run — the whole reason anybody types this from here.
    /// `jod login` on the command line defaults to *every* harness instead,
    /// and the difference is deliberate: a terminal has one conversation in
    /// front of it and a shell does not.
    Login(Option<HarnessKind>),
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
    /// Start a fresh conversation, forgetting the session cursor.
    New,
    /// Continue a specific conversation by its harness-assigned id.
    Resume(String),
    /// Go to a workspace. One variant for all nine, because the palette and the
    /// which-key menu must reach the same set — a screen you can open one way
    /// and not the other is a screen half the users never find.
    Open(Workspace),
    /// Go to a workspace and land the cursor on a named row.
    OpenNamed(Workspace, String),
    /// `/new schedule|goal|hook|memory|task` — start making one.
    NewKind(Workspace),
    /// Assert one fact. A triple rather than a sentence, because that is what
    /// `Store::remember` stores and splitting a sentence into three would be
    /// Jod guessing which word is the relation.
    Remember {
        subject: String,
        predicate: String,
        object: String,
    },
    Forget(String),
    /// `/main` — go and live in the main chat. The same destination as `⏎` on
    /// the fleet's pinned row.
    ///
    /// It used to take an instruction as well, which handed one sentence to the
    /// orchestrator without moving you. That form is gone: the main chat is a
    /// place you go, and typing in it is already the way to instruct it.
    EnterMain,
    /// Stop an agent, by an id prefix or its name.
    Stop(String),
    /// Keep a heartbeat on an agent, so a run that wedges is reaped.
    ///
    /// For a run you are going to walk away from for a few hours. Putting one
    /// on screen is the fleet's own `⏎`, not a command.
    Heartbeat { which: String, on: bool },
    /// Start over: empty the screen and drop the context the next message
    /// would have carried. Jod's own transcript is kept.
    ///
    /// It used to mean the first half only, and that was the bug. Typed in the
    /// main chat it emptied the screen while the pinned conversation kept its
    /// harness session, so the next message resumed the whole history the user
    /// had just watched disappear. Telegram's `/clear` has always meant "drop
    /// the context window, keep the transcript", and the main chat is one chat
    /// across every surface — so the desk now means what the phone means.
    ///
    /// Distinct from [`Slash::New`], which drops the context *and* leaves the
    /// conversation. `/clear` keeps you where you are standing.
    Clear,
    /// Summarise this conversation and carry on from the summary.
    ///
    /// The half-measure `/clear` is not: it keeps the thread going instead of
    /// dropping what was said, at the cost of a model call to write the
    /// summary. Jod has no model of its own, so the harness on screen is asked
    /// to write it and the command finishes when that run does.
    ///
    /// This used to be absent on purpose — see the note at the top of this
    /// module — because nothing behind the harness seam could shorten a
    /// context. `Store::continue_as_new` is what changed: the thread is
    /// compacted and continues on the same harness with the summary as its
    /// first turn, so the next run resumes nothing.
    Compact,
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
///
/// The directories a session works in and the repositories it resolves against
/// are both edited from their own screens now — the picker on `Ctrl-G d`, the
/// fleet's own keys — rather than through `/root`, `/add-dir` and `/project`.
/// `jod project` at a shell is untouched.
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
        // `auth` as well as `login`, because a harness that has just refused
        // to authenticate has put that word in front of you, not this one.
        "login" | "auth" | "signin" => match harness_named(arg) {
            Some(kind) => Slash::Login(Some(kind)),
            None if arg.is_empty() => Slash::Login(None),
            None => Slash::Unknown(format!("/login {arg}")),
        },
        "model" | "models" => {
            if arg.is_empty() || arg == "default" || arg == "clear" {
                Slash::Model(None)
            } else if let Some(said) = model_refusal(arg) {
                Slash::Refused(said)
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
        // The screen, and only the screen. Opening one hook by name, pausing it
        // and firing it are all things you do with the cursor once you are
        // there, so they are the panel's keys rather than four more words to
        // remember.
        "hooks" | "webhooks" => Slash::Open(Workspace::Hooks),
        "tasks" | "board" => Slash::Open(Workspace::Tasks),
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
        // One word for the screen, and it is the screen's own name. `/agents`
        // was the second spelling and is gone: the panel is called the fleet
        // everywhere else — `Ctrl-F`, `Ctrl-G f`, the keybar — and a command
        // that answers to a name nothing on screen uses is one more thing to
        // learn for nothing.
        "fleet" => Slash::Open(Workspace::Fleet),
        // `/main` and `/jod` both, because the second is what people type when
        // they mean "you decide" and the first is what the CLI verb is called.
        //
        // It takes nothing, and an instruction typed after it is refused rather
        // than dropped. `/main <instruction>` used to hand one sentence to the
        // orchestrator from wherever you were standing; now `/main` takes you
        // there and typing is what instructs it. Swallowing the words would
        // lose a whole instruction to a command that looked like it worked.
        "main" | "jod" => {
            if arg.is_empty() {
                Slash::EnterMain
            } else {
                Slash::Refused(format!(
                    "/main takes you to the main chat and takes nothing else — go there \
                     with /main, then type “{}”",
                    truncated(arg)
                ))
            }
        }
        "stop" | "kill" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/stop <id>")
            } else {
                Slash::Stop(arg.to_string())
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
        "clear" => Slash::Clear,
        // `summarise` as well, because that is what it visibly does and it is
        // the word someone reaches for who has not read the help.
        "compact" | "summarise" | "summarize" => Slash::Compact,
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

/// The longest a model id gets. Not a real limit any harness documents — it is
/// the point past which the API itself has been seen to refuse the value
/// (`model: String should have at most 256 characters`), so anything at or
/// past it is refused here first, with a sentence that names the mistake.
const MODEL_MAX_LEN: usize = 256;

/// Why `/model <arg>` cannot be a model name, if it cannot be one.
///
/// Two checks only, and both are true for every harness Jod knows about:
/// a model id is one token, and none of them come close to
/// [`MODEL_MAX_LEN`]. That is deliberately short of "is this actually a model
/// this harness offers" — this function is a pure read of the typed string,
/// run before an `App` or a harness choice exists, so it has no model list to
/// check against. A harness's own list (`app.models`) is the finer sieve and
/// belongs where that list lives; this is the coarse one that catches what is
/// *always* wrong regardless of harness or list — chiefly a whole prompt, or a
/// long paste, landing in the model slot. Catching that here means it reads
/// back as Jod's own refusal immediately, not as the harness's "model not
/// found" a whole turn later.
fn model_refusal(arg: &str) -> Option<String> {
    let len = arg.chars().count();
    if len >= MODEL_MAX_LEN {
        return Some(format!(
            "/model does not take {len} characters — a model name is well under {MODEL_MAX_LEN}; try /model <name> or /model default"
        ));
    }
    if arg.contains(char::is_whitespace) {
        return Some(format!(
            "/model does not take “{}” — a model name is one word, no spaces; try /model <name> or /model default",
            truncated(arg)
        ));
    }
    None
}

/// `arg`, or the first stretch of it, so a refusal never puts a wall of text
/// on screen — the point of the sentence is the diagnosis, not an echo of the
/// whole mistake.
fn truncated(arg: &str) -> String {
    const SHOWN: usize = 60;
    if arg.chars().count() <= SHOWN {
        return arg.to_string();
    }
    let mut shown: String = arg.chars().take(SHOWN).collect();
    shown.push('…');
    shown
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
        "/login [name]",
        "sign in to a harness — no argument means the one this conversation is on",
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
        "/resume <id>",
        "carry on with a conversation by its id — the fleet lists them",
    ),
    ("/main", "go into the main chat — the pinned one"),
    ("/fleet", "the fleet (Ctrl-F, Ctrl-G f)"),
    (
        "/heartbeat <id> [off]",
        "reap it if it goes silent — for runs you leave alone for hours",
    ),
    ("/stop <id>", "stop an agent and close its session"),
    ("/schedules", "cron-triggered runs (Ctrl-G s)"),
    ("/schedule <name>", "open one of them"),
    ("/goals", "looping objectives (Ctrl-G g)"),
    ("/goal <name>", "open one of them"),
    ("/hooks", "webhook rules (Ctrl-G h)"),
    ("/tasks", "the board as a screen (Ctrl-G t)"),
    (
        "/remember <s> | <p> | <o>",
        "assert one fact — subject, relation, value",
    ),
    ("/forget <name>", "drop a memory node"),
    (
        "/clear",
        "empty the screen and start the next message with no context behind it",
    ),
    (
        "/compact",
        "summarise this conversation and carry on from the summary — happens on its own when the context fills",
    ),
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
    /// How the row reads in the palette.
    ///
    /// Separate from [`Completion::line`] because the two are not the same
    /// question. What gets inserted is only what can be typed for you; what
    /// gets *shown* has to say what the command does. Collapsing them listed
    /// `/main` twice — "go into the main chat" and "send it one instruction" —
    /// with nothing on either row to say that the second one takes an
    /// argument and the first one does not.
    pub label: String,
    /// What is shown next to it.
    pub hint: String,
}

impl Completion {
    /// A row that reads as the text it inserts. The trailing space an argument
    /// leaves behind is not part of the reading.
    fn new(line: impl Into<String>, hint: impl Into<String>) -> Completion {
        let line = line.into();
        Completion {
            label: line.trim_end().to_string(),
            line,
            hint: hint.into(),
        }
    }

    /// A row that reads as its whole usage — `/main <instruction>` — while
    /// inserting only the part that can be typed for you.
    fn usage(
        line: impl Into<String>,
        label: impl Into<String>,
        hint: impl Into<String>,
    ) -> Completion {
        Completion {
            line: line.into(),
            label: label.into(),
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
                // Shown as the help table writes it, argument and all. Two
                // commands can share a name and differ only in what follows
                // it — `/main` and `/main <instruction>` do the opposite
                // things — and a palette that prints the name alone makes
                // them one row typed twice.
                Completion::usage(line, *usage, *hint)
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
        // Whichever agents are still worth naming. Both of these only offer the
        // ones they could actually act on — a finished run is one that cannot
        // be stopped and has no heartbeat left to keep — so the list answers
        // "what can I do" rather than merely "what exists".
        "stop" | "kill" | "heartbeat" | "hb" => agents
            .iter()
            .filter(|a| a.is_running())
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
        "new" => KINDS
            .iter()
            .filter(|kind| kind.starts_with(&typed))
            .map(|kind| Completion::new(format!("/new {kind}"), format!("a new {kind}")))
            .collect(),
        "config" | "prefs" | "preferences" | "settings" => config_completions(&name, &typed, app),
        // The same reasoning as the agent ids: retyping a name off the screen
        // above is not a user interface.
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
        "forget" => named(&name, &typed, app.memory.iter().map(|n| (&n.name, &n.body))),
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
        assert!(some.contains(&"/tasks".to_string()));
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

    /// A whole sentence in the model slot — what the `harness-eats-prompt`
    /// sibling bug can put there if its own fix ever slips — is refused by
    /// Jod at the moment it is typed, not accepted and left for the harness to
    /// fail a turn later.
    #[test]
    fn model_refuses_a_value_with_whitespace() {
        assert_eq!(
            parse("/model please summarize the last three commits for me"),
            Some(Slash::Refused(
                "/model does not take \u{201c}please summarize the last three commits for \
                 me\u{201d} — a model name is one word, no spaces; try /model <name> or \
                 /model default"
                    .into()
            ))
        );
    }

    /// The API itself has been seen to refuse a model id at 256 characters
    /// (`model: String should have at most 256 characters`); Jod refuses it
    /// first, immediately, and says why.
    #[test]
    fn model_refuses_a_value_that_is_too_long() {
        let long = "a".repeat(300);
        match parse(&format!("/model {long}")) {
            Some(Slash::Refused(said)) => {
                assert!(said.contains("300 characters"), "{said}");
                assert!(!said.contains(&long), "refusal echoed the whole value: {said}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A single well-formed token — including one with punctuation a real
    /// model id uses, like a provider prefix — is still accepted exactly as
    /// before: this backstop is a coarse, harness-agnostic sieve, not a
    /// lookup against the model list.
    #[test]
    fn model_still_accepts_an_ordinary_looking_name() {
        assert_eq!(
            parse("/model opuss"),
            Some(Slash::Model(Some("opuss".into())))
        );
        assert_eq!(
            parse("/model claude-sonnet-5"),
            Some(Slash::Model(Some("claude-sonnet-5".into())))
        );
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
        // The fleet answers to the name the rest of the console calls it.
        assert_eq!(parse("/fleet"), Some(Slash::Open(Workspace::Fleet)));
        assert_eq!(parse("/clear"), Some(Slash::Clear));
        for text in ["/compact", "/summarise", "/summarize"] {
            assert_eq!(parse(text), Some(Slash::Compact), "{text}");
        }
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
        // `/compact` used to be on this list and has since been earned.
        for missing in ["/undo", "/share", "/themes"] {
            assert_eq!(
                parse(missing),
                Some(Slash::Unknown(missing.into())),
                "{missing}"
            );
        }
    }

    #[test]
    fn the_agent_management_commands_all_parse() {
        assert_eq!(parse("/stop abc123"), Some(Slash::Stop("abc123".into())));
        assert_eq!(parse("/kill abc123"), Some(Slash::Stop("abc123".into())));
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
    }

    /// `/main` is a place, not a message. It goes into the pinned chat, which
    /// mirrors `jod main` with no words, and once you are there typing is what
    /// instructs the orchestrator.
    #[test]
    fn the_main_chat_is_somewhere_you_go() {
        assert_eq!(parse("/main"), Some(Slash::EnterMain));
        assert_eq!(parse("/jod"), Some(Slash::EnterMain));
    }

    /// The old `/main <instruction>` handed one sentence over from wherever you
    /// were standing. It is gone, and the words are refused with the instruction
    /// quoted back — swallowing them would lose a whole instruction to a command
    /// that looked like it had worked.
    #[test]
    fn an_instruction_typed_after_main_is_refused_rather_than_dropped() {
        let refusal = parse("/main every weekday at 8am, sweep the PRs");
        let Some(Slash::Refused(said)) = refusal else {
            panic!("expected a refusal, got {refusal:?}");
        };
        assert!(said.contains("sweep the PRs"), "{said}");
        assert!(matches!(parse("/jod do the thing"), Some(Slash::Refused(_))));
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
            ("/stop", "/stop <id>"),
            ("/heartbeat", "/heartbeat <id> [off]"),
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
        let offered = completions("/heartbeat ", &fleet(&agents))
            .into_iter()
            .map(|c| c.line)
            .collect::<Vec<_>>();
        assert_eq!(offered, vec!["/heartbeat abcdef12"]);

        assert_eq!(
            completions("/heartbeat abc", &fleet(&agents))[0].line,
            "/heartbeat abcdef12",
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
            completions("/stop ", &fleet(&agents))[0].hint,
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

    // ---- what the palette no longer carries ----

    /// The commands that were cut. Every one of them is now an unknown word,
    /// which is the point: an unrecognised `/word` is named back on screen, so
    /// somebody typing an old habit is told it is gone rather than having the
    /// line sent to the agent as prose.
    ///
    /// Most of the work they did is still reachable, from the picker, the panel
    /// it belonged to, or a key on the row. Two are not, and are gone rather
    /// than moved: attaching to a run's tmux session, and pointing the root
    /// picker at a tree outside the launch directory. Both are shell commands
    /// now, and the empty states say so.
    #[test]
    fn the_retired_commands_are_reported_rather_than_silently_accepted() {
        for text in [
            // Directories and repositories: the picker on `Ctrl-G d`, and
            // `jod project` at a shell.
            "/add-dir ~/Developer",
            "/adddir",
            "/add_dir",
            "/root",
            "/root add",
            "/root rm /home/reljod/repo/Jod",
            "/project",
            "/projects",
            "/project add",
            "/project untrack tetris",
            "/repo untrack tetris",
            "/repos",
            // Panels, reached by their own screens.
            "/memory",
            "/memory linear",
            "/memories",
            "/activity",
            "/inbox",
            "/team",
            "/hook nightly",
            "/webhook nightly",
            // Schedules and goals: `r` fires one and `p` pauses it, on the row.
            "/run nightly-inbox",
            "/pause nightly-inbox",
            "/unpause nightly-inbox",
            "/resume-schedule nightly-inbox",
            // Agents. `Ctrl-B` still delegates and `⏎` on a fleet row still
            // watches. Attaching has no console route left at all — the fleet's
            // `a` went with the other fleet verbs — so `jod agents` at a shell,
            // which prints each run's tmux command, is the whole of it now.
            "/delegate audit the deps",
            "/bg audit the deps",
            "/spawn audit the deps",
            "/watch abc123",
            "/focus abc123",
            "/attach abc123",
            // The board: `/tasks` is the screen, and the row does the rest.
            "/todo port the parser",
            "/task port the parser",
            "/done port-the-parser",
            "/finish port-the-parser",
            // The fleet answers to one name now.
            "/agents",
        ] {
            assert!(
                matches!(parse(text), Some(Slash::Unknown(_))),
                "{text} parses as {:?} — it was supposed to be retired",
                parse(text)
            );
        }
    }

    /// The screens those commands opened are still reachable by the commands
    /// that were kept, so this is a smaller palette and not a smaller console.
    #[test]
    fn the_screens_those_commands_opened_are_still_reachable() {
        assert_eq!(parse("/fleet"), Some(Slash::Open(Workspace::Fleet)));
        assert_eq!(parse("/hooks"), Some(Slash::Open(Workspace::Hooks)));
        assert_eq!(parse("/tasks"), Some(Slash::Open(Workspace::Tasks)));
        assert_eq!(parse("/schedules"), Some(Slash::Open(Workspace::Schedules)));
        assert_eq!(parse("/goals"), Some(Slash::Open(Workspace::Goals)));
    }

    /// Nothing retired may still be offered by the popup: a palette row that
    /// parses as unknown is a suggestion that fails the moment it is accepted.
    #[test]
    fn no_retired_command_is_still_offered() {
        let offered = lines("/");
        for gone in [
            "/add-dir", "/root", "/project", "/memory", "/activity", "/team", "/delegate",
            "/watch", "/attach", "/todo", "/done", "/run", "/pause", "/unpause", "/agents",
            "/hook",
        ] {
            assert!(
                !offered.iter().any(|line| line.trim_end() == gone),
                "{gone} is still in the palette: {offered:?}"
            );
        }
    }

    /// The console is where the sign-in failure is met — a run dies
    /// unauthenticated in the transcript — so the fix has to be reachable from
    /// there without quitting the conversation to get to a shell.
    #[test]
    fn login_names_a_harness_or_means_the_one_on_screen() {
        assert_eq!(parse("/login"), Some(Slash::Login(None)));
        assert_eq!(
            parse("/login opencode"),
            Some(Slash::Login(Some(HarnessKind::OpenCode)))
        );
        assert_eq!(
            parse("/login claude-code"),
            Some(Slash::Login(Some(HarnessKind::ClaudeCode)))
        );
    }

    /// `auth` is the word the harness itself puts in front of you — `claude
    /// auth login`, `Failed to authenticate` — so it reaches the same command
    /// rather than being reported as unknown.
    #[test]
    fn the_word_the_harness_uses_reaches_the_same_command() {
        assert_eq!(parse("/auth"), Some(Slash::Login(None)));
        assert_eq!(parse("/signin"), Some(Slash::Login(None)));
    }

    /// A name that is not a harness is refused rather than quietly treated as
    /// "no argument", which would sign in to something nobody asked for.
    #[test]
    fn login_refuses_a_word_that_is_not_a_harness() {
        assert!(matches!(parse("/login gemini"), Some(Slash::Unknown(_))));
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
