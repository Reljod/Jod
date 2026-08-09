//! `Jod` — the orchestrator facade.
//!
//! Jod never does the work. It launches harnesses, watches them, remembers what
//! they did, and answers questions about them. Every client (the `jod` command
//! today, an HTTP API and a phone later) drives this same struct, which is why
//! it knows nothing about terminals, sockets or HTTP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::error::{JodError, Result};
use crate::event::{AgentEnvelope, AgentEvent, Usage};
use crate::harness::{HarnessKind, PermissionPolicy, SpawnRequest};
use crate::store::{Store, StoredRun};
use crate::{paths, runner, tmux};

/// The persisted view of one agent. The whole summary is kept verbatim so
/// adding a field to `AgentSummary` never needs a schema migration.
fn stored_run(s: &AgentSummary) -> StoredRun {
    StoredRun {
        id: s.id.clone(),
        name: s.name.clone(),
        harness: s.harness.id().to_string(),
        status: serde_json::to_value(s.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "running".into()),
        cwd: s.cwd.clone(),
        session_id: s.session_id.clone(),
        tmux_session: s.tmux_session.clone(),
        created_at_ms: s.created_at_ms,
        summary: serde_json::to_value(s).unwrap_or(serde_json::Value::Null),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

/// Whether a harness can actually be used on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub id: String,
    pub label: String,
    pub available: bool,
    pub path: Option<String>,
}

/// The client-facing view of one agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: String,
    pub name: String,
    pub harness: HarnessKind,
    pub harness_label: String,
    pub status: AgentStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub permission: PermissionPolicy,
    pub tmux_session: String,
    pub attach_command: String,
    /// What to run from inside an existing tmux session, where `attach` refuses.
    pub switch_command: String,
    /// Agent sessions outlive the agent, so "is the run over" and "is the
    /// session gone" are different questions. This answers the second.
    pub session_closed: bool,
    pub created_at_ms: i64,
    pub session_id: Option<String>,
    pub usage: Usage,
    pub event_count: usize,
    /// Last assistant message, for a one-line status in a list view.
    pub last_message: Option<String>,
    pub stream_path: String,
}

struct AgentRecord {
    summary: AgentSummary,
    events: Vec<AgentEnvelope>,
}

#[derive(Default)]
struct State {
    agents: HashMap<String, AgentRecord>,
    order: Vec<String>,
}

pub struct Jod {
    state: Arc<RwLock<State>>,
    events_tx: mpsc::UnboundedSender<AgentEnvelope>,
    broadcast_tx: broadcast::Sender<AgentEnvelope>,
    store: Option<Arc<Store>>,
}

impl Jod {
    /// Build the service with no durable state. Everything is forgotten when
    /// the process exits — fine for a one-shot command, not for a daemon.
    ///
    /// Must be called from inside a Tokio runtime.
    pub fn new() -> Arc<Self> {
        Jod::build(None)
    }

    /// Build the service backed by `~/.jod/jod.db`, so runs, their transcripts
    /// and everything Jod has learned outlive the process.
    pub fn persistent() -> Result<Arc<Self>> {
        Ok(Jod::build(Some(Arc::new(Store::open(&paths::db_path())?))))
    }

    pub fn with_store(store: Arc<Store>) -> Arc<Self> {
        Jod::build(Some(store))
    }

    fn build(store: Option<Arc<Store>>) -> Arc<Self> {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<AgentEnvelope>();
        let (broadcast_tx, _) = broadcast::channel(1024);
        let jod = Arc::new(Self {
            state: Arc::new(RwLock::new(State::default())),
            events_tx,
            broadcast_tx: broadcast_tx.clone(),
            store: store.clone(),
        });

        let state = jod.state.clone();
        tokio::spawn(async move {
            while let Some(envelope) = events_rx.recv().await {
                let mut updated = None;
                {
                    let mut guard = state.write().await;
                    if let Some(record) = guard.agents.get_mut(&envelope.agent_id) {
                        apply(record, &envelope);
                        updated = Some(record.summary.clone());
                    }
                }
                if let Some(store) = &store {
                    // Persistence must never take the run down with it: a
                    // failed write is reported, and the agent keeps going.
                    if let Err(e) = store.append_event(&envelope) {
                        eprintln!("[jod] could not persist event: {e}");
                    }
                    if let Some(summary) = &updated {
                        if let Err(e) = store.save_run(&stored_run(summary)) {
                            eprintln!("[jod] could not persist run: {e}");
                        }
                    }
                }
                // A closed broadcast channel just means no client is attached.
                let _ = broadcast_tx.send(envelope);
            }
        });

        jod
    }

    /// The durable store, when this service has one.
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.store.as_ref()
    }

    /// Runs from previous processes as well as this one, newest first.
    pub fn history(&self, limit: usize) -> Result<Vec<StoredRun>> {
        match &self.store {
            Some(store) => store.runs(limit),
            None => Ok(vec![]),
        }
    }

    /// Load prior runs from the database back into memory. Returns how many.
    ///
    /// A daemon that restarts has no idea what it launched before; without this
    /// every earlier agent vanishes from `agents()` even though its tmux session
    /// may still be running. Call it once at boot.
    ///
    /// Each run's status is recomputed by replaying its stored events rather
    /// than trusting the last status written — a process killed mid-run never
    /// got to record how it ended. A run still marked running whose tmux
    /// session is gone did not report a result, and is reported as failed
    /// rather than left running forever.
    pub async fn rehydrate(&self, limit: usize) -> Result<usize> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let stored = store.runs(limit)?;
        let mut loaded = 0;

        // Oldest first, so `order` ends up in the same sequence a live process
        // would have produced.
        for run in stored.into_iter().rev() {
            let Ok(summary) = serde_json::from_value::<AgentSummary>(run.summary.clone()) else {
                // A summary written by an older, incompatible build. Skipping it
                // loses one row; failing here would lose the whole history.
                continue;
            };
            let mut record = AgentRecord {
                summary,
                events: Vec::new(),
            };
            for envelope in store.events(&run.id)? {
                apply(&mut record, &envelope);
            }

            let alive = tmux::has_session(&record.summary.tmux_session).await;
            record.summary.session_closed = !alive;
            if record.summary.status == AgentStatus::Running && !alive {
                record.summary.status = AgentStatus::Failed;
            }

            let mut guard = self.state.write().await;
            if guard.agents.contains_key(&run.id) {
                continue; // this process already owns a live copy
            }
            guard.order.push(run.id.clone());
            guard.agents.insert(run.id.clone(), record);
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Events after `after_seq` for one agent, oldest first.
    ///
    /// Serves a reconnecting client the tail it missed rather than the whole
    /// transcript. Falls back to the database when this process did not launch
    /// the agent itself.
    pub async fn events_since(&self, id: &str, after_seq: u64) -> Result<Vec<AgentEnvelope>> {
        let guard = self.state.read().await;
        if let Some(record) = guard.agents.get(id) {
            return Ok(record
                .events
                .iter()
                .filter(|e| e.seq > after_seq)
                .cloned()
                .collect());
        }
        drop(guard);
        match &self.store {
            Some(store) => store.events_since(id, after_seq, 10_000),
            None => Err(JodError::UnknownAgent(id.to_string())),
        }
    }

    /// Live event feed. Late subscribers should call `events` first to backfill.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEnvelope> {
        self.broadcast_tx.subscribe()
    }

    /// Which harnesses are installed, and where.
    pub fn harnesses(&self) -> Vec<HarnessInfo> {
        HarnessKind::ALL
            .iter()
            .map(|kind| {
                let path = kind.locate();
                HarnessInfo {
                    id: kind.id().to_string(),
                    label: kind.label().to_string(),
                    available: path.is_some(),
                    path: path.map(|p| p.to_string_lossy().to_string()),
                }
            })
            .collect()
    }

    /// tmux is a hard requirement — without it there is nothing to observe.
    pub fn tmux_available(&self) -> bool {
        tmux::locate().is_some()
    }

    /// Launch an agent. Returns once its tmux session exists.
    pub async fn spawn_agent(&self, req: SpawnRequest) -> Result<AgentSummary> {
        let program = req
            .harness
            .locate()
            .ok_or_else(|| JodError::HarnessNotFound(req.harness.label().to_string()))?;

        let id = uuid::Uuid::new_v4().to_string();
        let session = tmux::session_name(&id);
        let summary = AgentSummary {
            id: id.clone(),
            name: req.name.clone(),
            harness: req.harness,
            harness_label: req.harness.label().to_string(),
            status: AgentStatus::Running,
            cwd: req.cwd.to_string_lossy().to_string(),
            model: req.model.clone(),
            permission: req.permission,
            tmux_session: session.clone(),
            attach_command: tmux::attach_command(&session),
            switch_command: tmux::switch_command(&session),
            session_closed: false,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
            stream_path: paths::stream_path(&id).to_string_lossy().to_string(),
        };

        // Register before launching, so no event can arrive before its agent.
        {
            let mut guard = self.state.write().await;
            guard.order.push(id.clone());
            guard.agents.insert(
                id.clone(),
                AgentRecord {
                    summary: summary.clone(),
                    events: Vec::new(),
                },
            );
        }

        // Record the run before it starts, so a crash mid-launch still leaves a
        // trace of what was attempted.
        if let Some(store) = &self.store {
            if let Err(e) = store.save_run(&stored_run(&summary)) {
                eprintln!("[jod] could not persist run: {e}");
            }
        }

        let launch = runner::launch(
            &id,
            &req,
            &program,
            req.harness.build(),
            self.events_tx.clone(),
        )
        .await;

        if let Err(e) = launch {
            let mut guard = self.state.write().await;
            if let Some(record) = guard.agents.get_mut(&id) {
                record.summary.status = AgentStatus::Failed;
            }
            return Err(e);
        }

        // Persist metadata so a run remains inspectable after the app closes.
        let meta = paths::meta_path(&id);
        if let Ok(json) = serde_json::to_vec_pretty(&summary) {
            let _ = tokio::fs::write(&meta, json).await;
        }

        Ok(summary)
    }

    pub async fn agents(&self) -> Vec<AgentSummary> {
        let guard = self.state.read().await;
        guard
            .order
            .iter()
            .filter_map(|id| guard.agents.get(id))
            .map(|r| r.summary.clone())
            .collect()
    }

    pub async fn agent(&self, id: &str) -> Result<AgentSummary> {
        let guard = self.state.read().await;
        guard
            .agents
            .get(id)
            .map(|r| r.summary.clone())
            .ok_or_else(|| JodError::UnknownAgent(id.to_string()))
    }

    /// Full event history, for backfilling a client that just connected.
    pub async fn events(&self, id: &str) -> Result<Vec<AgentEnvelope>> {
        let guard = self.state.read().await;
        guard
            .agents
            .get(id)
            .map(|r| r.events.clone())
            .ok_or_else(|| JodError::UnknownAgent(id.to_string()))
    }

    /// Close an agent's tmux session.
    ///
    /// While the agent is still running this stops it, and the tailer notices
    /// and finalises the run. After it has finished this just reclaims the
    /// session, which outlives the agent so that watching one can never close
    /// the watcher's terminal.
    pub async fn kill_agent(&self, id: &str) -> Result<()> {
        let session = {
            let guard = self.state.read().await;
            guard
                .agents
                .get(id)
                .map(|r| r.summary.tmux_session.clone())
                .ok_or_else(|| JodError::UnknownAgent(id.to_string()))?
        };
        tmux::kill_session(&session).await?;
        let mut guard = self.state.write().await;
        if let Some(record) = guard.agents.get_mut(id) {
            record.summary.session_closed = true;
            if record.summary.status == AgentStatus::Running {
                record.summary.status = AgentStatus::Killed;
            }
        }
        Ok(())
    }

    /// A short digest of everything in flight — what Jod reports back to Reljod.
    pub async fn report(&self) -> Report {
        let agents = self.agents().await;
        Report {
            running: agents
                .iter()
                .filter(|a| a.status == AgentStatus::Running)
                .count(),
            completed: agents
                .iter()
                .filter(|a| a.status == AgentStatus::Completed)
                .count(),
            failed: agents
                .iter()
                .filter(|a| a.status == AgentStatus::Failed)
                .count(),
            killed: agents
                .iter()
                .filter(|a| a.status == AgentStatus::Killed)
                .count(),
            total_cost_usd: agents.iter().filter_map(|a| a.usage.cost_usd).sum(),
            agents,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub killed: usize,
    pub total_cost_usd: f64,
    pub agents: Vec<AgentSummary>,
}

/// Fold one event into an agent's stored state.
fn apply(record: &mut AgentRecord, envelope: &AgentEnvelope) {
    match &envelope.event {
        AgentEvent::Started { session_id, model } => {
            if record.summary.session_id.is_none() {
                record.summary.session_id.clone_from(session_id);
            }
            // The harness reports the model it actually used, which may differ
            // from what was requested (aliases, config defaults).
            if let Some(model) = model {
                record.summary.model = Some(model.clone());
            }
        }
        AgentEvent::Message { text } => {
            record.summary.last_message = Some(text.clone());
        }
        AgentEvent::Finished {
            is_error,
            usage,
            text,
            ..
        } => {
            // A kill already recorded the truthful cause; don't overwrite it.
            if record.summary.status == AgentStatus::Running {
                record.summary.status = if *is_error {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Completed
                };
            }
            if !usage.is_empty() {
                record.summary.usage = usage.clone();
            }
            if let Some(text) = text {
                record.summary.last_message = Some(text.clone());
            }
        }
        AgentEvent::Error { message } => {
            record.summary.last_message = Some(message.clone());
        }
        _ => {}
    }
    record.events.push(envelope.clone());
    record.summary.event_count = record.events.len();
}

/// Convenience: a request rooted at the user's home directory.
pub fn default_cwd() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> AgentRecord {
        AgentRecord {
            summary: AgentSummary {
                id: "a".into(),
                name: "n".into(),
                harness: HarnessKind::ClaudeCode,
                harness_label: "Claude Code".into(),
                status: AgentStatus::Running,
                cwd: "/tmp".into(),
                model: None,
                permission: PermissionPolicy::Ask,
                tmux_session: "jod-a".into(),
                attach_command: "tmux attach -t jod-a".into(),
                switch_command: "tmux switch-client -t jod-a".into(),
                session_closed: false,
                created_at_ms: 0,
                session_id: None,
                usage: Usage::default(),
                event_count: 0,
                last_message: None,
                stream_path: "/tmp/s".into(),
            },
            events: vec![],
        }
    }

    fn env(event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: "a".into(),
            at_ms: 0,
            seq: 0,
            event,
        }
    }

    #[test]
    fn started_records_the_session_and_the_model_actually_used() {
        let mut r = record();
        apply(
            &mut r,
            &env(AgentEvent::Started {
                session_id: Some("s1".into()),
                model: Some("claude-haiku-4-5".into()),
            }),
        );
        assert_eq!(r.summary.session_id.as_deref(), Some("s1"));
        assert_eq!(r.summary.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(r.summary.event_count, 1);
    }

    #[test]
    fn a_clean_finish_marks_the_agent_completed() {
        let mut r = record();
        apply(
            &mut r,
            &env(AgentEvent::Finished {
                text: Some("done".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage {
                    cost_usd: Some(0.01),
                    ..Default::default()
                },
            }),
        );
        assert_eq!(r.summary.status, AgentStatus::Completed);
        assert_eq!(r.summary.usage.cost_usd, Some(0.01));
        assert_eq!(r.summary.last_message.as_deref(), Some("done"));
    }

    #[test]
    fn an_errored_finish_marks_the_agent_failed() {
        let mut r = record();
        apply(
            &mut r,
            &env(AgentEvent::Finished {
                text: None,
                exit_code: Some(1),
                is_error: true,
                usage: Usage::default(),
            }),
        );
        assert_eq!(r.summary.status, AgentStatus::Failed);
    }

    #[test]
    fn a_finished_agent_still_has_a_session_to_reclaim() {
        let mut r = record();
        apply(
            &mut r,
            &env(AgentEvent::Finished {
                text: None,
                exit_code: Some(0),
                is_error: false,
                usage: Usage::default(),
            }),
        );
        assert_eq!(r.summary.status, AgentStatus::Completed);
        assert!(
            !r.summary.session_closed,
            "the tmux session outlives the agent, so it is still closeable"
        );
    }

    #[test]
    fn finishing_after_a_kill_keeps_the_killed_status() {
        let mut r = record();
        r.summary.status = AgentStatus::Killed;
        apply(
            &mut r,
            &env(AgentEvent::Finished {
                text: None,
                exit_code: None,
                is_error: true,
                usage: Usage::default(),
            }),
        );
        assert_eq!(r.summary.status, AgentStatus::Killed);
    }

    #[test]
    fn every_event_is_retained_for_replay() {
        let mut r = record();
        apply(&mut r, &env(AgentEvent::Thinking { text: "t".into() }));
        apply(&mut r, &env(AgentEvent::Message { text: "m".into() }));
        assert_eq!(r.events.len(), 2);
        assert_eq!(r.summary.last_message.as_deref(), Some("m"));
    }

    #[tokio::test]
    async fn a_fresh_service_has_no_agents_and_an_empty_report() {
        let jod = Jod::new();
        assert!(jod.agents().await.is_empty());
        let report = jod.report().await;
        assert_eq!(report.running, 0);
        assert_eq!(report.total_cost_usd, 0.0);
    }

    /// Build a store holding one finished run, as a previous process would
    /// have left behind.
    fn store_with_one_finished_run() -> std::sync::Arc<Store> {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "past".into();
        summary.name = "yesterday's work".into();
        summary.tmux_session = "jod-past-session-that-does-not-exist".into();
        store.save_run(&stored_run(&summary)).unwrap();
        store
            .append_event(&AgentEnvelope {
                agent_id: "past".into(),
                at_ms: 1,
                seq: 0,
                event: AgentEvent::Message {
                    text: "hello".into(),
                },
            })
            .unwrap();
        store
            .append_event(&AgentEnvelope {
                agent_id: "past".into(),
                at_ms: 2,
                seq: 1,
                event: AgentEvent::Finished {
                    text: Some("all done".into()),
                    exit_code: Some(0),
                    is_error: false,
                    usage: Usage::default(),
                },
            })
            .unwrap();
        store
    }

    #[tokio::test]
    async fn a_restarted_service_sees_nothing_until_it_rehydrates() {
        let jod = Jod::with_store(store_with_one_finished_run());
        assert!(jod.agents().await.is_empty());
        assert_eq!(jod.rehydrate(100).await.unwrap(), 1);
        let agents = jod.agents().await;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "past");
    }

    /// The status is recomputed from the events, so a run that finished after
    /// the last status write is still reported as finished.
    #[tokio::test]
    async fn rehydrating_replays_events_to_recover_the_real_outcome() {
        let jod = Jod::with_store(store_with_one_finished_run());
        jod.rehydrate(100).await.unwrap();
        let agent = jod.agent("past").await.unwrap();
        assert_eq!(agent.status, AgentStatus::Completed);
        assert_eq!(agent.last_message.as_deref(), Some("all done"));
        assert_eq!(agent.event_count, 2);
        assert!(agent.session_closed, "its tmux session is long gone");
    }

    /// A process killed mid-run never records how it ended. Leaving such a run
    /// "running" forever would make the report permanently wrong.
    #[tokio::test]
    async fn a_run_still_marked_running_with_no_session_is_reported_failed() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "orphan".into();
        summary.status = AgentStatus::Running;
        summary.tmux_session = "jod-orphan-no-such-session".into();
        store.save_run(&stored_run(&summary)).unwrap();

        let jod = Jod::with_store(store);
        jod.rehydrate(100).await.unwrap();
        assert_eq!(
            jod.agent("orphan").await.unwrap().status,
            AgentStatus::Failed
        );
    }

    #[tokio::test]
    async fn rehydrating_twice_does_not_duplicate_agents() {
        let jod = Jod::with_store(store_with_one_finished_run());
        assert_eq!(jod.rehydrate(100).await.unwrap(), 1);
        assert_eq!(jod.rehydrate(100).await.unwrap(), 0, "already loaded");
        assert_eq!(jod.agents().await.len(), 1);
    }

    #[tokio::test]
    async fn rehydrating_without_a_store_is_a_no_op_not_an_error() {
        assert_eq!(Jod::new().rehydrate(100).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_reconnecting_client_is_served_only_the_events_it_missed() {
        let jod = Jod::with_store(store_with_one_finished_run());
        jod.rehydrate(100).await.unwrap();
        let tail = jod.events_since("past", 0).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 1);
    }

    /// An agent this process never launched still has a transcript on disk.
    #[tokio::test]
    async fn the_tail_of_an_unknown_agent_comes_from_the_database() {
        let jod = Jod::with_store(store_with_one_finished_run());
        let tail = jod.events_since("past", 0).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 1);
    }

    #[tokio::test]
    async fn asking_for_the_tail_of_a_truly_unknown_agent_without_a_store_errors() {
        assert!(Jod::new().events_since("nope", 0).await.is_err());
    }

    #[tokio::test]
    async fn asking_about_an_unknown_agent_is_an_error_not_a_panic() {
        let jod = Jod::new();
        assert!(jod.agent("nope").await.is_err());
        assert!(jod.events("nope").await.is_err());
        assert!(jod.kill_agent("nope").await.is_err());
    }

    #[tokio::test]
    async fn harness_discovery_reports_every_known_harness() {
        let jod = Jod::new();
        let hs = jod.harnesses();
        assert_eq!(hs.len(), HarnessKind::ALL.len());
        assert!(hs.iter().any(|h| h.id == "claude_code"));
        assert!(hs.iter().any(|h| h.id == "open_code"));
    }

    #[tokio::test]
    async fn spawning_an_unavailable_harness_fails_cleanly() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("JOD_CLAUDE_BIN", "/definitely/not/a/binary");
        // Point PATH and HOME somewhere empty so discovery cannot find a real
        // claude via PATH or via the ~/.nvm and ~/.claude well-known paths.
        let saved_path = std::env::var("PATH").unwrap_or_default();
        let saved_home = std::env::var("HOME").unwrap_or_default();
        std::env::set_var("PATH", "/definitely/not/a/dir");
        std::env::set_var("HOME", "/definitely/not/a/home");

        let jod = Jod::new();
        let result = jod
            .spawn_agent(SpawnRequest {
                name: "x".into(),
                harness: HarnessKind::ClaudeCode,
                prompt: "hi".into(),
                cwd: PathBuf::from("/tmp"),
                model: None,
                permission: PermissionPolicy::Ask,
                resume: crate::harness::Resume::Fresh,
            })
            .await;

        std::env::set_var("PATH", saved_path);
        std::env::set_var("HOME", saved_home);
        std::env::remove_var("JOD_CLAUDE_BIN");

        assert!(matches!(result, Err(JodError::HarnessNotFound(_))));
    }
}
