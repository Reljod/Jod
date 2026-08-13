//! `@` — the inline file picker that opens under the cursor.
//!
//! Decision D1's UX bar, restated as the four things this module owes: results
//! **ranked** rather than merely filtered, the matched characters **highlighted**
//! in every row, re-ranked live on every keystroke, and `Esc` leaving what you
//! typed exactly alone.
//!
//! The matching itself is [`jod_core::rank`], which is deliberately not here:
//! ranking is logic and needs no terminal, so it is tested without one. What
//! this module owns is the *popup* — where it opens, what it replaces when a
//! row is accepted, and what it says when there is nothing to search.
//!
//! ## With no roots it says so and accepts nothing
//!
//! Spelled out in the spec because the alternative is worse than useless: a
//! picker that quietly searched the process's own working directory would offer
//! paths the agent has not been given access to, and inserting one would
//! produce a mention that resolves to nothing at the far end. So zero roots is
//! a *message*, not an empty list, and `⏎` on it does nothing at all.
//!
//! ## Roots overlap, and the list has to survive it
//!
//! A conversation collects roots over its whole life — `/add-dir`, a claimed
//! worktree, and the launch directory every `jod tui` hands to the conversation
//! on screen — and nothing makes them disjoint. The pinned main chat on this
//! machine holds `~/repo`, a jobs scratch directory and `~/repo/contra-dogfood`
//! at once, the first of which *contains* the third. Two consequences, both of
//! which people met on screen:
//!
//! - every file of a nested root is enumerated twice, once under each prefix,
//!   so the popup offered the same file on disk as two rows; and
//! - roots are ordered by when they were added, so a repository added months
//!   ago sorted above the one the console is standing in — `@` opened onto a
//!   stranger's files.
//!
//! So the merge below canonicalises before it dedupes, and treats *being under
//! the launch directory* as a ranking signal in its own right.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jod_core::rank;
use jod_core::roots::{normalise, Root};

/// How many rows the popup shows.
///
/// Eight, because the popup floats over the transcript and a taller one hides
/// the thing you were reading in order to help you write about it.
pub const ROWS: usize = 8;

/// What a narrow terminal, or a conversation with nothing set, is told.
/// Named as the command you can run from where you are standing. The popup is
/// open, the cursor is in the chat box, and `jod root add` in another terminal
/// is not a next step anybody takes from here.
pub const NO_ROOTS: &str = "no folder to search — /add-dir picks one (Ctrl-P)";

/// One offered path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Which root it came from, or `None` when the conversation has exactly
    /// one and qualifying every path would be noise.
    pub label: Option<String>,
    /// The path relative to its root, which is what gets inserted.
    pub path: String,
    /// Byte offsets in `path` the query matched, ascending.
    ///
    /// Kept beside the path rather than folded into a pre-rendered string so
    /// the renderer can highlight them — a fuzzy list you cannot read the match
    /// in is a list you stop trusting, which is the whole reason
    /// [`rank::Match`] carries positions at all.
    pub positions: Vec<usize>,
}

impl Row {
    /// What `⏎` puts into the line, after the `@`.
    ///
    /// Root-qualified when several roots are set, because `@src/main.rs` names
    /// two different files when a conversation can see two repositories.
    pub fn insertion(&self) -> String {
        match &self.label {
            Some(label) => format!("{label}/{}", self.path),
            None => self.path.clone(),
        }
    }
}

/// The popup, while it is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// Byte index of the `@` that opened it, in `App::input`.
    pub at: usize,
    /// What has been typed since, which is the query.
    pub query: String,
    pub selected: usize,
    pub rows: Vec<Row>,
    /// Whether the conversation has any root at all. `false` is not an empty
    /// result — it is a different message, and one `⏎` must refuse.
    pub rooted: bool,
}

impl Mention {
    /// Open a popup for the `@` at `at`.
    pub fn new(at: usize) -> Mention {
        Mention {
            at,
            query: String::new(),
            selected: 0,
            rows: Vec::new(),
            rooted: false,
        }
    }

    /// Whether `⏎` has something to accept. False with no roots, which is the
    /// spec's "accepts nothing" spelled as a predicate.
    pub fn acceptable(&self) -> Option<&Row> {
        if !self.rooted {
            return None;
        }
        self.rows.get(self.selected)
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

    /// Re-rank against what has been typed, keeping the highlight in range.
    ///
    /// `cwd` is the directory the console was launched in — the repository the
    /// person typing is standing in, which is not the same question as which
    /// roots the conversation has collected. Empty means a session standing
    /// nowhere, and then nothing is local to it.
    pub fn refresh(&mut self, cwd: &Path, roots: &[Root], candidates: &[Arc<Vec<String>>]) {
        self.rooted = !roots.is_empty();
        self.rows = rank_roots(&self.query, cwd, roots, candidates);
        if self.selected >= self.rows.len() {
            self.selected = 0;
        }
    }

    /// The byte range in the input that accepting replaces: the `@` and
    /// everything typed after it.
    pub fn span(&self) -> std::ops::Range<usize> {
        self.at..self.at + '@'.len_utf8() + self.query.len()
    }
}

/// What a candidate is worth for lying inside the launch directory.
///
/// Sized against core's own scale rather than picked round: larger than the
/// biggest placement bonus core awards a single character — a match starting at
/// a path segment boundary, which counts double — and smaller than its filename
/// bonus. So standing in a repository outranks *where in the path* a match
/// happens to land, and does not outrank a file you named outright somewhere
/// else. A hard partition was the other option and is worse: it would let eight
/// mediocre local matches hide the exact file you typed the name of, in a root
/// you added for exactly that reason.
const BONUS_LOCAL: i32 = 30;

/// Rank every root's candidates against one query and interleave the results.
///
/// [`rank::rank`] takes one slice and a conversation has several roots, so this
/// calls it once per root and merges. Ordering *within* a root is core's
/// decision and there is one place for it — if two files in one repository come
/// back in the wrong order, that is wrong in `rank.rs`, and correcting it here
/// would hide it from the tests that specify it.
///
/// What the merge adds is the one thing core cannot know, because core ranks a
/// bare list of strings and has never heard of a root: **where the path is**.
/// Two roots that overlap enumerate one file twice, and roots arrive in the
/// order they were added rather than in order of relevance, so the merge
/// canonicalises and applies [`BONUS_LOCAL`] — see the module note.
///
/// Merging rather than concatenating, because a second root is not a second
/// list — it is more of the same list. Appending would put every path from the
/// first repository above the exact match in the second.
///
/// The alternative — concatenate every root's paths and call `rank` once — is
/// not open: `rank` needs a contiguous `&[String]`, and building one would copy
/// a hundred thousand paths on every keystroke, which is the stall
/// [`rank::candidates_shared`] hands back an `Arc` to avoid.
pub fn rank_roots(
    query: &str,
    cwd: &Path,
    roots: &[Root],
    candidates: &[Arc<Vec<String>>],
) -> Vec<Row> {
    // Several roots means every path has to say which one it is from; one root
    // means qualifying is noise. Decided here rather than per row so a list of
    // rows cannot come out half-qualified.
    let qualify = roots.len() > 1;
    // Resolved once per keystroke, not once per row. Guarded on empty because
    // `Path::starts_with("")` is true of every path, and a fixture standing
    // nowhere would otherwise find the whole world local to it.
    let here = (!cwd.as_os_str().is_empty()).then(|| normalise(cwd));
    let mut scored: Vec<(i32, PathBuf, Row)> = Vec::new();
    for (root, paths) in roots.iter().zip(candidates) {
        for hit in rank::rank(query, paths, ROWS) {
            // Indexed rather than `get`: `Match::index` is an index into the
            // slice that was just passed in, and the slice has not moved
            // between the two lines.
            let path = paths[hit.index].clone();
            // The path as offered, before symlinks are resolved: locality is
            // about the mention the user is about to insert, and a root that
            // *contains* the launch directory offers both local and foreign
            // paths — `~/repo` holds this checkout and a stranger's alike — so
            // it can only be decided per row.
            let offered = root.path.join(&path);
            let local = here.as_ref().is_some_and(|here| offered.starts_with(here));
            scored.push((
                hit.score + if local { BONUS_LOCAL } else { 0 },
                // And the identity of the file *on disk*, which is what two
                // rows have to agree on to be one row. A few dozen resolutions
                // per keystroke, against the several hundred thousand string
                // comparisons core has just done.
                normalise(&offered),
                Row {
                    label: qualify.then(|| root.label()),
                    path,
                    positions: hit.positions,
                },
            ));
        }
    }
    // Stable, so an empty query — where core returns every candidate at score
    // zero, in input order — keeps the roots in the user's own order rather
    // than shuffling them per keystroke.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    // Deduped after the sort and before the truncation: the row that survives
    // is the best-placed spelling of the file, and eight rows means eight
    // *files* rather than eight entries of which two are the same one.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    scored
        .into_iter()
        .filter(|(_, real, _)| seen.insert(real.clone()))
        .take(ROWS)
        .map(|(_, _, row)| row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::roots::Origin;
    use std::path::PathBuf;

    fn root(name: &str) -> Root {
        Root {
            id: 0,
            conversation_id: "c".into(),
            path: PathBuf::from(format!("/home/reljod/repo/{name}")),
            writable: false,
            position: 0,
            origin: Origin::Human,
            added_at_ms: 0,
        }
    }

    fn paths(entries: &[&str]) -> Arc<Vec<String>> {
        Arc::new(entries.iter().map(|s| (*s).to_string()).collect())
    }

    /// A session standing nowhere, which is what a fixture that is not about
    /// locality is: no row is local to it, so the ordering is core's alone.
    fn nowhere() -> &'static Path {
        Path::new("")
    }

    /// D1's bar: ranked, not filtered. A deep exact path beats a coincidence of
    /// the same letters scattered through a longer one.
    #[test]
    fn the_rows_come_back_ranked_rather_than_in_list_order() {
        let roots = [root("jod")];
        let candidates = [paths(&[
            "cli/src/tui/mod.rs",
            "core/src/rank.rs",
            "docs/harness-config.md",
        ])];
        let rows = rank_roots("rank", nowhere(), &roots, &candidates);
        assert_eq!(rows[0].path, "core/src/rank.rs", "{rows:?}");
    }

    /// Without positions the popup can filter but cannot show *why* a row
    /// matched, and that is the difference between fzf and a `grep`.
    #[test]
    fn every_row_says_which_of_its_characters_matched() {
        let roots = [root("jod")];
        let candidates = [paths(&["core/src/rank.rs"])];
        let rows = rank_roots("rank", nowhere(), &roots, &candidates);
        let matched: String = rows[0]
            .positions
            .iter()
            .map(|at| rows[0].path[*at..].chars().next().unwrap())
            .collect();
        assert_eq!(matched, "rank");
    }

    /// Two roots is more of one list, not two lists — so the better match wins
    /// wherever it came from.
    #[test]
    fn a_better_match_in_the_second_root_outranks_a_worse_one_in_the_first() {
        let roots = [root("jod"), root("notes")];
        let candidates = [
            paths(&["docs/r-a-n-d-o-m/k.md"]),
            paths(&["rank.md"]),
        ];
        let rows = rank_roots("rank", nowhere(), &roots, &candidates);
        assert_eq!(rows[0].path, "rank.md", "{rows:?}");
    }

    /// `@src/main.rs` names two different files when a conversation can see two
    /// repositories, so the insertion has to say which.
    #[test]
    fn several_roots_qualify_the_path_and_one_root_does_not() {
        let candidates = [paths(&["src/main.rs"]), paths(&["src/main.rs"])];
        let two = rank_roots("main", nowhere(), &[root("jod"), root("notes")], &candidates);
        assert_eq!(two[0].insertion(), "jod/src/main.rs");

        let one = rank_roots("main", nowhere(), &[root("jod")], &candidates[..1]);
        assert_eq!(one[0].insertion(), "src/main.rs", "one root needs no prefix");
    }

    /// The spec's own words: with zero roots it says so and accepts nothing.
    /// An empty list would look like "no matches", and the next keystroke would
    /// look like it might help.
    #[test]
    fn with_no_roots_there_is_nothing_to_accept() {
        let mut popup = Mention::new(0);
        popup.query = "main".into();
        popup.refresh(nowhere(), &[], &[]);
        assert!(!popup.rooted);
        assert!(popup.acceptable().is_none());
        assert!(popup.rows.is_empty());
    }

    /// The popup opens on `@` before anything has been typed, so the empty
    /// query is a state it lives in rather than an edge case.
    #[test]
    fn an_empty_query_offers_the_first_candidates_rather_than_nothing() {
        let mut popup = Mention::new(0);
        popup.refresh(nowhere(), &[root("jod")], &[paths(&["a.rs", "b.rs"])]);
        assert_eq!(popup.rows.len(), 2);
        assert!(popup.acceptable().is_some());
    }

    #[test]
    fn the_popup_shows_at_most_its_own_height() {
        let many: Vec<String> = (0..40).map(|n| format!("file{n}.rs")).collect();
        let rows = rank_roots("file", nowhere(), &[root("jod")], &[Arc::new(many)]);
        assert_eq!(rows.len(), ROWS);
    }

    #[test]
    fn the_arrows_wrap_and_never_point_past_the_list() {
        let mut popup = Mention::new(0);
        popup.refresh(nowhere(), &[root("jod")], &[paths(&["a.rs", "b.rs"])]);
        popup.prev();
        assert_eq!(popup.selected, 1, "up from the top lands on the bottom");
        popup.next();
        assert_eq!(popup.selected, 0);
    }

    /// Typing narrows the list under the highlight, and a highlight left
    /// pointing past the end would accept nothing on `⏎`.
    #[test]
    fn narrowing_the_list_pulls_the_highlight_back_into_it() {
        let mut popup = Mention::new(0);
        popup.refresh(nowhere(), &[root("jod")], &[paths(&["alpha.rs", "beta.rs"])]);
        popup.next();
        assert_eq!(popup.selected, 1);
        popup.query = "alpha".into();
        popup.refresh(nowhere(), &[root("jod")], &[paths(&["alpha.rs", "beta.rs"])]);
        assert_eq!(popup.selected, 0);
        assert_eq!(popup.acceptable().map(|r| r.path.as_str()), Some("alpha.rs"));
    }

    // ---- what the popup offers on a real tree -------------------------

    /// A scratch directory that is emphatically **not** a git repository —
    /// no `.git`, no `.gitignore`, nothing to inherit ignore rules from.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jod-mention-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        jod_core::roots::normalise(&dir)
    }

    fn write(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn rooted_at(dir: &std::path::Path) -> Root {
        Root {
            id: 0,
            conversation_id: "c".into(),
            path: dir.to_path_buf(),
            writable: false,
            position: 0,
            origin: Origin::Human,
            added_at_ms: 0,
        }
    }

    /// BUG-15 as the user meets it: `/add-dir` a freshly scaffolded project —
    /// dependencies installed, never `git init`-ed — then press `@` and read
    /// the eight rows.
    ///
    /// The assertion is on those eight rows and nothing further down, because
    /// eight rows is the whole popup: a source file ranked ninth is a source
    /// file you cannot see.
    #[test]
    fn opening_the_popup_in_a_non_git_project_shows_source_not_dependencies() {
        let dir = scratch("non-git-popup");
        write(&dir.join("src/engine.js"), "export const board = [];");
        write(&dir.join("src/render.js"), "");
        write(&dir.join("index.html"), "<html>");
        write(&dir.join("package.json"), "{}");
        write(&dir.join("dist/assets/index-CGus7geV.js"), "");
        write(&dir.join("dist/index.html"), "");
        // pnpm keeps the real files in the hidden store and links the rest.
        for n in 0..40 {
            write(
                &dir.join(format!(
                    "node_modules/.pnpm/pkg{n}@1.0.0/node_modules/pkg{n}/index.js"
                )),
                "",
            );
        }

        jod_core::rank::clear_candidate_cache();
        let paths = Arc::new(jod_core::rank::candidates(&dir).unwrap());
        let mut popup = Mention::new(0);
        popup.refresh(&dir, &[rooted_at(&dir)], &[paths]);

        let shown: Vec<&str> = popup.rows.iter().map(|r| r.path.as_str()).collect();
        assert!(
            shown.iter().any(|p| *p == "src/engine.js"),
            "no source file in the eight rows: {shown:?}"
        );
        let noise = shown
            .iter()
            .filter(|p| p.starts_with("node_modules") || p.starts_with("dist"))
            .count();
        assert_eq!(noise, 0, "the popup is showing build output: {shown:?}");
    }

    /// And the flag that caused BUG-15 keeps what it was added for.
    #[test]
    fn a_dotfile_is_still_mentionable() {
        let dir = scratch("dotfiles-popup");
        write(&dir.join(".env"), "KEY=1");
        write(&dir.join("node_modules/.pnpm/left-pad@1/index.js"), "");

        jod_core::rank::clear_candidate_cache();
        let paths = Arc::new(jod_core::rank::candidates(&dir).unwrap());
        let mut popup = Mention::new(0);
        popup.query = "env".into();
        popup.refresh(&dir, &[rooted_at(&dir)], &[paths]);

        assert_eq!(
            popup.acceptable().map(|r| r.path.as_str()),
            Some(".env"),
            "{:?}",
            popup.rows
        );
    }

    // ---- where the file actually is -----------------------------------

    /// BUG-16 as it was met: `@` in a console standing in one repository put
    /// another repository's files at the top of the list.
    ///
    /// The fixture is the shape the machine's own pinned chat is in — a root
    /// added long ago, the launch directory added today, so the stranger sorts
    /// first on position — and the two files are the same name, which is the
    /// case where the merge has nothing but locality to go on.
    #[test]
    fn a_file_in_the_launch_directory_outranks_the_same_name_elsewhere() {
        let base = scratch("locality");
        write(&base.join("mine/src/main.rs"), "");
        write(&base.join("other/src/main.rs"), "");

        let roots = [
            rooted_at(&base.join("other")),
            rooted_at(&base.join("mine")),
        ];
        let candidates = [paths(&["src/main.rs"]), paths(&["src/main.rs"])];
        let here = base.join("mine");

        let rows = rank_roots("main", &here, &roots, &candidates);
        assert_eq!(
            rows[0].insertion(),
            "mine/src/main.rs",
            "the repository you are standing in comes first: {rows:?}"
        );
        assert_eq!(rows.len(), 2, "and the other one is still reachable");
    }

    /// A root inside another root enumerates every file twice, once under each
    /// prefix — which is the state `~/repo` plus `~/repo/contra-dogfood` leaves
    /// this machine's main chat in. One file on disk is one row.
    #[test]
    fn a_file_reachable_through_two_roots_is_offered_once() {
        let base = scratch("nested-roots");
        write(&base.join("project/notes/todo.md"), "");

        let roots = [
            rooted_at(&base.join("project")),
            rooted_at(&base.join("project/notes")),
        ];
        let candidates = [paths(&["notes/todo.md"]), paths(&["todo.md"])];

        let rows = rank_roots("todo", nowhere(), &roots, &candidates);
        assert_eq!(rows.len(), 1, "one file, two prefixes, one row: {rows:?}");
    }

    /// And the spelling of a path is not what makes two rows one file: a root
    /// reached through a symlink joins to a different string entirely, so the
    /// dedupe has to resolve before it compares.
    #[test]
    fn two_roots_that_resolve_to_one_directory_offer_each_file_once() {
        let base = scratch("linked-roots");
        write(&base.join("real/notes.md"), "");
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();

        let roots = [
            rooted_at(&base.join("real")),
            rooted_at(&base.join("link")),
        ];
        let candidates = [paths(&["notes.md"]), paths(&["notes.md"])];

        let rows = rank_roots("notes", nowhere(), &roots, &candidates);
        assert_eq!(
            rows.len(),
            1,
            "`real/notes.md` and `link/notes.md` are one file: {rows:?}"
        );
    }

    /// Accepting replaces the `@` and everything typed after it — no more, so
    /// the words either side of the mention are untouched.
    #[test]
    fn the_replaced_span_covers_the_at_sign_and_the_query() {
        let mut popup = Mention::new(5);
        popup.query = "main".into();
        assert_eq!(popup.span(), 5..10);
    }
}
