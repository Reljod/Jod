//! `jod-tui` — Jod's terminal client.
//!
//! Deliberately thin. Terminal setup, the select loop, and translating
//! crossterm keys into [`app::Key`] is all that lives here; every decision is
//! in `app.rs` and every service call is in `actions.rs`, so the parts worth
//! testing are testable without a terminal.

mod actions;
mod app;
mod ui;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use jod_core::{Jod, Team};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Key, Mode};

/// How often the loop wakes to redraw when nothing has happened.
const TICK: Duration = Duration::from_millis(120);

#[tokio::main]
async fn main() -> io::Result<()> {
    let jod = Jod::new();

    if !jod.tmux_available() {
        eprintln!("jod-tui needs tmux, and it is not on PATH.");
        eprintln!("Agents run inside tmux sessions so they survive this client closing.");
        return Ok(());
    }

    let available: Vec<String> = jod
        .harnesses()
        .into_iter()
        .filter(|h| h.available)
        .map(|h| h.label)
        .collect();
    if available.is_empty() {
        eprintln!("No agent harness found. Install one of: claude, opencode, agy.");
        return Ok(());
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let team_name = std::env::var("JOD_TEAM").ok();

    let mut terminal = setup()?;
    let result = run(&mut terminal, jod, cwd, team_name).await;
    restore(&mut terminal)?;

    // Report the harnesses after restoring, so the message is not eaten by the
    // alternate screen.
    println!("harnesses: {}", available.join(", "));
    result
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

async fn run(
    terminal: &mut Term,
    jod: std::sync::Arc<Jod>,
    cwd: PathBuf,
    team_name: Option<String>,
) -> io::Result<()> {
    let mut app = App {
        team_name: team_name.clone(),
        ..Default::default()
    };
    let mut events = jod.subscribe();

    loop {
        // Drain everything the service has produced since the last frame, so a
        // burst of output costs one redraw rather than one redraw per event.
        while let Ok(envelope) = events.try_recv() {
            app.ingest(envelope);
        }

        app.agents = jod.agents().await;
        if app.selected >= app.agents.len() {
            app.selected = app.agents.len().saturating_sub(1);
        }
        if let Some(name) = &team_name {
            let team = Team::new(name);
            app.members = team.members().await;
            app.tasks = team.tasks().await;
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;

        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports press *and* release; acting on both double-types.
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Ctrl-C always quits, including mid-compose.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        let Some(k) = translate(key, &app.mode) else {
            continue;
        };

        let action = app.on_key(k);
        if app.should_quit {
            return Ok(());
        }
        if let Some(status) = actions::perform(&jod, &app, action, &cwd).await {
            app.status = Some(status);
        }
    }
}

/// Map a crossterm key onto the app's small vocabulary.
///
/// In an editing mode every printable character is text, so a shortcut can
/// never fire from inside a prompt.
fn translate(key: KeyEvent, mode: &Mode) -> Option<Key> {
    let editing = *mode != Mode::Normal;
    match key.code {
        KeyCode::Char(c) => {
            if !editing && key.modifiers.contains(KeyModifiers::CONTROL) {
                return None;
            }
            Some(Key::Char(c))
        }
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn printable_keys_and_navigation_are_translated() {
        assert_eq!(translate(key(KeyCode::Char('r')), &Mode::Normal), Some(Key::Char('r')));
        assert_eq!(translate(key(KeyCode::Enter), &Mode::Normal), Some(Key::Enter));
        assert_eq!(translate(key(KeyCode::Esc), &Mode::Normal), Some(Key::Esc));
        assert_eq!(translate(key(KeyCode::Tab), &Mode::Normal), Some(Key::Tab));
        assert_eq!(translate(key(KeyCode::Backspace), &Mode::Normal), Some(Key::Backspace));
        assert_eq!(translate(key(KeyCode::Up), &Mode::Normal), Some(Key::Up));
        assert_eq!(translate(key(KeyCode::Down), &Mode::Normal), Some(Key::Down));
    }

    #[test]
    fn keys_with_no_meaning_are_ignored() {
        assert_eq!(translate(key(KeyCode::F(5)), &Mode::Normal), None);
        assert_eq!(translate(key(KeyCode::Insert), &Mode::Compose), None);
    }

    /// A modified key must not fire a shortcut in normal mode…
    #[test]
    fn control_chords_do_not_trigger_shortcuts() {
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(translate(ctrl_r, &Mode::Normal), None);
    }

    /// …but while typing, a character is a character.
    #[test]
    fn while_composing_every_character_is_text() {
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(translate(ctrl_r, &Mode::Compose), Some(Key::Char('r')));
        let shift_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(translate(shift_a, &Mode::Spawn), Some(Key::Char('A')));
    }
}
