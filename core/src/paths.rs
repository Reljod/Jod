//! Where Jod keeps its runtime state: `~/.jod`, overridable with `JOD_HOME`.
//!
//! A run's *transcript* lives in `jod.db`, because it is contended state that
//! several processes append to and read. What is left on disk is the record of
//! the launch — what was asked (`prompt.txt`), what was run (`spawn.json`), and
//! anything the supervisor itself had to say (`supervisor.log`) — so a run stays
//! inspectable with `cat` even when Jod is not running.

use std::path::{Path, PathBuf};

pub fn jod_home() -> PathBuf {
    if let Ok(explicit) = std::env::var("JOD_HOME") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".jod")
}

/// Where Jod keeps its things when nobody has said otherwise.
///
/// Separate from [`jod_home`] so that code can ask "is this the real
/// installation?" rather than only "where am I writing?". The one caller that
/// needs the distinction is MCP registration, which rewrites files outside the
/// repository that every tool on the machine reads: a daemon running against a
/// scratch home must not repoint a working Claude Code at a binary that will be
/// gone tomorrow.
pub fn default_jod_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".jod")
}

/// The one SQLite file that holds events, run history and memory.
pub fn db_path() -> PathBuf {
    jod_home().join("jod.db")
}

/// Secret values, one file each, at owner-only permissions.
///
/// Deliberately *beside* `jod.db` rather than inside it. A value in SQLite is a
/// value in every backup, every `jod conv show` and every screen share, and a
/// row cannot carry file permissions. Here the operating system enforces the
/// rule instead: the directory is `0700`, each file is `0600`, and
/// [`crate::secrets::read_secret_value`] refuses to read one whose mode has
/// since been widened. Being under `$JOD_HOME` rather than a repository is the
/// other half of it — nothing here can be committed by an agent working in a
/// checkout.
pub fn secrets_dir() -> PathBuf {
    jod_home().join("secrets")
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

/// This process's own executable, as a path another process can run.
///
/// [`std::env::current_exe`] reads `/proc/self/exe` on Linux, and once the file
/// behind it has been replaced the kernel hands back the old path with
/// **` (deleted)` appended**. That string is part of the returned path, so it
/// is written straight into an agent's MCP config as the command to run — and
/// nothing can execute it.
///
/// Not a theoretical case. It happens on every `/update` and `/upgrade`, which
/// replace the binary while the console keeps running, and on every rebuild for
/// anyone running Jod out of a checkout. Observed as a run whose `mcp.json`
/// said `command: ".../target/debug/jod (deleted)"`; the `jod` MCP server
/// failed to start, and the main chat lost `ask_manager`, `open_work` and every
/// other tool. It could still talk, so it kept answering — it simply could no
/// longer *do* anything, and nothing on any screen said why.
///
/// The marker is stripped and the result used only if something is there now,
/// which is exactly the upgrade case: a new binary sits at the same path. If
/// nothing is, the original is returned unchanged rather than guessed at — a
/// wrong path that looks plausible is harder to diagnose than the real one.
pub fn own_exe() -> std::io::Result<PathBuf> {
    Ok(undeleted(std::env::current_exe()?, |p| p.is_file()))
}

/// The rule [`own_exe`] applies, with the disk lookup handed in.
///
/// Split out so the marker can be tested without replacing the running test
/// binary, which is the only way to produce one for real.
fn undeleted(exe: PathBuf, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let Some(text) = exe.to_str() else {
        return exe;
    };
    let Some(live) = text.strip_suffix(" (deleted)") else {
        return exe;
    };
    let live = PathBuf::from(live);
    if exists(&live) {
        live
    } else {
        exe
    }
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

    /// The `/proc/self/exe` marker that made a console lose its tools.
    ///
    /// After the binary behind it is replaced — every `/update`, every
    /// `/upgrade`, every rebuild from a checkout — Linux hands back the old
    /// path with ` (deleted)` appended, and that string went straight into an
    /// agent's MCP config as the command to run. Observed as
    /// `command: ".../target/debug/jod (deleted)"`: the `jod` server failed to
    /// start, and the main chat lost `ask_manager`, `open_work` and the rest.
    /// It could still talk, so it kept answering; it just could not act, and
    /// nothing on any screen said why.
    #[test]
    fn a_replaced_binary_is_named_by_the_path_that_now_exists() {
        let deleted = PathBuf::from("/usr/local/bin/jod (deleted)");
        assert_eq!(
            undeleted(deleted.clone(), |p| p == Path::new("/usr/local/bin/jod")),
            PathBuf::from("/usr/local/bin/jod"),
            "the upgrade case: a new binary sits at the same path",
        );

        // Nothing there now, so the honest answer is the one the kernel gave.
        // A plausible-looking wrong path is harder to diagnose than the real
        // one.
        assert_eq!(undeleted(deleted.clone(), |_| false), deleted);

        // An ordinary path is untouched, including one that merely mentions the
        // word — the marker is a suffix, not a substring.
        let plain = PathBuf::from("/usr/local/bin/jod");
        assert_eq!(undeleted(plain.clone(), |_| true), plain);
        let odd = PathBuf::from("/home/reljod/jod (deleted) backup/jod");
        assert_eq!(undeleted(odd.clone(), |_| true), odd);
    }

    /// Point `JOD_HOME` somewhere for the length of one test, and put it back
    /// exactly as it was found.
    ///
    /// Restoring rather than unsetting, because `JOD_HOME` is genuinely set on
    /// the box Jod runs on: a test that ends with `remove_var` leaves the rest
    /// of the suite resolving `~/.jod` instead of the configured home, and the
    /// difference only shows up as a file written somewhere nobody looked.
    /// Holds [`crate::ENV_LOCK`] and restores on drop, so a panicking test
    /// cannot leave the variable pointing at its scratch directory.
    struct Override {
        previous: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Override {
        fn to(path: &str) -> Override {
            let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("JOD_HOME").ok();
            std::env::set_var("JOD_HOME", path);
            Override {
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for Override {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("JOD_HOME", value),
                None => std::env::remove_var("JOD_HOME"),
            }
        }
    }

    #[test]
    fn jod_home_honours_an_explicit_override() {
        let _home = Override::to("/tmp/jod-test-home");
        assert_eq!(jod_home(), PathBuf::from("/tmp/jod-test-home"));
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
        let _home = Override::to("/tmp/jod-test-home");
        let dir = run_dir("abc");
        for p in [
            prompt_path("abc"),
            spawn_path("abc"),
            supervisor_log_path("abc"),
            meta_path("abc"),
        ] {
            assert!(p.starts_with(&dir), "{p:?} escaped {dir:?}");
        }
    }

    #[test]
    fn the_secrets_directory_is_under_the_home_it_is_told_about() {
        // The one path where getting this wrong writes a credential into a
        // directory nobody meant — which is exactly what an unlocked caller
        // resolving `JOD_HOME` mid-test does.
        let _home = Override::to("/tmp/jod-test-home");
        assert_eq!(
            secrets_dir(),
            PathBuf::from("/tmp/jod-test-home").join("secrets")
        );
    }
}
