//! `jod tui` — the full-screen interface.
//!
//! Layout, top to bottom: a scrolling transcript, an input box, a status bar.
//! `Ctrl-A` reveals a panel listing every delegation this process knows about,
//! which is the part that makes this an orchestrator's UI rather than a chat
//! window — Jod's job is watching several agents, not talking to one.
//!
//! The terminal is put into raw mode and an alternate screen, and **must** be
//! put back however this function exits. A panic that skips the restore leaves
//! the user with an unusable shell, so the restore is installed as a panic hook
//! as well as run on the normal path.

mod app;
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
    refresh_team(&jod, &mut app);
    app.push(Entry::Notice(format!(
        "{} · Enter send · Ctrl-A agents · Ctrl-G team · Ctrl-T thinking · Ctrl-C quit",
        opts.harness.label()
    )));

    let mut keys = EventStream::new();
    let mut events = jod.subscribe();
    // The id of the delegation whose output belongs on screen. Events from any
    // other agent are ignored here; the agents panel is where they show up.
    let mut current: Option<String> = None;
    let mut viewport = 20usize;

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
                        if let Some(prompt) = on_key(&mut app, key, viewport) {
                            match spawn(&jod, &app, &opts, prompt.clone()).await {
                                Ok(id) => {
                                    current = Some(id);
                                    app.busy = true;
                                    app.push(Entry::You(prompt));
                                    app.scroll_to_bottom();
                                }
                                Err(e) => app.push(Entry::Notice(format!("could not start: {e}"))),
                            }
                            app.agents = list_agents(&jod).await;
                            refresh_team(&jod, &mut app);
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
                if current.as_deref() == Some(envelope.agent_id.as_str()) {
                    let finished = matches!(envelope.event, AgentEvent::Finished { .. });
                    app.apply(&envelope.event);
                    if finished {
                        app.agents = list_agents(&jod).await;
                        refresh_team(&jod, &mut app);
                    }
                }
            }
        }
    }
}

/// Handle one keypress. Returns a prompt when the user pressed Enter on a
/// non-empty line and no agent is already running.
fn on_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<String> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let max_scroll = app.transcript.len();

    if ctrl {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') => {
                // Refuse to leave silently while work is in flight; a second
                // press goes anyway.
                if app.busy && !app.confirm_quit {
                    app.confirm_quit = true;
                    app.push(Entry::Notice(
                        "an agent is still running — press again to leave it running".into(),
                    ));
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

    match key.code {
        KeyCode::Enter => {
            if app.busy {
                app.push(Entry::Notice(
                    "still working — wait for this turn to finish".into(),
                ));
                return None;
            }
            return app.take_input();
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_forward(),
        KeyCode::Left => app.left(),
        KeyCode::Right => app.right(),
        KeyCode::Home => app.home(),
        KeyCode::End => app.end(),
        KeyCode::Up => app.scroll_up(1, max_scroll),
        KeyCode::Down => app.scroll_down(1),
        KeyCode::PageUp => app.scroll_up(viewport.max(1), max_scroll),
        KeyCode::PageDown => app.scroll_down(viewport.max(1)),
        KeyCode::Esc => app.scroll_to_bottom(),
        KeyCode::Char(c) => app.insert(c),
        _ => {}
    }
    None
}

async fn spawn(jod: &Arc<Jod>, app: &App, opts: &Options, prompt: String) -> Result<String> {
    let agent = jod
        .spawn_agent(SpawnRequest {
            name: crate::default_name(&prompt),
            harness: opts.harness,
            prompt,
            cwd: opts.cwd.clone(),
            model: opts.model.clone(),
            permission: opts.permission,
            // App owns the conversation cursor: it advances to the exact
            // session the harness reported on the previous turn.
            resume: app.resume.clone(),
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
    jod.agents()
        .await
        .into_iter()
        .map(|a| AgentLine {
            id: a.id,
            name: a.name,
            harness: a.harness_label,
            status: format!("{:?}", a.status).to_lowercase(),
        })
        .collect()
}
