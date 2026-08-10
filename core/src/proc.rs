//! Detached process groups — what replaced tmux.
//!
//! A run has to outlive the process that started it. tmux used to provide that;
//! now the supervisor does it directly: it is `setsid`'d into its own session,
//! so it has no controlling terminal and closing an SSH connection cannot send
//! it `SIGHUP`. Because it leads its own process group, its pid *is* its pgid,
//! and the harness it spawns inherits that group.
//!
//! That single number is the whole control surface. Any Jod process — including
//! one started long after the run — can ask whether a run is alive and stop it,
//! using nothing but an integer it read out of SQLite.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Start `program` in a brand-new session, fully detached from this process's
/// terminal, and return its pid — which is also its process-group id.
///
/// `log` receives the child's own stdout and stderr. That is diagnostics for the
/// supervisor itself, not the transport: agent output goes to SQLite. Without
/// it, a supervisor that dies before it can open the database would leave no
/// explanation anywhere.
pub fn spawn_detached(program: &Path, args: &[String], cwd: &Path, log: &Path) -> io::Result<u32> {
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let log_err = log_file.try_clone()?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        // Nothing may read the terminal. The old launcher redirected the
        // harness's stdin for the same reason: a harness that decides to ask a
        // question would otherwise block for an answer nobody can type.
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    // SAFETY: `setsid` is async-signal-safe, which is the only requirement on
    // code running between `fork` and `exec` in a multithreaded parent.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    Ok(cmd.spawn()?.id())
}

/// Send `signal` to every process in the group led by `pgid`.
///
/// The whole group, not just the leader: killing a run must also stop whatever
/// the harness itself started, which is what `tmux kill-session` did.
pub fn signal_group(pgid: u32, signal: i32) -> io::Result<()> {
    // Refuse 0 and 1 outright. `kill(-0, …)` means "my own process group" and
    // would have Jod signal itself; a stored pgid should never be either, so
    // reaching here with one means the value is corrupt, not that we should
    // guess what it meant.
    if pgid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process group {pgid}"),
        ));
    }
    let rc = unsafe { libc::kill(-(pgid as i32), signal) };
    if rc == -1 {
        let err = io::Error::last_os_error();
        // Already gone is the outcome the caller wanted.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

/// Whether any process in the group led by `pgid` still exists.
///
/// `kill(pgid, 0)` performs the permission and existence checks and delivers
/// nothing. `EPERM` counts as alive: the process is there, it just is not ours.
///
/// Pids are recycled, so this can in principle be fooled by a new process that
/// inherited the number. Callers consult the run's recorded status first and
/// only probe when it still says running, which keeps the window to the life of
/// one run rather than the life of the database.
pub fn group_alive(pgid: u32) -> bool {
    if pgid <= 1 {
        return false;
    }
    let rc = unsafe { libc::kill(pgid as i32, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub const SIGTERM: i32 = libc::SIGTERM;
pub const SIGKILL: i32 = libc::SIGKILL;

/// Stop a run: ask politely, then insist.
///
/// `SIGTERM` first so the supervisor can record how the run ended; `SIGKILL`
/// only for a group that ignored it. A supervisor that is killed outright
/// writes nothing, and the run would be left marked running until something
/// else noticed — which is exactly the quiet failure the charter forbids.
pub async fn terminate_group(pgid: u32, grace: std::time::Duration) -> io::Result<()> {
    signal_group(pgid, SIGTERM)?;
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !group_alive(pgid) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    signal_group(pgid, SIGKILL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("prog.sh");
        std::fs::write(&p, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jod-proc-{tag}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_detached_child_leads_its_own_process_group() {
        let dir = tempdir("group");
        let prog = script(&dir, "sleep 30");
        let log = dir.join("log");
        let pid = spawn_detached(&prog, &[], &dir, &log).unwrap();

        // Its pid is its pgid precisely because `setsid` made it a leader. If it
        // were not, `kill(-pid, …)` would hit the wrong group — or ours.
        let pgid = unsafe { libc::getpgid(pid as i32) };
        assert_eq!(pgid, pid as i32, "the child must lead its own group");

        assert!(group_alive(pid));
        signal_group(pid, SIGKILL).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_whole_group_dies_not_just_the_leader() {
        let dir = tempdir("descendants");
        // The leader exits immediately; its child keeps the group alive. Killing
        // by group is the only thing that reaches the survivor.
        let prog = script(&dir, "sleep 30 & echo $! > pid; exit 0");
        let log = dir.join("log");
        let pid = spawn_detached(&prog, &[], &dir, &log).unwrap();

        let mut grandchild = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(dir.join("pid")) {
                if !s.trim().is_empty() {
                    grandchild = s.trim().to_string();
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let grandchild: i32 = grandchild.parse().expect("the leader must report its child");

        signal_group(pid, SIGKILL).unwrap();
        let mut gone = false;
        for _ in 0..100 {
            if unsafe { libc::kill(grandchild, 0) } == -1 {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(gone, "a descendant of the leader survived the group kill");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signalling_a_group_that_is_already_gone_is_success() {
        // The caller wanted it stopped, and it is stopped.
        assert!(signal_group(4_000_000, SIGTERM).is_ok());
    }

    #[test]
    fn group_zero_and_one_are_refused_rather_than_interpreted() {
        // `kill(-0, …)` means "my own process group": Jod would signal itself.
        assert!(signal_group(0, SIGTERM).is_err());
        assert!(signal_group(1, SIGTERM).is_err());
        assert!(!group_alive(0));
        assert!(!group_alive(1));
    }

    #[test]
    fn a_dead_group_is_not_reported_alive() {
        assert!(!group_alive(4_000_000));
    }
}
