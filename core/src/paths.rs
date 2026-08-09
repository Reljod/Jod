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

/// The one SQLite file that holds events, run history and memory.
pub fn db_path() -> PathBuf {
    jod_home().join("jod.db")
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

    use crate::testsupport::EnvGuard;

    #[test]
    fn without_an_override_state_lives_in_a_dot_directory_under_home() {
        let mut env = EnvGuard::new();
        env.remove("JOD_HOME");
        env.set("HOME", "/home/someone");
        assert_eq!(jod_home(), PathBuf::from("/home/someone/.jod"));
    }

    #[test]
    fn a_missing_home_still_yields_a_usable_relative_path() {
        let mut env = EnvGuard::new();
        env.remove("JOD_HOME");
        env.remove("HOME");
        assert_eq!(jod_home(), PathBuf::from("./.jod"));
    }

    #[test]
    fn every_run_lives_in_its_own_directory_under_runs() {
        let mut env = EnvGuard::new();
        env.set("JOD_HOME", "/tmp/jod-test-home");

        assert_eq!(runs_dir(), PathBuf::from("/tmp/jod-test-home/runs"));
        assert_eq!(run_dir("abc"), runs_dir().join("abc"));
        assert_ne!(run_dir("abc"), run_dir("def"));
    }

    #[test]
    fn each_run_file_has_its_own_stable_name() {
        let mut env = EnvGuard::new();
        env.set("JOD_HOME", "/tmp/jod-test-home");

        assert_eq!(stream_path("a").file_name().unwrap(), "stream.jsonl");
        assert_eq!(prompt_path("a").file_name().unwrap(), "prompt.txt");
        assert_eq!(script_path("a").file_name().unwrap(), "run.sh");
        assert_eq!(meta_path("a").file_name().unwrap(), "agent.json");
    }
}
