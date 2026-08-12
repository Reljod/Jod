//! `jod update` — take newer patch releases of the binaries running on this box.
//!
//! The work is done by `install.sh` out of Jod's own checkout, not
//! reimplemented here. Version resolution is a set of rules about tags
//! (highest patch within the installed MAJOR.MINOR, never a minor jump), and
//! two implementations of those rules that disagree would mean `jod update`
//! and `install.sh` installing different things from the same tag list. So
//! this module's whole job is to answer the three questions the script cannot:
//! where the checkout is, where the binaries currently live, and which of them
//! this machine actually has.
//!
//! It matters most on the VPS, where the console is a long-lived `jod tui`.
//! The installer renames the new binary over the old one rather than writing
//! it in place, so an update never fails with ETXTBSY on the binary running
//! it — the running process keeps its inode until it is restarted. That is
//! also how [`Outcome::replaced`] is decided: the inode under the running
//! binary's own path either changed or it did not, which is a fact about the
//! filesystem rather than a guess parsed out of the installer's prose.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use tokio::sync::mpsc::UnboundedSender;

/// What an update did, for a caller that has to decide what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// A new binary is on disk where the running one is. False for `--check`,
    /// and for a run that found itself already current — which is the
    /// difference between "restart to pick this up" and saying nothing.
    pub replaced: bool,
}

/// Run the installer against this machine's checkout, with its output going
/// wherever this process's output goes.
///
/// `check` reports and changes nothing; `version` overrides the patch-only
/// default with an explicit ref (that is the deliberate act a minor or major
/// move should be); `force` rebuilds even when the commit already matches.
pub fn run(check: bool, version: Option<String>, force: bool) -> Result<Outcome> {
    execute(check, version, force, None)
}

/// The same, with every line the installer writes handed to `lines` as it
/// arrives.
///
/// For the console, which cannot give up its terminal to a subprocess: an
/// update is minutes of `git` and `cargo` output, and that output is the only
/// diagnosis a failed update has. Blocking — run it on a thread that is
/// allowed to block.
pub fn run_streaming(
    check: bool,
    version: Option<String>,
    force: bool,
    lines: UnboundedSender<String>,
) -> Result<Outcome> {
    execute(check, version, force, Some(lines))
}

fn execute(
    check: bool,
    version: Option<String>,
    force: bool,
    lines: Option<UnboundedSender<String>>,
) -> Result<Outcome> {
    let src = source_checkout();
    let installer = src.join("install.sh");
    if !installer.is_file() {
        bail!(
            "no Jod checkout at {} — this build was not put here by install.sh.\n\
             Install it with:\n  \
             curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash\n\
             Or point $JOD_SRC at an existing checkout.",
            src.display()
        );
    }

    let exe = running_binary()?;
    let bin_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", exe.display()))?
        .to_path_buf();
    let before = file_identity(&exe);

    let mut cmd = Command::new("bash");
    cmd.arg(&installer);
    if check {
        cmd.arg("--check");
    }
    if force {
        cmd.arg("--force");
    }
    cmd.env("JOD_SRC", &src)
        // Where *this* binary is, not a default: an update has to replace the
        // binaries the box is actually running, whether that is ~/.local/bin
        // or the /usr/local/bin a systemd unit points at.
        .env("JOD_BIN_DIR", &bin_dir)
        .env("JOD_VERSION", version.as_deref().unwrap_or("patch"));
    // An update must not quietly drop a binary this machine has. jod-api is
    // opt-in at install time precisely because it is an endpoint that spawns
    // agents — but a box that already made that choice keeps it.
    if bin_dir.join("jod-api").exists() {
        cmd.env("JOD_WITH_API", "1");
    }

    let status = match lines {
        None => cmd
            .status()
            .with_context(|| format!("running {}", installer.display()))?,
        Some(tx) => stream(cmd, tx).with_context(|| format!("running {}", installer.display()))?,
    };
    if !status.success() {
        let how = status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| c.to_string());
        bail!("update failed — {} exited with {how}", installer.display());
    }

    Ok(Outcome {
        replaced: !check && file_identity(&exe) != before,
    })
}

/// Run it with both streams captured, forwarding each line as it is written.
///
/// stdout and stderr are read by their own threads rather than one after the
/// other: the installer writes progress to one and `cargo` writes to the
/// other, and draining them in sequence would deadlock the moment the pipe
/// nobody is reading fills up.
fn stream(mut cmd: Command, tx: UnboundedSender<String>) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let mut pumps = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        pumps.push(std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                // A closed receiver means the console moved on; that is not
                // this thread's problem to report.
                if tx.send(line).is_err() {
                    break;
                }
            }
        }));
    }

    let status = child.wait()?;
    // Joined *after* the wait so no line is lost between the last write and
    // the exit — the summary lines are the ones a reader most wants.
    for pump in pumps {
        let _ = pump.join();
    }
    Ok(status)
}

/// Where the source lives: `$JOD_SRC`, else `src/` inside Jod's state
/// directory. Keeping it under `$JOD_HOME` is what lets this be found with no
/// configuration at all — including from a systemd unit whose `$HOME` is not
/// the one that ran the installer.
fn source_checkout() -> PathBuf {
    checkout_from(std::env::var_os("JOD_SRC"), jod_core::paths::jod_home())
}

fn checkout_from(explicit: Option<std::ffi::OsString>, jod_home: PathBuf) -> PathBuf {
    explicit.map_or_else(|| jod_home.join("src"), PathBuf::from)
}

/// This binary's own path, with symlinks resolved — a symlinked `jod` must
/// update the file it points at, not the link.
pub fn running_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this binary's own path")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Which file is at this path, as the filesystem knows it. `None` when it
/// cannot be read, which compares unequal to nothing — an unreadable binary is
/// never reported as replaced.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checkout_defaults_to_src_inside_the_state_directory() {
        assert_eq!(
            checkout_from(None, PathBuf::from("/home/jod/.jod")),
            PathBuf::from("/home/jod/.jod/src")
        );
    }

    #[test]
    fn an_explicit_checkout_wins_over_the_state_directory() {
        assert_eq!(
            checkout_from(Some("/opt/Jod".into()), PathBuf::from("/home/jod/.jod")),
            PathBuf::from("/opt/Jod")
        );
    }

    #[test]
    fn a_missing_file_has_no_identity_and_so_never_reads_as_replaced() {
        assert_eq!(file_identity(Path::new("/nonexistent/jod")), None);
    }

    #[test]
    fn the_same_file_has_the_same_identity_twice() {
        let path = Path::new("/proc/self/exe");
        assert_eq!(file_identity(path), file_identity(path));
    }

    /// `$JOD_SRC` is process-wide, so the tests that set it take turns.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fixture checkout whose `install.sh` is `body`.
    fn checkout_with(name: &str, body: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jod-update-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture checkout");
        let script = dir.join("install.sh");
        std::fs::write(&script, body).expect("fixture installer");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    fn drain(mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            out.push(line);
        }
        out
    }

    /// The console shows the installer's output as it arrives, and that output
    /// is the only diagnosis a failed update has — so both streams have to
    /// come back, not just the one the script happened to use.
    #[test]
    fn streaming_forwards_stdout_and_stderr() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let src = checkout_with(
            "streams",
            "#!/usr/bin/env bash
echo 'to stdout'
echo 'to stderr' >&2
",
        );
        std::env::set_var("JOD_SRC", &src);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome =
            run_streaming(false, None, false, tx).expect("the fixture installer succeeds");
        std::env::remove_var("JOD_SRC");

        let lines = drain(rx);
        assert!(lines.contains(&"to stdout".to_string()), "{lines:?}");
        assert!(lines.contains(&"to stderr".to_string()), "{lines:?}");
        assert!(
            !outcome.replaced,
            "nothing replaced this test binary, so nothing claims a restart is needed"
        );
        let _ = std::fs::remove_dir_all(&src);
    }

    /// A non-zero exit is an error carrying the exit code, never a quiet
    /// success — an update that failed and said nothing is the worst outcome
    /// available here.
    #[test]
    fn a_failing_installer_is_an_error_that_names_the_code() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let src = checkout_with(
            "fails",
            "#!/usr/bin/env bash
echo 'no'
exit 3
",
        );
        std::env::set_var("JOD_SRC", &src);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let err = run_streaming(false, None, false, tx).expect_err("exit 3 is a failure");
        std::env::remove_var("JOD_SRC");

        assert!(format!("{err}").contains('3'), "{err}");
        let _ = std::fs::remove_dir_all(&src);
    }

    /// A checkout that is not there is a sentence telling you how to get one,
    /// not a backtrace.
    #[test]
    fn no_checkout_says_how_to_install_one() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_SRC", "/nonexistent/jod-src");
        let err = run(true, None, false).expect_err("there is no installer there");
        std::env::remove_var("JOD_SRC");
        let said = format!("{err}");
        assert!(said.contains("install.sh"), "{said}");
        assert!(said.contains("JOD_SRC"), "{said}");
    }
}
