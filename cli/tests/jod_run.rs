//! `jod run` works in the directory it was run in, and says so in the roots.
//!
//! The other half of the fault `main_chat.rs` guards, and the reason there are
//! two files rather than one. `jod main` was fixed on its own while every other
//! entry point was left answering `$HOME`, and the record then said the whole
//! thing was done. Observed by running the command rather than by reading it:
//! `jod run --detach -n probe "hi"`, fired inside a scratch repository under a
//! fresh `JOD_HOME`, recorded `cwd = /home/reljod` — the home directory, not
//! the one the command was typed in — and left `conversation_roots` empty. So
//! the run started somewhere the caller was not, and the part of the program
//! that asks *which directories may I read* had no answer at all.
//!
//! This runs the real binary, for the same reason `version.rs` does: the
//! failure was in what the program does when it is launched, and the launch
//! directory is a property of the process. A unit test would have to move the
//! test runner's own working directory, which every other test in the binary
//! shares.

use std::path::{Path, PathBuf};
use std::process::Command;

use jod_core::roots::{normalise, Origin};
use jod_core::store::Store;

/// A fresh `JOD_HOME` and a directory to run in, both real and canonicalised.
///
/// Canonicalised because `add_root` canonicalises, and on macOS
/// `std::env::temp_dir()` is a symlink — comparing the two spellings of one
/// directory is exactly the mistake the roots code keeps having to undo.
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "jod-run-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_nanos()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).expect("a scratch JOD_HOME");
    std::fs::create_dir_all(&work).expect("a directory to run in");
    (normalise(&home), normalise(&work))
}

/// A `claude` and a `jod-run` that exist, can be executed, and do nothing.
///
/// `jod run` refuses to start without a supervisor and would otherwise launch a
/// real harness, which a test must not do. Pointing the two discovery variables
/// at stand-ins is the same move `core/src/service.rs` already makes for the
/// concurrency cap, and it keeps the real launch path under test: the
/// conversation this file asserts on is opened by `spawn_agent` before anything
/// is executed.
fn stub_binaries(home: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let bin = home.parent().expect("a scratch base").join("bin");
    std::fs::create_dir_all(&bin).expect("a directory for the stand-ins");
    let claude = bin.join("claude");
    let supervisor = bin.join("jod-run");
    for path in [&claude, &supervisor] {
        std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("writing a stand-in");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("making a stand-in executable");
    }
    (claude, supervisor)
}

/// `jod run --detach`, fired in `from` against the stand-in harness.
///
/// Detached because the run itself is not what is being tested. The directory
/// and the root are settled before the harness is executed at all, so the test
/// asserts on them and lets the stand-in exit on its own.
fn run_detached_in(home: &Path, from: &Path) {
    let (claude, supervisor) = stub_binaries(home);
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["run", "--detach", "-n", "probe", "hi"])
        .current_dir(from)
        .env("JOD_HOME", home)
        .env("JOD_CLAUDE_BIN", &claude)
        .env("JOD_SUPERVISOR_BIN", &supervisor)
        .output()
        .expect("the built jod binary runs");
    assert!(
        out.status.success(),
        "jod run exited {} in {}: {}",
        out.status,
        from.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The one conversation that run opened: where it thinks it is, and what it may
/// read.
///
/// `jod run` opens a conversation of its own rather than the pinned main chat,
/// so this asks for the whole list and insists there is exactly one. A second
/// row would mean the run reached a thread this test never named, which is a
/// failure worth reading rather than one worth filtering past.
fn only_conversation(home: &Path) -> (PathBuf, Vec<jod_core::roots::Root>) {
    let store = Store::open(&home.join("jod.db")).expect("the database the run wrote");
    let all = store.conversations(10).expect("reading the conversations");
    assert_eq!(all.len(), 1, "one run, one conversation: {all:?}");
    let id = all[0].id.clone();
    let cwd = store
        .conversation(&id)
        .expect("reading the conversation")
        .expect("the conversation the listing named")
        .cwd;
    (PathBuf::from(cwd), store.roots(&id).expect("reading roots"))
}

/// The half of L3 that `jod main`'s fix left behind.
///
/// A `jod run` fired inside a repository recorded `cwd = /home/reljod` and no
/// roots at all, so the run started in the home directory and could not name
/// the checkout it was asked about.
#[test]
fn jod_run_works_in_the_directory_it_was_run_in_and_can_read_it() {
    let (home, work) = scratch("run-launch-root");

    run_detached_in(&home, &work);

    let (cwd, roots) = only_conversation(&home);
    assert_eq!(
        normalise(&cwd),
        work,
        "the run should start where the command was typed, not at $HOME"
    );
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert_eq!(roots[0].path, work);
    assert_eq!(
        roots[0].origin,
        Origin::Human,
        "the same grant every other entry point makes"
    );
    assert!(!roots[0].writable, "read-only, like every root Jod adds");
}

/// An explicit `--cwd` is somebody saying where, and it still wins over the
/// directory the command was typed in.
#[test]
fn an_explicit_cwd_still_beats_the_launch_directory() {
    let (home, work) = scratch("run-explicit-cwd");
    let elsewhere = work.parent().expect("a parent").join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a directory to point at");
    let elsewhere = normalise(&elsewhere);

    let (claude, supervisor) = stub_binaries(&home);
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["run", "--detach", "-n", "probe", "--cwd"])
        .arg(&elsewhere)
        .arg("hi")
        .current_dir(&work)
        .env("JOD_HOME", &home)
        .env("JOD_CLAUDE_BIN", &claude)
        .env("JOD_SUPERVISOR_BIN", &supervisor)
        .output()
        .expect("the built jod binary runs");
    assert!(
        out.status.success(),
        "jod run exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let (cwd, roots) = only_conversation(&home);
    assert_eq!(normalise(&cwd), elsewhere, "--cwd names the directory");
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert_eq!(roots[0].path, elsewhere);
}
