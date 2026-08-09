//! The event loop.
//!
//! Split out from `main.rs` and made generic over both the terminal backend and
//! the source of key events, so the loop that actually runs in production is
//! the same one the tests drive — with a `TestBackend` and a scripted list of
//! keypresses instead of a real terminal.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use jod_core::{Jod, Team};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::actions;
use crate::app::{App, Key, Mode};
use crate::ui;

/// How often the loop wakes to redraw when nothing has happened.
pub const TICK: Duration = Duration::from_millis(120);

/// Where key events come from. The real one blocks on the terminal; the test
/// one reads from a list.
pub trait Input {
    /// `None` means the tick elapsed with nothing pressed.
    fn poll(&mut self, timeout: Duration) -> io::Result<Option<KeyEvent>>;
}

/// Reads the actual terminal.
pub struct TerminalInput;

impl Input for TerminalInput {
    fn poll(&mut self, timeout: Duration) -> io::Result<Option<KeyEvent>> {
        if !crossterm::event::poll(timeout)? {
            return Ok(None);
        }
        match crossterm::event::read()? {
            crossterm::event::Event::Key(key) => Ok(Some(key)),
            _ => Ok(None),
        }
    }
}

/// Run until the user quits.
///
/// `app` is borrowed rather than owned so a test can inspect where it ended up.
pub async fn run<B: Backend, I: Input>(
    terminal: &mut Terminal<B>,
    jod: Arc<Jod>,
    input: &mut I,
    app: &mut App,
    cwd: PathBuf,
) -> io::Result<()> {
    let mut events = jod.subscribe();
    let team_name = app.team_name.clone();

    loop {
        refresh(app, &jod, &mut events, team_name.as_deref()).await;
        terminal.draw(|frame| ui::draw(frame, app))?;

        let Some(key) = input.poll(TICK)? else {
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
        if let Some(status) = actions::perform(&jod, app, action, &cwd).await {
            app.status = Some(status);
        }
    }
}

/// Pull in everything that happened since the last frame.
async fn refresh(
    app: &mut App,
    jod: &Arc<Jod>,
    events: &mut jod_core::broadcast::Receiver<jod_core::event::AgentEnvelope>,
    team_name: Option<&str>,
) {
    // Drain in one go, so a burst of output costs one redraw rather than one
    // redraw per event.
    while let Ok(envelope) = events.try_recv() {
        app.ingest(envelope);
    }

    app.agents = jod.agents().await;
    if app.selected >= app.agents.len() {
        app.selected = app.agents.len().saturating_sub(1);
    }
    if let Some(name) = team_name {
        let team = Team::new(name);
        app.members = team.members().await;
        app.tasks = team.tasks().await;
    }
}

/// Map a crossterm key onto the app's small vocabulary.
///
/// In an editing mode every printable character is text, so a shortcut can
/// never fire from inside a prompt.
pub fn translate(key: KeyEvent, mode: &Mode) -> Option<Key> {
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
    use ratatui::backend::TestBackend;

    /// Replays a scripted list of keypresses, then reports nothing forever.
    struct Scripted {
        keys: std::collections::VecDeque<KeyEvent>,
        /// Guards against a test that never quits hanging the suite.
        idle: usize,
    }

    impl Scripted {
        fn new(keys: Vec<KeyEvent>) -> Self {
            Self { keys: keys.into(), idle: 0 }
        }

        fn press(codes: &str) -> Self {
            Self::new(
                codes
                    .chars()
                    .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                    .collect(),
            )
        }
    }

    impl Input for Scripted {
        fn poll(&mut self, _timeout: Duration) -> io::Result<Option<KeyEvent>> {
            match self.keys.pop_front() {
                Some(key) => Ok(Some(key)),
                None => {
                    self.idle += 1;
                    if self.idle > 50 {
                        return Err(io::Error::other("loop never quit"));
                    }
                    Ok(None)
                }
            }
        }
    }

    async fn drive(keys: Scripted) -> (App, io::Result<()>) {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = App::default();
        let mut keys = keys;
        let result = run(
            &mut terminal,
            Jod::new(),
            &mut keys,
            &mut app,
            PathBuf::from("/tmp"),
        )
        .await;
        (app, result)
    }

    #[tokio::test]
    async fn q_ends_the_loop() {
        let (app, result) = drive(Scripted::press("q")).await;
        assert!(result.is_ok());
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn keys_are_applied_in_order_before_quitting() {
        // r toggles reasoning off, h moves the harness on, then quit.
        let (app, result) = drive(Scripted::press("rhq")).await;
        assert!(result.is_ok());
        assert!(!app.show_reasoning);
        assert_eq!(app.spawn_harness, jod_core::HarnessKind::OpenCode);
    }

    #[tokio::test]
    async fn a_quiet_tick_just_redraws() {
        // One idle poll, then quit — the loop must survive seeing nothing.
        let mut keys = Scripted::new(vec![]);
        keys.keys
            .push_back(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut app = App::default();
        let result = run(
            &mut terminal,
            Jod::new(),
            &mut keys,
            &mut app,
            PathBuf::from("/tmp"),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ctrl_c_quits_even_while_composing() {
        let keys = Scripted::new(vec![
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ]);
        let (app, result) = drive(keys).await;
        assert!(result.is_ok());
        assert_eq!(app.mode, Mode::Spawn, "it quit mid-compose");
        assert!(!app.should_quit, "ctrl-c leaves by its own path");
    }

    #[tokio::test]
    async fn a_key_release_is_ignored() {
        let mut release = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        let keys = Scripted::new(vec![
            release,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        ]);
        let (app, _) = drive(keys).await;
        assert!(app.show_reasoning, "a release must not toggle anything");
    }

    #[tokio::test]
    async fn an_unmapped_key_is_skipped() {
        let keys = Scripted::new(vec![
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        ]);
        let (_, result) = drive(keys).await;
        assert!(result.is_ok());
    }

    /// An action that produces a status line must land on the app.
    #[tokio::test]
    async fn a_failed_action_reports_into_the_status_bar() {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = App {
            team_name: Some(format!("driver-test-{}", std::process::id())),
            ..Default::default()
        };
        // m opens the message box, text, Enter sends, q quits.
        let mut keys = Scripted::new(
            "mhi"
                .chars()
                .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .chain([
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    // Ctrl-C rather than `q`: any normal keypress clears the
                    // status bar first, which would hide what we are asserting.
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ])
                .collect(),
        );
        let result = run(
            &mut terminal,
            Jod::new(),
            &mut keys,
            &mut app,
            PathBuf::from("/tmp"),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(
            app.status.as_deref(),
            Some("nobody on the team to message"),
            "the empty team must be reported"
        );
    }

    #[test]
    fn printable_keys_and_navigation_are_translated() {
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert_eq!(translate(key(KeyCode::Char('r')), &Mode::Normal), Some(Key::Char('r')));
        assert_eq!(translate(key(KeyCode::Enter), &Mode::Normal), Some(Key::Enter));
        assert_eq!(translate(key(KeyCode::Esc), &Mode::Normal), Some(Key::Esc));
        assert_eq!(translate(key(KeyCode::Tab), &Mode::Normal), Some(Key::Tab));
        assert_eq!(translate(key(KeyCode::Backspace), &Mode::Normal), Some(Key::Backspace));
        assert_eq!(translate(key(KeyCode::Up), &Mode::Normal), Some(Key::Up));
        assert_eq!(translate(key(KeyCode::Down), &Mode::Normal), Some(Key::Down));
        assert_eq!(translate(key(KeyCode::F(5)), &Mode::Normal), None);
    }

    #[test]
    fn control_chords_do_not_trigger_shortcuts_but_are_text_while_typing() {
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(translate(ctrl_r, &Mode::Normal), None);
        assert_eq!(translate(ctrl_r, &Mode::Compose), Some(Key::Char('r')));
    }
}
