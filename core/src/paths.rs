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

/// Jod's browser MCP server, or `None` if this machine has not got it.
///
/// The script ships in the repo, and where the repo *is* differs by how Jod got
/// onto the box, so this tries every layout rather than assuming one:
///
/// 1. `$JOD_BROWSER_MCP`, for anyone who keeps it somewhere else entirely.
/// 2. `$JOD_SRC`, then `$JOD_HOME/src` — the installed layout. `install.sh`
///    clones the source to `$JOD_HOME/src` and installs only the *binaries* to
///    `$JOD_BIN_DIR`, so the checkout is nowhere near the executable and cannot
///    be found by walking up from it.
/// 3. `$JOD_HOME` itself, which is where the checkout used to live before
///    install.sh started building binaries. Still true on a box installed by an
///    older script, and one line to keep working.
/// 4. Up from the running executable, which is what makes a development tree
///    work: the binary is at `target/{debug,release}/jod` and the repo root is
///    two levels above it.
///
/// `None` rather than a default path, because the caller's decision is whether
/// to *offer* the browser at all: registering an MCP server whose command does
/// not exist gives an agent a set of tools that fail on first use, which is
/// strictly worse than not advertising them.
pub fn browser_mcp_script() -> Option<PathBuf> {
    let named = |root: PathBuf| root.join("browser").join("jod_browser_mcp.py");
    let candidates = [
        std::env::var("JOD_BROWSER_MCP").ok().map(PathBuf::from),
        std::env::var("JOD_SRC").ok().map(|src| named(PathBuf::from(src))),
        Some(named(jod_home().join("src"))),
        Some(named(jod_home())),
        std::env::current_exe()
            .ok()
            .and_then(|exe| Some(named(exe.parent()?.parent()?.parent()?.to_path_buf()))),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// The interpreter that runs it.
///
/// A virtualenv is preferred over the system `python3` because camoufox is a
/// heavy dependency with a pinned Firefox build, and installing that into a
/// system Python is the kind of thing that breaks a machine's other Python.
/// `browser/setup.sh` creates it; falling back to `python3` keeps a
/// hand-managed environment working.
///
/// `browser-venv` is not a name chosen here — it is where the box Jod runs on
/// already has one, per [`docs/browser.md`]. Picking a tidier path would have
/// meant a second multi-hundred-megabyte Firefox on a machine that already had
/// one, to no benefit.
pub fn browser_python() -> PathBuf {
    if let Ok(explicit) = std::env::var("JOD_BROWSER_PYTHON") {
        return PathBuf::from(explicit);
    }
    let venv = jod_home().join("browser-venv").join("bin").join("python");
    if venv.is_file() {
        return venv;
    }
    PathBuf::from("python3")
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

    /// The installed layout, which is not the layout this started out
    /// assuming: `install.sh` clones the source to `$JOD_HOME/src` and installs
    /// only the binaries elsewhere, so the checkout cannot be found by walking
    /// up from the executable. Getting this wrong is silent — agents simply
    /// never get the browser, on exactly the machines where Jod is installed
    /// rather than developed.
    #[test]
    fn the_browser_is_found_where_the_installer_actually_puts_the_source() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("jod-paths-{}", std::process::id()));
        let script = home.join("src").join("browser");
        std::fs::create_dir_all(&script).unwrap();
        std::fs::write(script.join("jod_browser_mcp.py"), b"#\n").unwrap();

        std::env::set_var("JOD_HOME", &home);
        std::env::remove_var("JOD_BROWSER_MCP");
        std::env::remove_var("JOD_SRC");
        assert_eq!(
            browser_mcp_script(),
            Some(script.join("jod_browser_mcp.py")),
            "the installed layout ($JOD_HOME/src) was not searched"
        );

        std::env::remove_var("JOD_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A machine without the script gets `None`, so the caller can decline to
    /// advertise tools that would fail on first use.
    #[test]
    fn a_machine_without_the_browser_says_so_rather_than_guessing_a_path() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let empty = std::env::temp_dir().join(format!("jod-paths-none-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("JOD_HOME", &empty);
        std::env::set_var("JOD_SRC", empty.join("nowhere"));
        std::env::remove_var("JOD_BROWSER_MCP");

        assert_eq!(browser_mcp_script(), None);

        std::env::remove_var("JOD_HOME");
        std::env::remove_var("JOD_SRC");
        let _ = std::fs::remove_dir_all(&empty);
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
