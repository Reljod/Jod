//! Slash commands and skills a repository already defines.
//!
//! Reljod should not have to remember which harness knows about which command.
//! Jod scans every root and the user's own config, and offers what it finds in
//! its own palette, marked with where it came from.
//!
//! ## Forwarded, not reimplemented
//!
//! The measurement D7 asked for has been taken, against the real binaries, and
//! it is written up with its commands and their output in
//! [`docs/harness-support.md`](../../docs/harness-support.md). The answer:
//! **every harness expands its own commands, so Jod never substitutes a body.**
//! Claude Code and AGY resolve `/name` written straight into a print-mode
//! prompt; OpenCode does not, but expands the same command natively when it is
//! named by `run --command <name>` instead.
//!
//! So the inlining branch is deleted rather than kept just in case, exactly as
//! D7 said it should be. What survives is the distinction between the two
//! spellings that were actually observed — see [`Expansion`] — and the
//! `Unmeasured` value for a harness nobody has run yet.
//!
//! The one thing Jod deliberately does not do is forward a command across
//! conventions. Handing a `.claude/commands/foo.md` to OpenCode would find no
//! `.opencode/command/foo.md` to resolve, and inlining the body to cover that
//! would rebuild the branch this measurement just removed, for a case D7 never
//! asked about. Every [`Discovered`] therefore records the harness whose
//! convention it follows, and the palette offers it to that harness.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::harness::HarnessKind;
use crate::store::Store;

/// Where a command was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Under one of the conversation's roots — the repository's own.
    Root,
    /// The user's config directory. Available everywhere.
    User,
    /// Shipped by an installed plugin.
    ///
    /// Nothing produces this yet. Every harness here has a plugin mechanism —
    /// `--plugin-dir`, `opencode plugin`, `agy plugin` — and none of their
    /// on-disk layouts has been measured, so scanning for them would mean
    /// inventing paths. The value exists because the column does.
    Plugin,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Root => "root",
            Scope::User => "user",
            Scope::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Scope {
        match s {
            "user" => Scope::User,
            "plugin" => Scope::Plugin,
            _ => Scope::Root,
        }
    }
}

/// A command or a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Command,
    Skill,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Skill => "skill",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "skill" => Kind::Skill,
            _ => Kind::Command,
        }
    }
}

/// One thing Jod's palette can offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovered {
    pub id: i64,
    /// The directory it was found under; empty for user-level config.
    pub root: PathBuf,
    pub scope: Scope,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Whose convention it follows; empty when every harness would find it.
    pub harness: String,
    /// Once kept so Jod could paste a command's text into a harness that could
    /// not expand one itself. The measurement found no such harness, so this is
    /// always empty and nothing reads it. Removing the column belongs to
    /// whoever owns `store.rs`; a migration written purely to tidy up is worth
    /// less than the empty column costs.
    pub body: String,
    pub scanned_at_ms: i64,
}

/// How a harness takes a command Jod forwards to it.
///
/// One value per harness, measured against the real binary before anything
/// branched on it. The variants are the two spellings actually observed, plus
/// the honest absence of a reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expansion {
    /// `/name` written into the prompt resolves by itself. Jod forwards the
    /// line as typed and stays out of the way.
    Prompt,
    /// The harness expands its own commands, but only when the name arrives in
    /// a flag rather than in the prompt. OpenCode: `run --command <name>`.
    ///
    /// Worth its own variant rather than being lumped in with `Prompt`, because
    /// the difference is invisible in the happy case and expensive in the real
    /// one. Given `/name` in the message, OpenCode passed the literal text to
    /// the model, which went hunting with `ls` and `cat`, found the file
    /// because it happened to be in the working directory, and answered
    /// correctly. A run that looks right for the wrong reason is the failure
    /// this variant exists to keep Jod from shipping.
    Flag,
    /// Not yet measured. Nothing may branch on this value — it exists so that
    /// "we have not checked" is representable and cannot be mistaken for
    /// "it does not work".
    Unmeasured,
}

impl Expansion {
    /// What was measured for this harness. See `docs/harness-support.md` for
    /// the commands these readings came from.
    pub fn for_harness(kind: HarnessKind) -> Expansion {
        match kind {
            // `/jodcmd` returned `CMDFIRED` and `/jodskill` returned
            // `SKILLFIRED`, both with no intervening tool call.
            HarnessKind::ClaudeCode => Expansion::Prompt,
            // `/jodcmd` in the message was not expanded; `--command jodcmd`
            // was, in one step.
            HarnessKind::OpenCode => Expansion::Flag,
            // `/jodskill` returned `SKILLFIRED`, and the control run proves the
            // syntax is genuinely parsed: an unknown `/name` is refused with a
            // list of the commands AGY does know, rather than passed through as
            // prose.
            HarnessKind::Agy => Expansion::Prompt,
        }
    }
}

/// How the files under one directory are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `<dir>/<name>.md` — the name is the file stem.
    Flat,
    /// `<dir>/<name>/SKILL.md` — the name is the directory.
    Nested,
}

/// One place a harness keeps its customisations.
struct Source {
    /// Relative to the root being scanned.
    dir: &'static str,
    kind: Kind,
    layout: Layout,
    /// The harnesses measured to read this directory. More than one is not a
    /// guess — `.agents/skills/` was watched being loaded by two of them.
    harnesses: &'static [HarnessKind],
}

/// Every directory Jod scans under a root, and nothing on speculation.
///
/// A path nobody has been seen reading is worse than an absent row: it looks
/// like coverage. So AGY has no entry for a command directory — it has no such
/// concept, its customisations being Skills and Rules — and OpenCode's
/// directory is the singular `command`, because that is the one the probe
/// resolved, even though the binary contains both spellings.
const ROOT_SOURCES: &[Source] = &[
    Source {
        dir: ".claude/commands",
        kind: Kind::Command,
        layout: Layout::Flat,
        harnesses: &[HarnessKind::ClaudeCode],
    },
    Source {
        dir: ".claude/skills",
        kind: Kind::Skill,
        layout: Layout::Nested,
        harnesses: &[HarnessKind::ClaudeCode],
    },
    Source {
        dir: ".opencode/command",
        kind: Kind::Command,
        layout: Layout::Flat,
        harnesses: &[HarnessKind::OpenCode],
    },
    // The portable toolkit the charter describes, so a Jod checkout is already
    // a root full of these. Recorded against two harnesses rather than left
    // blank: AGY resolved a skill here from `/name`, and OpenCode loaded the
    // same directory through its own `skill` tool unprompted. Whether Claude
    // Code reads `.agents/skills/` was not measured, and a blank harness would
    // have claimed it does.
    Source {
        dir: ".agents/skills",
        kind: Kind::Skill,
        layout: Layout::Nested,
        harnesses: &[HarnessKind::Agy, HarnessKind::OpenCode],
    },
];

/// The user's own config, available under every root.
///
/// Only Claude Code's, and only its commands. This is the directory Claude Code
/// documents at user scope; whether the other two have an equivalent was not
/// established, so they get no entry rather than an invented one.
const USER_SOURCES: &[Source] = &[Source {
    dir: ".claude/commands",
    kind: Kind::Command,
    layout: Layout::Flat,
    harnesses: &[HarnessKind::ClaudeCode],
}];

/// Everything the palette can offer for these roots, plus the user's own.
///
/// A root that does not exist, or a directory that cannot be read, contributes
/// nothing and does not fail the scan: roots outlive the directories they point
/// at, and a palette that refuses to open because one repository was unmounted
/// would be worse than one listing less. A file that cannot be *read* is
/// likewise skipped rather than fatal — but note this is the only place
/// silence is acceptable, and it is silence about one file, not about a check.
///
/// Results are sorted, so a caller can compare two scans and the palette does
/// not reshuffle between keystrokes.
pub fn scan(roots: &[PathBuf]) -> Result<Vec<Discovered>> {
    let at = now_ms();
    let mut found = Vec::new();
    for root in roots {
        for source in ROOT_SOURCES {
            collect(&root.join(source.dir), source, Scope::Root, root, at, &mut found);
        }
    }
    if let Some(home) = home_dir() {
        for source in USER_SOURCES {
            // Empty root: user config belongs to no repository, and the column
            // is keyed on that emptiness.
            collect(
                &home.join(source.dir),
                source,
                Scope::User,
                Path::new(""),
                at,
                &mut found,
            );
        }
    }
    found.sort_by(|a, b| {
        (a.scope.as_str(), &a.root, a.kind.as_str(), &a.name, &a.harness).cmp(&(
            b.scope.as_str(),
            &b.root,
            b.kind.as_str(),
            &b.name,
            &b.harness,
        ))
    });
    Ok(found)
}

/// Read one directory into `out`, one entry per harness that reads it.
fn collect(
    dir: &Path,
    source: &Source,
    scope: Scope,
    root: &Path,
    at: i64,
    out: &mut Vec<Discovered>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let (name, file) = match source.layout {
            Layout::Flat => {
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                (stem.to_string(), path.clone())
            }
            Layout::Nested => {
                let manifest = path.join("SKILL.md");
                if !manifest.is_file() {
                    continue;
                }
                let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                (dir_name.to_string(), manifest)
            }
        };
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let description = describe(&text);
        for harness in source.harnesses {
            out.push(Discovered {
                id: 0,
                root: root.to_path_buf(),
                scope,
                kind: source.kind,
                name: name.clone(),
                description: description.clone(),
                path: file.clone(),
                harness: harness.id().to_string(),
                // Deliberately empty; see the field's own comment.
                body: String::new(),
                scanned_at_ms: at,
            });
        }
    }
}

/// One line describing the command, for the palette's right-hand column.
///
/// Front-matter `description:` wins because it is the only one the author wrote
/// *as* a description. Failing that, the first heading, then the first line of
/// prose — both of which are guesses at intent, which is why they come second
/// and third rather than first.
fn describe(text: &str) -> String {
    if let Some(found) = front_matter_description(text) {
        return found;
    }
    let body = strip_front_matter(text);
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(heading) = line.strip_prefix('#') {
            return heading.trim_start_matches('#').trim().to_string();
        }
        return line.to_string();
    }
    String::new()
}

/// `description:` from a leading `---` block, if there is one.
///
/// Deliberately a scan of the block's own lines rather than a YAML parse: the
/// front matter of a skill is hand-written and frequently not valid YAML, and a
/// parser that rejects the whole file over an unquoted colon somewhere else
/// would lose a description that is sitting right there in plain sight.
fn front_matter_description(text: &str) -> Option<String> {
    let block = front_matter(text)?;
    for line in block.lines() {
        if let Some(rest) = line.trim().strip_prefix("description:") {
            let value = rest.trim().trim_matches(['"', '\'']).trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The text between a leading `---` and the next `---`, if the file opens with
/// one.
fn front_matter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Everything after the front matter, so a heading inside the block is never
/// mistaken for the description.
///
/// Offsets are derived from the block's position rather than searched for.
/// Searching for the block's *text* would find the wrong copy of it in a file
/// that repeats itself, which is rare enough to survive review and produce a
/// description sliced out of the middle of a sentence.
fn strip_front_matter(text: &str) -> &str {
    let Some(block) = front_matter(text) else {
        return text;
    };
    // `front_matter` borrows from `text`, so the block's own address gives its
    // offset exactly.
    let start = block.as_ptr() as usize - text.as_ptr() as usize;
    text[start + block.len()..].trim_start_matches(['\n', '\r', '-'])
}

impl Store {
    /// Replace what is cached for these roots with a fresh scan.
    ///
    /// A replacement rather than an upsert, because the interesting change is
    /// usually a *deletion*: a command renamed or removed from a repository
    /// must leave the palette, and an upsert-only cache would offer it forever.
    /// The delete and the inserts share one transaction, so a palette opened
    /// mid-rescan sees the old set or the new one and never an empty one.
    ///
    /// Scoped to the roots just scanned, plus user config, so caching one
    /// conversation's roots cannot evict another's.
    pub fn cache_discovered(&self, roots: &[PathBuf], found: &[Discovered]) -> Result<()> {
        self.write(|tx| {
            for root in roots {
                tx.execute(
                    "DELETE FROM discovered_commands WHERE scope = 'root' AND root = ?1",
                    params![root.to_string_lossy()],
                )?;
            }
            tx.execute("DELETE FROM discovered_commands WHERE scope = 'user'", [])?;
            for d in found {
                tx.execute(
                    "INSERT OR REPLACE INTO discovered_commands
                       (root, scope, kind, name, description, path, harness, body, scanned_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        d.root.to_string_lossy(),
                        d.scope.as_str(),
                        d.kind.as_str(),
                        d.name,
                        d.description,
                        d.path.to_string_lossy(),
                        d.harness,
                        d.body,
                        d.scanned_at_ms,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Everything cached, in the order [`scan`] produced it.
    ///
    /// `harness` filters to one convention — what the palette wants, since a
    /// command is only offered to the harness that can resolve it.
    pub fn discovered(&self, harness: Option<HarnessKind>) -> Result<Vec<Discovered>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, root, scope, kind, name, description, path, harness, body, scanned_at_ms
               FROM discovered_commands
              WHERE (?1 IS NULL OR harness = ?1)
              ORDER BY scope, root, kind, name, harness",
        )?;
        let rows = stmt.query_map(params![harness.map(|h| h.id())], |r| {
            Ok(Discovered {
                id: r.get(0)?,
                root: PathBuf::from(r.get::<_, String>(1)?),
                scope: Scope::parse(&r.get::<_, String>(2)?),
                kind: Kind::parse(&r.get::<_, String>(3)?),
                name: r.get(4)?,
                description: r.get(5)?,
                path: PathBuf::from(r.get::<_, String>(6)?),
                harness: r.get(7)?,
                body: r.get(8)?,
                scanned_at_ms: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a root with one file at `rel`, creating parents.
    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jod-commands-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Names come from the file stem for a command and the directory for a
    /// skill, which is what each harness's own resolver uses.
    #[test]
    fn every_measured_convention_is_found_under_a_root() {
        let root = temp();
        write(&root, ".claude/commands/deploy.md", "Ship it.\n");
        write(&root, ".claude/skills/reviewing/SKILL.md", "Review it.\n");
        write(&root, ".opencode/command/build.md", "Build it.\n");
        write(&root, ".agents/skills/planning/SKILL.md", "Plan it.\n");

        let found = scan(&[root.clone()]).unwrap();
        let named = |n: &str| found.iter().filter(|d| d.name == n).count();
        assert_eq!(named("deploy"), 1, "a Claude Code command");
        assert_eq!(named("reviewing"), 1, "a Claude Code skill");
        assert_eq!(named("build"), 1, "an OpenCode command");
        // Two harnesses were measured reading `.agents/skills`.
        assert_eq!(named("planning"), 2, "AGY and OpenCode both read this");

        let deploy = found.iter().find(|d| d.name == "deploy").unwrap();
        assert_eq!(deploy.kind, Kind::Command);
        assert_eq!(deploy.scope, Scope::Root);
        assert_eq!(deploy.harness, HarnessKind::ClaudeCode.id());
        assert_eq!(deploy.root, root);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory read by two harnesses yields one row each, so the palette
    /// can offer it to both without either claiming the other's convention.
    #[test]
    fn a_shared_directory_is_recorded_once_per_harness_that_reads_it() {
        let root = temp();
        write(&root, ".agents/skills/planning/SKILL.md", "Plan it.\n");
        let found = scan(&[root.clone()]).unwrap();
        let mut harnesses: Vec<&str> = found.iter().map(|d| d.harness.as_str()).collect();
        harnesses.sort();
        assert_eq!(harnesses, vec![HarnessKind::Agy.id(), HarnessKind::OpenCode.id()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The three fallbacks, in the order they are tried.
    #[test]
    fn a_description_prefers_front_matter_then_a_heading_then_prose() {
        assert_eq!(
            describe("---\nname: x\ndescription: From the front matter.\n---\n\n# Heading\n"),
            "From the front matter."
        );
        assert_eq!(describe("# The Heading\n\nprose\n"), "The Heading");
        assert_eq!(describe("Just prose, no heading.\n"), "Just prose, no heading.");
        assert_eq!(describe(""), "");
    }

    /// Front matter that is not valid YAML must still give up its description.
    /// Skill front matter is hand-written and often has an unquoted colon
    /// somewhere; losing a description that is sitting in plain sight because
    /// of a different line would be a poor trade.
    #[test]
    fn a_description_survives_front_matter_that_would_not_parse_as_yaml() {
        let text = "---\nname: x\nnote: this: has: colons\ndescription: Still readable.\n---\nbody\n";
        assert_eq!(describe(text), "Still readable.");
    }

    /// A heading *inside* the front matter must not become the description,
    /// and a file that repeats its own front-matter text must still be cut at
    /// the right offset.
    #[test]
    fn the_description_comes_from_after_the_front_matter() {
        let text = "---\nname: x\n---\n\n# The Real Heading\n\nname: x\n";
        assert_eq!(describe(text), "The Real Heading");
    }

    /// Quotes are the author's punctuation, not part of the description.
    #[test]
    fn a_quoted_description_loses_its_quotes() {
        assert_eq!(
            describe("---\ndescription: \"Quoted value.\"\n---\n"),
            "Quoted value."
        );
    }

    /// A root that has gone — an unmounted disk, a deleted worktree — must
    /// leave the palette shorter rather than unopenable.
    #[test]
    fn a_missing_root_contributes_nothing_and_does_not_fail() {
        let found = scan(&[PathBuf::from("/nonexistent/jod/root")]).unwrap();
        assert!(found.iter().all(|d| d.scope != Scope::Root));
    }

    /// Nothing but `.md` is a command, and a skill directory without its
    /// manifest is not a skill.
    #[test]
    fn files_that_are_not_commands_are_ignored() {
        let root = temp();
        write(&root, ".claude/commands/notes.txt", "not a command\n");
        write(&root, ".claude/commands/real.md", "a command\n");
        write(&root, ".claude/skills/empty/README.md", "no manifest here\n");
        let found = scan(&[root.clone()]).unwrap();
        let names: Vec<&str> = found
            .iter()
            .filter(|d| d.scope == Scope::Root)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, vec!["real"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two scans of an unchanged tree must agree, or the palette reshuffles
    /// under the cursor between keystrokes.
    #[test]
    fn the_order_is_stable_across_scans() {
        let root = temp();
        write(&root, ".claude/commands/b.md", "b\n");
        write(&root, ".claude/commands/a.md", "a\n");
        write(&root, ".agents/skills/z/SKILL.md", "z\n");
        let first = scan(&[root.clone()]).unwrap();
        let second = scan(&[root.clone()]).unwrap();
        let names = |v: &[Discovered]| -> Vec<String> {
            v.iter().map(|d| format!("{}/{}", d.harness, d.name)).collect()
        };
        assert_eq!(names(&first), names(&second));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The measurement, pinned. If a harness upgrade changes one of these, the
    /// doc and this line have to move together.
    #[test]
    fn expansion_records_what_was_measured_for_each_harness() {
        assert_eq!(
            Expansion::for_harness(HarnessKind::ClaudeCode),
            Expansion::Prompt
        );
        assert_eq!(Expansion::for_harness(HarnessKind::Agy), Expansion::Prompt);
        assert_eq!(
            Expansion::for_harness(HarnessKind::OpenCode),
            Expansion::Flag,
            "OpenCode does not expand `/name` written into the message"
        );
        for kind in HarnessKind::ALL {
            assert_ne!(
                Expansion::for_harness(kind),
                Expansion::Unmeasured,
                "{kind:?} has been measured; see docs/harness-support.md"
            );
        }
    }

    /// Nothing scanned carries a body. The inlining branch is gone, and a
    /// populated body would be the first sign of it growing back.
    #[test]
    fn no_discovered_command_carries_a_body_to_inline() {
        let root = temp();
        write(&root, ".claude/commands/deploy.md", "Ship it.\n");
        let found = scan(&[root.clone()]).unwrap();
        assert!(found.iter().all(|d| d.body.is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scope_and_kind_round_trip_through_their_stored_spellings() {
        for scope in [Scope::Root, Scope::User, Scope::Plugin] {
            assert_eq!(Scope::parse(scope.as_str()), scope);
        }
        for kind in [Kind::Command, Kind::Skill] {
            assert_eq!(Kind::parse(kind.as_str()), kind);
        }
    }

    #[test]
    fn a_scan_round_trips_through_the_cache() {
        let root = temp();
        write(&root, ".claude/commands/deploy.md", "---\ndescription: Ship it.\n---\n");
        write(&root, ".agents/skills/planning/SKILL.md", "# Plan\n");
        let store = Store::in_memory().unwrap();
        let found = scan(&[root.clone()]).unwrap();
        store.cache_discovered(&[root.clone()], &found).unwrap();

        let back = store.discovered(None).unwrap();
        assert_eq!(back.len(), found.len());
        let deploy = back.iter().find(|d| d.name == "deploy").unwrap();
        assert_eq!(deploy.description, "Ship it.");
        assert_eq!(deploy.kind, Kind::Command);
        assert_eq!(deploy.root, root);

        // Filtering is what the palette does: a command is only offered to the
        // harness whose convention can resolve it.
        let only_agy = store.discovered(Some(HarnessKind::Agy)).unwrap();
        assert!(only_agy.iter().all(|d| d.harness == HarnessKind::Agy.id()));
        assert!(only_agy.iter().any(|d| d.name == "planning"));
        assert!(!only_agy.iter().any(|d| d.name == "deploy"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A command removed from disk must leave the palette. An upsert-only
    /// cache would keep offering it forever, and choosing it would do nothing.
    #[test]
    fn a_deleted_command_leaves_the_cache_on_the_next_scan() {
        let root = temp();
        write(&root, ".claude/commands/gone.md", "Doomed.\n");
        write(&root, ".claude/commands/stays.md", "Kept.\n");
        let store = Store::in_memory().unwrap();
        store
            .cache_discovered(&[root.clone()], &scan(&[root.clone()]).unwrap())
            .unwrap();
        assert_eq!(store.discovered(None).unwrap().len(), 2);

        std::fs::remove_file(root.join(".claude/commands/gone.md")).unwrap();
        store
            .cache_discovered(&[root.clone()], &scan(&[root.clone()]).unwrap())
            .unwrap();
        let back = store.discovered(None).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "stays");

        let _ = std::fs::remove_dir_all(&root);
    }
}
