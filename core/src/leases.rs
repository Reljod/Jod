//! Worktrees a session claimed to write in.
//!
//! A session starts pointed at your real checkout, read-only. The moment it
//! needs to change something it claims a lease — a fresh branch and worktree —
//! and that becomes its only *writable* root, with the original still beside it
//! so it can diff against what you are editing.
//!
//! ## Claiming is the agent's step, not Jod's inference
//!
//! "Detect the first write" has no harness-agnostic implementation: every
//! harness spells its pre-write hook differently and two of the three barely
//! have one. So the agent is told plainly that its root is read-only and that
//! it must claim before changing anything, and a watcher on the read-only root
//! is the backstop — a write that lands there anyway raises a card rather than
//! being silently kept. The watcher reports; it does not revert.
//!
//! ## A lease outlives the work that cut it
//!
//! Deleting a work does **not** remove its worktrees or their branches. Jod's
//! records are cheap to recreate and a branch with uncommitted work on it is
//! not — and the moment of deleting a session's history is exactly the moment
//! nobody is left to remember what was on it. So the row survives with a null
//! work, keeping [`Lease::work_title`] so an orphan can still say what it was
//! for; an orphaned lease that cannot explain itself is one nobody dares
//! delete.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cards::{CardKind, Importance, NewCard};
use crate::error::{JodError, Result};
use crate::roots::{self, NewRoot};
use crate::store::Store;

/// Whether a lease is still held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// In use. Offered to a sibling session on the same repository before a
    /// second branch is cut.
    Held,
    /// Given up, but the worktree is still on disk — because it was dirty, or
    /// unmerged, or both.
    Released,
    /// Given up and cleaned off disk. Only reachable when the tree was clean
    /// and merged.
    Removed,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Held => "held",
            State::Released => "released",
            State::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "released" => State::Released,
            "removed" => State::Removed,
            _ => State::Held,
        }
    }
}

/// One claimed worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: i64,
    /// Null once the work has been deleted. The lease deliberately survives it.
    pub work_id: Option<String>,
    /// Remembered so an orphaned lease can still say what it was for.
    pub work_title: String,
    pub conversation_id: Option<String>,
    /// The real checkout this was cut from.
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_ref: String,
    pub state: State,
    pub created_at_ms: i64,
    pub released_at_ms: Option<i64>,
}

/// What a lease looks like on disk right now.
///
/// Read from git at the moment somebody asks, never cached: a lease that was
/// clean an hour ago tells you nothing about whether deleting it now would
/// lose work. This is what the delete refusal prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub worktree_path: PathBuf,
    pub branch: String,
    /// Uncommitted changes present.
    pub dirty: bool,
    /// Every commit on this branch is reachable from its base.
    pub merged: bool,
    /// The worktree directory is gone from disk — somebody removed it by hand.
    pub missing: bool,
}

impl Condition {
    /// Whether removing this would destroy something that cannot be recovered.
    ///
    /// The single question the removal path is allowed to ask. Deliberately
    /// conservative: unmerged *or* dirty is enough to refuse, because the cost
    /// of keeping a stale worktree is a directory and the cost of the mistake
    /// is somebody's afternoon.
    pub fn safe_to_remove(&self) -> bool {
        self.missing || (!self.dirty && self.merged)
    }

    /// Why removal was refused, in the words the refusal prints.
    pub fn why_kept(&self) -> String {
        match (self.dirty, self.merged) {
            (true, false) => "it has uncommitted changes and its branch is not merged".into(),
            (true, true) => "it has uncommitted changes".into(),
            (false, false) => "its branch is not merged into its base".into(),
            (false, true) => "it is clean and merged".into(),
        }
    }
}

/// What happened when a session claimed somewhere to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A fresh branch and worktree.
    Cut(Lease),
    /// A sibling session in this work already held one for this repository, so
    /// it was offered rather than a second branch cut for the same job.
    Reused(Lease),
    /// The root is not a git repository. A card names it; nothing crashed, and
    /// the session is still running.
    NotGit {
        card_id: i64,
        path: PathBuf,
        detail: String,
    },
}

impl Claim {
    /// The lease, when there is one.
    pub fn lease(&self) -> Option<&Lease> {
        match self {
            Claim::Cut(l) | Claim::Reused(l) => Some(l),
            Claim::NotGit { .. } => None,
        }
    }
}

/// What happened when a lease was given up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// Clean, merged, and gone from disk.
    Removed { lease: Lease },
    /// Still on disk, and here is why. Keeping a stale worktree costs a
    /// directory; the mistake costs somebody's afternoon.
    Kept {
        lease: Lease,
        condition: Condition,
        reason: String,
    },
}

impl Release {
    pub fn removed(&self) -> bool {
        matches!(self, Release::Removed { .. })
    }
}

/// Where claimed worktrees live.
///
/// Under `$JOD_HOME` rather than inside the repository, for two reasons that
/// both bite: a worktree under the checkout appears in every `rg`, every `@`
/// mention and every build the person is running, and a worktree Jod created
/// inside a repository is a directory the repository's own tooling has to be
/// taught to ignore.
pub fn worktrees_dir() -> PathBuf {
    crate::paths::jod_home().join("worktrees")
}

// ---- git -------------------------------------------------------------------

/// What one `git` invocation produced.
struct GitRun {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Run git, or say plainly that it is not installed.
///
/// Shelling out rather than linking a git library: `git worktree` is a
/// composite of half a dozen operations on refs, the index and the
/// administrative files under `.git/worktrees`, and a reimplementation that is
/// subtly wrong about any of them is a reimplementation that loses somebody's
/// branch. The binary is also the same one the person is using in the same
/// checkout, which is worth more here than a dependency.
fn git(dir: &Path, args: &[&str]) -> Result<GitRun> {
    // Checked before spawning, because a `current_dir` that does not exist
    // fails with the same `NotFound` as a missing binary — and "git is not
    // installed" sent to somebody whose worktree was deleted by hand is an
    // hour spent in the wrong place. A directory that has gone is an ordinary
    // answer here: it is what `missing` means.
    if !dir.is_dir() {
        return Ok(GitRun {
            ok: false,
            stdout: String::new(),
            stderr: format!("`{}` is not a directory", dir.display()),
        });
    }
    let out = Command::new("git").current_dir(dir).args(args).output();
    let out = match out {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Named rather than passed through as a bare `NotFound`, which
            // reads as "the directory is missing" and sends whoever gets it
            // looking in the wrong place.
            return Err(JodError::Invalid(
                "`git` is not installed, and a lease is a git worktree — install git or run \
                 this session against a directory it does not need to write in"
                    .into(),
            ));
        }
        Err(e) => return Err(e.into()),
    };
    Ok(GitRun {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// The repository a path is inside, or `None` when it is inside none.
///
/// Resolved to the top level rather than taken as given, so two sessions
/// pointed at two subdirectories of one checkout are offered the *same* lease
/// instead of cutting a branch each.
fn toplevel(path: &Path) -> Result<Option<PathBuf>> {
    if !path.is_dir() {
        return Ok(None);
    }
    let run = git(path, &["rev-parse", "--show-toplevel"])?;
    if !run.ok || run.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(roots::normalise(Path::new(&run.stdout))))
}

/// What a new branch should be cut from: the branch that is checked out, or
/// the commit itself when the checkout is detached.
fn base_ref(repo: &Path) -> Result<String> {
    let named = git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if named.ok && !named.stdout.is_empty() {
        return Ok(named.stdout);
    }
    let sha = git(repo, &["rev-parse", "HEAD"])?;
    if sha.ok && !sha.stdout.is_empty() {
        return Ok(sha.stdout);
    }
    Err(JodError::Invalid(format!(
        "`{}` has no commits, so there is nothing to cut a branch from",
        repo.display()
    )))
}

/// A branch- and directory-safe version of a title.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.chars().count() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "work".to_string()
    } else {
        trimmed
    }
}

// ---- the store -------------------------------------------------------------

const LEASE_COLUMNS: &str = "id, work_id, work_title, conversation_id, repo_path,
     worktree_path, branch, base_ref, state, created_at_ms, released_at_ms";

fn read_lease(r: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: r.get(0)?,
        work_id: r.get(1)?,
        work_title: r.get(2)?,
        conversation_id: r.get(3)?,
        repo_path: PathBuf::from(r.get::<_, String>(4)?),
        worktree_path: PathBuf::from(r.get::<_, String>(5)?),
        branch: r.get(6)?,
        base_ref: r.get(7)?,
        state: State::parse(&r.get::<_, String>(8)?),
        created_at_ms: r.get(9)?,
        released_at_ms: r.get(10)?,
    })
}

impl Store {
    /// Claim somewhere to write.
    ///
    /// The explicit step D5 turns on. Jod does not infer it: "detect the first
    /// write" has no harness-agnostic implementation, so the session is told
    /// its root is read-only and claims before changing anything.
    ///
    /// Afterwards the session has two roots — the worktree, writable, and the
    /// real checkout beside it, readable and no longer writable — because a
    /// session that cannot read what you are editing cannot diff against it.
    pub fn claim_lease(
        &self,
        work_id: &str,
        conversation_id: &str,
        repo_path: &Path,
    ) -> Result<Claim> {
        let asked = roots::normalise(repo_path);
        let Some(repo) = toplevel(&asked)? else {
            // A card, not an error and certainly not a panic: the session is
            // still running and still useful, and somebody has to decide
            // whether this root was the wrong one or wants `git init`.
            let detail = format!(
                "`{}` is not inside a git repository, so there is no branch to cut and \
                 nowhere for this session to write",
                asked.display()
            );
            let card = self.raise_card(NewCard {
                conversation_id: conversation_id.to_string(),
                work_id: Some(work_id.to_string()),
                kind: Some(CardKind::Question),
                importance: Some(Importance::High),
                blocking: true,
                title: format!("cannot claim a worktree in `{}`", asked.display()),
                body: detail.clone(),
                dedupe_key: Some(format!("lease-not-git:{}", asked.display())),
                ..NewCard::default()
            })?;
            return Ok(Claim::NotGit {
                card_id: card.id,
                path: asked,
                detail,
            });
        };

        // Reuse before cutting. The partial unique index enforces one live
        // lease per work and repository; this is the same rule read out loud,
        // so a sibling is *offered* the existing worktree rather than finding
        // out through a constraint error.
        if let Some(existing) = self.held_lease(work_id, &repo)? {
            self.bind_lease_roots(conversation_id, &existing)?;
            return Ok(Claim::Reused(existing));
        }

        let work_title = self
            .work(work_id)?
            .map(|w| w.title)
            .unwrap_or_else(|| work_id.to_string());
        let base = base_ref(&repo)?;
        let stem = slug(&work_title);
        // A short random suffix, because two works can share a title and a
        // branch that already exists is a claim that fails for a reason nobody
        // can act on.
        let unique = &uuid::Uuid::new_v4().to_string()[..8];
        let branch = format!("jod/{stem}-{unique}");
        let repo_name = repo
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        let worktree = worktrees_dir()
            .join(format!("{stem}-{unique}"))
            .join(&repo_name);
        let at = now_ms();

        // The row is written *before* the worktree exists, so that two
        // sessions racing settle it here — where the index arbitrates — rather
        // than both running `git worktree add` and one of them failing with a
        // directory already on disk. If git then refuses, the reservation is
        // taken back below.
        let reserved: std::result::Result<i64, JodError> = self.write(|tx| {
            tx.execute(
                "INSERT INTO leases
                   (work_id, work_title, conversation_id, repo_path, worktree_path,
                    branch, base_ref, state, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'held', ?8)",
                params![
                    work_id,
                    work_title,
                    conversation_id,
                    repo.to_string_lossy(),
                    worktree.to_string_lossy(),
                    branch,
                    base,
                    at,
                ],
            )?;
            Ok(tx.last_insert_rowid())
        });
        let lease_id = match reserved {
            Ok(id) => id,
            Err(JodError::Db(rusqlite::Error::SqliteFailure(e, _)))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // Somebody else won the race between the reuse check and here.
                // Their lease is the answer, which is exactly what a second
                // session was going to be offered anyway.
                let Some(existing) = self.held_lease(work_id, &repo)? else {
                    return Err(JodError::Invalid(format!(
                        "another session holds a lease on `{}` that cannot be read back",
                        repo.display()
                    )));
                };
                self.bind_lease_roots(conversation_id, &existing)?;
                return Ok(Claim::Reused(existing));
            }
            Err(e) => return Err(e),
        };

        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let added = git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &worktree.to_string_lossy(),
                &base,
            ],
        );
        let failed = match added {
            Ok(run) if run.ok => None,
            Ok(run) => Some(if run.stderr.is_empty() {
                run.stdout
            } else {
                run.stderr
            }),
            Err(e) => Some(e.to_string()),
        };
        if let Some(why) = failed {
            // The reservation goes back, or the work holds a lease to a
            // worktree that was never created and no session can ever claim
            // this repository again.
            self.write(|tx| {
                tx.execute("DELETE FROM leases WHERE id = ?1", params![lease_id])?;
                Ok(())
            })?;
            return Err(JodError::Invalid(format!(
                "could not cut a worktree for `{}`: {why}",
                repo.display()
            )));
        }

        // Settle the stored path now that there is a directory to canonicalise.
        //
        // The row is written before `git worktree add` runs, so `normalise` at
        // insert time had nothing to resolve and returned what it was given.
        // Every other path on a lease is already canonical — `repo_path` comes
        // from `toplevel`, and `add_root` normalises whatever it is handed — so
        // leaving this one raw made the lease and its own root two spellings of
        // one directory.
        //
        // That is not cosmetic. `claim_worktree` reports `worktree_path` and
        // `list_roots` reports the root, and on macOS `$TMPDIR` is a symlink:
        // the tool answered `/var/…` while the roots said `/private/var/…` for
        // the same worktree. An agent handed two spellings has no way to tell
        // they are the same place, and the comparison sites in this module were
        // each wrapping `roots::normalise` around the field to paper over it.
        let settled = roots::normalise(&worktree);
        if settled != worktree {
            self.write(|tx| {
                tx.execute(
                    "UPDATE leases SET worktree_path = ?2 WHERE id = ?1",
                    params![lease_id, settled.to_string_lossy()],
                )?;
                Ok(())
            })?;
        }

        let lease = self
            .lease(lease_id)?
            .expect("the lease was just written in this process");
        self.bind_lease_roots(conversation_id, &lease)?;
        Ok(Claim::Cut(lease))
    }

    /// Point a session at its lease: the worktree writable, the checkout it
    /// came from still there and no longer writable.
    ///
    /// The original stays deliberately. Removing it would take away the
    /// session's ability to read and diff against what you are editing, which
    /// is half the reason the design puts a session in your real checkout in
    /// the first place.
    fn bind_lease_roots(&self, conversation_id: &str, lease: &Lease) -> Result<()> {
        self.add_root(conversation_id, NewRoot::lease(&lease.worktree_path))?;
        let known = self
            .roots(conversation_id)?
            .into_iter()
            .any(|r| r.path == lease.repo_path);
        if known {
            // Re-flagged rather than re-added, so a root that arrived as the
            // conversation's own `cwd` keeps saying so.
            self.set_root_writable(conversation_id, &lease.repo_path, false)?;
        } else {
            self.add_root(conversation_id, NewRoot::reading(&lease.repo_path))?;
        }
        Ok(())
    }

    pub fn lease(&self, id: i64) -> Result<Option<Lease>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("SELECT {LEASE_COLUMNS} FROM leases WHERE id = ?1"),
                params![id],
                read_lease,
            )
            .optional()?)
    }

    /// The live lease for a work and a repository, if there is one.
    pub fn held_lease(&self, work_id: &str, repo_path: &Path) -> Result<Option<Lease>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {LEASE_COLUMNS} FROM leases
                      WHERE work_id = ?1 AND repo_path = ?2 AND state = 'held'"
                ),
                params![work_id, roots::normalise(repo_path).to_string_lossy()],
                read_lease,
            )
            .optional()?)
    }

    /// Every lease a work cut, in the order it cut them.
    pub fn work_leases(&self, work_id: &str) -> Result<Vec<Lease>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM leases WHERE work_id = ?1 ORDER BY created_at_ms, id"
        ))?;
        let rows = stmt.query_map(params![work_id], read_lease)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Leases whose work has been deleted and whose worktree is still on disk.
    ///
    /// The reason `work_title` is a column: this list is read by somebody
    /// deciding what to clean up, and a path with no explanation is one nobody
    /// dares remove.
    pub fn orphaned_leases(&self) -> Result<Vec<Lease>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM leases
              WHERE work_id IS NULL AND state != 'removed' ORDER BY created_at_ms, id"
        ))?;
        let rows = stmt.query_map([], read_lease)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// What this lease looks like on disk right now.
    ///
    /// Never cached, and deliberately not a column. A lease that was clean an
    /// hour ago tells you nothing about whether removing it now would lose
    /// work, and a stored answer is one that is wrong exactly when it matters.
    pub fn lease_condition(&self, lease: &Lease) -> Result<Condition> {
        let missing = !lease.worktree_path.is_dir();
        let dirty = if missing {
            false
        } else {
            let status = git(&lease.worktree_path, &["status", "--porcelain"])?;
            // Untracked files count. A file an agent wrote and never added is
            // exactly the work this refusal exists to protect.
            status.ok && !status.stdout.is_empty()
        };
        // Merged means "every commit on this branch is reachable from its
        // base". Anything that cannot be answered — a base that has gone, a
        // repository that has moved — reads as unmerged, because the
        // conservative answer keeps a directory and the other one loses a
        // branch.
        let merged = match git(
            &lease.repo_path,
            &[
                "rev-list",
                "--count",
                &format!("{}..{}", lease.base_ref, lease.branch),
            ],
        ) {
            Ok(run) if run.ok => run.stdout.trim() == "0",
            _ => false,
        };
        Ok(Condition {
            worktree_path: lease.worktree_path.clone(),
            branch: lease.branch.clone(),
            dirty,
            merged,
            missing,
        })
    }

    /// Give up a lease, removing the worktree only if losing it costs nothing.
    ///
    /// A dirty or unmerged tree is kept and the reason is returned. The lease
    /// still stops being held either way, so the work may cut a fresh one —
    /// what is refused is destroying something, never making progress.
    pub fn release_lease(&self, lease_id: i64) -> Result<Release> {
        let Some(lease) = self.lease(lease_id)? else {
            return Err(JodError::Invalid(format!("no lease #{lease_id}")));
        };
        if lease.state == State::Removed {
            return Err(JodError::Invalid(format!(
                "lease #{lease_id} was already removed from disk"
            )));
        }
        let condition = self.lease_condition(&lease)?;
        if !condition.safe_to_remove() {
            let lease = self.mark_lease(lease_id, State::Released)?;
            let reason = format!(
                "kept `{}`: {}",
                lease.worktree_path.display(),
                condition.why_kept()
            );
            return Ok(Release::Kept {
                lease,
                condition,
                reason,
            });
        }
        self.remove_worktree_of(&lease, &condition)?;
        let lease = self.mark_lease(lease_id, State::Removed)?;
        Ok(Release::Removed { lease })
    }

    /// Remove a lease's worktree as a deliberate, separate act.
    ///
    /// What `jod work leases` calls to clean up afterwards, including for a
    /// lease whose work is gone. It refuses a dirty or unmerged tree exactly as
    /// [`Store::release_lease`] does: deleting a work never gained the power to
    /// remove one of these, and neither did tidying up after one.
    pub fn remove_worktree(&self, lease_id: i64) -> Result<Release> {
        let Some(lease) = self.lease(lease_id)? else {
            return Err(JodError::Invalid(format!("no lease #{lease_id}")));
        };
        let condition = self.lease_condition(&lease)?;
        if !condition.safe_to_remove() {
            let reason = format!(
                "kept `{}`: {}",
                lease.worktree_path.display(),
                condition.why_kept()
            );
            return Ok(Release::Kept {
                lease,
                condition,
                reason,
            });
        }
        self.remove_worktree_of(&lease, &condition)?;
        let lease = self.mark_lease(lease_id, State::Removed)?;
        Ok(Release::Removed { lease })
    }

    /// The disk half of removal, with the session's writable root taken back.
    ///
    /// A writable root pointing at a directory that is gone is worse than no
    /// root: the agent is told it may write somewhere it cannot.
    fn remove_worktree_of(&self, lease: &Lease, condition: &Condition) -> Result<()> {
        if !condition.missing {
            let run = git(
                &lease.repo_path,
                &["worktree", "remove", &lease.worktree_path.to_string_lossy()],
            )?;
            if !run.ok {
                return Err(JodError::Invalid(format!(
                    "could not remove `{}`: {}",
                    lease.worktree_path.display(),
                    if run.stderr.is_empty() {
                        run.stdout
                    } else {
                        run.stderr
                    }
                )));
            }
        } else {
            // Somebody removed the directory by hand; git still has an
            // administrative record of it, and leaving that behind makes the
            // next `worktree add` on the same path fail.
            let _ = git(&lease.repo_path, &["worktree", "prune"])?;
        }
        if let Some(conversation_id) = &lease.conversation_id {
            self.remove_root(conversation_id, &lease.worktree_path)?;
        }
        Ok(())
    }

    fn mark_lease(&self, lease_id: i64, state: State) -> Result<Lease> {
        let at = now_ms();
        self.write(|tx| {
            tx.execute(
                "UPDATE leases SET state = ?2, released_at_ms = ?3 WHERE id = ?1",
                params![lease_id, state.as_str(), at],
            )?;
            Ok(())
        })?;
        self.lease(lease_id)?
            .ok_or_else(|| JodError::Invalid(format!("no lease #{lease_id}")))
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Build a real git repository with one commit in it.
///
/// The spec's one sanctioned fake for this epic, and it is a fake only in the
/// sense that nobody wrote the code in it: every operation these tests exercise
/// runs against the real `git`, because a lease *is* a git worktree and a
/// stubbed one would test the stub.
///
/// Returns `None` when git is not installed, having said so loudly. A test that
/// quietly passed on a machine with no git would be a test that stopped
/// checking the thing it exists for.
#[cfg(test)]
pub(crate) fn fixture_repo(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).expect("a scratch directory");
    std::fs::write(dir.join("README.md"), "fixture\n").expect("a file to commit");
    let commit: Vec<&str> = vec![
        "-c",
        "user.name=Jod Test",
        "-c",
        "user.email=test@example.invalid",
        // Signing is configured on plenty of real machines and there is no
        // key in CI, so the fixture would fail for a reason that has nothing
        // to do with what is being tested.
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "init",
    ];
    for args in [vec!["init", "--quiet"], vec!["add", "README.md"], commit] {
        let run = Command::new("git")
            .current_dir(dir)
            .args(&args)
            // Hermetic: a global `core.hooksPath` or commit template on the
            // machine running the tests must not reach the fixture.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output();
        match run {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "SKIPPING a lease test: `git` is not installed on this machine, and a \
                     lease is a git worktree. Install git and run the suite again — this \
                     test checked nothing."
                );
                return None;
            }
            Err(e) => panic!("could not run `git {}`: {e}", args.join(" ")),
            Ok(out) if !out.status.success() => panic!(
                "`git {}` failed in the fixture: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ),
            Ok(_) => {}
        }
    }
    Some(roots::normalise(dir))
}

/// A scratch directory of this test's own, and `$JOD_HOME` pointed inside it.
///
/// Worktrees are cut under `$JOD_HOME`, so a test that did not move it would
/// write into the real one. The returned guard is the process-wide environment
/// lock: Rust runs tests as threads of one process, so two tests setting
/// `JOD_HOME` at once would each get the other's.
#[cfg(test)]
pub(crate) fn scratch(name: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
    let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("jod-lease-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::env::set_var("JOD_HOME", dir.join("jod-home"));
    (guard, roots::normalise(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessKind;
    use crate::works::Origin;

    /// A store, a work, and a session in it pointed at `repo`, read-only.
    fn session_on(store: &Store, repo: &Path) -> (String, String) {
        let work = store.create_work("tidy the parser").unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &repo.to_string_lossy(), None)
            .unwrap();
        store
            .add_root(&conversation.id, NewRoot::reading(repo))
            .unwrap();
        store
            .attach_conversation(&conversation.id, &work.id, None, Origin::Orchestrator)
            .unwrap();
        (work.id, conversation.id)
    }

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn branches(repo: &Path) -> String {
        git(repo, &["branch", "--list", "--format=%(refname:short)"])
            .unwrap()
            .stdout
    }

    #[test]
    fn claiming_cuts_a_branch_and_leaves_the_checkout_readable_but_not_writable() {
        let (_env, dir) = scratch("claim");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);

        let claim = s.claim_lease(&work, &conversation, &repo).unwrap();
        let Claim::Cut(lease) = claim else {
            panic!("the first claim on a repository cuts a branch, got {claim:?}");
        };
        assert!(lease.worktree_path.is_dir(), "the worktree is on disk");
        assert!(
            branches(&repo).lines().any(|b| b == lease.branch),
            "the branch exists in the real repository: {}",
            branches(&repo)
        );

        let roots = s.roots(&conversation).unwrap();
        let checkout = roots
            .iter()
            .find(|r| r.path == repo)
            .expect("the real checkout is still a root, so the session can diff against it");
        assert!(
            !checkout.writable,
            "claiming makes the checkout read-only; writing happens on the branch"
        );
        let worktree = roots
            .iter()
            .find(|r| r.path == roots::normalise(&lease.worktree_path))
            .expect("the worktree became a root");
        assert!(worktree.writable, "the worktree is the only writable root");
    }

    /// The partial unique index says one live lease per work and repository.
    /// This is that rule read out loud, so a sibling is offered the worktree
    /// rather than finding out through a constraint error.
    #[test]
    fn a_sibling_session_is_offered_the_lease_rather_than_a_second_branch() {
        let (_env, dir) = scratch("reuse");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, first) = session_on(&s, &repo);
        let sibling = s
            .new_conversation(HarnessKind::ClaudeCode, &repo.to_string_lossy(), None)
            .unwrap();
        s.attach_conversation(&sibling.id, &work, Some(&first), Origin::Agent)
            .unwrap();

        let cut = s.claim_lease(&work, &first, &repo).unwrap();
        let reused = s.claim_lease(&work, &sibling.id, &repo).unwrap();
        assert!(
            matches!(reused, Claim::Reused(_)),
            "a sibling on the same repository reuses, got {reused:?}"
        );
        assert_eq!(
            cut.lease().unwrap().id,
            reused.lease().unwrap().id,
            "and it is the same lease"
        );
        assert_eq!(s.work_leases(&work).unwrap().len(), 1);
        assert!(
            s.roots(&sibling.id)
                .unwrap()
                .iter()
                .any(|r| r.writable && r.path == roots::normalise(&cut.lease().unwrap().worktree_path)),
            "the sibling can write in it too"
        );
    }

    /// A subdirectory of a checkout is the same checkout. Two sessions pointed
    /// at two packages of one repository must not cut a branch each.
    #[test]
    fn a_claim_inside_a_subdirectory_belongs_to_the_repository_that_contains_it() {
        let (_env, dir) = scratch("subdir");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let inner = repo.join("crates/parser");
        std::fs::create_dir_all(&inner).unwrap();
        let s = store();
        let (work, conversation) = session_on(&s, &repo);

        let first = s.claim_lease(&work, &conversation, &repo).unwrap();
        let second = s.claim_lease(&work, &conversation, &inner).unwrap();
        assert!(matches!(second, Claim::Reused(_)), "got {second:?}");
        assert_eq!(first.lease().unwrap().id, second.lease().unwrap().id);
    }

    #[test]
    fn a_root_that_is_not_a_git_repository_raises_a_card_rather_than_crashing() {
        let (_env, dir) = scratch("not-git");
        let plain = dir.join("just-a-folder");
        std::fs::create_dir_all(&plain).unwrap();
        let s = store();
        let (work, conversation) = session_on(&s, &plain);

        let claim = s.claim_lease(&work, &conversation, &plain).unwrap();
        let Claim::NotGit { card_id, .. } = claim else {
            panic!("a folder that is not a repository cannot be claimed, got {claim:?}");
        };
        let card = s.card(card_id).unwrap().expect("the card was raised");
        assert!(card.is_open());
        assert!(
            card.blocking,
            "a session with nowhere to write cannot proceed, and the rail must say so"
        );
        assert!(card.title.contains("cannot claim"));
        assert!(s.work_leases(&work).unwrap().is_empty(), "nothing was recorded");
    }

    #[test]
    fn releasing_a_clean_merged_lease_removes_the_worktree() {
        let (_env, dir) = scratch("release-clean");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        let released = s.release_lease(lease.id).unwrap();
        assert!(released.removed(), "nothing was on it, got {released:?}");
        assert!(!lease.worktree_path.exists(), "the directory is gone");
        assert_eq!(s.lease(lease.id).unwrap().unwrap().state, State::Removed);
        assert!(
            !s.roots(&conversation)
                .unwrap()
                .iter()
                .any(|r| r.path == roots::normalise(&lease.worktree_path)),
            "a writable root pointing at a directory that is gone tells the agent it may \
             write somewhere it cannot"
        );
    }

    #[test]
    fn releasing_a_dirty_lease_keeps_it_and_says_why() {
        let (_env, dir) = scratch("release-dirty");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();
        std::fs::write(lease.worktree_path.join("half-done.rs"), "fn main() {}\n").unwrap();

        let released = s.release_lease(lease.id).unwrap();
        let Release::Kept {
            condition, reason, ..
        } = &released
        else {
            panic!("an untracked file is uncommitted work, got {released:?}");
        };
        assert!(condition.dirty);
        assert!(!condition.safe_to_remove());
        assert!(reason.contains("uncommitted"), "{reason}");
        assert!(lease.worktree_path.is_dir(), "it is still on disk");
        assert_eq!(s.lease(lease.id).unwrap().unwrap().state, State::Released);
    }

    #[test]
    fn a_clean_but_unmerged_branch_is_kept_too() {
        let (_env, dir) = scratch("release-unmerged");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();
        std::fs::write(lease.worktree_path.join("done.rs"), "fn main() {}\n").unwrap();
        commit_all(&lease.worktree_path, "the work");

        let released = s.release_lease(lease.id).unwrap();
        let Release::Kept { condition, .. } = &released else {
            panic!("a commit nobody merged is work to lose, got {released:?}");
        };
        assert!(!condition.dirty, "committing made it clean");
        assert!(!condition.merged);
        assert!(lease.worktree_path.is_dir());
    }

    /// The whole reason [`Condition`] is not a column.
    #[test]
    fn a_leases_condition_is_read_from_git_at_the_moment_it_is_asked() {
        let (_env, dir) = scratch("condition");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        assert!(!s.lease_condition(&lease).unwrap().dirty);
        std::fs::write(lease.worktree_path.join("scratch.txt"), "x").unwrap();
        assert!(
            s.lease_condition(&lease).unwrap().dirty,
            "a lease that was clean a moment ago says nothing about now"
        );
        std::fs::remove_dir_all(&lease.worktree_path).unwrap();
        let gone = s.lease_condition(&lease).unwrap();
        assert!(gone.missing);
        assert!(gone.safe_to_remove(), "there is nothing left to lose");
    }

    /// E4.S8: cleaning up afterwards is a separate act, and it did not gain the
    /// power to destroy anything either.
    #[test]
    fn removing_a_worktree_by_hand_still_refuses_a_dirty_one() {
        let (_env, dir) = scratch("remove-dirty");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();
        std::fs::write(lease.worktree_path.join("half-done.rs"), "fn main() {}\n").unwrap();

        let out = s.remove_worktree(lease.id).unwrap();
        assert!(matches!(out, Release::Kept { .. }), "got {out:?}");
        assert!(lease.worktree_path.is_dir());
        assert_eq!(
            s.lease(lease.id).unwrap().unwrap().state,
            State::Held,
            "a refused removal changes nothing at all"
        );
    }

    #[test]
    fn a_lease_outlives_the_work_that_cut_it() {
        let (_env, dir) = scratch("orphan");
        let Some(repo) = fixture_repo(&dir.join("repo")) else {
            return;
        };
        let s = store();
        let (work, conversation) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&work, &conversation, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        let refused = s.delete_work(&work, None).unwrap();
        let crate::works::Deletion::Refused { confirmation, .. } = &refused else {
            panic!("a work holding a lease refuses the first time, got {refused:?}");
        };
        assert!(s.delete_work(&work, Some(confirmation)).unwrap().happened());

        let orphans = s.orphaned_leases().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, lease.id);
        assert!(orphans[0].work_id.is_none());
        assert_eq!(
            orphans[0].work_title, "tidy the parser",
            "an orphan that cannot say what it was for is one nobody dares delete"
        );
        assert!(lease.worktree_path.is_dir(), "deletion never removes a worktree");
        assert!(branches(&repo).lines().any(|b| b == lease.branch));
    }

    fn commit_all(dir: &Path, message: &str) {
        for args in [
            vec!["add", "-A"],
            vec![
                "-c",
                "user.name=Jod Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        ] {
            let out = Command::new("git")
                .current_dir(dir)
                .args(&args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
