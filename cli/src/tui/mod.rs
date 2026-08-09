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
    // No harness name here: this line is frozen into the scrollback, so naming
    // the harness would leave a stale claim on screen the moment `/harness`
    // switches. The status bar is the one place that tracks it.
    app.push(Entry::Notice(
        "/help for commands · Enter send · Ctrl-A agents · Ctrl-G team · Ctrl-T thinking · Ctrl-C quit"
            .to_string(),
    ));

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

    // While the completion popup is up it owns Tab and the arrows, and Enter
    // finishes the word rather than sending a half-typed command.
    let suggestions = command::completions(&app.input);
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
                app.input.clear();
                app.cursor = 0;
                apply_slash(app, slash);
                return None;
            }
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

/// Carry out a slash command. Everything it touches is app state, so this
/// stays synchronous and testable; anything needing the service is handled by
/// setting a flag the loop picks up.
fn apply_slash(app: &mut App, slash: command::Slash) {
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
                "pick an id from the panel, then /resume <id>".into(),
            ));
        }
        Slash::Resume(id) => {
            app.resume = Resume::Session(id.clone());
            app.session = Some(id.clone());
            app.push(Entry::Notice(format!("continuing {id}")));
        }
        Slash::Agents => {
            app.pane = if app.pane == Pane::Agents { Pane::Chat } else { Pane::Agents };
        }
        Slash::Team => {
            app.pane = if app.pane == Pane::Team { Pane::Chat } else { Pane::Team };
        }
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
}

async fn spawn(jod: &Arc<Jod>, app: &App, opts: &Options, prompt: String) -> Result<String> {
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

    fn press(app: &mut App, code: KeyCode) -> Option<String> {
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
        // A second Enter runs it.
        press(&mut app, KeyCode::Enter);
        assert!(app.show_thinking);
    }

    #[test]
    fn an_explicit_model_still_reaches_the_harness() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Model(Some("haiku".into())));
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert!(app.status().contains("haiku"));
    }
}
