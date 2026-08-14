//! `jod-run --version` and `jod-run --help`.
//!
//! The bug: the supervisor took its first argument to be a path and nothing
//! else, so `jod-run --version` tried to *open* `--version` and answered
//!
//! ```text
//! jod-run: reading "--version": No such file or directory
//! ```
//!
//! exiting 1. Two separate things were wrong with that.
//!
//! It is the answer to a question every binary on `PATH` is expected to
//! answer, and `install.sh` puts this one there beside `jod`. The release
//! tarball deliberately carries no version in its filename *because* "the
//! binary answers `--version` itself" — true of `jod` and `jod-api`, and not
//! of the third binary in the same tarball.
//!
//! And it is the only way to tell a stale copy earlier on `PATH` from the one
//! built beside `jod` — the exact failure `jod --version` was given a build
//! stamp for, and one `discovery::find_binary` calls out by name as "a version
//! mismatch nobody asked for".
//!
//! Every test here runs the real binary. The failure was in what the program
//! did with its argv, not in what a constant says.

use std::process::{Command, Output};

/// The built supervisor, invoked with `args`.
fn jod_run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jod-run"))
        .args(args)
        .output()
        .expect("the built jod-run binary runs")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is utf-8")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr is utf-8")
}

/// The version the built binary actually prints, as one line.
fn printed_version(flag: &str) -> String {
    let out = jod_run(&[flag]);
    assert!(
        out.status.success(),
        "jod-run {flag} exited {} — stderr: {}",
        out.status,
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    let line = text
        .lines()
        .next()
        .expect("version output is not empty")
        .to_string();
    assert!(line.starts_with("jod-run "), "{line}");
    line
}

/// The regression itself, stated as the symptom: the flag must not be read as
/// a file. Nothing about a path may appear in the answer, and the exit code is
/// the one a successful question gets.
#[test]
fn the_version_flag_is_not_opened_as_a_file() {
    let out = jod_run(&["--version"]);
    let stderr = stderr_of(&out);

    assert!(
        !stderr.contains("No such file or directory"),
        "`--version` was opened as a path again: {stderr}"
    );
    assert!(!stderr.contains("reading"), "{stderr}");
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
}

/// Same shape `jod --version` answers in — release, then the commit this build
/// came from and that commit's date. Asserted here rather than taken on trust
/// because two binaries shipped in one tarball that disagree about how they
/// name a build are worse than one that cannot be asked.
#[test]
fn version_names_the_build_it_came_from() {
    let line = printed_version("--version");

    assert_ne!(
        line.trim(),
        concat!("jod-run ", env!("CARGO_PKG_VERSION")),
        "the version is the release number and nothing else — two different \
         builds are indistinguishable"
    );

    let build = line
        .split_once(" (")
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("no build identifier in parentheses: {line}"));
    let (id, date) = build
        .split_once(' ')
        .unwrap_or_else(|| panic!("the build identifier is not `<commit> <date>`: {line}"));

    assert!(!id.is_empty(), "empty commit in {line}");
    assert_ne!(
        id, "unknown",
        "built inside a checkout, so git had an answer: {line}"
    );
    let parts: Vec<&str> = date.split('-').collect();
    assert_eq!(parts.len(), 3, "a YYYY-MM-DD date in {line}");
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "a YYYY-MM-DD date in {line}"
    );
}

/// Cheap and load-bearing: the string has to fit on one line and survive being
/// pasted into a bug report.
#[test]
fn version_is_a_single_readable_line() {
    let line = printed_version("--version");
    assert!(line.len() < 80, "{} chars: {line}", line.len());
    assert!(!line.contains('\t'), "{line}");
}

/// `-V`, because that is the other spelling `jod` accepts and a person who
/// learned it there will type it here.
#[test]
fn the_short_version_flag_answers_the_same() {
    assert_eq!(printed_version("-V"), printed_version("--version"));
}

/// `--help` was broken by the same line for the same reason, and it is the
/// flag someone reaches for *first* when a binary on `PATH` is unfamiliar.
#[test]
fn help_explains_the_single_argument() {
    for flag in ["--help", "-h"] {
        let out = jod_run(&[flag]);
        let stderr = stderr_of(&out);

        assert!(
            !stderr.contains("No such file or directory"),
            "`{flag}` was opened as a path: {stderr}"
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "jod-run {flag} — stderr: {stderr}"
        );

        let text = stdout_of(&out);
        assert!(
            text.contains("spawn.json"),
            "help must name the one argument it takes: {text}"
        );
        assert!(text.contains("jod-run"), "{text}");
    }
}

/// The half that must not have changed. `jod-run` is spawned programmatically
/// by `core::runner::launch`, which hands it exactly one absolute path to the
/// run's `spawn.json` — so a first argument that is not a flag has to be
/// treated as a path exactly as before, including how it fails.
#[test]
fn a_path_argument_is_still_read_as_a_path() {
    let missing = std::env::temp_dir().join("jod-run-no-such-plan-file.json");
    let _ = std::fs::remove_file(&missing);

    let out = jod_run(&[missing.to_str().expect("a utf-8 temp path")]);
    let stderr = stderr_of(&out);

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("reading"), "{stderr}");
    assert!(
        stderr.contains("No such file or directory"),
        "the unchanged read error: {stderr}"
    );
}

/// And a path that *does* exist is still read and then parsed — proving the
/// argument reached `run()` rather than being intercepted.
#[test]
fn an_unparseable_plan_still_fails_at_parsing() {
    let dir = std::env::temp_dir().join(format!("jod-run-plan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp directory");
    let plan = dir.join("spawn.json");
    std::fs::write(&plan, b"not json at all").expect("writing the plan");

    let out = jod_run(&[plan.to_str().expect("a utf-8 temp path")]);
    let stderr = stderr_of(&out);

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("parsing"),
        "it must get past reading and into parsing: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// No arguments at all keeps the usage line and the `EX_USAGE` exit code it
/// already had.
#[test]
fn no_arguments_still_prints_usage() {
    let out = jod_run(&[]);
    assert_eq!(out.status.code(), Some(64), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("usage: jod-run <spawn.json>"));
}
