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
    Remember(String),
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
    /// Stop an agent, by an id prefix or its name.
    Stop(String),
    /// Put an agent's output on screen.
    Watch(String),
    /// Say how to attach to an agent's tmux session.
    Attach(String),
    /// Put a task on the watched team's board.
    Todo(String),
    /// Mark one of those tasks finished.
    Done(String),
    /// Clear the transcript on screen. The conversation is untouched.
    Clear,
    Exit,
    /// A `/word` nobody knows. Reported rather than sent to the agent.
    Unknown(String),
    /// A known command missing its argument.
    NeedsArgument(&'static str),
}

/// Parse a line as a slash command.
///
/// `None` means "this is not a command" — including a bare `/`, and anything
/// with leading whitespace, so a prompt that happens to start with a slash
/// (`/usr/bin/foo is missing`) still reaches the agent as long as it is a real
/// path rather than a single word.
pub fn parse(line: &str) -> Option<Slash> {
    let rest = line.strip_prefix('/')?;
    let mut parts = rest.split_whitespace();
    let name = parts.next()?.to_ascii_lowercase();
    let arg = parts.collect::<Vec<_>>().join(" ");
    let arg = arg.trim();

    Some(match name.as_str() {
        "help" | "?" => Slash::Help,
        "harness" | "agent" => match harness_from(arg) {
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
        // `/new` alone is still a fresh conversation, which is what it has
        // always meant; `/new schedule` is the form ladder's front door.
        "new" => match kind_from(arg) {
            Some(ws) => Slash::NewKind(ws),
            None if arg.is_empty() => Slash::New,
            None => Slash::Unknown(format!("/new {arg}")),
        },
        "sessions" => Slash::Sessions,
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
        "remember" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/remember <text>")
            } else {
                Slash::Remember(arg.to_string())
            }
        }
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
        "main" | "jod" => {
            if arg.is_empty() {
                Slash::NeedsArgument("/main <instruction>")
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
        "exit" | "quit" | "q" => Slash::Exit,
        other => Slash::Unknown(format!("/{other}")),
    })
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

fn harness_from(name: &str) -> Option<HarnessKind> {
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
    ("/harness <name>", "claude, opencode or agy — takes effect next turn"),
    ("/model <name>", "set the model; no argument restores the default"),
    ("/mode [name]", "plan, ask, edits or auto; no argument cycles (Tab)"),
    ("/thinking", "show or hide reasoning"),
    ("/details", "show or hide what tools returned"),
    ("/new [kind]", "a fresh conversation, or a new schedule/goal/hook/task"),
    ("/sessions", "conversations you can pick up"),
    ("/resume <id>", "continue one of them"),
    ("/delegate <prompt>", "run it in the background (Ctrl-B)"),
    ("/main <instruction>", "hand it to the orchestrator — it picks the shape"),
    ("/agents", "the fleet (Ctrl-A, Ctrl-K f)"),
    ("/watch <id>", "put an agent's output on screen"),
    ("/stop <id>", "stop an agent and close its session"),
    ("/attach <id>", "how to attach to its tmux session"),
    ("/memory [query]", "what Jod remembers (Ctrl-K m)"),
    ("/schedules", "cron-triggered runs (Ctrl-K s)"),
    ("/schedule <name>", "open one of them"),
    ("/goals", "looping objectives (Ctrl-K g)"),
    ("/goal <name>", "open one of them"),
    ("/hooks", "webhook rules (Ctrl-K h)"),
    ("/hook <name>", "open one of them"),
    ("/tasks", "the board as a screen (Ctrl-K t)"),
    ("/activity", "what happened while you were away (Ctrl-K a)"),
    ("/run <name>", "fire a schedule or a goal iteration now"),
    ("/pause <name>", "stop a schedule or goal firing"),
    ("/unpause <name>", "arm it again"),
    ("/remember <text>", "write something to memory"),
    ("/forget <name>", "drop a memory node"),
    ("/team", "the team panel (Ctrl-G)"),
    ("/todo <title>", "put a task on the team's board"),
    ("/done <task-id>", "mark one of those tasks finished"),
    ("/clear", "clear the transcript on screen"),
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
pub fn completions(input: &str, app: &crate::tui::App) -> Vec<Completion> {
    let agents = &app.agents;
    let Some(rest) = input.strip_prefix('/') else {
        return vec![];
    };

    // Still typing the command word: offer names.
    if !rest.contains(char::is_whitespace) {
        let typed = rest.to_ascii_lowercase();
        return HELP
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
    }

    // Past the name: offer arguments for the commands that have a fixed set.
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let typed = parts.next().unwrap_or_default().trim_start().to_ascii_lowercase();

    match name.as_str() {
        "harness" | "agent" => HarnessKind::ALL
            .into_iter()
            .filter(|k| k.id().replace('_', "").starts_with(&typed) || k.id().starts_with(&typed))
            .map(|k| Completion::new(format!("/{name} {}", short_name(k)), k.label()))
            .collect(),
        // Whichever agents are still worth naming. `/stop` only offers the ones
        // that could actually be stopped, so the list answers "what can I do"
        // rather than merely "what exists".
        "stop" | "kill" | "watch" | "focus" | "attach" => agents
            .iter()
            .filter(|a| !matches!(name.as_str(), "stop" | "kill") || a.is_running())
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
        "new" => KINDS
            .iter()
            .filter(|kind| kind.starts_with(&typed))
            .map(|kind| Completion::new(format!("/new {kind}"), format!("a new {kind}")))
            .collect(),
        // The same reasoning as the agent ids: retyping a name off the screen
        // above is not a user interface. A schedule and a goal are both things
        // you pause, run and un-pause, so both are offered on those verbs.
        "schedule" => named(&name, &typed, app.schedules.iter().map(|s| (&s.name, &s.gloss))),
        "goal" => named(&name, &typed, app.goals.iter().map(|g| (&g.name, &g.cadence))),
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
        completions(input, &fleet(&[])).into_iter().map(|c| c.line).collect()
    }

    fn agent(id: &str, status: &str) -> crate::tui::AgentLine {
        crate::tui::AgentLine {
            id: id.into(),
            name: "port the parser".into(),
            harness: "Claude Code".into(),
            status: status.into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }
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
        assert_eq!(lines("/hel"), vec!["/help".to_string()], "no argument, no space");
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
        assert_eq!(parse("/mode manual"), Some(Slash::Mode(Some(PermissionPolicy::Ask))));
        assert_eq!(
            parse("/mode bypass"),
            Some(Slash::Mode(Some(PermissionPolicy::Bypass)))
        );
    }

    #[test]
    fn an_unknown_mode_is_reported_rather_than_guessed() {
        assert_eq!(parse("/mode yolo"), Some(Slash::Unknown("/mode yolo".into())));
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
        assert_eq!(parse("/continue ses-1"), Some(Slash::Resume("ses-1".into())));
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
            assert_eq!(parse(missing), Some(Slash::Unknown(missing.into())), "{missing}");
        }
    }

    #[test]
    fn the_agent_management_commands_all_parse() {
        assert_eq!(parse("/delegate audit the deps"), Some(Slash::Delegate("audit the deps".into())));
        assert_eq!(parse("/bg audit the deps"), Some(Slash::Delegate("audit the deps".into())));
        assert_eq!(parse("/stop abc123"), Some(Slash::Stop("abc123".into())));
        assert_eq!(parse("/kill abc123"), Some(Slash::Stop("abc123".into())));
        assert_eq!(parse("/watch abc123"), Some(Slash::Watch("abc123".into())));
        assert_eq!(parse("/focus abc123"), Some(Slash::Watch("abc123".into())));
        assert_eq!(parse("/attach abc123"), Some(Slash::Attach("abc123".into())));
        assert_eq!(parse("/todo port the parser"), Some(Slash::Todo("port the parser".into())));
        assert_eq!(parse("/done port-the-parser"), Some(Slash::Done("port-the-parser".into())));
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
        assert_eq!(parse("/jod do the thing"), Some(Slash::Main("do the thing".into())));
        assert_eq!(parse("/main"), Some(Slash::NeedsArgument("/main <instruction>")));
    }

    /// Each of these does something irreversible or unguessable without its
    /// argument, so a bare one must ask rather than act.
    #[test]
    fn the_agent_management_commands_all_want_an_argument() {
        for (text, usage) in [
            ("/delegate", "/delegate <prompt>"),
            ("/stop", "/stop <id>"),
            ("/watch", "/watch <id>"),
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
        let agents = [agent("abcdef1234", "running"), agent("99887766", "completed")];
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
        let agents = [agent("abcdef1234", "running"), agent("99887766", "completed")];
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
        assert_eq!(completions("/watch ", &fleet(&agents))[0].hint, "running · port the parser");
    }

    #[test]
    fn naming_an_agent_completes_to_nothing_when_there_are_none() {
        assert!(completions("/stop ", &fleet(&[])).is_empty());
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
