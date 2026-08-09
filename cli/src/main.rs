//! `jod` — the command line over the agent harnesses.
//!
//! Jod does not answer prompts. It hands them to a harness (Claude Code,
//! OpenCode, AGY), runs that harness inside its own tmux session, and turns the
//! harness's output into one event stream that every command here renders.

mod render;
mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use jod_core::store::{NewFact, Origin};
use jod_core::{HarnessKind, Jod, PermissionPolicy, Resume, SpawnRequest};
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
    /// Print the command that attaches to an agent's tmux session.
    Attach { id: String },
    /// Stop an agent and close its session.
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

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum PermissionArg {
    /// Refuse tool calls that need approval — safe for read-only prompts.
    Ask,
    /// Let file edits through; still prompt for anything else.
    AcceptEdits,
    /// Auto-approve everything. Only sane in a throwaway directory.
    Bypass,
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
            if !jod.tmux_available() {
                bail!("tmux is not installed, and every agent runs inside a tmux session");
            }

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

        Command::Attach { id } => {
            jod.rehydrate(200).await?;
            let agent = jod.agent(&id).await?;
            println!("{}", agent.attach_command);
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
        } => {
            if !jod.tmux_available() {
                bail!("tmux is not installed, and every agent runs inside a tmux session");
            }
            jod.rehydrate(200).await?;
            tui::run(
                jod,
                tui::Options {
                    harness: harness.into(),
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

        Command::Chat {
            harness,
            cwd,
            model,
            permission,
            continue_last,
        } => {
            if !jod.tmux_available() {
                bail!("tmux is not installed, and every agent runs inside a tmux session");
            }
            chat(jod, harness, cwd, model, permission, continue_last).await?;
        }
    }

    Ok(())
}

/// One conversation, many turns.
///
/// Every turn after the first resumes the harness session the previous turn
/// reported, so context carries across turns without Jod storing any of it —
/// the harness owns the transcript, which is the whole point of the seam.
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
            .spawn_agent(SpawnRequest {
                name: default_name(&prompt),
                harness: kind,
                prompt,
                cwd: cwd.clone(),
                model: model.clone(),
                permission: permission.into(),
                resume: resume.clone(),
            })
            .await?;
        render::stream(events, &agent.id, false, false).await;

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

    // --- the argument surface --------------------------------------------
    //
    // These pin the CLI's contract: the flags, their short forms and their
    // defaults are what people type and what scripts depend on, so a rename or
    // a changed default should fail here rather than in someone's shell.

    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("should parse")
    }

    #[test]
    fn run_defaults_to_claude_asking_before_it_acts_and_waiting_for_the_result() {
        match parse(&["jod", "run", "do the thing"]).command {
            Command::Run {
                prompt,
                harness,
                permission,
                detach,
                json,
                thinking,
                continue_last,
                session,
                ..
            } => {
                assert_eq!(prompt.as_deref(), Some("do the thing"));
                assert!(harness == HarnessArg::Claude);
                assert!(permission == PermissionArg::Ask);
                assert!(!detach && !json && !thinking && !continue_last);
                assert_eq!(session, None);
            }
            _ => panic!("expected Run"),
        }
    }

    /// Omitting the prompt is how `jod run` reads from stdin.
    #[test]
    fn run_accepts_no_prompt_at_all() {
        match parse(&["jod", "run"]).command {
            Command::Run { prompt, .. } => assert_eq!(prompt, None),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_takes_short_forms_for_what_is_typed_often() {
        match parse(&[
            "jod", "run", "p", "-H", "agy", "-n", "scout", "-m", "opus", "-p", "bypass",
        ])
        .command
        {
            Command::Run {
                harness,
                name,
                model,
                permission,
                ..
            } => {
                assert!(harness == HarnessArg::Agy);
                assert_eq!(name.as_deref(), Some("scout"));
                assert_eq!(model.as_deref(), Some("opus"));
                assert!(permission == PermissionArg::Bypass);
            }
            _ => panic!("expected Run"),
        }
    }

    /// Resuming the last conversation and resuming a named one are different
    /// intents; asking for both at once is a mistake worth refusing.
    #[test]
    fn continuing_and_naming_a_session_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["jod", "run", "p", "--continue", "--session", "s1"]).is_err());

        match parse(&["jod", "run", "p", "--continue"]).command {
            Command::Run { continue_last, .. } => assert!(continue_last),
            _ => panic!("expected Run"),
        }
        match parse(&["jod", "run", "p", "--session", "s1"]).command {
            Command::Run { session, .. } => assert_eq!(session.as_deref(), Some("s1")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn the_read_only_commands_all_offer_json() {
        for (argv, label) in [
            (vec!["jod", "ls", "--json"], "ls"),
            (vec!["jod", "harnesses", "--json"], "harnesses"),
            (vec!["jod", "report", "--json"], "report"),
            (vec!["jod", "history", "--json"], "history"),
        ] {
            let json = match parse(&argv).command {
                Command::Ls { json }
                | Command::Harnesses { json }
                | Command::Report { json }
                | Command::History { json, .. } => json,
                _ => panic!("unexpected command for {label}"),
            };
            assert!(json, "{label} must accept --json");
        }
    }

    #[test]
    fn history_shows_a_bounded_number_of_runs_by_default() {
        match parse(&["jod", "history"]).command {
            Command::History { limit, json } => {
                assert_eq!(limit, 20);
                assert!(!json);
            }
            _ => panic!("expected History"),
        }
        match parse(&["jod", "history", "-l", "5"]).command {
            Command::History { limit, .. } => assert_eq!(limit, 5),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn attach_and_kill_each_need_an_agent_id() {
        match parse(&["jod", "attach", "a1"]).command {
            Command::Attach { id } => assert_eq!(id, "a1"),
            _ => panic!("expected Attach"),
        }
        match parse(&["jod", "kill", "a1"]).command {
            Command::Kill { id } => assert_eq!(id, "a1"),
            _ => panic!("expected Kill"),
        }
        assert!(Cli::try_parse_from(["jod", "attach"]).is_err());
        assert!(Cli::try_parse_from(["jod", "kill"]).is_err());
    }

    /// A fact typed by hand is Reljod's own word; that is the whole point of
    /// recording origin, so the default must not drift.
    #[test]
    fn remembering_by_hand_defaults_to_the_owners_own_word() {
        match parse(&["jod", "remember", "reljod", "prefers", "linear"]).command {
            Command::Remember {
                subject,
                predicate,
                object,
                source,
                origin,
                ..
            } => {
                assert_eq!((subject.as_str(), predicate.as_str(), object.as_str()),
                           ("reljod", "prefers", "linear"));
                assert_eq!(source, None);
                assert!(Origin::from(origin) == Origin::Owner);
            }
            _ => panic!("expected Remember"),
        }
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_guessed_at() {
        assert!(Cli::try_parse_from(["jod", "definitely-not-a-command"]).is_err());
        assert!(Cli::try_parse_from(["jod"]).is_err());
    }

    #[test]
    fn every_origin_arg_maps_to_a_distinct_origin() {
        let origins: Vec<Origin> = [
            OriginArg::Owner,
            OriginArg::Agent,
            OriginArg::Untrusted,
            OriginArg::System,
        ]
        .into_iter()
        .map(Origin::from)
        .collect();
        assert_eq!(origins.len(), 4);
        for (i, a) in origins.iter().enumerate() {
            for b in &origins[i + 1..] {
                assert!(a != b, "two origin args mapped to the same origin");
            }
        }
    }

    #[test]
    fn every_permission_arg_maps_to_a_distinct_policy() {
        let policies: Vec<PermissionPolicy> = [
            PermissionArg::Ask,
            PermissionArg::AcceptEdits,
            PermissionArg::Bypass,
        ]
        .into_iter()
        .map(PermissionPolicy::from)
        .collect();
        assert_eq!(policies.len(), 3);
        for (i, a) in policies.iter().enumerate() {
            for b in &policies[i + 1..] {
                assert_ne!(a, b, "two permission args mapped to the same policy");
            }
        }
    }
}
