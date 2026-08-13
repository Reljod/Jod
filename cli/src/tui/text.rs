//! Fitting text into a column that is narrower than it is.
//!
//! One helper, deliberately here rather than inside [`super::ui`]: three
//! separate places in the TUI clip a path against the terminal width, and each
//! of them clips from the wrong end. This is the shared answer, so the fourth
//! one does not have to be written again.
//!
//! ## Which end to drop
//!
//! For prose, the front is what matters and `ui::cut` — cut the tail,
//! mark it `…` — is right. For a **path** it is the exact opposite: the tail is
//! the filename, and the head is a prefix a dozen sibling rows share. Clipping
//! a path from the right produced this, six different files as six identical
//! rows:
//!
//! ```text
//! tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
//! tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
//! ```
//!
//! Eliding from the left keeps the end that distinguishes them:
//!
//! ```text
//! …/tinyglobby@0.2.17/dist/index.js
//! …/tinyglobby@0.2.17/dist/index.d.ts
//! ```
//!
//! ## It reports what it dropped
//!
//! A picker row is not a plain string — it carries the byte offsets the query
//! matched, and the renderer bolds them. Dropping bytes off the front moves
//! every one of those offsets. So [`Elided`] hands back where the surviving
//! tail started, and [`Elided::shift`] moves an offset into it, which is the
//! difference between a highlighted row and a panic on a stale index.

/// The marker that says text was dropped. One column, three bytes.
pub const ELLIPSIS: &str = "…";

/// The result of [`elide_left`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elided {
    /// What to draw. Starts with [`ELLIPSIS`] when anything was dropped.
    pub text: String,
    /// Byte offset in the original where `text`'s surviving tail begins.
    /// Zero when the whole string fitted.
    pub from: usize,
}

impl Elided {
    /// Where a byte offset in the original landed in [`Elided::text`], or
    /// `None` when that byte was one of the ones dropped.
    ///
    /// A caller that ignores the `None` and reuses the original offset is
    /// pointing into a string that is no longer there — a highlight on the
    /// wrong character at best, a slice off a character boundary at worst.
    pub fn shift(&self, at: usize) -> Option<usize> {
        if self.from == 0 {
            return Some(at);
        }
        if at < self.from {
            return None;
        }
        Some(at - self.from + ELLIPSIS.len())
    }

    /// Whether anything was dropped.
    pub fn is_elided(&self) -> bool {
        self.from != 0
    }
}

/// `s` fitted into `width` columns by dropping characters from the **left**.
///
/// Width is counted in `char`s, matching `ui::cut`: the TUI's own
/// measure throughout, and a second, more accurate one here would only
/// disagree with the widget that lays the row out.
///
/// The marker costs one of the `width` columns, so the caller never has to
/// budget for it. At `width` 1 there is room for the marker and nothing else,
/// and at 0 not even that — both are degenerate terminals rather than states
/// to design for, and both return something drawable rather than panicking.
pub fn elide_left(s: &str, width: usize) -> Elided {
    let count = s.chars().count();
    if count <= width {
        return Elided {
            text: s.to_string(),
            from: 0,
        };
    }
    if width == 0 {
        return Elided {
            text: String::new(),
            from: s.len(),
        };
    }
    if width == 1 {
        return Elided {
            text: ELLIPSIS.to_string(),
            from: s.len(),
        };
    }
    // One column goes to the marker; the rest is tail.
    let keep = width - 1;
    let from = s
        .char_indices()
        .nth(count - keep)
        .map_or(s.len(), |(at, _)| at);
    Elided {
        text: format!("{ELLIPSIS}{}", &s[from..]),
        from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_that_fits_is_returned_untouched() {
        let fitted = elide_left("src/main.rs", 20);
        assert_eq!(fitted.text, "src/main.rs");
        assert!(!fitted.is_elided());
        assert_eq!(fitted.shift(4), Some(4), "nothing moved");
    }

    #[test]
    fn a_string_exactly_the_width_is_not_elided() {
        assert_eq!(elide_left("abcde", 5).text, "abcde");
        // One column short, so the marker takes one and two characters go.
        assert_eq!(elide_left("abcde", 4).text, "…cde");
    }

    /// The whole point: the end survives, because the end is the filename.
    #[test]
    fn the_tail_survives_and_the_head_is_marked() {
        let long = "tetris/node_modules/.pnpm/tinyglobby@0.2.17/dist/index.js";
        let fitted = elide_left(long, 20);
        assert_eq!(fitted.text.chars().count(), 20);
        assert!(fitted.text.starts_with('…'), "{}", fitted.text);
        assert!(fitted.text.ends_with("index.js"), "{}", fitted.text);
    }

    /// Two rows that used to render identically must not.
    #[test]
    fn two_paths_sharing_a_long_prefix_come_out_different() {
        let stem = "tetris/node_modules/.pnpm/tinyglobby@0.2.17/dist";
        let one = elide_left(&format!("{stem}/index.js"), 24);
        let two = elide_left(&format!("{stem}/index.d.ts"), 24);
        assert_ne!(one.text, two.text, "{} vs {}", one.text, two.text);
        assert!(one.text.ends_with("index.js"));
        assert!(two.text.ends_with("index.d.ts"));
    }

    /// The offsets a picker highlights are byte offsets into the original, and
    /// the original just got shorter at the front.
    #[test]
    fn a_surviving_offset_moves_and_a_dropped_one_reports_itself() {
        let path = "aaaa/bbbb/target.rs";
        let fitted = elide_left(path, 12);
        assert_eq!(fitted.text, "…b/target.rs");
        // `t` of `target.rs` is at byte 10 in the original.
        let moved = fitted.shift(10).expect("the tail survived");
        assert_eq!(fitted.text[moved..].chars().next(), Some('t'));
        assert_eq!(fitted.shift(0), None, "the first byte was dropped");
    }

    /// Multi-byte characters must not be cut in half, and the offsets they
    /// carry must still land on a boundary.
    #[test]
    fn it_cuts_on_character_boundaries_not_byte_ones() {
        let path = "日本語/ディレクトリ/файл.rs";
        for width in 1..path.chars().count() + 2 {
            let fitted = elide_left(path, width);
            assert!(
                fitted.text.chars().count() <= width.max(1),
                "width {width} gave {:?}",
                fitted.text
            );
            // Slicing at `from` would have panicked already if it were not a
            // boundary; this asserts the survivors are addressable too.
            for (at, _) in path.char_indices() {
                if let Some(moved) = fitted.shift(at) {
                    assert!(fitted.text.is_char_boundary(moved), "width {width}");
                }
            }
        }
    }

    #[test]
    fn a_terminal_with_no_room_still_returns_something_drawable() {
        assert_eq!(elide_left("src/main.rs", 1).text, "…");
        assert_eq!(elide_left("src/main.rs", 0).text, "");
        assert_eq!(elide_left("src/main.rs", 0).shift(0), None);
        assert_eq!(elide_left("", 0).text, "", "empty fits in nothing");
    }
}
