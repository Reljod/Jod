//! The catalog: the repositories Reljod actually works on.
//!
//! This module exists because of one sentence — "btw, let's fix this" — and
//! what it takes to make that sentence resolvable when it arrives by voice.
//!
//! ## Why the two nouns Jod already had are not enough
//!
//! A [`crate::works::Work`] is one intent and it *closes*. A
//! [`crate::roots::Root`] is a directory one conversation may read and it dies
//! with the session. Both are the right shape for "what is happening right
//! now" and the wrong shape for "which repository is he talking about", which
//! is a question whose answer outlives every session that ever asked it.
//!
//! So a project is the checkout itself: the thing said out loud, the thing
//! still there next month.
//!
//! ## Sticky, because speech is not self-contained
//!
//! Dictation drops most of the context typing would have carried. "Let's fix
//! this" names nothing at all, and demanding it did would reintroduce exactly
//! the friction voice was meant to remove. So the current project is *sticky*:
//! it persists on the conversation until something changes it, and a message
//! that names no project simply inherits it.
//!
//! The cost of stickiness is that it is silently wrong in precisely the case
//! it is most useful, so every resolution is recorded with *how* it was
//! reached — see [`How`]. A guess nobody can see is a guess nobody can
//! correct, which is the same rule the router already lives under.
//!
//! ## Matching is done here, not by the model
//!
//! [`resolve`] is ordinary string matching over names, aliases and path
//! basenames, and it deliberately runs *before* the orchestrator is asked
//! anything. When Reljod says a project's name, that is not a judgement call
//! and it should not cost a model round-trip, be susceptible to a prompt, or
//! vary between two identical instructions. The model is asked only when this
//! finds nothing — which is the case that genuinely needs judgement.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{JodError, Result};
use crate::store::Store;

/// How much of a project's note is carried into the orchestrator's context.
///
/// The catalog is prepended to every main-chat turn, so this is a per-turn
/// cost multiplied by the number of projects. One line each keeps a
/// twenty-project catalog affordable.
pub const MAX_NOTE_CHARS: usize = 120;

/// The longest name worth storing. Long enough for "jod-cloud infrastructure",
/// short enough that the panel never has to decide how to wrap.
pub const MAX_NAME_CHARS: usize = 60;

/// Whether a project is still in play.
///
/// `Archived` is a state rather than a deletion because the catalog's whole
/// job is to still answer "what was that repo called" months later, and a
/// deleted row answers nothing.
///
/// Two things separate the three states, and it helps to keep them apart.
/// *Inference* is whether an unqualified mention may land here, and only
/// `Active` allows it — see [`State::inferrable`] and [`resolve`]. *Listing*
/// is whether the project shows up when nobody asked for the whole catalog,
/// and there `Paused` sides with `Active`: [`Store::projects`] filters on
/// `state != 'archived'`, so pausing a project leaves it on every everyday
/// surface while archiving takes it off them. Naming a project outright works
/// in all three states, through [`Store::projects_by_name`].
///
/// **Nothing can reach `Paused` yet.** There is no `jod project pause` and no
/// MCP tool that sets it, so today it is a state the code understands and no
/// caller can produce. Whether to expose it is a product question about what
/// "dormant" should mean, and it is open — see P4 in
/// `tasks/30-project-managers.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Worked on now. The only state [`resolve`] will match by default.
    Active,
    /// Real but dormant: still listed everywhere `Active` is, but kept out of
    /// inference so a repo untouched for months cannot win a vague "let's fix
    /// this" against the one open on screen. Not reachable from the CLI or any
    /// tool yet.
    Paused,
    /// Finished or abandoned. Kept out of inference *and* off the default
    /// listing, so it appears only when archived entries are asked for.
    Archived,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Active => "active",
            State::Paused => "paused",
            State::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "paused" => State::Paused,
            "archived" => State::Archived,
            _ => State::Active,
        }
    }

    /// Whether an unqualified mention may land here.
    pub fn inferrable(&self) -> bool {
        matches!(self, State::Active)
    }
}

/// How a conversation came to be pointed at the project it is pointed at.
///
/// The distinction that matters is [`How::Sticky`]: it marks a resolution
/// where *nothing in the message named a project* and the previous one simply
/// carried. That is the resolution most likely to be silently wrong, and
/// keeping it separate from a real inference is what lets the panel show the
/// difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// Reljod said which one, or picked it in the panel.
    Human,
    /// Something in the message identified it — a name, an alias, a path.
    Inferred,
    /// The message named nothing; the conversation's existing project carried.
    Sticky,
}

impl How {
    pub fn as_str(&self) -> &'static str {
        match self {
            How::Human => "human",
            How::Inferred => "inferred",
            How::Sticky => "sticky",
        }
    }

    pub fn parse(s: &str) -> How {
        match s {
            "human" => How::Human,
            "sticky" => How::Sticky,
            _ => How::Inferred,
        }
    }

    /// Whether this resolution is worth showing Reljod unprompted.
    ///
    /// A human choice needs no confirmation — he made it. The other two are
    /// the machine deciding on his behalf.
    pub fn worth_showing(&self) -> bool {
        !matches!(self, How::Human)
    }
}

/// One repository in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    /// What it is called out loud.
    pub name: String,
    pub path: PathBuf,
    pub remote: Option<String>,
    /// Other things he might say for it, lowercased on the way in.
    pub aliases: Vec<String>,
    pub state: State,
    pub colour: String,
    pub notes: String,
    pub created_at_ms: i64,
    /// When work last happened here — the tiebreak for a vague instruction.
    pub last_touched_ms: i64,
    /// The conversation that owns this project over time, once one exists.
    ///
    /// `None` until the first instruction about this project reaches a manager.
    /// See [`crate::store::Store::manager_conversation`], and note that this
    /// column and not `conversations.pinned` is how a manager is found.
    pub manager_conversation_id: Option<String>,
}

impl Project {
    /// The directory name, used when a path has to be shown short.
    pub fn basename(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }

    /// Every string that should match this project when spoken.
    ///
    /// The basename is included even though it was never typed as an alias:
    /// "the Jod repo" is a thing said about a directory called `Jod`, and
    /// making him register that by hand would be busywork with a wrong
    /// default.
    pub fn spoken_forms(&self) -> Vec<String> {
        let mut forms = vec![self.name.to_lowercase()];
        forms.extend(self.aliases.iter().cloned());
        let base = self.basename().to_lowercase();
        if !forms.contains(&base) {
            forms.push(base);
        }
        forms.retain(|f| !f.is_empty());
        forms
    }

    /// One line for the orchestrator's context and for the panel.
    pub fn summary_line(&self) -> String {
        let mut line = format!("{} · {}", self.name, self.path.display());
        if !self.aliases.is_empty() {
            line.push_str(&format!(" · also called: {}", self.aliases.join(", ")));
        }
        if !self.notes.is_empty() {
            line.push_str(&format!(" · {}", self.notes));
        }
        line
    }

    /// Why nothing can be started in this project's directory, when nothing
    /// can be.
    ///
    /// [`Store::add_project`] looks at the path once, on the way in, and
    /// nothing looks at it again. A checkout that is deleted, renamed, or
    /// living on a disk that is no longer mounted leaves its row exactly as it
    /// was, so the catalog carries on offering it as somewhere to start a
    /// session and every reader sees a healthy entry.
    ///
    /// Nothing downstream catches it either, and both failures name something
    /// other than the project. A run opened here is launched, recorded, and
    /// reported as running, and then dies in the supervisor with `could not
    /// start "/home/reljod/.local/bin/claude": No such file or directory (os
    /// error 2)` — the operating system refusing the working directory, blamed
    /// on the harness binary, which reads as Claude Code being missing from
    /// the machine. `claim_worktree` fares no better: `toplevel` gives up on
    /// anything that is not a directory, so the session gets a blocking card
    /// saying the path "is not inside a git repository", which sends the
    /// reader to `git init` a directory that is not there.
    ///
    /// This looks at the disk every time rather than storing a column, because
    /// the answer changes without anybody touching the database. It returns a
    /// sentence rather than a flag so each surface can print it as it stands.
    ///
    /// The row itself is deliberately left alone. Deleting or archiving a
    /// project because its directory is absent would answer a question nobody
    /// asked — an unmounted disk and a worktree part-way through being rebuilt
    /// both look exactly like this, and both come back — and it would throw
    /// away the name, aliases and notes that are the catalog's whole point.
    /// So the catalog says what it sees and leaves the decision to whoever is
    /// reading.
    pub fn path_trouble(&self) -> Option<String> {
        match std::fs::metadata(&self.path) {
            Ok(meta) if meta.is_dir() => None,
            Ok(_) => Some(format!(
                "`{}` is a file now, not a directory, so no session can be started in it. \
                 Catalogue the checkout where it actually lives, or archive this entry.",
                self.path.display()
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(format!(
                "there is nothing at `{}` any more, so no session can be started in it. The \
                 checkout was deleted or renamed — catalogue it at the path it lives at now, \
                 or archive this entry if it is gone for good.",
                self.path.display()
            )),
            // Kept separate from "it is not there": a path this process cannot
            // look at is usually an unmounted disk or a parent directory whose
            // permissions changed, and the answer to that is to put it back
            // rather than to re-catalogue anything.
            Err(e) => Some(format!(
                "`{}` could not be read: {e}. No session can be started in it until this \
                 machine can reach it again — check whether the disk holding it is mounted.",
                self.path.display()
            )),
        }
    }
}

/// A project about to be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub path: PathBuf,
    pub remote: Option<String>,
    pub aliases: Vec<String>,
    pub notes: String,
}

impl NewProject {
    /// The ordinary case: a checkout, named after its directory.
    ///
    /// Naming it after the directory is a *starting* value, not a derivation —
    /// it is written into the row and stays put if the directory is later
    /// renamed, because the alias set is anchored to the name.
    pub fn at(path: impl Into<PathBuf>) -> NewProject {
        let path = path.into();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        NewProject {
            name,
            path,
            remote: None,
            aliases: Vec::new(),
            notes: String::new(),
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> NewProject {
        self.name = name.into();
        self
    }

    pub fn with_aliases(mut self, aliases: Vec<String>) -> NewProject {
        self.aliases = aliases;
        self
    }

    pub fn with_notes(mut self, notes: impl Into<String>) -> NewProject {
        self.notes = notes.into();
        self
    }

    pub fn with_remote(mut self, remote: impl Into<String>) -> NewProject {
        self.remote = Some(remote.into());
        self
    }
}

/// One recorded change of the current project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub id: i64,
    pub conversation_id: String,
    pub project_id: Option<String>,
    pub utterance: String,
    pub how: How,
    pub reason: String,
    /// Set once Reljod has overridden this resolution.
    pub corrected: bool,
    pub decided_at_ms: i64,
}

/// What [`resolve`] concluded from one utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Exactly one project was named.
    One { project_id: String, matched: String },
    /// Several were named, so the utterance does not identify one.
    ///
    /// Kept distinct from [`Match::None`] because the two want opposite
    /// treatment: nothing named means fall back to the sticky project, whereas
    /// two named means the sticky project is very likely wrong and the
    /// orchestrator should be the one to choose.
    Ambiguous { project_ids: Vec<String> },
    /// Nothing in the utterance named a project.
    None,
}

/// Find the project an utterance names, by ordinary string matching.
///
/// Runs before the orchestrator is consulted, and answers only the easy
/// question: did he *say* a name we know? Word-boundary matched, longest form
/// first, so a project called `jod` cannot be triggered by the word "jodel"
/// and `jod-cloud` beats `jod` when both would match.
///
/// Paused and archived projects are skipped: a repository untouched for months
/// should not be able to win an offhand mention against the one being worked
/// on. Naming one explicitly still works — that goes through [`Store::projects_by_name`].
pub fn resolve(utterance: &str, catalog: &[Project]) -> Match {
    let haystack = utterance.to_lowercase();

    // Longest first so a more specific name wins outright. Without this,
    // "jod-cloud is broken" matches both `jod` and `jod-cloud` and reports an
    // ambiguity that is not real.
    let mut forms: Vec<(usize, String, &Project)> = Vec::new();
    for p in catalog.iter().filter(|p| p.state.inferrable()) {
        for form in p.spoken_forms() {
            forms.push((form.chars().count(), form, p));
        }
    }
    forms.sort_by_key(|f| std::cmp::Reverse(f.0));

    let mut hits: Vec<(String, String)> = Vec::new();
    // Each claimed span carries the project that claimed it. The owner is the
    // half that makes a shared name visible — without it, "these words are
    // already spoken for" and "these words name a second project" look
    // identical here, and the second project is dropped.
    let mut claimed: Vec<(usize, usize, String)> = Vec::new();
    for (_, form, project) in forms {
        let Some(span) = word_span(&haystack, &form) else {
            continue;
        };
        // A *longer* form already covering this span has spoken for it: `jod`
        // inside `jod-cloud` is not a second project being mentioned.
        //
        // A form covering exactly the same span is not a longer form. If it
        // belongs to a different project — two checkouts with the same
        // basename, or one alias typed on two projects — then one phrase has
        // named both of them, which is precisely the ambiguity this function
        // exists to report. Dropping it here is how a shared name came back as
        // `Match::One`, and `settle_project` then filed the instruction
        // against whichever project the catalog happened to list first.
        let spoken_for = claimed.iter().any(|(s, e, owner)| {
            span.0 >= *s && span.1 <= *e && (span != (*s, *e) || owner == &project.id)
        });
        if spoken_for {
            continue;
        }
        claimed.push((span.0, span.1, project.id.clone()));
        if !hits.iter().any(|(id, _)| id == &project.id) {
            hits.push((project.id.clone(), form));
        }
    }

    match hits.len() {
        0 => Match::None,
        1 => {
            let (project_id, matched) = hits.remove(0);
            Match::One {
                project_id,
                matched,
            }
        }
        _ => Match::Ambiguous {
            project_ids: hits.into_iter().map(|(id, _)| id).collect(),
        },
    }
}

/// Where `needle` appears in `haystack` as a whole word, if it does.
///
/// "Whole word" has to tolerate the punctuation a transcript carries and the
/// hyphens a repository name contains, so the boundary test is
/// alphanumeric-on-either-side rather than a regex over `\b` — `jod-cloud`
/// must match inside "fix jod-cloud, please" while `jod` must not match inside
/// "jodel".
fn word_span(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return Some((start, end));
        }
        // Advance past this occurrence rather than by one byte, so the scan
        // stays on a char boundary for multi-byte names.
        from = end;
    }
    None
}

/// Normalise a path to the single spelling this module stores.
///
/// Same reasoning as [`crate::roots::normalise`], and deliberately the same
/// behaviour: a path that does not resolve is kept as given rather than
/// refused, because a caller can legitimately be ahead of the filesystem.
pub fn normalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Tidy one alias into the form [`resolve`] matches against.
fn clean_alias(alias: &str) -> String {
    alias.trim().to_lowercase()
}

const PROJECT_COLUMNS: &str = "id, name, path, remote, aliases, state, colour, notes,
     created_at_ms, last_touched_ms, manager_conversation_id";

fn read_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let aliases: String = r.get(4)?;
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        path: PathBuf::from(r.get::<_, String>(2)?),
        remote: r.get(3)?,
        // A row whose JSON is unreadable degrades to "no aliases" rather than
        // failing the whole listing: one bad row must not make the catalog
        // unreadable, and the name and path still match.
        aliases: serde_json::from_str(&aliases).unwrap_or_default(),
        state: State::parse(&r.get::<_, String>(5)?),
        colour: r.get(6)?,
        notes: r.get(7)?,
        created_at_ms: r.get(8)?,
        last_touched_ms: r.get(9)?,
        manager_conversation_id: r.get(10)?,
    })
}

fn read_resolution(r: &rusqlite::Row<'_>) -> rusqlite::Result<Resolution> {
    Ok(Resolution {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        project_id: r.get(2)?,
        utterance: r.get(3)?,
        how: How::parse(&r.get::<_, String>(4)?),
        reason: r.get(5)?,
        corrected: r.get::<_, i64>(6)? != 0,
        decided_at_ms: r.get(7)?,
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    /// Put a repository in the catalog, or update the entry already there.
    ///
    /// Adding a path twice is not an error and does not duplicate it — the
    /// same call is how you rename a project or give it a new alias set, which
    /// is what every caller wants: the picker cannot know whether this
    /// directory has been added before, and neither can a voice instruction.
    /// The path is canonicalised first, so a symlink into a directory already
    /// in the catalog updates that entry rather than adding a second one.
    ///
    /// A field the second call leaves empty is left alone rather than emptied.
    /// Passing aliases replaces the alias set and passing notes replaces the
    /// notes, but passing neither keeps both, because renaming a project must
    /// not quietly delete what somebody typed. An empty value carries no way
    /// to tell "I did not mention this" apart from "I want this gone", and of
    /// the two readings only one is safe. Clearing an alias set or a note is
    /// therefore a thing to ask for through its own flag.
    ///
    /// `last_touched_ms` is deliberately *not* refreshed on a repeat add.
    /// Editing a catalog entry is not working in the repository, and letting
    /// an edit fake recency would corrupt the tiebreak that inference depends
    /// on.
    ///
    /// The path has to be a directory that is actually there, and a path that
    /// is not one is refused rather than written. A project is somewhere a
    /// session gets started, so a file or a typo can never become one, and
    /// everything downstream finds that out far too late to say anything
    /// useful about it. A run opened on such a path is launched, recorded, and
    /// then dies in the supervisor with `could not start ".../claude": Not a
    /// directory (os error 20)` — a message about the harness binary that
    /// names neither the project nor the path. `claim_worktree` fares no
    /// better: it decides the path "is not inside a git repository", which is
    /// plainly untrue when the file sits in one. This is the last point where
    /// the mistake is still cheap to explain, so it is explained here.
    pub fn add_project(&self, new: NewProject) -> Result<Project> {
        let name: String = new.name.trim().chars().take(MAX_NAME_CHARS).collect();
        if name.is_empty() {
            return Err(JodError::Invalid(
                "a project needs a name: it is what an instruction says out loud, and \
                 an unnamed row can never be matched by one"
                    .into(),
            ));
        }
        let path = normalise(&new.path);
        // Each refusal names the path and says what to do about it, because
        // the two mistakes have different answers: a file wants the directory
        // holding it, and a path that is not there wants either a correction
        // or the checkout to be made first.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                let parent = path
                    .parent()
                    .map(|p| format!("`{}`", p.display()))
                    .unwrap_or_else(|| "the directory holding it".to_string());
                return Err(JodError::Invalid(format!(
                    "`{}` is a file, not a directory. A project is a checkout a session gets \
                     started in, so catalogue {parent} instead — or, if that is not the \
                     repository you meant, the checkout that is.",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(JodError::Invalid(format!(
                    "there is nothing at `{}`. A project is a directory a session gets started \
                     in, so check the path for a typo, or make the checkout first and add it \
                     once it is there.",
                    path.display()
                )));
            }
            // Something is at that path and this process cannot look at it —
            // an unreadable parent directory, usually. Refused for the same
            // reason as the two above rather than assumed to be fine: a
            // catalog entry nobody can read is one every session will trip
            // over, and the error says which one it is.
            Err(e) => {
                return Err(JodError::Invalid(format!(
                    "`{}` could not be read: {e}. A project has to be a directory this machine \
                     can reach, so fix that and add it again.",
                    path.display()
                )));
            }
        }
        let text = path.to_string_lossy().to_string();
        let notes: String = new.notes.trim().chars().take(MAX_NOTE_CHARS).collect();

        let mut aliases: Vec<String> = new.aliases.iter().map(|a| clean_alias(a)).collect();
        aliases.retain(|a| !a.is_empty() && a != &name.to_lowercase());
        aliases.sort();
        aliases.dedup();
        let aliases_json = serde_json::to_string(&aliases).unwrap_or_else(|_| "[]".into());

        let at = now_ms();
        self.write(|tx| {
            let taken: Vec<String> = {
                let mut stmt =
                    tx.prepare("SELECT colour FROM projects WHERE state != 'archived'")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            tx.execute(
                "INSERT INTO projects
                   (id, name, path, remote, aliases, state, colour, notes,
                    created_at_ms, last_touched_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?8, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                   name    = excluded.name,
                   remote  = COALESCE(excluded.remote, projects.remote),
                   -- An empty incoming value means the caller did not mention
                   -- this field, not that they want it cleared. The COALESCE
                   -- on remote one line up is the same rule for a column that
                   -- can be NULL. These two default to an empty list and an
                   -- empty string instead, so the comparison has to be spelled
                   -- out. Without it, adding a path a second time just to
                   -- rename it deletes the aliases and the notes somebody
                   -- typed, silently and with no way to get them back.
                   -- Clearing either one is something to ask for on purpose
                   -- through its own flag, never a side effect of a rename, so
                   -- please do not simplify these back into plain assignments.
                   aliases = CASE WHEN excluded.aliases IN ('[]', '')
                                  THEN projects.aliases
                                  ELSE excluded.aliases END,
                   notes   = CASE WHEN excluded.notes = ''
                                  THEN projects.notes
                                  ELSE excluded.notes END",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    name,
                    text,
                    new.remote,
                    aliases_json,
                    crate::works::colour_for(&taken),
                    notes,
                    at,
                ],
            )?;
            Ok(tx.query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE path = ?1"),
                params![text],
                read_project,
            )?)
        })
    }

    pub fn project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
                params![id],
                read_project,
            )
            .optional()?)
    }

    /// The catalog, most recently worked in first.
    ///
    /// The order is the whole point: it is what makes the first entry the best
    /// guess for a vague instruction, and what puts the panel in the order he
    /// would have sorted it by hand.
    pub fn projects(&self, include_archived: bool) -> Result<Vec<Project>> {
        let where_clause = if include_archived {
            ""
        } else {
            " WHERE state != 'archived'"
        };
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects{where_clause}
              ORDER BY last_touched_ms DESC, name"
        ))?;
        let rows = stmt.query_map([], read_project)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every project that answers to a name, in catalog order.
    ///
    /// Unlike [`resolve`], this searches archived entries too: naming one
    /// explicitly is an instruction, not a guess, and refusing to find a
    /// project he just named because it is archived would be obtuse.
    ///
    /// It hands back a list rather than the first match because a spoken form
    /// can belong to more than one project: two checkouts called `proj` under
    /// different parents both answer to `proj`. Taking the first one meant
    /// `jod project archive proj` archived whichever row the catalog ordering
    /// put on top and said nothing about the other, and `project_switch` moved
    /// the conversation the same way. Deciding what to do about more than one
    /// belongs to the caller, who is the only one that knows whether there is
    /// a person there to ask — so please do not add a convenience wrapper that
    /// returns element zero.
    pub fn projects_by_name(&self, name: &str) -> Result<Vec<Project>> {
        let wanted = name.trim().to_lowercase();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .projects(true)?
            .into_iter()
            .filter(|p| p.spoken_forms().iter().any(|f| f == &wanted))
            .collect())
    }

    /// The catalog entry covering a directory, if one does.
    ///
    /// Matches a parent too, so a path inside a checkout finds the checkout —
    /// a session working in `Jod/core` is working on Jod. The longest match
    /// wins, so a project nested inside another resolves to the inner one.
    pub fn project_for_path(&self, path: &Path) -> Result<Option<Project>> {
        let path = normalise(path);
        let mut best: Option<Project> = None;
        for p in self.projects(true)? {
            if path.starts_with(&p.path)
                && best
                    .as_ref()
                    .is_none_or(|b| p.path.as_os_str().len() > b.path.as_os_str().len())
            {
                best = Some(p);
            }
        }
        Ok(best)
    }

    /// Record that work actually happened in a project.
    ///
    /// Separate from every other write for the reason given on `add_project`:
    /// this is the only thing allowed to move `last_touched_ms`, because that
    /// column is load-bearing for inference and an edit must not be able to
    /// forge it.
    pub fn touch_project(&self, id: &str) -> Result<()> {
        let at = now_ms();
        self.write(|tx| {
            tx.execute(
                "UPDATE projects SET last_touched_ms = ?2 WHERE id = ?1",
                params![id, at],
            )?;
            Ok(())
        })
    }

    pub fn set_project_state(&self, id: &str, state: State) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE projects SET state = ?2 WHERE id = ?1",
                params![id, state.as_str()],
            )?;
            Ok(())
        })
    }

    /// The project a conversation is currently about.
    pub fn current_project(&self, conversation_id: &str) -> Result<Option<Project>> {
        let id: Option<String> = {
            let conn = self.conn.lock().expect("store lock poisoned");
            conn.query_row(
                "SELECT current_project_id FROM conversations WHERE id = ?1",
                params![conversation_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten()
        };
        match id {
            Some(id) => self.project(&id),
            None => Ok(None),
        }
    }

    /// Point a conversation at a project, and record why.
    ///
    /// The write and the audit row are one transaction on purpose: a current
    /// project with no resolution behind it is exactly the un-correctable
    /// state this module exists to prevent.
    ///
    /// Touching the project is part of the same act — pointing the chat at a
    /// repository *is* the most recent evidence of working in it, and it is
    /// what keeps the catalog ordered the way he would order it.
    pub fn set_current_project(
        &self,
        conversation_id: &str,
        project_id: Option<&str>,
        utterance: &str,
        how: How,
        reason: &str,
    ) -> Result<Resolution> {
        let at = now_ms();
        self.write(|tx| {
            tx.execute(
                "UPDATE conversations SET current_project_id = ?2 WHERE id = ?1",
                params![conversation_id, project_id],
            )?;
            if let Some(pid) = project_id {
                tx.execute(
                    "UPDATE projects SET last_touched_ms = ?2 WHERE id = ?1",
                    params![pid, at],
                )?;
            }
            tx.execute(
                "INSERT INTO project_resolutions
                   (conversation_id, project_id, utterance, how, reason, decided_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    conversation_id,
                    project_id,
                    utterance.trim(),
                    how.as_str(),
                    reason.trim(),
                    at
                ],
            )?;
            let id = tx.last_insert_rowid();
            Ok(tx.query_row(
                "SELECT id, conversation_id, project_id, utterance, how, reason,
                        corrected, decided_at_ms
                   FROM project_resolutions WHERE id = ?1",
                params![id],
                read_resolution,
            )?)
        })
    }

    /// What the conversation decided about projects, most recent first.
    pub fn project_resolutions(&self, conversation_id: &str, limit: i64) -> Result<Vec<Resolution>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_id, utterance, how, reason,
                    corrected, decided_at_ms
               FROM project_resolutions
              WHERE conversation_id = ?1
              ORDER BY decided_at_ms DESC, id DESC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![conversation_id, limit], read_resolution)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Mark the last resolution as one Reljod had to take back.
    ///
    /// Called when he overrides an inferred project. The flag is what turns
    /// "the machine guessed" into evidence that can be counted later, rather
    /// than a correction that vanishes the moment it is made.
    pub fn mark_resolution_corrected(&self, conversation_id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE project_resolutions SET corrected = 1
                  WHERE id = (SELECT id FROM project_resolutions
                               WHERE conversation_id = ?1
                               ORDER BY decided_at_ms DESC, id DESC LIMIT 1)",
                params![conversation_id],
            )?;
            Ok(())
        })
    }

    /// Settle which project an instruction is about, and point the
    /// conversation at it.
    ///
    /// The whole sticky-with-override rule in one place, so the CLI, the API
    /// and the MCP tool cannot each implement a slightly different version of
    /// it:
    ///
    /// 1. The utterance names exactly one project → that one, as
    ///    [`How::Inferred`].
    /// 2. It names none and one is already current → keep it, as
    ///    [`How::Sticky`], and say so.
    /// 3. It names none and none is current, or it names several → nothing is
    ///    settled here. Returning `None` is the honest answer: the case that
    ///    needs judgement is handed to the orchestrator rather than guessed at
    ///    by a string matcher.
    ///
    /// Rule 3 covers a shared spoken form as well as two different names, and
    /// what it does about it is nothing at all — no write to
    /// `conversations.current_project_id`, no `touch_project`. That is
    /// deliberate, and it is not the same as clearing the current project.
    /// This function runs before every model turn with nobody to ask, so its
    /// only two honest options are to leave the conversation where it already
    /// was or to empty it. Leaving it wins: the project already in play was
    /// settled by an earlier instruction that was not ambiguous, and throwing
    /// it away would destroy a correct answer because a later sentence
    /// happened to be unclear. Not writing is the part that matters — an
    /// ambiguous instruction can no longer move the conversation to a project
    /// Reljod never confirmed, and it can no longer push that project to the
    /// top of the catalog, where it would bias the next inference too.
    pub fn settle_project(
        &self,
        conversation_id: &str,
        utterance: &str,
    ) -> Result<Option<Resolution>> {
        let catalog = self.projects(false)?;
        match resolve(utterance, &catalog) {
            Match::One {
                project_id,
                matched,
            } => {
                let current = self.current_project(conversation_id)?;
                // Re-recording a resolution that changes nothing would bury
                // the real switches under noise on every single message.
                if current.as_ref().is_some_and(|c| c.id == project_id) {
                    self.touch_project(&project_id)?;
                    return Ok(None);
                }
                Ok(Some(self.set_current_project(
                    conversation_id,
                    Some(&project_id),
                    utterance,
                    How::Inferred,
                    &format!("the instruction said \"{matched}\""),
                )?))
            }
            Match::None => {
                let Some(current) = self.current_project(conversation_id)? else {
                    return Ok(None);
                };
                self.touch_project(&current.id)?;
                Ok(Some(Resolution {
                    id: 0,
                    conversation_id: conversation_id.to_string(),
                    project_id: Some(current.id),
                    utterance: utterance.trim().to_string(),
                    how: How::Sticky,
                    reason: "nothing in the instruction named a project, so the one this \
                             conversation was already about carried"
                        .into(),
                    corrected: false,
                    decided_at_ms: now_ms(),
                }))
            }
            Match::Ambiguous { .. } => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, aliases: &[&str]) -> Project {
        Project {
            id: name.to_string(),
            name: name.to_string(),
            path: PathBuf::from(format!("/home/reljod/repo/{name}")),
            remote: None,
            aliases: aliases.iter().map(|a| a.to_lowercase()).collect(),
            state: State::Active,
            colour: "cyan".into(),
            notes: String::new(),
            created_at_ms: 0,
            last_touched_ms: 0,
            manager_conversation_id: None,
        }
    }

    fn store_with_chat() -> (Store, String) {
        let store = Store::in_memory().unwrap();
        let convo = store
            .new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        (store, convo.id)
    }

    // ---- matching -------------------------------------------------------

    #[test]
    fn a_project_is_matched_by_its_name() {
        let catalog = vec![project("tetris", &[])];
        assert_eq!(
            resolve("let's fix tetris", &catalog),
            Match::One {
                project_id: "tetris".into(),
                matched: "tetris".into()
            }
        );
    }

    /// The reason aliases exist: he says "the tetris thing", never the path.
    #[test]
    fn a_project_is_matched_by_an_alias_he_would_actually_say() {
        let catalog = vec![project("tetris", &["the tetris thing", "the game"])];
        match resolve("btw let's get back to the game", &catalog) {
            Match::One { project_id, .. } => assert_eq!(project_id, "tetris"),
            other => panic!("an alias did not match: {other:?}"),
        }
    }

    /// The directory name is a thing said out loud, so it matches without
    /// having to be registered as an alias by hand.
    #[test]
    fn the_directory_name_matches_without_being_registered() {
        let mut p = project("my agent", &[]);
        p.path = PathBuf::from("/home/reljod/repo/Jod");
        match resolve("push the Jod changes", &[p]) {
            Match::One { project_id, .. } => assert_eq!(project_id, "my agent"),
            other => panic!("the basename did not match: {other:?}"),
        }
    }

    /// A bare instruction is the whole point of the sticky design — it must
    /// resolve to nothing here so the caller falls back rather than guessing.
    #[test]
    fn an_instruction_that_names_nothing_matches_nothing() {
        let catalog = vec![project("tetris", &[]), project("jod", &[])];
        assert_eq!(resolve("btw, let's fix this", &catalog), Match::None);
    }

    /// Substring matching would make every short project name a landmine.
    #[test]
    fn a_name_inside_a_longer_word_is_not_a_match() {
        let catalog = vec![project("jod", &[])];
        assert_eq!(resolve("I listened to a jodel", &catalog), Match::None);
    }

    /// Transcripts arrive with punctuation attached; a name followed by a
    /// comma is still that name.
    #[test]
    fn punctuation_around_a_name_does_not_prevent_a_match() {
        let catalog = vec![project("tetris", &[])];
        match resolve("fix tetris, please", &catalog) {
            Match::One { project_id, .. } => assert_eq!(project_id, "tetris"),
            other => panic!("punctuation broke the match: {other:?}"),
        }
    }

    /// The specific-beats-general rule. Without it, one spoken name reports an
    /// ambiguity that was never there.
    #[test]
    fn the_longer_name_wins_when_one_contains_the_other() {
        let catalog = vec![project("jod", &[]), project("jod-cloud", &[])];
        match resolve("deploy jod-cloud", &catalog) {
            Match::One { project_id, .. } => assert_eq!(project_id, "jod-cloud"),
            other => panic!("the longer name did not win: {other:?}"),
        }
    }

    /// Two genuinely different projects named in one breath is not something
    /// a string matcher may pick between.
    #[test]
    fn naming_two_projects_is_reported_as_ambiguous_not_guessed() {
        let catalog = vec![project("tetris", &[]), project("jod", &[])];
        match resolve("port tetris to jod", &catalog) {
            Match::Ambiguous { project_ids } => assert_eq!(project_ids.len(), 2),
            other => panic!("two projects were resolved to one: {other:?}"),
        }
    }

    /// The same ambiguity, arriving as one word instead of two. Two checkouts
    /// called `proj` under different parents both answer to `proj`, so the
    /// phrase names both of them. The second one matches the exact byte span
    /// the first has already claimed, and suppressing it there is what used to
    /// turn this into a confident `Match::One` pointing at whichever project
    /// the catalog happened to list first.
    #[test]
    fn a_form_two_projects_share_is_ambiguous_rather_than_a_coin_flip() {
        let mut first = project("collide-proj", &[]);
        first.path = PathBuf::from("/home/reljod/repo/collide/proj");
        let mut second = project("other-proj", &[]);
        second.path = PathBuf::from("/home/reljod/repo/other/proj");

        match resolve("fix the login bug in proj", &[first, second]) {
            Match::Ambiguous { project_ids } => {
                assert!(
                    project_ids.contains(&"collide-proj".to_string())
                        && project_ids.contains(&"other-proj".to_string()),
                    "both projects answer to `proj`, but only {project_ids:?} was reported"
                );
            }
            other => panic!("a shared spoken form resolved to one project: {other:?}"),
        }
    }

    /// The other route to a shared form: the same alias typed on two projects.
    /// The basename case above collides by accident, this one by hand, and the
    /// matcher has to treat them the same way.
    #[test]
    fn an_alias_two_projects_share_is_ambiguous_rather_than_a_coin_flip() {
        let catalog = vec![
            project("tetris", &["the game"]),
            project("jod", &["the game"]),
        ];
        match resolve("let's get back to the game", &catalog) {
            Match::Ambiguous { project_ids } => assert_eq!(project_ids.len(), 2),
            other => panic!("a shared alias resolved to one project: {other:?}"),
        }
    }

    /// A dormant repository must not win an offhand mention against the one
    /// being worked on.
    #[test]
    fn an_archived_project_is_not_inferred() {
        let mut p = project("tetris", &[]);
        p.state = State::Archived;
        assert_eq!(resolve("fix tetris", &[p]), Match::None);
    }

    /// Taglish is the input this is built for, so a name inside code-switched
    /// speech has to match exactly as it would in English.
    #[test]
    fn a_name_matches_inside_taglish() {
        let catalog = vec![project("tetris", &[])];
        match resolve("pwede ba nating i-fix yung tetris ngayon", &catalog) {
            Match::One { project_id, .. } => assert_eq!(project_id, "tetris"),
            other => panic!("Taglish broke the match: {other:?}"),
        }
    }

    /// A multi-byte utterance must not panic the byte-indexed scan.
    #[test]
    fn a_non_ascii_utterance_does_not_panic() {
        let catalog = vec![project("tetris", &[])];
        assert_eq!(resolve("ano na — 日本語 — ayos?", &catalog), Match::None);
    }

    // ---- the catalog ----------------------------------------------------

    #[test]
    fn adding_the_same_path_twice_updates_rather_than_duplicates() {
        let store = Store::in_memory().unwrap();
        store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        store
            .add_project(NewProject::at(checkout("alpha")).named("Alpha Prime"))
            .unwrap();
        let all = store.projects(false).unwrap();
        assert_eq!(all.len(), 1, "the same checkout became two rows");
        assert_eq!(all[0].name, "Alpha Prime");
    }

    /// A real directory to catalogue, made on demand.
    ///
    /// These used to be string literals like `/tmp/alpha`, which were never
    /// there. `add_project` refuses a path that is not a directory now, so a
    /// test that wants a project has to have somewhere for it to live. The
    /// last segment of the name is still the project's name and its spoken
    /// form, so the tests below read exactly as they did.
    ///
    /// It does not delete what it finds, unlike [`scratch`]: tests run in
    /// parallel in one process and several of them ask for the same directory.
    fn checkout(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("jod-projects-{}", std::process::id()))
            .join(name);
        std::fs::create_dir_all(&dir).expect("a scratch checkout");
        normalise(&dir)
    }

    /// A real directory to hang a symlink off, because `normalise`
    /// canonicalises and a link that points nowhere resolves to itself.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jod-projects-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        normalise(&dir)
    }

    /// Renaming a project is the ordinary reason to add its path a second
    /// time, and the second call has no idea what the first one recorded. If
    /// an unmentioned field counted as an instruction to empty it, every
    /// rename would quietly destroy an alias set and a note somebody typed.
    #[test]
    fn re_adding_a_path_to_rename_it_keeps_its_aliases_and_notes() {
        let store = Store::in_memory().unwrap();
        store
            .add_project(
                NewProject::at(checkout("alpha"))
                    .named("alpha")
                    .with_aliases(vec!["the game".into()])
                    .with_notes("test project alpha"),
            )
            .unwrap();

        let again = store
            .add_project(NewProject::at(checkout("alpha")).named("Alpha Prime"))
            .unwrap();

        assert_eq!(again.name, "Alpha Prime", "the rename did not take");
        assert_eq!(
            again.aliases,
            vec!["the game".to_string()],
            "a rename wiped an alias set the second call never mentioned"
        );
        assert_eq!(
            again.notes, "test project alpha",
            "a rename wiped notes the second call never mentioned"
        );
    }

    /// The second route into the same wipe. `normalise` canonicalises before
    /// the insert, so adding a symlink to a directory already in the catalog
    /// lands on the very same `ON CONFLICT(path)` branch as adding the real
    /// directory again. Fixing only the direct route would leave this one
    /// destroying data exactly as before.
    #[cfg(unix)]
    #[test]
    fn adding_a_symlink_to_a_catalogued_directory_keeps_its_aliases_and_notes() {
        let dir = scratch("symlink-add");
        let real = dir.join("beta");
        let link = dir.join("beta-link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let store = Store::in_memory().unwrap();
        store
            .add_project(
                NewProject::at(&real)
                    .named("beta")
                    .with_aliases(vec!["the sequel".into()])
                    .with_notes("test project beta"),
            )
            .unwrap();

        let again = store
            .add_project(NewProject::at(&link).named("Beta Via Link"))
            .unwrap();

        assert_eq!(
            store.projects(false).unwrap().len(),
            1,
            "the symlink became a second row, so this was never the same entry"
        );
        assert_eq!(again.path, real, "the symlink was stored unresolved");
        assert_eq!(again.name, "Beta Via Link", "the rename did not take");
        assert_eq!(
            again.aliases,
            vec!["the sequel".to_string()],
            "adding a symlink wiped the alias set of the directory it points at"
        );
        assert_eq!(
            again.notes, "test project beta",
            "adding a symlink wiped the notes of the directory it points at"
        );
    }

    /// The other half of the rule: an empty value is ignored, but a value that
    /// is actually there still replaces what was recorded before. Otherwise
    /// correcting a typo in a note would be impossible.
    #[test]
    fn re_adding_a_path_with_new_aliases_and_notes_replaces_the_old_ones() {
        let store = Store::in_memory().unwrap();
        store
            .add_project(
                NewProject::at(checkout("alpha"))
                    .named("alpha")
                    .with_aliases(vec!["the game".into()])
                    .with_notes("first note"),
            )
            .unwrap();

        let again = store
            .add_project(
                NewProject::at(checkout("alpha"))
                    .named("alpha")
                    .with_aliases(vec!["the second alias".into()])
                    .with_notes("second note"),
            )
            .unwrap();

        assert_eq!(again.aliases, vec!["the second alias".to_string()]);
        assert_eq!(again.notes, "second note");
    }

    /// `last_touched_ms` is the tiebreak inference leans on, so editing a row
    /// must not be able to forge recency.
    #[test]
    fn editing_a_project_does_not_count_as_working_in_it() {
        let store = Store::in_memory().unwrap();
        let first = store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        let again = store
            .add_project(NewProject::at(checkout("alpha")).with_notes("renamed"))
            .unwrap();
        assert_eq!(
            first.last_touched_ms, again.last_touched_ms,
            "an edit moved the recency the router sorts by"
        );
    }

    /// Stamps `last_touched_ms` directly. `touch_project` reads the wall
    /// clock, and two touches inside one millisecond tie — real use never does
    /// that, but a test that raced the clock would be flaky about the ordering
    /// rather than about the thing it is checking.
    fn touched_at(store: &Store, id: &str, at: i64) {
        store
            .write(|tx| {
                tx.execute(
                    "UPDATE projects SET last_touched_ms = ?2 WHERE id = ?1",
                    params![id, at],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn the_catalog_is_ordered_by_where_work_last_happened() {
        let store = Store::in_memory().unwrap();
        let alpha = store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        let beta = store.add_project(NewProject::at(checkout("beta"))).unwrap();
        touched_at(&store, &alpha.id, 1_000);
        touched_at(&store, &beta.id, 2_000);

        let all = store.projects(false).unwrap();
        assert_eq!(all[0].name, "beta", "the catalog is not ordered by recency");

        // And the order follows the work, rather than being fixed at creation.
        store.touch_project(&alpha.id).unwrap();
        assert_eq!(store.projects(false).unwrap()[0].name, "alpha");
    }

    #[test]
    fn an_archived_project_leaves_the_default_listing_but_not_the_table() {
        let store = Store::in_memory().unwrap();
        let p = store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        store.set_project_state(&p.id, State::Archived).unwrap();
        assert!(store.projects(false).unwrap().is_empty());
        assert_eq!(store.projects(true).unwrap().len(), 1);
    }

    /// The one thing `Paused` does that `Archived` does not.
    ///
    /// Nothing outside these tests can reach `State::Paused` today — there is
    /// no `jod project pause` and no MCP tool that sets it — so this pins what
    /// the state would mean if something could, and stops the answer drifting
    /// while the question of whether to expose it is still open. See P4 in
    /// `tasks/30-project-managers.md`.
    ///
    /// Both states are kept out of inference and both can still be named
    /// outright. They part company in one place, [`Store::projects`]: the
    /// default listing filters on `state != 'archived'` rather than on
    /// `state = 'active'`, so a paused project stays on every everyday surface
    /// — `jod project ls`, the `project_list` tool, the TUI panel, and the
    /// catalog the orchestrator is given each turn — while an archived one
    /// drops off all four until something asks for archived entries too.
    #[test]
    fn pausing_and_archiving_are_not_the_same_thing() {
        let store = Store::in_memory().unwrap();
        let dormant = store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        let finished = store.add_project(NewProject::at(checkout("beta"))).unwrap();
        store.set_project_state(&dormant.id, State::Paused).unwrap();
        store
            .set_project_state(&finished.id, State::Archived)
            .unwrap();

        // Where they differ: the default listing keeps the paused project and
        // drops the archived one.
        let listed: Vec<String> = store
            .projects(false)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(listed, vec!["alpha".to_string()]);
        assert_eq!(store.projects(true).unwrap().len(), 2);

        // Where they agree: neither is inferrable, and both answer to their
        // own name when one is given outright.
        let catalog = store.projects(true).unwrap();
        assert_eq!(resolve("fix alpha", &catalog), Match::None);
        assert_eq!(resolve("fix beta", &catalog), Match::None);
        assert_eq!(store.projects_by_name("alpha").unwrap().len(), 1);
        assert_eq!(store.projects_by_name("beta").unwrap().len(), 1);
    }

    /// A session working in a subdirectory is working on the project.
    #[test]
    fn a_path_inside_a_checkout_finds_the_checkout() {
        let store = Store::in_memory().unwrap();
        store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        let found = store
            .project_for_path(&checkout("alpha").join("core/src"))
            .unwrap();
        assert_eq!(found.map(|p| p.name), Some("alpha".into()));
    }

    #[test]
    fn a_nested_project_wins_over_the_one_containing_it() {
        let store = Store::in_memory().unwrap();
        store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        store.add_project(NewProject::at(checkout("alpha/apps/ios"))).unwrap();
        let found = store
            .project_for_path(&checkout("alpha/apps/ios").join("src"))
            .unwrap();
        assert_eq!(found.map(|p| p.name), Some("ios".into()));
    }

    /// Naming an archived project explicitly is an instruction, not a guess.
    #[test]
    fn an_archived_project_can_still_be_named_explicitly() {
        let store = Store::in_memory().unwrap();
        let p = store.add_project(NewProject::at(checkout("alpha"))).unwrap();
        store.set_project_state(&p.id, State::Archived).unwrap();
        assert_eq!(store.projects_by_name("alpha").unwrap().len(), 1);
    }

    /// The explicit half of the shared-name problem. `jod project archive`,
    /// `jod project restore` and the `project_switch` tool all reach a project
    /// through this lookup, and it used to return the first row that answered
    /// to the name. With two checkouts called `proj`, `jod project archive
    /// proj` archived one of them and said nothing about the other.
    #[test]
    fn a_name_two_projects_answer_to_returns_both_rather_than_the_first() {
        let store = Store::in_memory().unwrap();
        store
            .add_project(NewProject::at(checkout("collide/proj")).named("collide-proj"))
            .unwrap();
        store
            .add_project(NewProject::at(checkout("other/proj")).named("other-proj"))
            .unwrap();

        let found = store.projects_by_name("proj").unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            found.len(),
            2,
            "both checkouts are called `proj`, but the lookup reported {names:?}"
        );
        assert!(names.contains(&"collide-proj") && names.contains(&"other-proj"));

        // Naming one of them exactly is still unambiguous, which is what makes
        // "name one of them exactly" a usable answer to the refusal.
        assert_eq!(store.projects_by_name("other-proj").unwrap().len(), 1);
    }

    /// The consequence that makes this severe rather than untidy.
    /// `settle_project` runs on every instruction before the model turn, so a
    /// shared name silently picked here is an instruction filed against a
    /// project Reljod never named. Nothing must be written, and the project
    /// already in play must survive: it was settled by an earlier instruction
    /// that was not ambiguous, and a later unclear sentence is no reason to
    /// throw a correct answer away.
    #[test]
    fn a_shared_name_settles_nothing_and_leaves_the_current_project_alone() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store
            .add_project(NewProject::at(checkout("collide/proj")).named("collide-proj"))
            .unwrap();
        store
            .add_project(NewProject::at(checkout("other/proj")).named("other-proj"))
            .unwrap();
        store.settle_project(&chat, "let's fix tetris").unwrap();

        let settled = store
            .settle_project(&chat, "fix the login bug in proj")
            .unwrap();
        assert!(
            settled.is_none(),
            "a shared name settled a project: {settled:?}"
        );
        assert_eq!(
            store.current_project(&chat).unwrap().map(|p| p.name),
            Some("tetris".into()),
            "an ambiguous instruction moved the conversation"
        );
        assert_eq!(
            store.project_resolutions(&chat, 10).unwrap().len(),
            1,
            "an ambiguous instruction was recorded as a decision"
        );
    }

    #[test]
    fn a_project_needs_a_name() {
        let store = Store::in_memory().unwrap();
        assert!(store
            .add_project(NewProject::at(checkout("alpha")).named("   "))
            .is_err());
    }

    /// A project is a directory a session gets started in, so a file cannot be
    /// one. This used to be accepted in silence, and the row it wrote looked
    /// exactly like a healthy repository in `jod project ls`.
    ///
    /// What made it worth refusing here is what happens afterwards. A session
    /// started on that path never runs: the supervisor asks the operating
    /// system to change into it and is told `Not a directory (os error 20)`,
    /// which is reported against the harness binary and never mentions the
    /// project or the path. `claim_worktree` is no better — it decides the
    /// path "is not inside a git repository" even when the file sits in one.
    /// The catalog is the last place the mistake is still cheap to explain.
    #[test]
    fn a_file_is_refused_rather_than_catalogued_as_a_repository() {
        let dir = scratch("a-file-not-a-dir");
        let file = dir.join("afile.txt");
        std::fs::write(&file, "hello\n").unwrap();

        let store = Store::in_memory().unwrap();
        let refused = store
            .add_project(NewProject::at(&file).named("a-file-not-a-dir"))
            .expect_err("a plain file was catalogued as a repository");

        let said = refused.to_string();
        assert!(
            said.contains(&file.display().to_string()),
            "the refusal does not say which path was wrong: {said}"
        );
        assert!(
            said.contains(&dir.display().to_string()),
            "the refusal does not offer the directory holding the file: {said}"
        );
        assert!(
            store.projects(true).unwrap().is_empty(),
            "the refusal still wrote a row"
        );
    }

    /// The commoner way into the same broken row: a path with a typo in it.
    ///
    /// `normalise` canonicalises, and canonicalising something that is not
    /// there fails — but it deliberately falls back to the path as given
    /// rather than refusing, so nothing was validating this either. Downstream
    /// the failure is the same one a file produces, arriving just as late.
    #[test]
    fn a_path_that_is_not_there_is_refused_rather_than_catalogued() {
        let missing = scratch("no-such-checkout").join("nope");

        let store = Store::in_memory().unwrap();
        let refused = store
            .add_project(NewProject::at(&missing).named("a-path-that-is-not-there"))
            .expect_err("a path that does not exist was catalogued");

        let said = refused.to_string();
        assert!(
            said.contains(&missing.display().to_string()),
            "the refusal does not say which path was wrong: {said}"
        );
        assert!(
            store.projects(true).unwrap().is_empty(),
            "the refusal still wrote a row"
        );
    }

    /// The hole the two refusals above leave: a path that was fine when it was
    /// catalogued and went bad afterwards.
    ///
    /// `add_project` is the only thing that ever looks at a project's path, so
    /// deleting the checkout leaves the row exactly as it was and the listing
    /// goes on presenting it as a healthy repository. It is worse than a stale
    /// line of text, because the catalog is ordered by where work last
    /// happened: the checkout deleted five minutes ago sorts to the *top*, and
    /// is therefore the best guess for an instruction that names no project.
    #[test]
    fn a_checkout_deleted_after_it_was_catalogued_is_flagged_not_listed_as_healthy() {
        let dir = scratch("gone-after-cataloguing");
        let store = Store::in_memory().unwrap();
        let added = store
            .add_project(NewProject::at(&dir).named("ephemeral-proj"))
            .unwrap();
        assert!(
            added.path_trouble().is_none(),
            "a checkout that is really there was reported as missing"
        );

        std::fs::remove_dir_all(&dir).unwrap();

        let listed = store.projects(false).unwrap();
        let listed = listed
            .iter()
            .find(|p| p.id == added.id)
            .expect("the entry vanished from the listing — it is meant to be kept, and flagged");
        let said = listed
            .path_trouble()
            .expect("a deleted checkout is still listed as a healthy repository");
        assert!(
            said.contains(&dir.display().to_string()),
            "the warning does not say which path is gone: {said}"
        );
        assert!(
            said.contains("archive"),
            "the warning says what is wrong but not what to do about it: {said}"
        );
    }

    /// The row is kept, not quietly tidied away.
    ///
    /// An unmounted disk and a worktree part-way through being rebuilt both
    /// look exactly like a deleted checkout, and both come back. Archiving or
    /// deleting the row on their behalf would throw away the name, aliases and
    /// notes that are the only reason the catalog exists — so a missing
    /// directory changes what is *said* about a project and nothing else about
    /// it.
    #[test]
    fn a_missing_directory_does_not_archive_or_delete_the_project() {
        let dir = scratch("missing-but-kept");
        let store = Store::in_memory().unwrap();
        let added = store
            .add_project(
                NewProject::at(&dir)
                    .named("tetris")
                    .with_aliases(vec!["the game".into()])
                    .with_notes("worth keeping"),
            )
            .unwrap();

        std::fs::remove_dir_all(&dir).unwrap();

        let still = store.projects(false).unwrap();
        assert_eq!(still.len(), 1, "the entry was dropped from the catalog");
        assert_eq!(still[0].id, added.id);
        assert_eq!(still[0].state, State::Active, "the entry was auto-archived");
        assert_eq!(still[0].aliases, vec!["the game".to_string()]);
        assert_eq!(still[0].notes, "worth keeping");
        assert_eq!(
            store.projects_by_name("tetris").unwrap().len(),
            1,
            "a project whose directory is gone can no longer be named"
        );
    }

    /// A directory replaced by a file is the same hole from the other side, and
    /// it gets its own sentence because the answer differs: nothing is coming
    /// back, so the entry wants re-cataloguing or archiving.
    #[test]
    fn a_checkout_replaced_by_a_file_is_flagged_too() {
        let dir = scratch("replaced-by-a-file");
        let checkout = dir.join("proj");
        std::fs::create_dir_all(&checkout).unwrap();

        let store = Store::in_memory().unwrap();
        let added = store
            .add_project(NewProject::at(&checkout).named("replaced-proj"))
            .unwrap();

        std::fs::remove_dir_all(&checkout).unwrap();
        std::fs::write(&checkout, "not a checkout any more\n").unwrap();

        let said = added
            .path_trouble()
            .expect("a checkout replaced by a file is still reported as healthy");
        assert!(
            said.contains("file"),
            "the warning does not say the path is a file now: {said}"
        );
        assert!(
            said.contains(&checkout.display().to_string()),
            "the warning does not say which path is wrong: {said}"
        );
    }

    // ---- sticky resolution ----------------------------------------------

    #[test]
    fn naming_a_project_switches_the_conversation_to_it() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        let r = store.settle_project(&chat, "let's fix tetris").unwrap();
        assert_eq!(r.map(|r| r.how), Some(How::Inferred));
        assert_eq!(
            store.current_project(&chat).unwrap().map(|p| p.name),
            Some("tetris".into())
        );
    }

    /// The sentence this module was built for.
    #[test]
    fn a_bare_instruction_carries_the_project_already_in_play() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.settle_project(&chat, "let's fix tetris").unwrap();

        let r = store
            .settle_project(&chat, "btw, let's fix this")
            .unwrap()
            .expect("a bare instruction lost the sticky project");
        assert_eq!(r.how, How::Sticky);
        assert_eq!(
            store.current_project(&chat).unwrap().map(|p| p.name),
            Some("tetris".into())
        );
    }

    /// Stickiness must not be a trap: naming another project switches away.
    #[test]
    fn naming_another_project_overrides_the_sticky_one() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.add_project(NewProject::at(checkout("jod"))).unwrap();
        store.settle_project(&chat, "fix tetris").unwrap();
        store.settle_project(&chat, "now deploy jod").unwrap();
        assert_eq!(
            store.current_project(&chat).unwrap().map(|p| p.name),
            Some("jod".into())
        );
    }

    /// Nothing is current and nothing was named: guessing here would be the
    /// machine inventing a subject.
    #[test]
    fn a_bare_instruction_with_no_project_in_play_settles_nothing() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        assert!(store.settle_project(&chat, "let's fix this").unwrap().is_none());
        assert!(store.current_project(&chat).unwrap().is_none());
    }

    /// Two named projects is exactly when the sticky one is most likely wrong,
    /// so it must not quietly survive as the answer.
    #[test]
    fn an_ambiguous_instruction_is_left_for_the_orchestrator() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.add_project(NewProject::at(checkout("jod"))).unwrap();
        assert!(store
            .settle_project(&chat, "port tetris to jod")
            .unwrap()
            .is_none());
    }

    /// Every switch has to leave a trail, or a wrong guess is uncorrectable.
    #[test]
    fn a_switch_is_recorded_with_the_words_that_caused_it() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.settle_project(&chat, "let's fix tetris").unwrap();
        let log = store.project_resolutions(&chat, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].utterance, "let's fix tetris");
        assert!(log[0].reason.contains("tetris"));
    }

    /// A resolution row per message would bury the real switches.
    #[test]
    fn repeating_the_same_project_does_not_add_a_resolution() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.settle_project(&chat, "fix tetris").unwrap();
        store.settle_project(&chat, "tetris again").unwrap();
        assert_eq!(store.project_resolutions(&chat, 10).unwrap().len(), 1);
    }

    #[test]
    fn an_override_marks_the_guess_it_took_back() {
        let (store, chat) = store_with_chat();
        let t = store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.settle_project(&chat, "fix tetris").unwrap();
        store.mark_resolution_corrected(&chat).unwrap();
        let log = store.project_resolutions(&chat, 10).unwrap();
        assert!(log[0].corrected);
        assert_eq!(log[0].project_id.as_deref(), Some(t.id.as_str()));
    }

    /// Archiving the catalog entry must not take the conversation with it.
    #[test]
    fn deleting_a_project_does_not_delete_the_chat_about_it() {
        let (store, chat) = store_with_chat();
        let p = store.add_project(NewProject::at(checkout("tetris"))).unwrap();
        store.settle_project(&chat, "fix tetris").unwrap();
        store.write(|tx| {
            tx.execute("DELETE FROM projects WHERE id = ?1", params![p.id])?;
            Ok(())
        })
        .unwrap();
        assert!(store.current_project(&chat).unwrap().is_none());
        assert!(store.conversation(&chat).unwrap().is_some());
    }
}
