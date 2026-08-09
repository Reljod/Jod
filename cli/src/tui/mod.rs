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
    app.push(Entry::Notice(format!(
        "{} · type to talk · Enter send · Ctrl-A agents · Ctrl-T thinking · Ctrl-C quit",
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
        // `!ctrl` matters: an unbound chord falls through the block above, and
        // without this guard Ctrl-Z would type a bare "z" into the prompt. The
        // bindings that *are* wanted with Control all returned already.
        KeyCode::Char(c) if !ctrl => app.insert(c),
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

    /// `on_key` is the whole keybinding contract, and it is pure: an `App`, a
    /// keypress and a viewport height in, a prompt or nothing out. The event
    /// loop around it needs a real terminal; this does not.
    fn app() -> App {
        App::new(HarnessKind::ClaudeCode, None, Resume::Fresh)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            on_key(app, press(KeyCode::Char(c)), 20);
        }
    }

    fn last_notice(app: &App) -> Option<&str> {
        app.transcript.iter().rev().find_map(|e| match e {
            Entry::Notice(text) => Some(text.as_str()),
            _ => None,
        })
    }

    // --- leaving ---------------------------------------------------------

    #[test]
    fn ctrl_c_quits_when_nothing_is_running() {
        let mut a = app();
        assert_eq!(on_key(&mut a, ctrl('c'), 20), None);
        assert!(a.should_quit);
    }

    /// Walking away from a running agent by accident is the expensive mistake
    /// here, so the first press only warns.
    #[test]
    fn quitting_while_an_agent_is_running_asks_once_first() {
        let mut a = app();
        a.busy = true;

        on_key(&mut a, ctrl('c'), 20);
        assert!(!a.should_quit, "the first press must not leave");
        assert!(a.confirm_quit);
        assert!(last_notice(&a).unwrap().contains("still running"));

        on_key(&mut a, ctrl('c'), 20);
        assert!(a.should_quit, "the second press goes anyway");
    }

    #[test]
    fn ctrl_d_leaves_the_same_way_as_ctrl_c() {
        let mut a = app();
        a.busy = true;
        on_key(&mut a, ctrl('d'), 20);
        assert!(a.confirm_quit && !a.should_quit);
        on_key(&mut a, ctrl('d'), 20);
        assert!(a.should_quit);
    }

    /// Having second thoughts is the common case: any other key stands the
    /// warning down, so a later stray Ctrl-C does not leave immediately.
    #[test]
    fn typing_anything_else_cancels_a_pending_quit() {
        let mut a = app();
        a.busy = true;
        on_key(&mut a, ctrl('c'), 20);
        assert!(a.confirm_quit);

        on_key(&mut a, press(KeyCode::Char('x')), 20);
        assert!(!a.confirm_quit);

        on_key(&mut a, ctrl('c'), 20);
        assert!(!a.should_quit, "the warning must start over");
    }

    // --- the control bindings --------------------------------------------

    #[test]
    fn ctrl_a_toggles_the_agents_panel() {
        let mut a = app();
        let start = a.pane;

        on_key(&mut a, ctrl('a'), 20);
        assert_eq!(a.pane, Pane::Agents);
        on_key(&mut a, ctrl('a'), 20);
        assert_eq!(a.pane, start);
    }

    #[test]
    fn ctrl_t_toggles_thinking_and_says_which_way_it_went() {
        let mut a = app();
        let before = a.show_thinking;

        on_key(&mut a, ctrl('t'), 20);
        assert_ne!(a.show_thinking, before);
        assert!(last_notice(&a).unwrap().contains("thinking"));

        on_key(&mut a, ctrl('t'), 20);
        assert_eq!(a.show_thinking, before);
    }

    #[test]
    fn ctrl_l_clears_the_transcript_and_returns_to_the_bottom() {
        let mut a = app();
        a.push(Entry::Notice("one".into()));
        a.push(Entry::Notice("two".into()));
        a.scroll_up(1, a.transcript.len());

        on_key(&mut a, ctrl('l'), 20);

        assert!(a.transcript.is_empty());
        assert!(a.following(), "clearing must not leave the view detached");
    }

    #[test]
    fn ctrl_u_clears_the_line_being_typed() {
        let mut a = app();
        type_in(&mut a, "half a thought");

        on_key(&mut a, ctrl('u'), 20);

        assert_eq!(a.input, "");
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn ctrl_w_deletes_only_the_last_word() {
        let mut a = app();
        type_in(&mut a, "two words");

        on_key(&mut a, ctrl('w'), 20);

        assert_eq!(a.input, "two ");
    }

    /// Ctrl-A is taken by the agents panel, so start-of-line is Ctrl-Home.
    #[test]
    fn ctrl_home_and_ctrl_end_move_to_the_ends_of_the_line() {
        let mut a = app();
        type_in(&mut a, "abc");

        on_key(&mut a, KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL), 20);
        assert_eq!(a.cursor, 0);

        on_key(&mut a, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), 20);
        assert_eq!(a.cursor, 3);

        on_key(&mut a, KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL), 20);
        on_key(&mut a, ctrl('e'), 20);
        assert_eq!(a.cursor, 3, "ctrl-e is the readline spelling of End");
    }

    /// Regression: unbound chords fell through the Control block into the
    /// plain-key match, so Ctrl-Z typed a "z", Ctrl-S an "s", and so on — a
    /// stray suspend attempt silently corrupted the prompt.
    #[test]
    fn an_unbound_control_key_is_ignored_rather_than_typed() {
        for c in ['z', 's', 'r', 'b', 'k'] {
            let mut a = app();
            assert_eq!(on_key(&mut a, ctrl(c), 20), None);
            assert_eq!(a.input, "", "ctrl-{c} must not insert a '{c}'");
        }
    }

    /// The chords that are bound must still work after that guard.
    #[test]
    fn the_bound_control_keys_still_do_their_jobs() {
        let mut a = app();
        type_in(&mut a, "text");
        on_key(&mut a, ctrl('u'), 20);
        assert_eq!(a.input, "");

        on_key(&mut a, ctrl('a'), 20);
        assert_eq!(a.pane, Pane::Agents);
    }

    // --- sending ---------------------------------------------------------

    #[test]
    fn enter_sends_what_was_typed_and_empties_the_line() {
        let mut a = app();
        type_in(&mut a, "do the thing");

        assert_eq!(
            on_key(&mut a, press(KeyCode::Enter), 20),
            Some("do the thing".into())
        );
        assert_eq!(a.input, "");
    }

    #[test]
    fn enter_on_an_empty_line_sends_nothing() {
        let mut a = app();
        assert_eq!(on_key(&mut a, press(KeyCode::Enter), 20), None);
    }

    /// One turn at a time: a second prompt would race the first agent's output.
    #[test]
    fn enter_is_refused_while_an_agent_is_still_working() {
        let mut a = app();
        a.busy = true;
        type_in(&mut a, "impatient");

        assert_eq!(on_key(&mut a, press(KeyCode::Enter), 20), None);
        assert!(last_notice(&a).unwrap().contains("still working"));
        assert_eq!(a.input, "impatient", "the typed line must survive");
    }

    // --- editing ---------------------------------------------------------

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut a = app();
        type_in(&mut a, "ac");
        on_key(&mut a, press(KeyCode::Left), 20);
        on_key(&mut a, press(KeyCode::Char('b')), 20);
        assert_eq!(a.input, "abc");
    }

    #[test]
    fn backspace_and_delete_remove_on_either_side_of_the_cursor() {
        let mut a = app();
        type_in(&mut a, "abc");

        on_key(&mut a, press(KeyCode::Backspace), 20);
        assert_eq!(a.input, "ab");

        on_key(&mut a, press(KeyCode::Home), 20);
        on_key(&mut a, press(KeyCode::Delete), 20);
        assert_eq!(a.input, "b");
    }

    #[test]
    fn home_and_end_reach_both_ends_without_a_modifier_too() {
        let mut a = app();
        type_in(&mut a, "abcd");

        on_key(&mut a, press(KeyCode::Home), 20);
        assert_eq!(a.cursor, 0);
        on_key(&mut a, press(KeyCode::End), 20);
        assert_eq!(a.cursor, 4);
    }

    #[test]
    fn the_arrows_move_the_cursor_and_stop_at_the_ends() {
        let mut a = app();
        type_in(&mut a, "ab");

        on_key(&mut a, press(KeyCode::Right), 20);
        assert_eq!(a.cursor, 2, "right at the end must not run off");

        for _ in 0..5 {
            on_key(&mut a, press(KeyCode::Left), 20);
        }
        assert_eq!(a.cursor, 0, "left at the start must not underflow");
    }

    // --- scrolling -------------------------------------------------------

    fn with_transcript(lines: usize) -> App {
        let mut a = app();
        for i in 0..lines {
            a.push(Entry::Notice(format!("line {i}")));
        }
        a
    }

    #[test]
    fn up_and_down_scroll_one_line_at_a_time() {
        let mut a = with_transcript(50);

        on_key(&mut a, press(KeyCode::Up), 20);
        assert!(!a.following());
        let scrolled = a.scroll;

        on_key(&mut a, press(KeyCode::Down), 20);
        assert_ne!(a.scroll, scrolled);
    }

    #[test]
    fn page_up_and_page_down_move_by_the_visible_height() {
        let mut a = with_transcript(200);

        on_key(&mut a, press(KeyCode::PageUp), 30);
        let by_page = a.scroll;

        let mut b = with_transcript(200);
        on_key(&mut b, press(KeyCode::Up), 30);
        assert_ne!(
            by_page, b.scroll,
            "a page must move further than a single line"
        );
    }

    /// The viewport is whatever the last draw measured, and a zero-height
    /// terminal would otherwise make PageUp a no-op.
    #[test]
    fn paging_still_moves_when_the_viewport_measures_zero() {
        let mut a = with_transcript(50);
        on_key(&mut a, press(KeyCode::PageUp), 0);
        assert!(!a.following(), "page up must move by at least one line");
    }

    #[test]
    fn escape_jumps_back_to_the_newest_output() {
        let mut a = with_transcript(50);
        on_key(&mut a, press(KeyCode::PageUp), 20);
        assert!(!a.following());

        on_key(&mut a, press(KeyCode::Esc), 20);
        assert!(a.following());
    }

    #[test]
    fn a_key_with_no_binding_changes_nothing() {
        let mut a = app();
        type_in(&mut a, "abc");
        let before = (a.input.clone(), a.cursor, a.scroll);

        assert_eq!(on_key(&mut a, press(KeyCode::F(5)), 20), None);
        assert_eq!((a.input.clone(), a.cursor, a.scroll), before);
    }

    // --- the agents panel ------------------------------------------------

    #[tokio::test]
    async fn a_service_with_no_delegations_lists_none() {
        let jod = Jod::new();
        assert!(list_agents(&jod).await.is_empty());
    }
}
