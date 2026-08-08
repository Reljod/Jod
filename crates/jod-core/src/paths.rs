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
}
