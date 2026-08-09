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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("jod-{sanitized}")
}

pub fn locate() -> Option<PathBuf> {
    crate::discovery::find_binary(
        "JOD_TMUX_BIN",
        &["tmux"],
        &[
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/usr/bin/tmux",
        ],
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
    let (ok, out) = run(&["new-session", "-d", "-s", name, "-c", &cwd, "bash", &script]).await?;
    if !ok {
        return Err(JodError::Tmux(out));
    }
    protect_attached_clients(name).await;
    Ok(())
}

/// Stop this session's destruction from taking a user's terminal with it.
///
/// tmux's `detach-on-destroy` defaults to `on`, which makes an attached client
/// *exit* when its session is destroyed rather than fall back to another
/// session. That is fatal in a common setup: oh-my-zsh's tmux plugin with
/// `ZSH_TMUX_AUTOSTART` sets `ZSH_TMUX_AUTOQUIT`, which runs `exit` the moment
/// its tmux client returns — so killing a Jod agent while watching it would
/// close the user's terminal window.
///
/// Set per-session, never globally: this is Jod's session to configure, and the
/// user's own sessions are none of our business. Best-effort — an older tmux
/// without the option should not fail a spawn.
async fn protect_attached_clients(name: &str) {
    let _ = run(&["set-option", "-t", name, "detach-on-destroy", "off"]).await;
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

/// The command a human types to watch this agent from outside tmux.
pub fn attach_command(name: &str) -> String {
    format!("tmux attach -t {name}")
}

/// The command to use from *inside* an existing tmux session.
///
/// `tmux attach` refuses to nest, and most people running Jod already live in
/// tmux — so offering only `attach` hands them a command that errors.
pub fn switch_command(name: &str) -> String {
    format!("tmux switch-client -t {name}")
}

/// One command that is correct whether or not the caller is inside tmux.
///
/// Used when Jod drives a terminal itself, where it cannot know what the new
/// window's shell will do — a login shell may auto-start tmux before this runs.
pub fn watch_command(name: &str) -> String {
    format!(
        "if [ -n \"$TMUX\" ]; then {}; else {}; fi",
        switch_command(name),
        attach_command(name)
    )
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

    #[test]
    fn the_switch_command_is_offered_because_attach_refuses_to_nest() {
        assert_eq!(switch_command("jod-x"), "tmux switch-client -t jod-x");
    }

    #[test]
    fn the_watch_command_picks_the_right_one_at_runtime() {
        let cmd = watch_command("jod-x");
        assert!(
            cmd.contains("$TMUX"),
            "must branch on being inside tmux: {cmd}"
        );
        assert!(cmd.contains("switch-client -t jod-x"));
        assert!(cmd.contains("attach -t jod-x"));
    }
}
