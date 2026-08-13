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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Worked on now. The only state [`resolve`] will match by default.
    Active,
    /// Real but dormant. Kept out of inference so a repo untouched for months
    /// cannot win a vague "let's fix this" against the one open on screen.
    Paused,
    /// Finished or abandoned. Listed only when explicitly asked for.
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
/// on. Naming one explicitly still works — that goes through [`Store::project_by_name`].
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
    let mut claimed: Vec<(usize, usize)> = Vec::new();
    for (_, form, project) in forms {
        let Some(span) = word_span(&haystack, &form) else {
            continue;
        };
        // A longer form already covering this span has spoken for it: `jod`
        // inside `jod-cloud` is not a second project being mentioned.
        if claimed.iter().any(|(s, e)| span.0 >= *s && span.1 <= *e) {
            continue;
        }
        claimed.push(span);
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
     created_at_ms, last_touched_ms";

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
    /// same call is how you rename a project or extend its alias set, which is
    /// what every caller wants: the picker cannot know whether this directory
    /// has been added before, and neither can a voice instruction.
    ///
    /// `last_touched_ms` is deliberately *not* refreshed on a repeat add.
    /// Editing a catalog entry is not working in the repository, and letting
    /// an edit fake recency would corrupt the tiebreak that inference depends
    /// on.
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
                   aliases = excluded.aliases,
                   notes   = excluded.notes",
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

    /// Look a project up by anything it might be called.
    ///
    /// Unlike [`resolve`], this searches archived entries too: naming one
    /// explicitly is an instruction, not a guess, and refusing to find a
    /// project he just named because it is archived would be obtuse.
    pub fn project_by_name(&self, name: &str) -> Result<Option<Project>> {
        let wanted = name.trim().to_lowercase();
        if wanted.is_empty() {
            return Ok(None);
        }
        Ok(self
            .projects(true)?
            .into_iter()
            .find(|p| p.spoken_forms().iter().any(|f| f == &wanted)))
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
        store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        store
            .add_project(NewProject::at("/tmp/alpha").named("Alpha Prime"))
            .unwrap();
        let all = store.projects(false).unwrap();
        assert_eq!(all.len(), 1, "the same checkout became two rows");
        assert_eq!(all[0].name, "Alpha Prime");
    }

    /// `last_touched_ms` is the tiebreak inference leans on, so editing a row
    /// must not be able to forge recency.
    #[test]
    fn editing_a_project_does_not_count_as_working_in_it() {
        let store = Store::in_memory().unwrap();
        let first = store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        let again = store
            .add_project(NewProject::at("/tmp/alpha").with_notes("renamed"))
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
        let alpha = store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        let beta = store.add_project(NewProject::at("/tmp/beta")).unwrap();
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
        let p = store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        store.set_project_state(&p.id, State::Archived).unwrap();
        assert!(store.projects(false).unwrap().is_empty());
        assert_eq!(store.projects(true).unwrap().len(), 1);
    }

    /// A session working in a subdirectory is working on the project.
    #[test]
    fn a_path_inside_a_checkout_finds_the_checkout() {
        let store = Store::in_memory().unwrap();
        store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        let found = store
            .project_for_path(Path::new("/tmp/alpha/core/src"))
            .unwrap();
        assert_eq!(found.map(|p| p.name), Some("alpha".into()));
    }

    #[test]
    fn a_nested_project_wins_over_the_one_containing_it() {
        let store = Store::in_memory().unwrap();
        store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        store.add_project(NewProject::at("/tmp/alpha/apps/ios")).unwrap();
        let found = store
            .project_for_path(Path::new("/tmp/alpha/apps/ios/src"))
            .unwrap();
        assert_eq!(found.map(|p| p.name), Some("ios".into()));
    }

    /// Naming an archived project explicitly is an instruction, not a guess.
    #[test]
    fn an_archived_project_can_still_be_named_explicitly() {
        let store = Store::in_memory().unwrap();
        let p = store.add_project(NewProject::at("/tmp/alpha")).unwrap();
        store.set_project_state(&p.id, State::Archived).unwrap();
        assert!(store.project_by_name("alpha").unwrap().is_some());
    }

    #[test]
    fn a_project_needs_a_name() {
        let store = Store::in_memory().unwrap();
        assert!(store
            .add_project(NewProject::at("/tmp/alpha").named("   "))
            .is_err());
    }

    // ---- sticky resolution ----------------------------------------------

    #[test]
    fn naming_a_project_switches_the_conversation_to_it() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
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
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
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
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
        store.add_project(NewProject::at("/tmp/jod")).unwrap();
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
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
        assert!(store.settle_project(&chat, "let's fix this").unwrap().is_none());
        assert!(store.current_project(&chat).unwrap().is_none());
    }

    /// Two named projects is exactly when the sticky one is most likely wrong,
    /// so it must not quietly survive as the answer.
    #[test]
    fn an_ambiguous_instruction_is_left_for_the_orchestrator() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
        store.add_project(NewProject::at("/tmp/jod")).unwrap();
        assert!(store
            .settle_project(&chat, "port tetris to jod")
            .unwrap()
            .is_none());
    }

    /// Every switch has to leave a trail, or a wrong guess is uncorrectable.
    #[test]
    fn a_switch_is_recorded_with_the_words_that_caused_it() {
        let (store, chat) = store_with_chat();
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
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
        store.add_project(NewProject::at("/tmp/tetris")).unwrap();
        store.settle_project(&chat, "fix tetris").unwrap();
        store.settle_project(&chat, "tetris again").unwrap();
        assert_eq!(store.project_resolutions(&chat, 10).unwrap().len(), 1);
    }

    #[test]
    fn an_override_marks_the_guess_it_took_back() {
        let (store, chat) = store_with_chat();
        let t = store.add_project(NewProject::at("/tmp/tetris")).unwrap();
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
        let p = store.add_project(NewProject::at("/tmp/tetris")).unwrap();
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
