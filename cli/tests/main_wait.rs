//! `jod main --wait` has to tell the caller how the run it waited for ended.
//!
//! The bug this guards, observed by running the command rather than by reading
//! it (finding X8): a main turn whose harness rejected the model died, the run
//! row said `failed`, and the store held the harness's own sentence naming the
//! rejected model — and `jod main --wait` printed not one character on stdout
//! or stderr and exited 0. A person at the terminal was told nothing was wrong,
//! and every script, cron entry and agent blocking on that exit code treated a
//! dead run as a finished one.
//!
//! This runs the real binary against a stand-in harness, because the failure is
//! in what the process prints and what it exits with, and neither is visible
//! from inside a unit test. The stand-in prints the message AGY really printed
//! and exits 1, which is the whole of what is needed to make a run genuinely
//! fail: no provider, no network, no credentials.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use jod_core::store::Store;

/// The sentence AGY prints when it is handed a model it does not know, as seen
/// in the run that produced finding X7. The point of X8 is that this exact text
/// is already in the store and was being thrown away, so the test asserts on
/// the text itself rather than on a paraphrase of it.
const REJECTED_MODEL: &str = "model gemini-3.7-flash-medium is not recognized as a known model";

/// A fresh `JOD_HOME`, a directory to run in, and a place for the stand-ins.
fn scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "jod-wait-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_nanos()
    ));
    let home = base.join("home");
    let work = base.join("work");
    let bin = base.join("bin");
    for dir in [&home, &work, &bin] {
        std::fs::create_dir_all(dir).expect("a scratch directory");
    }
    (home, work, bin)
}

/// Write an executable shell script and hand back its path.
fn script(at: &Path, body: &str) -> PathBuf {
    std::fs::write(at, body).expect("writing a stand-in");
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755))
        .expect("making a stand-in executable");
    at.to_path_buf()
}

/// The real `jod-run`, which is a sibling of the `jod` this test builds.
///
/// Named explicitly rather than left to discovery: discovery falls back to
/// `PATH`, and on a machine with Jod installed that would quietly drive an
/// installed supervisor instead of the one built from this checkout.
fn supervisor() -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_jod"))
        .parent()
        .expect("the test binary lives in a directory")
        .join("jod-run");
    assert!(
        path.is_file(),
        "{} is missing — build the whole workspace (`cargo test --workspace`), \
         because this test drives the real supervisor",
        path.display()
    );
    path
}

/// How long a `jod main --wait` is given before this test gives up on it.
///
/// Generous, because the point is not to measure how quick the command is. It
/// is that the failure this file guards has a variant where the command never
/// returns at all, and a test that waits for ever on that reports it as nothing
/// rather than as a failure.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// What one `jod main --wait` did: how it ended, what it printed, how long it
/// took, and `None` for a command that had to be killed.
struct Waited {
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
    took: std::time::Duration,
}

impl Waited {
    /// The exit status, insisting the command came back on its own.
    fn status(&self) -> std::process::ExitStatus {
        self.status.unwrap_or_else(|| {
            panic!(
                "`jod main --wait` never returned — killed after {:?}\nstdout: {}\nstderr: {}",
                self.took, self.stdout, self.stderr
            )
        })
    }
}

/// `jod main --wait` against a stand-in AGY with the given body, given
/// [`PATIENCE`] to come back on its own.
///
/// Output goes to files rather than to pipes because this has to poll for the
/// child instead of blocking on it, and `wait_with_output` cannot be polled.
fn wait_on(home: &Path, work: &Path, bin: &Path, agy_body: &str) -> Waited {
    let agy = script(&bin.join("agy"), agy_body);
    let out_path = bin.join("stdout.txt");
    let err_path = bin.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("a file for stdout");
    let err_file = std::fs::File::create(&err_path).expect("a file for stderr");

    let started = std::time::Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["main", "--wait", "-H", "agy", "say which model you are"])
        .current_dir(work)
        .env("JOD_HOME", home)
        .env("JOD_AGY_BIN", &agy)
        .env("JOD_SUPERVISOR_BIN", supervisor())
        .stdout(out_file)
        .stderr(err_file)
        .spawn()
        .expect("the built jod binary runs");

    let mut status = None;
    while started.elapsed() < PATIENCE {
        match child.try_wait().expect("asking whether jod has finished") {
            Some(done) => {
                status = Some(done);
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Waited {
        status,
        stdout: std::fs::read_to_string(&out_path).unwrap_or_default(),
        stderr: std::fs::read_to_string(&err_path).unwrap_or_default(),
        took: started.elapsed(),
    }
}

/// A stand-in AGY that fails the way a rejected model fails: two sentences on
/// stderr, exit 1. Neither line is JSON, so both reach the store as `Raw`
/// events — which is exactly where the real message was found and left.
const REJECTS_THE_MODEL: &str =
    "#!/bin/sh\n\
     echo 'invalid model selection (--model \"gemini-3.7-flash-medium\" --effort \"\"):' >&2\n\
     echo 'model gemini-3.7-flash-medium is not recognized as a known model or custom \
     model in settings' >&2\n\
     exit 1\n";

/// A stand-in AGY that answers, in the shape AGY really answers in.
const ANSWERS: &str = "#!/bin/sh\n\
     printf '%s\\n' '{\"event\":\"step_update\",\"step_update\":{\"step_index\":0,\
     \"state\":\"DONE\",\"step_type\":\"agent_response\",\"text_delta\":\"I am Gemini 3.7 Flash.\"}}'\n\
     printf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"c1\",\
     \"status\":\"SUCCESS\",\"response\":\"I am Gemini 3.7 Flash.\"}}'\n\
     exit 0\n";

/// A stand-in AGY that gets stopped part way through, by signalling the run's
/// own process group — which is what `jod kill`, the console's stop key and a
/// `SIGTERM` to the group all come down to.
///
/// The pause is load-bearing and generous on purpose. The supervisor spawns the
/// harness and then installs its signal handlers, so a `SIGTERM` fired in that
/// window kills the supervisor outright and the run ends with nothing written —
/// which is the *next* case below, not this one. Two seconds is thousands of
/// times the width of that window, so this stays the stop it says it is even on
/// a box with a dozen agents compiling.
const IS_STOPPED: &str = "#!/bin/sh\nsleep 2\nkill -TERM 0\nsleep 30\n";

/// A stand-in AGY that takes the whole run down where nothing can catch it.
/// `SIGKILL` to the process group leaves no verdict of any kind: no `Finished`
/// event, and a run row still saying `running`.
const VANISHES: &str = "#!/bin/sh\nkill -KILL 0\nsleep 30\n";

/// The status the supervisor recorded for the one run this test started.
fn only_run_status(home: &Path) -> String {
    let store = Store::open(&home.join("jod.db")).expect("the database the run wrote");
    let runs = store.runs(10).expect("reading the runs");
    assert_eq!(runs.len(), 1, "one turn, one run: {runs:?}");
    runs[0].status.clone()
}

/// X8, end to end: the run really fails, and the caller has to be able to tell.
#[test]
fn a_failed_main_turn_exits_non_zero_and_says_why() {
    let (home, work, bin) = scratch("failed");

    let out = wait_on(&home, &work, &bin, REJECTS_THE_MODEL);
    let (stdout, stderr) = (&out.stdout, &out.stderr);

    assert_eq!(
        only_run_status(&home),
        "failed",
        "the run has to have genuinely failed for the rest of this to mean anything"
    );
    assert!(
        !out.status().success(),
        "a failed run must not look like a successful one — exit {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status()
    );
    assert!(
        stderr.contains(REJECTED_MODEL),
        "the reason the store already holds has to reach the caller\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The other half of the same guarantee, and the reason the exit code is worth
/// anything: a run that answered still exits 0 and still prints its answer.
/// Without this, "always exit non-zero" would pass the test above.
#[test]
fn an_answered_main_turn_still_exits_zero_and_prints_the_answer() {
    let (home, work, bin) = scratch("answered");

    let out = wait_on(&home, &work, &bin, ANSWERS);
    let (stdout, stderr) = (&out.stdout, &out.stderr);

    assert_eq!(only_run_status(&home), "completed", "stderr: {stderr}");
    assert!(
        out.status().success(),
        "an answered run must exit 0 — exit {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status()
    );
    assert!(
        stdout.contains("I am Gemini 3.7 Flash."),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

/// A run somebody stopped is a third outcome, not a second name for failure.
/// The caller still did not get an answer, so the exit code is non-zero — but
/// the sentence says the run was stopped, because calling a deliberate stop a
/// crash sends the reader looking for a bug that is not there.
#[test]
fn a_stopped_main_turn_says_it_was_stopped_rather_than_that_it_failed() {
    let (home, work, bin) = scratch("stopped");

    let out = wait_on(&home, &work, &bin, IS_STOPPED);
    let (stdout, stderr) = (&out.stdout, &out.stderr);

    assert_eq!(
        only_run_status(&home),
        "killed",
        "the run has to have genuinely been stopped\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !out.status().success(),
        "a stopped run gave the caller no answer — exit {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status()
    );
    assert!(
        stderr.contains("stopped") && !stderr.contains("failed"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

/// The fourth ending, and the one that used to be worse than a wrong exit code:
/// a run whose supervisor was killed before it could write anything left
/// `jod main --wait` blocked for ever. The event stream cannot notice, because
/// the sender belongs to this process rather than to the run, so nothing ever
/// closed it. A script that hangs never even gets to be wrong.
#[test]
fn a_run_that_vanished_ends_the_wait_instead_of_blocking_for_ever() {
    let (home, work, bin) = scratch("vanished");

    let out = wait_on(&home, &work, &bin, VANISHES);
    let (stdout, stderr) = (&out.stdout, &out.stderr);

    // `status()` is what insists the command came back on its own rather than
    // being killed by the test, which is the whole of this case.
    assert!(
        !out.status().success(),
        "the caller got no answer — exit {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status()
    );
    assert!(
        stderr.contains("may still be going"),
        "the run's fate is unknown and the message has to say so\nstdout: {stdout}\nstderr: {stderr}"
    );
}
