//! Stamps the commit this binary was built from into `jod-run --version`.
//!
//! **This is `cli/build.rs`, applied to the second binary in the same
//! tarball.** The reasoning is written out in full there and is not repeated:
//! why the *commit's* date rather than the wall clock, why `rerun-if-changed`
//! is stated explicitly, why nothing here may fail the build. What matters is
//! that the two agree — `jod` and `jod-run` ship together, are installed
//! together by `install.sh`, and are looked up as siblings by
//! `discovery::find_binary`, so a build identifier that meant two different
//! things depending on which one you asked would be worse than none.
//!
//! Both emit the same two variables, `JOD_BUILD_ID` and `JOD_BUILD_DATE`, from
//! the same two git commands, with the same fallbacks. They are separate files
//! because a build script belongs to exactly one package and `cli` is not a
//! dependency of `supervisor` — extracting the shared half into a crate both
//! take as a build-dependency is the obvious follow-up, and is deliberately
//! not folded into a bug fix that must not touch the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let (id, date) = stamp();
    println!("cargo:rustc-env=JOD_BUILD_ID={id}");
    println!("cargo:rustc-env=JOD_BUILD_DATE={date}");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    for path in git_watch_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

/// `(build id, date)` — the commit and its date, or the honest admission that
/// this build could not tell.
fn stamp() -> (String, String) {
    match git(&["describe", "--always", "--dirty", "--abbrev=7", "--exclude=*"]) {
        Some(id) => {
            let date =
                git(&["log", "-1", "--date=short", "--format=%cd"]).unwrap_or_else(build_date);
            (id, date)
        }
        None => ("unknown".to_string(), build_date()),
    }
}

/// A git command's single-line output, reduced to characters that are safe to
/// stamp — or `None` for any reason at all.
fn git(args: &[&str]) -> Option<String> {
    sanitise(&git_raw(args)?)
}

/// The same, verbatim — for paths and ref names, which `sanitise` would strip
/// the separators out of.
fn git_raw(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Keeps the stamp to characters that cannot break out of the `rustc-env`
/// line cargo parses, or out of the single-line version string.
fn sanitise(text: &str) -> Option<String> {
    let clean: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// The date this build ran, in UTC — used only when there is no commit to
/// quote. `SOURCE_DATE_EPOCH` wins when set.
fn build_date() -> String {
    let secs = match std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        Some(pinned) => pinned,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    };
    civil_date(secs)
}

/// Epoch seconds → `YYYY-MM-DD`, UTC. Hinnant's civil-from-days.
fn civil_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// The git files whose contents decide the stamp: `HEAD`, and whatever `HEAD`
/// points at.
fn git_watch_paths() -> Vec<PathBuf> {
    let Some(git_dir) = git_raw(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else {
        return Vec::new();
    };
    // A worktree keeps its own `HEAD` beside `commondir`, while the refs it
    // names live in the main checkout's git directory.
    let common = git_raw(&["rev-parse", "--git-common-dir"])
        .map(|d| absolute(Path::new(&d)))
        .unwrap_or_else(|| git_dir.clone());

    let head = git_dir.join("HEAD");
    let mut paths = Vec::new();
    if head.is_file() {
        paths.push(head);
    }
    match git_raw(&["symbolic-ref", "-q", "HEAD"]) {
        Some(reference) => {
            let loose = common.join(&reference);
            if loose.is_file() {
                paths.push(loose);
            } else if common.join("packed-refs").is_file() {
                paths.push(common.join("packed-refs"));
            }
        }
        // Detached HEAD: the commit is written in `HEAD` itself, already
        // watched above.
        None => {}
    }
    paths
}

/// `--git-common-dir` answers relatively (`.git`) in a plain checkout and
/// absolutely from a worktree; cargo needs a path it can resolve.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
