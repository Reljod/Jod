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

    // --- talking to the tmux binary -------------------------------------
    //
    // These drive a fake `tmux` (see `testsupport`) rather than the real one.
    // Using the developer's tmux would be actively dangerous: `list_sessions`
    // and `kill_session` operate on `jod-*` names, which are exactly the
    // sessions belonging to their live agents.

    use crate::testsupport::{EnvGuard, FakeTmux, TempDir};

    /// Point discovery at `fake`, with `PATH`/`HOME` neutered so a real tmux on
    /// the machine cannot be picked up instead.
    fn using(fake: &FakeTmux) -> EnvGuard {
        let mut env = EnvGuard::new();
        env.isolate_discovery();
        env.set("JOD_TMUX_BIN", fake.bin());
        env
    }

    #[test]
    fn an_explicit_override_wins_over_everything_else() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        assert_eq!(locate(), Some(fake.bin()));
    }

    #[test]
    fn an_unusable_override_falls_through_to_normal_discovery() {
        let mut env = EnvGuard::new();
        env.set("JOD_TMUX_BIN", "/definitely/not/a/tmux");
        // Whatever it resolves to, a non-executable override must never be
        // returned as if it were a working tmux.
        assert_ne!(locate(), Some(PathBuf::from("/definitely/not/a/tmux")));
    }

    // Note: `tmux_bin`'s `JodError::TmuxNotFound` arm is not unit-tested. It
    // needs *every* lookup in `locate()` to fail, and the well-known list holds
    // absolute paths (`/usr/bin/tmux`, …) that no environment variable can hide
    // on a machine where tmux is installed. Faking it would mean asserting
    // something the test cannot actually establish.

    #[tokio::test]
    async fn has_session_answers_from_the_live_session_list() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        fake.seed_sessions(&["jod-alive"]);

        assert!(has_session("jod-alive").await);
        assert!(!has_session("jod-never-existed").await);
    }

    #[tokio::test]
    async fn a_new_session_runs_the_script_in_the_requested_directory() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        let work = TempDir::new("cwd");
        let script = work.join("run.sh");

        new_session("jod-new", work.path(), &script).await.expect("session starts");

        assert_eq!(fake.live_sessions(), vec!["jod-new".to_string()]);
        let started = fake
            .calls()
            .into_iter()
            .find(|c| c.starts_with("new-session"))
            .expect("new-session was invoked");
        assert!(started.contains("-d"), "must start detached: {started}");
        assert!(started.contains(&work.path().to_string_lossy().to_string()));
        assert!(started.contains(&script.to_string_lossy().to_string()));
    }

    /// Regression: killing an agent used to close the terminal of whoever was
    /// attached, because tmux's `detach-on-destroy` defaults to `on`.
    #[tokio::test]
    async fn a_new_session_is_told_not_to_take_its_watchers_down_with_it() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        let work = TempDir::new("cwd");

        new_session("jod-protected", work.path(), &work.join("run.sh"))
            .await
            .expect("session starts");

        assert!(
            fake.calls().iter().any(|c| c.contains("detach-on-destroy off")),
            "the session must opt out of detach-on-destroy: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn reusing_a_live_session_name_is_refused_before_anything_is_started() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        fake.seed_sessions(&["jod-taken"]);
        let work = TempDir::new("cwd");

        let err = new_session("jod-taken", work.path(), &work.join("run.sh"))
            .await
            .expect_err("a duplicate name must not silently attach");

        assert!(matches!(&err, JodError::Tmux(m) if m.contains("already exists")), "{err:?}");
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("new-session")),
            "nothing should have been started"
        );
    }

    #[tokio::test]
    async fn a_refusal_from_tmux_surfaces_its_own_message() {
        let fake = FakeTmux::broken();
        let _env = using(&fake);
        let work = TempDir::new("cwd");

        let err = new_session("jod-x", work.path(), &work.join("run.sh"))
            .await
            .expect_err("a failing tmux must fail the spawn");

        assert!(matches!(&err, JodError::Tmux(m) if m.contains("refusing")), "{err:?}");
    }

    #[tokio::test]
    async fn killing_a_session_removes_it() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        fake.seed_sessions(&["jod-a", "jod-b"]);

        kill_session("jod-a").await.expect("kill succeeds");

        assert_eq!(fake.live_sessions(), vec!["jod-b".to_string()]);
    }

    #[tokio::test]
    async fn killing_an_already_dead_session_is_the_outcome_the_caller_wanted() {
        let fake = FakeTmux::new();
        let _env = using(&fake);

        kill_session("jod-never-existed")
            .await
            .expect("`can't find session` is success, not failure");
    }

    #[tokio::test]
    async fn killing_a_session_with_no_server_at_all_is_also_success() {
        let fake = FakeTmux::with_kill_failure(Some("no server running on /tmp/tmux-1000/default"));
        let _env = using(&fake);

        kill_session("jod-x").await.expect("a dead server means nothing to kill");
    }

    #[tokio::test]
    async fn any_other_kill_failure_is_reported_rather_than_swallowed() {
        let fake = FakeTmux::with_kill_failure(Some("permission denied"));
        let _env = using(&fake);

        let err = kill_session("jod-x").await.expect_err("a real fault must surface");
        assert!(matches!(&err, JodError::Tmux(m) if m.contains("permission denied")), "{err:?}");
    }

    #[tokio::test]
    async fn listing_returns_only_jod_owned_sessions() {
        let fake = FakeTmux::new();
        let _env = using(&fake);
        fake.seed_sessions(&["jod-one", "my-editor", "jod-two", "0"]);

        let sessions = list_sessions().await;

        assert_eq!(sessions, vec!["jod-one".to_string(), "jod-two".to_string()]);
    }

    #[tokio::test]
    async fn listing_is_empty_rather_than_an_error_when_tmux_fails() {
        let fake = FakeTmux::broken();
        let _env = using(&fake);
        assert!(list_sessions().await.is_empty());
    }
}
