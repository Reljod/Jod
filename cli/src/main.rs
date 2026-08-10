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
use jod_core::team::MemberStatus;
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
}
