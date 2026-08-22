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
use std::collections::HashMap;
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

/// Where a manager decided one engineer is allowed to write.
///
/// The decision belongs to the manager rather than to the engineer, and it is
/// made before the session starts. An engineer that discovers for itself that
/// it needs somewhere to write spends a turn finding out, and sometimes skips
/// the finding out entirely and writes in the checkout somebody is editing.
///
/// [`Placement::Explore`] is the default, because reading is the reversible
/// one: a placement nobody stated must not be the one that cuts a branch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Read-only. No branch, no worktree, no pull request.
    #[default]
    Explore,
    /// A branch and worktree of this engineer's own.
    Worktree,
    /// Join the worktree another work already holds on this repository.
    Share { work_id: String },
    /// Write in Reljod's real checkout. Gated — see [`direct_is_allowed`].
    Direct,
}

/// Every placement id, in the order a manager should consider them.
///
/// Exported because the `open_work` tool offers these as a `one_of`, and a
/// list of ids written out a second time in the schema is a list that goes
/// stale the first time a placement is added.
pub const PLACEMENT_IDS: [&str; 4] = ["explore", "worktree", "share", "direct"];

impl Placement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Explore => "explore",
            Placement::Worktree => "worktree",
            Placement::Share { .. } => "share",
            Placement::Direct => "direct",
        }
    }

    /// Read a placement out of the two arguments a tool call carries.
    ///
    /// Fallible, unlike [`State::parse`] beside it, and for a reason worth
    /// keeping: that one reads a column Jod itself wrote and can safely fall
    /// back to the common value, while this one reads what a model typed. A
    /// misspelt placement quietly becoming `explore` would give an engineer
    /// that was meant to write no writable root at all, and the first anybody
    /// heard of it would be the engineer reporting that it could not do its
    /// task.
    ///
    /// `share` is the one that needs a second argument, so it is the one that
    /// can be asked for incorrectly: naming no lender is a refusal here rather
    /// than an empty work id that fails later in [`Store::share_lease`],
    /// where the message would be about a work that does not exist instead of
    /// about the argument that was left out.
    pub fn parse(id: &str, share_with: Option<&str>) -> Result<Placement> {
        match id.trim() {
            "explore" => Ok(Placement::Explore),
            "worktree" => Ok(Placement::Worktree),
            "share" => match share_with.map(str::trim).filter(|w| !w.is_empty()) {
                Some(work_id) => Ok(Placement::Share {
                    work_id: work_id.to_string(),
                }),
                None => Err(JodError::Invalid(
                    "a placement of `share` means joining the worktree another work already \
                     holds, so it needs `share_with` naming that work — or use `worktree` to \
                     cut one of this engineer's own"
                        .into(),
                )),
            },
            "direct" => Ok(Placement::Direct),
            other => Err(JodError::Invalid(format!(
                "`{other}` is not a placement. It is one of: {}",
                PLACEMENT_IDS.join(", ")
            ))),
        }
    }

    /// Whether this placement gives the session somewhere of its own to write.
    pub fn writes(&self) -> bool {
        !matches!(self, Placement::Explore)
    }
}

/// A session borrowing a worktree that another work holds the lease on.
///
/// One worktree is one lease, so a borrower is a row here rather than a second
/// row in `leases` — see [`Store::share_lease`] for why that distinction is
/// load-bearing rather than tidy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseSharer {
    pub lease_id: i64,
    pub conversation_id: String,
    /// Null for a session that is not part of any work.
    pub work_id: Option<String>,
    pub shared_at_ms: i64,
}

/// Whether an engineer may write straight into the real checkout.
///
/// Every failing condition is carried, not just the first. A manager told only
/// that there is a remote fixes that, asks again, and is then told about the
/// uncommitted changes — two turns spent learning two facts that were both
/// true at the same moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectVerdict {
    pub allowed: bool,
    /// Every condition that failed, in the words the refusal prints.
    pub because: Vec<String>,
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

impl GitRun {
    /// Whichever stream carries the message, for a run that failed.
    ///
    /// Git puts most of its complaints on stderr and a few of them on stdout,
    /// and a refusal that quotes the empty one tells the reader nothing at the
    /// moment they most need telling.
    fn said(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else {
            self.stderr.clone()
        }
    }
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

    /// Put a session into a worktree another work already holds.
    ///
    /// [`Store::claim_lease`] already shares within one work — a sibling on the
    /// same job is offered the lease rather than cutting a second branch for
    /// it. This is the same courtesy across works, which is the case a manager
    /// needs when it wants a second engineer working alongside the first
    /// instead of on a branch of its own. The two engineers own different files
    /// by the plan that placed them; that plan is what keeps them out of each
    /// other's way, and it only means anything if they really are in one
    /// directory.
    ///
    /// **No second `leases` row.** One worktree is one lease. The partial
    /// unique index is on `(work_id, repo_path)`, so a second row for the same
    /// directory under a different work would not even be refused — it would be
    /// accepted, and then [`Store::release_lease`] on either one would remove a
    /// tree the other session is standing in. The borrower goes in
    /// `lease_sharers` instead, which is also what the release refusal below
    /// reads.
    ///
    /// A lender that holds no lease is a plain refusal. Cutting a fresh
    /// worktree instead would be the worst possible fallback: the manager would
    /// believe two engineers were sharing a directory and dividing the files
    /// between them, while they were in fact on two branches, and the path
    /// ownership it planned so carefully would be protecting nothing.
    ///
    /// ## Path ownership is checked here, because here is where it can break
    ///
    /// [`Store::plan_work`] refuses a plan whose tasks claim overlapping files,
    /// but a plan belongs to one work and it is written before anybody decides
    /// where its engineers will sit. Sharing is the one thing that puts two
    /// *different* works in one directory, so it is the moment two plans that
    /// were each internally consistent can collide — and nothing above this
    /// function is in a position to notice.
    ///
    /// So every session already in the worktree is asked what its open task
    /// owns, and the borrower's open task is compared against all of it with
    /// [`crate::works::overlapping`]. A collision names both tasks and both
    /// paths and the share does not happen.
    ///
    /// A borrower with no task on `conversations.task_id` claims nothing, so
    /// there is nothing to compare and the share goes ahead. That is a real
    /// hole rather than a safe default: it means an engineer nobody gave a task
    /// to can be put anywhere. It is left open deliberately, because the
    /// alternative is refusing every share until every caller writes that
    /// column, and the column is written by `open_work` — which is where the
    /// fix belongs.
    pub fn share_lease(
        &self,
        work_id: &str,
        conversation_id: &str,
        lender_work_id: &str,
        repo_path: &Path,
    ) -> Result<Claim> {
        let asked = roots::normalise(repo_path);
        // A path that is not in a repository at all falls through to the same
        // refusal, which is the true answer either way: there is no held lease
        // there to join.
        let repo = toplevel(&asked)?.unwrap_or(asked);
        let Some(lease) = self.held_lease(lender_work_id, &repo)? else {
            let lender = self
                .work(lender_work_id)?
                .map(|w| format!("`{}` ({lender_work_id})", w.title))
                .unwrap_or_else(|| format!("`{lender_work_id}`"));
            return Err(JodError::Invalid(format!(
                "work {lender} holds no worktree on `{}`, so there is nothing to share. Place \
                 this engineer with `worktree` to cut one of its own, or share with the work \
                 that actually holds the one you meant.",
                repo.display()
            )));
        };

        // A borrower whose own work already holds a worktree here would come
        // out of this with two writable roots while the manager believed it had
        // put it in one. Nothing reaches that state today, which is the reason
        // to close it now rather than after something does.
        if let Some(own) = self.held_lease(work_id, &repo)? {
            if own.id != lease.id {
                return Err(JodError::Invalid(format!(
                    "this work already holds its own worktree on `{}` — `{}`, on branch `{}`. \
                     Sharing as well would leave this engineer with two places it may write \
                     and no way to say which one it meant, so release that one first or drop \
                     the share.",
                    repo.display(),
                    own.worktree_path.display(),
                    own.branch
                )));
            }
        }

        self.refuse_a_collision_in(&lease, conversation_id)?;

        let at = now_ms();
        self.write(|tx| {
            // `OR IGNORE`, because a session asking to share a worktree it is
            // already in has said something true. Keeping the first
            // `shared_at_ms` is the point: it is when this session actually
            // arrived, and a retry must not rewrite that.
            tx.execute(
                "INSERT OR IGNORE INTO lease_sharers
                   (lease_id, conversation_id, work_id, shared_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![lease.id, conversation_id, work_id, at],
            )?;
            Ok(())
        })?;
        self.bind_lease_roots(conversation_id, &lease)?;
        Ok(Claim::Reused(lease))
    }

    /// Refuse a borrower whose task owns a file somebody already in this
    /// worktree owns.
    ///
    /// Everybody in the directory is asked, not only the lender: a third
    /// engineer joining a worktree two are already sharing has to clear both of
    /// them, and checking only the lease holder would wave through exactly the
    /// collision that gets more likely as more engineers arrive.
    ///
    /// The borrower's own row is skipped, so a session asking to join a
    /// worktree it is already in does not collide with itself.
    fn refuse_a_collision_in(&self, lease: &Lease, borrower: &str) -> Result<()> {
        let Some((mine_title, mine)) = self.open_task_of(borrower)? else {
            return Ok(());
        };
        let mut occupants: Vec<String> = lease.conversation_id.clone().into_iter().collect();
        occupants.extend(
            self.lease_sharers(lease.id)?
                .into_iter()
                .map(|s| s.conversation_id),
        );
        for occupant in occupants {
            if occupant == borrower {
                continue;
            }
            let Some((theirs_title, theirs)) = self.open_task_of(&occupant)? else {
                continue;
            };
            if let Some((a, b)) = crate::works::overlapping(&mine, &theirs) {
                return Err(JodError::Invalid(format!(
                    "`{mine_title}` claims `{a}` and `{theirs_title}`, already working in \
                     `{}`, claims `{b}` — one is inside the other, and two engineers in one \
                     worktree cannot both own the same file. Give this engineer a worktree of \
                     its own, or plan the two tasks around different files.",
                    lease.worktree_path.display()
                )));
            }
        }
        Ok(())
    }

    /// The title and owned paths of the unfinished task this session was
    /// spawned onto, or `None` when it has none.
    ///
    /// `None` covers three different situations that all mean the same thing
    /// here — no `conversations.task_id`, a task that has been deleted, and a
    /// task already marked done — because none of them gives this session a
    /// file it can be said to own.
    fn open_task_of(&self, conversation_id: &str) -> Result<Option<(String, Vec<String>)>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT COALESCE(t.title, t.id), t.paths
                   FROM conversations c JOIN tasks t ON t.id = c.task_id
                  WHERE c.id = ?1 AND t.status != 'done'",
                params![conversation_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        crate::works::paths_from_column(r.get(1)?),
                    ))
                },
            )
            .optional()?)
    }

    /// Every session borrowing this lease's worktree, in the order they joined.
    pub fn lease_sharers(&self, lease_id: i64) -> Result<Vec<LeaseSharer>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT lease_id, conversation_id, work_id, shared_at_ms
               FROM lease_sharers WHERE lease_id = ?1 ORDER BY shared_at_ms, conversation_id",
        )?;
        let rows = stmt.query_map(params![lease_id], |r| {
            Ok(LeaseSharer {
                lease_id: r.get(0)?,
                conversation_id: r.get(1)?,
                work_id: r.get(2)?,
                shared_at_ms: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Step out of a worktree somebody else holds the lease on.
    ///
    /// The way out of the refusal in [`Store::release_lease`]. Without it a
    /// shared worktree could never be given up at all: the holder would be
    /// refused for as long as the borrower's row existed, and nothing would
    /// ever remove that row while the borrower's conversation was still there.
    ///
    /// Removing a sharer that was never attached is not an error. The caller is
    /// a session finishing up, and "you were not in this worktree" is not
    /// something it can act on.
    pub fn unshare_lease(&self, lease_id: i64, conversation_id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM lease_sharers WHERE lease_id = ?1 AND conversation_id = ?2",
                params![lease_id, conversation_id],
            )?;
            Ok(())
        })
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

    /// Every worktree currently held, keyed by the work holding it.
    ///
    /// One query for the whole fleet rather than one per work, because the
    /// caller is [`Store::forest_of`] — a screen redraw, where a query per row
    /// is the difference between a tree and a stutter.
    ///
    /// Held only. A released lease may still have a directory on disk, but the
    /// question a fleet row is answering is "where is this agent working now",
    /// and a released one is not anywhere.
    ///
    /// The unique index on `(work_id, repo_path) WHERE state = 'held'` means at
    /// most one per work per repository. A work spanning two repositories
    /// keeps the first, which is the one its session was started in.
    pub fn held_leases_by_work(&self) -> Result<HashMap<String, Lease>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM leases
              WHERE state = 'held' AND work_id IS NOT NULL
              ORDER BY created_at_ms, id"
        ))?;
        let rows = stmt.query_map([], read_lease)?;
        let mut out: HashMap<String, Lease> = HashMap::new();
        for lease in rows {
            let lease = lease?;
            if let Some(work) = lease.work_id.clone() {
                out.entry(work).or_insert(lease);
            }
        }
        Ok(out)
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
    /// what is refused is destroying something, never making progress. The one
    /// exception is a worktree somebody else is sharing: see below.
    pub fn release_lease(&self, lease_id: i64) -> Result<Release> {
        let Some(lease) = self.lease(lease_id)? else {
            return Err(JodError::Invalid(format!("no lease #{lease_id}")));
        };
        if lease.state == State::Removed {
            return Err(JodError::Invalid(format!(
                "lease #{lease_id} was already removed from disk"
            )));
        }
        if let Some(reason) = self.kept_for_a_sharer(&lease)? {
            // Nothing is marked released here, unlike the dirty and unmerged
            // refusals below. A lease that stopped being held while a borrower
            // was still writing in the worktree would let the next work cut a
            // second one on the same repository — the partial unique index only
            // covers held leases — and the fleet would stop showing the
            // directory an engineer is standing in.
            let condition = self.lease_condition(&lease)?;
            return Ok(Release::Kept {
                lease,
                condition,
                reason,
            });
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
        if let Some(reason) = self.kept_for_a_sharer(&lease)? {
            let condition = self.lease_condition(&lease)?;
            return Ok(Release::Kept {
                lease,
                condition,
                reason,
            });
        }
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

    /// The refusal for a worktree somebody else is still standing in, or
    /// `None` when nobody is.
    ///
    /// Only sharers *other than* the session holding the lease count. The
    /// holder giving up its own lease is the ordinary case and it is not
    /// somebody else, so a session that both holds and shares — which
    /// [`Store::share_lease`] permits, because asking to join a worktree you
    /// are already in is a true thing to say — must not block itself.
    ///
    /// The words match [`Condition::why_kept`] deliberately: this is one more
    /// reason a worktree was kept, printed in the same sentence as the others,
    /// and a reader should not have to notice which of them they got.
    fn kept_for_a_sharer(&self, lease: &Lease) -> Result<Option<String>> {
        let holder = lease.conversation_id.as_deref();
        let others: Vec<String> = self
            .lease_sharers(lease.id)?
            .into_iter()
            .filter(|s| Some(s.conversation_id.as_str()) != holder)
            .map(|s| format!("`{}`", s.conversation_id))
            .collect();
        if others.is_empty() {
            return Ok(None);
        }
        let who = if others.len() == 1 {
            format!("session {} is still working in it", others[0])
        } else {
            format!("sessions {} are still working in it", others.join(", "))
        };
        Ok(Some(format!(
            "kept `{}`: {who}",
            lease.worktree_path.display()
        )))
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

/// Whether an engineer may be placed straight into the real checkout.
///
/// Writing into the directory Reljod is editing is the rarest placement, and it
/// is decided on facts rather than on judgement. A model asking itself whether
/// something feels like a fresh project is exactly how a session ends up
/// committing on top of somebody's afternoon, so the three questions here are
/// all ones a database row or a `git` invocation answers:
///
/// 1. **No git remote.** Reljod tied the remote to the pull request rule, so
///    one fact decides both: a repository with a remote gets a branch and a
///    pull request, always.
/// 2. **No other work on this project.** "The first iteration", read out of the
///    works table instead of guessed at. Called before the new work exists, so
///    any row at all is somebody else.
/// 3. **The checkout is clean.** Writing into a tree with uncommitted changes
///    in it is the accident the whole lease system was built to prevent, and a
///    fresh project is not an exemption from it.
///
/// All three are checked even once one has failed, because a manager told only
/// the first reason fixes it, asks again, and is told the second — two turns
/// spent on two facts that were both true at the same moment.
pub fn direct_is_allowed(store: &Store, project_id: &str, repo: &Path) -> Result<DirectVerdict> {
    let asked = roots::normalise(repo);
    let mut because = Vec::new();

    match toplevel(&asked)? {
        None => {
            // Neither the remote nor the cleanliness question has an answer
            // here, and both would have been answered "no" by a failing `git`
            // for a reason that has nothing to do with what was asked. One
            // honest reason is worth more than two misleading ones.
            because.push(format!(
                "`{}` is not inside a git repository, so there is no way to tell what writing \
                 in it would disturb",
                asked.display()
            ));
        }
        Some(checkout) => {
            because.extend(remote_reason(&checkout, &git(&checkout, &["remote"])?));
            because.extend(cleanliness_reason(
                &checkout,
                &git(&checkout, &["status", "--porcelain"])?,
            ));
        }
    }

    let others: i64 = {
        let conn = store.conn.lock().expect("store lock poisoned");
        conn.query_row(
            "SELECT COUNT(*) FROM works WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )?
    };
    if others > 0 {
        because.push(format!(
            "this project already has {others} work{} on it, so this is not its first \
             iteration",
            if others == 1 { "" } else { "s" }
        ));
    }

    Ok(DirectVerdict {
        allowed: because.is_empty(),
        because,
    })
}

/// What `git remote` said, read as a reason `direct` is not allowed.
///
/// A failure is its own reason rather than silence. `git remote` prints nothing
/// when there is no remote and prints nothing when it could not run, and
/// letting the second look like the first would quietly drop a condition from a
/// list whose whole purpose is to be complete.
///
/// Split out from [`direct_is_allowed`] with [`cleanliness_reason`] because
/// that failure is unreachable through a real checkout — a repository broken
/// enough to fail `git remote` fails `git rev-parse` first, so the caller never
/// gets this far — and a branch no test can reach is a branch that rots.
fn remote_reason(checkout: &Path, run: &GitRun) -> Option<String> {
    if !run.ok {
        return Some(format!(
            "the remotes of `{}` could not be read, so it cannot be called remoteless: {}",
            checkout.display(),
            run.said()
        ));
    }
    if run.stdout.is_empty() {
        return None;
    }
    Some(format!(
        "`{}` has a git remote ({}), and a repository with a remote gets a branch and a pull \
         request",
        checkout.display(),
        run.stdout
            .lines()
            .map(|r| format!("`{}`", r.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// What `git status --porcelain` said, read as a reason `direct` is not
/// allowed. A status that could not be read is not a clean tree.
fn cleanliness_reason(checkout: &Path, run: &GitRun) -> Option<String> {
    if !run.ok {
        return Some(format!(
            "the state of `{}` could not be read, so it cannot be called clean: {}",
            checkout.display(),
            run.said()
        ));
    }
    if run.stdout.is_empty() {
        return None;
    }
    Some(format!(
        "`{}` has uncommitted changes in it, and they are somebody's work in progress",
        checkout.display()
    ))
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
/// Panics when git is not installed. Returning `None` for a caller to bail on
/// used to look like the polite thing to do, but a test that returns early
/// still reports as a pass, so the suite said "green" on a machine where half
/// of it never executed. A panic is the truthful answer: git is not an
/// optional extra for these tests, it is the thing under test.
#[cfg(test)]
pub(crate) fn fixture_repo(dir: &Path) -> PathBuf {
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => panic!(
                "`git` is not installed on this machine, and a lease is a git worktree, \
                 so this test cannot run. Install git and run the suite again."
            ),
            Err(e) => panic!("could not run `git {}`: {e}", args.join(" ")),
            Ok(out) if !out.status.success() => panic!(
                "`git {}` failed in the fixture: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ),
            Ok(_) => {}
        }
    }
    roots::normalise(dir)
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
        let repo = fixture_repo(&dir.join("repo"));
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

    /// The fleet has to be able to say where an agent is actually writing.
    ///
    /// A work session reads the checkout and writes to a worktree it claimed
    /// part-way through its run, so it can truthfully report a file changed
    /// while the checkout somebody is looking at is untouched. Until the forest
    /// carried these two fields nothing on any screen named the directory to
    /// look in — the tree's `Node` had no `cwd`, no branch and no lease.
    #[test]
    fn a_work_holding_a_worktree_says_so_on_the_fleet() {
        use crate::tree::NodeKind;

        let (_env, dir) = scratch("forest");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (work, conversation) = session_on(&s, &repo);

        let before = s.forest().unwrap();
        assert!(
            before
                .iter()
                .filter(|n| n.kind == NodeKind::Work || n.kind == NodeKind::Session)
                .all(|n| n.branch.is_none() && n.worktree.is_none()),
            "a work that has claimed nothing says nothing, rather than guessing",
        );

        let Claim::Cut(lease) = s.claim_lease(&work, &conversation, &repo).unwrap() else {
            panic!("the first claim on a repository cuts a branch");
        };

        let forest = s.forest().unwrap();
        let work_row = forest
            .iter()
            .find(|n| n.id == crate::tree::NodeId::work(&work))
            .expect("the work is on the fleet");
        assert_eq!(work_row.branch.as_deref(), Some(lease.branch.as_str()));
        assert_eq!(
            work_row.worktree.as_deref(),
            Some(lease.worktree_path.to_string_lossy().as_ref()),
        );

        // The session too, because `condense` folds the work row away when it
        // holds one session — so the row a person actually sees is that one,
        // and a branch only on the work would be a branch nobody reads.
        let session_row = forest
            .iter()
            .find(|n| n.id == crate::tree::NodeId::session(&conversation))
            .expect("the session is on the fleet");
        assert_eq!(session_row.branch.as_deref(), Some(lease.branch.as_str()));

        // Released, and the row stops claiming a worktree it no longer holds.
        s.release_lease(lease.id).unwrap();
        let after = s.forest().unwrap();
        assert!(
            after
                .iter()
                .filter(|n| n.kind == NodeKind::Work || n.kind == NodeKind::Session)
                .all(|n| n.branch.is_none()),
            "a released lease is not where the agent is working any more",
        );
    }

    /// The partial unique index says one live lease per work and repository.
    /// This is that rule read out loud, so a sibling is offered the worktree
    /// rather than finding out through a constraint error.
    #[test]
    fn a_sibling_session_is_offered_the_lease_rather_than_a_second_branch() {
        let (_env, dir) = scratch("reuse");
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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
        let repo = fixture_repo(&dir.join("repo"));
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

    /// How many lease rows point at one directory. The number `share_lease`
    /// exists to keep at one.
    fn leases_on(store: &Store, worktree: &Path) -> i64 {
        let conn = store.conn.lock().expect("store lock poisoned");
        conn.query_row(
            "SELECT COUNT(*) FROM leases WHERE worktree_path = ?1",
            params![worktree.to_string_lossy()],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Plan one task onto a work's board and hand back the row it wrote.
    ///
    /// Found by title rather than taken from the front of the board, because a
    /// work already has a task when it is created — the instruction itself —
    /// and that one claims no files. Reading index zero here points every
    /// session at the pathless task and quietly tests nothing, which is exactly
    /// what the first draft of these tests did.
    fn plan_one(
        store: &Store,
        work_id: &str,
        title: &str,
        paths: &[&str],
    ) -> crate::team::TeamTask {
        let plan = crate::works::Plan {
            tasks: vec![crate::works::PlannedTask {
                title: title.to_string(),
                paths: paths.iter().map(|p| (*p).to_string()).collect(),
            }],
        };
        let board = store.plan_work(work_id, &plan).unwrap();
        let task = board
            .into_iter()
            .find(|t| t.title == title)
            .unwrap_or_else(|| panic!("`{title}` is on the board `plan_work` returned"));
        assert_eq!(
            task.paths,
            paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            "and it owns the paths it was planned with"
        );
        task
    }

    /// Point a session at the task it was spawned onto.
    ///
    /// Raw SQL because `open_work` is the only writer of this column and it
    /// lives in `core/src/mcp.rs`, which this test cannot reach into.
    fn spawn_onto(store: &Store, conversation_id: &str, task_id: &str) {
        store
            .write(|tx| {
                tx.execute(
                    "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                    params![conversation_id, task_id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// The ids the `open_work` schema offers have to be ids this file accepts.
    ///
    /// The constant and the enum are two spellings of one list, and the way
    /// they go wrong is silent: a placement added to the enum and forgotten in
    /// the constant is one a manager is never offered, and one removed from the
    /// enum but left in the constant is one the tool advertises and then
    /// refuses.
    #[test]
    fn every_placement_id_the_schema_offers_is_one_that_parses_back() {
        for id in PLACEMENT_IDS {
            let placement = Placement::parse(id, Some("wk_lender"))
                .unwrap_or_else(|e| panic!("`{id}` is offered by the schema but refused: {e}"));
            assert_eq!(placement.as_str(), id, "and it round-trips to the same id");
        }
        assert_eq!(
            Placement::default(),
            Placement::Explore,
            "a placement nobody stated is the reversible one"
        );
        let no_lender = Placement::parse("share", None).unwrap_err().to_string();
        assert!(
            no_lender.contains("share_with"),
            "sharing with nobody names the argument that was left out: {no_lender}"
        );
        let unknown = Placement::parse("worktre", None).unwrap_err().to_string();
        assert!(
            unknown.contains("explore") && unknown.contains("worktree"),
            "a misspelt placement is told what the real ones are: {unknown}"
        );
    }

    /// One worktree is one lease, and a borrower is a row beside it.
    ///
    /// A second `leases` row for the same directory would not be refused by
    /// the partial unique index — that is per work — and releasing either of
    /// them would then remove a tree the other session is standing in.
    #[test]
    fn sharing_a_worktree_across_works_adds_a_sharer_rather_than_a_second_lease() {
        let (_env, dir) = scratch("share");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (lender_work, lender) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);

        let lease = s
            .claim_lease(&lender_work, &lender, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();
        let shared = s
            .share_lease(&borrower_work, &borrower, &lender_work, &repo)
            .unwrap();

        assert_eq!(
            shared.lease().map(|l| l.id),
            Some(lease.id),
            "the borrower is put in the lender's worktree, got {shared:?}"
        );
        assert_eq!(
            leases_on(&s, &lease.worktree_path),
            1,
            "one directory, one lease"
        );
        assert!(
            s.work_leases(&borrower_work).unwrap().is_empty(),
            "the borrowing work cut nothing of its own"
        );

        let sharers = s.lease_sharers(lease.id).unwrap();
        assert_eq!(sharers.len(), 1);
        assert_eq!(sharers[0].conversation_id, borrower);
        assert_eq!(sharers[0].work_id.as_deref(), Some(borrower_work.as_str()));

        let roots = s.roots(&borrower).unwrap();
        assert!(
            roots
                .iter()
                .any(|r| r.writable && r.path == roots::normalise(&lease.worktree_path)),
            "the borrower writes in the same directory the lender does"
        );
        let checkout = roots
            .iter()
            .find(|r| r.path == repo)
            .expect("the real checkout is still readable, so it can diff against it");
        assert!(!checkout.writable);
    }

    /// Falling back to cutting a worktree here would be the worst answer
    /// available: the manager would believe two engineers were dividing the
    /// files in one directory while they were in fact on two branches.
    #[test]
    fn sharing_with_a_work_that_holds_no_worktree_is_refused_rather_than_cutting_one() {
        let (_env, dir) = scratch("share-nothing");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (idle_work, _idle) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);

        let refused = s
            .share_lease(&borrower_work, &borrower, &idle_work, &repo)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&idle_work),
            "the refusal names the work that was asked and holds nothing: {refused}"
        );
        assert!(
            refused.contains("worktree"),
            "and says what to do instead: {refused}"
        );

        assert!(s.work_leases(&idle_work).unwrap().is_empty());
        assert!(s.work_leases(&borrower_work).unwrap().is_empty());
        assert!(
            !branches(&repo).lines().any(|b| b.starts_with("jod/")),
            "no branch was cut behind the manager's back: {}",
            branches(&repo)
        );
        assert!(
            !s.roots(&borrower).unwrap().iter().any(|r| r.writable),
            "and the borrower still has nowhere to write"
        );
    }

    /// `plan_work` checks a plan against its own work's board, and sharing is
    /// the one thing that puts two boards in one directory. If nothing checked
    /// here, the file ownership a manager planned would stop meaning anything
    /// at exactly the moment two engineers were put in the same worktree.
    #[test]
    fn a_borrower_whose_task_owns_a_file_somebody_in_the_worktree_owns_is_refused() {
        let (_env, dir) = scratch("share-collision");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (lender_work, lender) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);
        let theirs = plan_one(&s, &lender_work, "rewrite the parser", &["core/src"]);
        let mine = plan_one(&s, &borrower_work, "tidy the store", &["core/src/store.rs"]);
        spawn_onto(&s, &lender, &theirs.id);
        spawn_onto(&s, &borrower, &mine.id);
        let lease = s
            .claim_lease(&lender_work, &lender, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        let refused = s
            .share_lease(&borrower_work, &borrower, &lender_work, &repo)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("tidy the store") && refused.contains("rewrite the parser"),
            "both tasks are named, or the manager has to diff two boards to find the \
             collision: {refused}"
        );
        assert!(
            refused.contains("core/src/store.rs") && refused.contains("core/src`"),
            "and both paths, so it can see which one is inside the other: {refused}"
        );

        assert!(
            s.lease_sharers(lease.id).unwrap().is_empty(),
            "a refused share attaches nobody"
        );
        assert!(
            !s.roots(&borrower).unwrap().iter().any(|r| r.writable),
            "and leaves the borrower with nowhere to write"
        );
    }

    /// The other half of the refusal above: two tasks that own different files
    /// are exactly what sharing a worktree is for.
    #[test]
    fn a_borrower_whose_task_owns_different_files_shares_the_worktree() {
        let (_env, dir) = scratch("share-disjoint");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (lender_work, lender) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);
        let theirs = plan_one(&s, &lender_work, "rewrite the parser", &["core/src"]);
        let mine = plan_one(&s, &borrower_work, "redraw the fleet", &["cli/src"]);
        spawn_onto(&s, &lender, &theirs.id);
        spawn_onto(&s, &borrower, &mine.id);
        let lease = s
            .claim_lease(&lender_work, &lender, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        let shared = s
            .share_lease(&borrower_work, &borrower, &lender_work, &repo)
            .unwrap();
        assert_eq!(shared.lease().map(|l| l.id), Some(lease.id));
        assert_eq!(s.lease_sharers(lease.id).unwrap().len(), 1);
        assert!(s
            .roots(&borrower)
            .unwrap()
            .iter()
            .any(|r| r.writable && r.path == roots::normalise(&lease.worktree_path)));
    }

    /// Two writable roots is a session that cannot say where it works, and a
    /// manager that believes it put it in one place.
    #[test]
    fn a_borrower_that_already_holds_its_own_worktree_here_is_refused() {
        let (_env, dir) = scratch("share-two-roots");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (lender_work, lender) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);
        s.claim_lease(&lender_work, &lender, &repo).unwrap();
        let own = s
            .claim_lease(&borrower_work, &borrower, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();

        let refused = s
            .share_lease(&borrower_work, &borrower, &lender_work, &repo)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&own.branch),
            "the refusal names the worktree it is already holding: {refused}"
        );
        assert!(
            s.lease_sharers(own.id).unwrap().is_empty(),
            "and nothing was attached anywhere"
        );
    }

    /// A `git` that could not run said nothing, and nothing is what "there is
    /// no remote" looks like too.
    ///
    /// Unreachable through a real checkout — a repository broken enough to fail
    /// `git remote` fails `git rev-parse` first, and `direct_is_allowed` stops
    /// there — so the two reasons are read from a run this test builds itself.
    /// The cost of getting it wrong is not a wrong verdict but a missing
    /// reason, which is the one thing D3.3 exists to prevent.
    #[test]
    fn a_git_command_that_failed_is_its_own_reason_rather_than_a_silent_pass() {
        let checkout = Path::new("/somewhere/broken");
        let failed = |said: &str| GitRun {
            ok: false,
            stdout: String::new(),
            stderr: said.to_string(),
        };
        let said = |out: &str| GitRun {
            ok: true,
            stdout: out.to_string(),
            stderr: String::new(),
        };

        let unreadable =
            remote_reason(checkout, &failed("fatal: bad config line 9")).expect("a reason");
        assert!(
            unreadable.contains("bad config line 9"),
            "and it quotes what git said: {unreadable}"
        );
        assert!(
            remote_reason(checkout, &said("")).is_none(),
            "silence from a run that worked really is no remote"
        );
        let named = remote_reason(checkout, &said("origin\nupstream")).expect("a reason");
        assert!(
            named.contains("`origin`") && named.contains("`upstream`"),
            "every remote is named: {named}"
        );

        assert!(
            cleanliness_reason(checkout, &failed("fatal: index file corrupt")).is_some(),
            "a tree whose state could not be read is not a clean tree"
        );
        assert!(cleanliness_reason(checkout, &said("")).is_none());
        assert!(cleanliness_reason(checkout, &said(" M core/src/leases.rs")).is_some());
    }

    /// Releasing a worktree somebody else is standing in is the mistake the
    /// sharers table exists to catch.
    #[test]
    fn a_lease_with_a_sharer_attached_is_kept_and_the_sharer_is_named() {
        let (_env, dir) = scratch("release-shared");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let (lender_work, lender) = session_on(&s, &repo);
        let (borrower_work, borrower) = session_on(&s, &repo);
        let lease = s
            .claim_lease(&lender_work, &lender, &repo)
            .unwrap()
            .lease()
            .cloned()
            .unwrap();
        s.share_lease(&borrower_work, &borrower, &lender_work, &repo)
            .unwrap();

        let kept = s.release_lease(lease.id).unwrap();
        let Release::Kept { reason, .. } = &kept else {
            panic!("a worktree with somebody in it is kept, got {kept:?}");
        };
        assert!(
            reason.contains(&borrower),
            "the refusal names who is still in there: {reason}"
        );
        assert!(lease.worktree_path.is_dir(), "and it is still on disk");
        assert_eq!(
            s.lease(lease.id).unwrap().unwrap().state,
            State::Held,
            "a lease that stopped being held would let the next work cut a second one on \
             the same repository"
        );

        // The borrower steps out, and the refusal has nothing left to hold.
        s.unshare_lease(lease.id, &borrower).unwrap();
        let released = s.release_lease(lease.id).unwrap();
        assert!(
            released.removed(),
            "nobody is left and nothing was on it, got {released:?}"
        );
        assert!(!lease.worktree_path.exists());
    }

    /// A manager told one reason fixes it, asks again, and is told the next
    /// one. Three turns to learn three facts that were all true at once.
    #[test]
    fn a_direct_placement_is_refused_with_every_reason_it_failed_not_only_the_first() {
        let (_env, dir) = scratch("direct-refused");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let project = s
            .add_project(crate::projects::NewProject::at(&repo))
            .unwrap();
        s.create_work_in("the iteration before this one", Some(&project.id))
            .unwrap();
        assert!(
            git(
                &repo,
                &["remote", "add", "origin", "https://example.invalid/r.git"]
            )
            .unwrap()
            .ok,
            "the fixture takes a remote"
        );
        std::fs::write(repo.join("half-done.rs"), "fn main() {}\n").unwrap();

        let verdict = direct_is_allowed(&s, &project.id, &repo).unwrap();
        assert!(!verdict.allowed);
        assert_eq!(
            verdict.because.len(),
            3,
            "all three conditions failed and all three are reported: {:?}",
            verdict.because
        );
        assert!(
            verdict.because.iter().any(|b| b.contains("remote")),
            "{:?}",
            verdict.because
        );
        assert!(
            verdict.because.iter().any(|b| b.contains("uncommitted")),
            "{:?}",
            verdict.because
        );
        assert!(
            verdict
                .because
                .iter()
                .any(|b| b.contains("first iteration")),
            "{:?}",
            verdict.because
        );
    }

    /// The one shape that is allowed: a fresh checkout nobody else has touched.
    #[test]
    fn a_direct_placement_is_allowed_on_a_clean_first_iteration_with_no_remote() {
        let (_env, dir) = scratch("direct-allowed");
        let repo = fixture_repo(&dir.join("repo"));
        let s = store();
        let project = s
            .add_project(crate::projects::NewProject::at(&repo))
            .unwrap();

        let verdict = direct_is_allowed(&s, &project.id, &repo).unwrap();
        assert!(
            verdict.allowed,
            "no remote, no other work, nothing uncommitted: {:?}",
            verdict.because
        );
        assert!(verdict.because.is_empty());

        // And each of the three, on its own, is enough to take it back.
        std::fs::write(repo.join("scratch.txt"), "x").unwrap();
        let dirty = direct_is_allowed(&s, &project.id, &repo).unwrap();
        assert!(!dirty.allowed);
        assert_eq!(dirty.because.len(), 1, "{:?}", dirty.because);
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
