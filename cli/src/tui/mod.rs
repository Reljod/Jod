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
mod data;
mod graph;
mod keys;
mod ui;
mod workspace;

pub use app::{short_duration, AgentLine, App, Entry, Overlay, PromptIntent};
pub use workspace::Workspace;

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
    /// Open the typed line in `$EDITOR`. The TUI has to be suspended and
    /// restored around it, which only the loop can do.
    Editor,
    /// A verb the screens offer and the store cannot carry out yet. Named
    /// rather than silently ignored, and naming the missing call rather than
    /// apologising, so the gap is a to-do and not a mystery.
    Pending {
        verb: String,
        needs: &'static str,
    },
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
    refresh_workspaces(&jod, &mut app);
    app.reconcile();
    // No harness name here: this line is frozen into the scrollback, so naming
    // the harness would leave a stale claim on screen the moment `/harness`
    // switches. The status bar is the one place that tracks it.
    app.push(Entry::Notice(
        "Ctrl-K opens every screen · / for commands · Enter send · Ctrl-B delegate in the background · ? for keys · Ctrl-C quit"
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
                        match on_key(&mut app, key, viewport) {
                            // The editor takes the terminal, so it can only be
                            // done from here — with the same discipline as
                            // `enter`/`restore`, panic hook included.
                            Some(Action::Editor) => edit_in_editor(terminal, &mut app),
                            Some(action) => perform(&jod, &mut app, &opts, action).await,
                            None => {}
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
                    refresh_team(&jod, &mut app);
                    refresh_workspaces(&jod, &mut app);
                    app.reconcile();
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
                    refresh_workspaces(&jod, &mut app);
                    if matches!(app.workspace, Workspace::Team | Workspace::Tasks) {
                        refresh_team(&jod, &mut app);
                    }
                    app.reconcile();
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
                    agent.watch_command
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
        // Suspending and restoring the terminal is the loop's job, so this is
        // handled there rather than here. Reaching it means the loop did not.
        Action::Editor => app.push(Entry::Notice(
            "no $EDITOR handoff from here — set $EDITOR and try Ctrl-F in chat".into(),
        )),
        // Named rather than silently ignored: a key that appears to do nothing
        // is worse than one that says what it is waiting for.
        Action::Pending { verb, needs } => {
            app.push(Entry::Notice(format!("{verb} — not wired yet: needs {needs}")));
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
///
/// Three layers, checked in order, and the status bar always says which one you
/// are in: an **overlay** owns the keyboard while it is up, a **workspace**
/// makes letters into commands, and **chat** makes them text again. Quitting is
/// ahead of all three, because a key that cannot always leave is a trap.
fn on_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
        return on_quit(app);
    }
    // Any key other than a second quit means the user changed their mind.
    app.confirm_quit = false;

    if app.overlay.is_open() {
        return on_overlay_key(app, key);
    }
    if ctrl {
        if let Some(action) = on_chord(app, key) {
            return action;
        }
    }
    if app.workspace.is_list() {
        return on_workspace_key(app, key, viewport);
    }
    on_chat_key(app, key, viewport)
}

/// Refuse to leave silently while work is in flight; a second press goes
/// anyway. Any running agent counts, not just the one on screen — walking out
/// on four background jobs without being told is the same mistake, four times
/// over.
fn on_quit(app: &mut App) -> Option<Action> {
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
    None
}

/// The chords that work in every layer. `Some` means the chord was handled.
fn on_chord(app: &mut App, key: KeyEvent) -> Option<Option<Action>> {
    let handled = |a: Option<Action>| Some(a);
    match key.code {
        // The leader. One free chord reaches nine screens, which is the whole
        // reason this is a menu rather than five more chords.
        KeyCode::Char('k') => {
            app.overlay = Overlay::WhichKey;
            handled(None)
        }
        KeyCode::Char('a') => {
            app.go(if app.workspace == Workspace::Fleet {
                Workspace::Chat
            } else {
                Workspace::Fleet
            });
            handled(None)
        }
        KeyCode::Char('g') => {
            app.go(if app.workspace == Workspace::Team {
                Workspace::Chat
            } else {
                Workspace::Team
            });
            handled(None)
        }
        // Only meaningful once cron, goals and webhooks report endings while
        // nobody is at the terminal.
        KeyCode::Char('n') => {
            jump_to_oldest_unread(app);
            handled(None)
        }
        // `$EDITOR` on the input. Claude Code spells this `Ctrl+G`, which is
        // Jod's team panel and is documented — so `Ctrl-F`, with `Ctrl-K e` as
        // the discoverable alias.
        KeyCode::Char('f') => handled(Some(Action::Editor)),
        KeyCode::Char('t') => {
            app.show_thinking = !app.show_thinking;
            app.push(Entry::Notice(format!(
                "thinking {}",
                if app.show_thinking { "shown" } else { "hidden" }
            )));
            handled(None)
        }
        KeyCode::Char('o') => {
            app.show_details = !app.show_details;
            app.push(Entry::Notice(format!(
                "tool output {}",
                if app.show_details { "shown" } else { "hidden" }
            )));
            handled(None)
        }
        // Delegate: the typed line becomes an agent that runs without taking
        // the screen. This is the key that makes several jobs at once possible
        // without leaving the UI.
        KeyCode::Char('b') => handled(app.take_input().map(Action::Delegate)),
        // Stop what is being watched. Ctrl-C is quit, so interrupting a run
        // needs a key of its own or the only way out is to leave.
        KeyCode::Char('x') => handled(match app.watching.clone() {
            Some(id) if app.busy => Some(Action::Stop(id)),
            _ => {
                app.push(Entry::Notice("nothing running to stop here".into()));
                None
            }
        }),
        KeyCode::Char('l') => {
            app.transcript.clear();
            app.scroll_to_bottom();
            handled(None)
        }
        KeyCode::Char('u') => {
            app.clear_line();
            handled(None)
        }
        KeyCode::Char('w') => {
            app.delete_word();
            handled(None)
        }
        // Scrolling keeps a Ctrl form, because the bare arrows now walk back
        // through what has been sent.
        KeyCode::Up => {
            let max = app.transcript.len();
            app.scroll_up(1, max);
            handled(None)
        }
        KeyCode::Down => {
            app.scroll_down(1);
            handled(None)
        }
        // Ctrl-A is the fleet, so start-of-line is Home rather than the
        // readline binding.
        KeyCode::Home => {
            app.home();
            handled(None)
        }
        KeyCode::Char('e') | KeyCode::End => {
            app.end();
            handled(None)
        }
        _ => None,
    }
}

/// Put the cursor on the oldest thing you have not read, and open it.
fn jump_to_oldest_unread(app: &mut App) {
    let oldest = app
        .activity
        .iter()
        .filter(|a| a.unread)
        .min_by_key(|a| a.at_ms)
        .map(|a| a.id.clone());
    app.go(Workspace::Activity);
    match oldest {
        Some(id) => app.list_mut(Workspace::Activity).selected = Some(id),
        None => app.push(Entry::Notice("nothing unread".into())),
    }
}

/// Keys while an overlay is up. `Esc` always cancels exactly this overlay.
fn on_overlay_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    // A prompt is a line being typed, so it takes characters before anything
    // else looks at them.
    if let Overlay::Prompt {
        value,
        intent,
        label,
    } = &mut app.overlay
    {
        match key.code {
            KeyCode::Char(c) => {
                value.push(c);
                return None;
            }
            KeyCode::Backspace => {
                value.pop();
                return None;
            }
            KeyCode::Enter => {
                let (typed, intent, label) = (value.clone(), intent.clone(), label.clone());
                app.overlay = Overlay::None;
                return accept_prompt(app, label, typed, intent);
            }
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    match &app.overlay {
        // A destructive verb on a bare letter is one fat-fingered `Ctrl-K h x`
        // away from losing a secret, so the confirmation names the thing.
        Overlay::Confirm { verb, what, .. } => {
            let (verb, what) = (verb.clone(), what.clone());
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.overlay = Overlay::None;
                    Some(Action::Pending {
                        verb: format!("{verb} {what}"),
                        needs: "the store method for this kind — see cli/src/tui/data.rs",
                    })
                }
                // Anything that is not a yes is a no.
                _ => {
                    app.overlay = Overlay::None;
                    None
                }
            }
        }
        Overlay::Keymap => {
            app.overlay = Overlay::None;
            None
        }
        Overlay::WhichKey => on_which_key(app, key),
        Overlay::WhichKeyNew => {
            app.overlay = Overlay::None;
            match key.code {
                KeyCode::Char(c) => match Workspace::from_letter(c) {
                    Some(ws) if ws.is_list() => {
                        app.go(ws);
                        begin_new(app, ws)
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        Overlay::Prompt { .. } | Overlay::None => {
            app.overlay = Overlay::None;
            None
        }
    }
}

/// The which-key menu's second keystroke. Anything it does not know cancels
/// silently rather than doing something surprising.
fn on_which_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let KeyCode::Char(c) = key.code else {
        app.overlay = Overlay::None;
        return None;
    };
    match c {
        'n' => {
            app.overlay = Overlay::WhichKeyNew;
            None
        }
        'e' => {
            app.overlay = Overlay::None;
            Some(Action::Editor)
        }
        '?' => {
            app.overlay = Overlay::Keymap;
            None
        }
        _ => {
            app.overlay = Overlay::None;
            if let Some(ws) = Workspace::from_letter(c) {
                app.go(ws);
            }
            None
        }
    }
}

/// Keys while a workspace owns the keyboard.
fn on_workspace_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Action> {
    let ws = app.workspace;
    if ws == Workspace::MemoryGraph {
        return on_graph_key(app, key);
    }

    // A `/` line being typed owns the keyboard, letters and all — otherwise
    // filtering for "stop" would stop something.
    if app.here().editing_filter {
        match key.code {
            KeyCode::Char(c) => {
                if let Some(f) = app.here_mut().filter.as_mut() {
                    f.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(f) = app.here_mut().filter.as_mut() {
                    f.pop();
                }
            }
            // Accepting keeps the filter and hands the letters back to being
            // commands. `Esc` is the one that clears it.
            KeyCode::Enter => app.here_mut().editing_filter = false,
            KeyCode::Esc => {
                let list = app.here_mut();
                list.filter = None;
                list.editing_filter = false;
            }
            _ => {}
        }
        app.reconcile();
        return None;
    }

    let ids = app.row_ids(ws);
    let page = viewport.max(1) as isize;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.back();
            return None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.here_mut().step(-1, &ids);
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.here_mut().step(1, &ids);
            return None;
        }
        KeyCode::PageUp => {
            app.here_mut().step(-page, &ids);
            return None;
        }
        KeyCode::PageDown => {
            app.here_mut().step(page, &ids);
            return None;
        }
        KeyCode::Home => {
            app.here_mut().first(&ids);
            return None;
        }
        KeyCode::End => {
            app.here_mut().last(&ids);
            return None;
        }
        KeyCode::Char('/') => {
            let list = app.here_mut();
            list.filter = Some(String::new());
            list.editing_filter = true;
            return None;
        }
        KeyCode::Char('S') => {
            app.here_mut().sort += 1;
            app.reconcile();
            let name = ws.sort_name(app.here().sort);
            app.push(Entry::Notice(format!("sorted by {name}")));
            return None;
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            return None;
        }
        KeyCode::Char('n') => return begin_new(app, ws),
        KeyCode::Char('e') if is_editable(ws) => {
            return selected_label(app, ws).map(|what| Action::Pending {
                verb: format!("edit {what}"),
                needs: "the $EDITOR form ladder — tier 3 of the report's §5.4",
            })
        }
        KeyCode::Char('x') if is_editable(ws) => {
            if let Some(what) = selected_label(app, ws) {
                app.overlay = Overlay::Confirm {
                    verb: delete_verb(ws).to_string(),
                    what,
                };
            }
            return None;
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(target) = Workspace::from_digit(c) {
                app.go(target);
            }
            return None;
        }
        _ => {}
    }

    match ws {
        Workspace::Fleet => on_fleet_key(app, key),
        Workspace::Memory => on_memory_key(app, key),
        Workspace::Schedules => on_schedule_key(app, key),
        Workspace::Goals => on_goal_key(app, key),
        Workspace::Hooks => on_hook_key(app, key),
        Workspace::Tasks => on_task_key(app, key),
        Workspace::Activity => on_activity_key(app, key),
        Workspace::Team => on_team_key(app, key),
        Workspace::Chat | Workspace::MemoryGraph => None,
    }
}

/// The screens where editing and deleting mean something.
///
/// A run is not edited and an activity line is not deleted, so on those screens
/// `e` and `x` fall through to the screen's own keys rather than offering a
/// verb that cannot exist — which is the same rule that keeps `s` from
/// pretending to stop a finished run.
fn is_editable(ws: Workspace) -> bool {
    matches!(
        ws,
        Workspace::Memory
            | Workspace::Schedules
            | Workspace::Goals
            | Workspace::Hooks
            | Workspace::Tasks
    )
}

/// What `x` is called on each screen. "Forget" rather than "delete" for memory,
/// because that is what it does to you rather than to a row.
fn delete_verb(ws: Workspace) -> &'static str {
    match ws {
        Workspace::Memory => "forget",
        Workspace::Tasks => "remove",
        _ => "delete",
    }
}

/// The name of whatever the cursor is on, for a confirmation that names it.
fn selected_label(app: &App, ws: Workspace) -> Option<String> {
    match ws {
        Workspace::Fleet => app.selected_agent().map(|a| a.name.clone()),
        Workspace::Memory => app.selected_memory().map(|n| n.name.clone()),
        Workspace::Schedules => app.selected_schedule().map(|s| s.name.clone()),
        Workspace::Goals => app.selected_goal().map(|g| g.name.clone()),
        Workspace::Hooks => app.selected_hook().map(|h| h.name.clone()),
        Workspace::Tasks => app.selected_board_task().map(|t| t.id),
        Workspace::Activity => app.selected_activity().map(|a| a.id.clone()),
        Workspace::Team => app.selected_task().map(|t| t.id.clone()),
        Workspace::Chat | Workspace::MemoryGraph => None,
    }
}

/// `n` — tier 1 of the form ladder for the kinds whose first question is one
/// value, and a named to-do for the kinds that need the editor.
fn begin_new(app: &mut App, ws: Workspace) -> Option<Action> {
    let label = match ws {
        Workspace::Memory => "remember",
        Workspace::Tasks | Workspace::Team => "task",
        Workspace::Schedules => "schedule",
        Workspace::Goals => "goal",
        Workspace::Hooks => "webhook",
        _ => return None,
    };
    app.overlay = Overlay::Prompt {
        label: label.to_string(),
        value: String::new(),
        intent: PromptIntent::New(ws),
    };
    None
}

/// What a tier-1 prompt does once `⏎` is pressed.
fn accept_prompt(
    app: &mut App,
    label: String,
    typed: String,
    intent: PromptIntent,
) -> Option<Action> {
    let typed = typed.trim().to_string();
    if typed.is_empty() {
        return None;
    }
    match intent {
        // The board is the one kind the store can already take.
        PromptIntent::New(Workspace::Tasks) | PromptIntent::New(Workspace::Team) => {
            Some(Action::AddTask(typed))
        }
        PromptIntent::New(Workspace::Memory) => Some(Action::Pending {
            verb: format!("remember “{typed}”"),
            needs: "Store::remember with the memory-types NewFact shape",
        }),
        PromptIntent::New(ws) => Some(Action::Pending {
            verb: format!("new {} “{typed}”", ws.menu_name()),
            needs: "the $EDITOR form ladder — tier 3 of the report's §5.4",
        }),
        PromptIntent::Link(from) => Some(Action::Pending {
            verb: format!("link {from} → {typed}"),
            needs: "Store::link_memory from the graph work",
        }),
    }
    .or_else(|| {
        app.push(Entry::Notice(format!("{label}: nothing to do")));
        None
    })
}

fn on_fleet_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        // Reading a run is the common case, so it is the plain key.
        KeyCode::Enter => {
            let id = app.selected_agent()?.id.clone();
            app.go(Workspace::Chat);
            Some(Action::Watch(id))
        }
        KeyCode::Char('s') => {
            let agent = app.selected_agent()?;
            let (id, running, status) = (agent.id.clone(), agent.is_running(), agent.status.clone());
            if !running {
                // Killing a finished run only reclaims its tmux session, which
                // is not what "s" looks like it does. Say so instead.
                app.push(Entry::Notice(format!(
                    "{} is already {status} — nothing to stop",
                    short(&id)
                )));
                return None;
            }
            Some(Action::Stop(id))
        }
        KeyCode::Char('a') => Some(Action::Attach(app.selected_agent()?.id.clone())),
        // Continue the selected agent's conversation from the input box, which
        // is how an unattended run gets picked up and corrected.
        KeyCode::Char('r') => {
            let agent = app.selected_agent()?.clone();
            app.go(Workspace::Chat);
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
        // Run the same prompt again as a fresh background agent — the fastest
        // way to retry something that nearly worked.
        KeyCode::Char('d') => {
            let agent = app.selected_agent()?;
            Some(Action::Delegate(agent.name.clone()))
        }
        _ => None,
    }
}

fn on_memory_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => None,
        // The local graph is memory's second level, so it is drilled into
        // rather than jumped to: Esc comes back to the list.
        KeyCode::Char('g') => {
            let id = app.selected_memory()?.id.clone();
            app.graph = graph::GraphView::new(id);
            app.drill(Workspace::MemoryGraph);
            None
        }
        KeyCode::Char('t') => {
            app.memory_type = next_memory_type(app.memory_type);
            app.reconcile();
            let what = app
                .memory_type
                .map(|k| k.label().to_string())
                .unwrap_or_else(|| "every kind".into());
            app.push(Entry::Notice(format!("showing {what}")));
            None
        }
        KeyCode::Char('l') => {
            let from = app.selected_memory()?.name.clone();
            app.overlay = Overlay::Prompt {
                label: format!("link {from} →"),
                value: String::new(),
                intent: PromptIntent::Link(from),
            };
            None
        }
        // Memory writes are already events, so the last one can be un-written —
        // which is strictly better than a confirmation dialog.
        KeyCode::Char('u') => Some(Action::Pending {
            verb: "undo the last memory write".into(),
            needs: "Store::undo_last_memory_write from the memory-types work",
        }),
        _ => None,
    }
}

fn next_memory_type(current: Option<data::MemoryKind>) -> Option<data::MemoryKind> {
    let all = data::MemoryKind::ALL;
    match current {
        None => Some(all[0]),
        Some(kind) => match all.iter().position(|k| *k == kind) {
            Some(at) if at + 1 < all.len() => Some(all[at + 1]),
            _ => None,
        },
    }
}

fn on_graph_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let node = app.focused_memory().cloned();
    let rows = node
        .as_ref()
        .map(|n| graph::neighbours(n, app.graph.edge_kind.as_deref()))
        .unwrap_or_default();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g') => {
            app.back();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.graph.step(-1, rows.len());
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.graph.step(1, rows.len());
            None
        }
        KeyCode::Home => {
            app.graph.sel = 0;
            None
        }
        KeyCode::End => {
            app.graph.sel = rows.len().saturating_sub(1);
            None
        }
        KeyCode::Enter => {
            let row = rows.get(app.graph.sel)?;
            app.graph.recentre(row.edge.other.clone());
            // The list's cursor follows the eye, so leaving the graph lands on
            // the node you were last looking at rather than the one you left.
            app.list_mut(Workspace::Memory).selected = Some(app.graph.focus.clone());
            None
        }
        // Walking a graph without being able to walk back out of it is how you
        // get lost in one. An empty stack leaves the graph entirely.
        KeyCode::Backspace => {
            if !app.graph.back() {
                app.back();
            } else {
                app.list_mut(Workspace::Memory).selected = Some(app.graph.focus.clone());
            }
            None
        }
        KeyCode::Char('h') => {
            app.graph.toggle_hops();
            None
        }
        KeyCode::Char('f') => {
            let kinds = node.as_ref().map(graph::edge_kinds).unwrap_or_default();
            app.graph.cycle_edge_kind(&kinds);
            None
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            None
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(target) = Workspace::from_digit(c) {
                app.go(target);
            }
            None
        }
        _ => None,
    }
}

fn on_schedule_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let name = app.selected_schedule().map(|s| s.name.clone())?;
    match key.code {
        KeyCode::Enter => Some(Action::Pending {
            verb: format!("open {name}'s last run"),
            needs: "Store::fires to name the run a fire started",
        }),
        KeyCode::Char('r') => Some(Action::Pending {
            verb: format!("run {name} now"),
            needs: "Jod::fire_schedule from the scheduler work",
        }),
        KeyCode::Char('p') => Some(Action::Pending {
            verb: format!("pause or resume {name}"),
            needs: "Store::set_schedule_state, which exists but is not wired here",
        }),
        // Dry run: the honest answer to "did I get the cron right", which no
        // amount of staring at `0 2 * * *` gives you.
        KeyCode::Char('t') => Some(Action::Pending {
            verb: format!("show {name}'s next five fire times"),
            needs: "jod_core::schedule::next_fire, called five times",
        }),
        _ => None,
    }
}

fn on_goal_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let name = app.selected_goal().map(|g| g.name.clone())?;
    match key.code {
        KeyCode::Enter => Some(Action::Pending {
            verb: format!("open {name}'s last iteration"),
            needs: "Store::goal_iterations from the scheduler work",
        }),
        KeyCode::Char('r') => Some(Action::Pending {
            verb: format!("run an iteration of {name} now"),
            needs: "Jod::run_goal_iteration from the scheduler work",
        }),
        KeyCode::Char('p') => Some(Action::Pending {
            verb: format!("pause {name}"),
            needs: "Store::set_goal_state from the scheduler work",
        }),
        // A looping objective that quietly needs you and never says so is worse
        // than no goal at all.
        KeyCode::Char('a') => Some(Action::Pending {
            verb: format!("answer {name}'s escalation"),
            needs: "Store::answer_escalation from the scheduler work",
        }),
        _ => None,
    }
}

fn on_hook_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let hook = app.selected_hook()?;
    let (name, endpoint) = (hook.name.clone(), hook.endpoint.clone());
    match key.code {
        KeyCode::Enter => Some(Action::Pending {
            verb: format!("open the run {name}'s last delivery started"),
            needs: "Store::deliveries from the webhook work",
        }),
        KeyCode::Char('t') => Some(Action::Pending {
            verb: format!("test {name} with a sample payload"),
            needs: "Jod::test_webhook from the webhook work",
        }),
        KeyCode::Char('p') => Some(Action::Pending {
            verb: format!("pause {name}"),
            needs: "Store::set_webhook_state from the webhook work",
        }),
        KeyCode::Char('c') => {
            // No clipboard daemon and no OSC 52 yet, so the URL goes where it
            // can always be copied from: the transcript.
            app.push(Entry::Notice(format!("{name}: {endpoint}")));
            None
        }
        _ => None,
    }
}

fn on_task_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let task = app.selected_board_task()?;
    match key.code {
        KeyCode::Enter => {
            if task.state == data::TaskState::Done {
                app.push(Entry::Notice(format!("{} is already done", task.id)));
                return None;
            }
            Some(Action::FinishTask(task.id))
        }
        // The verb that makes the board worth a screen: it turns a task into an
        // agent run, so the board is where work starts rather than a list kept
        // in parallel with the fleet.
        KeyCode::Char('d') => Some(Action::Delegate(delegation_prompt(&task))),
        KeyCode::Char('c') => Some(Action::Pending {
            verb: format!("claim {}", task.id),
            needs: "Store::claim_task, which exists but is not wired here",
        }),
        KeyCode::Char('o') => match task.run {
            Some(run) => Some(Action::Watch(run)),
            None => {
                app.push(Entry::Notice(format!("{} has no run yet", task.id)));
                None
            }
        },
        _ => None,
    }
}

/// The prompt `d` seeds an agent with: the title, the runnable check, and the
/// spec, so the run starts knowing what "done" means.
fn delegation_prompt(task: &data::TaskRow) -> String {
    let mut prompt = task.title.clone();
    if !task.check.trim().is_empty() {
        prompt.push_str(&format!("\n\nIt is done when this passes: {}", task.check));
    }
    if let Some(spec) = &task.spec {
        prompt.push_str(&format!("\n\nThe spec is {spec}."));
    }
    prompt
}

fn on_activity_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => {
            let (ws, id) = app.selected_activity()?.jump_to.clone()?;
            app.go(ws);
            app.list_mut(ws).selected = Some(id);
            None
        }
        KeyCode::Char('m') => {
            let id = app.selected_activity()?.id.clone();
            if let Some(item) = app.activity.iter_mut().find(|a| a.id == id) {
                item.unread = false;
            }
            app.reconcile();
            None
        }
        KeyCode::Char('M') => {
            for item in &mut app.activity {
                item.unread = false;
            }
            app.reconcile();
            None
        }
        KeyCode::Char('u') => {
            app.unread_only = !app.unread_only;
            app.reconcile();
            None
        }
        KeyCode::Char('f') => {
            app.activity_source = next_source(app.activity_source);
            app.reconcile();
            None
        }
        _ => None,
    }
}

fn next_source(current: Option<data::Source>) -> Option<data::Source> {
    let all = data::Source::ALL;
    match current {
        None => Some(all[0]),
        Some(source) => match all.iter().position(|s| *s == source) {
            Some(at) if at + 1 < all.len() => Some(all[at + 1]),
            _ => None,
        },
    }
}

fn on_team_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => {
            let task = app.selected_task()?;
            if task.is_done() {
                app.push(Entry::Notice(format!("{} is already done", task.id)));
                return None;
            }
            Some(Action::FinishTask(task.id.clone()))
        }
        _ => None,
    }
}

/// Keys in chat, where letters are text and the input box owns them.
fn on_chat_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Action> {
    let max_scroll = app.transcript.len();

    // While the completion popup is up it owns Tab and the arrows, and Enter
    // finishes the word rather than sending a half-typed command.
    let suggestions = command::completions(&app.input, app);
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
        KeyCode::Esc => app.back(),
        // `?` on an *empty* input opens the keymap; with anything typed it is
        // the character it looks like. Backspacing down to a lone `?` therefore
        // never fires it, which is the edge case that makes the rule usable.
        KeyCode::Char('?') if app.input.is_empty() => app.overlay = Overlay::Keymap,
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
            app.go(Workspace::Fleet);
            app.push(Entry::Notice(
                "pick an id from the fleet, then /resume <id> — the shown prefix is enough".into(),
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
        // Every workspace verb is a toggle, so typing the command you are
        // already looking at takes you home rather than doing nothing.
        Slash::Open(ws) => app.go(toggled(app.workspace, ws)),
        // Naming a row lands the cursor on it, which is what makes
        // `/schedule nightly-inbox` worth having beside `/schedules`.
        Slash::OpenNamed(ws, name) => {
            app.go(ws);
            match app.row_ids(ws).into_iter().find(|id| id.starts_with(&name)) {
                Some(id) => app.list_mut(ws).selected = Some(id),
                None => app.push(Entry::Notice(format!(
                    "no {} called {name}",
                    ws.menu_name()
                ))),
            }
        }
        Slash::Memory(query) => {
            app.go(Workspace::Memory);
            if let Some(q) = query {
                let list = app.list_mut(Workspace::Memory);
                list.filter = Some(q);
                list.editing_filter = false;
                app.reconcile();
            }
        }
        Slash::NewKind(ws) => {
            app.go(ws);
            return begin_new(app, ws);
        }
        Slash::Pause(name) => {
            return Some(Action::Pending {
                verb: format!("pause {name}"),
                needs: "Store::set_schedule_state / set_goal_state, wired to a name lookup",
            })
        }
        Slash::Unpause(name) => {
            return Some(Action::Pending {
                verb: format!("resume {name}"),
                needs: "Store::set_schedule_state / set_goal_state, wired to a name lookup",
            })
        }
        Slash::Run(name) => {
            return Some(Action::Pending {
                verb: format!("run {name} now"),
                needs: "Jod::fire_schedule / run_goal_iteration from the scheduler work",
            })
        }
        Slash::Remember(text) => {
            return Some(Action::Pending {
                verb: format!("remember “{text}”"),
                needs: "Store::remember with the memory-types NewFact shape",
            })
        }
        Slash::Forget(name) => {
            return Some(Action::Pending {
                verb: format!("forget {name}"),
                needs: "Store::forget, wired to a node id rather than a triple",
            })
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

/// Where a workspace command lands: the workspace, unless you are already
/// there, in which case home.
fn toggled(here: Workspace, asked: Workspace) -> Workspace {
    if here == asked {
        Workspace::Chat
    } else {
        asked
    }
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

/// Re-read what the workspaces show.
///
/// Off the render path, on the tick, exactly as the team board already is — and
/// for the same reason: cron, webhooks and goals write these from *other
/// processes*, so an in-memory copy could never be authoritative. Each loader
/// swallows its own errors rather than taking the UI down over a locked
/// database.
fn refresh_workspaces(jod: &Arc<Jod>, app: &mut App) {
    app.memory = data::memory(jod);
    app.schedules = data::schedules(jod);
    app.goals = data::goals(jod);
    app.hooks = data::hooks(jod);
    app.activity = data::activity(jod);
    app.board = data::tasks(jod);
}

/// Hand the typed line to `$EDITOR`, and take back whatever comes out.
///
/// The user already has a configured editor; a one-line TUI field will never
/// beat it for a forty-line prompt. The terminal has to be given back and
/// retaken around the child, with the same discipline as `enter`/`restore` —
/// including the panic hook, which `enter` reinstalls.
fn edit_in_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) {
    let Some(editor) = std::env::var("EDITOR").ok().filter(|e| !e.trim().is_empty()) else {
        app.push(Entry::Notice(
            "no $EDITOR set — export one and press Ctrl-F again".into(),
        ));
        return;
    };

    let path = std::env::temp_dir().join(format!("jod-prompt-{}.md", std::process::id()));
    if let Err(e) = std::fs::write(&path, &app.input) {
        app.push(Entry::Notice(format!("could not open an editor: {e}")));
        return;
    }

    restore();
    let status = std::process::Command::new(&editor).arg(&path).status();
    let re_entered = enter();
    match re_entered {
        Ok(fresh) => *terminal = fresh,
        Err(e) => {
            app.push(Entry::Notice(format!("could not take the terminal back: {e}")));
            return;
        }
    }
    let _ = terminal.clear();

    match status {
        // A failed edit must not throw the work away — that is the one thing a
        // form must never do.
        Ok(code) if !code.success() => {
            app.push(Entry::Notice(format!("{editor} exited {code} — nothing changed")));
        }
        Err(e) => app.push(Entry::Notice(format!("could not run {editor}: {e}"))),
        Ok(_) => match std::fs::read_to_string(&path) {
            Ok(text) => {
                app.input = text.trim_end().to_string();
                app.cursor = app.input.len();
            }
            Err(e) => app.push(Entry::Notice(format!("could not read it back: {e}"))),
        },
    }
    let _ = std::fs::remove_file(&path);
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

    // ---- the fleet as a control surface ----

    fn panel_with_agents() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("aaa11111", "port the parser"), {
            let mut done = running("bbb22222", "write the docs");
            done.status = "completed".into();
            done
        }];
        app.go(Workspace::Fleet);
        app
    }

    /// The id the fleet cursor is on, which is what every key acts on.
    fn fleet_at(app: &App) -> String {
        app.selected_agent().map(|a| a.id.clone()).unwrap_or_default()
    }

    #[test]
    fn the_panel_arrows_move_its_cursor_rather_than_the_transcript() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(fleet_at(&app), "bbb22222");
        assert_eq!(app.scroll, 0, "the transcript did not move");
        press(&mut app, KeyCode::Up);
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    /// `j` and `k` do the same, on every workspace, so vim fingers work without
    /// vim modes.
    #[test]
    fn jk_move_the_cursor_wherever_the_arrows_do() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(fleet_at(&app), "bbb22222");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    #[test]
    fn enter_on_the_panel_puts_that_agent_on_screen_and_closes_the_panel() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Watch("bbb22222".into()))
        );
        assert_eq!(
            app.workspace,
            Workspace::Chat,
            "you asked to read it, so show it"
        );
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
        assert_eq!(app.workspace, Workspace::Chat);
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
        assert_eq!(app.workspace, Workspace::Chat);
    }

    #[test]
    fn the_team_panel_marks_the_selected_task_done() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Team);
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
        // The cursor is an id, so it has to be placed once the rows exist —
        // which is what the refresh does in the loop.
        app.reconcile();
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

    // ---- the which-key menu ----

    /// The discoverability spine. One free chord, a menu of every screen, and
    /// recognition instead of recall.
    #[test]
    fn ctrl_k_opens_the_which_key_menu() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        assert_eq!(app.overlay, Overlay::WhichKey);
    }

    #[test]
    fn a_which_key_letter_reaches_its_workspace() {
        for (letter, expected) in [
            ('f', Workspace::Fleet),
            ('m', Workspace::Memory),
            ('s', Workspace::Schedules),
            ('g', Workspace::Goals),
            ('h', Workspace::Hooks),
            ('t', Workspace::Tasks),
            ('a', Workspace::Activity),
            ('w', Workspace::Team),
            ('c', Workspace::Chat),
        ] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            ctrl(&mut app, KeyCode::Char('k'));
            press(&mut app, KeyCode::Char(letter));
            assert_eq!(app.workspace, expected, "Ctrl-K {letter}");
            assert_eq!(app.overlay, Overlay::None, "and the menu closed");
        }
    }

    /// Any key the menu does not know cancels silently rather than doing
    /// something surprising.
    #[test]
    fn an_unknown_which_key_letter_cancels_without_going_anywhere() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.workspace, Workspace::Chat);
        assert_eq!(app.input, "", "and it certainly is not typed into the box");
    }

    #[test]
    fn esc_cancels_the_which_key_menu() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// `Ctrl-K n s` is the two-key route into making a schedule.
    #[test]
    fn the_new_submenu_lands_on_the_screen_and_opens_its_prompt() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.overlay, Overlay::WhichKeyNew);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.workspace, Workspace::Schedules);
        assert!(matches!(app.overlay, Overlay::Prompt { .. }), "{:?}", app.overlay);
    }

    #[test]
    fn ctrl_k_question_mark_opens_the_keymap() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
    }

    /// `Ctrl-A` and `Ctrl-G` keep exactly the meanings they have today, and
    /// pressing them again comes home.
    #[test]
    fn the_old_chords_still_toggle_their_screens() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('a'));
        assert_eq!(app.workspace, Workspace::Fleet);
        ctrl(&mut app, KeyCode::Char('a'));
        assert_eq!(app.workspace, Workspace::Chat);

        ctrl(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::Team);
        ctrl(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    // ---- Esc goes back exactly one level ----

    fn with_memory() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.memory = vec![memory_node("prefers-spec-first"), memory_node("linear-is-truth")];
        app.go(Workspace::Memory);
        app
    }

    fn memory_node(name: &str) -> data::MemoryNode {
        data::MemoryNode {
            id: name.into(),
            name: name.into(),
            kind: data::MemoryKind::Belief,
            confidence: 0.9,
            degree: 3,
            age_ms: 0,
            seen: 1,
            body: "a belief".into(),
            contradicted: false,
            in_edges: vec![data::MemoryEdge {
                kind: "supports".into(),
                other: "linear-is-truth".into(),
                other_name: "linear-is-truth".into(),
                other_kind: data::MemoryKind::Belief,
                warn: false,
            }],
            out_edges: vec![],
            provenance: vec![],
        }
    }

    /// One back key, one meaning, and the bottom is always chat.
    #[test]
    fn esc_unwinds_exactly_one_level_and_ends_at_chat() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Memory, "one level, not two");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat);

        // And chat's own Esc is unchanged: follow the tail again.
        app.scroll_up(3, 10);
        press(&mut app, KeyCode::Esc);
        assert!(app.following());
    }

    /// An active filter is a level of its own: `Esc` clears it before it takes
    /// you anywhere, so you never lose the screen by trying to clear the box.
    #[test]
    fn esc_clears_an_active_filter_before_it_leaves_the_screen() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "spec");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.here().filter.as_deref(), Some("spec"));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.here().filter, None, "the filter went");
        assert_eq!(app.workspace, Workspace::Memory, "and the screen stayed");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// A top-level jump forgets the way back deliberately: `Esc` twice landing
    /// in a memory node you forgot opening is a maze, not a back button.
    #[test]
    fn jumping_to_another_workspace_resets_the_way_back() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);

        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.workspace, Workspace::Schedules);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat, "straight home");
    }

    #[test]
    fn q_is_a_synonym_for_esc_in_a_workspace() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    // ---- direct jumps ----

    #[test]
    fn a_digit_jumps_straight_to_its_workspace_from_another_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Fleet);
        for (digit, expected) in [
            ('1', Workspace::Chat),
            ('3', Workspace::Memory),
            ('4', Workspace::Schedules),
            ('8', Workspace::Activity),
        ] {
            app.go(Workspace::Fleet);
            press(&mut app, KeyCode::Char(digit));
            assert_eq!(app.workspace, expected, "digit {digit}");
        }
    }

    /// Digits stay literal text in chat. For digits to be navigation, digits
    /// would have to stop being text — which is a mode, and this is not one.
    #[test]
    fn digits_are_still_text_in_the_chat_box() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "run 4 agents");
        assert_eq!(app.input, "run 4 agents");
        assert_eq!(app.workspace, Workspace::Chat);
    }

    // ---- the `?` overlay ----

    /// Claude Code's rule exactly, including the edge case: backspacing down to
    /// a lone `?` must not fire it, because the key only acts on an *empty*
    /// input.
    #[test]
    fn question_mark_opens_the_keymap_only_on_an_empty_input() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
        press(&mut app, KeyCode::Esc);

        type_line(&mut app, "what?");
        assert_eq!(app.overlay, Overlay::None, "with text typed it is a character");
        assert_eq!(app.input, "what?");

        // Backspacing down to a lone `?` leaves it as text, not as a command:
        // the rule is about the keypress, not about what is in the box after
        // it.
        app.clear_line();
        app.input = "?x".into();
        app.cursor = app.input.len();
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input, "?");
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn question_mark_opens_the_keymap_on_a_workspace_too() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.overlay, Overlay::None, "any key closes it");
    }

    // ---- filtering ----

    /// While the `/` line is being typed it owns the keyboard, letters and all
    /// — otherwise filtering for "stop" would stop something.
    #[test]
    fn a_filter_being_typed_swallows_the_letters_that_are_otherwise_commands() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "stop");
        assert_eq!(app.here().filter.as_deref(), Some("stop"));
        assert!(
            app.transcript.is_empty(),
            "nothing was stopped: {:?}",
            app.transcript
        );
    }

    #[test]
    fn a_filter_narrows_the_list_and_the_cursor_lands_on_what_is_left() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "docs");
        assert_eq!(app.row_ids(Workspace::Fleet), vec!["bbb22222".to_string()]);
        assert_eq!(fleet_at(&app), "bbb22222");
    }

    /// `⏎` keeps the filter and hands the letters back; only `Esc` clears it.
    #[test]
    fn accepting_a_filter_keeps_it_and_returns_the_letters_to_being_commands() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "port");
        press(&mut app, KeyCode::Enter);
        assert!(!app.here().editing_filter);
        assert_eq!(app.here().filter.as_deref(), Some("port"));

        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::Stop("aaa11111".into())),
            "letters are commands again"
        );
    }

    #[test]
    fn backspace_edits_the_filter_rather_than_leaving_the_screen() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "port");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.here().filter.as_deref(), Some("por"));
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    // ---- sorting ----

    #[test]
    fn capital_s_cycles_the_sort_and_says_which_one_is_in_force() {
        let mut app = panel_with_agents();
        assert_eq!(app.here().sort, 0);
        press(&mut app, KeyCode::Char('S'));
        assert_eq!(app.here().sort, 1);
        assert!(
            format!("{:?}", app.transcript.last().unwrap()).contains("sorted by"),
            "{:?}",
            app.transcript.last()
        );
    }

    /// Re-sorting must not move the cursor onto a different row.
    #[test]
    fn re_sorting_keeps_the_cursor_on_the_same_item() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(fleet_at(&app), "bbb22222");
        press(&mut app, KeyCode::Char('S'));
        assert_eq!(fleet_at(&app), "bbb22222");
    }

    // ---- destructive verbs ----

    fn with_schedules() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.schedules = vec![data::ScheduleRow {
            name: "nightly-inbox".into(),
            gloss: "02:00 every day".into(),
            cron: "0 2 * * *".into(),
            timezone: "Asia/Manila".into(),
            next_ms: Some(1),
            last_ms: None,
            state: data::ScheduleState::Armed,
            history: vec![],
            prompt: "Triage the inbox.".into(),
            runs_as: "Claude Code".into(),
            policy: "overlap: skip".into(),
            recent: vec![],
        }];
        app.go(Workspace::Schedules);
        app
    }

    /// `x` deleting a webhook silently is one fat-fingered `Ctrl-K h x` away
    /// from losing a secret, so it asks first — and the question names the
    /// thing.
    #[test]
    fn x_asks_before_it_deletes_and_names_what_it_would_delete() {
        let mut app = with_schedules();
        assert_eq!(press(&mut app, KeyCode::Char('x')), None, "nothing yet");
        match &app.overlay {
            Overlay::Confirm { verb, what, .. } => {
                assert_eq!(verb, "delete");
                assert_eq!(what, "nightly-inbox");
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
    }

    #[test]
    fn anything_that_is_not_a_yes_cancels_the_deletion() {
        let mut app = with_schedules();
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.overlay, Overlay::None, "and it did not delete");

        press(&mut app, KeyCode::Char('x'));
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn y_confirms_the_deletion() {
        let mut app = with_schedules();
        press(&mut app, KeyCode::Char('x'));
        let action = press(&mut app, KeyCode::Char('y'));
        assert!(
            matches!(&action, Some(Action::Pending { verb, .. }) if verb.contains("nightly-inbox")),
            "{action:?}"
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    /// A run is not edited and an activity line is not deleted, so the keys
    /// fall through rather than offering a verb that cannot exist.
    #[test]
    fn edit_and_delete_do_nothing_on_the_screens_that_have_no_such_verb() {
        let mut app = panel_with_agents();
        assert_eq!(press(&mut app, KeyCode::Char('e')), None);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.overlay, Overlay::None, "and nothing asked to delete a run");

        let mut app = with_activity();
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Memory is forgotten, not deleted — the word on the screen is what it
    /// does to you, not what it does to a row.
    #[test]
    fn forgetting_a_memory_is_called_forgetting() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('x'));
        match &app.overlay {
            Overlay::Confirm { verb, .. } => assert_eq!(verb, "forget"),
            other => panic!("expected a confirmation, got {other:?}"),
        }
    }

    // ---- the local graph ----

    #[test]
    fn g_drills_from_the_memory_list_into_the_local_graph_of_the_selected_node() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);
        assert_eq!(app.graph.focus, "prefers-spec-first");
    }

    /// `⏎` re-centres on the highlighted neighbour and pushes the old focus;
    /// `Backspace` pops it.
    #[test]
    fn enter_re_centres_the_graph_and_backspace_walks_back() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.graph.focus, "linear-is-truth");
        assert_eq!(app.graph.trail, vec!["prefers-spec-first".to_string()]);

        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.graph.focus, "prefers-spec-first");
        assert!(app.graph.trail.is_empty());
        assert_eq!(app.workspace, Workspace::MemoryGraph, "still in the graph");
    }

    /// Walking back past the beginning leaves the graph rather than sitting
    /// there doing nothing.
    #[test]
    fn backspace_on_an_empty_visit_stack_leaves_the_graph() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.workspace, Workspace::Memory);
    }

    /// Coming back out of the graph should land on the node you were last
    /// looking at, not the one you left from.
    #[test]
    fn re_centring_moves_the_list_cursor_to_follow_the_eye() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Memory);
        assert_eq!(
            app.list(Workspace::Memory).selected.as_deref(),
            Some("linear-is-truth")
        );
    }

    #[test]
    fn h_toggles_between_one_hop_and_two() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.graph.hops, 1);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.graph.hops, 2);
    }

    // ---- activity ----

    fn with_activity() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.activity = vec![
            activity_item("newest", 100, true, None),
            activity_item("oldest", 1, true, Some((Workspace::Fleet, "aaa11111".into()))),
        ];
        app.go(Workspace::Activity);
        app
    }

    fn activity_item(
        id: &str,
        at_ms: i64,
        unread: bool,
        jump_to: Option<(Workspace, String)>,
    ) -> data::ActivityItem {
        data::ActivityItem {
            id: id.into(),
            at_ms,
            source: data::Source::Cron,
            text: format!("{id} happened"),
            unread,
            needs_you: false,
            jump_to,
        }
    }

    #[test]
    fn m_marks_one_item_read_and_capital_m_marks_them_all() {
        let mut app = with_activity();
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.unread(), 1);
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(app.unread(), 0);
    }

    #[test]
    fn u_hides_what_has_already_been_read() {
        let mut app = with_activity();
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('u'));
        assert!(app.unread_only);
        assert_eq!(app.row_ids(Workspace::Activity), vec!["oldest".to_string()]);
    }

    /// `⏎` jumps to the thing the event is about rather than showing a copy of
    /// it, which is what makes the feed a control surface.
    #[test]
    fn enter_on_an_activity_item_jumps_to_the_object_it_is_about() {
        let mut app = with_activity();
        app.agents = vec![running("aaa11111", "port the parser")];
        app.reconcile();
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.workspace, Workspace::Fleet);
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    /// `Ctrl-N` exists because an ending that arrived while you were away has
    /// to be reachable without hunting for it.
    #[test]
    fn ctrl_n_lands_on_the_oldest_thing_you_have_not_read() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.activity = vec![
            activity_item("newest", 100, true, None),
            activity_item("oldest", 1, true, None),
        ];
        app.reconcile();
        ctrl(&mut app, KeyCode::Char('n'));
        assert_eq!(app.workspace, Workspace::Activity);
        assert_eq!(
            app.list(Workspace::Activity).selected.as_deref(),
            Some("oldest")
        );
    }

    #[test]
    fn ctrl_n_with_nothing_unread_says_so_rather_than_pretending() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('n'));
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("nothing unread"));
    }

    // ---- tasks ----

    fn with_board() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.tasks = vec![jod_core::team::TeamTask {
            id: "port-the-parser".into(),
            title: "Port the parser to the new AST".into(),
            owner: None,
            status: "open".into(),
        }];
        app.go(Workspace::Tasks);
        app
    }

    /// The verb that makes the board worth a screen: a task becomes a run,
    /// seeded with what it is and how you would know it was done.
    #[test]
    fn d_turns_the_selected_task_into_an_agent_run() {
        let mut app = with_board();
        match press(&mut app, KeyCode::Char('d')) {
            Some(Action::Delegate(prompt)) => {
                assert!(prompt.contains("Port the parser to the new AST"), "{prompt}");
            }
            other => panic!("expected a delegation, got {other:?}"),
        }
    }

    #[test]
    fn a_delegated_task_carries_its_runnable_check_into_the_prompt() {
        let task = data::TaskRow {
            id: "migrate-store".into(),
            title: "Move the run transport into SQLite".into(),
            owner: None,
            state: data::TaskState::Open,
            run: None,
            age_ms: 0,
            what: "…".into(),
            check: "cargo test -p jod-core".into(),
            blocked_by: vec![],
            blocks: vec![],
            spec: Some("SPEC.md".into()),
            history: vec![],
        };
        let prompt = delegation_prompt(&task);
        assert!(prompt.contains("cargo test -p jod-core"), "{prompt}");
        assert!(prompt.contains("SPEC.md"), "{prompt}");
    }

    #[test]
    fn enter_on_the_tasks_screen_marks_it_done_as_the_board_always_has() {
        let mut app = with_board();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::FinishTask("port-the-parser".into()))
        );
    }

    #[test]
    fn n_on_the_tasks_screen_asks_for_a_title_and_puts_it_on_the_board() {
        let mut app = with_board();
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.overlay, Overlay::Prompt { .. }));
        type_line(&mut app, "write the docs");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AddTask("write the docs".into()))
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn cancelling_a_prompt_adds_nothing() {
        let mut app = with_board();
        press(&mut app, KeyCode::Char('n'));
        type_line(&mut app, "never mind");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    // ---- the palette reaches the same places ----

    /// A screen you can open one way and not the other is a screen half the
    /// users never find.
    #[test]
    fn every_workspace_is_reachable_by_a_slash_command_as_well_as_a_letter() {
        for (line, expected) in [
            ("/agents", Workspace::Fleet),
            ("/memory", Workspace::Memory),
            ("/schedules", Workspace::Schedules),
            ("/goals", Workspace::Goals),
            ("/hooks", Workspace::Hooks),
            ("/tasks", Workspace::Tasks),
            ("/activity", Workspace::Activity),
            ("/team", Workspace::Team),
        ] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            let slash = command::parse(line).unwrap_or_else(|| panic!("{line} did not parse"));
            apply_slash(&mut app, slash);
            assert_eq!(app.workspace, expected, "{line}");
        }
    }

    /// Typing the command for the screen you are already on takes you home,
    /// exactly as pressing `Ctrl-A` twice does.
    #[test]
    fn a_workspace_command_typed_twice_comes_back_to_chat() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Open(Workspace::Fleet));
        assert_eq!(app.workspace, Workspace::Fleet);
        apply_slash(&mut app, command::Slash::Open(Workspace::Fleet));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// `/memory prefers` should land you looking at the answer, not at the
    /// list with the query still to type.
    #[test]
    fn memory_with_a_query_opens_the_list_already_filtered() {
        let mut app = with_memory();
        app.go(Workspace::Chat);
        apply_slash(&mut app, command::Slash::Memory(Some("linear".into())));
        assert_eq!(app.workspace, Workspace::Memory);
        assert_eq!(
            app.row_ids(Workspace::Memory),
            vec!["linear-is-truth".to_string()]
        );
    }

    /// Naming a row is what makes `/schedule <name>` worth having beside
    /// `/schedules`.
    #[test]
    fn naming_a_row_lands_the_cursor_on_it() {
        let mut app = with_schedules();
        app.go(Workspace::Chat);
        apply_slash(
            &mut app,
            command::Slash::OpenNamed(Workspace::Schedules, "nightly".into()),
        );
        assert_eq!(app.workspace, Workspace::Schedules);
        assert_eq!(
            app.list(Workspace::Schedules).selected.as_deref(),
            Some("nightly-inbox")
        );
    }

    #[test]
    fn naming_a_row_that_is_not_there_says_so_rather_than_guessing() {
        let mut app = with_schedules();
        apply_slash(
            &mut app,
            command::Slash::OpenNamed(Workspace::Schedules, "nope".into()),
        );
        assert!(format!("{:?}", app.transcript.last().unwrap()).contains("no schedules called nope"));
    }

    /// A verb the store cannot carry out yet is named, not silently ignored:
    /// a key that appears to do nothing is worse than one that says what it is
    /// waiting for.
    #[test]
    fn a_verb_the_store_cannot_do_yet_says_what_it_is_waiting_for() {
        let mut app = with_schedules();
        match press(&mut app, KeyCode::Char('r')) {
            Some(Action::Pending { verb, needs }) => {
                assert!(verb.contains("nightly-inbox"), "{verb}");
                assert!(!needs.is_empty(), "it has to name the missing call");
            }
            other => panic!("expected a named to-do, got {other:?}"),
        }
    }

    /// The editor handoff is a decision the key handler makes and the loop
    /// carries out, so the decision is testable without a terminal.
    #[test]
    fn ctrl_f_asks_for_the_editor() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(ctrl(&mut app, KeyCode::Char('f')), Some(Action::Editor));
    }

    #[test]
    fn ctrl_k_e_is_the_discoverable_alias_for_the_editor() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        assert_eq!(press(&mut app, KeyCode::Char('e')), Some(Action::Editor));
    }

    /// Quitting is ahead of every layer, because a key that cannot always leave
    /// is a trap.
    #[test]
    fn ctrl_c_still_leaves_from_inside_an_overlay() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('k'));
        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit);
    }
}
