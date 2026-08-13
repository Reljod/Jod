//! Stamps the commit this binary was built from into `jod --version`.
//!
//! Why: `~/.local/bin/jod` is a copy, so a rebuilt checkout leaves a stale
//! binary on `$PATH`, and both answer `jod 0.1.0`. They are not the same
//! program — one of them was missing seven subcommands — and nothing the
//! program printed revealed it. A version has to name its source.
//!
//! Why the *commit's* date and not the wall clock: a script that stamped
//! `now()` would have to rerun on every `cargo build` to stay truthful,
//! recompiling the largest crate in the workspace each time — and a fleet of
//! agents builds this workspace in parallel, one `target/` each. If it did
//! *not* rerun, cargo would replay the previous value and the binary would
//! confidently report the time of an older build. The commit date is stable
//! for a given commit, so a rebuild of the same tree stays cache-hot and the
//! stamp stays honest. Uncommitted work is reported by `-dirty` instead.
//!
//! Nothing here may fail the build. A tarball with no `.git` (this repo makes
//! one itself — `tests/e2e/jod/build.sh` builds from `git archive`) and a box
//! with no `git` on `PATH` both have to compile; they get `unknown` and the
//! build date.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let (id, date) = stamp();
    println!("cargo:rustc-env=JOD_BUILD_ID={id}");
    println!("cargo:rustc-env=JOD_BUILD_DATE={date}");

    // The platform triple this build is for, which is what `jod upgrade` needs
    // to know which `jod-<target>.tar.gz` the release holds for it. Cargo sets
    // TARGET for every build script; asking the host with `uname` instead
    // would answer for the kernel rather than for this binary, and send a
    // cross-built or Rosetta'd copy to a tarball it cannot run.
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=JOD_BUILD_TARGET={target}");
    println!("cargo:rerun-if-env-changed=TARGET");

    // Emitting any `rerun-if-changed` replaces cargo's default of "rerun when
    // any file in the package changed", so `src` is re-stated here — it is
    // what `-dirty` is mostly about, and a source edit already recompiles this
    // crate, so the rerun costs nothing extra.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    for path in git_watch_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    // Reproducible builds set this; a change to it changes the fallback date.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

/// `(build id, date)` — the commit and its date, or the honest admission that
/// this build could not tell.
fn stamp() -> (String, String) {
    match git(&["describe", "--always", "--dirty", "--abbrev=7", "--exclude=*"]) {
        // `--exclude=*` matches no tag on purpose: `--always` then falls back
        // to the abbreviated commit, so the id reads `f4e4c72` rather than
        // `v0.1.0-359-gf4e4c72`, which repeats the release number already in
        // the version string.
        Some(id) => {
            let date = git(&["log", "-1", "--date=short", "--format=%cd"]).unwrap_or_else(build_date);
            (id, date)
        }
        None => ("unknown".to_string(), build_date()),
    }
}

/// A git command's single-line output, reduced to characters that are safe to
/// stamp — or `None` for any reason at all: no git, no repository, a git too
/// old for one of these flags, empty output.
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
/// quote. `SOURCE_DATE_EPOCH` wins when set, which is how a reproducible
/// build pins it.
fn build_date() -> String {
    let secs = match std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(pinned) => pinned,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    };
    civil_date(secs)
}

/// Epoch seconds → `YYYY-MM-DD`, UTC. Hinnant's civil-from-days: exact, and
/// cheaper than pulling a date crate into the build graph for one line of
/// fallback text.
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
/// points at. Watching these — rather than the whole `.git` — is what keeps a
/// commit or a checkout from being missed without making every `git status`
/// rebuild the crate.
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
        // A loose ref is a file that moves on every commit; a packed one lives
        // in `packed-refs` and moves when git repacks.
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
    std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
}
