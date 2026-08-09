//! Where Jod keeps its runtime state: `~/.jod`, overridable with `JOD_HOME`.
//!
//! Everything is a plain file on purpose. An agent's transcript stays readable
//! with `cat` after the desktop app is closed, and the same layout works
//! unchanged on a VPS.

use std::path::PathBuf;

pub fn jod_home() -> PathBuf {
    if let Ok(explicit) = std::env::var("JOD_HOME") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".jod")
}

pub fn runs_dir() -> PathBuf {
    jod_home().join("runs")
}

pub fn run_dir(agent_id: &str) -> PathBuf {
    runs_dir().join(agent_id)
}

/// The harness's raw JSONL, tailed by the runner and readable afterwards.
pub fn stream_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("stream.jsonl")
}

/// The prompt, written to disk so it never has to survive shell quoting.
pub fn prompt_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("prompt.txt")
}

/// The generated launcher that tmux executes.
pub fn script_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("run.sh")
}

/// The run's metadata, so a restarted app can rebuild its agent list.
pub fn meta_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("agent.json")
}

// ---- teams -------------------------------------------------------------
//
// A team is a directory of append-only logs, so `cat ~/.jod/teams/<name>/…`
// answers "who is on it, what did they say, who is doing what" with no tooling.

pub fn teams_dir() -> PathBuf {
    jod_home().join("teams")
}

pub fn team_dir(team: &str) -> PathBuf {
    teams_dir().join(sanitize(team))
}

/// Append-only log of joins, status changes and run bindings.
pub fn team_members_path(team: &str) -> PathBuf {
    team_dir(team).join("members.jsonl")
}

/// Append-only log of task additions, claims and completions.
pub fn team_tasks_path(team: &str) -> PathBuf {
    team_dir(team).join("tasks.jsonl")
}

/// One inbox per member, so a broadcast is still one file per reader.
pub fn team_inbox_path(team: &str, member: &str) -> PathBuf {
    team_dir(team).join("inbox").join(format!("{}.jsonl", sanitize(member)))
}

/// How far through its inbox a member has been shown.
pub fn team_cursor_path(team: &str, member: &str) -> PathBuf {
    team_dir(team).join("inbox").join(format!("{}.cursor", sanitize(member)))
}

/// Keep a team or member name inside its directory. Names reach here from
/// agents, so `../` must never become a path segment.
pub(crate) fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jod_home_honours_an_explicit_override() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", "/tmp/jod-test-home");
        assert_eq!(jod_home(), PathBuf::from("/tmp/jod-test-home"));
        std::env::remove_var("JOD_HOME");
    }

    #[test]
    fn every_team_file_lives_under_that_team_s_directory() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", "/tmp/jod-test-home");
        let dir = team_dir("crew");
        assert!(dir.starts_with(teams_dir()));
        for p in [
            team_members_path("crew"),
            team_tasks_path("crew"),
            team_inbox_path("crew", "scout"),
            team_cursor_path("crew", "scout"),
        ] {
            assert!(p.starts_with(&dir), "{p:?} escaped {dir:?}");
        }
        assert_eq!(
            team_inbox_path("crew", "scout").file_name().unwrap(),
            "scout.jsonl"
        );
        assert_eq!(
            team_cursor_path("crew", "scout").file_name().unwrap(),
            "scout.cursor"
        );
        std::env::remove_var("JOD_HOME");
    }

    /// Team and member names reach here from agents, so a name must never be
    /// able to climb out of the team directory.
    #[test]
    fn a_hostile_name_cannot_escape_its_directory() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", "/tmp/jod-test-home");
        for hostile in ["../../etc", "../evil", "a/b", "..", "./."] {
            let dir = team_dir(hostile);
            assert!(
                dir.starts_with(teams_dir()),
                "{hostile:?} produced {dir:?}"
            );
            assert!(
                !dir.components().any(|c| c.as_os_str() == ".."),
                "{hostile:?} produced a parent component: {dir:?}"
            );
            let inbox = team_inbox_path("crew", hostile);
            assert!(!inbox.components().any(|c| c.as_os_str() == ".."));
        }
        std::env::remove_var("JOD_HOME");
    }

    #[test]
    fn a_name_with_nothing_usable_becomes_unnamed() {
        assert_eq!(sanitize("---"), "unnamed");
        assert_eq!(sanitize(""), "unnamed");
        assert_eq!(sanitize("///"), "unnamed");
        assert_eq!(sanitize(".."), "unnamed");
    }

    #[test]
    fn ordinary_names_survive_sanitising() {
        assert_eq!(sanitize("crew"), "crew");
        assert_eq!(sanitize("team-1_a"), "team-1_a");
        assert_eq!(sanitize("Lead Agent"), "Lead-Agent");
    }

    #[test]
    fn every_run_file_lives_under_that_run_s_directory() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", "/tmp/jod-test-home");
        let dir = run_dir("abc");
        for p in [
            stream_path("abc"),
            prompt_path("abc"),
            script_path("abc"),
            meta_path("abc"),
        ] {
            assert!(p.starts_with(&dir), "{p:?} escaped {dir:?}");
        }
        std::env::remove_var("JOD_HOME");
    }
}
