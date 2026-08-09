//! Taking the terminal over, and giving it back.
//!
//! Separated from the loop for one reason: none of it can be tested in
//! process. `enable_raw_mode` acts on the real controlling TTY, so a test that
//! *succeeded* here would put the developer's own shell into raw mode with no
//! echo — the exact failure this module exists to prevent. A test that faked it
//! would assert nothing about the thing that goes wrong.
//!
//! So this file is excluded from the coverage floor by name (see
//! `.github/workflows/rust.yml`) and kept deliberately small: every line here
//! is one someone has to verify by running `jod tui` and pressing Ctrl-C. The
//! logic worth testing lives in `super::event_loop`, which is generic over the
//! backend precisely so it can be driven without any of this.

use std::io;

use anyhow::{Context, Result};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::sync::Arc;

use jod_core::Jod;

use super::Options;

pub async fn run(jod: Arc<Jod>, opts: Options) -> Result<()> {
    let mut terminal = enter().context("taking over the terminal")?;
    let mut keys = EventStream::new();
    let result = super::event_loop(&mut terminal, &mut keys, jod, opts).await;
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

/// Best-effort, and safe to call more than once: `run` calls it on every exit
/// path and the panic hook may call it again on the way down.
pub(super) fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}
