//! Which directories a *running* harness process was allowed to write in.
//!
//! ## Why this cannot be answered by writing a file
//!
//! The obvious way to find out whether a path is writable is to write to it.
//! That answer is worthless here, and measurably so. Jod cuts the worktree
//! itself, with `git worktree add`, from its own process; a probe from that
//! process therefore establishes that **Jod** can write there, which is a
//! question nobody asked. The process that has to write is the harness, it runs
//! under the harness's own directory rules, and those rules are decided from
//! the command line Jod built for it — once, at launch.
//!
//! Measured on the run that prompted this module. Its harness could not write
//! the worktree at all, and a probe from a Jod-shaped process on that exact
//! directory succeeded on the first try. A probe would have reported "writable"
//! about a directory in which the session had just been refused.
//!
//! ## What can be answered
//!
//! [`crate::runner::SpawnPlan`] is written to `runs/<id>/spawn.json` before the
//! supervisor starts anything, and it holds the literal argument list and
//! working directory the process was launched with. That is not a reconstruction
//! of what the process probably got — it is the record of what it did get. So
//! the question "may this session write here" becomes a question about that
//! file, and it can be answered without running anything.
//!
//! Reading the flags back is the job of this module rather than of each adapter
//! because there are only two spellings across all three harnesses, and one
//! place that knows both stays in step more reliably than three that each know
//! one. [`granted_at_launch`] is pinned to the adapters by a test per harness
//! that builds a real request, runs it through [`crate::harness::Harness::args`]
//! and asserts the directories come back out — so an adapter that changes its
//! flag fails here rather than starting to lie.

use std::path::{Path, PathBuf};

use crate::roots;

/// The directories a launched process may write in, as its command line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Every directory the process was handed, plus the one it was started in.
    ///
    /// The working directory belongs here because every harness treats it as
    /// granted whether or not it is repeated as a flag — Claude Code and
    /// OpenCode implicitly, AGY because `agy.rs` passes it explicitly for
    /// exactly this reason.
    pub dirs: Vec<PathBuf>,
    /// Whether the harness is checking `dirs` at all.
    ///
    /// False for a run launched in a mode that skips the directory rules
    /// wholesale. Such a run can write anywhere the filesystem allows, so a
    /// path outside `dirs` is not a problem for it and must not be reported as
    /// one. Crying wolf on every claim would be as useless as never crying.
    pub enforced: bool,
}

impl Grant {
    /// Whether `path` is inside one of the granted directories.
    ///
    /// Both sides are normalised first. A worktree under `/tmp` on this box
    /// arrives as `/tmp/...` from the lease and `/private/tmp/...` or a
    /// symlinked variant from the plan often enough that comparing the strings
    /// as written would answer "outside" for a directory that is plainly
    /// inside.
    pub fn covers(&self, path: &Path) -> bool {
        let path = roots::normalise(path);
        self.dirs
            .iter()
            .any(|dir| path == *dir || path.starts_with(dir))
    }
}

/// Directory flags, by the two spellings the adapters actually emit.
///
/// `--add-dir` is Claude Code's and AGY's; `--dir` is OpenCode's only one.
const DIR_FLAGS: [&str; 2] = ["--add-dir", "--dir"];

/// The flag that turns the whole directory question off.
///
/// Claude Code's `Bypass` arm. A run carrying this is not confined by `--add-dir`
/// or by anything else the harness would otherwise check.
const UNCONFINED_FLAG: &str = "--dangerously-skip-permissions";

/// Read a launched process's writable directories back off its command line.
///
/// `args` is [`crate::runner::SpawnPlan::args`] — resolved, exactly as it was
/// handed to the supervisor — and `cwd` is the directory the process was
/// started in.
///
/// Note that `--add-dir` is variadic on both harnesses that take it: `claude
/// --help` spells it `--add-dir <directories...>`, so the flag keeps swallowing
/// words until it meets another flag. The adapters emit one directory per flag
/// today, but reading only the next word would quietly under-report a grant the
/// moment that changed, and under-reporting here means telling a session it
/// cannot write somewhere it can. So every following non-flag word is taken.
pub fn granted_at_launch(args: &[String], cwd: &Path) -> Grant {
    let mut dirs = vec![roots::normalise(cwd)];
    let mut enforced = true;
    // Peekable, not a plain iterator. The inner loop has to look at the word
    // that ends a run of directories *without* eating it: the flag that follows
    // a `--add-dir` is a flag the outer loop still has to see. Consuming it cost
    // the bypass case, where the real command line is `--add-dir /repo
    // --dangerously-skip-permissions` and the directory loop swallowed the very
    // flag that says the grant is not enforced.
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        if arg == UNCONFINED_FLAG {
            enforced = false;
            continue;
        }
        if !DIR_FLAGS.contains(&arg.as_str()) {
            continue;
        }
        while let Some(word) = rest.peek() {
            if word.starts_with("--") {
                break;
            }
            let dir = roots::normalise(Path::new(word));
            // Kept in the order they were granted, so the message that names
            // them reads the way the command line did. `dedup` alone would only
            // catch neighbours, and the working directory is usually repeated
            // as a flag several places later.
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
            rest.next();
        }
    }
    Grant { dirs, enforced }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Agy, ArgPart, ClaudeCode, Harness, PermissionPolicy, SpawnRequest};

    /// The same resolution the runner does before it writes the plan, so these
    /// tests read the argument list a real process would have been given.
    fn flat(parts: &[ArgPart], prompt: &str) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                ArgPart::Literal(s) => s.clone(),
                ArgPart::Prompt => prompt.to_string(),
            })
            .collect()
    }

    fn req(roots: &[&str], permission: PermissionPolicy) -> SpawnRequest {
        SpawnRequest {
            prompt: "do the thing".into(),
            cwd: PathBuf::from("/repo"),
            roots: roots.iter().map(PathBuf::from).collect(),
            permission,
            ..SpawnRequest::default()
        }
    }

    /// **The test that keeps this module honest.** It does not assert against a
    /// hand-written argument list; it runs the adapter that builds the real one
    /// and asserts the directories survive the round trip. An adapter that
    /// renames its flag, or stops passing roots, fails here — which is the
    /// failure worth having, because the alternative is this module confidently
    /// reporting a grant that no longer matches the process.
    #[test]
    fn every_root_an_adapter_grants_is_read_back_out_of_its_arguments() {
        let request = req(&["/repo", "/repo/extra"], PermissionPolicy::AcceptEdits);
        for harness in [
            Box::new(ClaudeCode::default()) as Box<dyn Harness>,
            Box::new(Agy::default()) as Box<dyn Harness>,
        ] {
            let args = flat(&harness.args(&request), "do the thing");
            let grant = granted_at_launch(&args, &request.cwd);
            assert!(grant.enforced, "{:?} under acceptEdits", harness.kind());
            for root in &request.roots {
                assert!(
                    grant.covers(root),
                    "{:?} granted {} and it did not read back: {:?}",
                    harness.kind(),
                    root.display(),
                    grant.dirs,
                );
            }
        }
    }

    /// The whole point of `enforced`. A bypass run writes wherever it likes, so
    /// a path outside its grant is not a finding about that run.
    #[test]
    fn a_bypass_run_is_not_confined_by_the_directories_it_was_given() {
        let request = req(&["/repo"], PermissionPolicy::Bypass);
        let args = flat(&ClaudeCode::default().args(&request), "do the thing");
        assert!(!granted_at_launch(&args, &request.cwd).enforced, "{args:?}");
    }

    /// The shape of the run that prompted all of this: the checkout is granted,
    /// and the worktree Jod cuts afterwards lives somewhere else entirely.
    #[test]
    fn a_worktree_outside_every_granted_directory_is_not_covered() {
        let args = vec![
            "--add-dir".to_string(),
            "/tmp/scratch-repo".to_string(),
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
        ];
        let grant = granted_at_launch(&args, Path::new("/tmp/scratch-repo"));
        assert!(grant.covers(Path::new("/tmp/scratch-repo/src")));
        assert!(!grant.covers(Path::new("/tmp/jodhome/worktrees/w/scratch-repo")));
    }

    /// `--add-dir` is variadic, so a second directory that follows the first
    /// without repeating the flag is still granted. Reading only the next word
    /// would drop it and tell a session it cannot write somewhere it can.
    #[test]
    fn a_variadic_directory_flag_grants_every_word_that_follows_it() {
        let args = vec![
            "--add-dir".to_string(),
            "/a".to_string(),
            "/b".to_string(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        let grant = granted_at_launch(&args, Path::new("/repo"));
        assert!(grant.covers(Path::new("/a")), "{:?}", grant.dirs);
        assert!(grant.covers(Path::new("/b")), "{:?}", grant.dirs);
        assert!(!grant.covers(Path::new("/opus")), "{:?}", grant.dirs);
    }
}
