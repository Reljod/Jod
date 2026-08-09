//! Agent teams — Jod's own message bus.
//!
//! Every harness is growing a team feature of its own, and each one can only
//! ever contain that harness. Jod owns the bus instead, so a single team can
//! have its lead on Claude Code and its teammates on Antigravity and OpenCode.
//! → `docs/decisions.md`
//!
//! Everything here is an **append-only JSONL file** under `~/.jod/teams/`, for
//! three reasons that all matter:
//!
//! - A single `O_APPEND` write of a small record is atomic on POSIX, so two
//!   teammates claiming the same task race safely without a lock. The first
//!   `Claimed` record in the file wins, and every reader agrees who that was.
//! - Jod's tailer already knows how to follow an append-only file, so delivery
//!   is a mechanism that already exists rather than a new one.
//! - The whole team stays readable with `cat` when Jod is not running, which is
//!   the rule the rest of the runtime state follows.
//!
//! State is a *fold* over the log rather than a mutable record, so there is no
//! read-modify-write window for anything to be lost in.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::Result;
use crate::harness::HarnessKind;
use crate::paths;

/// A member's coarse lifecycle. Deliberately small: recovery logic reasons
/// about this, and the fine-grained "where in the loop is it" belongs to the
/// agent's own event stream, which the UI already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    /// Idle, and a message will wake it.
    Ready,
    /// A run is in flight; a message will be picked up on the next turn.
    Busy,
    /// Asked to stop after the current turn.
    ShutdownRequested,
    Shutdown,
    Error,
}

/// One teammate. `agent_id` is the Jod agent currently embodying it — a member
/// outlives any single run, because each turn is a fresh resumed spawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub harness: HarnessKind,
    pub role: String,
    pub status: MemberStatus,
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The harness-side conversation to resume, so the member keeps its context
    /// across turns.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// A message on the bus. `to: None` is a broadcast to every other member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
    pub text: String,
    pub at_ms: i64,
}

impl Message {
    /// How a delivered message is presented to the receiving agent: as a
    /// synthetic user turn, because that is the only channel every harness has.
    pub fn as_prompt(&self) -> String {
        format!("[message from {}]\n{}", self.from, self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub claimed_by: Option<String>,
    #[serde(default)]
    pub done: bool,
}

/// One record in the task log. State is the fold of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TaskEvent {
    Added { id: String, title: String },
    Claimed { id: String, by: String },
    Completed { id: String },
}

/// One record in the member log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MemberEvent {
    Joined {
        name: String,
        harness: HarnessKind,
        role: String,
    },
    Status {
        name: String,
        status: MemberStatus,
    },
    Bound {
        name: String,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
}

/// A named team, addressed by name. Holds no state of its own — every question
/// is answered by folding the log, so two processes never disagree.
///
/// The directory is resolved once at construction rather than read from the
/// environment on every call, so a caller (or a test) can point a team
/// somewhere explicit without mutating process-wide state.
#[derive(Debug, Clone)]
pub struct Team {
    pub name: String,
    dir: PathBuf,
}

impl Team {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let dir = paths::team_dir(&name);
        Self { name, dir }
    }

    /// A team rooted at an explicit directory.
    pub fn in_dir(name: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self { name: name.into(), dir: dir.into() }
    }

    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub fn members_path(&self) -> PathBuf {
        self.dir.join("members.jsonl")
    }

    pub fn tasks_path(&self) -> PathBuf {
        self.dir.join("tasks.jsonl")
    }

    pub fn inbox_path(&self, member: &str) -> PathBuf {
        self.dir.join("inbox").join(format!("{}.jsonl", paths::sanitize(member)))
    }

    pub fn cursor_path(&self, member: &str) -> PathBuf {
        self.dir.join("inbox").join(format!("{}.cursor", paths::sanitize(member)))
    }

    async fn append<T: Serialize>(&self, path: PathBuf, record: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        // One O_APPEND write of one short line: atomic, so concurrent writers
        // interleave records rather than corrupting each other's.
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    /// Read a JSONL file into records, skipping lines that do not parse.
    ///
    /// A torn or unknown line must not take the whole team down — the same
    /// reasoning as `AgentEvent::Raw`, applied to storage.
    async fn read_log<T: for<'de> Deserialize<'de>>(&self, path: PathBuf) -> Vec<T> {
        let Ok(body) = tokio::fs::read_to_string(&path).await else {
            return vec![];
        };
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    // ---- members -------------------------------------------------------

    pub async fn join(&self, name: &str, harness: HarnessKind, role: &str) -> Result<()> {
        self.append(
            self.members_path(),
            &MemberEvent::Joined {
                name: name.to_string(),
                harness,
                role: role.to_string(),
            },
        )
        .await
    }

    pub async fn set_status(&self, name: &str, status: MemberStatus) -> Result<()> {
        self.append(
            self.members_path(),
            &MemberEvent::Status {
                name: name.to_string(),
                status,
            },
        )
        .await
    }

    /// Record which run currently embodies a member, and which harness-side
    /// conversation to resume for its next turn.
    pub async fn bind(
        &self,
        name: &str,
        agent_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<()> {
        self.append(
            self.members_path(),
            &MemberEvent::Bound {
                name: name.to_string(),
                agent_id,
                session_id,
            },
        )
        .await
    }

    pub async fn members(&self) -> Vec<Member> {
        let events: Vec<MemberEvent> = self.read_log(self.members_path()).await;
        let mut order: Vec<String> = vec![];
        let mut by_name: HashMap<String, Member> = HashMap::new();

        for event in events {
            match event {
                MemberEvent::Joined { name, harness, role } => {
                    if !by_name.contains_key(&name) {
                        order.push(name.clone());
                    }
                    by_name.insert(
                        name.clone(),
                        Member {
                            name,
                            harness,
                            role,
                            status: MemberStatus::Ready,
                            agent_id: None,
                            session_id: None,
                        },
                    );
                }
                MemberEvent::Status { name, status } => {
                    if let Some(m) = by_name.get_mut(&name) {
                        m.status = status;
                    }
                }
                MemberEvent::Bound { name, agent_id, session_id } => {
                    if let Some(m) = by_name.get_mut(&name) {
                        m.agent_id = agent_id;
                        // A resumed turn reports the same conversation id, so a
                        // None here must not erase a known session.
                        if session_id.is_some() {
                            m.session_id = session_id;
                        }
                    }
                }
            }
        }
        order.into_iter().filter_map(|n| by_name.remove(&n)).collect()
    }

    pub async fn member(&self, name: &str) -> Option<Member> {
        self.members().await.into_iter().find(|m| m.name == name)
    }

    // ---- messaging -----------------------------------------------------

    /// Deliver a message. A `to` of `None` fans out to every member except the
    /// sender, so a broadcast is still one file per recipient and readers never
    /// have to merge two sources.
    pub async fn send(&self, message: &Message) -> Result<Vec<String>> {
        let recipients: Vec<String> = match &message.to {
            Some(to) => vec![to.clone()],
            None => self
                .members()
                .await
                .into_iter()
                .map(|m| m.name)
                .filter(|n| n != &message.from)
                .collect(),
        };
        for name in &recipients {
            self.append(self.inbox_path(name), message)
                .await?;
        }
        Ok(recipients)
    }

    pub async fn inbox(&self, member: &str) -> Vec<Message> {
        self.read_log(self.inbox_path(member))
            .await
    }

    /// Messages a member has not yet been shown, and the new cursor.
    ///
    /// The cursor is a count rather than a byte offset so it stays correct if
    /// the file is rewritten, and is only advanced by `mark_read` once the
    /// messages have actually been handed to the agent.
    pub async fn unread(&self, member: &str) -> Vec<Message> {
        let read = self.cursor(member).await;
        self.inbox(member).await.into_iter().skip(read).collect()
    }

    async fn cursor(&self, member: &str) -> usize {
        tokio::fs::read_to_string(self.cursor_path(member))
            .await
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub async fn mark_read(&self, member: &str, count: usize) -> Result<()> {
        let path = self.cursor_path(member);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, count.to_string()).await?;
        Ok(())
    }

    /// Take every pending message for a member and advance its cursor, so the
    /// same message is never injected into two turns.
    pub async fn drain(&self, member: &str) -> Result<Vec<Message>> {
        let all = self.inbox(member).await;
        let read = self.cursor(member).await;
        let pending: Vec<Message> = all.iter().skip(read).cloned().collect();
        if !pending.is_empty() {
            self.mark_read(member, all.len()).await?;
        }
        Ok(pending)
    }

    // ---- tasks ---------------------------------------------------------

    pub async fn add_task(&self, id: &str, title: &str) -> Result<()> {
        self.append(
            self.tasks_path(),
            &TaskEvent::Added {
                id: id.to_string(),
                title: title.to_string(),
            },
        )
        .await
    }

    /// Try to claim a task. Returns whether this member got it.
    ///
    /// Two members claiming at once both append; the fold makes the *first*
    /// record win, and both then agree on the winner. No lock, no lost work.
    pub async fn claim_task(&self, id: &str, by: &str) -> Result<bool> {
        self.append(
            self.tasks_path(),
            &TaskEvent::Claimed {
                id: id.to_string(),
                by: by.to_string(),
            },
        )
        .await?;
        Ok(self
            .tasks()
            .await
            .into_iter()
            .find(|t| t.id == id)
            .and_then(|t| t.claimed_by)
            .is_some_and(|winner| winner == by))
    }

    pub async fn complete_task(&self, id: &str) -> Result<()> {
        self.append(
            self.tasks_path(),
            &TaskEvent::Completed { id: id.to_string() },
        )
        .await
    }

    pub async fn tasks(&self) -> Vec<Task> {
        let events: Vec<TaskEvent> = self.read_log(self.tasks_path()).await;
        let mut order: Vec<String> = vec![];
        let mut by_id: HashMap<String, Task> = HashMap::new();

        for event in events {
            match event {
                TaskEvent::Added { id, title } => {
                    if !by_id.contains_key(&id) {
                        order.push(id.clone());
                        by_id.insert(
                            id.clone(),
                            Task { id, title, claimed_by: None, done: false },
                        );
                    }
                }
                TaskEvent::Claimed { id, by } => {
                    if let Some(t) = by_id.get_mut(&id) {
                        // First claim wins — this is the whole atomicity story.
                        if t.claimed_by.is_none() {
                            t.claimed_by = Some(by);
                        }
                    }
                }
                TaskEvent::Completed { id } => {
                    if let Some(t) = by_id.get_mut(&id) {
                        t.done = true;
                    }
                }
            }
        }
        order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
    }
}

/// Every team on this machine.
pub async fn list_teams() -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(paths::teams_dir()).await else {
        return vec![];
    };
    let mut names = vec![];
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own directory. No environment variable is touched,
    /// so these run in parallel without a lock.
    async fn team_in(tag: &str) -> Team {
        let dir = std::env::temp_dir().join(format!("jod-team-test-{tag}"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        Team::in_dir("crew", dir)
    }

    #[tokio::test]
    async fn a_member_joins_and_is_listed_once() {
        let team = team_in("join").await;
        team.join("lead", HarnessKind::ClaudeCode, "coordinator")
            .await
            .unwrap();
        team.join("scout", HarnessKind::Antigravity, "research")
            .await
            .unwrap();

        let members = team.members().await;
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "lead");
        assert_eq!(members[1].harness, HarnessKind::Antigravity);
        assert_eq!(members[0].status, MemberStatus::Ready);
    }

    /// The capability the whole design exists for: one team, three harnesses.
    #[tokio::test]
    async fn one_team_can_span_every_harness() {
        let team = team_in("cross").await;
        for (name, harness) in [
            ("lead", HarnessKind::ClaudeCode),
            ("builder", HarnessKind::OpenCode),
            ("scout", HarnessKind::Antigravity),
        ] {
            team.join(name, harness, "r").await.unwrap();
        }
        let kinds: Vec<HarnessKind> = team.members().await.iter().map(|m| m.harness).collect();
        assert_eq!(kinds.len(), HarnessKind::ALL.len());
        for kind in HarnessKind::ALL {
            assert!(kinds.contains(&kind), "{kind:?} must be able to join a team");
        }
    }

    #[tokio::test]
    async fn status_and_binding_fold_onto_the_member() {
        let team = team_in("fold").await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        team.set_status("scout", MemberStatus::Busy).await.unwrap();
        team.bind("scout", Some("agent-1".into()), Some("sess-1".into()))
            .await
            .unwrap();

        let m = team.member("scout").await.unwrap();
        assert_eq!(m.status, MemberStatus::Busy);
        assert_eq!(m.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(m.session_id.as_deref(), Some("sess-1"));
    }

    /// A later turn reports no session id; that must not erase the one we have,
    /// or the member would silently lose its context on the next resume.
    #[tokio::test]
    async fn rebinding_without_a_session_keeps_the_known_one() {
        let team = team_in("rebind").await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        team.bind("scout", Some("a1".into()), Some("sess-1".into()))
            .await
            .unwrap();
        team.bind("scout", Some("a2".into()), None).await.unwrap();

        let m = team.member("scout").await.unwrap();
        assert_eq!(m.agent_id.as_deref(), Some("a2"));
        assert_eq!(m.session_id.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn a_direct_message_reaches_only_its_recipient() {
        let team = team_in("dm").await;
        team.join("lead", HarnessKind::ClaudeCode, "r").await.unwrap();
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();

        let sent = team
            .send(&Message {
                from: "lead".into(),
                to: Some("scout".into()),
                text: "look at the parser".into(),
                at_ms: 1,
            })
            .await
            .unwrap();

        assert_eq!(sent, vec!["scout".to_string()]);
        assert_eq!(team.inbox("scout").await.len(), 1);
        assert!(team.inbox("lead").await.is_empty());
    }

    #[tokio::test]
    async fn a_broadcast_reaches_everyone_but_the_sender() {
        let team = team_in("broadcast").await;
        for name in ["lead", "a", "b"] {
            team.join(name, HarnessKind::ClaudeCode, "r").await.unwrap();
        }
        let sent = team
            .send(&Message { from: "lead".into(), to: None, text: "ship it".into(), at_ms: 1 })
            .await
            .unwrap();

        assert_eq!(sent.len(), 2);
        assert!(team.inbox("lead").await.is_empty());
        assert_eq!(team.inbox("a").await.len(), 1);
        assert_eq!(team.inbox("b").await.len(), 1);
    }

    /// Draining twice must not replay a message, or a teammate would act on the
    /// same instruction on every turn.
    #[tokio::test]
    async fn draining_yields_each_message_exactly_once() {
        let team = team_in("drain").await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        for i in 0..3 {
            team.send(&Message {
                from: "lead".into(),
                to: Some("scout".into()),
                text: format!("m{i}"),
                at_ms: i,
            })
            .await
            .unwrap();
        }

        let first = team.drain("scout").await.unwrap();
        assert_eq!(first.len(), 3);
        assert!(team.drain("scout").await.unwrap().is_empty());

        team.send(&Message {
            from: "lead".into(),
            to: Some("scout".into()),
            text: "m3".into(),
            at_ms: 9,
        })
        .await
        .unwrap();

        let second = team.drain("scout").await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "m3");
    }

    #[tokio::test]
    async fn a_delivered_message_is_labelled_with_its_sender() {
        let m = Message {
            from: "lead".into(),
            to: None,
            text: "status?".into(),
            at_ms: 0,
        };
        assert_eq!(m.as_prompt(), "[message from lead]\nstatus?");
    }

    #[tokio::test]
    async fn the_first_claim_wins_and_the_loser_is_told() {
        let team = team_in("claim").await;
        team.add_task("t1", "port the parser").await.unwrap();

        assert!(team.claim_task("t1", "a").await.unwrap());
        assert!(!team.claim_task("t1", "b").await.unwrap());

        let tasks = team.tasks().await;
        assert_eq!(tasks[0].claimed_by.as_deref(), Some("a"));
    }

    /// The race the append-only log exists to survive: many members claiming
    /// the same task at once must produce exactly one winner.
    #[tokio::test]
    async fn concurrent_claims_produce_exactly_one_winner() {
        let team = team_in("race").await;
        team.add_task("t1", "the only job").await.unwrap();

        let mut handles = vec![];
        for i in 0..8 {
            let team = team.clone();
            handles.push(tokio::spawn(async move {
                team.claim_task("t1", &format!("m{i}")).await.unwrap()
            }));
        }
        let mut winners = 0;
        for h in handles {
            if h.await.unwrap() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "exactly one member may own a task");
        assert!(team.tasks().await[0].claimed_by.is_some());
    }

    #[tokio::test]
    async fn tasks_track_completion() {
        let team = team_in("done").await;
        team.add_task("t1", "a").await.unwrap();
        team.add_task("t2", "b").await.unwrap();
        team.claim_task("t1", "m").await.unwrap();
        team.complete_task("t1").await.unwrap();

        let tasks = team.tasks().await;
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].done);
        assert!(!tasks[1].done);
    }

    /// Storage follows the same rule as the event stream: one bad line is
    /// skipped, never fatal.
    #[tokio::test]
    async fn a_corrupt_line_does_not_take_the_log_down() {
        let team = team_in("corrupt").await;
        team.add_task("t1", "real").await.unwrap();
        let path = team.tasks_path();
        let mut body = tokio::fs::read_to_string(&path).await.unwrap();
        body.push_str("{not json\n");
        tokio::fs::write(&path, body).await.unwrap();
        team.add_task("t2", "also real").await.unwrap();

        let tasks = team.tasks().await;
        assert_eq!(tasks.len(), 2);
    }

    /// The constructor production actually uses — the tests all use `in_dir`,
    /// so without this the real path resolution is never exercised.
    #[tokio::test]
    async fn a_team_named_alone_lands_under_jod_home() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_HOME", "/tmp/jod-team-home");
        let team = Team::new("crew");
        assert_eq!(team.name, "crew");
        assert!(team.dir().starts_with("/tmp/jod-team-home"));
        assert!(team.members_path().starts_with(team.dir()));
        std::env::remove_var("JOD_HOME");
    }

    #[tokio::test]
    async fn teams_on_disk_can_be_listed() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join("jod-list-teams-home");
        let _ = tokio::fs::remove_dir_all(&home).await;
        std::env::set_var("JOD_HOME", &home);

        assert!(list_teams().await.is_empty(), "no teams yet");
        for name in ["beta", "alpha"] {
            Team::new(name)
                .join("m", HarnessKind::ClaudeCode, "r")
                .await
                .unwrap();
        }
        assert_eq!(list_teams().await, vec!["alpha", "beta"], "sorted");

        std::env::remove_var("JOD_HOME");
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    /// `unread` must not advance the cursor — only `drain` does.
    #[tokio::test]
    async fn unread_peeks_without_consuming() {
        let team = team_in("unread").await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        team.send(&Message {
            from: "lead".into(),
            to: Some("scout".into()),
            text: "peek".into(),
            at_ms: 1,
        })
        .await
        .unwrap();

        assert_eq!(team.unread("scout").await.len(), 1);
        assert_eq!(team.unread("scout").await.len(), 1, "peeking twice sees it twice");
        team.drain("scout").await.unwrap();
        assert!(team.unread("scout").await.is_empty(), "draining consumed it");
    }

    #[tokio::test]
    async fn marking_read_moves_the_cursor_explicitly() {
        let team = team_in("cursor").await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        for i in 0..3 {
            team.send(&Message {
                from: "lead".into(),
                to: Some("scout".into()),
                text: format!("m{i}"),
                at_ms: i,
            })
            .await
            .unwrap();
        }
        team.mark_read("scout", 2).await.unwrap();
        let rest = team.unread("scout").await;
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].text, "m2");
    }

    /// The fold, not just the API: two `Claimed` records for one task must
    /// resolve to the first writer, however they got there.
    #[tokio::test]
    async fn the_fold_gives_a_contested_task_to_the_first_claimer() {
        let team = team_in("fold-claim").await;
        team.add_task("t1", "contested").await.unwrap();
        team.append(
            team.tasks_path(),
            &TaskEvent::Claimed { id: "t1".into(), by: "first".into() },
        )
        .await
        .unwrap();
        team.append(
            team.tasks_path(),
            &TaskEvent::Claimed { id: "t1".into(), by: "second".into() },
        )
        .await
        .unwrap();

        assert_eq!(team.tasks().await[0].claimed_by.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn claiming_or_completing_an_unknown_task_is_harmless() {
        let team = team_in("ghost-task").await;
        assert!(!team.claim_task("nope", "m").await.unwrap());
        team.complete_task("nope").await.unwrap();
        assert!(team.tasks().await.is_empty());
    }

    #[tokio::test]
    async fn adding_the_same_task_twice_keeps_one_entry() {
        let team = team_in("dupe-task").await;
        team.add_task("t1", "first title").await.unwrap();
        team.add_task("t1", "second title").await.unwrap();
        let tasks = team.tasks().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "first title");
    }

    #[tokio::test]
    async fn status_for_an_unknown_member_is_ignored() {
        let team = team_in("ghost-member").await;
        team.set_status("nobody", MemberStatus::Busy).await.unwrap();
        team.bind("nobody", Some("a".into()), None).await.unwrap();
        assert!(team.members().await.is_empty());
    }

    #[tokio::test]
    async fn rejoining_replaces_rather_than_duplicating() {
        let team = team_in("rejoin").await;
        team.join("scout", HarnessKind::OpenCode, "old").await.unwrap();
        team.join("scout", HarnessKind::Antigravity, "new").await.unwrap();
        let members = team.members().await;
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].role, "new");
        assert_eq!(members[0].harness, HarnessKind::Antigravity);
    }

    #[tokio::test]
    async fn every_member_status_survives_a_round_trip() {
        let team = team_in("statuses").await;
        team.join("m", HarnessKind::ClaudeCode, "r").await.unwrap();
        for status in [
            MemberStatus::Ready,
            MemberStatus::Busy,
            MemberStatus::ShutdownRequested,
            MemberStatus::Shutdown,
            MemberStatus::Error,
        ] {
            team.set_status("m", status).await.unwrap();
            assert_eq!(team.member("m").await.unwrap().status, status);
        }
    }

    #[tokio::test]
    async fn an_empty_team_answers_rather_than_failing() {
        let team = team_in("empty").await;
        assert!(team.members().await.is_empty());
        assert!(team.tasks().await.is_empty());
        assert!(team.inbox("nobody").await.is_empty());
        assert!(team.drain("nobody").await.unwrap().is_empty());
    }
}
