//! What `jod --version` answers.
//!
//! `jod 0.1.0` is true of every build this repo has ever produced, which makes
//! it useless for the one question anybody asks it: *is the binary I am
//! running the code I am reading?* A stale copy on `$PATH` was missing seven
//! subcommands from the tree beside it and said `jod 0.1.0` all the same. So
//! the release number is followed by the commit the binary was built from and
//! that commit's date, stamped by `build.rs`.

/// `0.1.0 (f4e4c72 2026-08-13)` — release, commit, commit date.
///
/// `-dirty` on the commit means the checkout had uncommitted changes;
/// `unknown` means the build had no git to ask, which is what a tarball or a
/// box without git gets. Neither is an error: the date still narrows it down.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("JOD_BUILD_ID"),
    " ",
    env!("JOD_BUILD_DATE"),
    ")"
);

#[cfg(test)]
mod tests {
    use super::LONG_VERSION;

    /// The point of the whole file: two builds of different commits must not
    /// print the same string. That needs a build id present and non-empty.
    #[test]
    fn the_version_carries_a_build_identifier() {
        let (release, build) = LONG_VERSION
            .split_once(" (")
            .expect("the version names its build in parentheses");
        let build = build.strip_suffix(')').expect("…closed parentheses");
        let (id, date) = build.split_once(' ').expect("…holding a commit and a date");

        assert!(!release.is_empty(), "{LONG_VERSION}");
        assert!(!id.is_empty(), "no build id in {LONG_VERSION}");
        assert_eq!(date.len(), 10, "a YYYY-MM-DD date in {LONG_VERSION}");
        assert_eq!(
            date.split('-').count(),
            3,
            "a YYYY-MM-DD date in {LONG_VERSION}"
        );
        assert!(
            date.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "a YYYY-MM-DD date in {LONG_VERSION}"
        );
    }

    /// Built inside this repo, the id is a real commit — not the `unknown`
    /// that only a checkout without git may fall back to.
    #[test]
    fn a_build_from_a_checkout_names_its_commit() {
        let id = LONG_VERSION
            .split_once(" (")
            .and_then(|(_, rest)| rest.split_once(' '))
            .map(|(id, _)| id)
            .expect("a build id");
        assert_ne!(id, "unknown", "{LONG_VERSION}");
        assert!(
            id.trim_end_matches("-dirty").len() >= 7,
            "an abbreviated commit is at least 7 characters: {LONG_VERSION}"
        );
    }
}
