//! Every agent gets its own tmux session.
//!
//! That is the whole observability story: `tmux attach -t <session>` to watch a
//! live agent, `tmux kill-session -t <session>` to stop one. Jod does not need
//! to reimplement a terminal, and an agent outlives the desktop app.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{JodError, Result};

/// tmux session names may not contain `.` or `:`; tmux treats them as
/// window/pane separators.
pub fn session_name(agent_id: &str) -> String {
    let sanitized: String = agent_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("jod-{sanitized}")
}

pub fn locate() -> Option<PathBuf> {
    crate::discovery::find_binary(
        "JOD_TMUX_BIN",
        &["tmux"],
        &["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"],
    )
}

fn tmux_bin() -> Result<PathBuf> {
    locate().ok_or(JodError::TmuxNotFound)
}

async fn run(args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new(tmux_bin()?)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok((out.status.success(), combined.trim().to_string()))
}

/// Start `script` detached in a fresh session. Fails if the name is taken.
pub async fn new_session(name: &str, cwd: &Path, script: &Path) -> Result<()> {
    if has_session(name).await {
        return Err(JodError::Tmux(format!("session `{name}` already exists")));
    }
    let cwd = cwd.to_string_lossy().to_string();
    let script = script.to_string_lossy().to_string();
    let (ok, out) = run(&[
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &cwd,
        "bash",
        &script,
    ])
    .await?;
    if !ok {
        return Err(JodError::Tmux(out));
    }
    Ok(())
}

pub async fn has_session(name: &str) -> bool {
    matches!(run(&["has-session", "-t", name]).await, Ok((true, _)))
}

pub async fn kill_session(name: &str) -> Result<()> {
    let (ok, out) = run(&["kill-session", "-t", name]).await?;
    // Killing an already-dead session is the outcome the caller wanted.
    if !ok && !out.contains("can't find session") && !out.contains("no server running") {
        return Err(JodError::Tmux(out));
    }
    Ok(())
}

/// Names of every live Jod-owned session.
pub async fn list_sessions() -> Vec<String> {
    match run(&["list-sessions", "-F", "#{session_name}"]).await {
        Ok((true, out)) => out
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("jod-"))
            .map(str::to_string)
            .collect(),
        _ => vec![],
    }
}

/// The command a human would type to watch this agent.
pub fn attach_command(name: &str) -> String {
    format!("tmux attach -t {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_prefixed_and_tmux_safe() {
        let n = session_name("a1b2");
        assert_eq!(n, "jod-a1b2");
        assert!(!n.contains('.') && !n.contains(':'));
    }

    #[test]
    fn dots_and_colons_are_replaced_not_kept() {
        assert_eq!(session_name("a.b:c"), "jod-a-b-c");
    }

    #[test]
    fn uuid_style_ids_keep_their_dashes() {
        assert_eq!(
            session_name("3f2a1b4c-0000-4444-8888-aabbccddeeff"),
            "jod-3f2a1b4c-0000-4444-8888-aabbccddeeff"
        );
    }

    #[test]
    fn the_attach_command_targets_the_session() {
        assert_eq!(attach_command("jod-x"), "tmux attach -t jod-x");
    }
}
