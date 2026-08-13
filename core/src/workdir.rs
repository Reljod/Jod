//! Where a run works, and whether what it wrote landed there.
//!
//! A delegated run is given one directory to start in and a set of roots it may
//! read. Both were decided somewhere far from here and neither was ever stated
//! to the agent, which is how "build a tetris game in the tetris directory"
//! became a project in the user's home directory while the directory they had
//! actually pointed at stayed empty — and the run still reported `done`.
//!
//! Three things live here, in the order they matter:
//!
//! 1. [`name_in_roots`] — a bare directory name resolves against the roots the
//!    conversation declared. Never against `$HOME`, and never against a guess.
//! 2. [`launch_cwd`] — the whole decision for one run: what the caller asked
//!    for, what the conversation declared, and a [`Refusal`] when the two
//!    cannot be reconciled without guessing.
//! 3. [`written_path`] / [`strayed`] — after the fact, which paths a run wrote
//!    to and whether *every one of them* landed outside the directories it was
//!    given. A run that finishes having written nothing where it was pointed is
//!    the failure this module exists for, and it is invisible from the run's own
//!    exit code.
//!
//! ## What this is not
//!
//! Not a sandbox, and nothing here becomes one. Roots are a convention — see
//! [`crate::roots`] — so this module cannot *stop* a write outside them. What it
//! can do is refuse to invent a destination nobody named, and say afterwards
//! that the work went somewhere else. Both are honesty, not enforcement.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::roots::normalise;

/// What a bare directory name turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// Exactly one declared root answers to the name.
    Found {
        /// The directory itself.
        path: PathBuf,
        /// The root it was found under, for saying *why* in a message.
        root: PathBuf,
    },
    /// Nothing declared answers to it. The caller must not guess.
    Unknown,
    /// Several do. Picking one is a coin toss with a repository on it.
    Ambiguous(Vec<PathBuf>),
}

/// Resolve a bare directory name against the directories a conversation
/// declared.
///
/// Two spellings count as an answer, and both are what a person means when they
/// say "the tetris directory":
///
/// - the root's own last component — you added `…/dogfood/tetris` and then said
///   `tetris`;
/// - an existing directory of that name *inside* a root — you added the
///   checkout and then said `apps/web`.
///
/// Existence is required for the second form and not for the first. A root is
/// declared whether or not it is on disk yet ([`crate::roots::normalise`] keeps
/// an unresolvable path as given), but "some directory under here" is only an
/// answer if there is one — otherwise every root would answer to every name and
/// the result would always be [`Named::Ambiguous`].
///
/// An absolute path is not a bare name and is not this function's business;
/// [`launch_cwd`] takes it as given.
pub fn name_in_roots(name: &Path, roots: &[PathBuf]) -> Named {
    if name.is_absolute() {
        return Named::Unknown;
    }
    let mut found: Vec<PathBuf> = Vec::new();
    let mut under: Vec<PathBuf> = Vec::new();

    for root in roots {
        // The root's own name. `file_name` rather than a string compare so
        // `tetris/` and `tetris` are one name, and a root of `/` is nobody's
        // bare name.
        if root.file_name().is_some_and(|last| Path::new(last) == name) {
            push_once(&mut found, root.clone());
            push_once(&mut under, root.clone());
            continue;
        }
        let joined = root.join(name);
        if joined.is_dir() {
            push_once(&mut found, joined);
            push_once(&mut under, root.clone());
        }
    }

    match found.len() {
        0 => Named::Unknown,
        1 => Named::Found {
            path: found.remove(0),
            root: under.remove(0),
        },
        _ => Named::Ambiguous(found),
    }
}

fn push_once(seen: &mut Vec<PathBuf>, path: PathBuf) {
    let settled = normalise(&path);
    if seen.iter().any(|p| normalise(p) == settled) {
        return;
    }
    seen.push(settled);
}

/// Why a run cannot be launched without guessing where to put it.
///
/// Carries enough to raise a card somebody can act on: what was asked for, and
/// what was on offer. A refusal that said only "could not resolve" would leave
/// the reader doing this module's job by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The name as the caller wrote it.
    pub name: String,
    /// The directories the conversation had declared, in the user's order.
    pub roots: Vec<PathBuf>,
    /// The candidates, when the name matched more than one.
    pub candidates: Vec<PathBuf>,
}

impl Refusal {
    /// One line for a card title or an error.
    pub fn title(&self) -> String {
        format!("`{}` is not one of this session's directories", self.name)
    }

    /// The whole story, for a card body or an error message.
    pub fn body(&self) -> String {
        let mut out = if self.candidates.is_empty() {
            format!(
                "This run was told to work in `{}`, which names none of the directories \
                 this session declared. Jod will not guess: resolving a bare name against \
                 your home directory is how a run's whole output lands somewhere nobody \
                 asked for, while the directory you pointed at stays empty.",
                self.name
            )
        } else {
            format!(
                "`{}` names more than one of this session's directories, and picking \
                 one of them is a guess:",
                self.name
            )
        };
        let listed = if self.candidates.is_empty() {
            &self.roots
        } else {
            &self.candidates
        };
        if listed.is_empty() {
            out.push_str("\n\nThis session has no directories at all — add one first.");
        } else {
            out.push_str("\n\n");
            for path in listed {
                out.push_str(&format!("  {}\n", path.display()));
            }
            out.push_str("\nName one of these, or add the directory you meant.");
        }
        out
    }
}

/// The directory a run starts in, or a refusal to invent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Workdir {
    At(PathBuf),
    Refused(Refusal),
}

/// Decide where one run works.
///
/// `requested` is whatever the caller put on the request; `roots` is what the
/// conversation declared, in the user's order. The rules, in order:
///
/// - An absolute path is a decision somebody made. Taken as given — roots are
///   not a sandbox and this is not the place to start pretending otherwise.
/// - `.` or nothing, with roots declared, means the first root. That is already
///   the rule for an unqualified `@mention` ([`crate::roots::Root::position`]),
///   and two different answers to "which directory did you mean" would be worse
///   than either.
/// - A bare name resolves through [`name_in_roots`].
///   - Matched: that directory.
///   - Matched nothing, or several, **with roots declared**: [`Workdir::Refused`].
///     This is the whole point. The alternative is a guess, and the guess this
///     replaced was `$HOME`.
///   - With no roots declared at all there is nothing to resolve against, so it
///     falls back to the directory the caller is standing in — never `$HOME`,
///     which is the one answer that is wrong whoever asked.
pub fn launch_cwd(requested: &Path, roots: &[PathBuf]) -> Workdir {
    if requested.is_absolute() {
        return Workdir::At(requested.to_path_buf());
    }
    let bare = requested.as_os_str().is_empty() || requested == Path::new(".");
    if bare {
        return match roots.first() {
            Some(first) => Workdir::At(first.clone()),
            None => Workdir::At(requested.to_path_buf()),
        };
    }
    match name_in_roots(requested, roots) {
        Named::Found { path, .. } => Workdir::At(path),
        Named::Unknown if roots.is_empty() => Workdir::At(
            std::env::current_dir()
                .map(|here| here.join(requested))
                .unwrap_or_else(|_| requested.to_path_buf()),
        ),
        Named::Unknown => Workdir::Refused(Refusal {
            name: requested.to_string_lossy().to_string(),
            roots: roots.to_vec(),
            candidates: Vec::new(),
        }),
        Named::Ambiguous(candidates) => Workdir::Refused(Refusal {
            name: requested.to_string_lossy().to_string(),
            roots: roots.to_vec(),
            candidates,
        }),
    }
}

// ---- what a run wrote --------------------------------------------------

/// Tool names that mean "this call put bytes on disk".
///
/// Matched after lowercasing and dropping `_` and `-`, so `NotebookEdit`,
/// `notebook_edit` and `notebook-edit` are one name across three harnesses.
///
/// Deliberately a list rather than a substring rule. A false *positive* here is
/// the dangerous direction: one misread read-tool inside the workspace would
/// suppress the warning about every real write outside it.
const WRITERS: &[&str] = &[
    "write",
    "writefile",
    "edit",
    "multiedit",
    "notebookedit",
    "editfile",
    "create",
    "createfile",
    "applypatch",
    "patch",
    "strreplace",
    "strreplaceeditor",
];

/// Keys a harness puts a written path under.
///
/// Same spellings the transcript's diff view already has to know about
/// (`cli/src/tui/diff.rs`), because they come from the same three harnesses.
const PATH_KEYS: &[&str] = &[
    "filepath",
    "path",
    "targetfile",
    "notebookpath",
    "filename",
    "file",
];

fn squash(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The path a tool call wrote to, if this call wrote to one.
///
/// `None` for everything else, including `Bash` — a shell command writes
/// wherever it likes and says nothing about it in its arguments. That is a real
/// gap and it is left open rather than papered over: the run that prompted this
/// module created a `node_modules/` tree in `$HOME` through `pnpm install`, and
/// no argument inspection would have caught it. What catches that is the
/// working directory being right in the first place.
pub fn written_path(tool: &str, input: Option<&Value>) -> Option<PathBuf> {
    if !WRITERS.contains(&squash(tool).as_str()) {
        return None;
    }
    let input = input?.as_object()?;
    for (key, value) in input {
        if !PATH_KEYS.contains(&squash(key).as_str()) {
            continue;
        }
        if let Some(text) = value.as_str() {
            if !text.trim().is_empty() {
                return Some(PathBuf::from(text));
            }
        }
    }
    None
}

/// Whether `path` lies inside `dir`.
///
/// Both sides normalised, because a run's own working directory reaches us
/// through a plan on disk and the paths it wrote reach us through a harness's
/// JSON — one of them can easily be the symlinked spelling of the other, and
/// `/var` vs `/private/var` on macOS would otherwise read as "outside".
///
/// Component-wise, not by string prefix: `/tmp/repo-old` is not inside
/// `/tmp/repo`.
pub fn inside(path: &Path, dir: &Path) -> bool {
    normalise(path).starts_with(normalise(dir))
}

/// Every write that landed outside `workspace`, when **none** landed inside it.
///
/// `None` — say nothing — when the run wrote nothing this module can see, or
/// when even one write landed where it was supposed to. A run that touched both
/// is a run that found its way, and a warning about the rest of it would be
/// noise that trains people to ignore the useful one.
///
/// `Some` is the case the report calls critical: the run finished, the record
/// says `done`, and not one byte reached any directory the user named.
pub fn strayed(written: &[PathBuf], workspace: &[PathBuf]) -> Option<Vec<PathBuf>> {
    if written.is_empty() || workspace.is_empty() {
        return None;
    }
    let mut outside = Vec::new();
    for path in written {
        if workspace.iter().any(|dir| inside(path, dir)) {
            return None;
        }
        push_once(&mut outside, path.clone());
    }
    Some(outside)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory tree, canonicalised — on macOS `std::env::temp_dir()`
    /// is a symlink, and half of what this module does is compare paths.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jod-workdir-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        normalise(&dir)
    }

    /// BUG-14, at the smallest scale it can be stated: the user declared
    /// `…/dogfood/tetris` and then said "the tetris directory". The answer is
    /// the root. It is never `$HOME/tetris`, and `$HOME` is not consulted to
    /// find that out.
    #[test]
    fn a_bare_name_that_is_a_declared_root_resolves_to_that_root() {
        let dir = scratch("root-name");
        let root = dir.join("dogfood").join("tetris");
        std::fs::create_dir_all(&root).unwrap();

        let resolved = launch_cwd(Path::new("tetris"), std::slice::from_ref(&root));

        assert_eq!(resolved, Workdir::At(root));
    }

    #[test]
    fn a_bare_name_inside_a_declared_root_resolves_under_it() {
        let dir = scratch("child");
        let root = dir.join("checkout");
        std::fs::create_dir_all(root.join("apps").join("web")).unwrap();

        let resolved = launch_cwd(Path::new("apps/web"), std::slice::from_ref(&root));

        assert_eq!(resolved, Workdir::At(root.join("apps").join("web")));
    }

    /// The heart of it. The directory does not exist under any root, so there
    /// is no honest answer — and the dishonest one that shipped was
    /// `$HOME/<name>`.
    #[test]
    fn a_bare_name_matching_no_declared_root_is_refused_rather_than_guessed() {
        let dir = scratch("no-match");
        let root = dir.join("dogfood");
        std::fs::create_dir_all(&root).unwrap();

        let resolved = launch_cwd(Path::new("tetris"), std::slice::from_ref(&root));

        let Workdir::Refused(refusal) = resolved else {
            panic!("a name nothing answers to must be refused, got {resolved:?}");
        };
        assert_eq!(refusal.name, "tetris");
        assert!(
            refusal.body().contains(&root.display().to_string()),
            "a refusal has to say what was on offer: {}",
            refusal.body()
        );
    }

    /// Not "it did not return `$HOME`" by accident — the home directory is on
    /// offer as a root here, and a `tetris` under it exists. Resolution still
    /// has to answer with the root that is actually named `tetris`.
    #[test]
    fn a_home_directory_holding_a_directory_of_that_name_does_not_win() {
        let dir = scratch("home-decoy");
        let home = dir.join("home");
        let declared = dir.join("dogfood").join("tetris");
        std::fs::create_dir_all(home.join("tetris")).unwrap();
        std::fs::create_dir_all(&declared).unwrap();

        // The home directory declared *second*: the name matches the first
        // root's own last component and a real directory under the second.
        let resolved = launch_cwd(Path::new("tetris"), &[declared.clone(), home.clone()]);

        let Workdir::Refused(refusal) = resolved else {
            panic!("two directories answer to this name; that is a guess, got {resolved:?}");
        };
        assert_eq!(refusal.candidates.len(), 2, "{refusal:?}");
        assert!(
            refusal.candidates.contains(&declared),
            "the refusal must name both, so a person can pick: {refusal:?}"
        );
    }

    #[test]
    fn an_absolute_path_is_taken_as_given() {
        let dir = scratch("absolute");
        assert_eq!(
            launch_cwd(&dir.join("anywhere"), std::slice::from_ref(&dir)),
            Workdir::At(dir.join("anywhere")),
            "roots are not a sandbox, and an absolute path is somebody's decision"
        );
    }

    #[test]
    fn nothing_named_with_roots_declared_means_the_first_root() {
        let a = PathBuf::from("/tmp/one");
        let b = PathBuf::from("/tmp/two");
        assert_eq!(
            launch_cwd(Path::new("."), &[a.clone(), b]),
            Workdir::At(a),
            "the first root is already what an unqualified mention resolves against"
        );
    }

    /// With nothing declared there is nothing to resolve against, and the
    /// directory the person is standing in is the only non-guess available.
    #[test]
    fn with_no_roots_a_bare_name_falls_back_to_where_the_caller_is() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            launch_cwd(Path::new("tetris"), &[]),
            Workdir::At(here.join("tetris"))
        );
    }

    #[test]
    fn a_sibling_with_a_longer_name_is_not_inside_a_directory() {
        assert!(!inside(
            Path::new("/tmp/repo-old/src/main.rs"),
            Path::new("/tmp/repo")
        ));
        assert!(inside(
            Path::new("/tmp/repo/src/main.rs"),
            Path::new("/tmp/repo")
        ));
    }

    #[test]
    fn a_write_tool_gives_up_its_path_whatever_the_harness_calls_it() {
        let claude = serde_json::json!({"file_path": "/x/a.rs", "content": "…"});
        assert_eq!(
            written_path("Write", Some(&claude)),
            Some(PathBuf::from("/x/a.rs"))
        );
        let agy = serde_json::json!({"TargetFile": "/x/b.rs"});
        assert_eq!(
            written_path("edit_file", Some(&agy)),
            Some(PathBuf::from("/x/b.rs"))
        );
        let opencode = serde_json::json!({"filePath": "/x/c.rs"});
        assert_eq!(
            written_path("write", Some(&opencode)),
            Some(PathBuf::from("/x/c.rs"))
        );
    }

    /// A read is not a write, and a shell command is a write nobody can see.
    /// Both must stay out: a phantom write *inside* the workspace would silence
    /// the warning about every real one outside it.
    #[test]
    fn reading_and_running_are_not_writing() {
        let input = serde_json::json!({"file_path": "/x/a.rs"});
        assert_eq!(written_path("Read", Some(&input)), None);
        assert_eq!(written_path("Grep", Some(&input)), None);
        assert_eq!(
            written_path("Bash", Some(&serde_json::json!({"command": "pnpm install"}))),
            None
        );
    }

    /// The run under BUG-14: every file it wrote went to `$HOME/tetris`, and
    /// the directory it was given stayed empty.
    #[test]
    fn writes_that_all_land_outside_the_workspace_are_reported() {
        let strays = strayed(
            &[
                PathBuf::from("/home/x/tetris/index.html"),
                PathBuf::from("/home/x/tetris/src/main.js"),
            ],
            &[PathBuf::from("/work/dogfood/tetris")],
        );
        assert_eq!(
            strays,
            Some(vec![
                PathBuf::from("/home/x/tetris/index.html"),
                PathBuf::from("/home/x/tetris/src/main.js"),
            ])
        );
    }

    #[test]
    fn a_run_that_wrote_anything_where_it_was_pointed_is_left_alone() {
        assert_eq!(
            strayed(
                &[
                    PathBuf::from("/work/repo/README.md"),
                    PathBuf::from("/tmp/scratch.txt"),
                ],
                &[PathBuf::from("/work/repo")]
            ),
            None,
            "a run that found its way is not worth a card about its scratch files"
        );
    }

    #[test]
    fn a_run_that_wrote_nothing_says_nothing() {
        assert_eq!(strayed(&[], &[PathBuf::from("/work/repo")]), None);
        assert_eq!(
            strayed(&[PathBuf::from("/anywhere")], &[]),
            None,
            "with no workspace declared there is nothing to be outside of"
        );
    }
}
