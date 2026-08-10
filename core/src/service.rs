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

use crate::conversation::NewMessage;
use crate::error::{JodError, Result};
use crate::event::{AgentEnvelope, AgentEvent, Usage};
use crate::harness::{HarnessKind, PermissionPolicy, SpawnRequest};
use crate::store::{Store, StoredRun};
use crate::{paths, proc, runner};

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
        pid: s.pid,
        pgid: s.pgid,
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

impl AgentStatus {
    /// The spelling stored in `runs.status`.
    ///
    /// The supervisor is a separate process that writes this column directly,
    /// so the two sides need one definition of the word rather than two string
    /// literals that can drift apart.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Running => "running",
            AgentStatus::Completed => "completed",
            AgentStatus::Failed => "failed",
            AgentStatus::Killed => "killed",
        }
    }

    pub fn parse(s: &str) -> Option<AgentStatus> {
        [
            AgentStatus::Running,
            AgentStatus::Completed,
            AgentStatus::Failed,
            AgentStatus::Killed,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
    }
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
    /// The supervising `jod-run` process, and the group holding both it and the
    /// harness. `None` before the launch; kept afterwards, so a finished run
    /// still says what ran it.
    ///
    /// Defaulted on deserialise so a summary written by an older build — one
    /// that recorded a tmux session instead — still loads. Losing a whole run's
    /// history to a renamed field would be a worse trade than a missing pid.
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub pgid: Option<u32>,
    /// Whether the run's process group still exists. Recomputed on read rather
    /// than stored, because a process can die without telling anyone.
    #[serde(default)]
    pub process_alive: bool,
    /// What a human runs to watch this agent. `jod watch` reads the same rows
    /// every other client does, so it works from anywhere the database does —
    /// which `tmux attach` never did.
    #[serde(default)]
    pub watch_command: String,
    pub created_at_ms: i64,
    pub session_id: Option<String>,
    pub usage: Usage,
    pub event_count: usize,
    /// Last assistant message, for a one-line status in a list view.
    pub last_message: Option<String>,
}

/// What a human types to follow this run.
pub fn watch_command(agent_id: &str) -> String {
    format!("jod watch {agent_id}")
}

/// Which conversation a run's turns are recorded in.
///
/// Stated by the caller, because only the caller knows whether this run
/// continues something. Jod cannot infer it: two runs in the same directory on
/// the same harness may be one thread or two, and guessing wrong welds
/// unrelated work into a single transcript that no fork can unpick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RunConversation {
    /// Mint a conversation for this run. The default, because a run nobody
    /// placed is still a thing that was said and answered.
    #[default]
    New,
    /// Extend one that already exists, appending at its head.
    Existing(String),
    /// Record nothing in the graph. For machinery whose prompt Jod generated
    /// and whose output Jod parses rather than reads — `jod consolidate` above
    /// all, whose prompt *is* a transcript and would otherwise be stored, and
    /// indexed for search, a second time.
    Detached,
}

/// Longest title derived from a prompt. A row in a listing, not a summary —
/// the same width `conversations` truncates its fallback to.
const TITLE_CHARS: usize = 60;

/// A conversation's name, taken from the prompt that opened it.
fn title_from(prompt: &str) -> String {
    let line: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > TITLE_CHARS {
        format!("{}…", line.chars().take(TITLE_CHARS - 1).collect::<String>())
    } else {
        line
    }
}

/// Resolve the conversation a run belongs to, and open it with the prompt that
/// started it.
///
/// `None` means "record nothing", which is an ordinary ending rather than a
/// failure: a detached run, a conversation id that names nothing, or a store
/// that would not take the write. The run happens either way — see
/// [`record_in_conversation`] for why that direction is never reversed.
fn open_conversation(
    store: &Store,
    req: &SpawnRequest,
    run_id: &str,
    binding: &RunConversation,
) -> Option<String> {
    let conversation = match binding {
        RunConversation::Detached => return None,
        RunConversation::Existing(id) => match store.conversation(id) {
            Ok(Some(c)) => c,
            Ok(None) => {
                eprintln!("[jod] no conversation `{id}` — this run will not be recorded in one");
                return None;
            }
            Err(e) => {
                eprintln!("[jod] could not read conversation `{id}`: {e}");
                return None;
            }
        },
        RunConversation::New => match store.new_conversation(
            req.harness,
            &req.cwd.to_string_lossy(),
            req.model.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[jod] could not start a conversation for this run: {e}");
                return None;
            }
        },
    };

    // A conversation nobody has named takes its name from what it was first
    // asked to do. `conversations` falls back to the opening message when the
    // column is blank, so this is for everything that reads the row itself.
    if conversation.title.is_empty() {
        if let Err(e) = store.set_conversation_title(&conversation.id, &title_from(&req.prompt)) {
            eprintln!("[jod] could not name conversation {}: {e}", conversation.id);
        }
    }

    // The prompt is the conversation's user turn, and the only one there will
    // ever be: `NewMessage::from_event` produces no `User` message because no
    // harness reports its own prompt back. Without this a transcript reads as
    // an agent talking to itself, and `resume`-by-replay would hand the next
    // harness an answer to a question nobody asked.
    if let Err(e) = store.append_message(
        &conversation.id,
        NewMessage::user(req.prompt.clone()).from_run(run_id),
    ) {
        eprintln!(
            "[jod] could not record the prompt on conversation {}: {e}",
            conversation.id
        );
    }

    Some(conversation.id)
}

/// Fold one of a run's events into the conversation the run belongs to.
///
/// **Who owns this write:** any process holding a binding for the run — which
/// in practice means the one that launched it, plus any that
/// [`Jod::rehydrate`] handed a live run back to. It is deliberately *not* one
/// owner, because `runner::follow` is not exclusive: a `jod watch` in another
/// terminal and a daemon that restarted both forward the same rows out of
/// `events`, and replay from a cursor is the normal case rather than the
/// exceptional one. Sole ownership would therefore have to be enforced by
/// discipline, and discipline is not a guard.
///
/// The guard is in the write instead. [`Store::append_envelopes`] carries each
/// message's `(run_id, seq)` and the schema is unique over the pair, so a
/// second writer of the same event appends nothing. That is what makes it safe
/// for the transcript to survive the process that started it.
///
/// Nothing here returns an error. A conversation is a *side effect* of a run,
/// and the Hermes audit is unambiguous about what happens when a memory side
/// effect is allowed to fail the work it was watching: a looping write
/// suppressed the user's own reply (`research/hermes-parity-2026/REPORT.md`
/// §3.2). So every failure is logged and the event stream carries on.
fn record_in_conversation(store: &Store, conversation_id: &str, envelope: &AgentEnvelope) {
    // The session id belongs on the conversation row, not in the transcript: it
    // is how `Store::resume_for` puts the *next* run back into this thread
    // instead of replaying it from text.
    if let AgentEvent::Started {
        session_id: Some(session),
        ..
    } = &envelope.event
    {
        if let Err(e) = store.set_conversation_session(conversation_id, Some(session)) {
            eprintln!("[jod] could not record the session on {conversation_id}: {e}");
        }
    }

    // `append_envelopes` skips what is not a turn, but it opens a write
    // transaction to find that out. Ask the same function first, because `Raw`
    // lines are as frequent as the harness is chatty.
    if NewMessage::from_event(&envelope.event).is_none() {
        return;
    }
    if let Err(e) = store.append_envelopes(conversation_id, std::slice::from_ref(envelope)) {
        eprintln!("[jod] could not record a turn on {conversation_id}: {e}");
    }
}

/// How long a run gets to shut down cleanly before it is killed outright.
///
/// Long enough for the supervisor to write the run's final events, which is the
/// entire point of asking rather than killing: a supervisor that vanishes
/// leaves the run marked running with no explanation.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

struct AgentRecord {
    summary: AgentSummary,
    events: Vec<AgentEnvelope>,
}

#[derive(Default)]
struct State {
    agents: HashMap<String, AgentRecord>,
    order: Vec<String>,
    /// Run id → the conversation its turns are appended to. Only ever holds
    /// runs *this* process launched, which is the whole of the no-double-write
    /// rule — see [`record_in_conversation`].
    conversations: HashMap<String, String>,
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
                let conversation;
                {
                    let mut guard = state.write().await;
                    if let Some(record) = guard.agents.get_mut(&envelope.agent_id) {
                        apply(record, &envelope);
                        updated = Some(record.summary.clone());
                    }
                    conversation = guard.conversations.get(&envelope.agent_id).cloned();
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
                    // Ordered after the event log on purpose. `events` is the
                    // record of what happened and `messages` is a projection of
                    // it, so a crash between the two loses a projection that a
                    // later pass could rebuild, never the run itself.
                    if let Some(id) = &conversation {
                        record_in_conversation(store, id, &envelope);
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
    /// every earlier agent vanishes from `agents()` even though its supervisor
    /// may still be running. Call it once at boot.
    ///
    /// Each run's summary is rebuilt by replaying its stored events, because a
    /// summary is only as fresh as the last process that serialised one.
    ///
    /// The *status* then comes from the `runs` row, when that row records a
    /// terminal one. The supervisor is the only process that saw the harness
    /// exit, so it is the only one that can tell a clean finish from a signal —
    /// and the replay cannot: a killed run's `Finished` event looks exactly
    /// like a completed run's. Trusting the replay here reported every killed
    /// run as `completed`.
    ///
    /// A row still saying `running` is the case where nothing authoritative was
    /// ever written, and that is where the process group is probed: a run
    /// marked running with a dead group did not report a result, and becomes
    /// *failed* rather than running forever.
    ///
    /// A run that *is* still alive is picked back up: a follower starts on it,
    /// so its remaining events reach this process's clients as they arrive.
    /// That is what the file tailer could never do, because it had to be
    /// started by whoever spawned the agent.
    ///
    /// It is also given its conversation back, so the transcript keeps growing
    /// across the restart instead of stopping wherever the old process died.
    /// Safe to do for a run another process may also be following, because
    /// [`record_in_conversation`] writes through an idempotent append.
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

            // The pgid comes from the row, not from the summary: the summary is
            // whatever the launching process last serialised, while the column
            // is written by the supervisor itself.
            record.summary.pid = run.pid;
            record.summary.pgid = run.pgid;
            record.summary.watch_command = watch_command(&run.id);

            // The supervisor's word on how it ended beats anything inferred
            // from the events, which cannot distinguish a kill from a clean
            // exit. A row still saying `running` says nothing, so it does not
            // override the replay.
            match AgentStatus::parse(&run.status) {
                Some(AgentStatus::Running) | None => {}
                Some(recorded) => record.summary.status = recorded,
            }

            // Only probe a run that still claims to be running. Pids are
            // recycled, and asking about a finished run's long-dead pgid is how
            // a stranger's process gets mistaken for an agent.
            let alive = record.summary.status == AgentStatus::Running
                && run.pgid.is_some_and(proc::group_alive);
            record.summary.process_alive = alive;
            if record.summary.status == AgentStatus::Running && !alive {
                record.summary.status = AgentStatus::Failed;
            }

            // Resume the follower *after* the last event already folded in
            // above, or rehydration's own replay would be delivered a second
            // time and every event would appear twice.
            let cursor = record.events.last().map(|e| e.seq);
            let follow = alive.then(|| (run.id.clone(), run.pgid.unwrap_or(0), cursor));

            // Which conversation this run was writing into, read back out of
            // the messages it already produced. Only for a run still going: a
            // finished one has nothing left to append, and binding it would
            // keep a map entry for every run in the history.
            let conversation = match alive {
                true => store.conversation_for_run(&run.id).unwrap_or_else(|e| {
                    eprintln!("[jod] could not find the conversation for {}: {e}", run.id);
                    None
                }),
                false => None,
            };

            {
                let mut guard = self.state.write().await;
                if guard.agents.contains_key(&run.id) {
                    continue; // this process already owns a live copy
                }
                guard.order.push(run.id.clone());
                guard.agents.insert(run.id.clone(), record);
                if let Some(conversation) = conversation {
                    guard.conversations.insert(run.id.clone(), conversation);
                }
            }
            loaded += 1;

            if let Some((id, pgid, cursor)) = follow {
                tokio::spawn(runner::follow(
                    id,
                    pgid,
                    store.clone(),
                    self.events_tx.clone(),
                    cursor,
                ));
            }
        }
        Ok(loaded)
    }

    /// Events after `after` for one agent, oldest first. `None` means "I have
    /// seen nothing", and returns the run from its very first event.
    ///
    /// Serves a reconnecting client the tail it missed rather than the whole
    /// transcript. Falls back to the database when this process did not launch
    /// the agent itself, so a client can reattach to a run started by an
    /// earlier process.
    ///
    /// The cursor is an `Option` because sequences start at 0: no integer can
    /// mean "nothing yet", and taking `0` for it would silently drop the
    /// `Started` event that carries the session id and model.
    pub async fn events_since(&self, id: &str, after: Option<u64>) -> Result<Vec<AgentEnvelope>> {
        let guard = self.state.read().await;
        if let Some(record) = guard.agents.get(id) {
            return Ok(record
                .events
                .iter()
                .filter(|e| after.is_none_or(|a| e.seq > a))
                .cloned()
                .collect());
        }
        drop(guard);
        match &self.store {
            Some(store) => store.events_since(id, after, 10_000),
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

    /// Whether the `jod-run` supervisor is installed.
    ///
    /// A hard requirement, as tmux used to be, and for the same reason: without
    /// it there is nothing to hold a run's output once the caller walks away.
    pub fn supervisor_available(&self) -> bool {
        runner::locate_supervisor().is_some()
    }

    /// Launch an agent in a conversation of its own.
    ///
    /// The default binding is [`RunConversation::New`] rather than nothing,
    /// because a run *is* a turn: something was asked and something answered,
    /// and a graph that only fills up when a caller remembers to ask for it is
    /// the state this had before — `jod conv ls` describing an empty table.
    pub async fn spawn_agent(&self, req: SpawnRequest) -> Result<AgentSummary> {
        self.spawn_agent_in(req, RunConversation::New).await
    }

    /// Launch an agent, recording its turns in the conversation the caller
    /// names. Returns once its supervisor is running.
    ///
    /// Requires a store. A run reports itself by writing to the database, so a
    /// Jod without one would start an agent whose output goes nowhere — and
    /// would then have to pretend that was a success.
    ///
    /// The binding says where the *transcript* goes; `req.resume` still says
    /// what the harness is told, and the two are deliberately separate. A
    /// conversation can outlive the harness session that produced it — that is
    /// the entire reason Jod keeps its own graph — so a caller continuing a
    /// thread on a different harness passes [`RunConversation::Existing`] with
    /// a `resume` of its own choosing, or reads [`Store::resume_for`] first.
    pub async fn spawn_agent_in(
        &self,
        req: SpawnRequest,
        conversation: RunConversation,
    ) -> Result<AgentSummary> {
        let store = self.store.clone().ok_or(JodError::StoreRequired)?;
        let program = req
            .harness
            .locate()
            .ok_or_else(|| JodError::HarnessNotFound(req.harness.label().to_string()))?;

        let id = uuid::Uuid::new_v4().to_string();
        let summary = AgentSummary {
            id: id.clone(),
            name: req.name.clone(),
            harness: req.harness,
            harness_label: req.harness.label().to_string(),
            status: AgentStatus::Running,
            cwd: req.cwd.to_string_lossy().to_string(),
            model: req.model.clone(),
            permission: req.permission,
            pid: None,
            pgid: None,
            process_alive: false,
            watch_command: watch_command(&id),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
        };

        // Open the conversation before the launch for the same reason the agent
        // is registered before it: an event that arrived first would find no
        // binding and be dropped from the transcript. It costs a conversation
        // holding only a prompt if the launch then fails, which is the truthful
        // record of an attempt rather than a leak.
        let conversation = open_conversation(&store, &req, &id, &conversation);

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
            if let Some(conversation) = conversation {
                guard.conversations.insert(id.clone(), conversation);
            }
        }

        // Record the run before it starts. The supervisor updates this row from
        // its own process, so the row has to exist before it is launched —
        // and a crash mid-launch still leaves a trace of what was attempted.
        if let Err(e) = store.save_run(&stored_run(&summary)) {
            eprintln!("[jod] could not persist run: {e}");
        }

        let launch = runner::launch(
            &id,
            &req,
            &program,
            req.harness.build(),
            store.clone(),
            self.events_tx.clone(),
        )
        .await;

        let launched = match launch {
            Ok(l) => l,
            Err(e) => {
                let mut guard = self.state.write().await;
                if let Some(record) = guard.agents.get_mut(&id) {
                    record.summary.status = AgentStatus::Failed;
                }
                let _ = store.set_run_status(&id, AgentStatus::Failed.as_str());
                return Err(e);
            }
        };

        let summary = {
            let mut guard = self.state.write().await;
            let record = guard.agents.get_mut(&id).expect("just registered");
            record.summary.pid = Some(launched.pid);
            record.summary.pgid = Some(launched.pgid);
            record.summary.process_alive = true;
            record.summary.clone()
        };

        // Persist metadata so a run remains inspectable after the app closes.
        let meta = paths::meta_path(&id);
        if let Ok(json) = serde_json::to_vec_pretty(&summary) {
            let _ = tokio::fs::write(&meta, json).await;
        }

        Ok(summary)
    }

    /// The conversation a run's turns are being recorded in, if this process is
    /// recording them.
    ///
    /// How a caller holding several turns of one thread keeps them in one
    /// conversation: launch the first with [`RunConversation::New`], ask for the
    /// id, and pass [`RunConversation::Existing`] from then on. Without it every
    /// turn of a chat would mint a conversation of its own and the graph would
    /// record a hundred one-message threads instead of one.
    pub async fn conversation_of(&self, run_id: &str) -> Option<String> {
        self.state.read().await.conversations.get(run_id).cloned()
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

    /// Stop an agent, and everything it started.
    ///
    /// The signal goes to the whole process group, so a harness that spawned
    /// children does not leave them behind — the same reach `tmux kill-session`
    /// had. `SIGTERM` first, so the supervisor gets to record how the run ended
    /// rather than disappearing and leaving it marked running for ever;
    /// `SIGKILL` only for a group that ignores it.
    ///
    /// Works from any process, including one that never launched this run: the
    /// process-group id is a column, not a handle.
    pub async fn kill_agent(&self, id: &str) -> Result<()> {
        let pgid = match self.state.read().await.agents.get(id) {
            Some(record) => record.summary.pgid,
            None => return Err(JodError::UnknownAgent(id.to_string())),
        };

        if let Some(pgid) = pgid {
            proc::terminate_group(pgid, KILL_GRACE)
                .await
                .map_err(|e| JodError::Spawn(format!("could not stop process group {pgid}: {e}")))?;
        }

        let mut guard = self.state.write().await;
        if let Some(record) = guard.agents.get_mut(id) {
            record.summary.process_alive = false;
            if record.summary.status == AgentStatus::Running {
                record.summary.status = AgentStatus::Killed;
                if let Some(store) = &self.store {
                    let _ = store.set_run_status(id, AgentStatus::Killed.as_str());
                }
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
            // Rust's f64 `Sum` folds from -0.0, so a run with no reported cost
            // serialises as `-0.0`. It parses back as zero, but it reads like a
            // bug in any UI that shows it. Adding 0.0 normalises the sign and
            // leaves every real total untouched.
            total_cost_usd: agents.iter().filter_map(|a| a.usage.cost_usd).sum::<f64>() + 0.0,
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
                pid: Some(4242),
                pgid: Some(4242),
                process_alive: true,
                watch_command: watch_command("a"),
                created_at_ms: 0,
                session_id: None,
                usage: Usage::default(),
                event_count: 0,
                last_message: None,
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

    /// A finished run keeps saying which process group ran it. That is the
    /// record of what happened, and dropping it on completion would make a
    /// completed run indistinguishable from one that never launched.
    #[test]
    fn a_finished_agent_still_reports_the_group_that_ran_it() {
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
        assert_eq!(r.summary.pgid, Some(4242));
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
        // A pid from a previous boot. Nothing is listening on it now.
        summary.pid = Some(4_000_000);
        summary.pgid = Some(4_000_000);
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
        assert!(
            !agent.process_alive,
            "the process group that ran it is long gone"
        );
    }

    /// A process killed mid-run never records how it ended. Leaving such a run
    /// "running" forever would make the report permanently wrong.
    #[tokio::test]
    async fn a_run_still_marked_running_with_a_dead_group_is_reported_failed() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "orphan".into();
        summary.status = AgentStatus::Running;
        summary.pid = Some(4_000_000);
        summary.pgid = Some(4_000_000);
        store.save_run(&stored_run(&summary)).unwrap();

        let jod = Jod::with_store(store);
        jod.rehydrate(100).await.unwrap();
        assert_eq!(
            jod.agent("orphan").await.unwrap().status,
            AgentStatus::Failed
        );
    }

    /// Regression, found by killing a real detached run and listing it from a
    /// fresh process: the run came back as `completed`.
    ///
    /// A killed run's `Finished` event is indistinguishable from a clean one —
    /// no exit code, `is_error` false — because the harness was signalled
    /// rather than having failed. Only the supervisor saw the signal, and only
    /// the `runs` row carries what it saw.
    #[tokio::test]
    async fn a_killed_run_does_not_come_back_as_completed() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "killed".into();
        summary.status = AgentStatus::Running; // what the launcher last saved
        store.save_run(&stored_run(&summary)).unwrap();
        store
            .append_event(&AgentEnvelope {
                agent_id: "killed".into(),
                at_ms: 1,
                seq: 0,
                event: AgentEvent::Finished {
                    text: None,
                    exit_code: None,
                    is_error: false,
                    usage: Usage::default(),
                },
            })
            .unwrap();
        store.set_run_status("killed", "killed").unwrap();

        let jod = Jod::with_store(store);
        jod.rehydrate(100).await.unwrap();
        assert_eq!(
            jod.agent("killed").await.unwrap().status,
            AgentStatus::Killed,
            "the supervisor's word must beat the replay"
        );
    }

    /// The converse, and the reason the row is not trusted blindly: a row left
    /// saying `running` records nothing at all, so the replay still decides.
    #[tokio::test]
    async fn a_stale_running_row_does_not_override_a_finished_replay() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "raced".into();
        summary.status = AgentStatus::Running;
        store.save_run(&stored_run(&summary)).unwrap();
        store
            .append_event(&AgentEnvelope {
                agent_id: "raced".into(),
                at_ms: 1,
                seq: 0,
                event: AgentEvent::Finished {
                    text: None,
                    exit_code: Some(1),
                    is_error: true,
                    usage: Usage::default(),
                },
            })
            .unwrap();
        // `runs.status` is still "running": the supervisor died before it could
        // write the final status, but its last event did land.

        let jod = Jod::with_store(store);
        jod.rehydrate(100).await.unwrap();
        assert_eq!(
            jod.agent("raced").await.unwrap().status,
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
        let tail = jod.events_since("past", Some(0)).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 1);
    }

    /// Regression: a client with no cursor must be served the run from its
    /// first event. Reading "no cursor" as `0` dropped `seq` 0 — the `Started`
    /// event carrying the session id and model — so a run appeared to have no
    /// beginning.
    #[tokio::test]
    async fn a_client_with_no_cursor_is_served_the_start_of_the_run() {
        let jod = Jod::with_store(store_with_one_finished_run());
        jod.rehydrate(100).await.unwrap();
        let all = jod.events_since("past", None).await.unwrap();
        assert_eq!(all.len(), 2, "the whole run, including seq 0");
        assert_eq!(all[0].seq, 0);
        assert!(matches!(all[0].event, AgentEvent::Message { .. }));
    }

    /// The same must hold when the run is served from the database rather than
    /// from this process's memory.
    #[tokio::test]
    async fn a_run_this_process_never_launched_is_also_served_from_its_start() {
        let jod = Jod::with_store(store_with_one_finished_run());
        let all = jod.events_since("past", None).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 0);
    }

    /// An agent this process never launched still has a transcript on disk.
    #[tokio::test]
    async fn the_tail_of_an_unknown_agent_comes_from_the_database() {
        let jod = Jod::with_store(store_with_one_finished_run());
        let tail = jod.events_since("past", Some(0)).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 1);
    }

    #[tokio::test]
    async fn asking_for_the_tail_of_a_truly_unknown_agent_without_a_store_errors() {
        assert!(Jod::new().events_since("nope", None).await.is_err());
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

        // A store, so the request gets as far as looking for the harness.
        let jod = Jod::with_store(std::sync::Arc::new(Store::in_memory().unwrap()));
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

    /// A run reports itself by writing to the database. Without one there is
    /// nowhere for its output to go, and starting the agent anyway would leave
    /// a real process running that nothing could ever observe or stop.
    #[tokio::test]
    async fn spawning_without_a_store_is_refused_rather_than_unobserved() {
        let result = Jod::new()
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
        assert!(matches!(result, Err(JodError::StoreRequired)));
    }

    #[test]
    fn every_status_has_one_spelling_shared_with_the_supervisor() {
        for status in [
            AgentStatus::Running,
            AgentStatus::Completed,
            AgentStatus::Failed,
            AgentStatus::Killed,
        ] {
            assert_eq!(AgentStatus::parse(status.as_str()), Some(status));
            // The column and the JSON must agree, or a rehydrated run's status
            // would depend on which of the two was read.
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status.as_str().into())
            );
        }
    }

    // ---- runs populate conversations --------------------------------------

    fn request(prompt: &str) -> SpawnRequest {
        SpawnRequest {
            name: "n".into(),
            harness: HarnessKind::ClaudeCode,
            prompt: prompt.into(),
            cwd: PathBuf::from("/work"),
            model: Some("opus".into()),
            permission: PermissionPolicy::Ask,
            resume: crate::harness::Resume::Fresh,
        }
    }

    fn envelope(run: &str, seq: u64, event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: run.into(),
            at_ms: seq as i64,
            seq,
            event,
        }
    }

    #[test]
    fn a_run_opens_exactly_one_conversation_named_after_its_prompt() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(
            &store,
            &request("summarise the inbox"),
            "run-1",
            &RunConversation::New,
        )
        .expect("a run belongs to a conversation");

        let all = store.conversations(10).unwrap();
        assert_eq!(all.len(), 1, "one run, one conversation");
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].title, "summarise the inbox");

        let thread = store.thread(&id).unwrap();
        assert_eq!(thread.len(), 1, "the prompt is the opening turn");
        assert_eq!(thread[0].role, crate::conversation::Role::User);
        assert_eq!(thread[0].run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn a_second_run_in_the_same_conversation_extends_it_rather_than_forking() {
        let store = Store::in_memory().unwrap();
        let first =
            open_conversation(&store, &request("first"), "run-1", &RunConversation::New).unwrap();
        let second = open_conversation(
            &store,
            &request("second"),
            "run-2",
            &RunConversation::Existing(first.clone()),
        )
        .unwrap();

        assert_eq!(second, first, "the caller's conversation is the one used");
        assert_eq!(
            store.conversations(10).unwrap().len(),
            1,
            "a continuation mints nothing"
        );
        let thread = store.thread(&first).unwrap();
        assert_eq!(
            thread.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(thread[1].parent_id, Some(thread[0].id), "one line, not two");
        assert!(store.conversation(&first).unwrap().unwrap().forked_from.is_none());
    }

    #[test]
    fn a_conversation_that_is_already_named_keeps_the_name_it_has() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("first"), "run-1", &RunConversation::New)
            .unwrap();
        store.set_conversation_title(&id, "the inbox sweep").unwrap();
        open_conversation(
            &store,
            &request("second"),
            "run-2",
            &RunConversation::Existing(id.clone()),
        );
        assert_eq!(
            store.conversation(&id).unwrap().unwrap().title,
            "the inbox sweep"
        );
    }

    #[test]
    fn a_detached_run_is_recorded_in_no_conversation_at_all() {
        let store = Store::in_memory().unwrap();
        assert!(open_conversation(
            &store,
            &request("extract the facts from …"),
            "run-1",
            &RunConversation::Detached
        )
        .is_none());
        assert!(store.conversations(10).unwrap().is_empty());
    }

    #[test]
    fn a_run_pointed_at_a_conversation_that_does_not_exist_is_left_unrecorded() {
        let store = Store::in_memory().unwrap();
        let bound = open_conversation(
            &store,
            &request("hello"),
            "run-1",
            &RunConversation::Existing("nope".into()),
        );
        assert!(bound.is_none(), "a bad id must not invent a conversation");
        assert!(store.conversations(10).unwrap().is_empty());
    }

    #[test]
    fn a_long_prompt_yields_a_title_a_listing_can_show() {
        let title = title_from(&"averylongword ".repeat(20));
        assert!(title.chars().count() <= TITLE_CHARS, "{title}");
        assert!(title.ends_with('…'));
    }

    #[test]
    fn the_events_of_a_run_become_messages_in_the_order_they_arrived() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();

        for (seq, event) in [
            AgentEvent::Started {
                session_id: Some("sess-1".into()),
                model: Some("opus".into()),
            },
            AgentEvent::Thinking {
                text: "considering".into(),
            },
            AgentEvent::ToolCall {
                name: "Bash".into(),
                input: Some(serde_json::json!({"command": "ls"})),
            },
            AgentEvent::ToolResult {
                name: "Bash".into(),
                summary: Some("a.txt".into()),
                is_error: false,
            },
            AgentEvent::Message {
                text: "there is one file".into(),
            },
            AgentEvent::Raw {
                line: "noise".into(),
            },
            AgentEvent::Finished {
                text: Some("there is one file".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage::default(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            record_in_conversation(&store, &id, &envelope("run-1", seq as u64, event));
        }

        use crate::conversation::Role;
        let thread = store.thread(&id).unwrap();
        assert_eq!(
            thread.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![
                Role::User,
                Role::Thinking,
                Role::ToolCall,
                Role::ToolResult,
                Role::Assistant
            ],
            "metadata and unclassifiable lines are not turns"
        );
        assert_eq!(thread[2].tool_name.as_deref(), Some("Bash"));
        assert!(thread[1..].iter().all(|m| m.run_id.as_deref() == Some("run-1")));
    }

    #[test]
    fn the_session_the_harness_reports_lands_on_the_conversation_so_it_can_resume() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();
        record_in_conversation(
            &store,
            &id,
            &envelope(
                "run-1",
                0,
                AgentEvent::Started {
                    session_id: Some("sess-1".into()),
                    model: None,
                },
            ),
        );
        assert_eq!(
            store.resume_for(&id).unwrap(),
            crate::harness::Resume::Session("sess-1".into())
        );
    }

    /// Two processes may follow one run, and a follower that reconnects
    /// replays from its cursor — so the store dedupes on `(run_id, seq)`. This
    /// is the call site depending on that: the envelope's sequence has to reach
    /// the write, or every restart would double the transcript.
    #[test]
    fn the_same_event_recorded_twice_leaves_one_message() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();
        let event = envelope(
            "run-1",
            0,
            AgentEvent::Message {
                text: "the answer".into(),
            },
        );

        record_in_conversation(&store, &id, &event);
        record_in_conversation(&store, &id, &event);

        let thread = store.thread(&id).unwrap();
        assert_eq!(
            thread.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["go", "the answer"]
        );
    }

    /// A conversation is a side effect of a run. The Hermes audit found what
    /// happens when a memory side effect is allowed to fail the work it was
    /// watching, so this direction is one-way: the transcript may lose a turn,
    /// the run may not lose anything.
    #[test]
    fn a_conversation_write_that_fails_leaves_the_run_untouched() {
        let store = Store::in_memory().unwrap();
        let event = envelope(
            "run-1",
            0,
            AgentEvent::Message {
                text: "said something".into(),
            },
        );
        store.append_event(&event).unwrap();

        // No such conversation: every write inside must fail, and none of it
        // may reach the caller.
        record_in_conversation(&store, "no-such-conversation", &event);
        record_in_conversation(
            &store,
            "no-such-conversation",
            &envelope(
                "run-1",
                1,
                AgentEvent::Started {
                    session_id: Some("sess-1".into()),
                    model: None,
                },
            ),
        );

        assert_eq!(store.events("run-1").unwrap().len(), 1, "the run is intact");
        assert!(store.conversations(10).unwrap().is_empty());
    }

    /// The old summaries recorded a tmux session and no pid. Losing a whole
    /// run's history to a renamed field would be a worse trade than a blank one.
    #[test]
    fn a_summary_written_before_process_supervision_still_loads() {
        let legacy = serde_json::json!({
            "id": "old", "name": "n", "harness": "claude_code",
            "harness_label": "Claude Code", "status": "completed", "cwd": "/tmp",
            "model": null, "permission": "ask",
            "tmux_session": "jod-old", "attach_command": "tmux attach -t jod-old",
            "switch_command": "tmux switch-client -t jod-old", "session_closed": true,
            "stream_path": "/x/stream.jsonl",
            "created_at_ms": 1, "session_id": null, "usage": {},
            "event_count": 3, "last_message": "done"
        });
        let summary: AgentSummary = serde_json::from_value(legacy).expect("must still parse");
        assert_eq!(summary.id, "old");
        assert_eq!(summary.status, AgentStatus::Completed);
        assert_eq!(summary.pgid, None);
        assert!(!summary.process_alive);
    }
}
