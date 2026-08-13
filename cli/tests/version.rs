//! `jod --version` has to identify the *build*, not just the release.
//!
//! The bug this guards: an installed copy of `jod` and a freshly built one
//! reported the identical `jod 0.1.0` while differing by seven subcommands, so
//! "unrecognised subcommand" was the only clue that the binary on `$PATH` was
//! months old. This runs the real binary, because the failure was in what the
//! program prints, not in what a constant says.

use std::process::Command;

/// The version the built binary actually prints, as one line.
fn printed_version() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .arg("--version")
        .output()
        .expect("the built jod binary runs");
    assert!(out.status.success(), "jod --version exited {}", out.status);
    let text = String::from_utf8(out.stdout).expect("version output is utf-8");
    let line = text.lines().next().expect("version output is not empty").to_string();
    assert!(line.starts_with("jod "), "{line}");
    line
}

#[test]
fn version_names_the_build_it_came_from() {
    let line = printed_version();

    // The regression itself: the whole answer being the release number.
    assert_ne!(
        line.trim(),
        concat!("jod ", env!("CARGO_PKG_VERSION")),
        "the version is the release number and nothing else — two different \
         builds are indistinguishable again"
    );

    let build = line
        .split_once(" (")
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("no build identifier in parentheses: {line}"));
    let (id, date) = build
        .split_once(' ')
        .unwrap_or_else(|| panic!("the build identifier is not `<commit> <date>`: {line}"));

    assert!(!id.is_empty(), "empty commit in {line}");
    assert_ne!(id, "unknown", "built inside a checkout, so git had an answer: {line}");
    let parts: Vec<&str> = date.split('-').collect();
    assert_eq!(parts.len(), 3, "a YYYY-MM-DD date in {line}");
    assert!(
        parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "a YYYY-MM-DD date in {line}"
    );
}

/// Cheap and load-bearing: the string has to fit on one line and survive being
/// pasted into a bug report.
#[test]
fn version_is_a_single_readable_line() {
    let line = printed_version();
    assert!(line.len() < 80, "{} chars: {line}", line.len());
    assert!(!line.contains('\t'), "{line}");
}
