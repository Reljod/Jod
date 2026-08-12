//! Fuzzy matching, in-process, with the positions needed to highlight it.
//!
//! Jod builds fzf's *feel* and depends on no picker binary. The target is the
//! interaction: type a few scattered letters, see ranked matches update on
//! every keystroke with the matched characters highlighted, move with the
//! arrows, accept with enter.
//!
//! Shelling out to `fzf` would actively prevent the good version of that.
//! `fzf` owns a whole terminal, so every `@` would tear down and restore the
//! screen, and an inline popup drawn under the cursor is not something an
//! external full-screen program can draw at all. So the matching lives here,
//! over a candidate list enumerated by ripgrep with a walker fallback, and no
//! picker binary is required, preferred, or supported.
//!
//! ## Why this is in core and not in the terminal
//!
//! Ranking is logic, not drawing. Everything here is testable without a
//! terminal, which is the rule that keeps the one-lane-owns-the-TUI split from
//! making the terminal a bottleneck — and it is a better shape regardless.
//!
//! ## The bar this is measured against
//!
//! - results **ranked**, not merely filtered
//! - matched characters highlighted in every row, which is why [`Match`]
//!   carries positions rather than only a score
//! - live on every keystroke with no perceptible lag on a large repository
//! - a deep exact path outranks a scattered-letters coincidence

/// One candidate that matched, with everything the renderer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the candidate list that was searched.
    pub index: usize,
    /// Higher is better. Comparable only within one query's results.
    pub score: i32,
    /// Byte offsets in the candidate that the query matched, ascending.
    ///
    /// The reason a score alone is not enough: without these the popup can
    /// filter but cannot show *why* a row matched, and a fuzzy list you cannot
    /// read the match in is a list you stop trusting.
    pub positions: Vec<usize>,
}
