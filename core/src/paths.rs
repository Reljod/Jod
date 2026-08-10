//! Where Jod keeps its runtime state: `~/.jod`, overridable with `JOD_HOME`.
//!
//! A run's *transcript* lives in `jod.db`, because it is contended state that
//! several processes append to and read. What is left on disk is the record of
//! the launch — what was asked (`prompt.txt`), what was run (`spawn.json`), and
//! anything the supervisor itself had to say (`supervisor.log`) — so a run stays
//! inspectable with `cat` even when Jod is not running.

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

/// The prompt, as it was asked. The harness's output no longer touches the
/// filesystem — it goes to SQLite — but what was *asked* stays greppable.
pub fn prompt_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("prompt.txt")
}

/// Everything the supervisor needs to launch this run, and the human-readable
/// record of exactly what was launched. Replaced the generated `run.sh`.
pub fn spawn_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("spawn.json")
}

/// The supervisor's own stdout and stderr.
///
/// Not the transport: agent output is parsed into events and written to the
/// database. This is where a supervisor that failed *before* it could reach the
/// database leaves its explanation, so such a failure is never silent.
pub fn supervisor_log_path(agent_id: &str) -> PathBuf {
    run_dir(agent_id).join("supervisor.log")
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
            prompt_path("abc"),
            spawn_path("abc"),
            supervisor_log_path("abc"),
            meta_path("abc"),
        ] {
            assert!(p.starts_with(&dir), "{p:?} escaped {dir:?}");
        }
        std::env::remove_var("JOD_HOME");
    }
}
