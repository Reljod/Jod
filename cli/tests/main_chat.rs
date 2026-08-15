//! `jod main` works in the directory it was run in, and says so in the roots.
//!
//! The bug this guards, observed by running the command rather than by reading
//! it: `jod main` in a scratch directory under a fresh `JOD_HOME` opened the
//! pinned main chat with `cwd = /home/reljod` — the home directory, not the one
//! the command was typed in — and left `conversation_roots` empty. Both halves
//! compound. The orchestrator's harness process started somewhere the user was
//! not, and the one part of the program that asks *which directories may I
//! read* had no answer at all, so an instruction about the repository you were
//! standing in reached a chat that could not open a file in it.
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
        "jod-main-{tag}-{}-{}",
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

/// `jod main` with no instruction, run in `from`. Reads the chat and starts
/// nothing, which is what makes this cheap enough to assert on: the path under
/// test — resolve the directory, open the main chat, grant the root — runs in
/// full before the empty instruction is noticed.
fn run_main_in(home: &Path, from: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .arg("main")
        .current_dir(from)
        .env("JOD_HOME", home)
        .output()
        .expect("the built jod binary runs");
    assert!(
        out.status.success(),
        "jod main exited {} in {}: {}",
        out.status,
        from.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The main chat as that run left it: where it thinks it is, and what it may
/// read.
fn chat_and_roots(home: &Path) -> (PathBuf, Vec<jod_core::roots::Root>) {
    let store = Store::open(&home.join("jod.db")).expect("the database the run wrote");
    let id = store
        .pinned_conversation()
        .expect("reading the pin")
        .expect("the run should have opened the main chat");
    let cwd = store
        .conversation(&id)
        .expect("reading the conversation")
        .expect("the conversation the pin names")
        .cwd;
    (PathBuf::from(cwd), store.roots(&id).expect("reading roots"))
}

#[test]
fn jod_main_works_in_the_directory_it_was_run_in_and_can_read_it() {
    let (home, work) = scratch("launch-root");

    run_main_in(&home, &work);

    let (cwd, roots) = chat_and_roots(&home);
    assert_eq!(
        normalise(&cwd),
        work,
        "the chat should work where the command was typed, not at $HOME"
    );
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert_eq!(roots[0].path, work);
    assert_eq!(
        roots[0].origin,
        Origin::Human,
        "the same grant the console makes when it opens"
    );
    assert!(!roots[0].writable, "read-only, like every root Jod adds");
}

/// The console grants once per process and remembers it in a set. A `jod main`
/// is one command, so the same directory has to survive being asked for twice
/// — and a second directory still has to arrive, at the end, the way a second
/// console launch puts it there.
#[test]
fn a_second_run_adds_nothing_and_a_second_directory_is_appended() {
    let (home, work) = scratch("repeat");
    let elsewhere = work.parent().expect("a parent").join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a second directory to run in");
    let elsewhere = normalise(&elsewhere);

    run_main_in(&home, &work);
    run_main_in(&home, &work);
    let (_, roots) = chat_and_roots(&home);
    assert_eq!(roots.len(), 1, "the same directory twice is one root: {roots:?}");

    run_main_in(&home, &elsewhere);
    let (_, roots) = chat_and_roots(&home);
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert_eq!(roots[0].path, work, "the first root keeps its place");
    assert_eq!(roots[1].path, elsewhere);
    assert_eq!(roots[1].position, 1);
}
