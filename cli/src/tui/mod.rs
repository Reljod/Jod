//! `jod tui` — the full-screen interface.
//!
//! Layout, top to bottom: a scrolling transcript, an input box, a status bar.
//! `Ctrl-A` reveals a panel listing every delegation this process knows about,
//! which is the part that makes this an orchestrator's UI rather than a chat
//! window — Jod's job is watching several agents, not talking to one.
//!
//! That panel is where unattended work is actually managed: it is a cursor over
//! the live fleet, and from it a run can be watched, stopped, resumed or
//! attached to. Sending a prompt with `Ctrl-B` (or `/delegate`) starts an agent
//! that never takes over the screen, so a long job can be left running while the
//! conversation carries on — and its ending arrives as a notice rather than
//! being missed.
//!
//! The terminal is put into raw mode and an alternate screen, and **must** be
//! put back however this function exits. A panic that skips the restore leaves
//! the user with an unusable shell, so the restore is installed as a panic hook
//! as well as run on the normal path.

mod app;
mod command;
mod ui;

pub use app::{AgentLine, App, Entry, Pane};

use std::io;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use jod_core::{AgentEvent, HarnessKind, Jod, PermissionPolicy, Resume, SpawnRequest};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::path::PathBuf;

/// Something the loop has to do that state alone cannot: it needs the service,
/// the store or the clock.
///
/// Key handling and slash commands stay pure by *describing* the work and
/// handing it back, which is what keeps every decision in this file testable
/// without a terminal or a running agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a prompt to the conversation on screen.
    Send(String),
    /// Start an agent that runs without taking over the transcript.
    Delegate(String),
    /// Stop an agent and close its tmux session.
    Stop(String),
    /// Put an agent's output on screen and follow it.
    Watch(String),
    /// Say how to attach to an agent's tmux session.
    Attach(String),
    /// Put a task on the watched team's board.
    AddTask(String),
    /// Mark a task on that board finished.
    FinishTask(String),
}

pub struct Options {
    pub harness: HarnessKind,
    /// The team to watch, if any. `None` leaves the team panel saying so
    /// rather than showing an empty board.
    pub team: Option<String>,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission: PermissionPolicy,
    pub resume: Resume,
}

pub async fn run(jod: Arc<Jod>, opts: Options) -> Result<()> {
    let mut terminal = enter().context("taking over the terminal")?;
    let result = event_loop(&mut terminal, jod, opts).await;
    // Restore before surfacing any error, or the message prints into a raw-mode
    // terminal that mangles it.
    restore();
    result
}

fn enter() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // A panic past this point would otherwise leave the shell in raw mode with
    // no echo — effectively broken until the user blindly types `reset`.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    jod: Arc<Jod>,
    opts: Options,
) -> Result<()> {
    let mut app = App::new(opts.harness, opts.model.clone(), opts.resume.clone());
    app.team = opts.team.clone();
    app.now_ms = now_ms();
    app.agents = list_agents(&jod).await;
    refresh_team(&jod, &mut app);
    // No harness name here: this line is frozen into the scrollback, so naming
    // the harness would leave a stale claim on screen the moment `/harness`
    // switches. The status bar is the one place that tracks it.
    app.push(Entry::Notice(
        "/help for commands · Enter send · Ctrl-B delegate in the background · Ctrl-A agents · Ctrl-G team · Ctrl-C quit"
            .to_string(),
    ));

    let mut keys = EventStream::new();
    let mut events = jod.subscribe();
    let mut viewport = 20usize;
    // Four frames a second: enough for the spinner to read as motion and for an
    // elapsed counter to look like a clock, cheap enough to be free.
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|f| viewport = ui::draw(f, &app))?;
        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            // Bias towards keys so the UI stays responsive while an agent
            // floods the channel with output.
            biased;

            Some(Ok(ev)) = keys.next() => {
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if let Some(action) = on_key(&mut app, key, viewport) {
                            perform(&jod, &mut app, &opts, action).await;
                        }
                    }
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => app.scroll_up(3, app.transcript.len()),
                        MouseEventKind::ScrollDown => app.scroll_down(3),
                        _ => {}
                    },
                    // A resize just needs a redraw, which the next loop does.
                    _ => {}
                }
            }

            Ok(envelope) = events.recv() => {
                let watched = app.watching.as_deref() == Some(envelope.agent_id.as_str());
                let finished = matches!(envelope.event, AgentEvent::Finished { .. });
                if watched {
                    app.apply(&envelope.event);
                }
                if finished {
                    app.agents = list_agents(&jod).await;
                    app.clamp_selection();
                    refresh_team(&jod, &mut app);
                    if watched {
                        // A prompt typed mid-turn goes now rather than being
                        // refused earlier and forgotten.
                        if let Some(next) = app.next_queued() {
                            perform(&jod, &mut app, &opts, Action::Send(next)).await;
                        }
                    } else {
                        announce(&mut app, &envelope.agent_id);
                    }
                }
            }

            _ = ticks.tick() => {
                app.advance(now_ms());
                // Statuses change in other processes as well as this one, and a
                // panel that only refreshes when the watched agent finishes
                // shows a fleet that stopped moving minutes ago.
                if app.tick.is_multiple_of(4) {
                    app.agents = list_agents(&jod).await;
                    app.clamp_selection();
                    if app.pane == Pane::Team {
                        refresh_team(&jod, &mut app);
                    }
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Say that a background agent ended, and how it went.
///
/// The whole point of delegating is not to watch, so the ending has to come and
/// find you. Naming the agent and how to open it makes the notice actionable
/// rather than merely informative.
fn announce(app: &mut App, id: &str) {
    let Some(agent) = app.agents.iter().find(|a| a.id == id) else {
        return;
    };
    let mark = match agent.status.as_str() {
        "completed" => "✓",
        "killed" => "■",
        _ => "✗",
    };
    let took = app::short_duration(app.now_ms.saturating_sub(agent.created_at_ms));
    app.push(Entry::Notice(format!(
        "{mark} {} {} after {took} — Ctrl-A to open it",
        agent.name, agent.status
    )));
}

/// Carry out one action against the service.
async fn perform(jod: &Arc<Jod>, app: &mut App, opts: &Options, action: Action) {
    match action {
        Action::Send(prompt) => {
            match spawn(jod, app, opts, prompt.clone(), app.resume.clone()).await {
                Ok(id) => {
                    app.watching = Some(id);
                    app.busy = true;
                    app.turn_started_ms = Some(app.now_ms);
                    app.push(Entry::You(prompt));
                    app.scroll_to_bottom();
                }
                Err(e) => app.push(Entry::Notice(format!("could not start: {e}"))),
            }
        }
        Action::Delegate(prompt) => {
            // Fresh, always: a background job that silently continued the
            // conversation on screen would inherit context nobody asked it to,
            // and two agents writing into one session is not a conversation.
            match spawn(jod, app, opts, prompt.clone(), Resume::Fresh).await {
                Ok(id) => {
                    app.push(Entry::Notice(format!(
                        "delegated {} — {} · runs in the background, Ctrl-A to watch",
                        short(&id),
                        crate::default_name(&prompt)
                    )));
                    app.scroll_to_bottom();
                }
                Err(e) => app.push(Entry::Notice(format!("could not delegate: {e}"))),
            }
        }
        Action::Stop(id) => match jod.kill_agent(&id).await {
            Ok(()) => {
                if app.watching.as_deref() == Some(id.as_str()) {
                    app.busy = false;
                    app.turn_started_ms = None;
                }
                app.push(Entry::Notice(format!("stopped {}", short(&id))));
            }
            Err(e) => app.push(Entry::Notice(format!("could not stop {}: {e}", short(&id)))),
        },
        Action::Watch(id) => match jod.events_since(&id, None).await {
            Ok(events) => {
                let running = app.agents.iter().any(|a| a.id == id && a.is_running());
                app.transcript.clear();
                app.watching = Some(id.clone());
                app.busy = running;
                app.turn_started_ms = running
                    .then(|| app.agents.iter().find(|a| a.id == id).map(|a| a.created_at_ms))
                    .flatten();
                // The session cursor follows the eye: typing next continues the
                // conversation being read, not the one that scrolled away.
                if let Some(session) = app
                    .agents
                    .iter()
                    .find(|a| a.id == id)
                    .and_then(|a| a.session.clone())
                {
                    app.resume = Resume::Session(session.clone());
                    app.session = Some(session);
                }
                app.push(Entry::Notice(format!("watching {}", short(&id))));
                for envelope in events {
                    app.apply(&envelope.event);
                }
                app.scroll_to_bottom();
            }
            Err(e) => app.push(Entry::Notice(format!("cannot open {}: {e}", short(&id)))),
        },
        Action::Attach(id) => match jod.agent(&id).await {
            Ok(agent) => {
                app.push(Entry::Notice(format!(
                    "from another terminal: {}",
                    agent.attach_command
                )));
            }
            Err(e) => app.push(Entry::Notice(format!("no agent {}: {e}", short(&id)))),
        },
        Action::AddTask(title) => {
            let (Some(team), Some(store)) = (app.team.clone(), jod.store()) else {
                app.push(Entry::Notice(
                    "no team to add to — start with `jod tui --team <name>`".into(),
                ));
                return;
            };
            let id = task_id(&title, &app.tasks);
            match store.add_team_task(&team, &id, &title) {
                Ok(()) => {
                    app.push(Entry::Notice(format!("{id} on {team}'s board")));
                    refresh_team(jod, app);
                }
                Err(e) => app.push(Entry::Notice(format!("could not add the task: {e}"))),
            }
        }
        Action::FinishTask(id) => {
            let Some(store) = jod.store() else {
                return;
            };
            match store.complete_task(&id) {
                Ok(true) => {
                    app.push(Entry::Notice(format!("{id} done")));
                    refresh_team(jod, app);
                }
                Ok(false) => app.push(Entry::Notice(format!("no task {id} on the board"))),
                Err(e) => app.push(Entry::Notice(format!("could not finish {id}: {e}"))),
            }
        }
    }
}

/// A short, stable id for a task typed into the board.
///
/// Derived from the title so the id means something when a teammate claims it
/// from the command line, and suffixed when that would collide — a board with
/// two `write-the-docs` rows is a board where `jod team claim` picks the wrong
/// one.
fn task_id(title: &str, existing: &[jod_core::team::TeamTask]) -> String {
    let slug: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    let base = if slug.is_empty() { "task".to_string() } else { slug };
    if !existing.iter().any(|t| t.id == base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.iter().any(|t| &t.id == candidate))
        .unwrap_or(base)
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Handle one keypress. Returns work for the loop to carry out, if any.
fn on_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let max_scroll = app.transcript.len();

    if ctrl {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') => {
                // Refuse to leave silently while work is in flight; a second
                // press goes anyway. Any running agent counts, not just the one
                // on screen — walking out on four background jobs without being
                // told is the same mistake, four times over.
                let running = app.running();
                if running > 0 && !app.confirm_quit {
                    app.confirm_quit = true;
                    let what = if running == 1 {
                        "an agent is still running".to_string()
                    } else {
                        format!("{running} agents are still running")
                    };
                    app.push(Entry::Notice(format!(
                        "{what} — press again to leave them running"
                    )));
                } else {
                    app.should_quit = true;
                }
                return None;
            }
            KeyCode::Char('a') => {
                app.pane = if app.pane == Pane::Agents {
                    Pane::Chat
                } else {
                    Pane::Agents
                };
                return None;
            }
            KeyCode::Char('g') => {
                app.pane = if app.pane == Pane::Team {
                    Pane::Chat
                } else {
                    Pane::Team
                };
                return None;
            }
            KeyCode::Char('t') => {
                app.show_thinking = !app.show_thinking;
                app.push(Entry::Notice(format!(
                    "thinking {}",
                    if app.show_thinking { "shown" } else { "hidden" }
                )));
                return None;
            }
            KeyCode::Char('o') => {
                app.show_details = !app.show_details;
                app.push(Entry::Notice(format!(
                    "tool output {}",
                    if app.show_details { "shown" } else { "hidden" }
                )));
                return None;
            }
            // Delegate: the typed line becomes an agent that runs without
            // taking the screen. This is the key that makes several jobs at
            // once possible without leaving the UI.
            KeyCode::Char('b') => {
                return app.take_input().map(Action::Delegate);
            }
            // Stop what is being watched. Ctrl-C is quit, so interrupting a run
            // needs a key of its own or the only way out is to leave.
            KeyCode::Char('x') => {
                return match app.watching.clone() {
                    Some(id) if app.busy => Some(Action::Stop(id)),
                    _ => {
                        app.push(Entry::Notice("nothing running to stop here".into()));
                        None
                    }
                };
            }
            KeyCode::Char('l') => {
                app.transcript.clear();
                app.scroll_to_bottom();
                return None;
            }
            KeyCode::Char('u') => {
                app.clear_line();
                return None;
            }
            KeyCode::Char('w') => {
                app.delete_word();
                return None;
            }
            // Scrolling keeps a Ctrl form, because the bare arrows now walk
            // back through what has been sent.
            KeyCode::Up => {
                app.scroll_up(1, max_scroll);
                return None;
            }
            KeyCode::Down => {
                app.scroll_down(1);
                return None;
            }
            // Ctrl-A is the agents panel, so start-of-line is Home rather than
            // the readline binding.
            KeyCode::Home => {
                app.home();
                return None;
            }
            KeyCode::Char('e') | KeyCode::End => {
                app.end();
                return None;
            }
            _ => {}
        }
    }

    // Any key other than a second quit means the user changed their mind.
    if !matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
        app.confirm_quit = false;
    }

    // An open panel owns the keyboard. It is a list you act on, so the letters
    // are commands rather than text — the input box is one Esc away, and the
    // panel's own footer says which letters do what.
    if app.pane != Pane::Chat {
        return on_panel_key(app, key);
    }

    // While the completion popup is up it owns Tab and the arrows, and Enter
    // finishes the word rather than sending a half-typed command.
    let suggestions = command::completions(&app.input, &app.agents);
    if !suggestions.is_empty() {
        app.clamp_suggestion(suggestions.len());
        match key.code {
            KeyCode::Tab => {
                let line = suggestions[app.suggestion].line.clone();
                app.accept_completion(&line);
                return None;
            }
            KeyCode::Up => {
                app.prev_suggestion(suggestions.len());
                return None;
            }
            KeyCode::Down => {
                app.next_suggestion(suggestions.len());
                return None;
            }
            // Enter picks the highlighted suggestion only when that would
            // actually change the line. Otherwise the command is already fully
            // typed and Enter must run it: a command needing an argument
            // completes to *itself* plus a space, so accepting here appended an
            // invisible space, swallowed the keypress, and left the text in the
            // box to corrupt whatever was typed next.
            KeyCode::Enter
                if suggestions[app.suggestion].line.trim_end() != app.input.trim_end() =>
            {
                let line = suggestions[app.suggestion].line.clone();
                app.accept_completion(&line);
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            // A slash line is an instruction to Jod, not to the agent, so it
            // works while an agent is busy — switching the model for the *next*
            // turn is exactly the sort of thing you do while waiting.
            if let Some(slash) = command::parse(app.input.trim()) {
                let typed = app.input.trim().to_string();
                app.remember_typed(&typed);
                app.input.clear();
                app.cursor = 0;
                return apply_slash(app, slash);
            }
            let prompt = app.take_input()?;
            // Queued rather than refused. The old behaviour left the sentence
            // sitting in the box with a scolding, so the box stayed blocked and
            // the only thing to do while an agent worked was to sit still.
            if app.busy {
                app.queue(prompt);
                app.push(Entry::Notice(format!(
                    "queued — sends when this turn ends ({} waiting)",
                    app.queued.len()
                )));
                return None;
            }
            return Some(Action::Send(prompt));
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_forward(),
        KeyCode::Left => app.left(),
        KeyCode::Right => app.right(),
        KeyCode::Home => app.home(),
        KeyCode::End => app.end(),
        // The arrows recall what has been sent, as every shell and every other
        // agent CLI does. Scrolling the transcript is PageUp/PageDown, the
        // mouse, or Ctrl with an arrow.
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        KeyCode::PageUp => app.scroll_up(viewport.max(1), max_scroll),
        KeyCode::PageDown => app.scroll_down(viewport.max(1)),
        KeyCode::Esc => app.scroll_to_bottom(),
        KeyCode::Char(c) => {
            app.insert(c);
            // Typing means the recalled line is now the user's own draft, so
            // ↓ must not silently replace what they are editing.
            app.history_at = None;
        }
        _ => {}
    }
    None
}

/// Keys while a panel is open. The panel is modal: this is a list you act on.
fn on_panel_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.pane = Pane::Chat;
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            match app.pane {
                Pane::Team => app.select_task(-1),
                _ => app.select_agent(-1),
            }
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            match app.pane {
                Pane::Team => app.select_task(1),
                _ => app.select_agent(1),
            }
            None
        }
        KeyCode::Home => {
            match app.pane {
                Pane::Team => app.task_sel = 0,
                _ => app.agent_sel = 0,
            }
            None
        }
        KeyCode::End => {
            match app.pane {
                Pane::Team => app.task_sel = app.tasks.len().saturating_sub(1),
                _ => app.agent_sel = app.agents.len().saturating_sub(1),
            }
            None
        }
        KeyCode::Enter => match app.pane {
            // Reading a run is the common case, so it is the plain key.
            Pane::Agents => {
                let id = app.selected_agent()?.id.clone();
                app.pane = Pane::Chat;
                Some(Action::Watch(id))
            }
            Pane::Team => {
                let task = app.selected_task()?;
                if task.is_done() {
                    app.push(Entry::Notice(format!("{} is already done", task.id)));
                    return None;
                }
                Some(Action::FinishTask(task.id.clone()))
            }
            Pane::Chat => None,
        },
        KeyCode::Char('s') if app.pane == Pane::Agents => {
            let agent = app.selected_agent()?;
            let (id, running) = (agent.id.clone(), agent.is_running());
            if !running {
                // Killing a finished run only reclaims its tmux session, which
                // is not what "s" looks like it does. Say so instead.
                app.push(Entry::Notice(format!(
                    "{} is already {} — nothing to stop",
                    short(&id),
                    agent.status
                )));
                return None;
            }
            Some(Action::Stop(id))
        }
        KeyCode::Char('a') if app.pane == Pane::Agents => {
            Some(Action::Attach(app.selected_agent()?.id.clone()))
        }
        // Continue the selected agent's conversation from the input box, which
        // is how an unattended run gets picked up and corrected.
        KeyCode::Char('r') if app.pane == Pane::Agents => {
            let agent = app.selected_agent()?.clone();
            app.pane = Pane::Chat;
            match agent.session {
                Some(session) => {
                    app.resume = Resume::Session(session.clone());
                    app.session = Some(session);
                    app.harness_from_label(&agent.harness);
                    app.push(Entry::Notice(format!(
                        "next turn continues {} — type to carry on",
                        agent.name
                    )));
                }
                None => app.push(Entry::Notice(format!(
                    "{} never reported a conversation, so there is nothing to continue",
                    agent.name
                ))),
            }
            None
        }
        _ => None,
    }
}

/// Carry out a slash command. Everything it touches is app state, so this
/// stays synchronous and testable; anything needing the service comes back as
/// an `Action` for the loop to run.
fn apply_slash(app: &mut App, slash: command::Slash) -> Option<Action> {
    use command::Slash;
    match slash {
        Slash::Help => {
            for (usage, what) in command::HELP {
                app.push(Entry::Notice(format!("{usage:<18} {what}")));
            }
        }
        Slash::Harness(kind) => {
            let changed = app.harness != kind;
            app.harness = kind;
            // Conversations belong to a harness, so carrying the old session
            // cursor across would try to resume a conversation the new harness
            // has never heard of.
            app.resume = Resume::Fresh;
            app.session = None;
            if changed {
                // Model names do not survive the crossing. `claude-sonnet-4-5`
                // means nothing to OpenCode or AGY, so keeping either the
                // requested or the reported name would hand the new harness a
                // model it rejects — and the switch would look like it simply
                // did not work. Dropping to `None` lets each harness choose its
                // own default, which is what switching harness should mean.
                app.model = None;
                app.reported_model = None;
                // Spend belongs to the conversation being abandoned. Carrying
                // it over showed `$0.11` next to AGY, which had charged nothing.
                app.cost_usd = 0.0;
            }
            app.push(Entry::Notice(format!(
                "{} from the next turn — fresh conversation, its own default model",
                kind.label()
            )));
        }
        Slash::Model(model) => {
            let said = match &model {
                Some(m) => format!("model: {m}"),
                None => "model: the harness default".to_string(),
            };
            app.model = model;
            app.push(Entry::Notice(said));
        }
        Slash::Thinking => {
            app.show_thinking = !app.show_thinking;
            app.push(Entry::Notice(format!(
                "thinking {}",
                if app.show_thinking { "shown" } else { "hidden" }
            )));
        }
        Slash::Details => {
            app.show_details = !app.show_details;
            app.push(Entry::Notice(format!(
                "tool output {}",
                if app.show_details { "shown" } else { "hidden" }
            )));
        }
        Slash::New => {
            app.resume = Resume::Fresh;
            app.session = None;
            app.cost_usd = 0.0;
            app.transcript.clear();
            app.scroll_to_bottom();
            app.push(Entry::Notice("new conversation".into()));
        }
        Slash::Sessions => {
            app.pane = Pane::Agents;
            app.push(Entry::Notice(
                "pick an id from the panel, then /resume <id> — the shown prefix is enough".into(),
            ));
        }
        Slash::Resume(id) => match app.resolve_session(&id) {
            app::Resolved::Session(session) => {
                app.resume = Resume::Session(session.clone());
                app.session = Some(session.clone());
                app.push(Entry::Notice(format!("continuing {session}")));
            }
            app::Resolved::Verbatim(raw) => {
                app.resume = Resume::Session(raw.clone());
                app.session = Some(raw.clone());
                // Say when it matched nothing on screen. A typo is otherwise
                // indistinguishable from a real resume until the harness
                // rejects it several seconds later.
                if app.agents.is_empty() {
                    app.push(Entry::Notice(format!("continuing {raw}")));
                } else {
                    app.push(Entry::Notice(format!(
                        "continuing {raw} — not one of the agents listed, passing it on as typed"
                    )));
                }
            }
            app::Resolved::NoSession(agent) => {
                app.push(Entry::Notice(format!(
                    "{agent} has not reported a conversation yet — resuming it would start a fresh one"
                )));
            }
            app::Resolved::Ambiguous(n) => {
                app.push(Entry::Notice(format!(
                    "{id} matches {n} agents — type more of it"
                )));
            }
        },
        Slash::Agents => {
            app.pane = if app.pane == Pane::Agents { Pane::Chat } else { Pane::Agents };
        }
        Slash::Team => {
            app.pane = if app.pane == Pane::Team { Pane::Chat } else { Pane::Team };
        }
        Slash::Delegate(prompt) => return Some(Action::Delegate(prompt)),
        Slash::Stop(which) => return resolve_agent(app, &which).map(Action::Stop),
        Slash::Watch(which) => return resolve_agent(app, &which).map(Action::Watch),
        Slash::Attach(which) => return resolve_agent(app, &which).map(Action::Attach),
        Slash::Todo(title) => return Some(Action::AddTask(title)),
        Slash::Done(id) => return Some(Action::FinishTask(id)),
        Slash::Clear => {
            app.transcript.clear();
            app.scroll_to_bottom();
        }
        Slash::Exit => app.should_quit = true,
        Slash::NeedsArgument(usage) => {
            app.push(Entry::Notice(format!("usage: {usage}")));
        }
        Slash::Unknown(what) => {
            app.push(Entry::Notice(format!("{what} is not a command — /help lists them")));
        }
    }
    None
}

/// Turn what was typed at `/stop`, `/watch` or `/attach` into one agent id.
///
/// A prefix is enough, because the panel shows eight characters and retyping a
/// UUID is not a user interface. An ambiguous or unknown one is refused rather
/// than guessed: stopping the wrong agent is not an undoable mistake.
fn resolve_agent(app: &mut App, typed: &str) -> Option<String> {
    let matches: Vec<&AgentLine> = app
        .agents
        .iter()
        .filter(|a| a.id.starts_with(typed) || a.name == typed)
        .collect();
    match matches.as_slice() {
        [only] => Some(only.id.clone()),
        [] => {
            app.push(Entry::Notice(format!(
                "no agent starts with {typed} — Ctrl-A lists them"
            )));
            None
        }
        many => {
            app.push(Entry::Notice(format!(
                "{typed} matches {} agents — type more of it",
                many.len()
            )));
            None
        }
    }
}

async fn spawn(
    jod: &Arc<Jod>,
    app: &App,
    opts: &Options,
    prompt: String,
    resume: Resume,
) -> Result<String> {
    let agent = jod
        .spawn_agent(SpawnRequest {
            name: crate::default_name(&prompt),
            // From the app, not the options: `/harness` and `/model` change
            // these mid-session, and a spawn must use what is current.
            harness: app.harness,
            prompt,
            cwd: opts.cwd.clone(),
            model: app.model.clone(),
            permission: opts.permission,
            // App owns the conversation cursor: it advances to the exact
            // session the harness reported on the previous turn. A background
            // delegation passes its own, because it is not part of this
            // conversation at all.
            resume,
        })
        .await?;
    Ok(agent.id)
}

/// Re-read the team from the store.
///
/// Members and tasks are written by the teammates themselves, in their own
/// processes, so the only way to know the current state is to ask the store —
/// there is no in-memory copy that could be authoritative.
fn refresh_team(jod: &Arc<Jod>, app: &mut App) {
    let (Some(team), Some(store)) = (app.team.clone(), jod.store()) else {
        return;
    };
    // A store error here must not take the UI down: the panel showing what it
    // last knew beats the whole session ending over a locked database.
    if let Ok(members) = store.team_members(&team) {
        app.members = members;
    }
    if let Ok(tasks) = store.team_tasks(&team) {
        app.tasks = tasks;
    }
}

async fn list_agents(jod: &Arc<Jod>) -> Vec<AgentLine> {
    let mut lines: Vec<AgentLine> = jod
        .agents()
        .await
        .into_iter()
        .map(|a| AgentLine {
            id: a.id,
            name: a.name,
            harness: a.harness_label,
            status: format!("{:?}", a.status).to_lowercase(),
            session: a.session_id,
            created_at_ms: a.created_at_ms,
            cost_usd: a.usage.cost_usd,
            last: a.last_message,
        })
        .collect();
    // Running first, then newest: the panel's top row should be the thing most
    // likely to need attention, not whatever was launched first three days ago.
    lines.sort_by(|a, b| {
        b.is_running()
            .cmp(&a.is_running())
            .then(b.created_at_ms.cmp(&a.created_at_ms))
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_on(harness: HarnessKind) -> App {
        App::new(harness, None, Resume::Fresh)
    }

    #[test]
    fn switching_harness_drops_the_old_model() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        // A turn ran, so the harness reported what it used.
        app.reported_model = Some("claude-opus-5".into());
        app.model = Some("opus".into());
        app.cost_usd = 0.11;

        apply_slash(&mut app, command::Slash::Harness(HarnessKind::OpenCode));

        // Neither name may survive: OpenCode rejects both, and passing either
        // made the switch look like it had not happened at all.
        assert_eq!(app.model, None);
        assert_eq!(app.reported_model, None);
        assert_eq!(app.cost_usd, 0.0);
        assert_eq!(app.harness, HarnessKind::OpenCode);
    }

    #[test]
    fn a_reported_model_never_becomes_the_next_request() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.apply(&AgentEvent::Started {
            session_id: Some("s1".into()),
            model: Some("claude-opus-5".into()),
        });
        // Reported, so it shows; requested, so it does not.
        assert_eq!(app.reported_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(app.model, None);
        assert!(app.status().contains("claude-opus-5"));
    }

    #[test]
    fn re_selecting_the_same_harness_keeps_the_chosen_model() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.model = Some("haiku".into());
        apply_slash(&mut app, command::Slash::Harness(HarnessKind::ClaudeCode));
        // Nothing crossed a harness boundary, so the choice stands.
        assert_eq!(app.model.as_deref(), Some("haiku"));
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        on_key(app, KeyEvent::new(code, KeyModifiers::NONE), 20)
    }

    fn type_line(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Enter on a command that still needs an argument must say so, not
    /// silently append a space and leave the text sitting in the box.
    #[test]
    fn enter_on_an_argumentless_command_reports_the_usage() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/resume");
        assert!(press(&mut app, KeyCode::Enter).is_none());

        assert_eq!(app.input, "", "the line must be consumed");
        let last = format!("{:?}", app.transcript.last().unwrap());
        assert!(last.contains("usage"), "expected a usage notice, got {last}");
    }

    /// The next command typed must not inherit the previous one's text.
    #[test]
    fn a_command_needing_an_argument_does_not_corrupt_the_next_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/resume");
        press(&mut app, KeyCode::Enter);
        type_line(&mut app, "/model haiku");
        press(&mut app, KeyCode::Enter);

        // Not `Resume("/model haiku")`, which is what the leftover text caused.
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert_eq!(app.session, None);
    }

    /// Enter still completes when completing would actually change the line.
    #[test]
    fn enter_completes_a_half_typed_command() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/thi");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.input, "/thinking");
        // A second Enter runs it — asserted as a flip, not a value, so the
        // test says "the command fired" rather than restating the default.
        let before = app.show_thinking;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.show_thinking, !before);
    }

    fn agent_line(id: &str, session: Option<&str>) -> AgentLine {
        AgentLine {
            id: id.into(),
            name: "work".into(),
            harness: "Claude Code".into(),
            status: "completed".into(),
            session: session.map(str::to_string),
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }
    }

    /// The panel shows a shortened *agent* id and tells you to `/resume` it,
    /// but the harness needs its own conversation id. The prefix must resolve.
    #[test]
    fn resume_accepts_the_shortened_id_the_panel_shows() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12-3456-7890", Some("sess-xyz"))];

        apply_slash(&mut app, command::Slash::Resume("abcdef12".into()));

        assert_eq!(app.resume, Resume::Session("sess-xyz".into()));
        assert_eq!(app.session.as_deref(), Some("sess-xyz"));
    }

    #[test]
    fn resume_still_takes_a_session_id_typed_in_full() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12-3456", Some("sess-xyz"))];
        apply_slash(&mut app, command::Slash::Resume("sess-xyz".into()));
        assert_eq!(app.resume, Resume::Session("sess-xyz".into()));
    }

    /// An id from another machine or a log is still legitimate to type.
    #[test]
    fn an_unrecognised_id_is_passed_through_untouched() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Resume("from-elsewhere".into()));
        assert_eq!(app.resume, Resume::Session("from-elsewhere".into()));
    }

    #[test]
    fn an_ambiguous_prefix_changes_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![
            agent_line("ab111111", Some("s1")),
            agent_line("ab222222", Some("s2")),
        ];
        apply_slash(&mut app, command::Slash::Resume("ab".into()));

        assert_eq!(app.resume, Resume::Fresh, "must not guess");
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("matches 2"));
    }

    /// Resuming an agent that never reported a conversation would quietly start
    /// a fresh one — the amnesia case.
    #[test]
    fn an_agent_with_no_conversation_is_refused() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12", None)];
        apply_slash(&mut app, command::Slash::Resume("abcdef12".into()));

        assert_eq!(app.resume, Resume::Fresh);
        assert!(format!("{:?}", app.transcript.last().unwrap())
            .contains("has not reported a conversation"));
    }

    #[test]
    fn an_explicit_model_still_reaches_the_harness() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Model(Some("haiku".into())));
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert!(app.status().contains("haiku"));
    }

    // ---- running several agents at once ----

    fn ctrl(app: &mut App, code: KeyCode) -> Option<Action> {
        on_key(app, KeyEvent::new(code, KeyModifiers::CONTROL), 20)
    }

    fn running(id: &str, name: &str) -> AgentLine {
        AgentLine {
            id: id.into(),
            name: name.into(),
            harness: "Claude Code".into(),
            status: "running".into(),
            session: Some(format!("sess-{id}")),
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }
    }

    /// The old behaviour left the line sitting in a blocked box with a
    /// scolding, so the only thing to do while an agent worked was to sit still.
    #[test]
    fn a_prompt_typed_mid_turn_is_queued_rather_than_refused() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.busy = true;
        type_line(&mut app, "and then deploy it");

        assert_eq!(press(&mut app, KeyCode::Enter), None, "not sent yet");
        assert_eq!(app.queued, vec!["and then deploy it".to_string()]);
        assert_eq!(app.input, "", "and the box is clear for the next thought");
    }

    #[test]
    fn a_queued_prompt_is_still_recallable_with_the_arrows() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.busy = true;
        type_line(&mut app, "queued thought");
        press(&mut app, KeyCode::Enter);
        app.history_prev();
        assert_eq!(app.input, "queued thought");
    }

    #[test]
    fn a_prompt_typed_while_idle_is_sent_straight_away() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "go");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Send("go".into()))
        );
        assert!(app.queued.is_empty());
    }

    /// The key that makes several jobs at once possible without leaving the UI.
    #[test]
    fn ctrl_b_delegates_the_typed_line_to_a_background_agent() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "audit the dependencies");
        assert_eq!(
            ctrl(&mut app, KeyCode::Char('b')),
            Some(Action::Delegate("audit the dependencies".into()))
        );
        assert_eq!(app.input, "");
    }

    #[test]
    fn delegating_an_empty_line_does_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(ctrl(&mut app, KeyCode::Char('b')), None);
    }

    /// Ctrl-C is quit, so interrupting a run needs a key of its own — otherwise
    /// the only way to stop a runaway agent is to leave the UI entirely.
    #[test]
    fn ctrl_x_stops_the_run_being_watched() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc123".into());
        app.busy = true;
        assert_eq!(
            ctrl(&mut app, KeyCode::Char('x')),
            Some(Action::Stop("abc123".into()))
        );
    }

    #[test]
    fn ctrl_x_with_nothing_running_says_so_rather_than_killing_something() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc123".into());
        assert_eq!(ctrl(&mut app, KeyCode::Char('x')), None);
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("nothing running"));
    }

    /// Walking out on four background jobs without being told is the same
    /// mistake, four times over.
    #[test]
    fn quitting_warns_about_every_running_agent_not_just_the_watched_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("a", "one"), running("b", "two")];
        assert!(!app.busy, "none of them is on screen");

        ctrl(&mut app, KeyCode::Char('c'));
        assert!(!app.should_quit, "the first press must not leave");
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("2 agents"));

        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit, "the second press goes anyway");
    }

    #[test]
    fn quitting_with_nothing_running_needs_no_confirmation() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit);
    }

    // ---- the agents panel as a control surface ----

    fn panel_with_agents() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("aaa11111", "port the parser"), {
            let mut done = running("bbb22222", "write the docs");
            done.status = "completed".into();
            done
        }];
        app.pane = Pane::Agents;
        app
    }

    #[test]
    fn the_panel_arrows_move_its_cursor_rather_than_the_transcript() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(app.agent_sel, 1);
        assert_eq!(app.scroll, 0, "the transcript did not move");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.agent_sel, 0);
    }

    #[test]
    fn enter_on_the_panel_puts_that_agent_on_screen_and_closes_the_panel() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Watch("bbb22222".into()))
        );
        assert_eq!(app.pane, Pane::Chat, "you asked to read it, so show it");
    }

    #[test]
    fn s_stops_the_selected_agent() {
        let mut app = panel_with_agents();
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::Stop("aaa11111".into()))
        );
    }

    /// Killing a finished run only reclaims its tmux session, which is not what
    /// the key looks like it does.
    #[test]
    fn s_on_a_finished_agent_explains_itself_instead() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("nothing to stop"));
    }

    #[test]
    fn a_asks_how_to_attach_to_the_selected_agent() {
        let mut app = panel_with_agents();
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            Some(Action::Attach("aaa11111".into()))
        );
    }

    /// How an unattended run gets picked up and corrected: point the next turn
    /// at its conversation without leaving the UI.
    #[test]
    fn r_points_the_next_turn_at_the_selected_agents_conversation() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.resume, Resume::Session("sess-aaa11111".into()));
        assert_eq!(app.pane, Pane::Chat);
    }

    #[test]
    fn r_on_an_agent_with_no_conversation_refuses_rather_than_starting_a_fresh_one() {
        let mut app = panel_with_agents();
        app.agents[0].session = None;
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.resume, Resume::Fresh);
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("never reported"));
    }

    /// The panel is modal, so its letters are commands. Typing must not leak
    /// into the input box behind it.
    #[test]
    fn letters_typed_at_an_open_panel_do_not_reach_the_input_box() {
        let mut app = panel_with_agents();
        type_line(&mut app, "hello");
        assert_eq!(app.input, "");
    }

    #[test]
    fn esc_closes_the_panel() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.pane, Pane::Chat);
    }

    #[test]
    fn the_team_panel_marks_the_selected_task_done() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.pane = Pane::Team;
        app.tasks = vec![
            jod_core::team::TeamTask {
                id: "t1".into(),
                title: "port the parser".into(),
                owner: None,
                status: "open".into(),
            },
            jod_core::team::TeamTask {
                id: "t2".into(),
                title: "write the docs".into(),
                owner: None,
                status: "done".into(),
            },
        ];
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::FinishTask("t1".into()))
        );

        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), None, "already done");
    }

    // ---- naming an agent without retyping a uuid ----

    #[test]
    fn an_id_prefix_is_enough_to_name_an_agent() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("abcdef12-3456", "work")];
        assert_eq!(
            apply_slash(&mut app, command::Slash::Stop("abcdef".into())),
            Some(Action::Stop("abcdef12-3456".into()))
        );
    }

    /// Stopping the wrong agent is not an undoable mistake, so an ambiguous
    /// prefix must refuse rather than guess.
    #[test]
    fn an_ambiguous_prefix_stops_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("ab111111", "one"), running("ab222222", "two")];
        assert_eq!(apply_slash(&mut app, command::Slash::Stop("ab".into())), None);
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("matches 2"));
    }

    #[test]
    fn naming_an_agent_that_does_not_exist_says_so() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(apply_slash(&mut app, command::Slash::Watch("zz".into())), None);
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("no agent"));
    }

    #[test]
    fn delegate_reaches_the_loop_as_a_background_spawn() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Delegate("do it".into())),
            Some(Action::Delegate("do it".into()))
        );
    }

    // ---- board ids ----

    fn board(ids: &[&str]) -> Vec<jod_core::team::TeamTask> {
        ids.iter()
            .map(|id| jod_core::team::TeamTask {
                id: (*id).into(),
                title: "x".into(),
                owner: None,
                status: "open".into(),
            })
            .collect()
    }

    /// The id has to mean something: a teammate claims it by name from the
    /// command line, where `task-7` tells you nothing.
    #[test]
    fn a_board_id_is_derived_from_the_title() {
        assert_eq!(task_id("Port the parser to Rust", &[]), "port-the-parser-to");
        assert_eq!(task_id("Ship it!", &[]), "ship-it");
    }

    /// Two identical ids on one board means `jod team claim` picks the wrong
    /// row, and nothing about that failure is visible.
    #[test]
    fn a_colliding_board_id_is_given_a_suffix() {
        let existing = board(&["ship-it"]);
        assert_eq!(task_id("Ship it", &existing), "ship-it-2");
        let existing = board(&["ship-it", "ship-it-2"]);
        assert_eq!(task_id("Ship it", &existing), "ship-it-3");
    }

    #[test]
    fn a_title_with_no_usable_characters_still_gets_an_id() {
        assert_eq!(task_id("!!! ???", &[]), "task");
    }

    // ---- scrolling and recall live on different keys ----

    /// The arrows recall what was sent, as every shell does. Scrolling the
    /// transcript keeps PageUp/PageDown and a Ctrl form.
    #[test]
    fn the_arrows_recall_history_and_ctrl_arrows_scroll() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "first prompt");
        press(&mut app, KeyCode::Enter);
        for i in 0..30 {
            app.push(Entry::Agent(format!("line {i}")));
        }

        press(&mut app, KeyCode::Up);
        assert_eq!(app.input, "first prompt");
        assert_eq!(app.scroll, 0, "the transcript stayed put");

        ctrl(&mut app, KeyCode::Up);
        assert_eq!(app.scroll, 1, "Ctrl with an arrow still scrolls");
    }

    /// Typing over a recalled line makes it the user's own draft; ↓ must not
    /// then replace what they are editing.
    #[test]
    fn editing_a_recalled_line_leaves_the_history() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "original");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(app.input, "original!");

        press(&mut app, KeyCode::Down);
        assert_eq!(app.input, "original!", "the edit survived");
    }
}
