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
    /// Lines added by the whole edit, **before** the middle was elided.
    ///
    /// Stored rather than counted off `lines`, and that is the whole reason it
    /// exists. Counting the rendered rows was right only while nothing was
    /// dropped: a four-hundred-line file written in one call keeps forty rows
    /// and would report `+40`, understating the change by an order of magnitude
    /// on exactly the edits where the number is load-bearing. The collapsed
    /// summary is often the *only* thing a reader sees about a file, so it has
    /// to be the real count.
    pub added: usize,
    pub removed: usize,
    /// What the tool did to the file, for the one-line summary.
    pub verb: Verb,
}

/// How a file was touched, in the words the summary uses.
///
/// Separate from the counts because "created" and "edited" are different facts
/// about the same `+12 -0`: a new file is all additions by definition, and
/// reading that as a twelve-line change to something that already existed is
/// the wrong picture entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Created,
    Edited,
}

impl Verb {
    /// The past tense, for a step that has finished.
    pub fn done(&self) -> &'static str {
        match self {
            Verb::Created => "created",
            Verb::Edited => "edited",
        }
    }

    /// The present participle, for a step still in flight — the difference
    /// between "this is happening" and "this happened", which is the whole
    /// point of showing a step while it runs.
    pub fn doing(&self) -> &'static str {
        match self {
            Verb::Created => "creating",
            Verb::Edited => "editing",
        }
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

    // A write that names no `old_string` is a file coming into existence as far
    // as this call can tell. That is not always literally true — a whole-file
    // write over an existing file spells the same way — but it is what the
    // arguments say, and the alternative is going to the filesystem, which
    // `from_tool` deliberately never does.
    let verb = if old.is_empty() {
        Verb::Created
    } else {
        Verb::Edited
    };
    Some(edit(path, old, new, verb))
}

/// One [`Edit`], diffed and counted.
///
/// The single place an `Edit` is built, so the totals cannot drift from the
/// lines they describe — both the tool path and the shell path come through
/// here.
fn edit(path: &str, old: &str, new: &str, verb: Verb) -> Edit {
    let full = walk_all(old, new);
    let added = full.iter().filter(|l| matches!(l, Line::Added(_))).count();
    let removed = full.iter().filter(|l| matches!(l, Line::Removed(_))).count();
    let (lines, elided) = trim(full);
    Edit {
        path: path.to_string(),
        lines,
        elided,
        added,
        removed,
        verb,
    }
}

/// The files a *shell* command writes, as diffs.
///
/// ## Why the shell has to be read at all
///
/// An agent with a shell does not need the edit tool, and a surprising number
/// of them do not use it. A whole project can be built with
/// `cat > src/car.js <<'EOF' … EOF` and never touch `Write` once — at which
/// point every file-change surface in this program goes quiet and the
/// transcript claims, in effect, that nothing was written. That is the worst
/// kind of wrong: not a missing feature but a confident silence.
///
/// ## Only heredocs
///
/// A heredoc is the one shell write whose *content* is in the command, so it
/// can be shown as a real diff rather than a rumour that a file changed.
/// Ordinary redirects (`echo hi > f`, `cmd | tee f`) are deliberately left
/// alone: their content is whatever the left-hand side prints, which is not
/// knowable without running it, and `2>&1` and `> /dev/null` would turn any
/// looser rule into a stream of false file changes. Under-reporting here is
/// recoverable — the command itself is still in the transcript. Inventing a
/// change is not.
pub fn from_shell(name: &str, input: &Value) -> Vec<Edit> {
    if !is_shell(name) {
        return Vec::new();
    }
    let Some(map) = input.as_object() else {
        return Vec::new();
    };
    let command = map
        .iter()
        .find(|(k, _)| matches!(normalise(k).as_str(), "command" | "cmd" | "script"))
        .and_then(|(_, v)| v.as_str());
    match command {
        Some(command) => heredocs(command),
        None => Vec::new(),
    }
}

/// Whether a tool's *name* says it runs a shell.
fn is_shell(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["bash", "shell", "terminal", "exec"]
        .iter()
        .any(|stem| name.contains(stem))
}

/// One `<<` redirect that was opened on a line.
struct Open {
    /// `None` when the heredoc feeds a command rather than a file — `python3 -
    /// <<PY`. Its body still has to be *skipped* so the lines inside it are not
    /// re-scanned as though they were shell, which is how a `>` in a string
    /// literal turns into an imaginary file.
    path: Option<String>,
    delim: String,
    /// `>>` rather than `>`: the file is being added to, not made.
    append: bool,
    /// `<<-`, which lets the closing delimiter be indented with tabs.
    dash: bool,
}

/// Every heredoc-written file in one shell command.
fn heredocs(command: &str) -> Vec<Edit> {
    let all: Vec<&str> = command.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < all.len() {
        let Some(open) = opening(all[i]) else {
            i += 1;
            continue;
        };
        let mut body: Vec<&str> = Vec::new();
        let mut end = None;
        for (j, line) in all.iter().enumerate().skip(i + 1) {
            let candidate = if open.dash {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if candidate.trim_end() == open.delim {
                end = Some(j);
                break;
            }
            body.push(line);
        }
        // An unterminated heredoc means this was not one — a `<<` inside a
        // string, most likely. Step over the opening line only, rather than
        // swallowing the rest of the command on a guess.
        let Some(end) = end else {
            i += 1;
            continue;
        };
        if let Some(path) = open.path {
            let text = if body.is_empty() {
                String::new()
            } else {
                format!("{}\n", body.join("\n"))
            };
            let verb = if open.append {
                Verb::Edited
            } else {
                Verb::Created
            };
            out.push(edit(&path, "", &text, verb));
        }
        i = end + 1;
    }
    out
}

/// The heredoc a line opens, if it opens one.
fn opening(line: &str) -> Option<Open> {
    let at = line.find("<<")?;
    let after = &line[at + 2..];
    // `<<<` is a here-*string* — one line of input, no body, no terminator. Read
    // as a heredoc it would eat the rest of the command looking for a delimiter
    // that never comes.
    if after.starts_with('<') {
        return None;
    }
    let (dash, after) = match after.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, after),
    };
    let after = after.trim_start();
    let quote = after.starts_with('\'') || after.starts_with('"');
    let rest = if quote { &after[1..] } else { after };
    let delim: String = rest
        .chars()
        .take_while(|c| {
            if quote {
                *c != '\'' && *c != '"'
            } else {
                c.is_alphanumeric() || *c == '_'
            }
        })
        .collect();
    if delim.is_empty() {
        return None;
    }
    // The redirect usually sits left of the `<<` (`cat > f <<'EOF'`), but the
    // shell is happy either way round, so the tail is checked too.
    let tail_from = at + 2 + (rest.find(&delim).map_or(0, |p| p + delim.len()));
    let found = redirect(&line[..at]).or_else(|| redirect(line.get(tail_from..).unwrap_or("")));
    let (path, append) = match found {
        Some((path, append)) => (Some(path), append),
        None => (None, false),
    };
    Some(Open {
        path,
        append,
        delim,
        dash,
    })
}

/// The file a `>`/`>>` in this fragment writes to, and whether it appends.
///
/// The **last** redirect wins, because that is the one nearest the `<<` and so
/// the one the heredoc feeds.
fn redirect(fragment: &str) -> Option<(String, bool)> {
    let mut found = None;
    let bytes: Vec<char> = fragment.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '>' {
            i += 1;
            continue;
        }
        // `2>`, `&>` and friends redirect a stream, not this heredoc's body.
        let fd = i > 0 && (bytes[i - 1].is_ascii_digit() || bytes[i - 1] == '&');
        let append = bytes.get(i + 1) == Some(&'>');
        let mut j = i + if append { 2 } else { 1 };
        // `>&2` is a duplication, not a filename.
        if bytes.get(j) == Some(&'&') {
            i = j + 1;
            continue;
        }
        while bytes.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        let quote = matches!(bytes.get(j), Some('\'') | Some('"'));
        if quote {
            j += 1;
        }
        let mut path = String::new();
        while let Some(c) = bytes.get(j) {
            let stop = if quote {
                *c == '\'' || *c == '"'
            } else {
                c.is_whitespace() || *c == ';' || *c == '&' || *c == '|' || *c == '<'
            };
            if stop {
                break;
            }
            path.push(*c);
            j += 1;
        }
        if !fd && !path.is_empty() && path != "/dev/null" {
            found = Some((path, append));
        }
        i = j.max(i + 1);
    }
    found
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
#[cfg(test)]
pub fn diff(old: &str, new: &str) -> (Vec<Line>, usize) {
    trim(walk_all(old, new))
}

/// Every line of the diff, narrowed to what is worth reading but **not** yet
/// trimmed.
///
/// Split out from [`diff`] so the added/removed totals can be taken before
/// [`trim`] throws the middle away. Counting after the trim was the bug: the
/// summary line reported the size of what survived on screen rather than the
/// size of the change.
fn walk_all(old: &str, new: &str) -> Vec<Line> {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();

    // Too big to diff properly: say what it is — a replacement — rather than
    // spending a second computing a prettier way to say the same thing.
    if before.len() > MAX_DIFFABLE || after.len() > MAX_DIFFABLE {
        let mut lines: Vec<Line> = before.iter().map(|l| Line::Removed(l.to_string())).collect();
        lines.extend(after.iter().map(|l| Line::Added(l.to_string())));
        return lines;
    }

    narrow(&walk(&before, &after))
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
        assert_eq!(edit.added, 2);
        assert_eq!(edit.removed, 0);
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
        assert_eq!(edit.added, 1);
        assert_eq!(edit.removed, 0);
    }

    /// The counts describe the *change*, not the part of it that fitted on the
    /// screen. They used to be taken off the trimmed line list, so a file
    /// longer than `MAX_LINES` reported the size of its own excerpt — `+40` for
    /// a four-hundred-line write. The collapsed summary is frequently the only
    /// thing said about a file, so an understated count there is not a display
    /// nicety; it is the wrong number in the one place it gets read.
    #[test]
    fn the_counts_survive_the_middle_being_elided() {
        let body: String = (0..400).map(|i| format!("line {i}\n")).collect();
        let edit = from_tool("Write", &json!({ "file_path": "big.txt", "content": body })).unwrap();
        assert_eq!(edit.added, 400, "every added line is counted");
        assert_eq!(edit.removed, 0);
        assert!(edit.elided > 0, "and the body really was trimmed");
        assert!(
            edit.lines.len() <= MAX_LINES,
            "only the excerpt is kept: {}",
            edit.lines.len()
        );
    }

    /// A new file and a change to an existing one are different facts about the
    /// same `+N -M`.
    #[test]
    fn a_write_is_a_creation_and_an_edit_is_not() {
        let write = from_tool("Write", &json!({ "file_path": "a.txt", "content": "hi\n" })).unwrap();
        assert_eq!(write.verb, Verb::Created);
        let edit = from_tool(
            "Edit",
            &json!({ "file_path": "a.txt", "old_string": "hi\n", "new_string": "ho\n" }),
        )
        .unwrap();
        assert_eq!(edit.verb, Verb::Edited);
    }

    // ---- files written through the shell ----

    /// An agent that builds a project out of heredocs never calls the edit
    /// tool, and every file-change surface in the program went quiet for the
    /// whole run. The content is right there in the command, so there is no
    /// reason for the transcript not to show it.
    #[test]
    fn a_heredoc_written_file_is_a_file_change() {
        let command = "cd /tmp/racing\ncat > src/car.js <<'EOF'\nexport const drag = 0.98;\nexport const grip = 1.2;\nEOF\npnpm test";
        let edits = from_shell("Bash", &json!({ "command": command }));
        assert_eq!(edits.len(), 1, "one file was written: {edits:?}");
        assert_eq!(edits[0].path, "src/car.js");
        assert_eq!(edits[0].verb, Verb::Created);
        assert_eq!(edits[0].added, 2);
        assert!(
            edits[0]
                .lines
                .iter()
                .any(|l| l.text().contains("drag = 0.98")),
            "and its content is the diff: {:?}",
            edits[0].lines
        );
    }

    /// One command often writes several files.
    #[test]
    fn every_heredoc_in_one_command_is_found() {
        let command = "cat > a.js <<'EOF'\nconst a = 1;\nEOF\ncat >> b.js <<'EOF'\nconst b = 2;\nEOF";
        let edits = from_shell("Bash", &json!({ "command": command }));
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].path, "a.js");
        assert_eq!(edits[1].path, "b.js");
        assert_eq!(
            edits[1].verb,
            Verb::Edited,
            "`>>` adds to a file rather than making one"
        );
    }

    /// A heredoc that feeds a *command* wrote no file, and the shell inside it
    /// is not shell to be read — a `>` in a Python string would otherwise
    /// become an imaginary file change.
    #[test]
    fn a_heredoc_that_feeds_a_program_writes_nothing() {
        let command = "python3 - <<'PY'\nprint('a > b')\nPY";
        assert!(from_shell("Bash", &json!({ "command": command })).is_empty());
    }

    /// Only heredocs. Everything else the shell redirects has content that is
    /// not knowable without running the command, and guessing produces file
    /// changes that never happened.
    #[test]
    fn ordinary_redirects_are_left_alone() {
        for command in [
            "echo hi > notes.txt",
            "cargo test 2>&1 | tail -40",
            "ls > /dev/null",
            "grep -rn foo . >> out.log",
        ] {
            assert!(
                from_shell("Bash", &json!({ "command": command })).is_empty(),
                "guessed at a file change in: {command}"
            );
        }
    }

    /// A here-string is one line of input with no terminator; read as a heredoc
    /// it would swallow the rest of the command looking for a delimiter that is
    /// never coming.
    #[test]
    fn a_here_string_is_not_a_heredoc() {
        assert!(from_shell("Bash", &json!({ "command": "wc -l <<< \"one line\"" })).is_empty());
    }

    /// Only the shell's own tool. A `command` argument to something else is not
    /// a shell command.
    #[test]
    fn only_a_shell_tool_is_read_as_shell() {
        let input = json!({ "command": "cat > a.js <<'EOF'\nx\nEOF" });
        assert!(!from_shell("Bash", &input).is_empty());
        assert!(from_shell("Edit", &input).is_empty());
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

