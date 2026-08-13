//! The directories a conversation may work in.
//!
//! A session can be pointed at several repositories at once, so "where does
//! this agent work" stopped being one path. `conversations.cwd` keeps its old
//! and narrower meaning — the directory the harness process starts in — and
//! this is the set of places it may read, mention and, for exactly one of
//! them, write.
//!
//! ## Read-only is the default, and it is a convention
//!
//! A session opens pointed at your real checkout with [`Root::writable`]
//! false. It writes only in a worktree it claimed for itself (see
//! [`crate::leases`]), and the original stays beside it, readable, so it can
//! still diff against what you are editing.
//!
//! Jod passes a deny rule where a harness supports one, but **nothing here
//! stops a determined agent writing outside its roots**. What the design
//! actually guarantees is narrower and still worth having: work happens on a
//! branch by default, so your checkout is not where a run's half-finished
//! state accumulates. Anything in this module that starts to read like a
//! sandbox is a comment that needs fixing, not a feature that needs adding.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{JodError, Result};
use crate::store::Store;

/// How a root came to be on a conversation, so the UI can explain one nobody
/// remembers adding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Somebody added it — the picker, the CLI, or a launch flag.
    Human,
    /// The conversation's original `cwd`, carried forward when roots were
    /// introduced so no existing conversation lost the directory it had.
    Inherited,
    /// A worktree this session claimed. The only kind that is writable.
    Lease,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Inherited => "inherited",
            Origin::Lease => "lease",
        }
    }

    pub fn parse(s: &str) -> Origin {
        match s {
            "inherited" => Origin::Inherited,
            "lease" => Origin::Lease,
            _ => Origin::Human,
        }
    }
}

/// One directory a conversation may work in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub id: i64,
    pub conversation_id: String,
    pub path: PathBuf,
    /// Whether the agent may change anything here. False for a real checkout;
    /// true only for a claimed worktree.
    pub writable: bool,
    /// The user's order, not the database's. The first root is the one an
    /// unqualified `@mention` resolves against.
    pub position: i64,
    pub origin: Origin,
    pub added_at_ms: i64,
}

impl Root {
    /// Whether `candidate` lies inside this root.
    ///
    /// Used to decide which root a mention belongs to and to label a path in
    /// the picker. Purely lexical over already-normalised paths: this answers
    /// "which root does this belong to", and must never be mistaken for a
    /// permission check — see the module note about sandboxes.
    pub fn contains(&self, candidate: &std::path::Path) -> bool {
        candidate.starts_with(&self.path)
    }

    /// The short label a mention shows when several roots are set, so
    /// `@src/main.rs` is unambiguous across two repositories that both have
    /// one.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }
}

/// A root about to be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRoot {
    pub path: PathBuf,
    pub writable: bool,
    pub origin: Origin,
}

impl NewRoot {
    /// The ordinary case: somewhere to read, added by a person.
    pub fn reading(path: impl Into<PathBuf>) -> NewRoot {
        NewRoot {
            path: path.into(),
            writable: false,
            origin: Origin::Human,
        }
    }

    /// A worktree a session claimed — the only kind that may be written to.
    pub fn lease(path: impl Into<PathBuf>) -> NewRoot {
        NewRoot {
            path: path.into(),
            writable: true,
            origin: Origin::Lease,
        }
    }
}

/// Resolve a path to the one spelling this module stores.
///
/// Two spellings of one directory — `~/repo/jod`, `../jod`, a symlink into it —
/// would otherwise be two roots, and the `UNIQUE(conversation_id, path)` index
/// that makes adding a root idempotent would never fire.
///
/// A path that does not resolve is kept exactly as given rather than refused.
/// The caller is often ahead of the filesystem: E4 records the worktree it is
/// about to create, and a launch flag can name a directory that is not there
/// yet. Refusing here would turn "you typed a path I cannot see" into a failed
/// launch, when the honest answer is an empty candidate list.
pub fn normalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

const ROOT_COLUMNS: &str = "id, conversation_id, path, writable, position, origin, added_at_ms";

fn read_root(r: &rusqlite::Row<'_>) -> rusqlite::Result<Root> {
    Ok(Root {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        path: PathBuf::from(r.get::<_, String>(2)?),
        writable: r.get::<_, i64>(3)? != 0,
        position: r.get(4)?,
        origin: Origin::parse(&r.get::<_, String>(5)?),
        added_at_ms: r.get(6)?,
    })
}

impl Store {
    /// Add a directory to a conversation, or update the one already there.
    ///
    /// Adding a path that is already a root is not an error and does not
    /// duplicate it — it re-flags it in place, keeping its position. That is
    /// the behaviour every caller wants: the picker cannot know whether you
    /// have added this directory before, and E4 flips an existing read-only
    /// root to writable by adding it again as a lease.
    pub fn add_root(&self, conversation_id: &str, new: NewRoot) -> Result<Root> {
        let path = normalise(&new.path);
        let text = path.to_string_lossy().to_string();
        let at = now_ms();
        self.write(|tx| {
            // Appended, never inserted: the order is the user's, and the first
            // root is the one an unqualified mention resolves against, so a
            // second root must not silently take that job.
            let next: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM conversation_roots
                  WHERE conversation_id = ?1",
                params![conversation_id],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO conversation_roots
                   (conversation_id, path, writable, position, origin, added_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(conversation_id, path) DO UPDATE SET
                   writable = excluded.writable,
                   origin   = excluded.origin",
                params![
                    conversation_id,
                    text,
                    new.writable as i64,
                    next,
                    new.origin.as_str(),
                    at
                ],
            )?;
            let root = tx.query_row(
                &format!(
                    "SELECT {ROOT_COLUMNS} FROM conversation_roots
                      WHERE conversation_id = ?1 AND path = ?2"
                ),
                params![conversation_id, text],
                read_root,
            )?;
            Ok(root)
        })
    }

    /// Drop a root. `false` when there was nothing there to drop.
    pub fn remove_root(&self, conversation_id: &str, path: &Path) -> Result<bool> {
        let normalised = normalise(path).to_string_lossy().to_string();
        let as_given = path.to_string_lossy().to_string();
        // Both spellings, because a root outlives its directory. Delete the
        // checkout and `normalise` can no longer resolve the path it resolved
        // when the root was added — matching only the normalised form would
        // leave a row nobody can name, and removing a stale root is exactly
        // what somebody in that position is trying to do.
        let changed = self.write(|tx| {
            let n = tx.execute(
                "DELETE FROM conversation_roots
                  WHERE conversation_id = ?1 AND path IN (?2, ?3)",
                params![conversation_id, normalised, as_given],
            )?;
            Ok(n)
        })?;
        Ok(changed > 0)
    }

    /// Every root of a conversation, in the user's order.
    pub fn roots(&self, conversation_id: &str) -> Result<Vec<Root>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {ROOT_COLUMNS} FROM conversation_roots
              WHERE conversation_id = ?1 ORDER BY position, id"
        ))?;
        let rows = stmt.query_map(params![conversation_id], read_root)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Flip whether a root may be written to.
    ///
    /// This is what E4 calls the moment a session claims a worktree, and the
    /// only sanctioned way a root becomes writable after the fact.
    ///
    /// Refused when the path is not a root of this conversation. Silence would
    /// be worse: a mistyped path would report success, and the caller would
    /// believe it had a place to write when it had not.
    pub fn set_root_writable(
        &self,
        conversation_id: &str,
        path: &Path,
        writable: bool,
    ) -> Result<()> {
        let text = normalise(path).to_string_lossy().to_string();
        let changed = self.write(|tx| {
            let n = tx.execute(
                "UPDATE conversation_roots SET writable = ?3
                  WHERE conversation_id = ?1 AND path = ?2",
                params![conversation_id, text, writable as i64],
            )?;
            Ok(n)
        })?;
        if changed == 0 {
            return Err(JodError::Invalid(format!(
                "`{text}` is not a root of conversation {conversation_id}"
            )));
        }
        Ok(())
    }

    /// Which root a path belongs to, if any.
    ///
    /// The innermost root wins. Roots nest in practice — the checkout and a
    /// worktree of it, or a monorepo and one package inside it — and the
    /// enclosing one is almost never the answer the caller wants: a file in the
    /// worktree is writable, and the same file resolved against the outer root
    /// would be labelled read-only.
    pub fn root_for(&self, conversation_id: &str, path: &Path) -> Result<Option<Root>> {
        let needle = normalise(path);
        let mut best: Option<Root> = None;
        for root in self.roots(conversation_id)? {
            if !root.contains(&needle) {
                continue;
            }
            // Depth in components rather than string length: `/a/bc` is not
            // inside `/a/b`, and comparing lengths would say it was.
            let deeper = best
                .as_ref()
                .is_none_or(|b| root.path.components().count() > b.path.components().count());
            if deeper {
                best = Some(root);
            }
        }
        Ok(best)
    }

    /// Give a conversation that predates roots the directory it already had.
    ///
    /// Every conversation written before migration `0013` has a `cwd` and no
    /// roots, and would open into a picker that says "no roots set" about a
    /// session that has been working in one directory for a week. This carries
    /// that directory forward, marked [`Origin::Inherited`] so the UI can say
    /// where it came from, and read-only like every root Jod adds itself.
    ///
    /// Idempotent, and it never touches a conversation that already has roots:
    /// somebody who removed their last root meant to remove it.
    pub fn ensure_inherited_root(&self, conversation_id: &str) -> Result<()> {
        let at = now_ms();
        self.write(|tx| {
            let has_roots: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM conversation_roots WHERE conversation_id = ?1 LIMIT 1",
                    params![conversation_id],
                    |r| r.get(0),
                )
                .optional()?;
            if has_roots.is_some() {
                return Ok(());
            }
            let cwd: Option<String> = tx
                .query_row(
                    "SELECT cwd FROM conversations WHERE id = ?1",
                    params![conversation_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(cwd) = cwd else {
                return Err(JodError::Invalid(format!(
                    "no conversation `{conversation_id}` to inherit a root from"
                )));
            };
            // A conversation with no `cwd` has nothing to inherit. Inserting an
            // empty root would be worse than none: it reads as "/" to anything
            // that joins a relative path onto it.
            if cwd.trim().is_empty() {
                return Ok(());
            }
            let path = normalise(Path::new(&cwd)).to_string_lossy().to_string();
            tx.execute(
                "INSERT OR IGNORE INTO conversation_roots
                   (conversation_id, path, writable, position, origin, added_at_ms)
                 VALUES (?1, ?2, 0, 0, ?3, ?4)",
                params![conversation_id, path, Origin::Inherited.as_str(), at],
            )?;
            Ok(())
        })
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessKind;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn conversation(s: &Store, cwd: &str) -> String {
        s.new_conversation(HarnessKind::ClaudeCode, cwd, None)
            .unwrap()
            .id
    }

    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jod-roots-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        normalise(&dir)
    }

    fn paths(roots: &[Root]) -> Vec<String> {
        roots
            .iter()
            .map(|r| r.path.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn roots_come_back_in_the_order_they_were_added() {
        let s = store();
        let c = conversation(&s, "/tmp");
        for p in ["/tmp/a", "/tmp/b", "/tmp/c"] {
            s.add_root(&c, NewRoot::reading(p)).unwrap();
        }
        assert_eq!(paths(&s.roots(&c).unwrap()), ["/tmp/a", "/tmp/b", "/tmp/c"]);
        assert_eq!(
            s.roots(&c)
                .unwrap()
                .iter()
                .map(|r| r.position)
                .collect::<Vec<_>>(),
            [0, 1, 2],
            "position is the user's order, and the first root is what an \
             unqualified mention resolves against"
        );
    }

    /// The picker cannot know whether this directory is already a root, so
    /// adding one twice has to be ordinary rather than an error — and it must
    /// not push a second row past the `UNIQUE` index either.
    #[test]
    fn a_root_added_twice_does_not_duplicate_the_row() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        s.add_root(&c, NewRoot::reading("/tmp/other")).unwrap();
        let again = s.add_root(&c, NewRoot::lease("/tmp/repo")).unwrap();

        let roots = s.roots(&c).unwrap();
        assert_eq!(roots.len(), 2);
        assert!(again.writable, "re-adding as a lease re-flags the root");
        assert_eq!(again.origin, Origin::Lease);
        assert_eq!(
            again.position, 0,
            "re-adding must not move a root to the end of the user's order"
        );
    }

    #[test]
    fn a_root_remembers_how_it_arrived() {
        let s = store();
        let c = conversation(&s, "/tmp");
        let lease = s.add_root(&c, NewRoot::lease("/tmp/wt")).unwrap();
        let reread = &s.roots(&c).unwrap()[0];
        assert_eq!(lease.origin, Origin::Lease);
        assert_eq!(reread.origin, Origin::Lease);
        assert!(reread.writable);
    }

    /// Two spellings of one directory would be two roots, and every "is this
    /// already a root" check downstream would answer wrongly.
    #[cfg(unix)]
    #[test]
    fn a_root_added_through_a_symlink_is_the_same_root_as_the_real_directory() {
        let dir = scratch("symlink");
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading(&real)).unwrap();
        s.add_root(&c, NewRoot::reading(&link)).unwrap();

        assert_eq!(s.roots(&c).unwrap().len(), 1);
        assert_eq!(s.roots(&c).unwrap()[0].path, real);
    }

    #[test]
    fn removing_a_root_says_whether_there_was_one_to_remove() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        assert!(s.remove_root(&c, Path::new("/tmp/repo")).unwrap());
        assert!(!s.remove_root(&c, Path::new("/tmp/repo")).unwrap());
        assert!(s.roots(&c).unwrap().is_empty());
    }

    /// A root outlives its directory, and the person removing it is usually
    /// removing it *because* the directory is gone.
    #[test]
    fn a_root_whose_directory_does_not_exist_can_still_be_added_and_removed() {
        let s = store();
        let c = conversation(&s, "/tmp");
        let ghost = std::env::temp_dir().join(format!("jod-roots-ghost-{}", std::process::id()));
        let added = s.add_root(&c, NewRoot::reading(&ghost)).unwrap();
        assert_eq!(added.path, ghost, "an unresolvable path is kept as given");
        assert!(s.remove_root(&c, &ghost).unwrap());
    }

    #[test]
    fn a_claimed_worktree_flips_its_root_to_writable() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        assert!(!s.roots(&c).unwrap()[0].writable, "read-only by default");

        s.set_root_writable(&c, Path::new("/tmp/repo"), true)
            .unwrap();
        assert!(s.roots(&c).unwrap()[0].writable);
    }

    #[test]
    fn making_a_path_that_is_not_a_root_writable_is_refused_rather_than_ignored() {
        let s = store();
        let c = conversation(&s, "/tmp");
        let err = s
            .set_root_writable(&c, Path::new("/tmp/nowhere"), true)
            .unwrap_err();
        assert!(
            matches!(err, JodError::Invalid(_)),
            "a caller that believes it has a place to write and has not is the \
             bug this refusal prevents; got {err:?}"
        );
    }

    /// A worktree lives inside — or beside — the checkout it came from, and the
    /// enclosing root would label a file in the worktree read-only.
    #[test]
    fn the_innermost_root_wins_when_roots_nest() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        s.add_root(&c, NewRoot::lease("/tmp/repo/.worktrees/feature"))
            .unwrap();

        let found = s
            .root_for(&c, Path::new("/tmp/repo/.worktrees/feature/src/main.rs"))
            .unwrap()
            .expect("a root contains this path");
        assert_eq!(found.path, PathBuf::from("/tmp/repo/.worktrees/feature"));
        assert!(found.writable);

        let outer = s
            .root_for(&c, Path::new("/tmp/repo/src/main.rs"))
            .unwrap()
            .unwrap();
        assert_eq!(outer.path, PathBuf::from("/tmp/repo"));
    }

    /// `/tmp/repo-old` starts with the same *characters* as `/tmp/repo` and is
    /// not inside it. A string prefix test would say it was.
    #[test]
    fn a_sibling_with_a_longer_name_is_not_inside_a_root() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        assert!(s
            .root_for(&c, Path::new("/tmp/repo-old/src/main.rs"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_path_outside_every_root_belongs_to_none_of_them() {
        let s = store();
        let c = conversation(&s, "/tmp");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        assert!(s.root_for(&c, Path::new("/etc/hosts")).unwrap().is_none());
    }

    #[test]
    fn a_conversation_that_predates_roots_keeps_the_directory_it_had() {
        let s = store();
        let c = conversation(&s, "/tmp/work");
        s.ensure_inherited_root(&c).unwrap();

        let roots = s.roots(&c).unwrap();
        assert_eq!(paths(&roots), ["/tmp/work"]);
        assert_eq!(roots[0].origin, Origin::Inherited);
        assert!(!roots[0].writable, "an inherited checkout is read-only");
    }

    #[test]
    fn inheriting_a_root_twice_leaves_one_root() {
        let s = store();
        let c = conversation(&s, "/tmp/work");
        s.ensure_inherited_root(&c).unwrap();
        s.ensure_inherited_root(&c).unwrap();
        assert_eq!(s.roots(&c).unwrap().len(), 1);
    }

    /// Somebody who removed their last root meant to remove it, and a
    /// conversation opened later must not quietly get its `cwd` back.
    #[test]
    fn inheriting_never_touches_a_conversation_that_already_has_roots() {
        let s = store();
        let c = conversation(&s, "/tmp/work");
        s.add_root(&c, NewRoot::reading("/tmp/elsewhere")).unwrap();
        s.ensure_inherited_root(&c).unwrap();
        assert_eq!(paths(&s.roots(&c).unwrap()), ["/tmp/elsewhere"]);
    }

    #[test]
    fn a_conversation_with_no_cwd_inherits_nothing_rather_than_an_empty_root() {
        let s = store();
        let c = conversation(&s, "");
        s.ensure_inherited_root(&c).unwrap();
        assert!(
            s.roots(&c).unwrap().is_empty(),
            "an empty root reads as `/` to anything that joins onto it"
        );
    }

    #[test]
    fn inheriting_a_root_for_a_conversation_that_does_not_exist_is_refused() {
        let s = store();
        let err = s.ensure_inherited_root("no-such-conversation").unwrap_err();
        assert!(matches!(err, JodError::Invalid(_)), "got {err:?}");
    }

    /// The foreign key carries `ON DELETE CASCADE` and `PRAGMA foreign_keys` is
    /// on; both are needed, and a root outliving its conversation would be an
    /// orphan nothing can list or remove.
    #[test]
    fn deleting_a_conversation_takes_its_roots_with_it() {
        let s = store();
        let c = conversation(&s, "/tmp/work");
        s.add_root(&c, NewRoot::reading("/tmp/repo")).unwrap();
        s.write(|tx| {
            tx.execute("DELETE FROM conversations WHERE id = ?1", params![c])?;
            Ok(())
        })
        .unwrap();
        assert!(s.roots(&c).unwrap().is_empty());
    }

    #[test]
    fn a_roots_label_is_its_directory_name_so_two_repos_are_distinguishable() {
        let a = Root {
            id: 1,
            conversation_id: "c".into(),
            path: PathBuf::from("/home/x/code/jod"),
            writable: false,
            position: 0,
            origin: Origin::Human,
            added_at_ms: 0,
        };
        assert_eq!(a.label(), "jod");
    }
}
