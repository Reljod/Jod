//! `Jod` — the orchestrator facade.
//!
//! Jod never does the work. It launches harnesses, watches them, remembers what
//! they did, and answers questions about them. Every client (the `jod` command
//! today, an HTTP API and a phone later) drives this same struct, which is why
//! it knows nothing about terminals, sockets or HTTP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, RwLock, Semaphore};

use crate::cards::{CardKind, Importance, NewCard, Source};
use crate::conversation::{Conversation, NewMessage};
use crate::error::{JodError, Result};
use crate::event::{AgentEnvelope, AgentEvent, Usage};
use crate::harness::{HarnessKind, PermissionPolicy, SpawnRequest};
use crate::store::{Store, StoredRun};
use crate::workdir::Workdir;
use crate::{paths, proc, recall, runner, workdir};

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
        format!(
            "{}…",
            line.chars().take(TITLE_CHARS - 1).collect::<String>()
        )
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
///
/// Returns the row and not just its id, because the row is what
/// [`Jod::spawn_agent_in`] has to read the model and the mode off before it
/// launches anything.
/// Turn a directory *name* on a request into the directory a run starts in.
///
/// A caller that names `tetris` means one of the directories this conversation
/// was pointed at. It has never meant `$HOME/tetris`, which is nonetheless
/// where a relative name ended up — either as a home-directory project nobody
/// asked for, or, once the supervisor got hold of it, as a failed `chdir` into
/// the run's own scratch directory.
///
/// So: resolve against the declared roots, and when the name answers to none of
/// them, **refuse**. A blocking card says which directories were on offer, and
/// the launch does not happen. Guessing is the one option ruled out — see
/// [`crate::workdir`], and the run this was written for, whose entire output
/// landed in `$HOME` while the directory the user had added stayed empty.
///
/// An absolute path is left alone, which is every ordinary spawn: it is a
/// decision somebody made, and roots are a convention rather than a sandbox.
fn settle_cwd(store: &Store, req: &mut SpawnRequest, binding: &RunConversation) -> Result<()> {
    if req.cwd.is_absolute() {
        return Ok(());
    }
    // The conversation's roots first, because those are the ones a person
    // added and can see. `req.roots` is the fallback for a run that carries
    // its grants rather than inheriting them — `jod run` builds them from the
    // command line.
    let declared: Vec<PathBuf> = match binding {
        RunConversation::Existing(id) => store
            .roots(id)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.path)
            .collect(),
        _ => Vec::new(),
    };
    let declared = if declared.is_empty() {
        req.roots.clone()
    } else {
        declared
    };

    match workdir::launch_cwd(&req.cwd, &declared) {
        Workdir::At(path) => {
            req.cwd = path;
            Ok(())
        }
        Workdir::Refused(refusal) => {
            // The card is what a person acts on; the error is what the caller
            // shows immediately. Both, because a delegation refused in the
            // status bar and nowhere else is the silence this whole area is
            // being fixed for.
            if let RunConversation::Existing(id) = binding {
                let card = NewCard {
                    conversation_id: id.clone(),
                    kind: Some(CardKind::Question),
                    importance: Some(Importance::High),
                    // The run cannot start until somebody says where. That is
                    // the definition of blocking, rather than a judgement about
                    // how much it matters.
                    blocking: true,
                    title: refusal.title(),
                    body: refusal.body(),
                    options: refusal
                        .roots
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                    source: Some(Source::Jod),
                    dedupe_key: Some(format!("workdir:{}", refusal.name)),
                    ..NewCard::default()
                };
                if let Err(e) = store.raise_card(card) {
                    eprintln!("[jod] could not raise the working-directory card: {e}");
                }
            }
            Err(JodError::Invalid(format!(
                "{}\n\n{}",
                refusal.title(),
                refusal.body()
            )))
        }
    }
}

fn open_conversation(
    store: &Store,
    req: &SpawnRequest,
    run_id: &str,
    binding: &RunConversation,
) -> Option<Conversation> {
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
    //
    // `append_prompt` rather than a plain append, so the question is keyed like
    // everything else the run writes. It cannot double today — every spawn
    // mints a fresh run id — and that is exactly why the guard belongs in the
    // store rather than in this call site's good behaviour.
    if let Err(e) = store.append_prompt(&conversation.id, run_id, &req.prompt) {
        eprintln!(
            "[jod] could not record the prompt on conversation {}: {e}",
            conversation.id
        );
    }

    Some(conversation)
}

/// Let the conversation overrule the request on the two settings it owns.
///
/// **Why the stored value wins outright.** Neither the model nor the permission
/// mode was ever a property of a process. Jod respawns the harness once per turn
/// against a resumed session, so `--model` and `--permission-mode` are decided
/// afresh at every spawn — a choice held in the caller lasts exactly one turn.
/// That is the bug: the TUI's `/model` set a field on the next request, and
/// reopening the conversation came back on whatever the client happened to
/// default to. The only place an answer can live and survive a restart is the
/// row the spawn is for, and once it lives there, deferring to the request would
/// mean every client had to remember to read the row first.
///
/// Changing either of them on a live thread is therefore a write —
/// [`Store::set_conversation_model`], [`Store::set_conversation_permission`] —
/// not a different argument on the next spawn.
///
/// `None` on the row is not a value; it means "no opinion", which is what every
/// conversation older than `0011_settings_and_modes` says, and it leaves the
/// caller's choice exactly where it was.
///
/// **`harness` is deliberately not treated this way.** Moving a thread to
/// another harness has consequences — a session id that means nothing on the
/// other side, a transcript that has to be replayed, context that has to be
/// compacted first ([`Store::switch_harness`]) — and none of that can be done by
/// a spawn quietly reading a column. A handoff lands here as
/// [`RunConversation::Existing`] naming the *new* conversation, whose harness is
/// already the one the caller asked for.
/// Public because "what will this spawn actually use" is a question clients ask
/// *before* spawning — a status bar that shows the app's own mode while the
/// conversation's stored one is what the run will get is a status bar that lies.
/// [`Jod::spawn_agent_in`] applies it either way; nobody has to remember to.
pub fn prefer_conversation_settings(req: &mut SpawnRequest, conversation: &Conversation) {
    if let Some(model) = &conversation.model {
        req.model = Some(model.clone());
    }
    if let Some(permission) = conversation.permission {
        req.permission = permission;
    }
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
/// **This is no longer the writer that matters.** A binding lasts exactly as
/// long as the process holding it, and the processes that launch runs routinely
/// exit while the run is still talking — `jod main` without `--wait` returns as
/// soon as the instruction is handed over, and a session opened through
/// `open_work` is launched by the MCP server, which exits with its harness.
/// Everything said after that was never written down, silently, while `events`
/// stayed complete because the supervisor writes that one. So the supervisor
/// now projects the transcript as well, and it is the writer that cannot miss
/// an event — see `EventWriter::record_in_conversation` in `supervisor`. This
/// one still runs, because it is the same write and the same idempotence key,
/// and because a live client watching a run should not have to wait on another
/// process's turn to see the row appear.
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
    //
    // Written together with the harness that minted it, because `resume_for`
    // hands the id back only to that same harness and a row where the two
    // disagree resumes nothing. The harness comes off the run rather than the
    // conversation for exactly that reason: the run is what actually just
    // spoke, and when it is not what the row expected, the row is the thing
    // that is wrong.
    if let AgentEvent::Started {
        session_id: Some(session),
        ..
    } = &envelope.event
    {
        // A run that reported a session but has no row is not a state worth
        // guessing through: writing the id under the conversation's existing
        // harness is the very pairing this avoids, so the id is dropped and
        // the thread starts fresh next turn rather than resuming wrongly.
        match store.run(&envelope.agent_id) {
            Ok(Some(run)) => match HarnessKind::from_id(&run.harness) {
                Some(on) => {
                    if let Err(e) = store.record_session(conversation_id, on, session) {
                        eprintln!("[jod] could not record the session on {conversation_id}: {e}");
                    }
                }
                None => eprintln!(
                    "[jod] run {} reported a session on unknown harness {:?}; not recording it",
                    envelope.agent_id, run.harness
                ),
            },
            Ok(None) => eprintln!(
                "[jod] run {} reported a session before it was stored; not recording it",
                envelope.agent_id
            ),
            Err(e) => eprintln!("[jod] could not read run {}: {e}", envelope.agent_id),
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

/// How often the concurrency watcher checks whether a launched run's process
/// group is still alive. Same order of magnitude as [`runner::follow`]'s own
/// poll, which this deliberately does not share — this one exists even when
/// nobody is watching the run's output, and coupling the two would mean a
/// change to one silently retunes the other.
const CAPACITY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// The cap on agents running at once, before an eighth agent takes a four-core
/// box to a load of 60. `available_parallelism` unless `JOD_MAX_CONCURRENT_AGENTS`
/// says otherwise — the same override shape [`crate::discovery::find_binary`]
/// uses, so one environment variable can pin this on a box that wants a
/// different number than its own core count.
///
/// A box that cannot even ask how many cores it has gets 1, not 0 — a cap of
/// zero would refuse every spawn forever, which is a worse failure than
/// running one agent at a time on hardware nobody could size.
pub fn default_max_concurrent_agents() -> usize {
    if let Ok(v) = std::env::var("JOD_MAX_CONCURRENT_AGENTS") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

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
    /// Bounds how many launched processes may be running at once. Every
    /// caller funnels through [`Jod::spawn_agent_in`], which acquires a
    /// permit before [`runner::launch`] and hands it to a watcher that holds
    /// it for the launched process's whole life — so the (N+1)th concurrent
    /// spawn queues on `acquire_owned` rather than piling another process
    /// onto a box that is already at capacity. → `default_max_concurrent_agents`
    concurrency: Arc<Semaphore>,
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
            concurrency: Arc::new(Semaphore::new(default_max_concurrent_agents())),
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
                    // The passive half of D2: a run launched without Jod's MCP
                    // server still puts its questions on the rail, lifted out
                    // of what the harness prints. Cheap on the ordinary event —
                    // `lift` matches a tool name and returns — and idempotent
                    // through the card's dedupe key, so a harness that both
                    // calls Jod's tool and prints its own question produces one
                    // card rather than two.
                    if let Err(e) =
                        crate::mcp::lift_into_cards(store, &envelope.agent_id, &envelope.event)
                    {
                        // Reported, never fatal. A card that could not be
                        // raised is a question nobody sees; a run taken down by
                        // one is work nobody gets.
                        eprintln!("[jod] could not raise a card from the stream: {e}");
                    }
                    // The stream half of E6.S3, here rather than on a tick
                    // because immediacy is the entire reason this half exists:
                    // a pull request URL is worth showing the moment it is
                    // printed, and the poll that follows is what gives it a
                    // state. Cheap on the ordinary event — no text, or text
                    // with no `/pull/` in it, costs one scan — and idempotent
                    // on the URL, so a replayed stream produces one row.
                    if let Err(e) = crate::prs::note_from_stream(
                        store,
                        conversation.as_deref(),
                        &envelope.event,
                    ) {
                        // Same rule as the card above. A pull request nobody
                        // recorded is a row missing from a panel; a run taken
                        // down by one is work nobody gets.
                        eprintln!("[jod] could not record a pull request from the stream: {e}");
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

    /// How many runs are on record, whichever process launched them. Zero
    /// without a store, where nothing outlives this process anyway.
    ///
    /// The database is the authority here, not [`agents`](Self::agents): this
    /// process only ever reads back the newest few hundred runs, so counting
    /// what it holds would understate a box that has been busy.
    pub fn run_count(&self) -> Result<usize> {
        match &self.store {
            Some(store) => store.run_count(),
            None => Ok(0),
        }
    }

    /// Load runs from the database into memory. Returns how many were new.
    ///
    /// A daemon that restarts has no idea what it launched before; without this
    /// every earlier agent vanishes from `agents()` even though its supervisor
    /// may still be running.
    ///
    /// Calling it *again* is how a process learns about runs it did not launch.
    /// Nothing crosses a process boundary here but the database: a run spawned
    /// by `jod tui` publishes to that process's broadcast channel and nowhere
    /// else, so a resident `jod-api` that only rehydrated at boot can never
    /// list it and never stream it. Repeating the scan is what closes that gap —
    /// see [`adopt_new_runs`](Self::adopt_new_runs), which is this on a timer.
    /// Repeating is cheap on purpose: a run already held is skipped before its
    /// events are read, so the steady state costs one indexed query.
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
            // Before anything expensive. The write-lock check further down is
            // the one that closes the race, but it comes after a full event
            // replay — fine once at boot, ruinous on a two-second timer, where
            // almost every row is one this process already owns.
            if self.state.read().await.agents.contains_key(&run.id) {
                continue;
            }

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

    /// Adopt runs other processes spawn, for as long as the caller lives.
    ///
    /// Jod is not one process. `jod tui`, `jod run` and `jod-api` each build
    /// their own [`Jod`] over the same SQLite file, and a run's events reach
    /// only the broadcast channel of the process that launched it. So a
    /// resident API that rehydrated once at boot is frozen: a run started from
    /// the TUI a minute later is absent from `/v1/agents` and silent on
    /// `/v1/events`, and the web HUD shows an idle fleet while a harness is
    /// working. Nothing was dropped — the API was never told.
    ///
    /// This is the telling. Each pass is a [`rehydrate`](Self::rehydrate), which
    /// skips what is already held and attaches a [`runner::follow`] to anything
    /// new that is still alive; the follower polls the shared store, so it works
    /// perfectly well for a run this process holds no handle to, and two
    /// processes following one run is expected rather than a conflict.
    ///
    /// Polling, because SQLite has no way to say "a row appeared". `every`
    /// therefore sets how late the fleet can be, and it is worth an interval
    /// well under a human's patience: the follower already polls at 120 ms once
    /// attached, so the discovery interval is the whole of the delay a person
    /// sees between starting a run and watching it move.
    ///
    /// A failing pass is reported and the loop continues. The database being
    /// briefly unreadable must not permanently stop the process from noticing
    /// new work — that failure mode is exactly the one this method exists to
    /// remove.
    ///
    /// Never returns. Spawn it, and drop the handle to stop.
    pub async fn adopt_new_runs(self: Arc<Self>, limit: usize, every: Duration) {
        // No store means no other process to share one with.
        if self.store.is_none() {
            return;
        }
        loop {
            tokio::time::sleep(every).await;
            match self.rehydrate(limit).await {
                Ok(0) => {}
                Ok(n) => eprintln!("[jod] picked up {n} run(s) started elsewhere"),
                Err(e) => eprintln!("[jod] could not scan for new runs: {e}"),
            }
        }
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

    /// Launch an agent whose prompt was built from material Jod did not write.
    ///
    /// The only way to start a run from a GitHub payload, an email, a fetched
    /// page — anything a stranger can put text into. It caps the tool grant to
    /// what [`crate::store::Origin::Untrusted`] may ever reach, which is
    /// reading and nothing else.
    ///
    /// **This exists as its own method because the cap was written, tested and
    /// applied nowhere.** `ToolAccess::capped_for` had two callers, both unit
    /// tests, one of them named `untrusted_material_can_never_reach_more_than
    /// _reading` — passing while nothing enforced it. A rule that depends on
    /// every future call site remembering to apply it is not a rule; it is a
    /// convention with a test pretending to be a guard.
    ///
    /// The escalation it closes: a webhook rule names a schedule someone raised
    /// to `orchestrate`, a stranger opens a pull request matching that rule, and
    /// their text is steering an agent that can arm schedules. Each step is
    /// reasonable on its own, which is what makes it the shape to worry about.
    pub async fn spawn_from_untrusted(&self, mut req: SpawnRequest) -> Result<AgentSummary> {
        req.tools = req
            .tools
            .map(|granted| granted.capped_for(crate::store::Origin::Untrusted));
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
        mut req: SpawnRequest,
        conversation: RunConversation,
    ) -> Result<AgentSummary> {
        let store = self.store.clone().ok_or(JodError::StoreRequired)?;

        // Before anything is written down, because everything written down
        // afterwards records this directory: the conversation's `cwd`, the
        // run's row, and the plan the supervisor chdirs into.
        //
        // And before the harness is located, so that "`tetris` is not one of
        // your directories" is what a person is told rather than having it
        // masked by whichever harness happens to be missing on this machine.
        // Neither question depends on the other.
        settle_cwd(&store, &mut req, &conversation)?;

        let program = req
            .harness
            .locate()
            .ok_or_else(|| JodError::HarnessNotFound(req.harness.label().to_string()))?;

        let id = uuid::Uuid::new_v4().to_string();

        // Open the conversation before the launch for the same reason the agent
        // is registered before it: an event that arrived first would find no
        // binding and be dropped from the transcript. It costs a conversation
        // holding only a prompt if the launch then fails, which is the truthful
        // record of an attempt rather than a leak.
        let conversation = open_conversation(&store, &req, &id, &conversation);

        // ...and before the summary is built, because the conversation outranks
        // the request on two of its fields. See `prefer_conversation_settings`.
        if let Some(open) = &conversation {
            prefer_conversation_settings(&mut req, open);
        }

        // What Jod already knows about this, as framing rather than as a turn.
        //
        // Here — the one entry point every spawn funnels through — because a
        // recall that only the chat box performed would be a Jod that learns
        // from you when you are watching and forgets when a schedule fires. The
        // whole point is the opposite.
        //
        // `Origin::Agent` is the trigger: this call is Jod acting, not the
        // owner speaking, and `recall` uses that to decide how much it may
        // lean on lower-trust material. Nothing marked `Untrusted` is ever
        // injected, whatever the trigger — a preamble is the position from
        // which an agent is steered, and material Jod merely *read* has no
        // business there.
        recall::augment(&store, &mut req, crate::store::Origin::Agent);

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
                guard.conversations.insert(id.clone(), conversation.id);
            }
        }

        // Record the run before it starts. The supervisor updates this row from
        // its own process, so the row has to exist before it is launched —
        // and a crash mid-launch still leaves a trace of what was attempted.
        if let Err(e) = store.save_run(&stored_run(&summary)) {
            eprintln!("[jod] could not persist run: {e}");
        }

        // Wait for a slot before starting a process, not before accepting the
        // request — the request is already recorded above. This is the one
        // seam every caller funnels through (the TUI calls this directly; the
        // API's own pre-check at `max_concurrent_agents` runs before it ever
        // gets here), so queueing here is what keeps an eighth agent on a
        // four-core box from becoming an eighth process. `acquire_owned` is a
        // fair FIFO wait, not a rejection: a queued caller's `spawn_agent_in`
        // simply takes longer to return, which is the whole of "queue, don't
        // reject."
        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .expect("the concurrency semaphore is never closed");

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
                // `permit` drops here, freeing the slot immediately: nothing
                // was ever running, so nothing should be waited on.
                let mut guard = self.state.write().await;
                if let Some(record) = guard.agents.get_mut(&id) {
                    record.summary.status = AgentStatus::Failed;
                }
                let _ = store.set_run_status(&id, AgentStatus::Failed.as_str());
                return Err(e);
            }
        };

        // Hand the permit to a watcher that holds it for the process's whole
        // life and releases it the moment the process group is gone —
        // however the run ends: it finishes, it is killed, or its supervisor
        // dies without ever writing a `Finished` event. Tied to the process
        // group rather than to the event stream on purpose: a slot the event
        // stream forgot to free is a concurrency cap that silently stops
        // admitting anyone, which is worse than the bug this fixes.
        {
            let pgid = launched.pgid;
            tokio::spawn(async move {
                let _permit = permit;
                while proc::group_alive(pgid) {
                    tokio::time::sleep(CAPACITY_POLL_INTERVAL).await;
                }
            });
        }

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

    /// The newest `limit` agents, and how many this process holds in total.
    ///
    /// [`agents`] hands back launch order — oldest first — which is what a
    /// board that renders every row wants, and every other caller depends on.
    /// A terminal listing wants the opposite, for the same reason
    /// [`history`](Self::history) is newest first: on a box that has
    /// accumulated runs, the one still running is at the *new* end, and
    /// oldest-first pushed the only row worth reading off the bottom of the
    /// screen. The count comes back with the page so the caller can say how
    /// many rows it hid rather than silently truncating.
    ///
    /// `limit` is a row cap, not a fetch cap: what this process knows about is
    /// decided by [`rehydrate`](Self::rehydrate).
    pub async fn recent_agents(&self, limit: usize) -> (Vec<AgentSummary>, usize) {
        let guard = self.state.read().await;
        // `order` can name a run whose record is gone, so the total has to be
        // counted from what actually resolves rather than from `order.len()`.
        let known: Vec<&String> = guard
            .order
            .iter()
            .filter(|id| guard.agents.contains_key(*id))
            .collect();
        let total = known.len();
        let page = known
            .into_iter()
            .rev()
            .take(limit)
            .filter_map(|id| guard.agents.get(id))
            .map(|r| r.summary.clone())
            .collect();
        (page, total)
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
        let (pgid, was_running) = match self.state.read().await.agents.get(id) {
            Some(record) => (
                record.summary.pgid,
                record.summary.status == AgentStatus::Running,
            ),
            None => return Err(JodError::UnknownAgent(id.to_string())),
        };

        if let Some(pgid) = pgid {
            proc::terminate_group(pgid, KILL_GRACE)
                .await
                .map_err(|e| JodError::Kill(format!("process group {pgid}: {e}")))?;
        }

        let mut guard = self.state.write().await;
        if let Some(record) = guard.agents.get_mut(id) {
            record.summary.process_alive = false;
            // `Failed` counts, and this is the whole of it: the harness dies
            // *because* of the signal above, and its own ending arrives — as a
            // `Finished { is_error: true }`, folded in by `apply` — while
            // `terminate_group` is still waiting out the grace. So by the time
            // this lock is taken the status may already say the run failed,
            // written by the exit this call caused. The supervisor knows better
            // and stores `killed`; without this the memory the TUI reads
            // disagreed with the row `jod ls` reads, and a run the user stopped
            // on purpose showed as a red failure until the next restart.
            //
            // `was_running` is what keeps that from relabelling somebody else's
            // failure: only a run that was still going when this was asked can
            // have been ended by it.
            let ended_here = was_running
                && matches!(
                    record.summary.status,
                    AgentStatus::Running | AgentStatus::Failed
                );
            if ended_here {
                record.summary.status = AgentStatus::Killed;
                if let Some(store) = &self.store {
                    let _ = store.set_run_status(id, AgentStatus::Killed.as_str());
                }
            }
        }
        Ok(())
    }

    /// Stop a run a watchdog has judged dead, and make its status say so.
    ///
    /// Distinct from [`Jod::kill_agent`] in two ways that matter.
    ///
    /// **It ends `Failed`, not `Killed`.** `Killed` means a person decided to
    /// stop this; `Failed` means it stopped working and something noticed.
    /// Collapsing them would make "I stopped it" and "it wedged and was reaped"
    /// the same row, which is the distinction anybody looking at the history is
    /// there to make.
    ///
    /// **It works from the store, not only from memory.** `kill_agent` reads
    /// the pgid out of the in-memory map and fails with `UnknownAgent` if the
    /// run is not there. A heartbeat sweep is exactly the caller that cannot
    /// rely on that: it runs in a daemon that rehydrates a bounded number of
    /// runs, so a long-running run started before the last few hundred others
    /// is watched by a row it can still read and absent from the map. Falling
    /// back to the stored pgid is what keeps "long-running" and "reapable" from
    /// being mutually exclusive.
    ///
    /// `terminate` is false for a run whose process group is already gone —
    /// signalling a recycled pgid would reach whatever now holds that number.
    pub async fn fail_agent(&self, id: &str, terminate: bool) -> Result<()> {
        let in_memory = self.state.read().await.agents.get(id).map(|r| r.summary.pgid);
        let pgid = match in_memory {
            Some(pgid) => pgid,
            None => match &self.store {
                Some(store) => store.run(id)?.and_then(|r| r.pgid),
                None => None,
            },
        };

        if terminate {
            if let Some(pgid) = pgid {
                // A failure to signal is not a reason to leave the status
                // lying. The group may have exited between the probe and here,
                // which is the ordinary case, not an error.
                let _ = proc::terminate_group(pgid, KILL_GRACE).await;
            }
        }

        if let Some(store) = &self.store {
            store.set_run_status(id, AgentStatus::Failed.as_str())?;
        }
        let mut guard = self.state.write().await;
        if let Some(record) = guard.agents.get_mut(id) {
            record.summary.process_alive = false;
            if record.summary.status == AgentStatus::Running {
                record.summary.status = AgentStatus::Failed;
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

    /// The cap has to be applied by the *spawn path*, not by whoever remembers
    /// to call it. It previously had two callers, both unit tests, one named
    /// `untrusted_material_can_never_reach_more_than_reading` — green while
    /// nothing enforced it anywhere a run could reach.
    ///
    /// This asserts the reduction the method performs, which is the part that
    /// must not regress: a grant of `orchestrate` on an untrusted prompt comes
    /// out as read-only.
    #[test]
    fn an_untrusted_spawn_is_capped_to_reading_whatever_it_was_granted() {
        use crate::harness::ToolAccess;
        for granted in [
            ToolAccess::ReadOnly,
            ToolAccess::Delegate,
            ToolAccess::Orchestrate,
        ] {
            let capped = Some(granted.capped_for(crate::store::Origin::Untrusted));
            assert_eq!(capped, Some(ToolAccess::ReadOnly), "{granted:?} escaped");
            assert!(!capped.unwrap().may_delegate());
        }
    }

    /// A run with no grant at all must stay ungranted rather than acquiring
    /// read-only on its way through the cap.
    #[test]
    fn capping_an_ungranted_spawn_does_not_hand_it_tools() {
        let none: Option<crate::harness::ToolAccess> = None;
        let capped =
            none.map(|g: crate::harness::ToolAccess| g.capped_for(crate::store::Origin::Untrusted));
        assert_eq!(capped, None);
    }

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

    /// A box that has accumulated runs: `count` finished ones, oldest first,
    /// and then one still running as the newest — the shape `jod ls` was
    /// reported on, where the single running agent was the very last line.
    fn store_with_a_backlog(count: usize) -> std::sync::Arc<Store> {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        for i in 0..count {
            let mut summary = record().summary;
            summary.id = format!("old-{i:03}");
            summary.name = format!("run {i}");
            summary.status = AgentStatus::Completed;
            summary.created_at_ms = i as i64;
            summary.pid = Some(4_000_000);
            summary.pgid = Some(4_000_000);
            store.save_run(&stored_run(&summary)).unwrap();
        }
        let mut live = record().summary;
        live.id = "the-live-one".into();
        live.name = "the run worth reading".into();
        live.status = AgentStatus::Running;
        live.created_at_ms = count as i64;
        // This test process, so the liveness probe in `rehydrate` finds a real
        // group and leaves the run marked running instead of failing it.
        live.pid = Some(std::process::id());
        live.pgid = Some(std::process::id());
        store.save_run(&stored_run(&live)).unwrap();
        store
    }

    /// The reported bug: 88 rows came out oldest first, so the one running
    /// agent was the last line and scrolled off the screen.
    #[tokio::test]
    async fn listing_agents_puts_the_newest_run_first() {
        let jod = Jod::with_store(store_with_a_backlog(87));
        jod.rehydrate(1000).await.unwrap();

        let (page, total) = jod.recent_agents(88).await;
        assert_eq!(total, 88);
        assert_eq!(page.len(), 88);
        assert_eq!(page[0].id, "the-live-one", "the running run must lead");
        assert_eq!(page[0].status, AgentStatus::Running);
        assert_eq!(page.last().unwrap().id, "old-000", "oldest run goes last");

        let times: Vec<i64> = page.iter().map(|a| a.created_at_ms).collect();
        let mut descending = times.clone();
        descending.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(times, descending, "every row must be newest first");
    }

    /// The cap keeps the listing to a screenful, and takes it off the *old*
    /// end — capping the new end would hide exactly the row the cap exists to
    /// surface.
    #[tokio::test]
    async fn the_listing_cap_keeps_the_newest_rows_and_reports_the_total() {
        let jod = Jod::with_store(store_with_a_backlog(87));
        jod.rehydrate(1000).await.unwrap();

        let (page, total) = jod.recent_agents(20).await;
        assert_eq!(page.len(), 20, "the cap is applied");
        assert_eq!(total, 88, "the total is still reported, so 68 hidden");
        assert_eq!(
            jod.run_count().unwrap(),
            88,
            "and the database agrees, so the hidden count is not a guess"
        );
        assert_eq!(page[0].id, "the-live-one");
        assert_eq!(page.last().unwrap().id, "old-068");
        assert!(
            !page.iter().any(|a| a.id == "old-000"),
            "the oldest rows are the ones dropped"
        );
    }

    /// The escape hatch: `jod ls --all` passes a limit past the row count and
    /// must come back with every run, still newest first.
    #[tokio::test]
    async fn asking_for_everything_returns_every_run() {
        let jod = Jod::with_store(store_with_a_backlog(87));
        jod.rehydrate(1000).await.unwrap();

        let (page, total) = jod.recent_agents(i64::MAX as usize).await;
        assert_eq!(page.len(), 88);
        assert_eq!(page.len(), total);
        assert_eq!(page[0].id, "the-live-one");
        assert_eq!(page.last().unwrap().id, "old-000");
    }

    /// A cap larger than the listing is not an error and hides nothing.
    #[tokio::test]
    async fn a_cap_wider_than_the_listing_hides_nothing() {
        let jod = Jod::with_store(store_with_one_finished_run());
        jod.rehydrate(100).await.unwrap();
        let (page, total) = jod.recent_agents(20).await;
        assert_eq!(page.len(), 1);
        assert_eq!(total, 1);
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

    /// The wiring test for the stream half of E6.S3, and it is the *point* of
    /// that half rather than a formality.
    ///
    /// Everything in `prs` was built, unit-tested and green while nothing
    /// called any of it, which is a state unit tests cannot detect: a test on
    /// an unreachable function passes for ever. So this one drives the real
    /// event loop — `events_tx` is private, which is why the test has to live
    /// here — and asserts the side effect. Delete the `note_from_stream` call
    /// in `build` and this fails.
    ///
    /// The broadcast is the synchronisation point rather than a sleep: the loop
    /// sends the envelope on only after the store side effects, so receiving it
    /// back is proof they have happened.
    #[tokio::test]
    async fn a_pull_request_printed_by_a_run_is_recorded_as_the_event_goes_past() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let jod = Jod::with_store(store.clone());
        let mut watching = jod.subscribe();

        jod.events_tx
            .send(envelope(
                "run-1",
                0,
                AgentEvent::ToolResult {
                    name: "Bash".into(),
                    // What `gh pr create` prints, which is where a pull request
                    // URL actually appears — a tool result, not prose.
                    summary: Some("https://github.com/Reljod/Jod/pull/61".into()),
                    is_error: false,
                },
            ))
            .unwrap();
        watching.recv().await.expect("the loop handled the event");

        let recorded = store
            .pull_request("https://github.com/Reljod/Jod/pull/61")
            .unwrap()
            .expect("the run's pull request was recorded as it went past");
        assert_eq!(recorded.number, Some(61));
        assert_eq!(
            recorded.state,
            crate::prs::State::Unknown,
            "a URL is not a status; the poll is what gives it one"
        );
    }

    /// The other half of the same guard: this runs on every event of every run,
    /// so it must record nothing at all from ordinary output.
    #[tokio::test]
    async fn ordinary_output_going_past_records_no_pull_request() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let jod = Jod::with_store(store.clone());
        let mut watching = jod.subscribe();

        jod.events_tx
            .send(envelope(
                "run-1",
                0,
                AgentEvent::Message {
                    text: "I have pushed the branch and the tests are green.".into(),
                },
            ))
            .unwrap();
        watching.recv().await.expect("the loop handled the event");

        assert!(store.stale_pull_requests(10).unwrap().is_empty());
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

    /// A failure to *stop* must not be worded as a failure to *start*. The one
    /// message a worried reader studies hardest is the one about an agent that
    /// may still be writing to their files, and it used to contain three
    /// contradictory claims.
    #[test]
    fn a_stop_failure_does_not_claim_the_agent_would_not_start() {
        let said = JodError::Kill("process group 37237: Operation not permitted".into()).to_string();
        assert!(said.contains("could not stop the agent"), "{said}");
        assert!(!said.contains("start"), "{said}");
    }

    /// The guard on the repair above: a run that had already failed on its own
    /// before anybody asked it to stop keeps its failure. Relabelling that as a
    /// kill would hide the one status a reader needs to see.
    #[tokio::test]
    async fn stopping_a_run_that_had_already_failed_leaves_it_failed() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "already-failed".into();
        summary.status = AgentStatus::Running;
        // A pgid nothing is behind, so the stop is a no-op and the status is
        // the only thing under test.
        summary.pid = Some(4_000_000);
        summary.pgid = Some(4_000_000);
        store.save_run(&stored_run(&summary)).unwrap();

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        assert_eq!(
            jod.agent("already-failed").await.unwrap().status,
            AgentStatus::Failed,
            "a running row with a dead group is a failure, and this is before the kill"
        );

        jod.kill_agent("already-failed").await.unwrap();
        assert_eq!(
            jod.agent("already-failed").await.unwrap().status,
            AgentStatus::Failed
        );
    }

    /// A run stopped on purpose must read `killed` *without* a restart.
    ///
    /// The race this pins is the ordinary case, not a corner: `kill_agent`
    /// signals the group and then waits out the grace, and the harness dies
    /// during that wait. Its ending arrives as `Finished { is_error: true }` —
    /// a process killed by a signal is an error to everything that only sees
    /// the exit — and folds into the record while the kill is still waiting.
    /// The supervisor, which saw the signal, stores `killed`. So the row said
    /// `killed`, memory said `failed`, and the TUI reading memory showed a red
    /// ✗ for a run the reader stopped themselves, until the next restart
    /// silently changed its mind.
    #[tokio::test]
    async fn a_run_stopped_on_purpose_reads_killed_before_any_restart() {
        let dir = std::env::temp_dir().join(format!("jod-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("run.sh");
        // Deaf to `SIGTERM`, so the group is still there while the event lands —
        // in production the grace is what holds this window open. The `ready`
        // file is waited for below: a signal that arrives before the script has
        // exec'd hits a child with no trap installed yet and kills it outright,
        // which closes the window this test needs open.
        std::fs::write(
            &prog,
            "#!/usr/bin/env bash\ntrap '' TERM\n: > ready\nfor _ in $(seq 1 30); do sleep 0.05; done\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pgid = crate::proc::spawn_detached(&prog, &[], &dir, &dir.join("log")).unwrap();
        for _ in 0..200 {
            if dir.join("ready").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            crate::proc::group_alive(pgid),
            "the fixture run must actually be running"
        );

        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let mut summary = record().summary;
        summary.id = "raced-kill".into();
        summary.status = AgentStatus::Running;
        summary.pid = Some(pgid);
        summary.pgid = Some(pgid);
        store.save_run(&stored_run(&summary)).unwrap();

        let jod = Jod::with_store(store.clone());
        jod.rehydrate(100).await.unwrap();
        let mut watching = jod.subscribe();

        let stopping = tokio::spawn({
            let jod = jod.clone();
            async move { jod.kill_agent("raced-kill").await }
        });

        // The harness's own ending, arriving mid-kill through the real event
        // loop — the same envelope the supervisor emits for a signalled run.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !stopping.is_finished(),
            "the ending has to land while the stop is still waiting, or this \
             test proves nothing"
        );
        jod.events_tx
            .send(AgentEnvelope {
                agent_id: "raced-kill".into(),
                at_ms: 1,
                seq: 0,
                event: AgentEvent::Finished {
                    text: None,
                    exit_code: None,
                    is_error: true,
                    usage: Usage::default(),
                },
            })
            .unwrap();
        watching.recv().await.expect("the loop handled the ending");

        stopping.await.unwrap().expect("the run stops");

        assert_eq!(
            jod.agent("raced-kill").await.unwrap().status,
            AgentStatus::Killed,
            "a deliberate stop is not a failure"
        );
        assert_eq!(
            store.run("raced-kill").unwrap().unwrap().status,
            "killed",
            "and the live view agrees with the row `jod ls` reads"
        );
        std::fs::remove_dir_all(&dir).ok();
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

    /// Write the row another process's `spawn_agent` would have written.
    ///
    /// The pgid is this test process, so `rehydrate`'s liveness probe finds a
    /// real group and treats the run as still going — which is the only case
    /// that attaches a follower, and the whole point of the tests below.
    fn spawned_elsewhere(store: &Store, id: &str) {
        let mut summary = record().summary;
        summary.id = id.into();
        summary.status = AgentStatus::Running;
        summary.pid = Some(std::process::id());
        summary.pgid = Some(std::process::id());
        store.save_run(&stored_run(&summary)).unwrap();
    }

    /// The reported bug, at its root: a run started in `jod tui` never showed
    /// up in the web HUD, live or otherwise.
    ///
    /// `jod-api` rehydrated once at boot and then only ever heard its own
    /// broadcast channel, so a run another process launched a minute later was
    /// absent from `/v1/agents` and silent on `/v1/events`. Nothing was lost —
    /// the API was never told. Rescanning is the telling.
    #[tokio::test]
    async fn a_second_process_never_learns_of_a_new_run_without_rescanning() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let api = Jod::with_store(store.clone());
        api.rehydrate(100).await.unwrap(); // boot: the store is empty

        spawned_elsewhere(&store, "from-the-tui");

        assert!(
            api.agents().await.is_empty(),
            "the boot-time scan cannot see a run that did not exist yet"
        );
        assert_eq!(api.rehydrate(100).await.unwrap(), 1, "the rescan sees it");
        assert_eq!(api.agents().await[0].id, "from-the-tui");
    }

    /// Listing it is half the fix; the HUD's animation is driven by envelopes.
    ///
    /// Adopting an *alive* run attaches a [`runner::follow`], which polls the
    /// shared store — so events written by a process this one holds no handle
    /// to still reach this process's subscribers.
    #[tokio::test]
    async fn an_adopted_run_streams_its_events_to_this_process() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let api = Jod::with_store(store.clone());
        let mut watching = api.subscribe();

        spawned_elsewhere(&store, "from-the-tui");
        api.rehydrate(100).await.unwrap();

        // Written after adoption, by "the other process".
        store
            .append_event(&envelope(
                "from-the-tui",
                0,
                AgentEvent::Message {
                    text: "working on the Tetris game".into(),
                },
            ))
            .unwrap();

        let seen = tokio::time::timeout(Duration::from_secs(5), watching.recv())
            .await
            .expect("the follower never forwarded the event")
            .expect("the broadcast channel closed");
        assert_eq!(seen.agent_id, "from-the-tui");
        assert!(matches!(seen.event, AgentEvent::Message { .. }));
    }

    /// Running the scan on a timer means running it over runs already held,
    /// over and over. That has to be inert — and the part a client would see if
    /// it were not is a **second follower** on a live run, doubling every
    /// envelope on the stream.
    ///
    /// The cost side of the same change — skipping a held run *before* replaying
    /// its events rather than after — is not asserted here. It is not
    /// observable through this API, only in how much work a pass does.
    #[tokio::test]
    async fn rescanning_is_inert_on_a_run_it_already_holds() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let jod = Jod::with_store(store.clone());
        spawned_elsewhere(&store, "held");

        jod.rehydrate(100).await.unwrap();
        let events_before = jod.agent("held").await.unwrap().event_count;
        let mut watching = jod.subscribe();
        assert_eq!(jod.rehydrate(100).await.unwrap(), 0, "nothing new");
        assert_eq!(jod.agents().await.len(), 1, "the run was listed twice");
        assert_eq!(
            jod.agent("held").await.unwrap().event_count,
            events_before,
            "the rescan re-applied a run it already held"
        );

        store
            .append_event(&envelope(
                "held",
                0,
                AgentEvent::Message {
                    text: "once, please".into(),
                },
            ))
            .unwrap();

        let first = tokio::time::timeout(Duration::from_secs(5), watching.recv())
            .await
            .expect("the follower never forwarded the event")
            .expect("the broadcast channel closed");
        assert_eq!(first.seq, 0);
        // The follower polls every 120 ms, so a duplicate would already be here.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), watching.recv())
                .await
                .is_err(),
            "the event arrived twice — the rescan attached a second follower"
        );
    }

    #[tokio::test]
    async fn adopting_without_a_store_returns_rather_than_polling_forever() {
        // No store is no shared database, so there is no second process to
        // learn from and nothing to poll. Looping anyway would burn a task for
        // the life of every in-memory `Jod`, tests included.
        tokio::time::timeout(
            Duration::from_secs(5),
            Jod::new().adopt_new_runs(100, Duration::from_secs(1)),
        )
        .await
        .expect("adopt_new_runs polled a service that has no store");
    }

    /// The loop's own promise: it keeps scanning, so a run started at any point
    /// after boot is adopted rather than only one started before the first tick.
    #[tokio::test]
    async fn the_adoption_loop_keeps_scanning_after_its_first_pass() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let api = Jod::with_store(store.clone());
        let task = tokio::spawn(
            api.clone()
                .adopt_new_runs(100, Duration::from_millis(50)),
        );

        // Deliberately after the loop is running and has already found nothing.
        tokio::time::sleep(Duration::from_millis(200)).await;
        spawned_elsewhere(&store, "much-later");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while api.agent("much-later").await.is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop stopped scanning after its first pass"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        task.abort();
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
                system: None,
                cwd: PathBuf::from("/tmp"),
                model: None,
                permission: PermissionPolicy::Ask,
                resume: crate::harness::Resume::Fresh,
                tools: None,
                ..SpawnRequest::default()
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
                system: None,
                cwd: PathBuf::from("/tmp"),
                model: None,
                permission: PermissionPolicy::Ask,
                resume: crate::harness::Resume::Fresh,
                tools: None,
                ..SpawnRequest::default()
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

    // ---- where a run is pointed -------------------------------------------

    /// A real directory, canonicalised — `std::env::temp_dir()` is a symlink on
    /// macOS and this whole area is path comparison.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jod-settle-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        crate::roots::normalise(&dir)
    }

    /// BUG-14 through the store, with nothing pre-arranged: a conversation, a
    /// root added exactly as `/add-dir` adds one, and a request that names that
    /// directory the way a person does — by its name alone.
    ///
    /// The answer is the directory the user added. It is not `$HOME/tetris`,
    /// and the home directory is never consulted to find that out.
    #[test]
    fn a_bare_directory_name_is_resolved_to_the_root_the_user_added() {
        let dir = scratch("added");
        let tetris = dir.join("dogfood").join("tetris");
        std::fs::create_dir_all(&tetris).unwrap();

        let store = Store::in_memory().unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &dir.to_string_lossy(), None)
            .unwrap();
        store
            .add_root(&conversation.id, crate::roots::NewRoot::reading(&tetris))
            .unwrap();

        let mut req = SpawnRequest {
            cwd: PathBuf::from("tetris"),
            ..request("build a tetris game in the tetris directory")
        };
        settle_cwd(
            &store,
            &mut req,
            &RunConversation::Existing(conversation.id.clone()),
        )
        .expect("a name that is one of this session's directories resolves");

        assert_eq!(req.cwd, tetris);
    }

    /// The refusal. `tetris` names nothing this session was pointed at, so
    /// there is no honest answer — and the answer that shipped was
    /// `$HOME/tetris`, written to silently while the run reported success.
    ///
    /// A card, because the status bar is where the last one of these went
    /// unnoticed.
    #[test]
    fn a_bare_name_matching_no_root_is_refused_and_raises_a_blocking_card() {
        let dir = scratch("unmatched");
        let checkout = dir.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();

        let store = Store::in_memory().unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &dir.to_string_lossy(), None)
            .unwrap();
        store
            .add_root(&conversation.id, crate::roots::NewRoot::reading(&checkout))
            .unwrap();

        let mut req = SpawnRequest {
            cwd: PathBuf::from("tetris"),
            ..request("build a tetris game in the tetris directory")
        };
        let refused = settle_cwd(
            &store,
            &mut req,
            &RunConversation::Existing(conversation.id.clone()),
        );

        assert!(
            matches!(refused, Err(JodError::Invalid(_))),
            "a directory nobody declared must not be guessed at: {refused:?}"
        );
        let home = std::env::var("HOME").unwrap_or_default();
        assert!(
            !home.is_empty() && !req.cwd.starts_with(&home),
            "the guess this replaced was the home directory; cwd is {:?}",
            req.cwd
        );

        let cards = store
            .cards(&crate::cards::Query {
                conversation_id: Some(conversation.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let card = cards
            .first()
            .expect("a refused launch is worth a card, not a line in the status bar");
        assert!(card.blocking, "{card:?}");
        assert!(
            card.body.contains(&checkout.display().to_string()),
            "the card has to say what was on offer: {}",
            card.body
        );
    }

    /// The wiring, which is the part that goes missing.
    ///
    /// A resolver that is correct and never called is this repository's
    /// characteristic failure — `SpawnRequest::roots` is translated into
    /// `--add-dir` by three harness adapters, each with its own passing test,
    /// and no caller has ever filled it. So this asserts the entry point:
    /// `spawn_agent_in` itself refuses, before it goes looking for a harness,
    /// and no run is recorded as having started.
    #[tokio::test]
    async fn spawning_at_a_directory_nobody_declared_is_refused_at_the_entry_point() {
        let dir = scratch("entry-point");
        let checkout = dir.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();

        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &dir.to_string_lossy(), None)
            .unwrap();
        store
            .add_root(&conversation.id, crate::roots::NewRoot::reading(&checkout))
            .unwrap();

        let result = Jod::with_store(store.clone())
            .spawn_agent_in(
                SpawnRequest {
                    cwd: PathBuf::from("tetris"),
                    ..request("build a tetris game in the tetris directory")
                },
                RunConversation::Existing(conversation.id.clone()),
            )
            .await;

        assert!(
            matches!(result, Err(JodError::Invalid(_))),
            "a directory nobody declared must stop the launch, whatever \
             harnesses this machine has: {result:?}"
        );
        assert!(
            store.runs(10).unwrap().is_empty(),
            "nothing may be recorded as running when nothing was launched"
        );
    }

    /// The regression this whole cap exists to fix: 8 agents took a 4-core box
    /// to a load of 60, because nothing on the `spawn_agent_in` path — the one
    /// seam the TUI and the API both funnel through — ever consulted a core
    /// count. With the cap set to 2, a third concurrent spawn must not start a
    /// third process; it must sit queued until one of the first two ends.
    ///
    /// Stands up fake `claude` and `jod-run` binaries under `JOD_CLAUDE_BIN`
    /// and `JOD_SUPERVISOR_BIN` — the same override discovery.rs already
    /// supports for pointing at a real install — so this exercises the actual
    /// launch path (`runner::launch`, `proc::spawn_detached`, a real process
    /// group) rather than asserting a config value is merely read.
    #[tokio::test]
    async fn a_spawn_past_the_cap_queues_instead_of_launching() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = scratch("cap");

        // A `claude` that discovery can find and `runner::launch` can exec.
        let claude_bin = dir.join("claude");
        std::fs::write(&claude_bin, "#!/bin/sh\nexit 0\n").unwrap();
        // A `jod-run` stand-in: it holds its process group open for two
        // seconds, standing in for a harness actually doing work, so a slot
        // it occupies stays occupied long enough for the assertions below to
        // land inside that window rather than racing it.
        let supervisor_bin = dir.join("jod-run");
        std::fs::write(&supervisor_bin, "#!/bin/sh\nsleep 2\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&supervisor_bin, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let saved_home = std::env::var("JOD_HOME").ok();
        let saved_claude = std::env::var("JOD_CLAUDE_BIN").ok();
        let saved_supervisor = std::env::var("JOD_SUPERVISOR_BIN").ok();
        let saved_cap = std::env::var("JOD_MAX_CONCURRENT_AGENTS").ok();
        std::env::set_var("JOD_HOME", &dir);
        std::env::set_var("JOD_CLAUDE_BIN", &claude_bin);
        std::env::set_var("JOD_SUPERVISOR_BIN", &supervisor_bin);
        std::env::set_var("JOD_MAX_CONCURRENT_AGENTS", "2");

        // A file-backed store, not `Store::in_memory()`: `runner::launch`
        // needs a `db_path` to hand the supervisor, and an in-memory store's
        // `path()` is `None` — which would fail every launch before it ever
        // reached a process, and this test would queue nothing because
        // nothing would ever be occupying a slot.
        let store = std::sync::Arc::new(Store::open(&dir.join("jod.db")).unwrap());
        let jod = Jod::with_store(store.clone());

        let first = jod.spawn_agent(request("first")).await;
        let second = jod.spawn_agent(request("second")).await;

        // Restore the environment before any assertion can fail this test —
        // a panic must not leave the rest of the suite pointed at a scratch
        // `JOD_HOME` or a fake harness.
        macro_rules! restore_env {
            () => {
                match saved_home.clone() {
                    Some(v) => std::env::set_var("JOD_HOME", v),
                    None => std::env::remove_var("JOD_HOME"),
                }
                match saved_claude.clone() {
                    Some(v) => std::env::set_var("JOD_CLAUDE_BIN", v),
                    None => std::env::remove_var("JOD_CLAUDE_BIN"),
                }
                match saved_supervisor.clone() {
                    Some(v) => std::env::set_var("JOD_SUPERVISOR_BIN", v),
                    None => std::env::remove_var("JOD_SUPERVISOR_BIN"),
                }
                match saved_cap.clone() {
                    Some(v) => std::env::set_var("JOD_MAX_CONCURRENT_AGENTS", v),
                    None => std::env::remove_var("JOD_MAX_CONCURRENT_AGENTS"),
                }
            };
        }

        let first = match first {
            Ok(a) => a,
            Err(e) => {
                restore_env!();
                panic!("the first spawn, well under the cap, must launch: {e:?}");
            }
        };
        let second = match second {
            Ok(a) => a,
            Err(e) => {
                restore_env!();
                panic!("the second spawn, exactly at the cap, must launch: {e:?}");
            }
        };
        assert!(first.pid.is_some(), "a launched run has a pid");
        assert!(second.pid.is_some(), "a launched run has a pid");

        // The third spawn is past the cap. Race it against a timeout well
        // inside the fake supervisor's 2-second lifetime: if the cap did
        // nothing, `spawn_agent` returns almost immediately and this fails.
        let jod_for_third = jod.clone();
        let mut third_task =
            tokio::spawn(async move { jod_for_third.spawn_agent(request("third")).await });
        let raced =
            tokio::time::timeout(std::time::Duration::from_millis(700), &mut third_task).await;
        if raced.is_ok() {
            restore_env!();
            panic!(
                "a third spawn on a cap of two must still be queued 700ms in, \
                 while both slots are held by a process that sleeps for 2s; \
                 instead it returned: {raced:?}"
            );
        }

        // It must still get its turn once a slot frees — queued, not lost.
        let third =
            tokio::time::timeout(std::time::Duration::from_secs(5), third_task).await;
        restore_env!();
        let third = third
            .expect("the queued spawn must eventually get a slot, not hang forever")
            .expect("the spawning task must not panic")
            .expect("the queued spawn must eventually succeed");
        assert!(third.pid.is_some(), "the queued run must actually launch");
    }

    // ---- runs populate conversations --------------------------------------

    fn request(prompt: &str) -> SpawnRequest {
        SpawnRequest {
            name: "n".into(),
            harness: HarnessKind::ClaudeCode,
            prompt: prompt.into(),
            system: None,
            cwd: PathBuf::from("/work"),
            model: Some("opus".into()),
            permission: PermissionPolicy::Ask,
            resume: crate::harness::Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
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

    /// The row `spawn_agent_in` writes before it launches anything.
    ///
    /// Present in the fixture because it is present in production: a session id
    /// is recorded against the harness that minted it, and the run row is where
    /// `record_in_conversation` reads that from.
    fn launched(store: &Store, run: &str, harness: HarnessKind) {
        store
            .save_run(&crate::store::StoredRun {
                id: run.into(),
                name: "n".into(),
                harness: harness.id().into(),
                status: AgentStatus::Running.as_str().into(),
                cwd: "/work".into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::Value::Null,
            })
            .expect("the run row");
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
        .expect("a run belongs to a conversation")
        .id;

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
        let first = open_conversation(&store, &request("first"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
        let second = open_conversation(
            &store,
            &request("second"),
            "run-2",
            &RunConversation::Existing(first.clone()),
        )
        .unwrap()
        .id;

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
        assert!(store
            .conversation(&first)
            .unwrap()
            .unwrap()
            .forked_from
            .is_none());
    }

    #[test]
    fn a_conversation_that_is_already_named_keeps_the_name_it_has() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("first"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
        store
            .set_conversation_title(&id, "the inbox sweep")
            .unwrap();
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

    /// The failure this closes: `/model` in the TUI set a field passed at spawn
    /// time and nothing persisted it, so resuming a conversation the next day
    /// came back on the client's default. The harness is respawned once per
    /// turn, so "the model I chose" has to be re-answered at every single spawn
    /// — and the conversation row is the only thing that is still around to
    /// answer it.
    #[test]
    fn a_resumed_conversation_comes_back_on_the_model_it_was_left_in() {
        let store = Store::in_memory().unwrap();
        let opened =
            open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();
        store
            .set_conversation_model(&opened.id, Some("sonnet"))
            .unwrap();

        // A later turn, from a caller carrying the default it was built with.
        let mut later = request("and again");
        assert_eq!(later.model.as_deref(), Some("opus"), "the client's default");
        let reopened = open_conversation(
            &store,
            &later,
            "run-2",
            &RunConversation::Existing(opened.id.clone()),
        )
        .unwrap();
        prefer_conversation_settings(&mut later, &reopened);

        assert_eq!(later.model.as_deref(), Some("sonnet"));
    }

    /// Same argument for the permission mode, which before this was fixed once
    /// at `jod tui` launch and could never be changed — not per conversation,
    /// not at all.
    #[test]
    fn a_resumed_conversation_comes_back_in_the_mode_it_was_left_in() {
        let store = Store::in_memory().unwrap();
        let opened =
            open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();
        store
            .set_conversation_permission(&opened.id, Some(PermissionPolicy::Plan))
            .unwrap();

        let mut later = request("and again");
        assert_eq!(later.permission, PermissionPolicy::Ask);
        let reopened = open_conversation(
            &store,
            &later,
            "run-2",
            &RunConversation::Existing(opened.id.clone()),
        )
        .unwrap();
        prefer_conversation_settings(&mut later, &reopened);

        assert_eq!(later.permission, PermissionPolicy::Plan);
        assert!(!later.permission.may_act(), "plan means plan");
    }

    /// `None` is the absence of an opinion, not a value. Every conversation
    /// older than `0011_settings_and_modes` reads back this way, and one of them
    /// resuming must not silently change what the caller asked for.
    #[test]
    fn a_conversation_with_no_opinion_leaves_the_callers_model_and_mode_alone() {
        let store = Store::in_memory().unwrap();
        let mut req = request("go");
        let opened = open_conversation(&store, &req, "run-1", &RunConversation::New).unwrap();
        store.set_conversation_model(&opened.id, None).unwrap();
        let opened = store.conversation(&opened.id).unwrap().unwrap();
        assert_eq!(opened.model, None);
        assert_eq!(opened.permission, None);

        prefer_conversation_settings(&mut req, &opened);

        assert_eq!(req.model.as_deref(), Some("opus"));
        assert_eq!(req.permission, PermissionPolicy::Ask);
    }

    /// A new conversation takes the model it was opened with, so the very next
    /// turn already resumes into the same one without anybody setting it.
    #[test]
    fn a_new_conversation_remembers_the_model_the_run_that_opened_it_used() {
        let store = Store::in_memory().unwrap();
        let opened =
            open_conversation(&store, &request("go"), "run-1", &RunConversation::New).unwrap();
        assert_eq!(opened.model.as_deref(), Some("opus"));

        let mut later = request("again");
        later.model = None;
        prefer_conversation_settings(&mut later, &opened);
        assert_eq!(later.model.as_deref(), Some("opus"));
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
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New)
            .unwrap()
            .id;

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
        assert!(thread[1..]
            .iter()
            .all(|m| m.run_id.as_deref() == Some("run-1")));
    }

    #[test]
    fn the_session_the_harness_reports_lands_on_the_conversation_so_it_can_resume() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
        launched(&store, "run-1", HarnessKind::ClaudeCode);
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
            store.resume_for(&id, HarnessKind::ClaudeCode).unwrap(),
            crate::harness::Resume::Session("sess-1".into())
        );
    }

    /// The bug the pinned main chat died of, as a unit.
    ///
    /// A `/harness agy` switch left the conversation naming AGY and holding an
    /// AGY session id. The console came back up on Claude Code, and because
    /// nothing compared the two, every turn spawned `claude --resume
    /// <agy-session>` — rejected in about a second, zero tokens, no output, and
    /// nothing on screen to say why.
    #[test]
    fn a_session_is_never_offered_to_a_harness_that_did_not_mint_it() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
        launched(&store, "run-1", HarnessKind::Agy);
        record_in_conversation(
            &store,
            &id,
            &envelope(
                "run-1",
                0,
                AgentEvent::Started {
                    session_id: Some("agy-session".into()),
                    model: None,
                },
            ),
        );

        assert_eq!(
            store.resume_for(&id, HarnessKind::Agy).unwrap(),
            crate::harness::Resume::Session("agy-session".into()),
            "the harness that minted it must still get it back"
        );
        assert_eq!(
            store.resume_for(&id, HarnessKind::ClaudeCode).unwrap(),
            crate::harness::Resume::Fresh,
            "an AGY session id was offered to Claude Code, which is the crash"
        );
    }

    /// ...and the thread then accumulates on the new harness rather than
    /// restarting on every turn, which is what a row left naming the old one
    /// would have caused: mismatch, fresh, mismatch, fresh, forever.
    #[test]
    fn a_thread_that_changed_harness_resumes_normally_from_the_next_turn() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
        launched(&store, "run-1", HarnessKind::Agy);
        record_in_conversation(
            &store,
            &id,
            &envelope(
                "run-1",
                0,
                AgentEvent::Started {
                    session_id: Some("agy-session".into()),
                    model: None,
                },
            ),
        );

        // The console is on Claude Code now: the turn starts fresh, and reports
        // a Claude Code session of its own.
        launched(&store, "run-2", HarnessKind::ClaudeCode);
        record_in_conversation(
            &store,
            &id,
            &envelope(
                "run-2",
                0,
                AgentEvent::Started {
                    session_id: Some("claude-session".into()),
                    model: None,
                },
            ),
        );

        assert_eq!(
            store.resume_for(&id, HarnessKind::ClaudeCode).unwrap(),
            crate::harness::Resume::Session("claude-session".into()),
            "the thread started over instead of carrying on"
        );
    }

    /// Two processes may follow one run, and a follower that reconnects
    /// replays from its cursor — so the store dedupes on `(run_id, seq)`. This
    /// is the call site depending on that: the envelope's sequence has to reach
    /// the write, or every restart would double the transcript.
    #[test]
    fn the_same_event_recorded_twice_leaves_one_message() {
        let store = Store::in_memory().unwrap();
        let id = open_conversation(&store, &request("go"), "run-1", &RunConversation::New)
            .unwrap()
            .id;
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
