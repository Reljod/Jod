//! Turning a file-editing tool call into a diff.
//!
//! An edit currently arrives as a one-line summary — `Edit · src/main.rs` — and
//! that is unreadable as review. It tells you an edit happened and nothing
//! about what it did, which is the difference between watching an agent work
//! and being able to trust it afterwards.
//!
//! ## The diff is built from the call, not from the disk
//!
//! Every harness's edit tool carries the before and after in its arguments, and
//! that is deliberately what this reads. Going to the filesystem instead would
//! be wrong in two directions: the file has already moved on by the time the
//! transcript is scrolled back to, and a run replayed from the record has no
//! filesystem at all. The arguments are what actually happened; the disk is
//! what happened *last*.
//!
//! ## Harnesses spell it three ways
//!
//! Claude Code sends `file_path` with `old_string`/`new_string`, or `content`
//! for a whole-file write. AGY spells the path `TargetFile`. OpenCode uses
//! `filePath`. [`normalise`] flattens case and underscores so one table covers
//! all of them, the same trick `app::tool_detail` already uses — and the same
//! reason: the harnesses genuinely disagree, and a missed spelling silently
//! costs a whole harness its diffs.

use serde_json::Value;

/// One line of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Context(String),
    Added(String),
    Removed(String),
}

impl Line {
    /// The character in the gutter. Colour is never the only channel here, and
    /// on a diff that rule is not a nicety: red and green are the two colours
    /// most often indistinguishable to a reader.
    pub fn sign(&self) -> char {
        match self {
            Line::Context(_) => ' ',
            Line::Added(_) => '+',
            Line::Removed(_) => '-',
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Line::Context(t) | Line::Added(t) | Line::Removed(t) => t,
        }
    }
}

/// A file edit, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    pub lines: Vec<Line>,
    /// How many lines were dropped from the middle. Printed rather than
    /// silently omitted: a diff that is quietly partial is one you review as
    /// though it were whole.
    pub elided: usize,
}

impl Edit {
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|l| matches!(l, Line::Added(_))).count()
    }

    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, Line::Removed(_)))
            .count()
    }
}

/// How many lines of an edit are shown before the middle is collapsed.
///
/// A whole-file write of a thousand lines is not review material, and pasting
/// it into the transcript pushes the conversation off the screen. The head and
/// the tail are what say *what kind* of change it was.
pub const MAX_LINES: usize = 40;

/// Lines of unchanged context kept either side of a change.
pub const CONTEXT: usize = 2;

/// The largest edit that gets a real line-by-line diff.
///
/// Beyond this the quadratic table below would cost more than the answer is
/// worth, and the honest fallback — everything removed, everything added — is
/// what a whole-file rewrite actually is anyway.
const MAX_DIFFABLE: usize = 600;

/// Whether this tool call is a file edit, and what it changed.
///
/// `None` for everything else, so the transcript keeps rendering ordinary tool
/// calls the way it always has.
pub fn from_tool(name: &str, input: &Value) -> Option<Edit> {
    let map = input.as_object()?;
    let get = |want: &str| {
        map.iter()
            .find(|(k, _)| normalise(k) == want)
            .and_then(|(_, v)| v.as_str())
    };
    let path = get("filepath")
        .or_else(|| get("path"))
        .or_else(|| get("targetfile"))?;

    // An edit names both sides; a write names only the new content. Checked in
    // that order because a harness that sends both means the first.
    let (old, new) = match (get("oldstring"), get("newstring")) {
        (Some(old), Some(new)) => (old, new),
        _ => match get("content") {
            Some(content) => ("", content),
            // A tool with a path and no content is a read, a glob or a
            // deletion. None of those is a diff.
            None => return None,
        },
    };
    if !is_edit(name) && old.is_empty() && new.is_empty() {
        return None;
    }

    let (lines, elided) = diff(old, new);
    Some(Edit {
        path: path.to_string(),
        lines,
        elided,
    })
}

/// Whether a tool's *name* suggests an edit, for the whole-file-write case
/// where the arguments alone are ambiguous.
///
/// Substring rather than exact: the three harnesses ship `Edit`, `MultiEdit`,
/// `Write`, `str_replace_editor` and `create_file` between them, and matching
/// the stem covers spellings none of them has shipped yet.
fn is_edit(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["edit", "write", "create", "replace", "patch"]
        .iter()
        .any(|stem| name.contains(stem))
}

/// Key comparison with case and underscores ignored, so `file_path`,
/// `filePath` and `FilePath` are one key.
fn normalise(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A line diff of `old` against `new`, already trimmed to what is worth
/// reading.
///
/// Returns the lines and how many were elided from the middle.
pub fn diff(old: &str, new: &str) -> (Vec<Line>, usize) {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();

    // Too big to diff properly: say what it is — a replacement — rather than
    // spending a second computing a prettier way to say the same thing.
    if before.len() > MAX_DIFFABLE || after.len() > MAX_DIFFABLE {
        let mut lines: Vec<Line> = before.iter().map(|l| Line::Removed(l.to_string())).collect();
        lines.extend(after.iter().map(|l| Line::Added(l.to_string())));
        return trim(lines);
    }

    trim(narrow(&walk(&before, &after)))
}

/// The longest-common-subsequence walk, as a flat line list.
///
/// A table rather than Myers: the inputs here are one tool call's before and
/// after, which is tens of lines in the common case, and a correct simple
/// algorithm beats a fast one nobody can check. `MAX_DIFFABLE` is what keeps
/// the quadratic honest.
fn walk(before: &[&str], after: &[&str]) -> Vec<Line> {
    let (n, m) = (before.len(), after.len());
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if before[i] == after[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut lines = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if before[i] == after[j] {
            lines.push(Line::Context(before[i].to_string()));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            lines.push(Line::Removed(before[i].to_string()));
            i += 1;
        } else {
            lines.push(Line::Added(after[j].to_string()));
            j += 1;
        }
    }
    lines.extend(before[i..].iter().map(|l| Line::Removed(l.to_string())));
    lines.extend(after[j..].iter().map(|l| Line::Added(l.to_string())));
    lines
}

/// Drop runs of context longer than [`CONTEXT`] either side of a change.
///
/// Without this, a two-line change inside a two-hundred-line `old_string`
/// renders two hundred lines of unchanged text — technically a diff, and
/// useless as one.
fn narrow(lines: &[Line]) -> Vec<Line> {
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !matches!(l, Line::Context(_)))
        .map(|(at, _)| at)
        .collect();
    if changed.is_empty() {
        return lines.to_vec();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(at, line)| {
            !matches!(line, Line::Context(_))
                || changed
                    .iter()
                    .any(|c| at.abs_diff(*c) <= CONTEXT)
        })
        .map(|(_, line)| line.clone())
        .collect()
}

/// Keep the head and the tail, and say how much of the middle went.
fn trim(lines: Vec<Line>) -> (Vec<Line>, usize) {
    if lines.len() <= MAX_LINES {
        return (lines, 0);
    }
    let half = MAX_LINES / 2;
    let elided = lines.len() - MAX_LINES;
    let mut kept: Vec<Line> = lines[..half].to_vec();
    kept.extend_from_slice(&lines[lines.len() - half..]);
    (kept, elided)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_edit_becomes_added_and_removed_lines() {
        let (lines, elided) = diff("one\ntwo\nthree\n", "one\ntwo and a half\nthree\n");
        assert_eq!(elided, 0);
        assert_eq!(
            lines,
            vec![
                Line::Context("one".into()),
                Line::Removed("two".into()),
                Line::Added("two and a half".into()),
                Line::Context("three".into()),
            ]
        );
    }

    #[test]
    fn an_unchanged_file_produces_only_context() {
        let (lines, _) = diff("one\ntwo\n", "one\ntwo\n");
        assert!(lines.iter().all(|l| matches!(l, Line::Context(_))));
    }

    /// A whole-file write has nothing to compare against, so every line is new.
    #[test]
    fn a_write_with_no_previous_content_is_all_additions() {
        let (lines, _) = diff("", "hello\nworld\n");
        assert_eq!(
            lines,
            vec![Line::Added("hello".into()), Line::Added("world".into())]
        );
    }

    /// Two hundred unchanged lines around a one-line change is technically a
    /// diff and useless as one.
    #[test]
    fn unchanged_runs_are_narrowed_to_a_little_context() {
        let old: String = (0..60).map(|n| format!("line {n}\n")).collect();
        let new = old.replace("line 30\n", "line thirty\n");
        let (lines, _) = diff(&old, &new);
        assert!(
            lines.len() <= 2 * CONTEXT + 2,
            "expected a small window, got {}: {lines:?}",
            lines.len()
        );
        assert!(lines.contains(&Line::Removed("line 30".into())));
        assert!(lines.contains(&Line::Added("line thirty".into())));
        assert!(lines.contains(&Line::Context("line 29".into())));
    }

    /// A diff that is quietly partial is one you review as though it were
    /// whole, so the count of what went is part of the output.
    #[test]
    fn a_huge_change_keeps_both_ends_and_counts_the_middle() {
        let new: String = (0..200).map(|n| format!("line {n}\n")).collect();
        let (lines, elided) = diff("", &new);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(elided, 200 - MAX_LINES);
        assert_eq!(lines[0], Line::Added("line 0".into()), "the head survives");
        assert_eq!(
            lines[lines.len() - 1],
            Line::Added("line 199".into()),
            "and so does the tail"
        );
    }

    /// Past the diffable size the honest answer is "this was replaced", which
    /// is what a whole-file rewrite is.
    #[test]
    fn an_enormous_edit_degrades_to_a_replacement_rather_than_hanging() {
        let old: String = (0..MAX_DIFFABLE + 10).map(|n| format!("a {n}\n")).collect();
        let new: String = (0..MAX_DIFFABLE + 10).map(|n| format!("b {n}\n")).collect();
        let (lines, elided) = diff(&old, &new);
        assert_eq!(lines.len(), MAX_LINES);
        assert!(elided > 0);
    }

    /// The three harnesses spell the path three ways, and a missed spelling
    /// silently costs a whole harness its diffs.
    #[test]
    fn every_harness_spelling_of_the_path_is_recognised() {
        for key in ["file_path", "filePath", "TargetFile", "path"] {
            let input = json!({ key: "src/main.rs", "old_string": "a", "new_string": "b" });
            let edit = from_tool("Edit", &input)
                .unwrap_or_else(|| panic!("{key} was not recognised"));
            assert_eq!(edit.path, "src/main.rs");
        }
    }

    #[test]
    fn a_write_is_recognised_from_its_content() {
        let input = json!({ "file_path": "notes.md", "content": "one\ntwo\n" });
        let edit = from_tool("Write", &input).expect("a write is an edit");
        assert_eq!(edit.added(), 2);
        assert_eq!(edit.removed(), 0);
    }

    /// Everything that is not a file edit keeps rendering the way it always
    /// has.
    #[test]
    fn a_tool_that_is_not_an_edit_produces_no_diff() {
        assert!(from_tool("Bash", &json!({ "command": "cargo test" })).is_none());
        assert!(from_tool("Read", &json!({ "file_path": "src/main.rs" })).is_none());
        assert!(from_tool("Grep", &json!({ "pattern": "fn main" })).is_none());
        assert!(from_tool("Edit", &json!("not an object")).is_none());
    }

    #[test]
    fn the_counts_say_how_big_the_change_was() {
        let input = json!({
            "file_path": "src/main.rs",
            "old_string": "one\ntwo\n",
            "new_string": "one\ntwo\nthree\n",
        });
        let edit = from_tool("Edit", &input).unwrap();
        assert_eq!(edit.added(), 1);
        assert_eq!(edit.removed(), 0);
    }

    /// Colour is never the only channel, and on a diff that is not a nicety:
    /// red and green are the two most often indistinguishable.
    #[test]
    fn every_line_carries_a_sign_as_well_as_a_colour() {
        assert_eq!(Line::Added("x".into()).sign(), '+');
        assert_eq!(Line::Removed("x".into()).sign(), '-');
        assert_eq!(Line::Context("x".into()).sign(), ' ');
    }
}
