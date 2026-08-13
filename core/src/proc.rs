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

    let child = cmd.spawn()?;
    let pid = child.id();
    reap(child);
    Ok(pid)
}

/// Collect a detached child's exit status, so a run that ends leaves nothing
/// behind.
///
/// Detaching a process does not stop it being this process's child: `setsid`
/// changes its session, not its parentage. So the kernel still holds its exit
/// status until somebody asks for it, and until then the pid stays in the
/// table as a zombie. `jod tui` starts one supervisor per agent and, on the VPS,
/// stays up for weeks — so every finished run used to add a corpse that lived
/// as long as the console did.
///
/// A thread per live run rather than a `SIGCHLD` handler, because the
/// disposition of that signal belongs to the whole process: `jod` also runs
/// `git`, `gh` and `$EDITOR` through `Command::status`, and anything global
/// enough to reap these would take their exit statuses too. Waiting on one
/// known `Child` is the same thing the supervisor does with its harness, and it
/// touches nothing else.
///
/// The thread blocks until the run ends, which is exactly as long as the corpse
/// would otherwise have to wait, and it costs the run nothing: the child is in
/// its own session and never learns that anyone is waiting. If the console exits
/// first the run keeps going, orphaned to init — which reaps it instead.
fn reap(mut child: std::process::Child) {
    let pid = child.id();
    let spawned = std::thread::Builder::new()
        .name(format!("jod-reap-{pid}"))
        .spawn(move || {
            // The status itself is nobody's business here. How a run ended is
            // the supervisor's to record, into the database, where a process
            // that never started it can still read it. This wants only the
            // side effect of asking.
            let _ = child.wait();
        });
    if let Err(e) = spawned {
        // The child is already running and healthy; refusing to report it
        // because a thread could not start would turn a live run into a failed
        // spawn. It goes unreaped instead — the behaviour of every run before
        // this existed — and says so.
        eprintln!("[jod] could not start a reaper for pid {pid}: {e}");
    }
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
/// **A zombie does not count.** The question every caller is asking is "is this
/// still running", and a leader that has exited but not been reaped is not: it
/// is an exit status nobody has collected yet. `kill(pid, 0)` cannot tell the
/// two apart, so this used to report a finished run as a live one for as long
/// as the corpse held the pid, which is what had [`terminate_group`] watch it
/// for the whole grace and then signal it.
///
/// [`spawn_detached`] now waits on its own children, so a run this process
/// started is a zombie only for as long as its reaper takes to notice. This
/// check still has to be here for every other case: a run is addressed by a pgid
/// read out of SQLite, so the asker is very often not the process that started
/// it and cannot wait on it at all.
///
/// Pids are recycled, so this can in principle be fooled by a new process that
/// inherited the number — and reaping releases a pid for reuse sooner than a
/// corpse left in the table would. Callers consult the run's recorded status
/// first and only probe when it still says running, which keeps the window to
/// the life of one run rather than the life of the database.
pub fn group_alive(pgid: u32) -> bool {
    if pgid <= 1 {
        return false;
    }
    let rc = unsafe { libc::kill(pgid as i32, 0) };
    if rc == 0 {
        return !zombie(pgid);
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether `pid` has exited and is only waiting to be reaped.
///
/// Asked of the group *leader*, which is the process [`group_alive`] probes.
#[cfg(target_os = "linux")]
fn zombie(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // The state is the field after `comm`, which is parenthesised and may
    // itself contain a `)` — so it is the *last* one that ends the name.
    match stat.rfind(')') {
        Some(end) => stat[end + 1..].trim_start().starts_with('Z'),
        None => false,
    }
}

/// Whether `pid` has exited and is only waiting to be reaped.
///
/// `KERN_PROC_PID` is the interface that reports a zombie at all: `proc_pidinfo`
/// answers `ESRCH` for one, since libproc describes running processes and a
/// corpse is not one. This reads the `p_stat` byte `ps` prints as `Z` out of the
/// `extern_proc` that leads `struct kinfo_proc`.
///
/// The two fields are at fixed offsets — the layout is frozen ABI, and every
/// `ps` on the platform depends on it — but the answer is only trusted when the
/// pid field at its own offset is the pid that was asked about. A struct that
/// ever moved therefore reports "not a zombie" and leaves the probe exactly as
/// it was before this existed, rather than reading a byte of something else.
#[cfg(target_os = "macos")]
fn zombie(pid: u32) -> bool {
    const P_STAT: usize = 36;
    const P_PID: usize = 40;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as i32,
    ];
    let mut buf = [0u8; 1024];
    let mut len = buf.len();
    // SAFETY: `mib` is a four-element control name of exactly the length
    // passed, and `len` both describes `buf` going in and is updated with what
    // was written coming out.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < P_PID + 4 {
        return false;
    }
    let reported = u32::from_ne_bytes([buf[P_PID], buf[P_PID + 1], buf[P_PID + 2], buf[P_PID + 3]]);
    reported == pid && u32::from(buf[P_STAT]) == libc::SZOMB
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn zombie(_pid: u32) -> bool {
    false
}

pub const SIGTERM: i32 = libc::SIGTERM;
pub const SIGKILL: i32 = libc::SIGKILL;

/// Stop a run: ask politely, then insist.
///
/// `SIGTERM` first so the supervisor can record how the run ended; `SIGKILL`
/// only for a group that ignored it. A supervisor that is killed outright
/// writes nothing, and the run would be left marked running until something
/// else noticed — which is exactly the quiet failure the charter forbids.
/// A refused signal is only a failure if something is still running when this
/// returns. The goal state is "not running", and a group that died as we
/// reached for it has reached it — reporting that as an error is what told the
/// reader their agent "may still be writing" after every deliberate stop.
pub async fn terminate_group(pgid: u32, grace: std::time::Duration) -> io::Result<()> {
    if let Err(e) = signal_group(pgid, SIGTERM) {
        return if group_alive(pgid) { Err(e) } else { Ok(()) };
    }
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !group_alive(pgid) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    match signal_group(pgid, SIGKILL) {
        Ok(()) => Ok(()),
        Err(e) if group_alive(pgid) => Err(e),
        Err(_) => Ok(()),
    }
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

    /// A run that ends is a corpse until its reaper collects it, and the pid
    /// probe alone cannot see the difference. That window is short now and used
    /// to last the whole session, but "exited" has to read as not-alive in
    /// either case — so this asserts the answer, not the timing.
    #[test]
    fn a_leader_that_exited_is_never_reported_alive() {
        let dir = tempdir("zombie");
        let prog = script(&dir, "exit 0");
        let pid = spawn_detached(&prog, &[], &dir, &dir.join("log")).unwrap();

        let mut gone = false;
        for _ in 0..200 {
            if !group_alive(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(gone, "a corpse holding pid {pid} was reported as running");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The leak a resident console reaches: a finished child must leave the
    /// process table by itself.
    ///
    /// `jod tui` starts one detached `jod-run` per agent and then stays up for
    /// weeks — that is how Jod is deployed. Every run that ended used to leave a
    /// corpse behind for the rest of the session, because the `Child` was
    /// dropped and nothing ever waited on it. Twenty short-lived children are
    /// enough to see it, and the count in the failure message is the leak.
    #[test]
    fn a_finished_child_is_reaped_rather_than_left_in_the_table() {
        let dir = tempdir("reaped");
        let prog = script(&dir, "exit 0");
        let log = dir.join("log");
        let pids: Vec<u32> = (0..20)
            .map(|_| spawn_detached(&prog, &[], &dir, &log).unwrap())
            .collect();

        // First that they all really ended, so the reaping question is asked of
        // corpses and not of children still on their way to `exit`.
        let running = poll_until(|| pids.iter().copied().filter(|&p| group_alive(p)).collect());
        assert!(running.is_empty(), "children never finished: {running:?}");

        // Then that none of them is still an unreaped exit status. `zombie` is
        // the exact question — a pid that was reaped and immediately reused by
        // some unrelated process on this box would answer a liveness probe, but
        // it would not answer this one.
        let corpses = poll_until(|| pids.iter().copied().filter(|&p| zombie(p)).collect());
        assert!(
            corpses.is_empty(),
            "{} of {} finished children were never reaped: {corpses:?}",
            corpses.len(),
            pids.len()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Re-run `probe` until it comes back empty, and hand back whatever it said
    /// last. Ten seconds is far longer than a `bash -c 'exit 0'` needs; a probe
    /// still returning pids by then is reporting a state that will not change.
    fn poll_until(probe: impl Fn() -> Vec<u32>) -> Vec<u32> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let left = probe();
            if left.is_empty() || std::time::Instant::now() >= deadline {
                return left;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The whole of BUG-18: stopping a run that has already ended must be a
    /// fast success, not five seconds of watching a corpse followed by a signal
    /// the kernel refuses and a warning that the agent "may still be writing".
    #[tokio::test]
    async fn stopping_a_group_that_already_ended_succeeds_at_once() {
        let dir = tempdir("already-gone");
        let prog = script(&dir, "exit 0");
        let pid = spawn_detached(&prog, &[], &dir, &dir.join("log")).unwrap();

        // Waited for without `group_alive`, which is the thing under test.
        for _ in 0..200 {
            if zombie(pid) || unsafe { libc::kill(pid as i32, 0) } == -1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let started = std::time::Instant::now();
        let stopped = terminate_group(pid, std::time::Duration::from_secs(5)).await;
        let took = started.elapsed();

        assert!(stopped.is_ok(), "an already-dead group: {stopped:?}");
        assert!(
            took < std::time::Duration::from_secs(1),
            "waited {took:?} out on a process that had already ended"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
