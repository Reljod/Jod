//! `jod-tui` — Jod's terminal client.
//!
//! Deliberately thin: this file owns terminal setup and teardown and nothing
//! else. The event loop lives in `driver.rs`, generic over the backend and the
//! input source so it can be tested; every decision is in `app.rs`; every
//! service call is in `actions.rs`.

mod actions;
mod app;
mod driver;
mod ui;

use std::io::{self, Stdout};
use std::path::PathBuf;

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use jod_core::Jod;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::driver::TerminalInput;

type Term = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    let jod = Jod::new();

    if let Err(why) = preflight(&jod) {
        eprintln!("{why}");
        return Ok(());
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = App {
        team_name: std::env::var("JOD_TEAM").ok(),
        ..Default::default()
    };

    let mut terminal = setup()?;
    let result = driver::run(&mut terminal, jod, &mut TerminalInput, &mut app, cwd).await;
    // Restore the terminal even if the loop failed, or the shell is left in raw
    // mode with no cursor.
    let restored = restore(&mut terminal);
    result.and(restored)
}

/// Refuse to start rather than opening a UI that cannot do anything.
fn preflight(jod: &Jod) -> Result<Vec<String>, String> {
    if !jod.tmux_available() {
        return Err("jod-tui needs tmux, and it is not on PATH.\n\
                    Agents run inside tmux sessions so they survive this client closing."
            .to_string());
    }
    let available: Vec<String> = jod
        .harnesses()
        .into_iter()
        .filter(|h| h.available)
        .map(|h| h.label)
        .collect();
    if available.is_empty() {
        return Err("No agent harness found. Install one of: claude, opencode, agy.".to_string());
    }
    Ok(available)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether this passes depends on the machine, but it must never panic and
    /// must always explain itself when it refuses.
    #[tokio::test]
    async fn preflight_either_lists_harnesses_or_explains_the_refusal() {
        let jod = Jod::new();
        match preflight(&jod) {
            Ok(found) => assert!(!found.is_empty(), "Ok means at least one harness"),
            Err(why) => {
                assert!(!why.is_empty());
                assert!(
                    why.contains("tmux") || why.contains("harness"),
                    "a refusal must say which dependency is missing, got: {why}"
                );
            }
        }
    }
}
