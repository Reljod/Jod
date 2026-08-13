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

use std::sync::Arc;

use jod_core::rank;
use jod_core::roots::Root;

/// How many rows the popup shows.
///
/// Eight, because the popup floats over the transcript and a taller one hides
/// the thing you were reading in order to help you write about it.
pub const ROWS: usize = 8;

/// What a narrow terminal, or a conversation with nothing set, is told.
pub const NO_ROOTS: &str =
    "no roots set — `jod root add <path>`, or launch with --root";

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
    pub fn refresh(&mut self, roots: &[Root], candidates: &[Arc<Vec<String>>]) {
        self.rooted = !roots.is_empty();
        self.rows = rank_roots(&self.query, roots, candidates);
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

/// Rank every root's candidates against one query and interleave the results.
///
/// [`rank::rank`] takes one slice and a conversation has several roots, so this
/// calls it once per root and merges. The merge is a **stable sort on the score
/// core returned** and nothing else: no second pass, no bonus of its own, no
/// filtering. Ordering is core's decision and there is one place for it — if
/// the ranking ever looks wrong, it is wrong in `rank.rs`, and correcting it
/// here would hide that from the tests that specify it.
///
/// Merging rather than concatenating, because a second root is not a second
/// list — it is more of the same list. Appending would put every path from the
/// first repository above the exact match in the second.
///
/// The alternative — concatenate every root's paths and call `rank` once — is
/// not open: `rank` needs a contiguous `&[String]`, and building one would copy
/// a hundred thousand paths on every keystroke, which is the stall
/// [`rank::candidates_shared`] hands back an `Arc` to avoid.
pub fn rank_roots(query: &str, roots: &[Root], candidates: &[Arc<Vec<String>>]) -> Vec<Row> {
    // Several roots means every path has to say which one it is from; one root
    // means qualifying is noise. Decided here rather than per row so a list of
    // rows cannot come out half-qualified.
    let qualify = roots.len() > 1;
    let mut scored: Vec<(i32, Row)> = Vec::new();
    for (root, paths) in roots.iter().zip(candidates) {
        for hit in rank::rank(query, paths, ROWS) {
            scored.push((
                hit.score,
                Row {
                    label: qualify.then(|| root.label()),
                    // Indexed rather than `get`: `Match::index` is an index into
                    // the slice that was just passed in, and the slice has not
                    // moved between the two lines.
                    path: paths[hit.index].clone(),
                    positions: hit.positions,
                },
            ));
        }
    }
    // Stable, so an empty query — where core returns every candidate at score
    // zero, in input order — keeps the roots in the user's own order rather
    // than shuffling them per keystroke.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(ROWS).map(|(_, row)| row).collect()
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
        let rows = rank_roots("rank", &roots, &candidates);
        assert_eq!(rows[0].path, "core/src/rank.rs", "{rows:?}");
    }

    /// Without positions the popup can filter but cannot show *why* a row
    /// matched, and that is the difference between fzf and a `grep`.
    #[test]
    fn every_row_says_which_of_its_characters_matched() {
        let roots = [root("jod")];
        let candidates = [paths(&["core/src/rank.rs"])];
        let rows = rank_roots("rank", &roots, &candidates);
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
        let rows = rank_roots("rank", &roots, &candidates);
        assert_eq!(rows[0].path, "rank.md", "{rows:?}");
    }

    /// `@src/main.rs` names two different files when a conversation can see two
    /// repositories, so the insertion has to say which.
    #[test]
    fn several_roots_qualify_the_path_and_one_root_does_not() {
        let candidates = [paths(&["src/main.rs"]), paths(&["src/main.rs"])];
        let two = rank_roots("main", &[root("jod"), root("notes")], &candidates);
        assert_eq!(two[0].insertion(), "jod/src/main.rs");

        let one = rank_roots("main", &[root("jod")], &candidates[..1]);
        assert_eq!(one[0].insertion(), "src/main.rs", "one root needs no prefix");
    }

    /// The spec's own words: with zero roots it says so and accepts nothing.
    /// An empty list would look like "no matches", and the next keystroke would
    /// look like it might help.
    #[test]
    fn with_no_roots_there_is_nothing_to_accept() {
        let mut popup = Mention::new(0);
        popup.query = "main".into();
        popup.refresh(&[], &[]);
        assert!(!popup.rooted);
        assert!(popup.acceptable().is_none());
        assert!(popup.rows.is_empty());
    }

    /// The popup opens on `@` before anything has been typed, so the empty
    /// query is a state it lives in rather than an edge case.
    #[test]
    fn an_empty_query_offers_the_first_candidates_rather_than_nothing() {
        let mut popup = Mention::new(0);
        popup.refresh(&[root("jod")], &[paths(&["a.rs", "b.rs"])]);
        assert_eq!(popup.rows.len(), 2);
        assert!(popup.acceptable().is_some());
    }

    #[test]
    fn the_popup_shows_at_most_its_own_height() {
        let many: Vec<String> = (0..40).map(|n| format!("file{n}.rs")).collect();
        let rows = rank_roots("file", &[root("jod")], &[Arc::new(many)]);
        assert_eq!(rows.len(), ROWS);
    }

    #[test]
    fn the_arrows_wrap_and_never_point_past_the_list() {
        let mut popup = Mention::new(0);
        popup.refresh(&[root("jod")], &[paths(&["a.rs", "b.rs"])]);
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
        popup.refresh(&[root("jod")], &[paths(&["alpha.rs", "beta.rs"])]);
        popup.next();
        assert_eq!(popup.selected, 1);
        popup.query = "alpha".into();
        popup.refresh(&[root("jod")], &[paths(&["alpha.rs", "beta.rs"])]);
        assert_eq!(popup.selected, 0);
        assert_eq!(popup.acceptable().map(|r| r.path.as_str()), Some("alpha.rs"));
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
