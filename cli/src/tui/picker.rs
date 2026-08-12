//! The full-screen half of **one** picker.
//!
//! E1.S4 asks for a directory picker with "the same matcher and the same keys
//! as the popup, so there is one picker with two sizes rather than two
//! pickers". That is taken literally here:
//!
//! - the matcher is [`jod_core::rank`], the same call `@` makes;
//! - the row type is [`mention::Row`], so the same renderer highlights the
//!   matched characters in both;
//! - the keys are the popup's — type to narrow, `↑↓` to move, `⏎` to accept,
//!   `Esc` to leave without changing anything.
//!
//! What differs is the candidate list and the size of the box. Nothing else,
//! deliberately: two pickers that drift apart is how you end up with `Esc`
//! clearing your query in one of them.
//!
//! ## Why it enumerates directories itself
//!
//! [`rank::candidates`] returns files *and* folders, which is right for `@` —
//! you mention both — and wrong here, where every row must be somewhere a root
//! can point at. Telling them apart afterwards would mean a `stat` per path
//! over a list that can be a hundred thousand long. So this walks for
//! directories only, bounded, and skips the trees nobody sets a root inside.

use std::path::{Path, PathBuf};

use jod_core::rank;

use super::mention::Row;

/// How many rows the full-screen picker shows.
///
/// More than the inline popup's eight because there is room for them: this
/// owns the screen, and the reason to open the big one is to browse rather
/// than to complete a word you have half-typed.
pub const ROWS: usize = 14;

/// The most directories that will be enumerated.
///
/// A bound rather than a promise of completeness, and the screen says when it
/// bites. Unbounded, a picker opened in `/` walks the whole filesystem while
/// the user waits for a box that never arrives.
pub const MAX_DIRS: usize = 20_000;

/// How deep the walk goes.
///
/// Roots are repositories and project folders; nobody sets one eight levels
/// down inside `node_modules`. Depth is what keeps the walk cheap without a
/// per-directory judgement call.
pub const MAX_DEPTH: usize = 6;

/// Directory names never worth offering as a root.
///
/// Not a security measure — [`skip`] is about noise. A picker whose first
/// twenty matches for `src` are all inside `target/debug/build` is a picker
/// that makes you type the whole path anyway.
const NOISE: [&str; 6] = ["node_modules", "target", ".git", "dist", "build", "venv"];

/// The full-screen picker, while it is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// Where the walk started. Every row is relative to this, and accepting
    /// one joins it back on.
    pub base: PathBuf,
    pub query: String,
    pub selected: usize,
    pub rows: Vec<Row>,
    /// Every directory found under `base`, relative and unranked.
    pub entries: Vec<String>,
    /// Whether [`MAX_DIRS`] cut the walk short, so the screen can say so.
    /// A list that is quietly partial is one you trust and should not.
    pub truncated: bool,
}

impl Picker {
    pub fn new(base: PathBuf, entries: Vec<String>, truncated: bool) -> Picker {
        let mut picker = Picker {
            base,
            query: String::new(),
            selected: 0,
            rows: Vec::new(),
            entries,
            truncated,
        };
        picker.refresh();
        picker
    }

    /// Re-rank against what has been typed, keeping the highlight in range.
    ///
    /// Called on every keystroke, exactly as the popup does — the whole point
    /// of sharing a matcher is that the big picker feels like the small one.
    pub fn refresh(&mut self) {
        self.rows = rank::rank(&self.query, &self.entries, ROWS)
            .into_iter()
            .map(|hit| Row {
                // Never root-qualified: a directory picker is browsing one
                // tree, so there is no second root for a path to be ambiguous
                // between.
                label: None,
                path: self.entries[hit.index].clone(),
                positions: hit.positions,
            })
            .collect();
        if self.selected >= self.rows.len() {
            self.selected = 0;
        }
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.refresh();
    }

    pub fn pop(&mut self) {
        self.query.pop();
        self.refresh();
    }

    pub fn next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1) % self.rows.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + self.rows.len() - 1) % self.rows.len();
        }
    }

    /// The absolute directory `⏎` would add, or `None` when nothing matched.
    ///
    /// `.` is in the candidate list and stands for the base itself, so the
    /// common case — "this directory, the one I launched in" — is the first
    /// row and one keystroke, rather than something you have to find a way to
    /// express.
    pub fn chosen(&self) -> Option<PathBuf> {
        let row = self.rows.get(self.selected)?;
        Some(if row.path == "." {
            self.base.clone()
        } else {
            self.base.join(&row.path)
        })
    }
}

/// Every directory under `base`, relative to it, breadth-first.
///
/// Breadth-first on purpose: the directories somebody wants as a root are near
/// the top, so a walk cut short by [`MAX_DIRS`] loses the deep noise rather
/// than the answer. A depth-first walk would spend the whole budget inside the
/// first subtree it entered.
///
/// Unreadable directories are skipped rather than failing the walk — a picker
/// that returns nothing because one folder denied permission is worse than one
/// that quietly offers the rest.
pub fn directories(base: &Path) -> (Vec<String>, bool) {
    // The base itself, first, because "the directory I am in" is the most
    // common answer and should not need typing.
    let mut found = vec![".".to_string()];
    let mut queue: Vec<(PathBuf, usize)> = vec![(base.to_path_buf(), 0)];
    let mut truncated = false;

    while let Some((dir, depth)) = queue.first().cloned() {
        queue.remove(0);
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` rather than `metadata`: it does not follow symlinks,
            // so a link pointing at an ancestor cannot send this walk round in
            // a circle.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if skip(&name) {
                continue;
            }
            if found.len() >= MAX_DIRS {
                truncated = true;
                return (found, truncated);
            }
            if let Ok(relative) = path.strip_prefix(base) {
                found.push(relative.to_string_lossy().to_string());
            }
            queue.push((path, depth + 1));
        }
    }
    (found, truncated)
}

/// Whether a directory is noise rather than somewhere to point a root.
///
/// Hidden directories go too. `.git` is the obvious one, but `.cache`,
/// `.venv` and the rest are the same argument: a root is a place you work,
/// and none of these are.
fn skip(name: &str) -> bool {
    name.starts_with('.') || NOISE.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(entries: &[&str]) -> Picker {
        Picker::new(
            PathBuf::from("/home/reljod/repo"),
            entries.iter().map(|s| (*s).to_string()).collect(),
            false,
        )
    }

    /// One picker at two sizes: the big one ranks with the same call the popup
    /// makes, so a query behaves identically in both.
    #[test]
    fn the_rows_are_ranked_by_the_same_matcher_as_the_popup() {
        let p = picker(&[".", "Jod/cli/src", "Jod/core/src", "notes/src-archive"]);
        let mut p = p;
        p.query = "coresrc".into();
        p.refresh();
        assert_eq!(p.rows[0].path, "Jod/core/src", "{:?}", p.rows);
        assert!(
            !p.rows[0].positions.is_empty(),
            "and the matched characters travel with it"
        );
    }

    /// "The directory I am in" is the most common answer, so it is the first
    /// row and costs one keystroke.
    #[test]
    fn the_base_itself_is_the_first_offer() {
        let p = picker(&[".", "Jod", "notes"]);
        assert_eq!(p.rows[0].path, ".");
        assert_eq!(p.chosen(), Some(PathBuf::from("/home/reljod/repo")));
    }

    #[test]
    fn accepting_a_row_joins_it_back_onto_the_base() {
        let mut p = picker(&[".", "Jod/cli"]);
        p.query = "cli".into();
        p.refresh();
        assert_eq!(p.chosen(), Some(PathBuf::from("/home/reljod/repo/Jod/cli")));
    }

    #[test]
    fn nothing_matching_means_nothing_to_accept() {
        let mut p = picker(&[".", "Jod"]);
        p.query = "zzzzz".into();
        p.refresh();
        assert!(p.rows.is_empty());
        assert_eq!(p.chosen(), None);
    }

    /// Same rule as the popup: narrowing the list must pull the highlight back
    /// into it, or `⏎` accepts a row that is no longer there.
    #[test]
    fn narrowing_the_list_pulls_the_highlight_back_into_it() {
        let mut p = picker(&[".", "alpha", "beta"]);
        p.next();
        p.next();
        assert_eq!(p.selected, 2);
        p.query = "alpha".into();
        p.refresh();
        assert_eq!(p.selected, 0);
        assert_eq!(p.chosen(), Some(PathBuf::from("/home/reljod/repo/alpha")));
    }

    #[test]
    fn the_arrows_wrap_the_way_they_do_in_the_popup() {
        let mut p = picker(&[".", "a", "b"]);
        p.prev();
        assert_eq!(p.selected, 2);
        p.next();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn typing_and_backspacing_re_rank_on_every_keystroke() {
        let mut p = picker(&[".", "alpha", "beta"]);
        for c in "alph".chars() {
            p.push(c);
        }
        assert_eq!(p.rows.len(), 1);
        p.pop();
        p.pop();
        p.pop();
        p.pop();
        assert_eq!(p.query, "");
        assert_eq!(p.rows.len(), 3, "an empty query offers everything again");
    }

    /// Noise is the difference between a usable picker and one where the first
    /// twenty matches for `src` are all inside `target/debug/build`.
    #[test]
    fn the_walk_skips_the_directories_nobody_sets_a_root_inside() {
        assert!(skip("node_modules"));
        assert!(skip("target"));
        assert!(skip(".git"));
        assert!(skip(".cache"), "hidden directories are not workplaces");
        assert!(!skip("src"));
        assert!(!skip("core"));
    }

    /// The walk is over a real directory, which is a sanctioned fixture: a
    /// temporary tree built by the test, not a mock of the filesystem.
    #[test]
    fn the_walk_finds_directories_and_leaves_files_alone() {
        let base = std::env::temp_dir().join(format!("jod-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("cli/src")).expect("a fixture tree");
        std::fs::create_dir_all(base.join("target/debug")).expect("noise");
        std::fs::create_dir_all(base.join(".git/objects")).expect("more noise");
        std::fs::write(base.join("README.md"), "not a directory").expect("a file");

        let (found, truncated) = directories(&base);
        assert!(!truncated);
        assert!(found.contains(&".".to_string()), "{found:?}");
        assert!(found.contains(&"cli".to_string()), "{found:?}");
        assert!(found.contains(&"cli/src".to_string()), "{found:?}");
        assert!(
            !found.iter().any(|f| f.contains("README")),
            "files are not roots: {found:?}"
        );
        assert!(
            !found.iter().any(|f| f.starts_with("target")),
            "noise is skipped: {found:?}"
        );
        assert!(
            !found.iter().any(|f| f.starts_with(".git")),
            "and so is git's own tree: {found:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
