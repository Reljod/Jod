//! `jod tui` — the full-screen interface.
//!
//! Layout, top to bottom: a scrolling transcript, an input box, a status bar.
//! `Ctrl-F` reveals a panel listing every delegation this process knows about,
//! which is the part that makes this an orchestrator's UI rather than a chat
//! window — Jod's job is watching several agents, not talking to one.
//!
//! That panel is where unattended work is actually managed: it is a cursor over
//! the live fleet, and from it a run can be watched, stopped, resumed or
//! attached to. Sending a prompt with `Ctrl-B` (or `/delegate`) starts an agent
//! that never takes over the screen, so a long job can be left running while the
//! conversation carries on — and its ending arrives as a notice rather than
//! being missed.
//!
//! The terminal is put into raw mode and an alternate screen, and **must** be
//! put back however this function exits. A panic that skips the restore leaves
//! the user with an unusable shell, so the restore is installed as a panic hook
//! as well as run on the normal path.

mod app;
mod command;
mod config;
mod delivery;
// `data` and `ui` are public so `examples/screens.rs` can build the app from
// the real loaders and render it against a `TestBackend`. That example is how
// "the screens show what is in the database" is demonstrated without a TTY, and
// it can only reach these two by name.
pub mod data;
mod graph;
mod keys;
mod diff;
mod fleet;
mod mention;
mod picker;
mod rail;
mod secret;
pub mod sessions;
mod text;
mod todo;
/// Public for `examples/screens.rs`, which compiles this module in by path and
/// renders the traffic log off a real database — the same reason [`data`] and
/// [`ui`] are public. Nothing outside the TUI links against this crate.
pub mod traffic;
mod yank;
pub mod ui;
mod workspace;

pub use app::{short_duration, AgentLine, App, Entry, Overlay, PromptIntent};
pub use workspace::Workspace;

use std::io;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use jod_core::harness::ToolAccess;
use jod_core::schedule::{GoalState, ScheduleState};
use jod_core::service::{AgentStatus, RunConversation};
use jod_core::store::Store;
use jod_core::{AgentEvent, HarnessKind, Jod, Model, PermissionPolicy, Resume, SpawnRequest};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::collections::HashSet;
use std::path::PathBuf;

/// Something the loop has to do that state alone cannot: it needs the service,
/// the store or the clock.
///
/// Key handling and slash commands stay pure by *describing* the work and
/// handing it back, which is what keeps every decision in this file testable
/// without a terminal or a running agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a prompt to the conversation on screen.
    Send(String),
    /// Start an agent that runs without taking over the transcript.
    Delegate(String),
    /// Hand an instruction to the orchestrator and let it decide the shape.
    Orchestrate(String),
    /// Move the conversation to another harness, carrying its context across.
    ///
    /// Not something `apply_slash` can do: it needs a summary, a summary needs a
    /// model, and Jod has no model client — so the first half is a *run* on the
    /// harness being left. See [`begin_crossing`].
    SwitchHarness(HarnessKind),
    /// Summarise this conversation and carry on from the summary, on the same
    /// harness.
    ///
    /// The same shape as [`Action::SwitchHarness`] and for the same reason: a
    /// summary needs a model and Jod has no model client, so the first half is a
    /// run. See [`begin_compaction`].
    Compact,
    /// Write a setting down on the conversation it belongs to.
    ///
    /// The model and the permission mode are not properties of this process, and
    /// holding them here is what made `/model` evaporate on resume — see
    /// [`Setting`].
    ///
    /// Named `Keep` rather than `Remember` because this file already has a
    /// `Remember`, and it means the other thing: writing a *fact* to memory.
    /// Two verbs called remember, one about a preference and one about what
    /// Jod knows, is a collision waiting to be resolved by whoever guesses
    /// wrong first.
    Keep(Setting),
    /// Stop talking into the conversation the chat box was bound to.
    ///
    /// `/new` and `/resume` both move the cursor somewhere the binding does not
    /// follow. Without this, a handed-over conversation would keep collecting
    /// turns that belong to a different thread — see [`Thread::conversation`].
    ///
    /// It is also how you leave the main chat: entering it binds, and this
    /// unbinds, so `/new` is the way back to an ordinary thread.
    NewThread,
    /// Forget the context of the conversation the chat box is bound to, while
    /// staying in it.
    ///
    /// The half of `/clear` that needs the database. Everything else it does is
    /// app state — the screen, the resume cursor, the running cost — but the
    /// main chat takes its resume from the *store* rather than from
    /// `App::resume` (see `hand_to_orchestrator`, which reads
    /// `Store::resume_for`). So clearing at the desk without this wrote nothing
    /// down, and the next message picked the whole history back up.
    ///
    /// Bound to `Thread::conversation` and deliberately not to whatever run is
    /// being watched: `/clear` typed while looking at somebody else's agent
    /// stops looking, and must not reach into that agent's session.
    Clear,
    /// Put the chat box into the main chat.
    ///
    /// The pinned conversation is one of several the TUI can be in, and this is
    /// the verb that goes there — `⏎` on the fleet's top row, or `/main` with
    /// no instruction. Distinct from [`Action::Orchestrate`], which hands over a
    /// single instruction from wherever you already are and leaves you there.
    EnterMain,
    /// Put the chat box into a project's manager conversation.
    ///
    /// `⏎` on a manager row in the fleet, the same movement `EnterMain` is for
    /// the pinned row. Carries the conversation id, which the tree row already
    /// holds — a project id here would make this look it up again.
    EnterManager(String),
    /// Stop an agent and close its tmux session.
    Stop(String),
    /// Run a command this repository offers, in the spelling its harness takes.
    ///
    /// Both fields come from `commands::Discovered::invoke`, which is the one
    /// place that knows Claude Code and AGY expand `/name` from the prompt
    /// while OpenCode needs `run --command <name>`. Nothing here branches on
    /// the harness — a second copy of that measurement is how the two would
    /// drift.
    RunCommand {
        prompt: String,
        command: Option<String>,
    },
    /// Put text on the terminal's clipboard.
    ///
    /// An `Action` because it writes an escape sequence to stdout, which the
    /// key handler may not do. What to copy is decided in `yank.rs`, off the
    /// transcript rather than off the screen — the screen has wrapping and
    /// borders in it.
    Yank(String),
    /// End the turn in flight and stay in the conversation.
    ///
    /// The same call as [`Action::Stop`] at the process level — Jod runs one
    /// process per turn, so there is nothing else to end — and a different act
    /// at every level above it. `Stop` is "I am done with this agent"; this is
    /// "not like that, try again". The state that makes it the second one is
    /// set in the key handler, so the session survives whether or not the kill
    /// succeeds.
    Interrupt(String),
    /// Put an agent's output on screen and follow it.
    Watch(String),
    /// Arm or disarm a run's heartbeat — see [`command::Slash::Heartbeat`].
    Heartbeat { id: String, on: bool },
    /// Say how to attach to an agent's tmux session.
    Attach(String),
    /// Put a task on the watched team's board.
    AddTask(String),
    /// Mark a task on that board finished.
    FinishTask(String),
    /// Bring a schedule's next instant forward so the ordinary tick fires it.
    RunSchedule(String),
    /// Pause a schedule that is armed, or arm one that is stopped.
    ToggleSchedule(String),
    DeleteSchedule(String),
    /// Watch the run a schedule's most recent fire started.
    OpenScheduleRun(String),
    /// The same three, for a goal's iteration loop.
    RunGoal(String),
    ToggleGoal(String),
    DeleteGoal(String),
    /// Disable a webhook rule that is enabled, or enable one that is not.
    ToggleHook(String),
    DeleteHook(String),
    /// Destroy everything Jod believes about one subject.
    Forget(String),
    /// Assert one fact — subject, relation, value.
    Remember {
        subject: String,
        predicate: String,
        object: String,
    },
    /// Read or change a preference that outlives the session.
    ///
    /// Parsed and checked before it gets here, so this only ever carries
    /// something the store can be asked for; the writing needs the database and
    /// so belongs to the loop.
    Config(config::Request),
    /// List, open, rewind, restore or fork a conversation in Jod's own message
    /// graph. The screens hold no store, so the whole verb travels as data and
    /// `sessions::apply` runs it.
    Sessions(sessions::Request),
    /// Open the typed line in `$EDITOR`. The TUI has to be suspended and
    /// restored around it, which only the loop can do.
    Editor,
    /// Sign in to a harness, through the harness's own flow.
    ///
    /// Travels to the loop for the same reason `$EDITOR` does, and it is the
    /// same discipline: the flow prints a URL and waits for a code, so it
    /// needs the real terminal rather than a screen Jod is drawing over.
    SignIn(HarnessKind),
    /// Start dictating, or stop and transcribe what was said.
    ///
    /// Belongs to the loop for the same reason [`Action::Editor`] does: it
    /// owns a child process across turns of the event loop, which no key
    /// handler can hold.
    Dictate,
    /// Throw away the utterance in progress.
    ///
    /// Distinct from [`Action::Dictate`] rather than being the same toggle:
    /// stopping transcribes, and this is the verb for a sentence you started
    /// and do not want. A single key that did both would make the expensive
    /// one the default.
    CancelDictation,
    /// Update the binaries this console is running from, or say what an update
    /// would take.
    ///
    /// Runs as a background job with its output streamed into the transcript,
    /// rather than taking the terminal the way [`Action::Editor`] does. An
    /// update is minutes of `git` and `cargo`, and the console is the thing
    /// you were in the middle of using — freezing it for the duration would
    /// make "keep working while it builds" impossible, which is the whole
    /// reason the console exists.
    Update {
        check: bool,
    },
    /// Install the newest release of those binaries, downloaded prebuilt.
    ///
    /// Runs through the same background-job machinery as [`Action::Update`]
    /// and differs in what it asks for: a verified tarball off the release
    /// rather than a `cargo build`, and the newest release rather than the
    /// newest patch of the installed minor. It is usually seconds instead of
    /// minutes, and it is the only one of the two that works on a box with no
    /// checkout to build from.
    Upgrade {
        check: bool,
    },
    /// Restart this console into whatever `jod` is now on disk.
    ///
    /// The one thing an update cannot do to itself: replacing the file does
    /// not replace the running process. Offered after an update that changed
    /// the binary, and available as `/reload` whenever a build landed some
    /// other way.
    Reload,
    /// Answer a card in the rail: an option chosen by digit, prose typed at
    /// the prompt, or both.
    ///
    /// Never applied here, and that is the design rather than a convenience.
    /// Answering *queues* — [`Store::answer_card`] writes the answer and a
    /// pending delivery in one transaction, and a handler in core decides when
    /// the agent is told. A turn in flight is untouched, because its prompt was
    /// assembled before the answer existed. See decision D2.
    AnswerCard {
        id: i64,
        chosen: Option<String>,
        answer: Option<String>,
    },
    /// Read a card and deliberately not answer it. Queues nothing: an agent
    /// told about a dismissal could not tell it from an answer, and would act
    /// on a decision nobody made.
    DismissCard(i64),
    /// Store a credential collected by a `Secret` card, and answer the card
    /// with its *name*.
    ///
    /// The value rides in a [`secret::Typed`], which cannot print itself — this
    /// enum derives `Debug`, and one diagnostic on a dispatched action would
    /// otherwise be a live credential in a log file. It is written once and
    /// dropped; nothing downstream of `put_secret` ever sees it, and the card's
    /// answer says only that a name was stored.
    PutSecret {
        card: i64,
        name: String,
        scope: jod_core::secrets::Scope,
        /// The work or conversation the scope refers to; empty for global.
        scope_id: String,
        value: secret::Typed,
    },
    /// Give this conversation another directory to work in, read-only.
    ///
    /// Read-only is not a default that could as easily have been the other
    /// way: per D5 a session reads your real checkout and writes only in a
    /// worktree it claims, so a root added by hand is somewhere to read until
    /// something explicitly claims it.
    AddRoot(PathBuf),
    RemoveRoot(PathBuf),
    /// Put a repository in the catalog an unqualified instruction is resolved
    /// against.
    ///
    /// Distinct from [`Action::AddRoot`], which is about *permission* — what
    /// this one conversation may read. A project is about *reference*: it is
    /// what "let's fix this" resolves to, it outlives the conversation, and
    /// until one is listed every instruction has to spell the path out.
    ///
    /// The path is already resolved by the time it gets here — see
    /// `apply_slash`, which refuses one that is not a directory rather than
    /// writing a row nothing will ever match.
    AddProject(PathBuf),
    /// Take a repository out of the working set — `x` on a fleet project row,
    /// or `/project untrack <name>`.
    ///
    /// The two routes know different things, and the difference is the whole
    /// reason both fields are here. The fleet row carries the project's id, so
    /// there is nothing to resolve and nothing to be ambiguous about — the
    /// cursor was on exactly one repository. A typed name carries no id, so it
    /// has to be looked up, and two checkouts called `proj` is the case where
    /// picking one is worse than refusing.
    ///
    /// Collapsing this to a name would have made `x` on a row refuse because
    /// some *other* row shares its name, which is the console arguing with a
    /// finger already pointing at the answer. The name comes along either way,
    /// because every sentence about what happened has to say it.
    UntrackProject {
        id: Option<String>,
        name: String,
    },
    /// Print the catalog, and put it on screen.
    ListProjects,
    /// Print this conversation's roots, in the user's own order, saying which
    /// is writable — the one fact that decides whether an agent may change
    /// anything there.
    ListRoots,
    /// A verb the screens offer and the store cannot carry out yet. Named
    /// rather than silently ignored, and naming the missing call rather than
    /// apologising, so the gap is a to-do and not a mystery.
    Pending {
        verb: String,
        needs: &'static str,
    },
}

/// Which Jod conversation the chat box is talking into, and what the next spawn
/// still owes it.
///
/// Held by the loop rather than by [`App`], which is a deliberate compromise
/// and not a design: it belongs on the app beside `session` and `resume`, but
/// `app.rs` is owned by another track while this lands. Everything here is
/// state the *loop* consumes, so it works where it is; move it when that file
/// is free.
///
/// Note what this is not. `App::session` is the *harness's* conversation id —
/// what `--resume` takes. This is Jod's, which is a different thing: it outlives
/// the harness session, and after a handoff it is the only one of the two that
/// still exists.
#[derive(Debug, Default)]
struct Thread {
    /// The conversation every turn from the chat box is recorded in.
    ///
    /// `None` means "the one the run on screen wrote", which is what
    /// [`Store::conversation_for_run`] answers. It is only set explicitly after
    /// a harness switch, because the conversation that switch minted has no run
    /// yet — nothing to find it by.
    conversation: Option<String>,
    /// Prior context the next spawn has to carry in its system framing.
    ///
    /// Taken once and then dropped. A handoff lands on a harness with no session
    /// of its own, so the *first* turn is the only one that has to bring the
    /// context with it; from the second turn on, the new harness's own session
    /// is carrying it. Leaving it set would re-send a summary the model is
    /// already looking at.
    carried: Option<String>,
    /// A switch or a compaction waiting on the run that is writing its summary.
    summarising: Option<PendingSummary>,
    /// The orchestrator said the main chat is due for compaction, and this turn
    /// has not finished yet.
    ///
    /// A flag rather than a compaction started on the spot, because the answer
    /// to `compaction_due` arrives *with* a reply the user is about to read and
    /// the harness is still mid-turn. Compacting under a running turn would
    /// summarise a thread that is still being written into. So it waits for the
    /// turn to end, which is the natural break the trigger was describing
    /// anyway.
    compaction_owed: bool,
    /// Whether the run on screen is one this chat box started.
    ///
    /// `/watch` puts somebody else's agent on screen and the context bar then
    /// reads *its* usage, because that is the run being applied. Nothing is
    /// wrong with that until a full bar starts doing something: the automatic
    /// compaction would fire on a conversation the user is reading rather than
    /// talking into, and compact a running agent's thread out from under it.
    ///
    /// Cleared by `/watch`, set again by the next turn typed here. It gates the
    /// automatic pass only — `/compact` still acts on whatever is on screen,
    /// the way `/harness` does, because a person typing it can see what they
    /// are pointing at.
    watching_own_turn: bool,
    /// Settings chosen before there was a conversation to write them on.
    ///
    /// A conversation is minted by the first *run*, so `/model opus` typed into
    /// an empty chat box has nowhere to go yet. Dropping it would be the same
    /// bug one layer along; applying it to the app and forgetting to store it is
    /// what this whole change is undoing. So it waits here and is written the
    /// moment the first turn creates something to write it on.
    ///
    /// Applied in order, so choosing twice before typing leaves the second
    /// choice — which is what the user would expect from having chosen it last.
    pending: Vec<Setting>,
    /// Whether the composer is showing `offer_models`'s auto-prefilled
    /// `/model ` line, and nothing has been pressed since it landed.
    ///
    /// `offer_models` cannot tell a chosen suggestion from a prompt that simply
    /// starts with the same characters, because both are just text in
    /// `app.input` by the time a key arrives. This is the one bit that makes
    /// the difference legible: true only for the key immediately after the
    /// offer, so `on_chat_key` can tell "the next thing typed is the offer
    /// being read" from "the next thing typed is a sentence that happens to
    /// begin where the offer left the cursor" — and clear the prefill instead
    /// of typing into it. Consumed by the very next key, of any kind, so it
    /// never survives to misjudge a later, unrelated keystroke.
    model_offer_unread: bool,
}

impl Thread {
    /// Where the next turn's transcript goes.
    ///
    /// `New` unless something bound this chat to a conversation — a handoff, or
    /// entering the main chat. The harness session is what carries an ordinary
    /// conversation forward, and Jod's graph records each turn beside it; after
    /// a switch there is no harness session yet, so the binding is the only
    /// thing holding the thread together.
    fn binding(&self) -> RunConversation {
        match &self.conversation {
            Some(id) => RunConversation::Existing(id.clone()),
            None => RunConversation::New,
        }
    }

    /// Whether the chat box is currently inside the main chat.
    ///
    /// Asked of the store rather than remembered on a flag, because the pin can
    /// move under this process — `/harness` mints a conversation and carries
    /// the pin to it — and a flag set at entry would then be pointing at the
    /// thread that was handed over.
    fn in_main(&self, store: Option<&Store>) -> bool {
        let (Some(store), Some(here)) = (store, self.conversation.as_deref()) else {
            return false;
        };
        matches!(store.pinned_conversation(), Ok(Some(pinned)) if pinned == here)
    }
}

/// Something the user chose that belongs to the conversation rather than to
/// this process.
///
/// Both of these are re-decided at every spawn — Jod respawns the harness once
/// per turn against a resumed session, so `--model` and `--permission-mode` are
/// arguments built afresh each time. A choice kept only in [`App`] therefore
/// lasts until the next `jod tui` and then silently reverts, which is exactly
/// what `/model` did. [`Store::set_conversation_model`] and
/// [`Store::set_conversation_permission`] are the other end;
/// `prefer_conversation_settings` reads them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    /// `None` is a choice too — "whatever the harness picks" — and has to be
    /// storable, or `/model` with no argument could set a model but never unset
    /// one.
    Model(Option<String>),
    Mode(PermissionPolicy),
}

/// Something that has spawned a summariser and is waiting for it.
///
/// One kind of pending work rather than two, because a switch and a compaction
/// are the same wait: a detached run is writing prose, the chat box is held
/// busy, and what finishes when it ends is decided by [`Summarising`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingSummary {
    intent: Summarising,
    /// The run asked to write the summary. It completes when this run finishes,
    /// and only for this run.
    run: String,
    /// The conversation being summarised.
    conversation: String,
}

/// What a summary is being written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Summarising {
    /// Hand the thread to another harness, and continue there.
    Handover(HarnessKind),
    /// Stay on this harness and continue from the summary, so the next turn
    /// resumes nothing and the context starts over.
    Compaction {
        /// Whether a person typed `/compact`, or the context bar reached the
        /// point where it would have nagged.
        ///
        /// Only the wording depends on it, and that is worth a field: a line
        /// that says "compacting" out of nowhere reads as a fault when nobody
        /// asked for it, and reads as an echo when somebody did.
        asked: bool,
    },
}

impl Summarising {
    /// What the run is called in the fleet, and in "already …" refusals.
    fn label(&self) -> String {
        match self {
            Summarising::Handover(to) => format!("summarise for {}", to.id()),
            Summarising::Compaction { .. } => {
                jod_core::works::COMPACTION_RUN_NAME.to_string()
            }
        }
    }
}

/// What `/harness <kind>` turns out to mean, decided before anything is spawned.
///
/// Separated from carrying it out so the decision is testable without a harness
/// on the machine — the part with the interesting cases is this one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Crossing {
    /// Already on that harness. Nothing to do, and nothing to throw away.
    Stay,
    /// Nothing has been said yet, so there is nothing to summarise and no model
    /// call to pay for. The app simply moves.
    Bare,
    /// A thread has to be summarised first, by a run on the harness being left.
    Summarise {
        conversation: String,
        /// The transcript to summarise, when the harness cannot be asked to
        /// read its own — see [`begin_crossing`].
        material: Option<String>,
    },
}

pub struct Options {
    /// The harness asked for on the command line, or `None` when nothing was
    /// asked for — which is what lets a stored preference win without having to
    /// guess whether `claude` was typed or merely defaulted to.
    pub harness: Option<HarnessKind>,
    /// The team to watch, if any. `None` leaves the team panel saying so
    /// rather than showing an empty board.
    pub team: Option<String>,
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// The mode asked for, and — when one was — the ceiling the TUI may not
    /// raise past. `None` means nobody set a ceiling, so the built-in default
    /// applies and a stored preference may override it.
    ///
    /// The distinction earns its `Option`. With a clap default these were the
    /// same value, so `load_preferences` had to guess by comparing against the
    /// default — and the moment that default moved from `ask` to `auto` the
    /// comparison silently stopped matching and every stored mode preference
    /// was ignored. A guess that fails quietly when a constant changes is not
    /// a guess worth keeping.
    pub permission: Option<PermissionPolicy>,
    pub resume: Resume,
}

impl Options {
    /// The harness to open on when no preference is stored either.
    pub fn harness_or_default(&self) -> HarnessKind {
        self.harness.unwrap_or(HarnessKind::ClaudeCode)
    }

    /// The mode to start in when no preference is stored either.
    pub fn mode_or_default(&self) -> PermissionPolicy {
        self.permission.unwrap_or_default()
    }

    /// How far the mode may be raised from inside the program.
    ///
    /// An explicit `--permission` is a ceiling; saying nothing is not. Nobody
    /// who omitted the flag has expressed an opinion to be protected from.
    pub fn ceiling(&self) -> PermissionPolicy {
        self.permission.unwrap_or(PermissionPolicy::Bypass)
    }
}

pub async fn run(jod: Arc<Jod>, opts: Options) -> Result<()> {
    let mut terminal = enter().context("taking over the terminal")?;
    let result = event_loop(&mut terminal, jod, opts).await;
    // Restore before surfacing any error, or the message prints into a raw-mode
    // terminal that mangles it.
    restore();
    result
}

fn enter() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // A panic past this point would otherwise leave the shell in raw mode with
    // no echo — effectively broken until the user blindly types `reset`.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

/// The transcript line a delegation leaves behind.
///
/// The prompt in full rather than `default_name`'s summary, and the directory
/// beside it. This is fire-and-forget spending on an agent nobody is watching:
/// the confirmation is the one moment a person can notice that the run was
/// pointed somewhere they did not intend, and neither a truncated title nor a
/// status-bar count can show that.
fn delegated(id: String, prompt: String, opts: &Options) -> Entry {
    Entry::Delegated {
        id,
        prompt,
        dir: opts.cwd.display().to_string(),
    }
}

/// The line the console opens with.
///
/// A function rather than a literal at the one call site so a test can start an
/// `App` in exactly the state a cold launch leaves it in. A test that pushed
/// its own approximation of this line would still pass on the day the real one
/// stopped being a [`Entry::Hint`] — which is the bug the splash rule turns on.
fn startup_hint() -> Entry {
    // No harness name here: this line is frozen into the scrollback, so naming
    // the harness would leave a stale claim on screen the moment `/harness`
    // switches. The status bar is the one place that tracks it.
    //
    // A hint and not a notice: nobody asked for it, so it must not count as
    // output the splash would be covering up. See `ui::fresh`.
    Entry::Hint(
        "Ctrl-G opens every screen · / for commands · Enter send · Ctrl-B delegate in the background · ? for keys · Ctrl-C quit"
            .to_string(),
    )
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    jod: Arc<Jod>,
    opts: Options,
) -> Result<()> {
    let mut app = App::new(
        opts.harness_or_default(),
        opts.model.clone(),
        opts.resume.clone(),
    );
    // Start where the launch flag put us, or at the built-in default when it
    // said nothing. An explicit flag is also the ceiling — see `bounded` — so
    // starting anywhere else would show a mode the first spawn would clamp.
    app.mode = opts.mode_or_default();
    // ...and then whatever was chosen last time, which may move both. Only
    // where the command line did not insist, which is what `Options`' two
    // `Option`s exist to make knowable.
    //
    // This call is the whole feature. Without it `/config` writes preferences
    // the program never reads back, and every setting silently resets on the
    // next launch — which is exactly what it did until this line was added.
    if let Some(store) = jod.store() {
        load_preferences(&mut app, &store, &opts);
    }
    app.team = opts.team.clone();
    // Where this console is standing. Read by the header band, and granted to
    // the conversation on screen by `ensure_launch_root` below.
    app.cwd = opts.cwd.clone();
    app.now_ms = now_ms();
    app.agents = list_agents(&jod).await;
    refresh_team(&jod, &mut app);
    // Which Jod conversation the chat box is talking into. Starts derived —
    // "the one the run on screen wrote" — and becomes explicit when a harness
    // switch mints a conversation no run has reached yet, or when you enter the
    // main chat.
    //
    // Declared before the first refresh because the rail is read against it: a
    // refresh that ran first would ask for one conversation's cards and then be
    // told it was looking at another.
    let mut thread = Thread::default();
    // Which conversations have already been handed the launch directory. Held
    // by the loop rather than by `App` because it is a record of what this
    // *process* has done, not of anything on screen — and because it is what
    // makes a removed root stay removed: see `ensure_launch_root`.
    let mut granted: HashSet<String> = HashSet::new();
    bind_rail(&jod, &mut app, &thread);
    ensure_launch_root(&jod, &mut app, &mut granted);
    refresh_workspaces(&jod, &mut app);
    app.reconcile();
    // Open *in* the main chat rather than beside it. The binding used to start
    // derived — "the conversation the run on screen wrote" — which on a cold
    // start means the last run this machine happened to make, so the first
    // sentence typed after launch went to whichever agent finished most
    // recently. Being in the main chat is what makes typing an instruction to
    // Jod, and that is what this program is for; anything else is a place you
    // choose to go.
    //
    // Not when `--resume` named a conversation to continue. That flag is an
    // explicit choice of where to be, and this would overrule it.
    if matches!(opts.resume, Resume::Fresh) {
        enter_main(&jod, &mut app, &opts, &mut thread, true).await;
    }
    app.push(startup_hint());

    let mut keys = EventStream::new();
    let mut events = jod.subscribe();
    let mut viewport = 20usize;
    // Where the last frame put the rail, so a click can be resolved against the
    // cards that were actually on screen when it happened.
    let mut hits = ui::RailHits::default();
    // ...and where it put the catalog, for the same reason.
    let mut panel_hits = ui::PanelHits::default();
    // Four frames a second: enough for the spinner to read as motion and for an
    // elapsed counter to look like a clock, cheap enough to be free.
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // What `/model` will offer, and which harness is currently being asked.
    //
    // A channel rather than a call, because asking costs a subprocess and AGY
    // asks the network: doing it where the popup is drawn would freeze the
    // program for as long as the harness takes to answer.
    let (models_tx, mut models_rx) =
        tokio::sync::mpsc::unbounded_channel::<(HarnessKind, Vec<Model>)>();
    let mut asking_models: Option<HarnessKind> = None;
    // `/update`, running as a background job. The channel carries the
    // installer's own output line by line so the transcript shows a build
    // happening rather than a console that has gone quiet.
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<UpdateMsg>();
    // Dictation. The recorder is a child process held across turns of this
    // loop, so it lives here rather than on `App` — the same reason `$EDITOR`
    // is an `Action` and not a key handler. The channel carries the finished
    // transcript back from the upload, which must not block the keyboard: a
    // console frozen for a second per sentence is worse than typing.
    let (voice_tx, mut voice_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<String, String>>();
    // The listening session, while the microphone is on. Held here rather than
    // on `App` because it owns a child process and a read position in the file
    // that process is writing.
    //
    // The engine is resolved once when listening starts and reused for every
    // utterance in the session, so the engine announced on switch-on is the one
    // that transcribes.
    let mut session: Option<(jod_voice_core::Session, crate::voice::Engine)> = None;

    loop {
        terminal.draw(|f| {
            let painted = ui::draw(f, &app);
            viewport = painted.viewport;
            hits = painted.rail;
            panel_hits = painted.panel;
        })?;
        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            // Bias towards keys so the UI stays responsive while an agent
            // floods the channel with output.
            biased;

            Some(Ok(ev)) = keys.next() => {
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // What the rail is asking the store for, before the key
                        // is allowed to change it. A filter, a sort or a stack
                        // toggle has to re-read *now* — the tick that would
                        // eventually catch up is a second away, and a filter
                        // box that lags a second behind the letters is one
                        // nobody trusts. Everything else must not pay for a
                        // query per keystroke, which is what the comparison
                        // buys.
                        let asked = app.rail.query(app.conversation.clone());
                        match on_key(&mut app, &mut thread, key, viewport) {
                            // The editor takes the terminal, so it can only be
                            // done from here — with the same discipline as
                            // `enter`/`restore`, panic hook included.
                            Some(Action::Editor) => edit_in_editor(terminal, &mut app),
                            // Takes the terminal for the same reason, and
                            // gives it back the same way.
                            Some(Action::SignIn(kind)) => sign_in(terminal, &mut app, kind),
                            // Both take something only the loop has: the
                            // terminal, or the job slot the update occupies.
                            Some(Action::Update { check }) => {
                                start_take(&mut app, &update_tx, check, Take::Update);
                            }
                            Some(Action::Upgrade { check }) => {
                                start_take(&mut app, &update_tx, check, Take::Upgrade);
                            }
                            Some(Action::Dictate) => {
                                toggle_listening(&jod, &mut app, &mut session, &voice_tx);
                            }
                            Some(Action::CancelDictation) => {
                                stop_listening(&mut app, &mut session, &voice_tx, true);
                            }
                            Some(Action::Reload) => reload(terminal, &mut app),
                            Some(action) => {
                                // The keypress is acknowledged *before* the
                                // work it asked for. The frame at the top of
                                // this loop was drawn before the key was read,
                                // and `perform` talks to the service — so
                                // without this the screen keeps showing the
                                // state the key just changed, spinner and
                                // elapsed counter frozen, for as long as the
                                // action takes. Stopping a run takes seconds,
                                // and a frozen clock is how a program looks
                                // when it has hung rather than when it is
                                // doing what it was told.
                                terminal.draw(|f| {
                                    let painted = ui::draw(f, &app);
                                    viewport = painted.viewport;
                                    hits = painted.rail;
                                    panel_hits = painted.panel;
                                })?;
                                perform(&jod, &mut app, &opts, &mut thread, action).await
                            }
                            None => {}
                        }
                        if app.rail.query(app.conversation.clone()) != asked {
                            refresh_rail(&jod, &mut app);
                        }
                        // Both cheap when their overlay is shut, which is
                        // almost always.
                        refresh_mention(&jod, &mut app);
                        refresh_search(&jod, &mut app);
                        refresh_sessions(&jod, &mut app);
                    }
                    // The pointer. The rail claims the events that land on it —
                    // a click answers a card, and the wheel walks the stack or
                    // the expanded card — and everything else scrolls the
                    // transcript as it always has.
                    Event::Mouse(m) => {
                        let asked = app.rail.query(app.conversation.clone());
                        match m.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(action) =
                                    on_click(&mut app, &hits, &panel_hits, m.column, m.row)
                                {
                                    perform(&jod, &mut app, &opts, &mut thread, action).await;
                                }
                            }
                            MouseEventKind::ScrollUp if hits.holds(m.column, m.row) => {
                                on_rail_wheel(&mut app, &hits, -1);
                            }
                            MouseEventKind::ScrollDown if hits.holds(m.column, m.row) => {
                                on_rail_wheel(&mut app, &hits, 1);
                            }
                            // The catalog walks its cursor rather than holding
                            // an offset of its own, for the reason
                            // `on_rail_wheel` gives: the window is derived from
                            // where the cursor is, and a second independent
                            // offset would let the cursor scroll off screen.
                            //
                            // Scrolling does not take the keyboard. A wheel over
                            // a box you are only reading is not a statement that
                            // you want to type into it.
                            MouseEventKind::ScrollUp if panel_hits.holds(m.column, m.row) => {
                                app.step_project(-1);
                            }
                            MouseEventKind::ScrollDown if panel_hits.holds(m.column, m.row) => {
                                app.step_project(1);
                            }
                            MouseEventKind::ScrollUp => app.scroll_up(3, app.transcript.len()),
                            MouseEventKind::ScrollDown => app.scroll_down(3),
                            _ => {}
                        }
                        if app.rail.query(app.conversation.clone()) != asked {
                            refresh_rail(&jod, &mut app);
                        }
                    }
                    // A resize just needs a redraw, which the next loop does.
                    _ => {}
                }
            }

            Ok(envelope) = events.recv() => {
                let watched = app.watching.as_deref() == Some(envelope.agent_id.as_str());
                let finished = matches!(envelope.event, AgentEvent::Finished { .. });
                if watched {
                    app.apply(&envelope.event);
                }
                let switching = thread
                    .summarising
                    .as_ref()
                    .is_some_and(|s| s.run == envelope.agent_id);
                if finished {
                    app.agents = list_agents(&jod).await;
                    refresh_team(&jod, &mut app);
                    refresh_workspaces(&jod, &mut app);
                    app.reconcile();
                    if switching {
                        // The other half of `/harness` and of `/compact`. It is
                        // not `announce`d as a finished delegation, because from
                        // the user's side this was never an agent they started —
                        // it is the thing they asked for, arriving.
                        let pending = thread.summarising.take().expect("just checked");
                        let summary = match jod.events_since(&pending.run, None).await {
                            Ok(events) => said(&events),
                            Err(e) => {
                                app.push(Entry::Notice(format!(
                                    "could not read the summary back, so nothing was \
                                     changed: {e}"
                                )));
                                String::new()
                            }
                        };
                        match jod.store() {
                            Some(store) => {
                                finish_summary(store, &mut app, &mut thread, &pending, &summary)
                            }
                            None => app.push(Entry::Notice(
                                "the database went away mid-summary, so nothing was changed"
                                    .into(),
                            )),
                        }
                        app.busy = false;
                        app.turn_started_ms = None;
                    }
                    if watched || switching {
                        // A prompt typed mid-turn goes now rather than being
                        // refused earlier and forgotten. Mid-*switch* counts:
                        // the queue is why the switch could take the screen at
                        // all without losing what was typed over it.
                        if let Some(next) = app.next_queued() {
                            perform(&jod, &mut app, &opts, &mut thread, Action::Send(next)).await;
                        } else {
                            // Nothing waiting, so the screen is genuinely idle:
                            // the natural break a compaction wants. Nothing is
                            // mid-thought, and the usage the harness just
                            // reported is the freshest reading of how full the
                            // window is. After the queue rather than before it,
                            // so a compaction cannot start underneath a prompt
                            // that is about to be sent.
                            maybe_compact(&jod, &mut app, &opts, &mut thread).await;
                        }
                    } else {
                        announce(&mut app, &envelope.agent_id);
                    }
                }
            }

            // A harness answered. Kept only if it is still the harness on
            // screen: `/harness` may have moved on while it was thinking, and
            // one harness's model names are not another's.
            Some(msg) = update_rx.recv() => {
                match msg {
                    // Into the transcript *and* onto the job, which are two
                    // different questions: "what is it doing right now" is the
                    // job's last line, and "what did it do" is scrollback that
                    // is still readable an hour later.
                    UpdateMsg::Line { job, line } => {
                        app.job_line(job, &line);
                        app.push(Entry::Notice(line));
                    }
                    UpdateMsg::Done { job, said, ok, replaced } => {
                        app.job_done(job, ok, app.now_ms);
                        app.push(Entry::Notice(said));
                        // Asked, never taken. Reloading throws away the screen
                        // you are looking at, and the moment an update lands is
                        // not automatically the moment to lose it.
                        if replaced {
                            app.overlay = Overlay::ConfirmReload;
                        }
                    }
                }
            }

            Some(said) = voice_rx.recv() => {
                app.dictation.note_pending(-1);
                match said {
                    Ok(text) => {
                        if let Some(instruction) = heard_utterance(&mut app, &text) {
                            // The same function `⏎` reaches, so a spoken send
                            // routes to the orchestrator or to the watched
                            // agent by exactly the rule a typed one does.
                            // A private path here would be a second answer to
                            // "where does a prompt go".
                            send_turn(&jod, &mut app, &opts, &mut thread, instruction, None).await;
                        }
                    }
                    Err(why) => app.push(Entry::Notice(why)),
                }
                // A spoken "stop listening" is the one command that has to
                // reach the recorder, which only this loop holds.
                if app.stop_listening_requested {
                    app.stop_listening_requested = false;
                    stop_listening(&mut app, &mut session, &voice_tx, false);
                }
            }

            Some((kind, models)) = models_rx.recv() => {
                if app.harness == kind {
                    app.models = models;
                    app.models_for = Some(kind);
                }
                if asking_models == Some(kind) {
                    asking_models = None;
                }
            }

            _ = ticks.tick() => {
                app.advance(now_ms());
                // The microphone, four times a second. Cheap when the room is
                // quiet: a file length check and a short read, with no model
                // involved until a sentence has actually ended.
                poll_listening(&mut app, &mut session, &voice_tx);
                // Every tick rather than at startup only, because `/harness`
                // changes the answer. `ask_models` returns immediately when the
                // list on hand already belongs to the current harness, which is
                // the usual case.
                ask_models(&app, &mut asking_models, &models_tx);
                // Statuses change in other processes as well as this one, and a
                // panel that only refreshes when the watched agent finishes
                // shows a fleet that stopped moving minutes ago.
                if app.tick.is_multiple_of(4) {
                    // Read the store back first, or the refresh below is only
                    // ever about runs *this* process started.
                    //
                    // `Jod::agents` reads an in-memory map, and every agent a
                    // project manager starts is spawned by an MCP server in
                    // another process. `rehydrate` at launch is what put the
                    // fleet's runs there, and nothing called it again — so a
                    // manager's engineer appeared in the tree, which is built
                    // from SQL, and was absent from the agent list, which is
                    // not. `selected_agent` therefore answered `None` for a row
                    // visibly spinning, and every run verb on it — `s`, `r`,
                    // `a`, `d`, and the thread keys — refused with "that row is
                    // a session with nothing running on it". There was no way
                    // to stop an agent a manager had started.
                    //
                    // Cheap to repeat by design: `rehydrate` checks the map
                    // before replaying anything, and its own comment says the
                    // check is there because a full replay would be "ruinous on
                    // a two-second timer". Only genuinely new runs cost.
                    refresh_fleet(&jod, &mut app).await;
                    // Before the refresh, because the chat box may have been
                    // rebound since the last one — `/resume`, a harness switch,
                    // entering the main chat — and the rail would otherwise
                    // spend a second showing the previous conversation's cards.
                    bind_rail(&jod, &mut app, &thread);
                    // ...and after it, because a rebind is exactly when a
                    // conversation that has never been told where this console
                    // is standing arrives on screen. Nothing happens on the
                    // ticks in between — it is a lookup in a set.
                    ensure_launch_root(&jod, &mut app, &mut granted);
                    refresh_workspaces(&jod, &mut app);
                    if matches!(app.workspace, Workspace::Team | Workspace::Tasks) {
                        refresh_team(&jod, &mut app);
                    }
                    app.reconcile();
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Ask the current harness what models it accepts, unless that is already known
/// or already being asked.
///
/// Blocking rather than async: `HarnessKind::models` runs a child process and
/// waits on it, which is exactly what must not happen on the runtime's own
/// threads while a turn is streaming.
fn ask_models(
    app: &App,
    asking: &mut Option<HarnessKind>,
    tx: &tokio::sync::mpsc::UnboundedSender<(HarnessKind, Vec<Model>)>,
) {
    let kind = app.harness;
    if app.models_for == Some(kind) || *asking == Some(kind) {
        return;
    }
    *asking = Some(kind);
    let tx = tx.clone();
    // The result may arrive after the program has quit, or after another
    // harness has been chosen. Both are fine: the send fails or the receiver
    // drops it, and neither is worth reporting to somebody who has moved on.
    tokio::task::spawn_blocking(move || {
        let _ = tx.send((kind, kind.models()));
    });
}

/// Say that a background agent ended, and how it went.
///
/// The whole point of delegating is not to watch, so the ending has to come and
/// find you. Naming the agent and how to open it makes the notice actionable
/// rather than merely informative.
fn announce(app: &mut App, id: &str) {
    let Some(agent) = app.agents.iter().find(|a| a.id == id) else {
        return;
    };
    let mark = match agent.status.as_str() {
        "completed" => "✓",
        "killed" => "■",
        _ => "✗",
    };
    let took = app::short_duration(app.now_ms.saturating_sub(agent.created_at_ms));
    app.push(Entry::Notice(format!(
        "{mark} {} {} after {took} — Ctrl-F to open it",
        agent.name, agent.status
    )));
}

/// Put the chat box into the main chat, and the main chat on the screen.
///
/// This is the verb the pinned chat never had. It was reachable only by
/// *sending* to it — `jod main "…"`, `/main <instruction>` — and readable only
/// as a static dump, so the one conversation that never ends was the one you
/// could not sit in. Binding is the whole of it: from here `Action::Send` sees
/// `Thread::in_main` and routes through the orchestrator, and `/new` unbinds.
///
/// The transcript is replaced rather than appended to, which is the opposite of
/// what a turn does and right for a move: you are going somewhere else, and
/// leaving the last conversation's lines above the new one's would read as one
/// thread that changed its mind.
///
/// `at_launch` is the one difference between the move and the starting
/// position: on a cold start there is nothing to move *from*, so a chat nobody
/// has said anything to must leave the transcript untouched. [`fresh`] shows
/// the splash only while the transcript holds nothing but hints, and
/// [`replay`]'s empty-state line would replace the wordmark with a sentence
/// saying the screen is blank — on every launch, forever.
async fn enter_main(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    at_launch: bool,
) {
    let Some(store) = jod.store() else {
        // Silent at launch: `jod tui` with no store says so in its own words
        // already, and a second line about the main chat in particular is one
        // consequence of a fault whose cause is elsewhere.
        if !at_launch {
            app.push(Entry::Notice(format!("{NO_STORE} — there is no main chat")));
        }
        return;
    };
    let id = match store.main_conversation(app.harness, &opts.cwd.display().to_string()) {
        Ok(id) => id,
        Err(e) => {
            app.push(Entry::Notice(format!("could not open the main chat: {e}")));
            return;
        }
    };
    enter_conversation(&store, app, thread, &id, "the main chat", at_launch);
}

/// Move the screen to one conversation and bind the chat box to it.
///
/// What `⏎` on the pinned row has always done, with the conversation as an
/// argument — because a manager row does exactly the same thing to a different
/// conversation, and the second copy of this is where the two would drift.
///
/// `what` names the conversation in the two sentences this can produce, so
/// "already in the main chat" and "already in tetris's manager" come out of one
/// function rather than two.
fn enter_conversation(
    store: &Store,
    app: &mut App,
    thread: &mut Thread,
    id: &str,
    what: &str,
    at_launch: bool,
) {
    // "Already there" is bound *and* looking at it, not merely bound. The chat
    // box stays bound while you walk the fleet or watch somebody's run — and
    // since the console launches in the main chat, bound is the ordinary state
    // rather than the rare one. Tested on the binding alone, `⏎` on the fleet's
    // row would answer "already there" from a screen that is plainly not it,
    // which is the dead key that row exists to stop being.
    if thread.conversation.as_deref() == Some(id)
        && app.workspace == Workspace::Chat
        && app.watching.is_none()
    {
        app.push(Entry::Notice(format!("already in {what}")));
        return;
    }
    thread.conversation = Some(id.to_string());
    // Not carried. `carried` is a summary owed to a harness that has never seen
    // this thread, and it belongs to the conversation being *left* — sending it
    // in here would paste another conversation's history into this one.
    thread.carried = None;
    // Stop following whatever run was on screen. The transcript is about to be
    // replaced with this conversation's, and a run still being watched would
    // append its next event into the middle of somebody else's thread.
    app.watching = None;
    app.go(Workspace::Chat);
    app.transcript.clear();

    // Replayed from Jod's own record rather than from a run's events: the chat
    // spans many runs, one per instruction, and any single one of them holds
    // only a slice.
    match store.live_window(id) {
        Ok(live) if at_launch && live.is_empty() => {}
        Ok(live) => {
            for entry in replay(&live, what, app.show_thinking) {
                app.push(entry);
            }
        }
        Err(e) => app.push(Entry::Notice(format!(
            "in {what}, but could not read it back: {e}"
        ))),
    }
    app.scroll_to_bottom();
}

/// A conversation's live window as transcript entries.
///
/// Pure, so what the screen shows on entering is testable without a service.
///
/// `live_window` and not `thread`: it is what the harness would actually be
/// sent, which is the honest thing to put on screen. A message compacted out of
/// it is still on disk and deliberately not here — showing it would suggest the
/// model can see something it cannot.
///
/// Every role gets the entry the live stream would have given it, and that is
/// the whole point of the arms below. A stored tool call is a *truncated
/// rendering of its arguments* — `{"command":"mkdir -p …","description":"…"}` —
/// and a stored result is the harness's JSON content array. Replayed as
/// `Entry::Agent` they were painted as the agent's own prose, so a conversation
/// that folded its steps away while it ran came back on re-entry as pages of
/// JSON that `Ctrl-O` had no say over: `hidden` folds `Tool` and `ToolOut`, and
/// those entries were no longer either one. Classifying here is what makes
/// reading a chat back the same act as watching it happen.
fn replay(
    live: &[jod_core::conversation::Message],
    what: &str,
    show_thinking: bool,
) -> Vec<Entry> {
    use jod_core::conversation::{Message, Role};
    if live.is_empty() {
        return vec![Entry::Notice(format!(
            "{what} — nothing said yet. Type an instruction; it decides who does the work."
        ))];
    }
    let mut entries: Vec<Entry> = Vec::new();
    for message in live
        .iter()
        .filter(|message| show_thinking || message.role != Role::Thinking)
    {
        match message.role {
            Role::User => entries.push(Entry::You(message.text.clone())),
            // Reasoning replays as reasoning. Folded into `Agent` it was
            // rendered as the chat's own words, so re-entering the chat turned
            // a model muttering to itself into something it had said to you —
            // and the same text read differently live and on the way back.
            Role::Thinking => entries.push(Entry::Thinking(message.text.clone())),
            Role::Assistant => entries.push(Entry::Agent(message.text.clone())),
            // A runner error or a note Jod injected. It is not the agent
            // speaking, and `apply` already gives the same event a notice.
            Role::System => entries.push(Entry::Notice(message.text.clone())),
            Role::ToolCall => replay_call(&mut entries, message),
            Role::ToolResult => replay_result(&mut entries, message),
        }
    }
    // The count is the live window's, not the conversation's, and the line says
    // "live" so the two are not confused: a chat with a hundred messages and
    // four live ones is a chat that was compacted, not one that lost anything.
    entries.push(Entry::Notice(format!(
        "{what} · {} in the live window — /new leaves it",
        match live.len() {
            1 => "1 message".to_string(),
            n => format!("{n} messages"),
        }
    )));
    return entries;

    /// One stored tool call, as the same entries `AgentEvent::ToolCall` makes.
    fn replay_call(entries: &mut Vec<Entry>, message: &Message) {
        // A harness that named no tool still called one, and `⚙ tool · <args>`
        // is a step a reader can fold. Dropping the message instead would lose
        // a call the live view had shown.
        let name = message.tool_name.clone().unwrap_or_else(|| "tool".into());
        // A todo call *is* the plan block and gets no line of its own, exactly
        // as while it ran — otherwise a turn that revised its plan a dozen
        // times replays as a dozen `⚙ TodoWrite` rows.
        if let Some(plan) = message
            .tool_input
            .as_ref()
            .and_then(|i| todo::from_tool(&name, i))
        {
            if !app::replace_plan(entries, &plan) {
                entries.push(Entry::Plan(plan));
            }
            return;
        }
        entries.push(Entry::Tool {
            name: name.clone(),
            detail: message.tool_input.as_ref().and_then(app::tool_detail),
            step: app::Step::Running,
        });
        if let Some(edit) = message
            .tool_input
            .as_ref()
            .and_then(|i| diff::from_tool(&name, i))
        {
            entries.push(Entry::Diff {
                edit,
                step: app::Step::Running,
            });
        }
        // The same shell-written files the live path shows — see
        // `diff::from_shell`. Without it a conversation reopened from the store
        // loses every file an agent wrote with a heredoc, which is all of them
        // for an agent that never used the edit tool.
        for edit in message
            .tool_input
            .as_ref()
            .map(|i| diff::from_shell(&name, i))
            .unwrap_or_default()
        {
            entries.push(Entry::Diff {
                edit,
                step: app::Step::Running,
            });
        }
    }

    /// One stored tool result, as the same entries `AgentEvent::ToolResult` makes.
    ///
    /// Unlike the live arm this pushes the output whether or not details are
    /// on. Live, that setting decides what is *recorded*, because an event not
    /// kept then is gone; here the entry already exists on disk, so `hidden`
    /// can decide what is drawn and `Ctrl-O` still has something to open.
    fn replay_result(entries: &mut Vec<Entry>, message: &Message) {
        // `0006_conversations` has no column for it, so the flag rides along in
        // `tool_input` — see `Message::tool_input`.
        let failed = message
            .tool_input
            .as_ref()
            .and_then(|i| i.get("is_error"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let name = message.tool_name.clone().unwrap_or_else(|| "tool".into());
        // The call this result answers is finished, and says so on its own
        // line — including the failure, which is why no second bare `✗ Bash`
        // is pushed under it any more.
        let settled = app::settle_in(entries, &name, failed);
        // A result needs a call line above it when none was announced: a fast
        // OpenCode tool reports only its completion, and a bare `└ Wrote file
        // successfully.` is an answer with its question missing.
        if !settled && (failed || !app::announced_in(entries, &name)) {
            entries.push(Entry::Tool {
                name,
                detail: None,
                step: if failed { app::Step::Failed } else { app::Step::Ok },
            });
        }
        if !message.text.trim().is_empty() {
            entries.push(Entry::ToolOut {
                text: app::first_lines(&message.text, 6),
                failed,
            });
        }
    }
}

/// Hand one typed line to the main chat.
///
/// Every line the chat box sends arrives here — a plain turn and `/main` alike
/// — because since the TUI holds one conversation they are the same act. It
/// goes straight through [`crate::hand_to_orchestrator`], the call `jod main`
/// and the Telegram bridge also make, rather than a TUI-shaped copy of it: which
/// conversation, which tools and which permission mode are decisions with four
/// bugs already behind them (`tests/e2e/main-chat/REPORT.md`), and a second copy
/// would be a second place for the fifth to hide.
///
/// Deliberately not `watch()`, which the `/main` verb used to call. `watch`
/// clears the transcript before replaying a run, which is right when you are
/// moving your eye to a *different* agent and wrong for every turn of the chat
/// you are already in — it would wipe the conversation once per message. The
/// three lines it is reduced to here are what make the pinned chat read as one
/// continuous thread: the run is watched, so its events stream into the
/// transcript already on screen.
async fn orchestrate(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    instruction: String,
) {
    // On screen before the await. Handing over talks to a harness process, and
    // a line that appears only once that returns reads as a dropped keystroke.
    app.push(Entry::You(instruction.clone()));
    app.scroll_to_bottom();

    // Cloned rather than taken, so a hand-over that fails leaves the summary
    // still owed to the next attempt. A switch whose context evaporated because
    // the harness was briefly unreachable is the worst of both endings.
    let carried = thread.carried.clone();
    match crate::hand_to_orchestrator(
        jod,
        &instruction,
        app.harness,
        opts.cwd.clone(),
        carried,
        "main",
        // The mode on the status bar, and the whole point of it being there.
        // It used to stop at this call: the chat showed `auto`, the
        // orchestrator ran in `accept_edits`, and so did everything it opened.
        bounded(opts.ceiling(), app.mode),
    )
    .await
    {
        Ok(handed) => {
            // Once. From here the harness has a session of its own and is
            // holding the context itself; re-sending it every turn would hand
            // the model a summary of the conversation it is already in.
            thread.carried = None;
            // Noted rather than announced. This used to print "the main chat is
            // due for compaction (size) — 128400 chars live", which told the
            // user about a problem and left them to fix it with a command that
            // did not exist. It is now a trigger: the compaction runs itself
            // once this turn is over, which is the natural break the `idle`
            // reason was describing in the first place.
            if handed.compaction_due.is_some() {
                thread.compaction_owed = true;
            }
            // Re-asserted from the thing that just did the writing rather than
            // assumed still correct: `hand_to_orchestrator` resolves the pinned
            // conversation itself, and if a switch moved the pin under us this
            // is where the binding catches up.
            if let Some(store) = jod.store() {
                if let Ok(Some(id)) = store.conversation_for_run(&handed.agent.id) {
                    thread.conversation = Some(id);
                }
            }
            // Anything chosen before the first turn now has a conversation to
            // be written on.
            flush_pending(jod, app, thread, &handed.agent.id);
            // Ours, so the context bar is reading this chat's window again.
            thread.watching_own_turn = true;
            app.push(Entry::Routing(format!(
                "→ {} · handed to the orchestrator — it decides where this goes",
                short(&handed.agent.id)
            )));
            // Watched, so the reply lands in the transcript: the whole point of
            // routing through the orchestrator is *which* route it picks, and
            // that arrives as its answer.
            app.begin_turn(handed.agent.id, app.now_ms);
            app.scroll_to_bottom();
        }
        Err(e) => app.push(Entry::Notice(format!("could not reach the main chat: {e}"))),
    }
}

/// Carry out one action against the service.
async fn perform(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    action: Action,
) {
    // The clock the app is already drawing with, rather than a fresh reading:
    // "run now" writes an instant the *tick* then compares against `now`, and a
    // value a hair in the future would sit undue for a quarter of a second.
    let now = app.now_ms;
    match action {
        // Where a typed line goes depends on which conversation you are in, and
        // there is exactly one that is special. Inside the main chat, typing is
        // instructing the orchestrator — that is what being in it means. In any
        // other conversation it is what it always was: a turn to an agent whose
        // answer fills the screen.
        Action::Send(prompt) => send_turn(jod, app, opts, thread, prompt, None).await,
        // A repository's own command. Same turn, one field different — and
        // that field is the whole of D7's measurement: Claude Code and AGY take
        // `/name` in the prompt, OpenCode takes it in a flag.
        Action::RunCommand { prompt, command } => {
            send_turn(jod, app, opts, thread, prompt, command).await
        }
        Action::Orchestrate(instruction) => orchestrate(jod, app, opts, thread, instruction).await,
        Action::EnterMain => enter_main(jod, app, opts, thread, false).await,
        Action::EnterManager(conversation) => match jod.store() {
            None => app.push(Entry::Notice(format!("{NO_STORE} — there are no managers"))),
            Some(store) => {
                // Named by its project rather than by its id, because "already
                // in 7f3a2b1c" is a sentence about a row and "already in
                // tetris's manager" is a sentence about a repository.
                let what = store
                    .current_project(&conversation)
                    .ok()
                    .flatten()
                    .map(|p| format!("{}'s manager", p.name))
                    .unwrap_or_else(|| "that manager".to_string());
                enter_conversation(&store, app, thread, &conversation, &what, false);
            }
        },
        Action::Delegate(prompt) => {
            // Fresh, always: a background job that silently continued the
            // conversation on screen would inherit context nobody asked it to,
            // and two agents writing into one session is not a conversation.
            //
            // Read-only rather than nothing, and rather than the orchestrating
            // set a watched turn gets. Nobody is reading this one's output as
            // it goes, and the thing you least want unattended is an agent that
            // can create more unattended agents — see `ToolAccess::unattended`,
            // whose reasoning this is. Reading is the half that pays for
            // itself: an agent that can see what else is running can decline to
            // duplicate it.
            match spawn(
                jod,
                app,
                opts,
                prompt.clone(),
                Resume::Fresh,
                DELEGATED,
                // Its own conversation, and no context from this one. A
                // background job that silently joined the thread on screen would
                // interleave two agents' turns in one transcript.
                RunConversation::New,
                None,
                None,
            )
            .await
            {
                Ok(id) => {
                    app.push(delegated(id, prompt, opts));
                    app.scroll_to_bottom();
                }
                Err(e) => app.push(Entry::Notice(format!("could not delegate: {e}"))),
            }
        }
        Action::Keep(setting) => {
            // Before the first turn there is no conversation: one is minted by
            // the first *run*, not by opening the program. The choice is already
            // on the app, so the first spawn uses it either way — `spawn` reads
            // `app.model` and `app.mode` — and `open_conversation` creates the
            // conversation with that model. What it cannot do is record the
            // *mode*, so the choice waits and is written the moment there is
            // something to write it on.
            match jod.store() {
                Some(store) => match current_conversation(store, app, thread) {
                    Some(id) => {
                        if let Some(said) = write_setting(store, &id, &setting) {
                            app.push(Entry::Notice(said));
                        }
                    }
                    None => thread.pending.push(setting),
                },
                // Without a database it applies to this session and stops there,
                // which the user should hear once rather than discover tomorrow.
                None => app.push(Entry::Notice(format!(
                    "{NO_STORE} — that applies to this session only"
                ))),
            }
        }
        Action::SwitchHarness(to) => begin_crossing(jod, app, opts, thread, to).await,
        Action::Compact => begin_compaction(jod, app, opts, thread, true).await,
        Action::NewThread => {
            thread.conversation = None;
            thread.carried = None;
        }
        Action::Clear => {
            // No database means there is nothing stored to forget, and the app
            // half of `/clear` has already happened. An ordinary thread is
            // finished either way: it resumes from `app.resume`, which
            // `apply_slash` has just put back to `Fresh`.
            if let Some(store) = jod.store() {
                if let Some(said) = forget_bound_session(store.as_ref(), thread) {
                    app.push(said);
                }
            }
        }
        // The transcript already says the turn ended — `on_chat_key` said so on
        // the keypress. All that is left is ending the process, and a failure
        // here is reported without undoing any of it: the conversation is
        // intact either way, and a harness that outlives its interruption is a
        // supervisor problem rather than a reason to put the user back into a
        // turn they have already abandoned.
        // Straight to stdout, past ratatui: this is a message for the terminal
        // emulator rather than something to draw, and the next frame repaints
        // over anything the draw buffer thought was there.
        //
        // A failure is reported and nothing else — the clipboard cannot be read
        // back, so there is no state to repair, and the notice has already gone
        // out saying what was copied.
        Action::Yank(sequence) => {
            use std::io::Write as _;
            let mut out = io::stdout();
            if out
                .write_all(sequence.as_bytes())
                .and_then(|()| out.flush())
                .is_err()
            {
                app.push(Entry::Notice(
                    "the terminal would not take the clipboard sequence".into(),
                ));
            }
        }
        Action::Interrupt(id) => {
            if let Err(e) = jod.kill_agent(&id).await {
                // Only a stop that genuinely failed reaches here now: a group
                // that had already ended is reported as stopped, because it is.
                // The wording carries the error as it comes — `JodError::Kill`
                // already says "could not stop the agent", and the sentence
                // this used to wrap it in said so a second time, around a
                // message that claimed the agent would not *start*.
                app.push(Entry::Notice(format!(
                    "{e} — it may still be writing; Ctrl-X kills it outright"
                )));
                // Nothing more is coming, so the status bar must stop saying a
                // stop is under way.
                app.interrupting = None;
            }
        }
        Action::Stop(id) => match jod.kill_agent(&id).await {
            Ok(()) => {
                if app.watching.as_deref() == Some(id.as_str()) {
                    app.busy = false;
                    app.turn_started_ms = None;
                }
                app.push(Entry::Notice(format!("stopped {}", short(&id))));
            }
            Err(e) => {
                app.push(Entry::Notice(format!("could not stop {}: {e}", short(&id))));
                // Nothing more is coming for this one, so the status bar must
                // stop saying a stop is under way. Only for the run that
                // failed: another may still be being stopped.
                app.claims_interrupt(&id);
            }
        },
        // Arming this is the opposite gesture to `Action::Watch`: it is what
        // you do to a run you are about to stop looking at. So it says out loud
        // that the sweep lives in the daemon — a heartbeat with nothing
        // sweeping it is a promise that quietly does not hold, and the TUI is
        // exactly where somebody would arm one without a daemon running.
        Action::Heartbeat { id, on } => match jod.store() {
            None => app.push(Entry::Notice("no store — nothing can be watched".into())),
            Some(store) => {
                let said = if on {
                    let hb = jod_core::heartbeat::Heartbeat::starting(
                        &id,
                        jod_core::heartbeat::Watching::Run,
                        app.now_ms,
                    );
                    store.watch_run(&hb).map(|()| {
                        format!(
                            // It used to say "reaped". A session is now marked
                            // and left running, and a message promising a kill
                            // that will not happen is worse than none: it is
                            // what somebody decides not to intervene on.
                            "heartbeat on {} — flagged if silent for {} min, not stopped \
                             (needs `jod daemon`)",
                            short(&id),
                            hb.stall_ms / 60_000
                        )
                    })
                } else {
                    store.unwatch_run(&id).map(|had| match had {
                        true => format!("heartbeat off {}", short(&id)),
                        false => format!("{} was not being watched", short(&id)),
                    })
                };
                app.push(Entry::Notice(match said {
                    Ok(text) => text,
                    Err(e) => format!("could not change the heartbeat on {}: {e}", short(&id)),
                }));
            }
        },
        // The binding follows the eye, like the session cursor does: watching
        // another agent means the next turn continues *that* conversation, and
        // `current_conversation` derives it from the run being watched. Leaving
        // the main chat by looking away is deliberate — you cannot be reading
        // one thread and instructing another.
        Action::Watch(id) => {
            thread.conversation = None;
            thread.carried = None;
            // Somebody else's run from here, so the context bar is reading
            // their window and the automatic compaction must keep its hands off
            // it. See `Thread::watching_own_turn`.
            thread.watching_own_turn = false;
            watch(jod, app, id).await
        }
        Action::Attach(id) => match jod.agent(&id).await {
            Ok(agent) => {
                app.push(Entry::Notice(format!(
                    "from another terminal: {}",
                    agent.watch_command
                )));
            }
            Err(e) => app.push(Entry::Notice(format!("no agent {}: {e}", short(&id)))),
        },
        Action::AddTask(title) => {
            let (Some(team), Some(store)) = (app.team.clone(), jod.store()) else {
                app.push(Entry::Notice(
                    "no team to add to — start with `jod tui --team <name>`".into(),
                ));
                return;
            };
            let id = task_id(&title, &app.tasks);
            match store.add_team_task(&team, &id, &title) {
                Ok(()) => {
                    app.push(Entry::Notice(format!("{id} on {team}'s board")));
                    refresh_team(jod, app);
                }
                Err(e) => app.push(Entry::Notice(format!("could not add the task: {e}"))),
            }
        }
        Action::FinishTask(id) => {
            let Some(store) = jod.store() else {
                return;
            };
            match store.complete_task(&id) {
                Ok(true) => {
                    app.push(Entry::Notice(format!("{id} done")));
                    refresh_team(jod, app);
                }
                Ok(false) => app.push(Entry::Notice(format!("no task {id} on the board"))),
                Err(e) => app.push(Entry::Notice(format!("could not finish {id}: {e}"))),
            }
        }
        // Every verb below is one store call and one sentence about it. The
        // sentence is written by a free function over `&Store` so that what the
        // user is told — including a refusal and including a failure — is
        // testable against `Store::in_memory()` without a terminal.
        Action::RunSchedule(name) => on_store(jod, app, |store| run_schedule(store, &name, now)),
        Action::ToggleSchedule(name) => on_store(jod, app, |store| toggle_schedule(store, &name)),
        Action::DeleteSchedule(name) => on_store(jod, app, |store| delete_schedule(store, &name)),
        Action::RunGoal(name) => on_store(jod, app, |store| run_goal(store, &name, now)),
        Action::ToggleGoal(name) => on_store(jod, app, |store| toggle_goal(store, &name)),
        Action::DeleteGoal(name) => on_store(jod, app, |store| delete_goal(store, &name)),
        Action::ToggleHook(name) => on_store(jod, app, |store| toggle_hook(store, &name)),
        Action::DeleteHook(name) => on_store(jod, app, |store| delete_hook(store, &name)),
        Action::Forget(subject) => on_store(jod, app, |store| forget_about(store, &subject)),
        // Both card verbs go through `on_store` like every other store verb,
        // and the sentence they hand back is the whole feature: `Store::
        // answer_card` writes the answer *and* a pending delivery in one
        // transaction, so what actually happened is "recorded, and the agent
        // will be told when it comes up for air". Saying "answered" alone would
        // be the lie D2 is written against.
        Action::AnswerCard { id, chosen, answer } => on_store(jod, app, |store| {
            answered_card(store, id, chosen.as_deref(), answer.as_deref())
        }),
        // The one place a credential is revealed, and it goes straight to the
        // store. `scope_id` is filled in here rather than in the key handler
        // because only the loop knows which conversation the rail is bound to.
        //
        // Nothing about this arm logs, formats or returns the value: the
        // sentence `on_store` pushes comes from `secret::stored_note`, which
        // takes a `SecretMeta` — a type that cannot reconstruct a value.
        Action::PutSecret {
            card,
            name,
            scope,
            scope_id,
            value,
        } => {
            let scope_id = if scope_id.is_empty() {
                app.conversation.clone().unwrap_or_default()
            } else {
                scope_id
            };
            on_store(jod, app, move |store| {
                stored_secret(store, card, &name, scope, &scope_id, value)
            })
        }
        Action::AddRoot(path) => {
            let conversation = app.conversation.clone();
            on_store(jod, app, move |store| match conversation {
                Some(conversation) => {
                    match store.add_root(&conversation, jod_core::roots::NewRoot::reading(&path)) {
                        // The label rather than the path: `roots` normalises,
                        // so what was typed and what was stored can differ, and
                        // the stored one is the one that will be matched
                        // against later.
                        Ok(root) => format!(
                            "added {} — read-only, as every root is until something claims it",
                            root.path.display()
                        ),
                        Err(e) => format!("{} not added: {e}", path.display()),
                    }
                }
                // Roots hang off a conversation, so there has to be one. Said
                // rather than silently dropped: the picker just spent the
                // user's attention.
                None => "no conversation to add a root to yet — say something first".to_string(),
            })
        }
        Action::RemoveRoot(path) => {
            let conversation = app.conversation.clone();
            on_store(jod, app, move |store| match conversation {
                Some(conversation) => match store.remove_root(&conversation, &path) {
                    Ok(true) => format!("removed {}", path.display()),
                    Ok(false) => format!("{} was not one of this session's roots", path.display()),
                    Err(e) => format!("{} not removed: {e}", path.display()),
                },
                None => NO_STORE.to_string(),
            })
        }
        // Multi-line, like `/config` and `/sessions`: folding a list of roots
        // into one notice is several directories in a paragraph.
        Action::ListRoots => {
            let lines = match (jod.store(), app.conversation.clone()) {
                (Some(store), Some(conversation)) => {
                    let roots = store.roots(&conversation).unwrap_or_default();
                    if roots.is_empty() {
                        vec![
                            "no roots — /add-dir picks one (Ctrl-G d), and `@` says so until there is"
                                .to_string(),
                        ]
                    } else {
                        // Spelled out, as `jod root ls` already does. `ro` was
                        // two letters nothing on the screen explained, and it
                        // is the one fact on the line worth reading: a checkout
                        // is read-only, which is *why* an agent's edits land in
                        // a worktree rather than where you are looking. Someone
                        // who does not know what `ro` means is exactly the
                        // person that surprises.
                        roots
                            .iter()
                            .map(|r| {
                                format!(
                                    "{}  {}  {}",
                                    if r.writable { "writable " } else { "read-only" },
                                    r.label(),
                                    r.path.display()
                                )
                            })
                            .collect()
                    }
                }
                _ => vec!["no conversation yet, so no roots to list".to_string()],
            };
            for line in lines {
                app.push(Entry::Notice(line));
            }
        }
        Action::AddProject(path) => {
            // The catalog itself is the answer, so the box that holds it comes
            // out. A notice alone would be the whole of the feedback, and on a
            // fresh session the transcript is not on screen at all — see
            // `ui::fresh`. The panel is drawn beside every screen, so it is the
            // one channel that shows the new row whatever you are looking at.
            reveal_catalog(app);
            on_store(jod, app, move |store| {
                match store.add_project(jod_core::projects::NewProject::at(&path)) {
                    // What it will answer to, not merely that it worked: the
                    // spoken forms are what an offhand mention is matched
                    // against, and they are derived rather than typed, so this
                    // is the only place you find out what they came out as.
                    Ok(project) => format!(
                        "{} — say {}",
                        project.summary_line(),
                        project.spoken_forms().join(", ")
                    ),
                    Err(e) => format!("{} not catalogued: {e}", path.display()),
                }
            })
        }
        // The catalog comes out for the same reason `AddProject` brings it out:
        // the panel is where the effect is visible, and a row leaving it is as
        // worth seeing as one arriving.
        //
        // No `Overlay::Confirm`. That overlay is titled "this cannot be undone"
        // and means it, and this can: nothing is deleted, the works and
        // transcripts stay, and one command puts the whole subtree back. The
        // notice names that command, which is the friction that fits a
        // reversible verb — see the charter's "reversible by default".
        Action::UntrackProject { id, name } => {
            reveal_catalog(app);
            on_store(jod, app, move |store| {
                // An id means a row was pointed at. Resolving it back through
                // the name would reintroduce exactly the ambiguity the row had
                // already settled.
                let found = match &id {
                    Some(id) => store.project(id).unwrap_or_default().into_iter().collect(),
                    None => store.projects_by_name(&name).unwrap_or_default(),
                };
                match found.as_slice() {
                    [only] if only.state == jod_core::projects::State::Archived => {
                        format!("{} was already untracked — nothing changed", only.name)
                    }
                    [only] => match store
                        .set_project_state(&only.id, jod_core::projects::State::Archived)
                    {
                        Ok(()) => {
                            // The undo names the path when the name is shared,
                            // because otherwise this sentence offers a command
                            // that refuses: two checkouts called `web` both
                            // answer to `web`, and `jod project restore web`
                            // cannot pick between them. A remedy that does not
                            // run is worse than none — it reads as reversible.
                            let shared = store
                                .projects_by_name(&only.name)
                                .map(|found| found.len() > 1)
                                .unwrap_or(false);
                            let handle = if shared {
                                only.path.display().to_string()
                            } else {
                                only.name.clone()
                            };
                            format!(
                                "{} untracked — off the fleet with its works, and out of \
                                 inference. `jod project restore {handle}` puts it back",
                                only.name
                            )
                        }
                        Err(e) => format!("{} not untracked: {e}", only.name),
                    },
                    [] => format!(
                        "no project called `{name}` — `/project ls` says what is catalogued"
                    ),
                    // Untracking either takes a repository off the fleet, so
                    // the tie is broken by the person, not by row order. This
                    // is the trap `jod project archive proj` has: it archives
                    // whichever row came back first and says nothing about the
                    // other. The fleet's `x` never lands here, which is what
                    // makes "go and press it there" a real instruction.
                    several => format!(
                        "`{name}` is the name of {} projects — {}. Press `x` on the one \
                         you mean on the fleet, which knows which row you are on",
                        several.len(),
                        several
                            .iter()
                            .map(|p| p.path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            });
            refresh_workspaces(jod, app);
        }
        // Multi-line, like `/root` and `/sessions`: a catalog folded into one
        // notice is a paragraph of directories.
        Action::ListProjects => {
            reveal_catalog(app);
            let lines = match jod.store() {
                Some(store) => {
                    let projects = store.projects(false).unwrap_or_default();
                    if projects.is_empty() {
                        vec![CATALOG_EMPTY.to_string()]
                    } else {
                        projects.iter().map(|p| p.summary_line()).collect()
                    }
                }
                None => vec![NO_STORE.to_string()],
            };
            for line in lines {
                app.push(Entry::Notice(line));
            }
            refresh_workspaces(jod, app);
        }
        Action::DismissCard(id) => on_store(jod, app, |store| match store.dismiss_card(id) {
            Ok(()) => format!("card #{id} dismissed — the agent is told nothing"),
            Err(e) => format!("card #{id} not dismissed: {e}"),
        }),
        Action::Remember {
            subject,
            predicate,
            object,
        } => on_store(jod, app, |store| {
            remember_fact(store, &subject, &predicate, &object)
        }),
        // The only action that answers in more than one sentence: `/config`
        // with no argument is a table, and folding it into one notice would
        // wrap four preferences into a paragraph.
        Action::Config(request) => {
            let lines = match jod.store() {
                Some(store) => config::apply(store, &request),
                None => vec![format!(
                    "{NO_STORE} — this preference lasts the session only"
                )],
            };
            for line in lines {
                app.push(Entry::Notice(line));
            }
        }
        // The other multi-line answer, for the same reason: a list of
        // conversations folded into one notice is fifty threads in a paragraph.
        Action::Sessions(request) => {
            let lines = match jod.store() {
                Some(store) => sessions::apply(store, &request, now),
                None => vec![NO_STORE.to_string()],
            };
            for line in lines {
                app.push(Entry::Notice(line));
            }
        }
        Action::OpenScheduleRun(name) => {
            let found = match jod.store() {
                Some(store) => last_run_of(store, &name),
                None => Err(NO_STORE.to_string()),
            };
            match found {
                Ok(id) => {
                    app.go(Workspace::Chat);
                    watch(jod, app, id).await;
                }
                Err(said) => app.push(Entry::Notice(said)),
            }
        }
        // Suspending and restoring the terminal is the loop's job, so this is
        // handled there rather than here. Reaching it means the loop did not.
        Action::Editor => app.push(Entry::Notice(
            "no $EDITOR handoff from here — set $EDITOR and try Ctrl-G e in chat".into(),
        )),
        // The sign-in flow takes the terminal, so the same rule applies: only
        // the loop can lend it, and a caller that is not the loop says so
        // rather than starting a flow nobody can see or answer.
        Action::SignIn(kind) => app.push(Entry::Notice(format!(
            "signing in to {} needs the console's own loop — run `jod login {}` at a shell instead",
            kind.label(),
            kind.id().replace('_', "-"),
        ))),
        // Both need something only the loop holds — the job table for one, the
        // terminal for the other. Reached only from a caller that has neither,
        // so they say so rather than pretending to have run.
        Action::Update { .. } => app.push(Entry::Notice(
            "/update runs from the console's own loop — run `jod update` at a shell instead".into(),
        )),
        Action::Upgrade { .. } => app.push(Entry::Notice(
            "/upgrade runs from the console's own loop — run `jod upgrade` at a shell instead"
                .into(),
        )),
        Action::Reload => app.push(Entry::Notice(
            "/reload restarts the console, which only the console can do".into(),
        )),
        // The recorder is a child process the loop owns across turns, so it is
        // handled there for the same reason `Action::Editor` is.
        Action::Dictate | Action::CancelDictation => app.push(Entry::Notice(
            "dictation runs from the console's own loop — press Ctrl-V in chat".into(),
        )),
        // Named rather than silently ignored: a key that appears to do nothing
        // is worse than one that says what it is waiting for.
        Action::Pending { verb, needs } => {
            app.push(Entry::Notice(format!(
                "{verb} — not wired yet: needs {needs}"
            )));
        }
    }
}

/// What every store verb says when there is no database to talk to.
const NO_STORE: &str = "no database is open, so nothing was changed";

/// Run one store verb, say what it did, and re-read the screens.
///
/// The refresh is the point: a row that still says `armed` after `p` reads as a
/// key that did nothing, and the tick that would eventually correct it is up to
/// four seconds away. Errors come back as a sentence rather than as a `Result`,
/// because a locked database must cost the user a notice and not the session —
/// the same discipline `refresh_team` already keeps.
/// What an empty catalog is told, in the transcript's width rather than the
/// panel's. The panel says the same thing in thirty columns — see
/// `ui::CATALOG_REMEDY` — and both name the command, because a remedy the
/// empty state does not name is one you have to already know.
const CATALOG_EMPTY: &str = "no projects — /project add <path> catalogs one, and until one is \
                             listed “let's fix this” has nothing to resolve to";

/// Put the catalog where it can be seen before writing to it.
///
/// Both `/project` verbs answer in the panel, and the panel is shut by default
/// and can be collapsed on top of that. Opening it is not a liberty: the whole
/// complaint is that the catalog is unreachable from the console, and a verb
/// that filled it while leaving it invisible would fix half of that.
fn reveal_catalog(app: &mut App) {
    app.panel = true;
    app.projects_open = true;
}

/// `Ctrl-P`: give the catalog the keyboard, or put it away again.
///
/// One key both ways, for the reason [`rail::RailState::toggle`] gives: the key
/// that opened something is the key people press to close it.
///
/// Focusing rather than merely showing is the other half. The catalog has been
/// on the panel since it was added and there has never been a way to put a
/// cursor in it, so `⏎` on a project — the obvious thing to want, once you can
/// see the list — did not exist.
fn toggle_catalog(app: &mut App) {
    if app.panel && app.projects_open && app.panel_focused {
        app.close_catalog();
        return;
    }
    app.focus_catalog();
}

fn on_store(jod: &Arc<Jod>, app: &mut App, verb: impl FnOnce(&Store) -> String) {
    let said = match jod.store() {
        Some(store) => verb(store),
        None => NO_STORE.to_string(),
    };
    app.push(Entry::Notice(said));
    refresh_workspaces(jod, app);
}

/// Put a run's output on screen and follow it.
///
/// Its own function because two verbs end here: `⏎` on the fleet, and `⏎` on a
/// schedule, which finds the run its last fire started and then wants exactly
/// this.
async fn watch(jod: &Arc<Jod>, app: &mut App, id: String) {
    match jod.events_since(&id, None).await {
        Ok(events) => {
            let running = app.agents.iter().any(|a| a.id == id && a.is_running());
            app.transcript.clear();
            app.watching = Some(id.clone());
            app.busy = running;
            app.turn_started_ms = running
                .then(|| {
                    app.agents
                        .iter()
                        .find(|a| a.id == id)
                        .map(|a| a.created_at_ms)
                })
                .flatten();
            // The session cursor follows the eye: typing next continues the
            // conversation being read, not the one that scrolled away.
            if let Some(session) = app
                .agents
                .iter()
                .find(|a| a.id == id)
                .and_then(|a| a.session.clone())
            {
                app.resume = Resume::Session(session.clone());
                app.session = Some(session);
            }
            app.push(Entry::Notice(format!("watching {}", short(&id))));
            for envelope in events {
                app.apply(&envelope.event);
            }
            app.scroll_to_bottom();
        }
        Err(e) => app.push(Entry::Notice(format!("cannot open {}: {e}", short(&id)))),
    }
}

// ---- switching harness ---------------------------------------------------

/// What the harness being left is asked to write.
///
/// Addressed to a model, so it says what the *next* agent needs rather than
/// asking for a nice summary: a handoff that omits the paths and the commands
/// leaves the receiving agent to rediscover them, which is the whole cost the
/// switch was meant to avoid.
const SUMMARISE: &str = "Summarise this conversation so that an agent who has \
    never seen it can pick the work up. Cover what is being done and why, what \
    has already been decided, what has already been changed — files, commands, \
    results — and what is still open. Be specific: real paths, real names, real \
    commands. Output the summary and nothing else: no preamble, no questions, \
    no offer to continue.";

/// The same, for a harness that has no session to read and must be handed the
/// record in its prompt.
const SUMMARISE_RECORD: &str = "Below is the record of a conversation. \
    Summarise it so that an agent who has never seen it can pick the work up. \
    Cover what is being done and why, what has already been decided, what has \
    already been changed — files, commands, results — and what is still open. \
    Be specific: real paths, real names, real commands. Output the summary and \
    nothing else: no preamble, no questions, no offer to continue.";

/// Why the compaction happened, for the row `Store::compact` writes.
const CROSSING: &str = "harness switch";

/// The same, for a compaction that stayed on the harness it was already on.
///
/// Two words rather than one shared "compaction", because the `compactions`
/// table is the record of why a thread got shorter and "a switch did it" and "it
/// was getting long" are different answers to that.
const COMPACTED: &str = "compact";

/// The conversation the chat box is talking into.
///
/// Explicit after a handoff, because the conversation a handoff mints has no run
/// to be found by. Otherwise it is derived — "the one the run on screen wrote" —
/// which needs no state and cannot go stale.
fn current_conversation(store: &Store, app: &App, thread: &Thread) -> Option<String> {
    if let Some(bound) = &thread.conversation {
        return Some(bound.clone());
    }
    store
        .conversation_for_run(app.watching.as_deref()?)
        .ok()
        .flatten()
}

/// What `/harness <to>` turns out to mean.
///
/// Pure, and separate from carrying it out, because the cases that matter are
/// decided here: whether a model call is owed at all, and whether the harness
/// being left can be asked to summarise what it is already holding.
fn crossing(store: Option<&Store>, app: &App, thread: &Thread, to: HarnessKind) -> Crossing {
    if app.harness == to {
        return Crossing::Stay;
    }
    // No database means no conversation to hand over — the app can still move,
    // it just moves empty-handed. That is the old behaviour, and it is the
    // honest one when there is nothing stored to carry.
    let Some(store) = store else {
        return Crossing::Bare;
    };
    match summarisable(store, app, thread) {
        Some((conversation, material)) => Crossing::Summarise {
            conversation,
            material,
        },
        None => Crossing::Bare,
    }
}

/// The thread a summariser would be asked about, and the record it has to be
/// handed when it cannot read its own.
///
/// `None` when there is nothing to summarise: no conversation on screen, or one
/// that has said nothing. Nothing live is not an error and not a summary — it is
/// a conversation that has said nothing, and summarising it would spend a model
/// call to produce the word "nothing".
///
/// Shared by `/harness` and `/compact` because it is the same question. The two
/// differ in what they do with the answer, not in how they find it.
fn summarisable(store: &Store, app: &App, thread: &Thread) -> Option<(String, Option<String>)> {
    let conversation = current_conversation(store, app, thread)?;
    match store.live_window(&conversation) {
        Ok(live) if !live.is_empty() => {}
        _ => return None,
    }
    // A harness with a session of its own is holding the conversation and can be
    // asked about it directly. One resuming nothing has never seen this thread —
    // it would summarise an empty context and say so — so the record has to
    // travel in the prompt. That is the case immediately after a previous switch
    // or compaction, which is exactly when it would be missed.
    let material = match app.resume {
        Resume::Fresh => store.handoff_text(&conversation).ok(),
        _ => None,
    };
    Some((conversation, material))
}

/// What to say about a target that cannot be handed structure, if it is one.
///
/// Asked of the store rather than decided here, so "which carriers lose
/// something" has one answer and not a copy of it in the UI — today that is AGY
/// alone, and the day a harness grows an import path this line stops warning
/// about it without being edited.
///
/// Said *before* the move: it is the one loss a user can still avoid, by
/// choosing a different target. The compaction's cost is reported after,
/// because by then it is a fact rather than a choice.
fn lossy_warning(store: &Store, conversation: &str, to: HarnessKind) -> Option<String> {
    store
        .handoff(conversation, to)
        .ok()
        .filter(|carrier| carrier.is_lossy())
        .map(|_| {
            format!(
                "{} has no import path, so the context can only travel as prose in the prompt \
                 — tool calls and structure will not survive the crossing",
                to.label()
            )
        })
}

/// Start a harness switch: warn about what it costs, and put the harness being
/// left to work writing the summary that makes it possible.
///
/// Returns without moving the app when a summary is owed. The switch finishes
/// in [`finish_crossing`] when that run ends, which is what keeps the screen
/// alive while a model writes several paragraphs — the alternative was awaiting
/// a run inside `perform`, which freezes the whole interface, keys included.
async fn begin_crossing(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    to: HarnessKind,
) {
    if already_summarising(app, thread) {
        return;
    }
    let store = jod.store();
    match crossing(store.map(Arc::as_ref), app, thread, to) {
        // Said rather than silently done. `/harness claude` on Claude Code used
        // to reset the session cursor and the model — a no-op that quietly threw
        // away the conversation you were in the middle of.
        Crossing::Stay => app.push(Entry::Notice(format!(
            "already on {} — nothing to switch",
            to.label()
        ))),
        Crossing::Bare => {
            point_at(app, thread, to, None, None);
            app.push(Entry::Notice(format!(
                "{} from the next turn — nothing had been said, so there was nothing to carry",
                to.label()
            )));
        }
        Crossing::Summarise {
            conversation,
            material,
        } => {
            // Asked of the store rather than decided here, so there is one
            // answer to "does this carrier lose anything" and not a copy of it
            // in the UI. Before the move, because it is the loss you could still
            // avoid by choosing a different target.
            if let Some(warning) = store.and_then(|s| lossy_warning(s, &conversation, to)) {
                app.push(Entry::Notice(warning));
            }
            let started = spawn_summariser(
                jod,
                app,
                opts,
                thread,
                Summarising::Handover(to),
                conversation,
                material,
            )
            .await;
            if started {
                app.push(Entry::Notice(format!(
                    "summarising this conversation on {} before handing it to {}…",
                    app.harness.label(),
                    to.label()
                )));
                app.scroll_to_bottom();
            }
        }
    }
}

/// Whether a summariser is already running, saying so if it is.
///
/// One at a time. A second `/harness` or `/compact` while the first summariser
/// is still running would overwrite the pending work, and the abandoned run
/// would then finish into something nobody is waiting for.
fn already_summarising(app: &mut App, thread: &Thread) -> bool {
    let Some(under_way) = &thread.summarising else {
        return false;
    };
    let what = match under_way.intent {
        Summarising::Handover(to) => format!("handing this conversation to {}", to.label()),
        Summarising::Compaction { .. } => "compacting this conversation".to_string(),
    };
    app.push(Entry::Notice(format!(
        "already {what} — wait for the summary"
    )));
    true
}

/// Put the harness on screen to work writing a summary of the thread it is
/// holding, and record what that summary is for.
///
/// Answers whether the run actually started. The caller says what it is about to
/// happen *after* that answer, so a spawn that failed is never announced as one
/// that worked.
///
/// The whole reason this is a spawned run rather than an `await` inside
/// `perform` is that a model writing several paragraphs takes long enough to
/// freeze the interface, keys included. It finishes in [`finish_summary`].
async fn spawn_summariser(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    intent: Summarising,
    conversation: String,
    material: Option<String>,
) -> bool {
    let prompt = match &material {
        Some(record) => format!("{SUMMARISE_RECORD}\n\n{record}"),
        None => SUMMARISE.to_string(),
    };
    let request = SpawnRequest {
        name: intent.label(),
        // The harness holding the conversation. For a switch that is the one
        // being *left* — asking the new one to summarise a thread it has never
        // seen is the bug this whole flow exists to fix — and for a compaction
        // there is only the one.
        harness: app.harness,
        prompt,
        system: None,
        cwd: opts.cwd.clone(),
        model: app.model.clone(),
        // Reading and writing prose, nothing else. A summariser that stopped to
        // ask permission would hang a switch nobody is watching the prompt of.
        permission: bounded(opts.ceiling(), PermissionPolicy::Bypass),
        // With a session, it summarises what it is already holding; without one,
        // the record came in the prompt.
        resume: match material {
            Some(_) => Resume::Fresh,
            None => app.resume.clone(),
        },
        // No Jod verbs. This run answers a question, it does not act.
        tools: None,
        ..SpawnRequest::default()
    };
    // Detached: its prompt is a request to summarise, and recording that in the
    // conversation being summarised would put "summarise this" into the
    // transcript a moment before compacting it — and index it for search as
    // though somebody had said it.
    match jod.spawn_agent_in(request, RunConversation::Detached).await {
        Ok(agent) => {
            thread.summarising = Some(PendingSummary {
                intent,
                run: agent.id.clone(),
                conversation,
            });
            // Busy so a turn typed now queues instead of racing the summary onto
            // whichever thread wins.
            app.busy = true;
            app.turn_started_ms = Some(app.now_ms);
            true
        }
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not start the summary, so nothing was changed: {e}"
            )));
            false
        }
    }
}

/// Start a compaction: summarise this conversation so the next turn can carry on
/// from the summary instead of resuming everything said so far.
///
/// `asked` separates `/compact` from the automatic pass. Only the wording
/// depends on it; both do exactly the same thing, which is the point — the
/// automatic one is not a lesser version that skips something.
async fn begin_compaction(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    asked: bool,
) {
    if already_summarising(app, thread) {
        return;
    }
    // No database means no thread to compact. Said plainly rather than
    // pretended: `/clear` is the command that shortens a context without one.
    let Some(store) = jod.store() else {
        if asked {
            app.push(Entry::Notice(
                "no database, so there is no conversation to compact — `/clear` drops the \
                 context instead"
                    .into(),
            ));
        }
        return;
    };
    let Some((conversation, material)) = summarisable(store.as_ref(), app, thread) else {
        if asked {
            app.push(Entry::Notice(
                "nothing has been said yet, so there is nothing to compact".into(),
            ));
        }
        return;
    };
    let intent = Summarising::Compaction { asked };
    if !spawn_summariser(jod, app, opts, thread, intent, conversation, material).await {
        // A spawn that fails costs no model call, but a spawn that keeps
        // failing would put "could not start the summary" on screen after every
        // turn for the rest of the session.
        stop_compacting_on_its_own(app, intent);
        return;
    }
    app.push(Entry::Notice(if asked {
        "summarising this conversation so it can carry on from the summary…".to_string()
    } else {
        // Says why it is happening. A line that announces itself with no cause
        // reads as a fault, and this one is a routine housekeeping pass the
        // user is being told about rather than asked about.
        format!(
            "the context is {}% full — compacting so the next turn starts smaller…",
            (app.context_fraction() * 100.0).round() as u64
        )
    }));
    app.scroll_to_bottom();
}

/// Compact without being asked, when the context has filled far enough to be
/// worth it.
///
/// This is what the context bar's `⚠ compact recommended` used to be. Advice
/// nobody could act on was the worst of both endings: it told the user a problem
/// existed and left them to solve it with a command that did not exist. Since
/// the summary can now be written and carried, the honest reading of the
/// threshold is "do it", not "consider it".
///
/// Two triggers, one action. [`App::should_compact`] watches what the *harness*
/// is holding, which is what runs into a context limit; `compaction_owed`
/// carries the orchestrator's own verdict about the main chat, which is
/// measured on Jod's live window and fires on a long idle as well as on size.
/// Either one is reason enough.
async fn maybe_compact(jod: &Arc<Jod>, app: &mut App, opts: &Options, thread: &mut Thread) {
    if !compaction_is_due(app, thread) {
        return;
    }
    thread.compaction_owed = false;
    begin_compaction(jod, app, opts, thread, false).await;
}

/// Whether to compact without being asked.
///
/// Pure and separate from doing it, because the interesting part is this one and
/// it is the part that must not misfire: an automatic pass spends a model call,
/// and a predicate that answers yes one time too many spends one after every
/// turn for the rest of the session.
fn compaction_is_due(app: &App, thread: &Thread) -> bool {
    // Given up on after a failure. See `App::auto_compact`.
    if !app.auto_compact {
        return false;
    }
    // Never a thread this chat box is only reading. See
    // `Thread::watching_own_turn`.
    if !thread.watching_own_turn {
        return false;
    }
    // Either trigger is reason enough. `should_compact` watches what the
    // harness is holding, which is what runs into a context limit;
    // `compaction_owed` carries the orchestrator's verdict about the main chat,
    // measured on Jod's own live window and fired on a long idle as well as on
    // size.
    if !thread.compaction_owed && !app.should_compact() {
        return false;
    }
    // Never on top of something. A second summariser would overwrite the first,
    // and one started mid-turn would summarise a thread still being written to.
    thread.summarising.is_none() && !app.busy
}

/// Complete whatever a finished summariser was writing for.
///
/// Takes the summary as text rather than reading it out of a run, so the part
/// with the decisions in it is testable without a harness: what happens when the
/// model says nothing, and what happens when the store refuses.
///
/// Every failure path leaves the app exactly where it was. A half-completed
/// switch — new harness, no context — is strictly worse than one that did not
/// happen, because the conversation is still there and the user no longer has a
/// way back to it. The same goes for a compaction: a thread pointed at a fresh
/// session with no summary behind it has simply lost its context.
fn finish_summary(
    store: &Store,
    app: &mut App,
    thread: &mut Thread,
    pending: &PendingSummary,
    said: &str,
) {
    let summary = said.trim();
    // Not fabricated, not defaulted, not "(no summary)". The store treats an
    // empty summary as an error precisely so a thread cannot be compacted into
    // nothing; inventing a placeholder here would walk straight through that
    // guard.
    if summary.is_empty() {
        app.push(Entry::Notice(match pending.intent {
            Summarising::Handover(_) => format!(
                "the summary came back empty, so nothing was handed over — still on {}",
                app.harness.label()
            ),
            Summarising::Compaction { .. } => {
                "the summary came back empty, so nothing was compacted — the conversation \
                 carries on as it was"
                    .to_string()
            }
        }));
        stop_compacting_on_its_own(app, pending.intent);
        return;
    }
    match pending.intent {
        Summarising::Handover(to) => {
            finish_crossing(store, app, thread, &pending.conversation, to, summary)
        }
        Summarising::Compaction { .. } => {
            if !finish_compaction(store, app, thread, &pending.conversation, summary) {
                stop_compacting_on_its_own(app, pending.intent);
            }
        }
    }
}

/// Give up on automatic compaction for the rest of the session.
///
/// Called when a compaction nobody asked for fails. The threshold that started
/// it is met on every turn once it is crossed, so a failure that repeats is a
/// model call after every turn forever — see [`App::auto_compact`]. A person
/// who typed `/compact` gets the failure and nothing else: they can decide
/// whether to try again, which is not a loop.
fn stop_compacting_on_its_own(app: &mut App, intent: Summarising) {
    if !matches!(intent, Summarising::Compaction { asked: false }) || !app.auto_compact {
        return;
    }
    app.auto_compact = false;
    app.push(Entry::Notice(
        "not going to keep trying that on its own — type /compact when you want another go".into(),
    ));
}

/// Move the app onto the harness a finished handover was for.
fn finish_crossing(
    store: &Store,
    app: &mut App,
    thread: &mut Thread,
    conversation: &str,
    to: HarnessKind,
    summary: &str,
) {
    let switch = match store.switch_harness(conversation, to, summary, CROSSING) {
        Ok(switch) => switch,
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not hand the conversation over, so it stays where it is: {e}"
            )));
            return;
        }
    };
    // The summary has to reach the new harness's *prompt*, because the new
    // conversation has no session for it to be resumed into — and nothing in
    // `runner` can stream a transcript into a harness yet. See
    // `Store::handoff_text`.
    let carried = store.handoff_text(&switch.conversation.id).ok();
    let compaction = switch.compaction.clone();
    point_at(app, thread, to, Some(&switch.conversation.id), carried);
    app.push(Entry::Notice(format!(
        "handed over to {} — a new conversation carrying the summary, {}",
        to.label(),
        match &compaction {
            Some(c) => format!(
                "{} chars of transcript became {}",
                c.before_chars, c.after_chars
            ),
            None => "nothing needed compacting".to_string(),
        }
    )));
    app.scroll_to_bottom();
}

/// Carry the thread on from its summary, on the harness it is already on.
///
/// The counterpart of [`finish_crossing`], and deliberately the smaller of the
/// two: nothing about the harness changes, so the model, the model list and the
/// running spend all still apply. What changes is that the next turn resumes
/// nothing — which is the entire point, since resuming the long session is what
/// filled the window.
/// Answers whether it worked, so an automatic pass that failed can stop trying.
fn finish_compaction(
    store: &Store,
    app: &mut App,
    thread: &mut Thread,
    conversation: &str,
    summary: &str,
) -> bool {
    let carried = match store.continue_as_new(conversation, summary, COMPACTED) {
        Ok(carried) => carried,
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not compact, so the conversation carries on as it was: {e}"
            )));
            return false;
        }
    };
    // The summary reaches the next turn through the *prompt*, because the
    // continuation has no session to be resumed into. See `Store::handoff_text`.
    let text = store.handoff_text(&carried.conversation.id).ok();
    let compaction = carried.compaction.clone();
    point_at_continuation(app, thread, &carried.conversation.id, text);
    app.push(Entry::Notice(format!(
        "compacted — {} chars of conversation became {}, and the next turn starts from the \
         summary. Nothing was deleted; the earlier turns are still searchable.",
        compaction.before_chars, compaction.after_chars
    )));
    app.scroll_to_bottom();
    true
}

/// Point the app at the thread that continues this one after a compaction.
///
/// Deliberately narrower than [`point_at`]: the harness has not changed, so
/// dropping the model or the spend the way a switch does would be undoing
/// choices nothing invalidated.
fn point_at_continuation(
    app: &mut App,
    thread: &mut Thread,
    conversation: &str,
    carried: Option<String>,
) {
    // The session is the thing being left behind. Everything else here follows
    // from that.
    app.resume = Resume::Fresh;
    app.session = None;
    // The new session is holding nothing yet and the bar has to say so —
    // otherwise the next automatic pass reads a number left over from the thread
    // that was just compacted away and fires again immediately.
    app.context_tokens = 0;
    thread.conversation = Some(conversation.to_string());
    thread.carried = carried;
}

/// Point the app at another harness, and at the conversation it is now talking
/// into.
///
/// The model is dropped every time and that is not incidental:
/// `claude-sonnet-4-5` means nothing to OpenCode or AGY, so keeping either the
/// requested or the reported name would hand the new harness a model it rejects
/// — and the switch would look like it simply did not work.
fn point_at(
    app: &mut App,
    thread: &mut Thread,
    to: HarnessKind,
    conversation: Option<&str>,
    carried: Option<String>,
) {
    app.harness = to;
    // A harness session id is the old harness's word for a transcript it owns.
    // The new one has never heard of it.
    app.resume = Resume::Fresh;
    app.session = None;
    app.model = None;
    app.reported_model = None;
    // And the list `/model` offers, for the same reason as the model itself:
    // `opencode/claude-opus-5` is not a name AGY accepts, so offering the old
    // harness's models until the new list arrives would be offering names that
    // fail the turn. Cleared rather than replaced — the loader notices the
    // mismatch on the next tick and asks the new harness.
    app.models = Vec::new();
    app.models_for = None;
    // Spend belongs to the conversation being left. Carrying it over showed
    // `$0.11` next to AGY, which had charged nothing.
    app.cost_usd = 0.0;
    // And so does the context reading, for the same reason. It is the last turn
    // as the *old* harness reported it, and the new one is holding nothing yet.
    // Left standing it would put a full bar over an empty session — and now that
    // a full bar starts a compaction, it would start one against a thread with a
    // single seeded turn in it.
    app.context_tokens = 0;
    thread.conversation = conversation.map(str::to_string);
    thread.carried = carried;
    // Set only when the offer actually landed in the box: `offer_models`
    // refuses to clobber a prompt that was already half-typed, and if it did
    // not touch `app.input` there is nothing for the next key to be mistaken
    // for.
    thread.model_offer_unread = offer_models(app, "/model ");
}

/// Put the model picker in front of somebody who has just changed harness.
///
/// The model was dropped two lines ago and the new harness's names look nothing
/// like the old one's, so "what does this one take" is the next question every
/// time. The answer is a list that only the completion popup holds — and the
/// popup opens on what is in the input box. So the offer *is* the prefilled
/// line: `/model ` with the cursor after it, which draws the list without
/// choosing from it. Enter on nothing chosen restores the harness default, which
/// is where the switch had already left things, so ignoring the offer costs
/// nothing.
///
/// Only into an empty box. A prompt half-typed is worth more than a hint, and a
/// switch that finishes while somebody is mid-sentence must not eat the
/// sentence.
///
/// Answers whether it actually prefilled the box, so the caller can mark the
/// line unread for `on_chat_key` — typing over the offer is the common case
/// (see `Thread::model_offer_unread`), and only a call that changed the box
/// needs that guard armed.
fn offer_models(app: &mut App, line: &str) -> bool {
    if !app.input.is_empty() {
        return false;
    }
    app.accept_completion(line);
    true
}

/// The harness a preference value names, if it names one.
fn harness_of(value: &config::Value) -> Option<HarnessKind> {
    match value {
        config::Value::Harness(kind) => Some(*kind),
        _ => None,
    }
}

/// Everything a finished run said, oldest first.
///
/// Every assistant message rather than the last, for the reason
/// `collect_output` gives: a model asked for bare text will preface it,
/// apologise, or wrap it — and dropping all but the final message would throw
/// away the summary to keep the sign-off.
fn said(events: &[jod_core::AgentEnvelope]) -> String {
    let mut out = String::new();
    for envelope in events {
        if let AgentEvent::Message { text } = &envelope.event {
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

/// Bring a schedule's next instant forward to now.
///
/// The store refuses one that is not armed, and that refusal has to be *said*.
/// A paused schedule that stays paused while the key looks like it worked is
/// the failure this whole screen exists to prevent, so the reason is read back
/// out of the row rather than reported as a bare "no".
fn run_schedule(store: &Store, name: &str, at_ms: i64) -> String {
    match store.run_schedule_now(name, at_ms) {
        Ok(true) => format!("{name} is due now — the next tick starts it"),
        Ok(false) => match store.schedule_named(name) {
            Ok(Some(s)) => format!(
                "{name} is {}, so it was not brought forward — press p to arm it",
                s.state.as_str()
            ),
            Ok(None) => format!("no schedule called {name}"),
            Err(e) => format!("could not bring {name} forward: {e}"),
        },
        Err(e) => format!("could not bring {name} forward: {e}"),
    }
}

/// Stop an armed schedule, or start any stopped one.
///
/// Not a two-state toggle: `broken` is a third stored state, and treating it as
/// "not paused" would make `p` on a schedule the breaker tripped pause
/// something that was already stopped — two presses to reach the one thing a
/// person looking at a broken row wants. Anything that is not armed arms.
fn toggle_schedule(store: &Store, name: &str) -> String {
    let state = match store.schedule_named(name) {
        Ok(Some(s)) => s.state,
        Ok(None) => return format!("no schedule called {name}"),
        Err(e) => return format!("could not read {name}: {e}"),
    };
    let armed = state == ScheduleState::Armed;
    let next = if armed {
        ScheduleState::Paused
    } else {
        ScheduleState::Armed
    };
    match store.set_schedule_state(name, next) {
        Ok(true) if armed => format!("{name} is paused — it will not fire until you arm it"),
        Ok(true) => match store.schedule_named(name) {
            Ok(Some(s)) => match s.next_fire_at_ms {
                Some(at) => format!("{name} is armed — next {}", clock(at)),
                None => format!("{name} is armed"),
            },
            _ => format!("{name} is armed"),
        },
        Ok(false) => format!("no schedule called {name}"),
        Err(e) => format!("could not change {name}: {e}"),
    }
}

fn delete_schedule(store: &Store, name: &str) -> String {
    match store.delete_schedule(name) {
        Ok(true) => format!("deleted {name} — its fire history went with it"),
        Ok(false) => format!("no schedule called {name}"),
        Err(e) => format!("could not delete {name}: {e}"),
    }
}

/// The run a schedule most recently started, for `⏎` to open.
///
/// Read out of the fire record rather than the schedule row: a fire is the only
/// thing that names a run, and a fire that skipped or failed to spawn names
/// none — so the newest fire is not necessarily the newest *run*.
fn last_run_of(store: &Store, name: &str) -> std::result::Result<String, String> {
    let id = match store.schedule_named(name) {
        Ok(Some(s)) => s.id,
        Ok(None) => return Err(format!("no schedule called {name}")),
        Err(e) => return Err(format!("could not read {name}: {e}")),
    };
    match store.fires(&id, FIRE_LOOKBACK) {
        Ok(fires) => fires
            .into_iter()
            .find_map(|f| f.run_id)
            .ok_or_else(|| format!("{name} has not started a run yet")),
        Err(e) => Err(format!("could not read {name}'s fires: {e}")),
    }
}

/// How far back `⏎` looks for a fire that started a run. A schedule that has
/// been skipping all week still has a run worth opening.
const FIRE_LOOKBACK: usize = 50;

fn run_goal(store: &Store, name: &str, at_ms: i64) -> String {
    match store.run_goal_now(name, at_ms) {
        Ok(true) => format!("{name}'s next iteration is due now — the next tick starts it"),
        Ok(false) => match store.goal_named(name) {
            Ok(Some(g)) => format!(
                "{name} is {}, so it was not brought forward — press p to start it again",
                g.state.as_str()
            ),
            Ok(None) => format!("no goal called {name}"),
            Err(e) => format!("could not bring {name} forward: {e}"),
        },
        Err(e) => format!("could not bring {name} forward: {e}"),
    }
}

/// Stop a running goal, or start any stopped one — including a stalled or
/// exhausted one, where restarting is exactly the decision a person is there
/// to make.
fn toggle_goal(store: &Store, name: &str) -> String {
    let state = match store.goal_named(name) {
        Ok(Some(g)) => g.state,
        Ok(None) => return format!("no goal called {name}"),
        Err(e) => return format!("could not read {name}: {e}"),
    };
    let running = state == GoalState::Running;
    let next = if running {
        GoalState::Paused
    } else {
        GoalState::Running
    };
    match store.set_goal_state(name, next) {
        Ok(true) if running => format!("{name} is paused — no further iterations"),
        Ok(true) => format!("{name} is running again, from {}", state.as_str()),
        Ok(false) => format!("no goal called {name}"),
        Err(e) => format!("could not change {name}: {e}"),
    }
}

fn delete_goal(store: &Store, name: &str) -> String {
    match store.delete_goal(name) {
        // The store writes the line, so the run this leaves working is named
        // on screen as well as on the terminal. A notice wraps on `\n`, so the
        // second sentence lands on its own line rather than being run on.
        Ok(Some(forgotten)) => forgotten.summary(),
        Ok(None) => format!("no goal called {name}"),
        Err(e) => format!("could not delete {name}: {e}"),
    }
}

fn toggle_hook(store: &Store, name: &str) -> String {
    let enabled = match store.webhook_rule(name) {
        Ok(Some(rule)) => rule.enabled,
        Ok(None) => return format!("no webhook called {name}"),
        Err(e) => return format!("could not read {name}: {e}"),
    };
    match store.set_webhook_rule_enabled(name, !enabled) {
        Ok(true) if enabled => {
            format!("{name} is off — deliveries are still recorded, but start nothing")
        }
        Ok(true) => format!("{name} is on"),
        Ok(false) => format!("no webhook called {name}"),
        Err(e) => format!("could not change {name}: {e}"),
    }
}

fn delete_hook(store: &Store, name: &str) -> String {
    match store.delete_webhook_rule(name) {
        Ok(true) => format!("deleted {name} — its deliveries are kept, matched to no rule"),
        Ok(false) => format!("no webhook called {name}"),
        Err(e) => format!("could not delete {name}: {e}"),
    }
}

/// Destroy everything Jod believes about one subject.
///
/// `Store::forget` takes a triple and a memory row is a *subject*, so the
/// predicates are read first and each one forgotten in turn. Forgetting only
/// the predicate that happens to be showing would leave the rest of the node
/// readable and the screen claiming it was gone.
///
/// The bare name survives, with no edges: `facts` cascades into `relations` but
/// not into `entities`, so the row stays until the graph is rebuilt. Said out
/// loud, because the row not disappearing otherwise reads as a key that failed.
/// Write a credential, answer its card with a *name*, and say what happened.
///
/// The value is consumed here and reaches exactly one call. Note what the card
/// is answered with: `secret::stored_summary`, a name and a scope. That string
/// becomes the agent's delivery via `Card::answer_body`, so it is the sentence
/// the model will read — which is precisely why it must not contain, hint at,
/// or measure the value. The agent is told a name; it reads the value as an
/// environment variable or not at all.
///
/// A failed write does **not** answer the card. A card marked answered against
/// a secret that was never stored would take the request out of the rail and
/// leave the run blocked on a credential nobody is going to supply.
fn stored_secret(
    store: &Store,
    card: i64,
    name: &str,
    scope: jod_core::secrets::Scope,
    scope_id: &str,
    value: secret::Typed,
) -> String {
    let meta = match store.put_secret(name, scope, scope_id, value.reveal(), "") {
        Ok(meta) => meta,
        // The error is the store's own and names the rule that was broken —
        // an illegal variable name, an empty value, a NUL byte. None of them
        // can quote the value, because none of them were given it to quote.
        Err(e) => return format!("`{name}` not stored: {e}"),
    };
    let said = secret::stored_note(&meta);
    match store.answer_card(card, None, Some(&secret::stored_summary(name, scope))) {
        Ok(_) => said,
        Err(e) => format!("{said}. The card could not be closed, though: {e}"),
    }
}

/// Answer a card, and say what that actually did.
///
/// The sentence is the feature. "Answered" alone would be the lie decision D2
/// is written against: the answer is *recorded and queued*, and the agent hears
/// it at the next turn boundary — immediately if it is idle, at the end of the
/// current turn if it is not. A reader told only "answered" goes back to
/// watching the transcript for a change that is not due yet, decides the key
/// did not work, and answers again.
fn answered_card(store: &Store, id: i64, chosen: Option<&str>, answer: Option<&str>) -> String {
    match store.answer_card(id, chosen, answer) {
        Ok(card) => {
            let what = card.chosen.or(card.answer).unwrap_or_default();
            format!(
                "card #{id} answered “{}” — queued; it reaches the agent at the end of the turn \
                 in flight",
                app::one_line(&what, 60)
            )
        }
        Err(e) => format!("card #{id} not answered: {e}"),
    }
}

fn forget_about(store: &Store, subject: &str) -> String {
    let facts = match store.facts_about(subject) {
        Ok(facts) => facts,
        Err(e) => return format!("could not read what is known about {subject}: {e}"),
    };
    let mut triples: Vec<(String, String)> =
        facts.into_iter().map(|f| (f.scope, f.predicate)).collect();
    triples.sort();
    triples.dedup();
    if triples.is_empty() {
        return format!("nothing is recorded about {subject}, so there was nothing to forget");
    }

    let mut gone = 0usize;
    for (scope, predicate) in triples {
        match store.forget(&scope, subject, &predicate) {
            Ok(versions) => gone += versions,
            // Partial is reported as partial. Claiming the whole node was
            // forgotten when half of it is still answerable is the one failure
            // `Store::forget` is written to avoid.
            Err(e) => {
                return format!("forgot {gone} of what is known about {subject}, then failed: {e}")
            }
        }
    }
    format!(
        "forgot {gone} {} about {subject} — the bare name stays in the graph until it is rebuilt",
        if gone == 1 { "thing" } else { "things" }
    )
}

/// An absolute instant, for the fire times a dry run prints.
///
/// Local time, always: the person reading it is at this terminal, and a
/// schedule's own timezone is printed beside the expression so the two can be
/// compared rather than confused.
fn clock(at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%a %d %b %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string())
}

/// A short, stable id for a task typed into the board.
///
/// Derived from the title so the id means something when a teammate claims it
/// from the command line, and suffixed when that would collide — a board with
/// two `write-the-docs` rows is a board where `jod team claim` picks the wrong
/// one.
fn task_id(title: &str, existing: &[jod_core::team::TeamTask]) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    let base = if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    };
    if !existing.iter().any(|t| t.id == base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.iter().any(|t| &t.id == candidate))
        .unwrap_or(base)
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Handle one keypress. Returns work for the loop to carry out, if any.
///
/// Three layers, checked in order, and the status bar always says which one you
/// are in: an **overlay** owns the keyboard while it is up, a **workspace**
/// makes letters into commands, and **chat** makes them text again. Quitting is
/// ahead of all three, because a key that cannot always leave is a trap.
fn on_key(app: &mut App, thread: &mut Thread, key: KeyEvent, viewport: usize) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
        return on_quit(app);
    }
    // Any key other than a second quit means the user changed their mind.
    app.confirm_quit = false;

    // Ahead of the overlay and of every screen's own Esc, and only while the
    // microphone is live. Escape means a dozen things here depending on where
    // the cursor is; while a recorder is running it means exactly one, because
    // that is the state whose cost is outside the program — a hot microphone
    // and an upload about to be paid for. When nothing is recording this falls
    // through and Escape means what it always did.
    if key.code == KeyCode::Esc && app.dictation.is_active() {
        return Some(Action::CancelDictation);
    }

    if app.overlay.is_open() {
        return on_overlay_key(app, key);
    }
    if ctrl || alt {
        if let Some(action) = on_chord(app, key) {
            return action;
        }
    }
    // An Alt chord nobody claimed is still a chord, not a keystroke. Falling
    // through would reach the `KeyCode::Char(c)` arm at the bottom of chat and
    // type `d` into the prompt when the user pressed Alt-D — a stray letter in
    // a line they are about to send. Ctrl is left falling through as it always
    // has: changing it is a separate question from moving the keymap.
    if alt && matches!(key.code, KeyCode::Char(_)) {
        return None;
    }
    // Tab and Shift-Tab, on every screen, because they answer two questions you
    // have everywhere: how much may this thing do, and what else is running.
    //
    // Tab defers to the completion popup, which owns it while it is up — one
    // key doing two jobs is only safe while the layer that has it is visible,
    // and the popup is. Shift-Tab has no such contender.
    match key.code {
        KeyCode::BackTab => {
            app.panel = !app.panel;
            // Closing the panel hands the keyboard back with it, the way hiding
            // the rail does — otherwise the bare keys stay the catalog's with no
            // catalog on screen to explain them.
            if !app.panel {
                app.panel_focused = false;
            }
            return None;
        }
        KeyCode::Tab if command::completions(&app.input, app).is_empty() => {
            let said = app.cycle_mode();
            if app.workspace == Workspace::Chat {
                app.push(Entry::Notice(said));
            }
            return None;
        }
        _ => {}
    }
    // The rail sits above both of the layers below it, because it is drawn
    // beside both and `Ctrl-N` may have been pressed on either. It only owns the
    // keyboard once it has been given it, which is what keeps a rail that is
    // merely *visible* from stealing the letters you are typing.
    if app.rail.focused && app.rail.shown {
        return on_rail_key(app, key);
    }
    // The catalog, on the same terms and for the same reason: drawn beside every
    // screen, so it is above both layers, and it holds the bare keys only once
    // `Ctrl-P` or a click has handed them over. Below the rail because a card
    // that is blocking a run is more pressing than a list of repositories, and
    // both cannot hold the keyboard at once.
    if app.panel_focused && app.panel && app.projects_open {
        return on_catalog_key(app, key);
    }
    if app.workspace.is_list() {
        return on_workspace_key(app, key, viewport);
    }
    on_chat_key(app, thread, key, viewport)
}

/// Keys while the decision rail has the keyboard.
///
/// Every one of these is a bare letter, which is only safe because the focus is
/// explicit and printed: `Ctrl-N` gives the rail the keyboard, the keybar
/// changes to the rail's verbs while it holds it, and `Esc` gives it back with
/// the typed line untouched. See [`keys::RAIL`].
fn on_rail_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    // A `/` line being typed owns the keyboard, letters and all — otherwise
    // filtering for "sqlite" would try to sort, then dismiss a card.
    if app.rail.editing_filter {
        match key.code {
            KeyCode::Char(c) => {
                if let Some(f) = app.rail.filter.as_mut() {
                    f.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(f) = app.rail.filter.as_mut() {
                    f.pop();
                }
            }
            // Accepting keeps the filter and hands the letters back to being
            // verbs. `Esc` is the one that clears it.
            KeyCode::Enter => app.rail.editing_filter = false,
            KeyCode::Esc => {
                app.rail.filter = None;
                app.rail.editing_filter = false;
            }
            _ => {}
        }
        return None;
    }

    let ids = app.card_ids();
    match key.code {
        KeyCode::Esc => {
            app.rail.back();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.rail.step(-1, &ids);
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.rail.step(1, &ids);
            None
        }
        KeyCode::Home => {
            app.rail.selected = ids.first().copied();
            None
        }
        KeyCode::End => {
            app.rail.selected = ids.last().copied();
            None
        }
        // `→` expands and `←` collapses, spelled spatially, because the
        // expanded card is wider than the collapsed one and the movement reads
        // as the shape it makes. `⏎` is the same verb on the primary key.
        KeyCode::Enter | KeyCode::Right => {
            if app.rail.selected.is_some() {
                app.rail.expanded = !app.rail.expanded;
            }
            None
        }
        KeyCode::Left => {
            app.rail.expanded = false;
            None
        }
        KeyCode::Char('/') => {
            app.rail.filter = Some(String::new());
            app.rail.editing_filter = true;
            None
        }
        KeyCode::Char('S') => {
            let sort = app.rail.cycle_sort();
            app.push(Entry::Notice(format!("rail sorted by {}", sort.as_str())));
            None
        }
        KeyCode::Char('t') => {
            let stack = app.rail.cycle_stack();
            app.push(Entry::Notice(format!("rail showing {} cards", stack.as_str())));
            None
        }
        KeyCode::Char('f') => {
            let kind = app.rail.cycle_kind();
            let what = kind.map(|k| k.as_str()).unwrap_or("every kind of");
            app.push(Entry::Notice(format!("rail showing {what} card")));
            None
        }
        // The subtree scope. Cascade is upward only, so this widens to "this
        // session and everything it started" and narrows back to "this session
        // alone" — it can never show a parent's cards to a child.
        KeyCode::Char('c') => {
            app.rail.cascade = !app.rail.cascade;
            app.push(Entry::Notice(
                if app.rail.cascade {
                    "rail showing this session and everything below it"
                } else {
                    "rail showing this session only"
                }
                .into(),
            ));
            None
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            None
        }
        // No confirmation, unlike `x` on a list screen. Dismissing is a state
        // change and not a deletion — the card stays, findable under the
        // answered/dismissed toggle — and a card that took two keys to put down
        // is a rail nobody clears.
        KeyCode::Char('x') => app.selected_card().map(|c| Action::DismissCard(c.id)),
        KeyCode::Char('a') => {
            let card = app.selected_card()?;
            let (id, title) = (card.id, card.title.clone());
            // A credential takes a different field from an answer, and the
            // difference is not cosmetic: `Overlay::Prompt` echoes what is
            // typed and hands it on as an ordinary `String`, which is right
            // for a schedule's name and disqualifying for a token. See
            // `secret.rs`.
            app.overlay = if card.kind == jod_core::cards::CardKind::Secret {
                Overlay::Secret {
                    card: id,
                    name: card.secret_name.clone().unwrap_or_else(|| title.clone()),
                    scope: card
                        .secret_scope
                        .as_deref()
                        .map(jod_core::secrets::Scope::parse)
                        // Work, matching the store's own default: a key given
                        // for one project must not be handed to every session
                        // on the box because a field was left blank.
                        .unwrap_or_default(),
                    value: secret::Typed::new(),
                }
            } else {
                Overlay::Prompt {
                    label: format!("answer #{id} · {}", app::one_line(&title, 40)),
                    value: String::new(),
                    intent: PromptIntent::AnswerCard(id),
                }
            };
            None
        }
        KeyCode::Char(c) if c.is_ascii_digit() => answer_by_digit(app, c),
        _ => None,
    }
}

/// Keys while the project catalog has the keyboard.
///
/// Bare letters, which is only safe because the focus is explicit and printed,
/// exactly as it is for the rail: `Ctrl-P` gives the catalog the keyboard, the
/// keybar changes to the catalog's verbs while it holds it, and `Esc` gives it
/// back with the typed line untouched. See [`keys::CATALOG`].
///
/// Deliberately few verbs. The catalog is a list of repositories and the useful
/// things to do to one — cataloguing, untracking, restoring — already have
/// homes: `/project` from the chat box, and `x` on the fleet, which knows which
/// row you mean when two projects share a name. What was missing was the plain
/// ability to move a cursor down the box and open one, so that is what this is.
fn on_catalog_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        // One level, and there is only one: the catalog holds no filter and no
        // expanded row, so `Esc` is the way out and nothing else.
        KeyCode::Esc => {
            app.close_catalog();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.step_project(-1);
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.step_project(1);
            None
        }
        KeyCode::Home => {
            app.project_selected = app.catalog().first().map(|p| p.id.clone());
            None
        }
        KeyCode::End => {
            app.project_selected = app.catalog().last().map(|p| p.id.clone());
            None
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            None
        }
        // `⏎` goes into the project's manager, which is the conversation that
        // owns that repository's work — the same movement `⏎` makes on a
        // manager row of the fleet, and the reason the row is worth pointing at
        // rather than merely reading.
        //
        // A project catalogued before managers existed has none, and says so
        // rather than doing nothing: a key that appears inert is how somebody
        // concludes the whole box is broken.
        KeyCode::Enter => match app.selected_project() {
            None => {
                app.push(Entry::Notice(CATALOG_EMPTY.into()));
                None
            }
            Some(project) => match project.manager_conversation_id.clone() {
                Some(conversation) => Some(Action::EnterManager(conversation)),
                None => {
                    app.push(Entry::Notice(format!(
                        "{} has no manager yet — one is made the first time work is \
                         routed to it",
                        project.name
                    )));
                    None
                }
            },
        },
        _ => None,
    }
}

/// A click, resolved against what the last frame drew.
///
/// The rail is the only thing on screen that takes the pointer, and it takes it
/// for one reason: on a phone the rail lies along the bottom of the chat, the
/// bare keys belong to the composer, and reaching a card meant a chord followed
/// by a digit — on a keyboard that is not there. A tap has to be able to do it.
///
/// Three gestures on the rail, and no more: a tap on a card in the stack opens
/// it, a tap on one of its numbered options answers with that option, and a tap
/// on the expanded card's title puts it back in the stack. A tap anywhere else
/// inside the card does nothing on purpose — the rest of the card is prose being
/// read, and a stray tap that collapsed it, or worse answered it, would be the
/// one mistake this feature must not introduce.
///
/// The catalog takes the pointer on the same terms, and it is the same argument
/// one box over: a project row is a thing you point at, and on the panel there
/// was no way to point at one at all.
fn on_click(
    app: &mut App,
    hits: &ui::RailHits,
    panel: &ui::PanelHits,
    column: u16,
    row: u16,
) -> Option<Action> {
    if panel.holds(column, row) {
        return on_catalog_click(app, panel, column, row);
    }
    if !hits.holds(column, row) {
        return None;
    }
    // A pointer in the rail takes the keyboard, so it must take it from the
    // catalog as well — see `App::focus_catalog`.
    app.panel_focused = false;
    // A pointer in the rail is the same statement as `Ctrl-N`: the rail has the
    // keyboard now. Without this the digits would still be the composer's, and
    // the card that was just tapped could not be answered by typing.
    app.rail.shown = true;
    app.rail.focused = true;

    if let Some((id, at)) = hits.option_at(column, row) {
        let card = app.cards.iter().find(|c| c.id == id)?;
        let chosen = card.options.get(at)?.clone();
        app.rail.look_at(id);
        return Some(Action::AnswerCard {
            id,
            chosen: Some(chosen),
            answer: None,
        });
    }
    if hits.on_back(column, row) {
        app.rail.collapse();
        return None;
    }
    if let Some(id) = hits.card_at(column, row) {
        app.rail.look_at(id);
        // Opened, not merely selected. A tap that only moved a cursor would
        // need a second gesture nobody has been told about, and the card's body
        // — the actual question — is only in the expanded view.
        app.rail.expanded = true;
    }
    None
}

/// A click inside the project catalog.
///
/// Two gestures. A tap anywhere in the box hands it the keyboard, which is the
/// same statement `Ctrl-P` makes and is what a click on a list means everywhere
/// else in this program. A tap on a project row also puts the cursor on that
/// row — and stops there rather than going into the project's manager.
///
/// Stopping there is the difference from the rail, and it is deliberate. A card
/// is a question waiting to be answered, so a tap that opens it costs nothing;
/// entering a manager rebinds the chat box to another conversation, and a stray
/// click that moved the sentence you were typing into a different repository is
/// precisely the mistake the panel exists to prevent. So the pointer selects and
/// `⏎` commits, with the row highlighted in between.
fn on_catalog_click(
    app: &mut App,
    panel: &ui::PanelHits,
    column: u16,
    row: u16,
) -> Option<Action> {
    app.focus_catalog();
    if let Some(id) = panel.project_at(column, row) {
        app.project_selected = Some(id.to_string());
    }
    None
}

/// The wheel over the rail: the expanded card scrolls, the stack walks.
///
/// The stack moves its *selection* rather than an offset of its own. The window
/// is derived from where the cursor is, so one position is the only one there
/// is — a second, independent offset would let the cursor drift off screen, and
/// then a digit would answer a card nobody can see.
fn on_rail_wheel(app: &mut App, hits: &ui::RailHits, delta: i16) {
    if hits.expanded.is_some() {
        app.rail.scroll_card(delta, hits.past);
        return;
    }
    let ids = app.card_ids();
    app.rail.step(delta as isize, &ids);
}

/// A digit in the rail picks the numbered option under the cursor.
///
/// A digit that names no option says so rather than doing nothing: the options
/// are printed numbered beside the card, so pressing `4` on a card with three
/// of them is a misread, and silence would leave the reader believing the
/// answer went in.
fn answer_by_digit(app: &mut App, c: char) -> Option<Action> {
    let card = app.selected_card()?;
    let (id, options) = (card.id, card.options.clone());
    let at = c.to_digit(10)? as usize;
    if at == 0 || at > options.len() {
        app.push(Entry::Notice(if options.is_empty() {
            format!("card #{id} offers no options — `a` answers it in prose")
        } else {
            format!(
                "card #{id} offers {} — press 1–{}",
                app::plural(options.len(), "option"),
                options.len()
            )
        }));
        return None;
    }
    Some(Action::AnswerCard {
        id,
        chosen: Some(options[at - 1].clone()),
        answer: None,
    })
}

/// Refuse to leave silently while work is in flight; a second press goes
/// anyway. Any running agent counts, not just the one on screen — walking out
/// on four background jobs without being told is the same mistake, four times
/// over.
fn on_quit(app: &mut App) -> Option<Action> {
    let running = app.running();
    if running > 0 && !app.confirm_quit {
        app.confirm_quit = true;
        let what = if running == 1 {
            "an agent is still running".to_string()
        } else {
            format!("{running} agents are still running")
        };
        app.push(Entry::Notice(format!(
            "{what} — press again to leave them running"
        )));
    } else {
        app.should_quit = true;
    }
    None
}

/// The chords that work in every layer. `Some` means the chord was handled.
///
/// **Ctrl throughout, minus the letters something else already holds.** Alt was
/// tried and is unpressable: a stock macOS terminal types `å` rather than
/// sending Option as Meta, so the chords could not be reached at all. Ctrl can
/// be, everywhere except the six letters tmux is prefixed and paned on —
/// `a s h j k l` — which is why several verbs here are two keys rather than
/// one. `keys.rs`'s module header carries the whole argument.
///
/// Two groups, and which one a chord is in is a decision, not a pattern:
///
/// - **`either`** — Jod's own verbs. Ctrl is what the keybar prints, because it
///   is the spelling that can be typed; Alt keeps firing for anyone who learned
///   the last release and for the terminals that do send it. `Ctrl-N`,
///   `Ctrl-P`, `Ctrl-R`, `Ctrl-T` and `Ctrl-Y` have readline meanings on paper
///   (next- and previous-history, reverse-search, transpose, yank) that Jod has
///   never implemented and does not intend to — the whole input is one prompt
///   that `Ctrl-U` clears, and history is on the bare arrows.
/// - **`ctrl` only** — readline's own keys, which no multiplexer steals because
///   every shell needs them. `Ctrl-A`/`Ctrl-E` to the ends of the line,
///   `Ctrl-U` to clear it, `Ctrl-W` to eat a word. `Ctrl-C`/`Ctrl-D` quit,
///   ahead of all of this in `on_key`. An Alt spelling would be a second way to
///   press a key nobody is having trouble pressing.
///
/// Note that `Ctrl-B` and `Ctrl-F` are readline's word motions. Jod binds no
/// word motion at all, so the chords are free — but if one is ever wanted it
/// has to find another key, because delegate and the fleet are printed here.
fn on_chord(app: &mut App, key: KeyEvent) -> Option<Option<Action>> {
    let handled = |a: Option<Action>| Some(a);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let either = alt || ctrl;
    match key.code {
        // The leader, and now the *only* way to most of the screens: eleven
        // free letters did not stretch to sixteen verbs, so destinations went
        // behind this one and the chords went to the verbs you need without
        // stopping the sentence you are typing. `g` for go.
        KeyCode::Char('g') if either => {
            app.overlay = Overlay::WhichKey;
            handled(None)
        }
        // The one destination that kept a chord, because the fleet is where a
        // delegated run goes: `Ctrl-B` then `Ctrl-F` is a single thought. It
        // used to be `Ctrl-A`, which is tmux's prefix here and readline's
        // start-of-line everywhere — the one Ctrl collision Jod inflicted on
        // itself rather than inherited, and it is not coming back.
        KeyCode::Char('f') if either => {
            app.go(if app.workspace == Workspace::Fleet {
                Workspace::Chat
            } else {
                Workspace::Fleet
            });
            handled(None)
        }
        KeyCode::Char('t') if either => {
            app.show_thinking = !app.show_thinking;
            app.push(Entry::Notice(format!(
                "thinking {}",
                if app.show_thinking { "shown" } else { "hidden" }
            )));
            handled(None)
        }
        // Unfold the steps of every turn already finished, and fold them away
        // again. It used to turn `show_details` on and off, which is what
        // `/details` is for and is the setting this now reads: `/details`
        // decides whether the steps of the turn *in flight* are streamed at
        // all, and this key decides whether the ones already over can be read
        // back. Nothing is said about the toggle — the transcript growing or
        // shrinking under the cursor is the answer, and a notice announcing a
        // view change would be one more line of the noise being removed.
        KeyCode::Char('o') if either => {
            app.expand_details = !app.expand_details;
            handled(None)
        }
        // Dictation. A toggle rather than a hold, because a terminal cannot
        // see a key being released — see [`Dictation`]. `Esc` while listening
        // throws the utterance away, which is handled with the other escapes.
        //
        // `v` was the one letter left unspent, and dictation is the verb with
        // the strongest claim to a chord in the program: it is *only* useful
        // while your hands are not on the keyboard.
        //
        // The projects toggle arrived in the same change and did not get one —
        // `Ctrl-D` is quit, and there was nothing left to give it. It has
        // `Ctrl-P` now, taken off the directory picker: see the module header
        // in `keys.rs` for why that swap and not another. Shift-Tab still
        // closes the whole panel, which is the other half of the pair it was
        // designed against.
        KeyCode::Char('v') if either => handled(Some(Action::Dictate)),
        // The rail. `Ctrl-R` is readline's reverse-search on paper, which Jod
        // has never implemented — the input is one prompt, not a history buffer
        // to walk — so the letter is free and `r` is the one that means rail.
        //
        // A visibility toggle, deliberately separate from `Ctrl-N`: hiding the
        // rail must not depend on where the cursor is, and showing it must not
        // cost you the keyboard. Hiding it also hands the keyboard back, or the
        // bare keys would still be the rail's with no rail on screen.
        KeyCode::Char('r') if either => {
            if app.rail.shown {
                app.rail.close();
            } else {
                app.rail.shown = true;
            }
            handled(None)
        }
        // Open the rail, take the keyboard, and put it away again on the second
        // press. Safe in the middle of a sentence — this is the property E2.S3
        // asks for by name — because a chord never reaches `App::insert`.
        //
        // It was `Ctrl-C`, which is quit and always will be. This letter is the
        // one the oldest-unread jump used to have; that is a destination and
        // went behind the leader, where a card you have to answer is not.
        //
        // It used to *cycle* the stack instead of closing it, and that left the
        // rail with no way out on its own key: the only one was `Ctrl-R`, which
        // the rail's keybar never printed. Stepping is what `↑↓`/`jk` are for
        // once the rail has the keyboard, and they were already there.
        KeyCode::Char('n') if either => {
            let ids = app.card_ids();
            app.rail.toggle(&ids);
            // The other half of the mutual exclusion `App::focus_catalog`
            // keeps. Only one thing may hold the bare keys.
            if app.rail.focused {
                app.panel_focused = false;
            }
            handled(None)
        }
        // Copy the last reply. The terminal's own selection stops working the
        // moment a pane has scrollback and wrapping, which is always.
        KeyCode::Char('y') if either => handled(match yank::from_transcript(&app.transcript) {
            Some(found) => {
                let sequence = yank::osc52(&found.text);
                app.push(Entry::Notice(yank::note(&found)));
                Some(Action::Yank(sequence))
            }
            None => {
                app.push(Entry::Notice("nothing to copy yet".into()));
                None
            }
        }),
        // The projects. `p` for projects, which is the letter anybody would
        // guess for the box titled `projects` — it was `Ctrl-G d`, a leader
        // followed by a letter that stands for nothing, chosen only because
        // `Ctrl-D` is quit and `Ctrl-P` was spent on the directory picker.
        //
        // The picker is a destination reached about twice a day and the catalog
        // is a panel glanced at constantly, so the chord went to the one that is
        // pressed. The picker is on `Ctrl-G d` now, and `/add-dir` still opens
        // it by name.
        //
        // Same key both ways, and it takes the keyboard rather than only
        // showing the box: "I cannot navigate to the panel" was the complaint,
        // and a catalog you can see but not move a cursor through is a picture
        // of a list.
        KeyCode::Char('p') if either => {
            toggle_catalog(app);
            handled(None)
        }
        // Delegate: the typed line becomes an agent that runs without taking
        // the screen. This is the key that makes several jobs at once possible
        // without leaving the UI. `Ctrl-B` is tmux's *default* prefix and was
        // the reason the keymap fled to Alt — but this tmux is prefixed on
        // `Ctrl-A`, so the letter is free here, and a chord nobody can type is
        // a worse answer than one a default nobody runs would have eaten.
        KeyCode::Char('b') if either => handled(app.take_input().map(Action::Delegate)),
        // Stop what is being watched. Ctrl-C is quit, so interrupting a run
        // needs a key of its own or the only way out is to leave.
        KeyCode::Char('x') if either => handled(match app.watching.clone() {
            Some(id) if app.busy => {
                // Harsher than `Esc` and asked for the same way, so it is
                // written down the same way: the run's ending is one this
                // reader asked for, and reporting it as a failure would
                // contradict the "stopped" notice a line above it.
                app.interrupting = Some(id.clone());
                Some(Action::Stop(id))
            }
            _ => {
                app.push(Entry::Notice("nothing running to stop here".into()));
                None
            }
        }),
        // Ctrl only, both of them: these *are* readline's verbs, not Jod verbs
        // that happen to sit on readline's keys. An Alt spelling would be a
        // second way to press a key nobody is having trouble pressing.
        KeyCode::Char('u') if ctrl => {
            app.clear_line();
            handled(None)
        }
        KeyCode::Char('w') if ctrl => {
            app.delete_word();
            handled(None)
        }
        // Scrolling keeps a modifier at all because the bare arrows now walk
        // back through what has been sent.
        KeyCode::Up if either => {
            let max = app.transcript.len();
            app.scroll_up(1, max);
            handled(None)
        }
        KeyCode::Down if either => {
            app.scroll_down(1);
            handled(None)
        }
        // Start and end of the line. `Ctrl-A` means this again now that the
        // fleet has moved to `Ctrl-F`; `Ctrl-Home`/`Ctrl-End` stay because on a
        // list screen the bare Home and End are the first and last *row*, so
        // without them there is no way to reach the ends of the typed line
        // from there at all.
        //
        // Under a prefix-on-`Ctrl-A` tmux this one wants pressing twice. That
        // is the price of readline's convention and it is not Jod's to move —
        // and `Home` is right there, which is why the label prints both.
        KeyCode::Char('a') if ctrl => {
            app.home();
            handled(None)
        }
        KeyCode::Home if either => {
            app.home();
            handled(None)
        }
        KeyCode::Char('e') if ctrl => {
            app.end();
            handled(None)
        }
        KeyCode::End if either => {
            app.end();
            handled(None)
        }
        _ => None,
    }
}

/// Put the cursor on the oldest thing you have not read, and open it.
fn jump_to_oldest_unread(app: &mut App) {
    let oldest = app
        .activity
        .iter()
        .filter(|a| a.unread)
        .min_by_key(|a| a.at_ms)
        .map(|a| a.id.clone());
    app.go(Workspace::Activity);
    match oldest {
        Some(id) => app.list_mut(Workspace::Activity).selected = Some(id),
        None => app.push(Entry::Notice("nothing unread".into())),
    }
}

/// Keys while an overlay is up. `Esc` always cancels exactly this overlay.
fn on_overlay_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    // A credential field, ahead of the ordinary prompt because it must never
    // fall through to one. Every route out of here either moves the value to
    // the store or drops it: there is no branch that keeps it.
    if let Overlay::Secret {
        card,
        name,
        scope,
        value,
    } = &mut app.overlay
    {
        match key.code {
            KeyCode::Char(c) => {
                value.push(c);
                return None;
            }
            KeyCode::Backspace => {
                value.pop();
                return None;
            }
            KeyCode::Enter => {
                if value.is_empty() {
                    // Refused rather than stored. `put_secret` rejects an empty
                    // value anyway, but a card answered with nothing would look
                    // answered while the run stayed blocked.
                    return None;
                }
                // Moved, not copied, and the overlay is closed in the same
                // breath — so no frame drawn after this keypress has the value
                // in it to draw.
                let action = Action::PutSecret {
                    card: *card,
                    name: name.clone(),
                    scope: *scope,
                    // Filled in by the loop, which is the only layer that knows
                    // which work or conversation this card belongs to.
                    scope_id: String::new(),
                    value: value.take(),
                };
                app.overlay = Overlay::None;
                return Some(action);
            }
            KeyCode::Esc => {
                // Cleared explicitly rather than left for the overlay to be
                // overwritten: the value is the one piece of state in this
                // program whose lifetime is worth being deliberate about.
                value.clear();
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    // Search owns every key while it is up. The store lookup itself happens in
    // the loop — `refresh_search` — because this function does no I/O.
    if let Overlay::Search {
        query,
        selected,
        hits,
    } = &mut app.overlay
    {
        match key.code {
            KeyCode::Char(c) => {
                query.push(c);
                *selected = 0;
                return None;
            }
            KeyCode::Backspace => {
                query.pop();
                *selected = 0;
                return None;
            }
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                if *selected + 1 < hits.len() {
                    *selected += 1;
                }
                return None;
            }
            // Land in the conversation holding the turn. That is the whole
            // point of the screen: finding the hit is not the job, getting to
            // it is.
            KeyCode::Enter => {
                let found = hits.get(*selected).map(|h| h.conversation_id.clone());
                app.overlay = Overlay::None;
                return found.map(|c| Action::Sessions(sessions::Request::Open(c)));
            }
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    // The session list owns every key while it is up, for the same reason
    // search does: it is a line being typed into plus a cursor, and a bare `s`
    // in it must narrow the list rather than stop a run. Its rows come from the
    // loop — `refresh_sessions` — because this function does no I/O.
    if let Overlay::Sessions(browser) = &mut app.overlay {
        match key.code {
            KeyCode::Char(c) => {
                browser.push(c);
                return None;
            }
            KeyCode::Backspace => {
                browser.pop();
                return None;
            }
            KeyCode::Up => {
                browser.up();
                return None;
            }
            KeyCode::Down => {
                browser.down();
                return None;
            }
            // Go into the conversation, which is the whole point: finding the
            // thread is not the job, carrying on with it is.
            KeyCode::Enter => {
                let chosen = browser.chosen().cloned();
                app.overlay = Overlay::None;
                return chosen.and_then(|row| continue_session(app, &row));
            }
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    // The full-screen picker, which owns every key while it is up for the same
    // reason the `@` popup does: it is a line being typed into plus a cursor.
    if let Overlay::Picker(p) = &mut app.overlay {
        match key.code {
            KeyCode::Char(c) => {
                p.push(c);
                return None;
            }
            KeyCode::Backspace => {
                p.pop();
                return None;
            }
            KeyCode::Up => {
                p.prev();
                return None;
            }
            KeyCode::Down => {
                p.next();
                return None;
            }
            KeyCode::Enter => {
                let chosen = p.chosen();
                app.overlay = Overlay::None;
                // Nothing matched means nothing to add. Closing anyway is the
                // right answer: the alternative is an `⏎` that appears dead.
                return chosen.map(Action::AddRoot);
            }
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    // A prompt is a line being typed, so it takes characters before anything
    // else looks at them.
    if let Overlay::Prompt {
        value,
        intent,
        label,
    } = &mut app.overlay
    {
        match key.code {
            KeyCode::Char(c) => {
                value.push(c);
                return None;
            }
            KeyCode::Backspace => {
                value.pop();
                return None;
            }
            KeyCode::Enter => {
                let (typed, intent, label) = (value.clone(), intent.clone(), label.clone());
                app.overlay = Overlay::None;
                return accept_prompt(app, label, typed, intent);
            }
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return None;
            }
            _ => return None,
        }
    }

    match &app.overlay {
        // A destructive verb on a bare letter is one fat-fingered `Ctrl-K h x`
        // away from losing a secret, so the confirmation names the thing.
        Overlay::Confirm { verb, what, .. } => {
            let (verb, what) = (verb.clone(), what.clone());
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.overlay = Overlay::None;
                    confirmed(app, &verb, &what)
                }
                // Anything that is not a yes is a no.
                _ => {
                    app.overlay = Overlay::None;
                    None
                }
            }
        }
        Overlay::Keymap => {
            app.overlay = Overlay::None;
            None
        }
        // A list you read, not one you act on: closing on any key is what the
        // keymap already does, and two read-only panels that dismiss
        // differently would be one to learn twice.
        Overlay::Jobs => {
            app.overlay = Overlay::None;
            None
        }
        // Anything that is not a yes leaves the console on the build it
        // started with — the safe half of the question, and the one a stray
        // keypress should land on.
        Overlay::ConfirmReload => {
            app.overlay = Overlay::None;
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::Reload),
                _ => None,
            }
        }
        Overlay::WhichKey => on_which_key(app, key),
        Overlay::WhichKeyNew => {
            app.overlay = Overlay::None;
            match key.code {
                KeyCode::Char(c) => match Workspace::from_letter(c) {
                    Some(ws) if ws.is_list() => {
                        app.go(ws);
                        begin_new(app, ws)
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        // Both are handled above and cannot reach here; closing rather than
        // falling through keeps an unexpected state from stranding the
        // keyboard in an overlay nothing dismisses.
        Overlay::Search { .. }
        | Overlay::Sessions(_)
        | Overlay::Picker(_)
        | Overlay::Secret { .. }
        | Overlay::Prompt { .. }
        | Overlay::None => {
            app.overlay = Overlay::None;
            None
        }
    }
}

/// Carry on with the conversation picked off the session list.
///
/// The same three writes `r` on the fleet makes, because it is the same act
/// reached from a different handle: the fleet resumes the run under the cursor,
/// and this resumes a thread that may have no run on screen at all. The harness
/// is set from the row rather than left alone — a Claude session handed to
/// OpenCode is not a resume, it is a fresh conversation wearing the wrong id.
///
/// A conversation the harness never named is refused out loud. Passing `None`
/// through would silently start a new session under the title of an old one,
/// which is the one outcome someone who came here to *go back* to something
/// cannot check.
fn continue_session(app: &mut App, row: &sessions::SessionRow) -> Option<Action> {
    let Some(session) = row.session_id.clone() else {
        app.push(Entry::Notice(format!(
            "{} never reported a conversation, so there is nothing to continue — \
             ⏎ on it would start a fresh one",
            row.short
        )));
        return None;
    };
    app.go(Workspace::Chat);
    app.resume = Resume::Session(session.clone());
    app.session = Some(session);
    app.harness_from_label(&row.harness);
    app.push(Entry::Notice(format!(
        "next turn continues “{}” — type to carry on",
        row.title
    )));
    // Out of whatever the chat box was bound to, for the reason `/resume` does
    // the same: the next turn belongs to this thread now.
    Some(Action::NewThread)
}

/// Stop looking at what is on screen and go back to the fleet.
///
/// Nothing is stopped, and nothing needs to be: a Jod run is a detached process
/// group that reports through the database, so the TUI was only ever a viewer
/// of it. "Background" here means this window stops looking, which is why it
/// can be done without asking the supervisor anything.
///
/// And why it is not asked about either. This used to raise a confirmation, and
/// the question had no answer worth giving: backgrounding is what a Jod run
/// already *is*, so the prompt was asking whether to leave a thing already left,
/// once per trip out of a session. The notice below is the whole of what the
/// question was for — it says the run is still going and which two keys reopen
/// it — and it says so without costing a keystroke.
fn background(app: &mut App) {
    let what = app
        .watching
        .as_deref()
        .map(short)
        .unwrap_or_else(|| "this turn".to_string());
    app.watching = None;
    app.go(Workspace::Fleet);
    app.push(Entry::Notice(format!(
        "{what} keeps running — ⏎ or → opens it again"
    )));
}

/// Back out of a manager conversation to the fleet, cursor on the row that
/// opened it.
///
/// The inverse of `⏎` on a manager row. Nothing is unbound: the chat box stays
/// tied to that conversation exactly as it stays tied to the main chat while
/// you walk the fleet, so typing after coming back in still instructs the same
/// manager. What changes is only which screen you are looking at, which is why
/// this asks the supervisor nothing — see [`background`], which leaves a run
/// alone for the same reason.
///
/// The cursor placement is the half that makes it a round trip. Left where it
/// was, `←` then `⏎` would reopen whatever row the fleet happened to be
/// pointing at — very likely a run belonging to something else entirely.
/// [`fleet::TreeState::reveal`] also opens the project above the row, because a
/// cursor on a hidden row is dropped at the next frame.
fn leave_manager(app: &mut App, conversation: &str) {
    let row = jod_core::tree::NodeId::manager(conversation);
    let forest = app.forest.clone();
    app.tree.reveal(&forest, &row);
    app.go(Workspace::Fleet);
}

/// What `y` on a confirmation actually does.
///
/// Read off the screen the question was asked on rather than carried inside
/// `Overlay::Confirm`: an overlay owns the keyboard for as long as it is up, so
/// the workspace cannot have moved between the question and the answer — and
/// keeping the overlay to a verb and a name is what lets the renderer draw it
/// without knowing that actions exist.
fn confirmed(app: &mut App, verb: &str, what: &str) -> Option<Action> {
    let what = what.to_string();
    match app.workspace {
        Workspace::Schedules => Some(Action::DeleteSchedule(what)),
        Workspace::Goals => Some(Action::DeleteGoal(what)),
        Workspace::Hooks => Some(Action::DeleteHook(what)),
        Workspace::Memory => Some(Action::Forget(what)),
        // A board row is a team's, not this session's: removing one is a verb
        // the store does not have, and `complete_task` is a different thing
        // wearing the same word.
        _ => Some(Action::Pending {
            verb: format!("{verb} {what}"),
            needs: "a store method to remove a task from a board — only complete_task exists",
        }),
    }
}

/// The which-key menu's second keystroke. Anything it does not know cancels
/// silently rather than doing something surprising.
///
/// Six of these are verbs rather than screens, and they are here because there
/// was nowhere else: eleven free Ctrl letters did not stretch to sixteen verbs
/// once tmux's six were taken out, so what a chord is *for* decided who kept
/// one. A chord buys reachability mid-sentence; none of these six need it.
/// `$EDITOR` takes the line away from you anyway, the transcript is not being
/// cleared halfway through a thought, and searching, the jobs panel and the
/// oldest unread are all somewhere to *go*. See `keys.rs`'s module header.
///
/// The letters are free of the nine workspaces by construction — `Workspace`
/// claims `c f m s g h t a w`, and `a_which_key_verb_does_not_shadow_a_screen`
/// is what keeps a new screen from quietly taking one back.
fn on_which_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let KeyCode::Char(c) = key.code else {
        app.overlay = Overlay::None;
        return None;
    };
    match c {
        'n' => {
            app.overlay = Overlay::WhichKeyNew;
            None
        }
        'e' => {
            app.overlay = Overlay::None;
            Some(Action::Editor)
        }
        // The background shells this console started — an update building while
        // you carry on working, and whatever joins it later.
        'j' => {
            app.overlay = Overlay::Jobs;
            None
        }
        // Every conversation you could go back into. `r` for *resume*, which is
        // the word the command line already uses for this and the word for what
        // pressing ⏎ on a row does — `s` is schedules and `c` is chat, so the
        // two letters the subject is named after were both spoken for.
        //
        // Behind the leader rather than on a chord because it is a destination,
        // and that is the rule the eleven free Ctrl letters were spent under.
        // See `keys.rs`'s module header.
        'r' => {
            app.overlay = Overlay::Sessions(sessions::Browser::default());
            None
        }
        // Only meaningful once cron, goals and webhooks report endings while
        // nobody is at the terminal.
        'u' => {
            app.overlay = Overlay::None;
            jump_to_oldest_unread(app);
            None
        }
        // `l` is what clears the screen in every shell, so the letter survives
        // the chord it can no longer have — `Ctrl-L` is a tmux pane here.
        'l' => {
            app.overlay = Overlay::None;
            app.transcript.clear();
            app.scroll_to_bottom();
            None
        }
        // The full-screen directory picker, which used to be `Ctrl-P` and gave
        // that chord up to the projects catalog: a picker opened twice a day
        // does not need a chord more than a panel glanced at constantly does,
        // and `p` for projects is the letter a reader guesses. `/add-dir` is
        // the same picker under its folder-first name.
        //
        // `d` for directory, which is what it adds — the letter it inherited
        // stood for nothing, having been picked because `Ctrl-D` is quit.
        //
        // Enumerating from the key handler is the one place this file does
        // I/O, and it is deliberate: the walk is bounded and happens once, on
        // an explicit keystroke, rather than on the tick — a background walk of
        // the filesystem to keep a picker warm that is opened twice a day is a
        // cost nobody asked for. Every *keystroke* after this ranks in memory.
        'd' => {
            app.overlay = Overlay::None;
            open_picker(app, launch_dir());
            None
        }
        // Search every transcript. `/` is the command palette in chat and the
        // list filter everywhere else, so this is the one place in the program
        // where it can mean the third thing without ambiguity: the menu is up,
        // it is drawn, and it owns the keyboard while it is.
        '/' => {
            app.overlay = Overlay::Search {
                query: String::new(),
                selected: 0,
                hits: Vec::new(),
            };
            None
        }
        '?' => {
            app.overlay = Overlay::Keymap;
            None
        }
        _ => {
            app.overlay = Overlay::None;
            // The letter, or the digit printed beside it. Every row of this
            // menu says "or 4" against its letter, and the digit did nothing
            // here — it works from inside another workspace, which is the one
            // place you are not while reading this menu. A hint printed only
            // where it is false is worse than no hint.
            if let Some(ws) = Workspace::from_letter(c).or_else(|| Workspace::from_digit(c)) {
                app.go(ws);
            }
            None
        }
    }
}

/// Keys while a workspace owns the keyboard.
fn on_workspace_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Action> {
    let ws = app.workspace;
    if ws == Workspace::MemoryGraph {
        return on_graph_key(app, key);
    }

    // A `/` line being typed owns the keyboard, letters and all — otherwise
    // filtering for "stop" would stop something.
    if app.here().editing_filter {
        match key.code {
            KeyCode::Char(c) => {
                if let Some(f) = app.here_mut().filter.as_mut() {
                    f.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(f) = app.here_mut().filter.as_mut() {
                    f.pop();
                }
            }
            // Accepting keeps the filter and hands the letters back to being
            // commands. `Esc` is the one that clears it.
            KeyCode::Enter => app.here_mut().editing_filter = false,
            KeyCode::Esc => {
                let list = app.here_mut();
                list.filter = None;
                list.editing_filter = false;
            }
            _ => {}
        }
        app.reconcile();
        return None;
    }

    // Before the spine, and only for the one screen that draws somebody else's
    // cursor: a fleet holding works highlights the row `TreeState` points at,
    // not the one this list does. Left below the spine, `↑`/`↓` were answered
    // here first and stepped a flat list nobody was looking at — the highlight
    // stayed put, which reads as a dead key rather than as two cursors.
    //
    // The tree takes the cursor keys and the tree's own verbs; everything else
    // — `/`, `S`, `?`, a digit — falls through to the spine as before, which is
    // what keeps the fleet's filter line the one the tree reads its needle
    // from.
    if ws == Workspace::Fleet && app.has_tree() {
        if let Some(action) = on_tree_key(app, key, viewport) {
            return action;
        }
    }

    let ids = app.row_ids(ws);
    let page = viewport.max(1) as isize;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.back();
            return None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.here_mut().step(-1, &ids);
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.here_mut().step(1, &ids);
            return None;
        }
        KeyCode::PageUp => {
            app.here_mut().step(-page, &ids);
            return None;
        }
        KeyCode::PageDown => {
            app.here_mut().step(page, &ids);
            return None;
        }
        KeyCode::Home => {
            app.here_mut().first(&ids);
            return None;
        }
        KeyCode::End => {
            app.here_mut().last(&ids);
            return None;
        }
        KeyCode::Char('/') => {
            let list = app.here_mut();
            list.filter = Some(String::new());
            list.editing_filter = true;
            return None;
        }
        KeyCode::Char('S') => {
            app.here_mut().sort += 1;
            app.reconcile();
            let name = ws.sort_name(app.here().sort);
            app.push(Entry::Notice(format!("sorted by {name}")));
            return None;
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            return None;
        }
        KeyCode::Char('n') => return begin_new(app, ws),
        KeyCode::Char('e') if is_editable(ws) => {
            return selected_label(app, ws).map(|what| Action::Pending {
                verb: format!("edit {what}"),
                needs: "the $EDITOR form ladder — tier 3 of the report's §5.4",
            })
        }
        KeyCode::Char('x') if is_editable(ws) => {
            if let Some(what) = selected_label(app, ws) {
                app.overlay = Overlay::Confirm {
                    verb: delete_verb(ws).to_string(),
                    what,
                };
            }
            return None;
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(target) = Workspace::from_digit(c) {
                app.go(target);
            }
            return None;
        }
        _ => {}
    }

    match ws {
        Workspace::Fleet => on_fleet_key(app, key),
        Workspace::Memory => on_memory_key(app, key),
        Workspace::Schedules => on_schedule_key(app, key),
        Workspace::Goals => on_goal_key(app, key),
        Workspace::Hooks => on_hook_key(app, key),
        Workspace::Tasks => on_task_key(app, key),
        Workspace::Activity => on_activity_key(app, key),
        Workspace::Team => on_team_key(app, key),
        Workspace::Traffic => on_traffic_key(app, key),
        Workspace::Chat | Workspace::MemoryGraph => None,
    }
}

/// The traffic log's own verbs.
///
/// Two of them, because `/` and `S` are the spine's and are already handled
/// above — which is the point of G5.S5: narrowing this list works exactly as
/// narrowing every other one does.
///
/// No I/O, like every key handler here. `f` and `⏎` change what the *next*
/// render draws out of state the tick already loaded; nothing reaches the
/// store, so both are testable without a runtime.
fn on_traffic_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        // The message in full, in the transcript rather than in an overlay: a
        // message is prose of unknown length, and the transcript is the one
        // pane in this program that scrolls.
        KeyCode::Enter => {
            let held = app.selected_is_held();
            let envelope = app.selected_message()?.clone();
            let mut said = format!(
                "{} → {} · {} · depth {}\n{}",
                envelope.message.from,
                envelope.message.to,
                traffic::state_word(&envelope, held),
                envelope.depth,
                envelope.message.text
            );
            // The reason last, so it is the thing left on screen. A refusal
            // that printed its state and buried its cause would be the silence
            // A8 exists to prevent, one layer along.
            if let Some(trouble) = traffic::trouble(&envelope, held) {
                said.push_str(&format!("\n{trouble}"));
            }
            app.push(Entry::Notice(said));
            None
        }
        // The state cycle, spelled as the rail's `f` is. Said out loud, because
        // a filter nobody can see is a screen that looks empty for no reason.
        KeyCode::Char('f') => {
            app.traffic_shown = app.traffic_shown.next();
            app.reconcile();
            app.push(Entry::Notice(format!(
                "showing {}",
                app.traffic_shown.label()
            )));
            None
        }
        _ => None,
    }
}

/// The tree's own keys. `Some` means the tree took the key.
///
/// Returns `Option<Option<Action>>` for the reason [`on_chord`] does: the outer
/// layer says whether it was handled at all, so a key the tree does not know
/// falls through to the fleet's row verbs rather than being swallowed. `s`
/// still stops a run, `d` still delegates.
fn on_tree_key(app: &mut App, key: KeyEvent, viewport: usize) -> Option<Option<Action>> {
    let handled = |a: Option<Action>| Some(a);
    let rows = app.tree_rows();
    let page = viewport.max(1) as isize;
    // The pinned chat, answered before the forest's own keys. Everything below
    // looks the cursor up in `forest` and finds nothing on this row — which on
    // the top row of the screen is four keys that quietly do nothing.
    //
    // `↑`/`↓` are deliberately not here: they step `rows`, which already holds
    // this row first, so the cursor walks off it the same way it walks off any
    // other. Nor are `E`/`C`/`z`/`/`, which are about the tree rather than
    // about the row under the cursor.
    if app.tree_main_selected() {
        match key.code {
            // Not "watch it", which is what `⏎` does to a run, but go *into*
            // it: the chat box binds to the main conversation and its
            // transcript is replayed. `→` as well, so the pair reads as one
            // movement — `←` backs out of a conversation, `→` goes into the one
            // under the cursor.
            KeyCode::Enter | KeyCode::Right => return handled(Some(Action::EnterMain)),
            // Nothing to fold and no parent to climb to. Answered rather than
            // passed down, where `collapse_or_parent` would act on whichever
            // node it found instead — which is not the row that is highlighted.
            KeyCode::Left | KeyCode::Char(' ') => return handled(None),
            _ => {}
        }
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.tree.step(-1, &rows);
            handled(None)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.tree.step(1, &rows);
            handled(None)
        }
        KeyCode::PageUp => {
            app.tree.step(-page, &rows);
            handled(None)
        }
        KeyCode::PageDown => {
            app.tree.step(page, &rows);
            handled(None)
        }
        KeyCode::Home => {
            app.tree.first(&rows);
            handled(None)
        }
        KeyCode::End => {
            app.tree.last(&rows);
            handled(None)
        }
        KeyCode::Right => {
            // A manager row is a conversation, not a branch. Core builds it
            // with no children — see `tree::forest` — so `expand_or_descend`
            // has nothing to open and nothing to descend into, and `→` was a
            // printed key that did nothing.
            //
            // It goes *into* the conversation instead, which is the same pair
            // the pinned chat row makes at the top of this function: `←` backs
            // out of a conversation, `→` goes into the one under the cursor.
            // Run and session rows are left alone — those are not conversations
            // the chat box can bind to, and `⏎` is their key.
            let manager = app
                .selected_node()
                .filter(|node| node.kind == jod_core::tree::NodeKind::Manager)
                .map(|node| node.id.id.clone());
            if let Some(conversation) = manager {
                return handled(Some(Action::EnterManager(conversation)));
            }
            let (forest, closed) = (app.forest.clone(), app.closed_works.clone());
            app.tree.expand_or_descend(&forest, &closed);
            handled(None)
        }
        KeyCode::Left => {
            let (forest, closed) = (app.forest.clone(), app.closed_works.clone());
            app.tree.collapse_or_parent(&forest, &closed);
            handled(None)
        }
        KeyCode::Char(' ') => {
            let closed = app.closed_works.clone();
            app.tree.toggle(&closed);
            handled(None)
        }
        // Capitals, because the lower-case pair is the cursor. `E`/`C` are the
        // two keys that make a forty-node tree navigable at all.
        KeyCode::Char('E') => {
            let forest = app.forest.clone();
            app.tree.expand_all(&forest);
            handled(None)
        }
        KeyCode::Char('C') => {
            let forest = app.forest.clone();
            app.tree.collapse_all(&forest);
            handled(None)
        }
        // The archives. Off by default and off again after looking, because a
        // tree holding everything ever done is one people stop reading.
        KeyCode::Char('z') => {
            app.tree.show_closed = !app.tree.show_closed;
            app.push(Entry::Notice(
                if app.tree.show_closed {
                    "closed works shown, collapsed, below the live ones"
                } else {
                    "closed works hidden"
                }
                .into(),
            ));
            handled(None)
        }
        // `x` — stop tracking the repository the cursor is on.
        //
        // The same letter that forgets a memory and deletes a schedule, because
        // it means the same thing on all three: take this row off this list.
        // Fleet is not in `is_editable`, so the shared `x` above never fires
        // here and this arm is reached.
        //
        // **The cursor has to be on the project row itself.** `T` climbs to the
        // work above it and that is right for a key that navigates; climbing
        // here would mean `x` on a run — one row inside a session inside a work
        // — untracked the whole repository, which is a lot of screen to lose
        // for one keystroke aimed at something small. So it refuses, and the
        // refusal says which row to go to rather than only that this one is
        // wrong.
        KeyCode::Char('x') => {
            // The pinned chat lands here too. It is a sentinel rather than a row
            // in the forest, so `selected_node` answers None for it, and the
            // guard above only intercepts the run verbs — `x` is not one, and
            // adding it there would refuse with a sentence about processes.
            // Left silent it would be a printed key that does nothing and says
            // nothing, which is the thing that guard exists to stop.
            //
            // A fleet with no tree at all never reaches this function; its `x`
            // is answered in `on_fleet_key`.
            let node = app.selected_node().cloned();
            if node.as_ref().map(|n| n.kind) != Some(jod_core::tree::NodeKind::Project) {
                // Three different rows reach this point and they need three
                // different sentences. Saying "the top row of the group this one
                // is in" to all of them was wrong for two: the pinned chat is in
                // no group, and a work with no project *is* a top-level row, so
                // the instruction sent the reader looking upwards at the row
                // they were already on.
                app.push(Entry::Notice(match &node {
                    None => "that is the main chat, not a repository — `x` untracks the \
                             project row the cursor is on"
                        .to_string(),
                    Some(node) => match app.project_above(node) {
                        Some(project) => format!(
                            "untracking is a repository's, so `x` works on a project row — \
                             `{project}`, above this one"
                        ),
                        // No project row above it because there is no project:
                        // the work was opened in a directory the catalog does
                        // not know, so it is drawn at the top level beside the
                        // project rows rather than under one. That is the thing
                        // to say, because the row looks like a repository and
                        // the reason it cannot be untracked is that it is not
                        // one.
                        None => "this work belongs to no catalogued repository, so there is \
                                 nothing to untrack — it sits at the top level because it has \
                                 no project, and `/project add <path>` catalogs one"
                            .to_string(),
                    },
                }));
                return handled(None);
            }
            let node = node.expect("the kind was just matched, so there is a node");
            handled(Some(Action::UntrackProject {
                id: Some(node.id.id.clone()),
                name: node.label.clone(),
            }))
        }
        // `T` — what these agents are saying to each other.
        //
        // From a session or a run as well as from a work, and it opens the
        // *work's* bus in each case: G5.S1 is a message log per work, and a
        // session's own half of a conversation is not a conversation. Walking
        // up is what makes the key work from wherever the cursor happens to be,
        // which is the difference between a screen people find and one they are
        // told about.
        //
        // Capital because `t` retries a run on this screen, and a letter that
        // retried on one press and navigated on the next would be a collision
        // where one of the two is destructive.
        KeyCode::Char('T') => {
            let Some(work) = work_above(app) else {
                app.push(Entry::Notice(
                    "that row belongs to no work, so there is no bus to read — \
                     traffic is a work's, and a session's half of it is not a conversation"
                        .into(),
                ));
                return handled(None);
            };
            app.traffic_of = Some(traffic::Watching::work(work));
            // Emptied rather than left holding the last work's messages: the
            // tick fills it within a frame, and a screen that opens showing
            // another work's conversation is worse than one that opens empty.
            app.traffic = traffic::Log::default();
            app.drill(Workspace::Traffic);
            handled(None)
        }
        // `⏎` opens whatever the row stands for. A run is something to watch; a
        // session is a conversation to go into; a manager is a conversation to
        // go *into* the way the pinned row is; a work and a project are
        // headings, so they toggle rather than pretending to open something.
        KeyCode::Enter => {
            // A row from the pane below the tree, which is a run with no node
            // to be. Answered here rather than left to fall through, because
            // `selected_node` says `None` for it and the arm below would take
            // that for "nothing is selected" on a row that is plainly
            // highlighted.
            if let Some(run) = app
                .tree
                .selected
                .as_ref()
                .filter(|id| fleet::is_loose(id))
                .map(|id| id.id.clone())
            {
                app.go(Workspace::Chat);
                return handled(Some(Action::Watch(run)));
            }
            let Some(node) = app.selected_node().cloned() else {
                return handled(None);
            };
            match node.kind {
                jod_core::tree::NodeKind::Run => {
                    app.go(Workspace::Chat);
                    handled(Some(Action::Watch(node.id.id)))
                }
                jod_core::tree::NodeKind::Session => {
                    handled(Some(Action::Sessions(sessions::Request::Open(node.id.id))))
                }
                // The row carries the conversation id, so this is the id to
                // bind to — the same movement `⏎` on the pinned row makes.
                jod_core::tree::NodeKind::Manager => {
                    handled(Some(Action::EnterManager(node.id.id)))
                }
                // Jod's row *is* the pinned row, so it makes the movement the
                // pinned row already makes rather than binding by id.
                jod_core::tree::NodeKind::Main => handled(Some(Action::EnterMain)),
                jod_core::tree::NodeKind::Work | jod_core::tree::NodeKind::Project => {
                    let closed = app.closed_works.clone();
                    app.tree.toggle(&closed);
                    handled(None)
                }
            }
        }
        _ => None,
    }
}

/// The work the cursor is inside, whichever level of the tree it is on.
///
/// Read out of `work_of` rather than climbed to. The tree no longer *has* work
/// rows to climb to — `fleet::condense` folds them away and hangs their
/// sessions straight off the project — so the answer comes from the map built
/// while the fold still knew it. A project row and the pinned chat belong to no
/// work and answer `None`, which is what the caller reports.
fn work_above(app: &App) -> Option<String> {
    let id = app.tree.selected.as_ref()?;
    app.work_of.get(id).cloned()
}

/// The screens where editing and deleting mean something.
///
/// A run is not edited and an activity line is not deleted, so on those screens
/// `e` and `x` fall through to the screen's own keys rather than offering a
/// verb that cannot exist — which is the same rule that keeps `s` from
/// pretending to stop a finished run.
fn is_editable(ws: Workspace) -> bool {
    matches!(
        ws,
        Workspace::Memory
            | Workspace::Schedules
            | Workspace::Goals
            | Workspace::Hooks
            | Workspace::Tasks
    )
}

/// What `x` is called on each screen. "Forget" rather than "delete" for memory,
/// because that is what it does to you rather than to a row.
fn delete_verb(ws: Workspace) -> &'static str {
    match ws {
        Workspace::Memory => "forget",
        Workspace::Tasks => "remove",
        _ => "delete",
    }
}

/// The name of whatever the cursor is on, for a confirmation that names it.
fn selected_label(app: &App, ws: Workspace) -> Option<String> {
    match ws {
        Workspace::Fleet => app.selected_agent().map(|a| a.name.clone()),
        Workspace::Memory => app.selected_memory().map(|n| n.name.clone()),
        Workspace::Schedules => app.selected_schedule().map(|s| s.name.clone()),
        Workspace::Goals => app.selected_goal().map(|g| g.name.clone()),
        Workspace::Hooks => app.selected_hook().map(|h| h.name.clone()),
        Workspace::Tasks => app.selected_board_task().map(|t| t.id),
        Workspace::Activity => app.selected_activity().map(|a| a.id.clone()),
        Workspace::Team => app.selected_task().map(|t| t.id.clone()),
        Workspace::Traffic => app
            .selected_message()
            .map(|e| format!("message #{}", e.message.id)),
        Workspace::Chat | Workspace::MemoryGraph => None,
    }
}

/// `n` — tier 1 of the form ladder for the kinds whose first question is one
/// value, and a named to-do for the kinds that need the editor.
fn begin_new(app: &mut App, ws: Workspace) -> Option<Action> {
    let label = match ws {
        // The shape is in the label because it is the only place to put it: a
        // box saying "remember" invites a sentence, and a fact is three fields.
        Workspace::Memory => "remember  subject | relation | value",
        Workspace::Tasks | Workspace::Team => "task",
        Workspace::Schedules => "schedule",
        Workspace::Goals => "goal",
        Workspace::Hooks => "webhook",
        _ => return None,
    };
    app.overlay = Overlay::Prompt {
        label: label.to_string(),
        value: String::new(),
        intent: PromptIntent::New(ws),
    };
    None
}

/// What a tier-1 prompt does once `⏎` is pressed.
fn accept_prompt(
    app: &mut App,
    label: String,
    typed: String,
    intent: PromptIntent,
) -> Option<Action> {
    let typed = typed.trim().to_string();
    if typed.is_empty() {
        return None;
    }
    match intent {
        // The board is the one kind the store can already take.
        PromptIntent::New(Workspace::Tasks) | PromptIntent::New(Workspace::Team) => {
            Some(Action::AddTask(typed))
        }
        // A branch named by the `#id` printed beside it.
        //
        // The conversation is read off the fleet cursor rather than carried in
        // the overlay, for the reason `confirmed` gives: an overlay owns the
        // keyboard for as long as it is up, so the selection cannot have moved
        // between `g` and `⏎`.
        //
        // It has to be carried at all — `Request::Restore` takes a
        // *conversation*, and handing it a message id let a number prefix-match
        // a uuid and move the head of a thread nobody was looking at. Parsing
        // and the refusals belong to `sessions::goto`, which is why the typed
        // line goes through unexamined.
        PromptIntent::Branch => {
            let Some(conversation) = app.selected_agent().map(|a| a.id.clone()) else {
                app.push(Entry::Notice(
                    "the fleet moved on — reopen the branch list and try again".into(),
                ));
                return None;
            };
            Some(Action::Sessions(sessions::Request::GoTo {
                conversation,
                branch: typed,
            }))
        }
        // `Store::remember` takes a triple — subject, predicate, object — and
        // splitting one typed line into three would be Jod guessing at which
        // word is the relation. The pipe is the form: three fields on one line,
        // the same shape `/remember` takes, so there is one thing to learn.
        PromptIntent::New(Workspace::Memory) => match command::triple(&typed) {
            Some((subject, predicate, object)) => Some(Action::Remember {
                subject,
                predicate,
                object,
            }),
            // Returned early rather than falling through: the `or_else` below
            // would add "nothing to do" on top of a refusal that has already
            // said what to type instead.
            None => {
                app.push(Entry::Notice(format!(
                    "“{typed}” is a sentence, and memory holds triples — {}",
                    command::REMEMBER_USAGE
                )));
                return None;
            }
        },
        PromptIntent::New(ws) => Some(Action::Pending {
            verb: format!("new {} “{typed}”", ws.menu_name()),
            needs: "the $EDITOR form ladder — tier 3 of the report's §5.4",
        }),
        // Prose, against the card the prompt was opened on rather than against
        // whatever the cursor is on now — the rail re-queries underneath an
        // overlay, so the two are not the same card.
        PromptIntent::AnswerCard(id) => Some(Action::AnswerCard {
            id,
            chosen: None,
            answer: Some(typed.clone()),
        }),
        // Edges are derived from facts rather than written directly, so linking
        // two nodes by hand means asserting the fact the edge would come from —
        // a triple, which a one-line prompt cannot collect.
        PromptIntent::Link(from) => Some(Action::Pending {
            verb: format!("link {from} → {typed}"),
            needs: "a predicate as well as the two ends — edges are folded out of facts",
        }),
    }
    .or_else(|| {
        app.push(Entry::Notice(format!("{label}: nothing to do")));
        None
    })
}

/// The fleet verbs that act on a *run* rather than on the screen.
///
/// Listed once, here, because the two rows that hold no run — a tree's work and
/// session headings, and the pinned chat — both have to answer them, and a list
/// kept in two places is one that drifts the next time a verb is added.
fn is_run_verb(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char(
            's' | 'a' | 'r' | 'd' | 'b' | 'u' | 'U' | 'g' | 'f' | 'm' | 't'
        )
    )
}

/// Why the run under the cursor cannot be carried on, when it cannot.
///
/// The question `continue_agent` asks in `core/src/mcp.rs`, asked again here
/// because `r` is a second way into the same act: it points the next turn at a
/// run's stored session. A session belonging to a run that was killed or failed
/// breaks off wherever the process happened to stop, and the model picks that
/// half-finished state up as though it were its own last turn. Nothing on
/// screen says so — the chat simply opens and looks ready — which is what makes
/// this worth a refusal rather than a warning.
///
/// The answer is a keypress in a terminal rather than a tool's reply, so it is
/// one sentence shorter than the tool's and it names a key instead of a tool.
/// That key is `d`, which starts a fresh agent on the same prompt and exists
/// only on this screen — so the refusal has to leave the cursor here.
///
/// Every status is written out rather than caught by a wildcard, so a fifth one
/// added later has to be decided on here instead of quietly inheriting whichever
/// answer the wildcard gave. A fleet row carries its status as a string, and
/// [`AgentStatus::parse`] is what turns that back into something the compiler
/// can count.
fn refusal_to_continue(name: &str, status: &str) -> Option<String> {
    match AgentStatus::parse(status) {
        // The ordinary target of a follow-up, and the run a second instruction
        // reaches mid-task. Both have a session that means what it says.
        Some(AgentStatus::Completed | AgentStatus::Running) => None,
        Some(AgentStatus::Killed | AgentStatus::Failed) => Some(format!(
            "{name} did not finish cleanly — it is {status}. Continuing it would pick up a \
             turn that broke off part-way, so press d to start a fresh agent instead"
        )),
        // A word this build cannot read, which `list_agents` cannot produce:
        // every row's status is written out of an `AgentStatus` a few lines
        // before it gets here. Let it through rather than refuse on it. A key
        // that stops working because of a status nobody can name is a worse
        // failure than the one this gate exists to prevent, and the run it
        // would be refusing is not known to be dead.
        None => None,
    }
}

fn on_fleet_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    // With works on the board the fleet is a tree, and the arrows mean the
    // tree's things — but that half runs in `on_workspace_key`, *above* the
    // list spine, because the spine owns `↑`/`↓` and would answer them first.
    // What is left here are the row verbs, which the tree does not know: `s`
    // still stops a run, `d` still delegates, on a tree row as on a flat one.
    //
    // The tree's pinned chat is the same case as the flat list's pinned row
    // further down, and it comes first because `selected_node` cannot answer
    // for it: it is a sentinel, not a node in the forest. Left out, these verbs
    // fall through to `selected_agent`, which reads the *flat* list's cursor —
    // so `s` on the main row would stop whichever agent that other, unseen
    // cursor happened to be on.
    if app.tree_main_selected() && is_run_verb(key.code) {
        app.push(Entry::Notice(
            "that is the main chat, not an agent — it has no process to stop or attach to, \
             and ⏎ goes into it"
                .into(),
        ));
        return None;
    }
    // A tree row with no process on it is the same case again, and answered for
    // the same reason: these verbs are all `selected_agent()?`, which on a
    // project heading is a printed key that does nothing and says nothing. `⏎`
    // is named because it is what the row *does* answer — a heading toggles, an
    // agent's row opens its conversation.
    //
    // Asked of `selected_agent` rather than of the row's kind. An agent's row
    // now answers for the run underneath it, so "is there a process here" is no
    // longer the same question as "is this a run row" — and after the fold there
    // are no run rows left for the old test to find.
    if let Some(node) = app.selected_node().filter(|_| app.has_tree()) {
        if is_run_verb(key.code) && app.selected_agent().is_none() {
            let what = match node.kind {
                jod_core::tree::NodeKind::Project => "a project",
                jod_core::tree::NodeKind::Work => "a work",
                jod_core::tree::NodeKind::Manager => "a manager's chat",
                _ => "a session with nothing running on it",
            };
            app.push(Entry::Notice(format!(
                "that row is {what}, not a run — there is no process on it to act on. \
                 Move onto an agent that is running, or press ⏎ to open what this row \
                 does hold"
            )));
            return None;
        }
    }
    // `x` on a fleet with no tree. The keybar advertises it either way, and
    // `on_tree_key` — where the real verb lives — is only called when there is
    // a forest, so without this the key is printed and silent on exactly the
    // screen with no project rows to explain it. The sentence says that rather
    // than sending the reader hunting for a row that is not drawn.
    if !app.has_tree() && key.code == KeyCode::Char('x') {
        app.push(Entry::Notice(
            "no projects on the fleet to untrack — these sessions belong to no work, \
             and `/project add` catalogs a repository"
                .into(),
        ));
        return None;
    }
    // The pinned row is a conversation, not a run, so the verbs that act on a
    // run are answered rather than attempted. Without this branch every one of
    // them is `selected_agent()?` — a silent `None`, which on the top row of
    // the list is a key that looks broken.
    //
    // **Only when the flat list is the thing on screen.** `main_selected` reads
    // that list's cursor, and a fleet drawing the tree keeps two: the tree's is
    // the one highlighted, and the flat one sits wherever the last reconcile
    // left it — the pinned row, on any fleet whose first refresh happened
    // before an agent existed. Ungated, this refused every run verb on the tree
    // with a sentence about the main chat while the cursor was plainly
    // somewhere else. `tree_main_selected` above is the same guard for the
    // cursor that *is* drawn.
    if !app.has_tree() && app.main_selected() {
        match key.code {
            // The verb the pinned chat never had: not "watch it", which is what
            // ⏎ does to a run, but *go into it* — the chat box binds and the
            // transcript is replayed.
            KeyCode::Enter | KeyCode::Right => return Some(Action::EnterMain),
            KeyCode::Char('s') | KeyCode::Char('a') | KeyCode::Char('r') => {
                app.push(Entry::Notice(
                    "that is the main chat, not an agent — it has no process to stop or attach \
                     to, and ⏎ goes into it"
                        .into(),
                ));
                return None;
            }
            // Everything else is a verb about the *list* rather than about the
            // row — `c` opens the conversation graph, and it is the way out of
            // a fleet with nothing in it. Falling through rather than swallowing
            // them keeps the pinned row from disabling the screen it sits on.
            _ => {}
        }
    }
    match key.code {
        // Reading a run is the common case, so it is the plain key.
        //
        // `→` is the same verb spelled spatially: `←` backs out of a run into
        // this list, `→` goes into the one under the cursor. Having both means
        // the pair reads as one movement rather than as a key and its unrelated
        // opposite.
        KeyCode::Enter | KeyCode::Right => {
            let id = app.selected_agent()?.id.clone();
            app.go(Workspace::Chat);
            Some(Action::Watch(id))
        }
        KeyCode::Char('s') => {
            let agent = app.selected_agent()?;
            let (id, running, status) =
                (agent.id.clone(), agent.is_running(), agent.status.clone());
            if !running {
                // Killing a finished run only reclaims its tmux session, which
                // is not what "s" looks like it does. Say so instead.
                app.push(Entry::Notice(format!(
                    "{} is already {status} — nothing to stop",
                    short(&id)
                )));
                return None;
            }
            Some(Action::Stop(id))
        }
        KeyCode::Char('a') => Some(Action::Attach(app.selected_agent()?.id.clone())),
        // Continue the selected agent's conversation from the input box, which
        // is how an unattended run gets picked up and corrected.
        KeyCode::Char('r') => {
            let agent = app.selected_agent()?.clone();
            // Asked before the screen moves and before the session id, for two
            // reasons. How a run ended is a fact about the run, while a missing
            // session id is a fact about the mechanism for resuming one, so a
            // killed run that also lost its session id is better told it was
            // killed. And the way out of this refusal is `d`, which is a fleet
            // key: answering here leaves the cursor on the row it is about.
            if let Some(refusal) = refusal_to_continue(&agent.name, &agent.status) {
                app.push(Entry::Notice(refusal));
                return None;
            }
            app.go(Workspace::Chat);
            match agent.session {
                Some(session) => {
                    app.resume = Resume::Session(session.clone());
                    app.session = Some(session);
                    app.harness_from_label(&agent.harness);
                    app.push(Entry::Notice(format!(
                        "next turn continues {} — type to carry on",
                        agent.name
                    )));
                    // Out of the main chat, if that is where you were: the next
                    // turn goes to this agent, and leaving the binding set
                    // would send it to the orchestrator instead.
                    return Some(Action::NewThread);
                }
                None => app.push(Entry::Notice(format!(
                    "{} never reported a conversation, so there is nothing to continue",
                    agent.name
                ))),
            }
            None
        }
        // Run the same prompt again as a fresh background agent — the fastest
        // way to retry something that nearly worked.
        KeyCode::Char('d') => {
            let agent = app.selected_agent()?;
            Some(Action::Delegate(agent.name.clone()))
        }
        // ---- the conversation graph ------------------------------------
        //
        // Five keys, and only the first is about the *list*. The other four are
        // about the run under the cursor, because that is the handle this
        // screen already has: a fleet row is a run, `Store::conversation_for_run`
        // turns one into a thread, and `sessions::resolve` makes that the
        // caller's problem instead of this function's.
        //
        // `c` opens the session list — every conversation, with a cursor on it.
        //
        // It used to print the same list into the transcript, which was a list
        // you could read and not a list you could use: the fleet's rows are
        // *runs*, so the only way from here back into a thread that no longer
        // has a run on screen was to read an id off the printout and type
        // `/resume` at it. The overlay is the same rows with the last step
        // joined on.
        KeyCode::Char('c') => {
            app.overlay = Overlay::Sessions(sessions::Browser::default());
            None
        }
        // `b` — the branches of the selected run's thread: the turns, which one
        // the head is on, and every leaf a revert left behind, each named.
        KeyCode::Char('b') => Some(Action::Sessions(sessions::Request::Open(
            app.selected_agent()?.id.clone(),
        ))),
        // `u` undoes the last turn and `U` puts it back — the pair OpenCode
        // ships as `revert`/`unrevert`.
        //
        // Lowercase is *undo*, and the capital is the inverse, because
        // `on_memory_key` already spells undo `u`. The usual defence for a
        // letter meaning two things — `a` attaches here and answers an
        // escalation in goals — does not cover this pair: those are unrelated
        // verbs, so nothing transfers, while undo and redo are one verb
        // inverted and the muscle memory transfers exactly. Reaching for `u` on
        // a screen of destructive-looking verbs and getting redo is the one
        // collision worth spending a capital on, and `S` for sort and `M` for
        // mark-all already set that pattern.
        //
        // Neither is behind `Overlay::Confirm`, and that is a decision rather
        // than an omission. That overlay's frame reads "this cannot be undone",
        // which is true of `x` on a webhook and false of every verb here:
        // `revert_to` keeps every row and every parent edge on purpose, and
        // `move_head` exists precisely so the head can be put back. Making a
        // reversible act wear an irreversible warning teaches the user to click
        // through the warning that matters. What these do instead is name the
        // way back in the same breath — see `sessions::rewind`.
        KeyCode::Char('u') => Some(Action::Sessions(sessions::Request::Rewind(
            app.selected_agent()?.id.clone(),
        ))),
        KeyCode::Char('U') => Some(Action::Sessions(sessions::Request::Restore(
            app.selected_agent()?.id.clone(),
        ))),
        // `g` — go to a branch by the `#id` printed beside it.
        //
        // `U` takes the newest tip, which is the only case most people ever
        // have: undo, then change your mind. This is for the rest — three or
        // more branches set aside, and the one you want is not the last one you
        // left. Without it those branches are listed, numbered, and
        // unreachable, which is worse than not listing them: it shows you
        // something and gives you no way to get to it.
        KeyCode::Char('g') => {
            app.selected_agent()?;
            app.overlay = Overlay::Prompt {
                label: "go to branch #".to_string(),
                value: String::new(),
                intent: PromptIntent::Branch,
            };
            None
        }
        // `f` forks at the head: a second conversation from this point, sharing
        // the prefix rather than copying it. It writes one row and destroys
        // nothing, so it asks nothing either.
        KeyCode::Char('f') => Some(Action::Sessions(sessions::Request::Fork(
            app.selected_agent()?.id.clone(),
        ))),
        // `m` — did the message this run owed anybody actually arrive?
        //
        // The fleet says `completed`, and `completed` is silent about the one
        // thing the ledger was written to record: a run can finish, say its
        // piece, and the person it was for hear nothing. Answered here rather
        // than only in `jod ledger`, because the question is asked while
        // looking at the run, and a ledger read in another program answers too
        // late to change what anybody does next.
        //
        // Most runs owe nobody anything and say so — that is not the same
        // answer as "delivered", and `delivery::about_run` keeps them apart.
        KeyCode::Char('m') => Some(Action::Sessions(sessions::Request::Delivery(
            app.selected_agent()?.id.clone(),
        ))),
        // `t` — try the last question again. The answer it got stays where it
        // is and the new attempt lands beside it, so this is "regenerate" with
        // both attempts kept rather than the second painted over the first.
        KeyCode::Char('t') => Some(Action::Sessions(sessions::Request::Retry(
            app.selected_agent()?.id.clone(),
        ))),
        // `T` belongs to the tree, and is answered here for the case where
        // there is no tree — a fleet of sessions started before works existed.
        // The keybar prints it on this screen whatever the fleet holds, so
        // falling through would be an advertised key that silently does
        // nothing, which is exactly the trap the keymap's drift net exists to
        // stop one spelling of.
        KeyCode::Char('T') => {
            app.push(Entry::Notice(
                "traffic is a work's bus, and nothing here belongs to a work yet — \
                 delegate something and the tree will have one"
                    .into(),
            ));
            None
        }
        _ => None,
    }
}

fn on_memory_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => None,
        // The local graph is memory's second level, so it is drilled into
        // rather than jumped to: Esc comes back to the list.
        KeyCode::Char('g') => {
            let id = app.selected_memory()?.id.clone();
            app.graph = graph::GraphView::new(id);
            app.drill(Workspace::MemoryGraph);
            None
        }
        KeyCode::Char('t') => {
            app.memory_type = next_memory_type(app.memory_type);
            app.reconcile();
            let what = app
                .memory_type
                .map(|k| k.label().to_string())
                .unwrap_or_else(|| "every kind".into());
            app.push(Entry::Notice(format!("showing {what}")));
            None
        }
        KeyCode::Char('l') => {
            let from = app.selected_memory()?.name.clone();
            app.overlay = Overlay::Prompt {
                label: format!("link {from} →"),
                value: String::new(),
                intent: PromptIntent::Link(from),
            };
            None
        }
        // Memory writes are already events, so the last one could be un-written
        // — which is strictly better than a confirmation dialog. Nothing keeps
        // the order of writes yet, so `x` asks instead.
        KeyCode::Char('u') => Some(Action::Pending {
            verb: "undo the last memory write".into(),
            needs: "a record of the last write — Store::forget cannot be reversed",
        }),
        _ => None,
    }
}

fn next_memory_type(current: Option<data::MemoryKind>) -> Option<data::MemoryKind> {
    let all = data::MemoryKind::ALL;
    match current {
        None => Some(all[0]),
        Some(kind) => match all.iter().position(|k| *k == kind) {
            Some(at) if at + 1 < all.len() => Some(all[at + 1]),
            _ => None,
        },
    }
}

fn on_graph_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let node = app.focused_memory().cloned();
    let rows = node
        .as_ref()
        .map(|n| graph::neighbours(n, app.graph.edge_kind.as_deref()))
        .unwrap_or_default();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g') => {
            app.back();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.graph.step(-1, rows.len());
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.graph.step(1, rows.len());
            None
        }
        KeyCode::Home => {
            app.graph.sel = 0;
            None
        }
        KeyCode::End => {
            app.graph.sel = rows.len().saturating_sub(1);
            None
        }
        KeyCode::Enter => {
            let row = rows.get(app.graph.sel)?;
            app.graph.recentre(row.edge.other.clone());
            // The list's cursor follows the eye, so leaving the graph lands on
            // the node you were last looking at rather than the one you left.
            app.list_mut(Workspace::Memory).selected = Some(app.graph.focus.clone());
            None
        }
        // Walking a graph without being able to walk back out of it is how you
        // get lost in one. An empty stack leaves the graph entirely.
        KeyCode::Backspace => {
            if !app.graph.back() {
                app.back();
            } else {
                app.list_mut(Workspace::Memory).selected = Some(app.graph.focus.clone());
            }
            None
        }
        KeyCode::Char('h') => {
            app.graph.toggle_hops();
            None
        }
        KeyCode::Char('f') => {
            let kinds = node.as_ref().map(graph::edge_kinds).unwrap_or_default();
            app.graph.cycle_edge_kind(&kinds);
            None
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Keymap;
            None
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(target) = Workspace::from_digit(c) {
                app.go(target);
            }
            None
        }
        _ => None,
    }
}

fn on_schedule_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let row = app.selected_schedule()?.clone();
    let name = row.name.clone();
    match key.code {
        KeyCode::Enter => Some(Action::OpenScheduleRun(name)),
        KeyCode::Char('r') => Some(Action::RunSchedule(name)),
        KeyCode::Char('p') => Some(Action::ToggleSchedule(name)),
        // Dry run: the honest answer to "did I get the cron right", which no
        // amount of staring at `0 2 * * *` gives you. Nothing is stored and
        // nothing is read, so it is answered here rather than as an action.
        KeyCode::Char('t') => {
            for line in next_fires(&row.cron, &row.timezone, app.now_ms, DRY_RUN_FIRES) {
                app.push(Entry::Notice(line));
            }
            None
        }
        _ => None,
    }
}

/// How many fire times `t` prints. Five is enough to see a weekly pattern and
/// short enough not to bury the transcript.
const DRY_RUN_FIRES: usize = 5;

/// When a cron expression would next fire, as lines for the transcript.
///
/// A refusal is a line too. An expression the store already accepted can still
/// fail here — a timezone the build has no data for — and printing nothing
/// would make the key look broken rather than the schedule.
fn next_fires(cron: &str, timezone: &str, from_ms: i64, how_many: usize) -> Vec<String> {
    let mut lines = vec![format!("{cron} in {timezone}, shown in local time:")];
    let mut at = from_ms;
    for _ in 0..how_many {
        match jod_core::schedule::next_fire(cron, timezone, at) {
            Ok(Some(next)) => {
                lines.push(format!("  {}", clock(next)));
                at = next;
            }
            // A cron expression can genuinely run out — `0 0 30 2 *` names a
            // day that never comes — and saying so is the point of a dry run.
            Ok(None) => {
                lines.push("  and never again".to_string());
                break;
            }
            Err(e) => {
                lines.push(format!("  cannot be read: {e}"));
                break;
            }
        }
    }
    lines
}

fn on_goal_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let name = app.selected_goal().map(|g| g.name.clone())?;
    match key.code {
        // An iteration leaves an episodic fact and nothing else: `current-run`
        // is superseded every time round, so by the time an iteration is in the
        // log the id that would name its run is gone. Opening one needs the
        // ticker to keep the run id on the `iteration` fact.
        KeyCode::Enter => Some(Action::Pending {
            verb: format!("open {name}'s last iteration"),
            needs: "the run id on the iteration fact — the ticker supersedes it away",
        }),
        KeyCode::Char('r') => Some(Action::RunGoal(name)),
        KeyCode::Char('p') => Some(Action::ToggleGoal(name)),
        // A looping objective that quietly needs you and never says so is worse
        // than no goal at all. Reading the escalation works — it is on the
        // screen — but answering it has nowhere to go.
        KeyCode::Char('a') => Some(Action::Pending {
            verb: format!("answer {name}'s escalation"),
            needs: "Store::answer_escalation, which does not exist yet",
        }),
        _ => None,
    }
}

fn on_hook_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let hook = app.selected_hook()?;
    let (name, endpoint) = (hook.name.clone(), hook.endpoint.clone());
    // The deliveries are already loaded and already carry the run each one
    // started, so `⏎` needs no store call — unlike a schedule, where the run
    // lives in a fire record the row does not hold.
    let last_run = hook.deliveries.iter().find_map(|d| d.run.clone());
    match key.code {
        KeyCode::Enter => match last_run {
            Some(run) => Some(Action::Watch(run)),
            None => {
                app.push(Entry::Notice(format!(
                    "no delivery to {name} has started a run yet"
                )));
                None
            }
        },
        // Not a store verb at all: a test delivery has to be *posted* at the
        // running daemon, signed, so that it goes through the same signature
        // check and the same rule match as a real one.
        KeyCode::Char('t') => Some(Action::Pending {
            verb: format!("test {name} with a sample payload"),
            needs: "a signed POST at the running jod-api, which the TUI cannot reach",
        }),
        KeyCode::Char('p') => Some(Action::ToggleHook(name)),
        KeyCode::Char('c') => {
            // No clipboard daemon and no OSC 52 yet, so the URL goes where it
            // can always be copied from: the transcript.
            app.push(Entry::Notice(format!("{name}: {endpoint}")));
            None
        }
        _ => None,
    }
}

fn on_task_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let task = app.selected_board_task()?;
    match key.code {
        KeyCode::Enter => {
            if task.state == data::TaskState::Done {
                app.push(Entry::Notice(format!("{} is already done", task.id)));
                return None;
            }
            Some(Action::FinishTask(task.id))
        }
        // The verb that makes the board worth a screen: it turns a task into an
        // agent run, so the board is where work starts rather than a list kept
        // in parallel with the fleet.
        KeyCode::Char('d') => Some(Action::Delegate(delegation_prompt(&task))),
        // `Store::claim_task` takes an owner, and this session has no name on
        // any board — a TUI started with `--team` watches a team without being
        // a member of it. Claiming as nobody would put a row in a state no
        // teammate could take back.
        KeyCode::Char('c') => Some(Action::Pending {
            verb: format!("claim {}", task.id),
            needs: "a member name for this session — Store::claim_task claims as somebody",
        }),
        KeyCode::Char('o') => match task.run {
            Some(run) => Some(Action::Watch(run)),
            None => {
                app.push(Entry::Notice(format!("{} has no run yet", task.id)));
                None
            }
        },
        _ => None,
    }
}

/// The prompt `d` seeds an agent with: the title, the runnable check, and the
/// spec, so the run starts knowing what "done" means.
fn delegation_prompt(task: &data::TaskRow) -> String {
    let mut prompt = task.title.clone();
    if !task.check.trim().is_empty() {
        prompt.push_str(&format!("\n\nIt is done when this passes: {}", task.check));
    }
    if let Some(spec) = &task.spec {
        prompt.push_str(&format!("\n\nThe spec is {spec}."));
    }
    prompt
}

/// Keys on the activity feed.
///
/// `m` and `M` mark read in memory, which is the right optimistic behaviour —
/// but the feed is re-read from the store on every fourth tick, so once
/// `Store::activity` exists these two **must** also write through
/// (`Store::mark_activity_read`) or the tick will quietly un-read them. Noted
/// here rather than in the loader, because this is the call site that would
/// look correct and not be.
fn on_activity_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => {
            let (ws, id) = app.selected_activity()?.jump_to.clone()?;
            app.go(ws);
            app.list_mut(ws).selected = Some(id);
            None
        }
        KeyCode::Char('m') => {
            let id = app.selected_activity()?.id.clone();
            if let Some(item) = app.activity.iter_mut().find(|a| a.id == id) {
                item.unread = false;
            }
            app.reconcile();
            None
        }
        KeyCode::Char('M') => {
            for item in &mut app.activity {
                item.unread = false;
            }
            app.reconcile();
            None
        }
        KeyCode::Char('u') => {
            app.unread_only = !app.unread_only;
            app.reconcile();
            None
        }
        KeyCode::Char('f') => {
            app.activity_source = next_source(app.activity_source);
            app.reconcile();
            None
        }
        _ => None,
    }
}

fn next_source(current: Option<data::Source>) -> Option<data::Source> {
    let all = data::Source::ALL;
    match current {
        None => Some(all[0]),
        Some(source) => match all.iter().position(|s| *s == source) {
            Some(at) if at + 1 < all.len() => Some(all[at + 1]),
            _ => None,
        },
    }
}

fn on_team_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => {
            let task = app.selected_task()?;
            if task.is_done() {
                app.push(Entry::Notice(format!("{} is already done", task.id)));
                return None;
            }
            Some(Action::FinishTask(task.id.clone()))
        }
        _ => None,
    }
}

/// Keys in chat, where letters are text and the input box owns them.
fn on_chat_key(
    app: &mut App,
    thread: &mut Thread,
    key: KeyEvent,
    viewport: usize,
) -> Option<Action> {
    // Consumed here, once, regardless of which arm below ends up handling the
    // key — Tab or Enter on the suggestion list is reading the offer exactly
    // as intended, and every other key means the box is being edited by hand
    // either way. Only a plain character typed while this was still true
    // means "that offer was never read"; see the `KeyCode::Char(c)` arm.
    let offer_was_unread = std::mem::take(&mut thread.model_offer_unread);
    let max_scroll = app.transcript.len();

    // The `@` popup owns the arrows and `⏎` while it is up, and `Esc` closes it
    // *without* touching the line — D1's fourth requirement, and the one people
    // notice: a picker that wipes what you typed when you change your mind is a
    // picker you stop opening.
    //
    // Everything else falls through to the ordinary editing keys and then
    // re-derives the popup from the line, so typing, backspacing and moving the
    // cursor all keep it honest without each one having to remember to.
    if app.mention.is_some() {
        match key.code {
            KeyCode::Up => {
                if let Some(popup) = &mut app.mention {
                    popup.prev();
                }
                return None;
            }
            KeyCode::Down => {
                if let Some(popup) = &mut app.mention {
                    popup.next();
                }
                return None;
            }
            KeyCode::Tab | KeyCode::Enter => {
                // With no roots there is nothing to accept, so the key does
                // nothing at all rather than closing the popup on a shrug — the
                // message stays up, which is the answer to "why is this empty".
                if app.accept_mention() {
                    return None;
                }
                if app.mention.as_ref().is_some_and(|p| !p.rooted) {
                    return None;
                }
            }
            KeyCode::Esc => {
                app.mention = None;
                return None;
            }
            _ => {}
        }
    }

    // While the completion popup is up it owns Tab and the arrows, and Enter
    // finishes the word rather than sending a half-typed command.
    let suggestions = if app.completions_dismissed {
        Vec::new()
    } else {
        command::completions(&app.input, app)
    };
    if !suggestions.is_empty() {
        // Escape closes the popup and nothing else. It used to fall through to
        // `back()`, which scrolls the transcript — so the list stayed up, no
        // key dismissed it, and its own header offered none.
        if key.code == KeyCode::Esc {
            app.completions_dismissed = true;
            return None;
        }
        app.clamp_suggestion(suggestions.len());
        match key.code {
            KeyCode::Tab => {
                let line = suggestions[app.suggestion].line.clone();
                app.accept_completion(&line);
                return None;
            }
            KeyCode::Up => {
                app.prev_suggestion(suggestions.len());
                return None;
            }
            KeyCode::Down => {
                app.next_suggestion(suggestions.len());
                return None;
            }
            // Enter picks the highlighted suggestion only when that would
            // actually change the line. Otherwise the command is already fully
            // typed and Enter must run it: a command needing an argument
            // completes to *itself* plus a space, so accepting here appended an
            // invisible space, swallowed the keypress, and left the text in the
            // box to corrupt whatever was typed next.
            KeyCode::Enter
                if suggestions[app.suggestion].line.trim_end() != app.input.trim_end() =>
            {
                let line = suggestions[app.suggestion].line.clone();
                app.accept_completion(&line);
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            // A slash line is an instruction to Jod, not to the agent, so it
            // works while an agent is busy — switching the model for the *next*
            // turn is exactly the sort of thing you do while waiting.
            let parsed = command::parse(app.input.trim());
            // A repository's own command sits in exactly one gap: after Jod's
            // own names, before the line becomes prose.
            //
            // Both edges are bugs a test caught here. Checked *first*, a repo
            // shipping a `/model` would shadow Jod's — and Jod's `/model`
            // changes this program's state, which no repository should be able
            // to take over by choosing a filename. Checked *last*, it would
            // never run at all: `parse` answers `Slash::Unknown` rather than
            // `None` for an unrecognised name, so the line would be reported as
            // an unknown command instead of forwarded.
            //
            // Hence the condition: only where Jod has no opinion.
            let jod_has_no_opinion =
                matches!(parsed, None | Some(command::Slash::Unknown(_)));
            if jod_has_no_opinion {
                if let Some((name, invocation)) = command::repo_invocation(&app.input, app) {
                    let typed = app.input.trim().to_string();
                    app.remember_typed(&typed);
                    app.input.clear();
                    app.cursor = 0;
                    app.push(Entry::You(format!("/{name}")));
                    return Some(Action::RunCommand {
                        prompt: invocation.prompt,
                        command: invocation.command,
                    });
                }
            }
            if let Some(slash) = parsed {
                let typed = app.input.trim().to_string();
                app.remember_typed(&typed);
                app.input.clear();
                app.cursor = 0;
                return apply_slash(app, slash);
            }
            let prompt = app.take_input()?;
            // Queued rather than refused. The old behaviour left the sentence
            // sitting in the box with a scolding, so the box stayed blocked and
            // the only thing to do while an agent worked was to sit still.
            if app.busy {
                app.queue(prompt);
                app.push(Entry::Notice(format!(
                    "queued — sends when this turn ends ({} waiting)",
                    app.queued.len()
                )));
                return None;
            }
            return Some(Action::Send(prompt));
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_forward(),
        // `←` on an *empty* input backs out of the run into the agents list,
        // and does it on the one press. With anything typed it is the cursor, as
        // it must be: the same rule `?` follows two arms down, for the same
        // reason — stealing a key people use to edit text is not worth any
        // shortcut.
        //
        // Only when there is something to leave: `←` on an idle main chat is a
        // dead key rather than a trip to the fleet, because there is no run to
        // stop looking at. See [`background`] for why nothing is asked.
        KeyCode::Left if app.input.is_empty() && (app.busy || app.watching.is_some()) => {
            background(app);
        }
        // A manager is something to leave even when nothing is running, which
        // is the difference between it and the main chat. You got here by
        // pressing `⏎` on a row one screen away, so the way back has to be the
        // one key that already means "out" — and it was a dead key, because the
        // arm above only fires while a run is on screen and a manager sitting
        // idle has none.
        //
        // The main chat deliberately keeps the old behaviour. It is home rather
        // than somewhere you went, so there is nothing to back out *to*.
        KeyCode::Left
            if app.input.is_empty()
                && thread
                    .conversation
                    .as_deref()
                    .is_some_and(|id| app.is_manager_conversation(id)) =>
        {
            let conversation = thread
                .conversation
                .clone()
                .expect("the guard just read it");
            leave_manager(app, &conversation);
        }
        KeyCode::Left => app.left(),
        KeyCode::Right => app.right(),
        KeyCode::Home => app.home(),
        KeyCode::End => app.end(),
        // The arrows recall what has been sent, as every shell and every other
        // agent CLI does. Scrolling the transcript is PageUp/PageDown, the
        // mouse, or Ctrl with an arrow.
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        KeyCode::PageUp => app.scroll_up(viewport.max(1), max_scroll),
        KeyCode::PageDown => app.scroll_down(viewport.max(1)),
        // **Stop, but stay.** The most-used key in a coding harness: you see a
        // turn going the wrong way in the first two seconds and you correct it.
        //
        // Everything that makes this "interrupt" rather than `Ctrl-X`'s "kill"
        // happens *here*, in a pure handler, and none of it touches
        // `App::session` or `App::resume`. Those two are the harness's
        // conversation id and how the next turn continues it — the run is one
        // process of many in that conversation, so ending the process ends the
        // turn and leaves the conversation exactly where it was. The next thing
        // typed carries on the same session, which is the whole point and is
        // what `escape_interrupts_the_turn_without_losing_the_session` pins.
        //
        // Only the kill itself travels as an `Action`, because only the loop
        // has the service. Marking the turn over here rather than there is
        // deliberate: the reader gets the input box back on the keypress rather
        // than after a round trip through the supervisor.
        KeyCode::Esc if app.busy && app.watching.is_some() => {
            let id = app.watching.clone().expect("just checked");
            // Recorded as what it was. A partial turn silently dropped would
            // leave the transcript claiming the agent simply stopped talking,
            // and the next reader cannot tell an interruption from a crash.
            app.push(Entry::Done {
                text: match app.elapsed() {
                    Some(t) => format!("interrupted after {t}"),
                    None => "interrupted".to_string(),
                },
                failed: false,
            });
            app.push(Entry::Notice(
                "stopped — the conversation is kept, so just say what to do instead".into(),
            ));
            app.busy = false;
            app.turn_started_ms = None;
            // Named as being stopped, not merely stopped. The kill itself takes
            // a moment — the signal goes out and the harness winds down — and
            // for that moment the status bar has something true to say instead
            // of `working` over a clock that has stopped. It is also what tells
            // the run's own ending, when it arrives, that it was asked for.
            app.interrupting = Some(id.clone());
            // The line being typed is left alone: the correction is usually
            // already half-written by the time you reach for Escape.
            return Some(Action::Interrupt(id));
        }
        KeyCode::Esc => app.back(),
        // `?` on an *empty* input opens the keymap; with anything typed it is
        // the character it looks like. Backspacing down to a lone `?` therefore
        // never fires it, which is the edge case that makes the rule usable.
        KeyCode::Char('?') if app.input.is_empty() => app.overlay = Overlay::Keymap,
        KeyCode::Char(c) => {
            // The line on screen is `offer_models`'s prefill, not anything the
            // user typed — so this character is not extending a `/model`
            // invocation, it is the first letter of whatever they actually
            // meant to say. Dropping the offer here, rather than appending
            // into it, is the whole fix for `harness-eats-prompt`: without
            // this, "PONG" typed after a switch became "/model PONG" and the
            // turn that should have run silently never spawned.
            if offer_was_unread {
                app.input.clear();
                app.cursor = 0;
            }
            // `@` opens the picker under the cursor. The index is taken before
            // the insert because the popup replaces the sign along with the
            // query, so it has to know where the sign landed.
            let at = app.cursor;
            app.insert(c);
            if c == '@' {
                app.open_mention(at);
            }
            // Typing means the recalled line is now the user's own draft, so
            // ↓ must not silently replace what they are editing.
            app.history_at = None;
        }
        _ => {}
    }
    // Derived rather than tracked: every edit key above would otherwise have to
    // remember to keep the popup in step with the line, and the one that forgot
    // would leave it ranking a query the line no longer holds.
    app.sync_mention();
    None
}

/// Carry out a slash command. Everything it touches is app state, so this
/// stays synchronous and testable; anything needing the service comes back as
/// an `Action` for the loop to run.
fn apply_slash(app: &mut App, slash: command::Slash) -> Option<Action> {
    use command::Slash;
    match slash {
        Slash::Help => {
            for (usage, what) in command::HELP {
                app.push(Entry::Notice(format!("{usage:<18} {what}")));
            }
        }
        // Handed straight back rather than applied here. Switching harness used
        // to mean *dropping* the conversation — fresh resume, no session, no
        // model — with the whole thread sitting in the graph unread. Carrying it
        // across needs a summary, a summary needs a model, and Jod has no model
        // client: so the first half of this is a run, and a run belongs to the
        // loop. `perform` decides whether one is owed at all.
        Slash::Harness(kind) => return Some(Action::SwitchHarness(kind)),
        // No argument means the harness on screen: it is the one that just
        // refused to run, which is why anybody is typing this.
        Slash::Login(named) => return Some(Action::SignIn(named.unwrap_or(app.harness))),
        // Applied to the app *and* handed back to be written down. Neither of
        // these was ever a property of the process: the harness is respawned
        // once per turn, so both are re-read at every spawn, and a choice held
        // only in this struct lasted until the next `jod tui` and no longer.
        Slash::Model(model) => {
            // Checked here rather than in `command::parse`, which is a
            // harness-agnostic sieve and has no list to check against. Refused
            // rather than warned about: a name this harness has no model for
            // does not degrade the next turn, it kills it, and every turn after
            // it, with an error that names neither the model nor the cause.
            // Storing it would be storing a broken conversation.
            //
            // Only ever refuses against a list that actually loaded, so
            // `/model <anything>` still works when the harness could not be
            // asked — the list stays an aid there, exactly as before.
            if let Some(objection) = model.as_deref().and_then(|m| app.model_objection(m)) {
                app.push(Entry::Notice(objection));
                return None;
            }
            let said = match &model {
                Some(m) => format!("model: {m}"),
                None => "model: the harness default".to_string(),
            };
            app.model = model.clone();
            // The last run's model stops being the answer the moment another
            // one is asked for. `status` prefers what the harness reported over
            // what was requested — rightly, for a run that has happened — but
            // nothing had cleared it here, so `/model haiku` printed "model:
            // haiku" while the status bar went on saying `claude-opus-5` until
            // some later turn overwrote it. Two lines disagreeing about the
            // model is indistinguishable from the switch not working.
            app.reported_model = None;
            app.push(Entry::Notice(said));
            return Some(remember_model(model));
        }
        // Naming a mode sets it; naming none moves to the next, so `/mode` and
        // the Tab key are one setting reached two ways rather than two.
        Slash::Mode(mode) => {
            let said = match mode {
                Some(m) => {
                    app.mode = m;
                    format!("mode: {}", m.label())
                }
                None => app.cycle_mode(),
            };
            app.push(Entry::Notice(said));
            // `app.mode` rather than `mode`: cycling picked one, and this has to
            // record the mode arrived at, not the argument that was absent.
            return Some(remember_mode(app.mode));
        }
        // The two toggles apply now and are recorded as a choice, so the answer
        // to "do I want to watch it think" is given once rather than at every
        // launch. Nothing is said here: the sentence comes back from the write,
        // which is the only place that knows whether it stuck.
        Slash::Thinking => {
            app.show_thinking = !app.show_thinking;
            return Some(remember_flag(config::Pref::Thinking, app.show_thinking));
        }
        Slash::Details => {
            app.show_details = !app.show_details;
            return Some(remember_flag(config::Pref::Details, app.show_details));
        }
        // Setting a preference that has a live twin on the app changes the live
        // one too, or `/config thinking off` would take effect at the *next*
        // launch and look broken at this one.
        Slash::Config(request) => {
            if let config::Request::Set(pref, value) = &request {
                match (pref, value.flag()) {
                    (config::Pref::Thinking, Some(on)) => app.show_thinking = on,
                    (config::Pref::Details, Some(on)) => app.show_details = on,
                    // `harness`, `model` and `mode` are deliberately not applied
                    // to this session: they are what a *new* session starts
                    // with, and `/harness`, `/model` and `/mode` are how you
                    // change the one you are in. Two commands, two scopes, both
                    // said out loud.
                    _ => {}
                }
                // The same offer `/harness` makes, one scope up: the default
                // model has just been dropped for belonging to the harness you
                // left, so the names that could replace it are the next thing
                // wanted. `/config model ` rather than `/model `, because the
                // choice being made here is about new conversations and not
                // this one.
                //
                // Withheld when the harness just chosen is not the one this
                // session is on. The loaded list belongs to `app.harness` and
                // nothing has asked the other harness for its own, so opening
                // the picker there would offer Claude Code's names for a
                // preference that is now about OpenCode — the exact failure
                // `/model`'s live list was built to end. A notice can still be
                // right where a list cannot: `apply` names the dropped model.
                if let (config::Pref::Harness, Some(chosen)) = (pref, harness_of(value)) {
                    if chosen == app.harness {
                        offer_models(app, "/config model ");
                    }
                }
            }
            return Some(Action::Config(request));
        }
        Slash::New => {
            app.resume = Resume::Fresh;
            app.session = None;
            app.cost_usd = 0.0;
            go_home(app);
            // A hint, like the startup line: `/new` clears the screen back to
            // the splash on purpose, and a notice here would be output the
            // splash was covering. See `ui::fresh`.
            app.push(Entry::Hint("new conversation".into()));
            // Jod's conversation as well as the harness's. Without this, a
            // conversation handed over by `/harness` would keep collecting the
            // turns of the fresh one that replaced it — and it is how you leave
            // the main chat, which binds the same field.
            return Some(Action::NewThread);
        }
        Slash::Root(what) => {
            return match what {
                command::RootCmd::List => Some(Action::ListRoots),
                // No path means the picker, which is the same screen `Ctrl-P`
                // opens — one picker, reached two ways, rather than a second
                // one that would drift.
                command::RootCmd::Add(None) => {
                    open_picker(app, launch_dir());
                    None
                }
                command::RootCmd::Add(Some(path)) => Some(Action::AddRoot(PathBuf::from(path))),
                // `/add-dir <where>` — the same picker, started somewhere you
                // name. Refused rather than opened empty when the name is not
                // a directory: a picker with no rows would read as "nothing
                // here" when the truth is "that is not a place".
                command::RootCmd::AddFrom(named) => {
                    match picker::base_named(&named) {
                        Some(base) => open_picker(app, base),
                        None => app.push(Entry::Notice(format!(
                            "{named} is not a directory — /add-dir takes somewhere that exists, \
                             or nothing at all to pick from here"
                        ))),
                    }
                    None
                }
                command::RootCmd::Remove(path) => Some(Action::RemoveRoot(PathBuf::from(path))),
            }
        }
        Slash::Project(what) => {
            return match what {
                command::ProjectCmd::List => Some(Action::ListProjects),
                // Nothing after `add` is the directory the console was launched
                // in — the same default `jod project add` takes from the shell.
                command::ProjectCmd::Add(None) => Some(Action::AddProject(launch_dir())),
                // Resolved through the picker's own opener, which expands `~`,
                // makes a relative path absolute and canonicalises the result:
                // the catalog is matched against later, so two spellings of one
                // checkout would be two projects.
                //
                // Refused rather than stored when it is not a directory. The
                // CLI keeps an unresolvable path as given, which is right for a
                // script that is ahead of the filesystem; here it is a typo,
                // and a row nothing will ever match is worse than no row.
                command::ProjectCmd::Add(Some(named)) => match picker::base_named(&named) {
                    Some(path) => Some(Action::AddProject(path)),
                    None => {
                        app.push(Entry::Notice(format!(
                            "{named} is not a directory — /project add takes a checkout that \
                             exists, or nothing at all for the one Jod was launched in"
                        )));
                        None
                    }
                },
                // A name, and not run through the picker: this one names a row
                // that is already catalogued, so a path on disk is neither
                // needed nor a good check — the checkout of an untracked
                // project is often the one that has already been deleted.
                command::ProjectCmd::Untrack(name) => {
                    Some(Action::UntrackProject { id: None, name })
                }
            }
        }
        // The session list itself, rather than directions to it. This used to
        // send you to the fleet with an instruction to read an id off a row and
        // type it back — which is a way in only for someone who already knows
        // which of fifty threads they want.
        Slash::Sessions => app.overlay = Overlay::Sessions(sessions::Browser::default()),
        Slash::Resume(id) => match app.resolve_session(&id) {
            app::Resolved::Session(session) => {
                app.resume = Resume::Session(session.clone());
                app.session = Some(session.clone());
                app.push(Entry::Notice(format!("continuing {session}")));
                // The cursor moved to a thread Jod's binding does not follow.
                return Some(Action::NewThread);
            }
            app::Resolved::Verbatim(raw) => {
                app.resume = Resume::Session(raw.clone());
                app.session = Some(raw.clone());
                // Say, always, that this matched nothing on screen. A typo is
                // otherwise indistinguishable from a real resume until the
                // harness rejects it several seconds later — and an empty fleet
                // is the case where that is *most* likely, not least: there is
                // nothing it could have matched. Saying only "continuing
                // bogus-id-123" there read as success.
                app.push(Entry::Notice(if app.agents.is_empty() {
                    format!(
                        "continuing {raw} — nothing is running here to match it against, \
                         so it is passed on to the harness as typed"
                    )
                } else {
                    format!(
                        "continuing {raw} — not one of the agents listed, passing it on as typed"
                    )
                }));
                return Some(Action::NewThread);
            }
            app::Resolved::NoSession(agent) => {
                app.push(Entry::Notice(format!(
                    "{agent} has not reported a conversation yet — resuming it would start a fresh one"
                )));
            }
            app::Resolved::Ambiguous(n) => {
                app.push(Entry::Notice(format!(
                    "{id} matches {n} agents — type more of it"
                )));
            }
        },
        // Every workspace verb is a toggle, so typing the command you are
        // already looking at takes you home rather than doing nothing.
        Slash::Open(ws) => app.go(toggled(app.workspace, ws)),
        // Naming a row lands the cursor on it, which is what makes
        // `/schedule nightly-inbox` worth having beside `/schedules`.
        Slash::OpenNamed(ws, name) => {
            app.go(ws);
            match app.row_ids(ws).into_iter().find(|id| id.starts_with(&name)) {
                Some(id) => app.list_mut(ws).selected = Some(id),
                None => app.push(Entry::Notice(format!(
                    "no {} called {name}",
                    ws.menu_name()
                ))),
            }
        }
        Slash::Memory(query) => {
            app.go(Workspace::Memory);
            if let Some(q) = query {
                let list = app.list_mut(Workspace::Memory);
                list.filter = Some(q);
                list.editing_filter = false;
                app.reconcile();
            }
        }
        Slash::NewKind(ws) => {
            app.go(ws);
            return begin_new(app, ws);
        }
        // `/pause` and `/unpause` are the same verb typed two ways: both toggle,
        // because the alternative is a command that reports success having
        // changed nothing when the thing was already in the state asked for.
        Slash::Pause(name) | Slash::Unpause(name) => {
            return match named(app, &name) {
                Named::Schedule => Some(Action::ToggleSchedule(name)),
                Named::Goal => Some(Action::ToggleGoal(name)),
                Named::Neither | Named::Both => None,
            }
        }
        Slash::Run(name) => {
            return match named(app, &name) {
                Named::Schedule => Some(Action::RunSchedule(name)),
                Named::Goal => Some(Action::RunGoal(name)),
                Named::Neither | Named::Both => None,
            }
        }
        Slash::Remember {
            subject,
            predicate,
            object,
        } => {
            return Some(Action::Remember {
                subject,
                predicate,
                object,
            })
        }
        // Typed rather than pointed at, and still destructive, so it goes
        // through the same confirmation the `x` key does — landing on the row
        // first, so the thing about to be forgotten is on screen while the
        // question is asked.
        Slash::Forget(name) => {
            app.go(Workspace::Memory);
            if let Some(id) = app
                .row_ids(Workspace::Memory)
                .into_iter()
                .find(|id| *id == name)
            {
                app.list_mut(Workspace::Memory).selected = Some(id);
            }
            app.overlay = Overlay::Confirm {
                verb: "forget".to_string(),
                what: name,
            };
        }
        Slash::Delegate(prompt) => return Some(Action::Delegate(prompt)),
        Slash::Main(instruction) => return Some(Action::Orchestrate(instruction)),
        Slash::EnterMain => return Some(Action::EnterMain),
        Slash::Stop(which) => return resolve_agent(app, &which).map(Action::Stop),
        Slash::Watch(which) => return resolve_agent(app, &which).map(Action::Watch),
        Slash::Heartbeat { which, on } => {
            return resolve_agent(app, &which).map(|id| Action::Heartbeat { id, on })
        }
        Slash::Attach(which) => return resolve_agent(app, &which).map(Action::Attach),
        Slash::Todo(title) => return Some(Action::AddTask(title)),
        Slash::Done(id) => return Some(Action::FinishTask(id)),
        Slash::Clear => {
            // The same three fields `/new` resets, for the same reason: they
            // are what an ordinary turn resumes from, and a cleared screen in
            // front of a live session is the lie this command was reported for.
            // What `/clear` does *not* do is drop the binding — that is `/new`,
            // and it means "leave", not "start over here".
            app.resume = Resume::Fresh;
            app.session = None;
            app.cost_usd = 0.0;
            go_home(app);
            // A hint and not a notice, for the reason `/new`'s line gives: the
            // splash is drawn over hints, and this command's whole purpose is
            // to land you back on it.
            app.push(Entry::Hint(
                "cleared — the next message starts with no context behind it".into(),
            ));
            return Some(Action::Clear);
        }
        // The opposite trade to `/clear`: that one drops the context for free,
        // this one keeps it for the price of a model call.
        Slash::Compact => return Some(Action::Compact),
        Slash::Update { check } => return Some(Action::Update { check }),
        Slash::Upgrade { check } => return Some(Action::Upgrade { check }),
        Slash::Jobs => app.overlay = Overlay::Jobs,
        Slash::Reload => return Some(Action::Reload),
        Slash::Exit => app.should_quit = true,
        Slash::NeedsArgument(usage) => {
            app.push(Entry::Notice(format!("usage: {usage}")));
        }
        Slash::Unknown(what) => {
            app.push(Entry::Notice(format!(
                "{what} is not a command — /help lists them"
            )));
        }
        // The reason was written by whoever refused it, and it already names
        // what would have worked. Repeating "/help lists them" here would bury
        // that under advice the user does not need.
        Slash::Refused(said) => app.push(Entry::Notice(said)),
    }
    None
}

/// Empty the chat and put the console back on the splash.
///
/// Clearing the transcript is not enough on its own to get you home. The splash
/// is drawn for the chat workspace and only while nothing is being watched, so
/// `/clear` typed from the fleet — or from inside somebody else's run — used to
/// empty a screen you were not looking at and leave you standing where you
/// were. Both commands mean "start again from the top", and the top is the
/// wordmark with the mascot over it.
///
/// Nothing is stopped by coming home: a watched run is a detached process group
/// reporting through the database, and this window was only ever a viewer of
/// it. `/clear` stops looking; it does not stop the work.
fn go_home(app: &mut App) {
    app.watching = None;
    app.transcript.clear();
    app.scroll_to_bottom();
    // Takes the overlay and the back stack with it, which is the rest of home.
    app.go(Workspace::Chat);
}

/// Drop the harness session behind the conversation the chat box is bound to,
/// and answer with whatever the user should be told about it.
///
/// This is the half of `/clear` that has to reach the database. An ordinary
/// thread carries its resume cursor on the app, so emptying `App::resume` is
/// the whole job there; the main chat carries its cursor on the conversation
/// row, and `hand_to_orchestrator` reads it back through `Store::resume_for`
/// every turn. Clearing without this wrote nothing down, so the screen went
/// blank and the very next message resumed the history that had just left it.
///
/// The session id is the only thing holding a model's context window, so
/// dropping it is the entire reset. Jod's transcript is deliberately kept, for
/// the reason Telegram's `/new` gives: Jod owns the record, and a reset that
/// destroyed it would make the main chat unauditable from whichever surface
/// reset it last.
///
/// It reads `Thread::conversation` and never `current_conversation`. The
/// difference matters exactly once, and it is the case that would be a bug:
/// `current_conversation` falls back to the conversation of the run *being
/// watched*, so `/clear` typed while looking at somebody else's agent would
/// reach in and forget that agent's session. `/clear` stops looking. It does
/// not touch what it was looking at.
fn forget_bound_session(store: &Store, thread: &Thread) -> Option<Entry> {
    let id = thread.conversation.as_deref()?;
    match store.set_conversation_session(id, None) {
        // Said for the main chat and nowhere else, because only there is it
        // news. The desk, `jod main` and the phone are one conversation, so
        // clearing at any of them clears it at all of them — the consequence
        // Telegram's own reply spells out, and not something the user should
        // have to discover from the other end.
        Ok(_) if thread.in_main(Some(store)) => Some(Entry::Hint(
            "the main chat is one chat — every surface starts fresh with it".into(),
        )),
        Ok(_) => None,
        // Reported rather than undone: the screen is already empty and
        // `App::resume` is already `Fresh`, and putting the user back into a
        // conversation they asked to leave would be the worse of the two. What
        // must not happen is silence — a main chat that quietly resumes a
        // session the user was told had been dropped is the exact fault this
        // function exists to end.
        Err(e) => Some(Entry::Notice(format!(
            "the screen is clear, but the stored session could not be dropped: {e}"
        ))),
    }
}

/// Put the directory picker on screen, walking from `base`.
///
/// One function because there are now three ways in — `Ctrl-G d`, `/root add` and
/// `/add-dir` — and they must open the *same* picker. Three copies of "walk,
/// construct, assign" is three places for the bound, the noise list or the
/// starting row to drift apart, which is exactly the drift `picker.rs` was
/// written to prevent between its own two sizes.
///
/// The walk is the one piece of I/O the key path does, and it stays here for
/// the reason spelled out at `Ctrl-G d`: bounded, on an explicit keystroke, never
/// on the tick.
fn open_picker(app: &mut App, base: PathBuf) {
    let (entries, truncated) = picker::directories(&base);
    app.overlay = Overlay::Picker(picker::Picker::new(base, entries, truncated));
}

/// Where a picker opened with no argument starts: the directory `jod` was
/// launched in.
fn launch_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Record the model this conversation is to run on from now on.
///
/// Its own function, beside [`remember_flag`], for the same reason that one has:
/// `/model`, `/config` and any future picker must end at one place, because two
/// paths writing the same setting is how one of them ends up not writing it.
fn remember_model(model: Option<String>) -> Action {
    Action::Keep(Setting::Model(model))
}

/// Record how much this conversation's agent may do without asking.
///
/// **Tab does not reach this yet.** Cycling the mode with Tab is handled in
/// `on_key`, which changes `app.mode` and returns `None`, so the choice applies
/// to the next spawn and is never written down — the same gap `/mode` had. The
/// fix is one line in that branch, and it is deliberately not made here because
/// `on_key` belongs to another track: return `Some(remember_mode(app.mode))`
/// instead of `None`.
fn remember_mode(mode: PermissionPolicy) -> Action {
    Action::Keep(Setting::Mode(mode))
}

/// Write one setting onto one conversation.
///
/// The single writer. Returns what to say only when something went wrong: the
/// slash command has already said what it did, and a second notice confirming
/// that it was also *stored* describes plumbing rather than the choice.
///
/// A write that finds no row is a failure and not a silent nothing. It means the
/// conversation the chat box believes it is talking into is not there, which is
/// precisely the state where a setting quietly failing to stick would be
/// impossible to diagnose.
fn write_setting(store: &Store, conversation: &str, setting: &Setting) -> Option<String> {
    let wrote = match setting {
        Setting::Model(model) => store.set_conversation_model(conversation, model.as_deref()),
        Setting::Mode(mode) => store.set_conversation_permission(conversation, Some(*mode)),
    };
    match wrote {
        Ok(true) => None,
        Ok(false) => Some(format!(
            "there is no conversation {} to remember that on — it applies to this turn only",
            short(conversation)
        )),
        Err(e) => Some(format!(
            "could not remember that for next time, so it applies to this turn only: {e}"
        )),
    }
}

/// Write down whatever was chosen before there was a conversation to write it
/// on.
///
/// Called once the first turn has minted one. `conversation_for_run` is what
/// finds it: the run has already recorded its prompt by the time `spawn`
/// returns, so the conversation exists and the run is the handle on it.
fn flush_pending(jod: &Arc<Jod>, app: &mut App, thread: &mut Thread, run_id: &str) {
    if thread.pending.is_empty() {
        return;
    }
    let Some(store) = jod.store() else {
        return;
    };
    let Some(conversation) = thread
        .conversation
        .clone()
        .or_else(|| store.conversation_for_run(run_id).ok().flatten())
    else {
        return;
    };
    for setting in thread.pending.drain(..) {
        if let Some(said) = write_setting(store, &conversation, &setting) {
            app.push(Entry::Notice(said));
        }
    }
}

/// Record a flag preference the user just toggled.
///
/// Its own function so `/thinking`, `/details` and `/config thinking off` all
/// end at one place: two toggles that write the same setting through two paths
/// is how one of them ends up not persisting.
fn remember_flag(pref: config::Pref, on: bool) -> Action {
    Action::Config(config::Request::Set(pref, config::Value::Flag(on)))
}

/// Read the stored preferences onto a freshly built app.
///
/// Called once at startup, before the first draw, so the opening frame already
/// obeys what the user chose last time rather than flickering to it. An unset
/// preference leaves the built-in default in place — `App::new` has already put
/// it there — which is why this only writes what `Current::chosen` says
/// somebody actually decided.
///
/// The two `default.*` preferences are applied only when the launch flags say
/// nothing new, and clap cannot tell "not given" from "given the default": both
/// arrive as `HarnessArg::Claude` and `PermissionArg::Ask`. So a preference is
/// allowed to win only over exactly those two values. Making `--harness` and
/// `--permission` `Option` in `main.rs` would remove the guess; until then, a
/// person who types `-H claude` explicitly and has stored `opencode` gets
/// OpenCode, which is the one case this gets wrong.
fn load_preferences(app: &mut App, store: &Store, opts: &Options) {
    let all = match config::read_all(store) {
        Ok(all) => all,
        // A preference that cannot be read is not worth losing the session
        // over, but it must not pass silently either.
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not read your preferences: {e}"
            )));
            return;
        }
    };
    // Which harness `default.model` was chosen against — the stored one, or the
    // built-in when nobody stored one. `/config` drops the model whenever this
    // changes, so the two are in step in the database; what it cannot see is
    // `-H opencode` on this launch, which overrides the stored harness and would
    // otherwise be handed a model chosen for Claude Code.
    let stored_harness = all
        .iter()
        .find_map(|c| match (c.pref, &c.value) {
            (config::Pref::Harness, config::Value::Harness(kind)) => Some(*kind),
            _ => None,
        })
        .unwrap_or(HarnessKind::ClaudeCode);
    for current in all {
        if !current.chosen {
            if let Some(junk) = current.unreadable {
                app.push(Entry::Notice(format!(
                    "{} is set to “{junk}”, which I cannot read — using {}",
                    current.pref.name(),
                    current.value.label()
                )));
            }
            continue;
        }
        match (current.pref, &current.value) {
            (config::Pref::Thinking, config::Value::Flag(on)) => app.show_thinking = *on,
            (config::Pref::Details, config::Value::Flag(on)) => app.show_details = *on,
            // A stored preference wins only where the command line said
            // nothing at all. `is_none` rather than a comparison against the
            // default: comparing was the old guess, and it broke silently the
            // day the default moved.
            (config::Pref::Harness, config::Value::Harness(kind)) if opts.harness.is_none() => {
                app.harness = *kind;
            }
            // `--model` is already an `Option`, so the second half of this needs
            // none of the guesswork above: nothing given means nothing was asked
            // for. The first half is the harness check — a stored model is only
            // ever a name for the harness it was stored beside, and a launch
            // flag that picks a different one strands it exactly the way
            // `/config harness` would have.
            (config::Pref::Model, config::Value::Model(model))
                if opts.model.is_none()
                    && opts.harness.unwrap_or(stored_harness) == stored_harness =>
            {
                app.model = model.clone();
            }
            (config::Pref::Mode, config::Value::Mode(mode)) if opts.permission.is_none() => {
                app.mode = *mode;
            }
            _ => {}
        }
    }
}

/// Write one fact, and say what was written.
///
/// The trust decisions are Jod's, not the typist's: a fact typed at this
/// terminal is [`Origin::Owner`] in the default scope, because the person at
/// the keyboard is Reljod. `/remember` deliberately offers no way to assert
/// something *as* an agent or as untrusted material — that is what
/// `jod consolidate` is for, and letting the chat box choose its own origin
/// would put the graph's trust boundary in the hands of whatever pasted a line
/// into it.
fn remember_fact(store: &Store, subject: &str, predicate: &str, object: &str) -> String {
    let fact = jod_core::store::NewFact {
        scope: jod_core::store::DEFAULT_SCOPE.to_string(),
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        origin: jod_core::store::Origin::Owner,
        source: Some("jod tui".to_string()),
        valid_from: None,
    };
    match store.remember(fact) {
        Ok(id) => format!("remembered #{id}: {subject} {predicate} {object}"),
        Err(e) => format!("could not remember that: {e}"),
    }
}

/// What kind of thing a name typed at `/pause`, `/unpause` or `/run` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    Schedule,
    Goal,
    /// A name that is both. Refused rather than guessed at.
    Both,
    Neither,
}

/// Decide which screen a typed name belongs to, and say so when it cannot.
///
/// Schedules and goals share a namespace on screen but not in the store, so one
/// name can legitimately be both — and pausing the wrong one is invisible until
/// the thing that should have happened does not. Exact matches only: a prefix
/// that pauses the wrong nightly job is the same mistake with a shorter cause.
fn named(app: &mut App, name: &str) -> Named {
    let schedule = app.schedules.iter().any(|s| s.name == name);
    let goal = app.goals.iter().any(|g| g.name == name);
    match (schedule, goal) {
        (true, false) => Named::Schedule,
        (false, true) => Named::Goal,
        (true, true) => {
            app.push(Entry::Notice(format!(
                "{name} is both a schedule and a goal — open the screen and press p there"
            )));
            Named::Both
        }
        (false, false) => {
            app.push(Entry::Notice(format!(
                "no schedule or goal called {name} — /schedules and /goals list them"
            )));
            Named::Neither
        }
    }
}

/// Where a workspace command lands: the workspace, unless you are already
/// there, in which case home.
fn toggled(here: Workspace, asked: Workspace) -> Workspace {
    if here == asked {
        Workspace::Chat
    } else {
        asked
    }
}

/// Turn what was typed at `/stop`, `/watch` or `/attach` into one agent id.
///
/// A prefix is enough, because the panel shows eight characters and retyping a
/// UUID is not a user interface. An ambiguous or unknown one is refused rather
/// than guessed: stopping the wrong agent is not an undoable mistake.
fn resolve_agent(app: &mut App, typed: &str) -> Option<String> {
    let matches: Vec<&AgentLine> = app
        .agents
        .iter()
        .filter(|a| a.id.starts_with(typed) || a.name == typed)
        .collect();
    match matches.as_slice() {
        [only] => Some(only.id.clone()),
        [] => {
            app.push(Entry::Notice(format!(
                "no agent starts with {typed} — Ctrl-F lists them"
            )));
            None
        }
        many => {
            app.push(Entry::Notice(format!(
                "{typed} matches {} agents — type more of it",
                many.len()
            )));
            None
        }
    }
}

/// What a turn you are sitting in front of may do to Jod itself.
///
/// The full set, because the condition this codebase attaches to it — "you,
/// present, watching" — is met by definition here: this is the run whose output
/// is filling your screen.
const WATCHED: Option<ToolAccess> = Some(ToolAccess::Orchestrate);

/// What a run that goes off on its own may do to Jod itself.
///
/// Named as a pair with [`WATCHED`] so the difference between them is one line
/// to read rather than two call sites to find. The whole distinction is
/// whether anybody is looking.
const DELEGATED: Option<ToolAccess> = Some(ToolAccess::ReadOnly);

/// The stricter of the launch ceiling and what is asked for.
///
/// `jod_core::mcp::permits` already knows the ordering, so this asks it rather
/// than restating it — a second copy of "how much is more" is exactly the kind
/// of duplicate that drifts and turns a ceiling into a suggestion.
fn bounded(ceiling: PermissionPolicy, wanted: PermissionPolicy) -> PermissionPolicy {
    if jod_core::mcp::permits(ceiling, wanted) {
        wanted
    } else {
        ceiling
    }
}

/// Start a run for this conversation.
///
/// `tools` is a parameter rather than a constant because this one function
/// serves two situations with different amounts of trust in them: a turn you
/// are watching, and a delegation that goes off on its own. It used to hard-code
/// `None` for both, which is how "tell Jod to schedule this" became a request it
/// could answer but not carry out.
///
/// `conversation` and `system` are parameters for the same reason. Only the
/// caller knows whether a run continues something — Jod cannot infer it, and
/// guessing welds unrelated work into one transcript — and only the caller knows
/// whether this is the first turn after a handoff, which is the one turn that
/// has to bring its context with it.
#[allow(clippy::too_many_arguments)]
async fn spawn(
    jod: &Arc<Jod>,
    app: &App,
    opts: &Options,
    prompt: String,
    resume: Resume,
    tools: Option<ToolAccess>,
    conversation: RunConversation,
    system: Option<String>,
    // The harness's own command name, for the one harness that takes it in a
    // flag rather than in the prompt. `None` for every ordinary turn.
    command: Option<String>,
) -> Result<String> {
    let agent = jod
        .spawn_agent_in(
            SpawnRequest {
                name: crate::default_name(&prompt),
                // From the app, not the options: `/harness` and `/model` change
                // these mid-session, and a spawn must use what is current.
                harness: app.harness,
                prompt,
                // A handed-over transcript is framing, not something anybody said.
                // Folded into the prompt it would become the opening *user* turn of
                // the new conversation — the exact bug `SpawnRequest::system` was
                // added to fix.
                system,
                cwd: opts.cwd.clone(),
                model: app.model.clone(),
                // The live mode, clamped by the one the process was launched with.
                //
                // A ceiling rather than a default, and the asymmetry is the point:
                // `jod tui --permission plan` is somebody saying "not on this
                // machine, not today", and a Tab press inside the program must not
                // be able to talk them out of it. Downwards is always allowed —
                // asking for *less* needs no permission.
                permission: bounded(opts.ceiling(), app.mode),
                // App owns the conversation cursor: it advances to the exact
                // session the harness reported on the previous turn. A background
                // delegation passes its own, because it is not part of this
                // conversation at all.
                resume,
                // Decided by the caller — see the parameter's own doc. Each of the
                // two call sites says which situation it is in and why.
                tools,
                // Set only for a repository command on a harness that takes the
                // name in a flag. `commands::Discovered::invoke` decides which
                // spelling this is; nothing here branches on the harness.
                command,
                ..SpawnRequest::default()
            },
            conversation,
        )
        .await?;
    Ok(agent.id)
}


/// One turn from the chat box, with or without a repository command attached.
///
/// Extracted so `Action::Send` and `Action::RunCommand` cannot drift: they are
/// the same turn, and the only difference is whether a command name rides along
/// in the spawn request. A second copy of this would be a second place for the
/// tool grant, the carried summary and the conversation binding to get out of
/// step.
async fn send_turn(
    jod: &Arc<Jod>,
    app: &mut App,
    opts: &Options,
    thread: &mut Thread,
    prompt: String,
    command: Option<String>,
) {
    if thread.in_main(jod.store().map(Arc::as_ref)) {
        orchestrate(jod, app, opts, thread, prompt).await;
        return;
    }

            // The one place Jod's own verbs are handed over from the chat box.
            //
            // The rule this codebase already states is "the main chat is you,
            // present, watching" — and a turn you just typed into the TUI is
            // exactly that. The grant used to be withheld here on the grounds
            // that "an agent started from the chat box is doing a task, not
            // orchestrating", which conflated two different things reached
            // through one function: a turn you are watching, and a delegation
            // that goes off on its own. Only the second is unattended, and only
            // the second still gets nothing.
            //
            // Without this you could ask Jod to schedule something and it would
            // answer as if it had, having no verb to do it with.
            match spawn(
                jod,
                app,
                opts,
                prompt.clone(),
                app.resume.clone(),
                WATCHED,
                // Into the conversation the chat box is bound to, which after a
                // handoff is the one carrying the summary. Everything else is
                // still `New`, as it was.
                thread.binding(),
                thread.carried.clone(),
                command,
            )
            .await
            {
                Ok(id) => {
                    // Only once. From here the harness has a session of its own
                    // and is holding the context itself; re-sending it every
                    // turn would hand the model a summary of a conversation it
                    // is already in.
                    thread.carried = None;
                    // The first turn is what mints a conversation, so it is the
                    // first moment anything chosen before it can be written
                    // down.
                    flush_pending(jod, app, thread, &id);
                    // Ours, so the context bar is reading this chat's window
                    // again and the automatic compaction may act on it.
                    thread.watching_own_turn = true;
                    app.begin_turn(id, app.now_ms);
                    app.push(Entry::You(prompt));
                    app.scroll_to_bottom();
                }
                Err(e) => app.push(Entry::Notice(format!("could not start: {e}"))),
            }
}

/// Re-read the team from the store.
///
/// Members and tasks are written by the teammates themselves, in their own
/// processes, so the only way to know the current state is to ask the store —
/// there is no in-memory copy that could be authoritative.
fn refresh_team(jod: &Arc<Jod>, app: &mut App) {
    let (Some(team), Some(store)) = (app.team.clone(), jod.store()) else {
        return;
    };
    // A store error here must not take the UI down: the panel showing what it
    // last knew beats the whole session ending over a locked database.
    if let Ok(members) = store.team_members(&team) {
        app.members = members;
    }
    if let Ok(tasks) = store.team_tasks(&team) {
        app.tasks = tasks;
    }
}

/// Re-read what the workspaces show.
///
/// Off the render path, on the tick, exactly as the team board already is — and
/// for the same reason: cron, webhooks and goals write these from *other
/// processes*, so an in-memory copy could never be authoritative. Each loader
/// swallows its own errors rather than taking the UI down over a locked
/// database.
///
/// The board is one team's when a team is joined, and every team's otherwise —
/// the tasks screen is *the* board, where the team panel is scoped to one team.
///
/// `graph_size` is read beside the memory list rather than derived from it: the
/// list is capped at the most-connected few hundred, and a status bar that
/// counted what it happened to load could never say so.
fn refresh_workspaces(jod: &Arc<Jod>, app: &mut App) {
    app.memory = data::memory(jod);
    app.graph_size = data::graph_size(jod);
    app.schedules = data::schedules(jod);
    app.goals = data::goals(jod);
    app.hooks = data::hooks(jod);
    app.activity = data::activity(jod);
    app.board = data::tasks(jod, app.team.as_deref());
    // The forest, and then the cursor onto a row that still exists — the tree
    // reshapes on every tick as runs finish, which is the whole reason the
    // selection is an id.
    // What this repository offers, for the harness on screen. On the tick
    // because `/harness` changes the answer and the palette must not go stale
    // mid-session.
    app.discovered = data::discovered(jod, app.harness);
    // On the tick like every other list: another session touching a project
    // reorders the catalog, and the current project changes underneath this
    // console whenever the orchestrator resolves an instruction.
    app.projects = data::projects(jod);
    app.current_project = data::current_project(jod, app.conversation.as_deref());
    // ...and that reordering is what the catalog's cursor has to survive. A row
    // untracked from the fleet, or archived by another session, leaves the
    // cursor naming a project nothing draws.
    app.reconcile_catalog();
    let tree = data::forest(jod, app.tree.show_closed);
    app.forest = tree.nodes;
    app.closed_works = tree.closed;
    app.work_of = tree.works;
    app.tree_runs = tree.runs;
    app.run_of = tree.run_of;
    let rows = app.tree_rows();
    app.tree.reconcile(&rows);
    // The bus a work's agents are talking on. Loaded here rather than when `T`
    // is pressed, for the reason every list on this tick is: agents write to it
    // from other processes, so a copy read once at open would be stale by the
    // second message. Cheap when nothing has been opened — `data::traffic`
    // returns an empty log without touching the store.
    app.traffic = data::traffic(jod, app.traffic_of.as_ref());
    // Said once, and only while there is something being watched. Every session
    // now arms a heartbeat, so without a daemon the fleet would draw every
    // wedged agent as healthy — which is precisely the state the mark was added
    // to end, quietly restored by a daemon nobody started.
    // Read every tick, not only until it has been said once: the fleet keeps
    // showing it for as long as it is true, and a one-shot flag cannot answer
    // "is it still true" for a screen opened later.
    app.nothing_is_sweeping = data::watched_but_unswept(jod, app.now_ms);
    if !app.said_nothing_is_sweeping && app.nothing_is_sweeping {
        app.said_nothing_is_sweeping = true;
        app.push(Entry::Notice(
            "nothing is watching these sessions for stalls — start `jod daemon`, \
             or a wedged agent will keep reading as running"
                .into(),
        ));
    }
    app.reconcile();
    refresh_rail(jod, app);
}

/// Re-read the rail, and open it the first time something is blocked.
///
/// The whole rail travels as one query — filter, sort, kind and stack are all
/// in it — so this is one indexed read however the rail is set up, which is why
/// it can run on every keystroke of the filter box as well as on the tick.
fn refresh_rail(jod: &Arc<Jod>, app: &mut App) {
    app.cards = data::cards(jod, &app.rail.query(app.conversation.clone()));
    app.reconcile_rail();
    // Once per session, and said out loud when it happens: a column that
    // appears on its own without explanation reads as a rendering fault.
    if app.rail.auto_open(&app.cards) {
        app.push(Entry::Notice(
            "a run is blocked — the rail is open; Ctrl-N answers, and closes it again".into(),
        ));
    }
}

/// Which conversation the rail and the `@` picker belong to.
///
/// The conversation the chat box is bound to, falling back to the pinned main
/// chat. The fallback is what makes the rail useful before the first turn: an
/// unbound chat box has no conversation of its own yet, and the cards worth
/// looking at in that state are the orchestrator's — which is where anything
/// delegated from here reports back to.
fn bind_rail(jod: &Arc<Jod>, app: &mut App, thread: &Thread) {
    app.conversation = jod.store().and_then(|store| {
        current_conversation(store, app, thread)
            .or_else(|| store.pinned_conversation().ok().flatten())
    });
}

/// Hand the directory `jod tui` was launched in to the conversation on screen,
/// exactly as `/add-dir` would.
///
/// The gap this closes: a console opened inside a repository knew where it was
/// — every turn's harness process starts there — and the one part of the
/// program that asks "which directories may I search" did not. `@` in a fresh
/// session said *no folder to search* about the repository you were standing
/// in, and the fix was to type the path you had just `cd`-ed to.
///
/// Read-only, like every root Jod adds itself; a worktree is what makes one
/// writable. It goes to the conversation [`bind_rail`] just resolved — the one
/// the rail and the `@` popup are already reading — which before the first turn
/// is the pinned main chat, because that is the conversation this console is
/// looking at.
///
/// **Once per conversation, per process,** which is what the set is for.
/// `Store::add_root` is idempotent, so re-adding would cost nothing and mean
/// something: it would put the directory back every quarter-second after
/// `/root remove` took it away, and a console that undoes your removals is
/// worse than one that never offered the root. Removal therefore holds for the
/// rest of the session, and the next launch grants it again — which is the same
/// bargain `/add-dir` itself makes.
///
/// The grant itself is [`jod_core::store::Store::grant_launch_root`], shared
/// with `jod main`, `jod chat` and `jod run` so that the console and the
/// commands cannot answer this question differently again. What this function
/// keeps is the part that is genuinely the console's: the set, which is a
/// record of what *this process* has done, and a notice in the transcript,
/// which is how a console reports a failure.
///
/// What the shared helper adds is a check the console never had. A directory
/// the conversation already holds is left alone, so opening a second console
/// inside a worktree this conversation had claimed no longer takes the write
/// back — `add_root` upserts, and its update clause writes `writable`.
fn ensure_launch_root(jod: &Arc<Jod>, app: &mut App, granted: &mut HashSet<String>) {
    // Nowhere to put it, or nowhere to put it *in*. A fixture with no launch
    // directory is not a session standing anywhere.
    if app.cwd.as_os_str().is_empty() {
        return;
    }
    let Some(store) = jod.store() else {
        return;
    };
    // A console on a machine where nothing has run yet has no conversation at
    // all: [`bind_rail`] falls back to the pinned main chat, and on a fresh
    // install there is not one to fall back to. Opening it is what `enter_main`
    // does and `main_conversation` is a singleton, so this mints one exactly
    // once in the life of the machine and finds it every time after.
    //
    // Measured, not assumed: with this missing, a `jod tui` opened in a
    // repository on a fresh `JOD_HOME` ran for minutes and left `conversations`
    // and `conversation_roots` both empty — the whole feature waiting on a turn
    // being typed before it could do anything.
    let conversation = match app.conversation.clone() {
        Some(conversation) => conversation,
        None => match store.main_conversation(app.harness, &app.cwd.display().to_string()) {
            Ok(id) => {
                app.conversation = Some(id.clone());
                id
            }
            // No conversation and none to be had. The `@` popup's own empty
            // state covers this — it is the state it was written for.
            Err(_) => return,
        },
    };
    if !granted.insert(conversation.clone()) {
        return;
    }
    let cwd = app.cwd.clone();
    // Silent when it works: the header band names the directory, `/root` lists
    // it, and a notice on every launch would be a line of chrome saying what
    // the screen already says. A failure is worth one line — it is the
    // difference between "`@` searches here" and "`@` says there is nothing to
    // search", and the popup's own empty state cannot explain why.
    if let Err(e) = store.grant_launch_root(&conversation, &cwd) {
        app.push(Entry::Notice(format!(
            "{} is where this console is, but it could not be added as a root: {e}",
            cwd.display()
        )));
    }
}

/// Give the `@` popup something to search, and re-rank it against it.
///
/// Loaded here rather than in `on_key`, which does no I/O by design:
/// enumerating a hundred thousand paths is not something a keystroke may block
/// on. [`jod_core::rank::candidates_shared`] caches for a few seconds, so a
/// burst of typing costs one walk and the rest are pointer copies — which is
/// what makes "live on every keystroke" true rather than aspirational.
/// Re-run the transcript search against what has been typed.
///
/// In the loop for the same reason `refresh_mention` is: `on_key` does no I/O,
/// and this is a full-text query. Cheap when no search is open, which is almost
/// always.
fn refresh_search(jod: &Arc<Jod>, app: &mut App) {
    let Overlay::Search { query, .. } = &app.overlay else {
        return;
    };
    // An empty box searches for nothing rather than for everything: `fts_query`
    // would return no expression anyway, and a list of every message ever is
    // not a starting point anyone wants.
    let found = if query.trim().is_empty() {
        Vec::new()
    } else {
        data::search(jod, &query.clone(), SEARCH_HITS)
    };
    if let Overlay::Search { hits, selected, .. } = &mut app.overlay {
        *hits = found;
        if *selected >= hits.len() {
            *selected = 0;
        }
    }
}

/// How many hits the search screen asks for. The store caps lower than this in
/// its own right; the number here is what fits on a screen worth reading.
const SEARCH_HITS: usize = 40;

/// Fill the session list, once, the first time it is opened.
///
/// Once rather than on every keystroke, which is the difference between this
/// and [`refresh_search`]: the search box asks the store a new question with
/// each letter, while the session list already holds every row and the typing
/// only narrows what is drawn. Loading again per keystroke would re-run a tip
/// query per conversation for a filter that needs no database at all.
///
/// Left alone entirely while the overlay is shut, which is almost always.
fn refresh_sessions(jod: &Arc<Jod>, app: &mut App) {
    let Overlay::Sessions(browser) = &app.overlay else {
        return;
    };
    if browser.loaded {
        return;
    }
    // `LIST_LIMIT` rather than a number of this screen's own: it is also what
    // `sessions::resolve` matches an id against, and a list that showed a
    // fifty-first thread would offer a row nothing else in the module can find.
    let rows = match jod.store() {
        Some(store) => sessions::session_rows(store.as_ref(), sessions::LIST_LIMIT),
        None => Vec::new(),
    };
    if let Overlay::Sessions(browser) = &mut app.overlay {
        browser.rows = rows;
        browser.loaded = true;
    }
}

fn refresh_mention(jod: &Arc<Jod>, app: &mut App) {
    if app.mention.is_none() {
        return;
    }
    app.roots = data::roots(jod, app.conversation.as_deref());
    app.candidates = data::candidates(&app.roots);
    let (cwd, roots, candidates) = (
        app.cwd.clone(),
        app.roots.clone(),
        app.candidates.clone(),
    );
    if let Some(popup) = &mut app.mention {
        popup.refresh(&cwd, &roots, &candidates);
    }
}

// ---- dictation -----------------------------------------------------------
//
// Hands-free, which changes the shape of everything here. The microphone is a
// switch: turned on once, it stays on, and sentences arrive on their own as
// they are finished. Nothing below waits for a key.
//
// The session owns a child process and a read position in the file that
// process is writing, so it lives in the event loop and is threaded through
// these functions rather than sitting on `App`.

/// Type alias for the listening session and the engine transcribing it.
type Listening = Option<(jod_voice_core::Session, crate::voice::Engine)>;

/// The channel a finished transcript comes back on.
type VoiceTx = tokio::sync::mpsc::UnboundedSender<Result<String, String>>;

/// Switch the microphone on, or off.
fn toggle_listening(jod: &Arc<Jod>, app: &mut App, session: &mut Listening, tx: &VoiceTx) {
    if session.is_some() {
        stop_listening(app, session, tx, false);
        return;
    }

    // Resolved before he speaks rather than after. A sentence dictated into a
    // console that was never going to transcribe it is a sentence said twice,
    // and the message says which of the three parts is missing.
    let Some(store) = jod.store() else {
        app.push(Entry::Notice(
            "dictation needs Jod's database, which this console does not have open".into(),
        ));
        return;
    };
    let engine = match crate::voice::resolve(store, &jod_core::paths::jod_home()) {
        Ok(engine) => engine,
        Err(why) => {
            app.push(Entry::Notice(why));
            return;
        }
    };

    match jod_voice_core::Session::start() {
        Ok(live) => {
            app.dictation = app::Dictation::Listening {
                since_ms: app.now_ms,
                backend: live.backend().to_string(),
                pending: 0,
                speaking: false,
                level: 0.0,
                heard: 0,
            };
            app.push(Entry::Notice(format!(
                "listening · {} · say \"go ahead\" to send, \"stop listening\" to switch off",
                engine.label()
            )));
            *session = Some((live, engine));
        }
        Err(why) => app.push(Entry::Notice(why)),
    }
}

/// Switch the microphone off.
///
/// `discard` throws away the part-spoken sentence; without it, whatever was
/// being said when the switch was flipped is still transcribed. Switching off
/// mid-sentence should not silently lose the sentence — that is the difference
/// between a toggle you can trust and one you have to time.
fn stop_listening(app: &mut App, session: &mut Listening, tx: &VoiceTx, discard: bool) {
    let Some((live, engine)) = session.take() else {
        return;
    };
    // Read before the state is cleared: sentences already being transcribed
    // are still on their way, and saying how many is what stops the pause that
    // follows from looking like something went wrong.
    let mut in_flight = app.dictation.pending();
    let tail = live.finish();
    app.dictation = app::Dictation::Off;

    if let (Some(samples), false) = (tail, discard) {
        transcribe_in_background(samples, engine, tx);
        in_flight += 1;
    }

    app.push(Entry::Notice(match (discard, in_flight) {
        (true, _) => "stopped listening — dropped what was being said".to_string(),
        (false, 0) => "stopped listening".to_string(),
        (false, 1) => "stopped listening — one sentence still coming".to_string(),
        (false, n) => format!("stopped listening — {n} sentences still coming"),
    }));
}

/// Read the microphone, and start transcribing anything that finished.
///
/// Called on the tick. Deliberately does no model work itself: it hands
/// finished audio to a background task and returns, so the console stays
/// responsive while a sentence is being transcribed and the next one spoken.
fn poll_listening(app: &mut App, session: &mut Listening, tx: &VoiceTx) {
    let Some((live, engine)) = session.as_mut() else {
        return;
    };

    // A recorder that died leaves a console that looks like it is listening
    // and is deaf — the worst state for something being talked to by somebody
    // whose hands are full, so it is said out loud rather than left to be
    // discovered.
    if !live.is_running() {
        app.push(Entry::Notice(
            "the recorder stopped — listening is off. `jod voice check` says what is wrong."
                .into(),
        ));
        *session = None;
        app.dictation = app::Dictation::Off;
        return;
    }

    match live.poll() {
        Ok(jod_voice_core::Heard::Nothing { level, speaking }) => {
            app.dictation.note_level(level, speaking);
        }
        Ok(jod_voice_core::Heard::Utterance { samples }) => {
            app.dictation.note_level(0.0, false);
            app.dictation.note_pending(1);
            transcribe_in_background(samples, engine.clone(), tx);
        }
        Err(why) => {
            app.push(Entry::Notice(format!("listening stopped: {why}")));
            *session = None;
            app.dictation = app::Dictation::Off;
        }
    }
}

/// Transcribe one utterance off the event loop.
fn transcribe_in_background(samples: Vec<f32>, engine: crate::voice::Engine, tx: &VoiceTx) {
    let wav = jod_voice_core::stream::to_wav(&samples);
    let tx = tx.clone();
    // `spawn_blocking`, because the local engine is whisper.cpp occupying a
    // core for a second or so. On `spawn` that would sit on a runtime worker
    // and stall the event loop this channel exists to keep free.
    tokio::task::spawn_blocking(move || {
        let said = tokio::runtime::Handle::current().block_on(async { engine.transcribe(&wav).await });
        // The receiver is gone only when the console is exiting, and a
        // transcript nobody can receive is not an error worth reporting to a
        // screen that is being torn down.
        let _ = tx.send(said);
    });
}

/// Act on one transcribed sentence.
///
/// Returns the instruction to send when he asked for it out loud, and `None`
/// when the sentence was dictation, a correction, or nothing usable.
///
/// **Sending is the only thing here that leaves the console**, and it happens
/// only on an explicit spoken command — never because a sentence sounded
/// finished. See [`jod_voice_core::spoken`] for why that rule is a narrow
/// phrase match rather than a model's judgement.
fn heard_utterance(app: &mut App, transcript: &str) -> Option<String> {
    use jod_voice_core::Spoken;

    match jod_voice_core::spoken::interpret(transcript) {
        Spoken::Nothing => None,
        Spoken::Text(said) => {
            append_dictation(app, &said);
            app.dictation.note_heard();
            None
        }
        Spoken::Clear => {
            app.input.clear();
            app.cursor = 0;
            app.dictated.clear();
            app.push(Entry::Notice("cleared".into()));
            None
        }
        Spoken::Undo => {
            match app.dictated.pop() {
                Some(last) => {
                    // Only if it is still the tail. Anything typed since means
                    // the words this would remove are no longer the ones he
                    // means, and guessing is worse than saying so.
                    if app.input.trim_end().ends_with(&last) {
                        let cut = app.input.trim_end().len() - last.len();
                        app.input.truncate(cut);
                        let trimmed = app.input.trim_end().to_string();
                        app.input = trimmed;
                        app.cursor = app.input.len();
                        app.push(Entry::Notice(format!("took back: {last}")));
                    } else {
                        app.push(Entry::Notice(
                            "that is no longer the end of the line — nothing taken back".into(),
                        ));
                    }
                }
                None => app.push(Entry::Notice("nothing to take back".into())),
            }
            None
        }
        Spoken::Stop => {
            // The recorder lives in the event loop, so this is a request the
            // loop picks up rather than something done here.
            app.stop_listening_requested = true;
            None
        }
        Spoken::Send(before) => {
            if !before.is_empty() {
                append_dictation(app, &before);
                app.dictation.note_heard();
            }
            let instruction = app.input.trim().to_string();
            if instruction.is_empty() {
                app.push(Entry::Notice("nothing to send yet".into()));
                return None;
            }
            // Cleared here rather than by the send path, so a spoken send
            // leaves the composer exactly as `⏎` would.
            app.input.clear();
            app.cursor = 0;
            app.dictated.clear();
            Some(instruction)
        }
    }
}

/// Put a transcribed sentence into the composer.
///
/// Appended at the end rather than at the cursor: while listening, the cursor
/// is wherever it was last left, and sentences arriving in the middle of
/// earlier ones would scramble a paragraph nobody is watching closely.
fn append_dictation(app: &mut App, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !app.input.is_empty() && !app.input.ends_with(char::is_whitespace) {
        app.input.push(' ');
    }
    app.input.push_str(text);
    app.cursor = app.input.len();
    // Remembered so "undo that" can take back exactly this sentence rather
    // than a guessed number of words.
    app.dictated.push(text.to_string());
}
/// Hand the typed line to `$EDITOR`, and take back whatever comes out.
///
/// The user already has a configured editor; a one-line TUI field will never
/// beat it for a forty-line prompt. The terminal has to be given back and
/// retaken around the child, with the same discipline as `enter`/`restore` —
/// including the panic hook, which `enter` reinstalls.
fn edit_in_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) {
    let Some(editor) = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
    else {
        app.push(Entry::Notice(
            "no $EDITOR set — export one and press Ctrl-G e again".into(),
        ));
        return;
    };

    let path = std::env::temp_dir().join(format!("jod-prompt-{}.md", std::process::id()));
    if let Err(e) = std::fs::write(&path, &app.input) {
        app.push(Entry::Notice(format!("could not open an editor: {e}")));
        return;
    }

    restore();
    let status = std::process::Command::new(&editor).arg(&path).status();
    let re_entered = enter();
    match re_entered {
        Ok(fresh) => *terminal = fresh,
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not take the terminal back: {e}"
            )));
            return;
        }
    }
    let _ = terminal.clear();

    match status {
        // A failed edit must not throw the work away — that is the one thing a
        // form must never do.
        Ok(code) if !code.success() => {
            app.push(Entry::Notice(format!(
                "{editor} exited {code} — nothing changed"
            )));
        }
        Err(e) => app.push(Entry::Notice(format!("could not run {editor}: {e}"))),
        Ok(_) => match std::fs::read_to_string(&path) {
            Ok(text) => {
                app.input = text.trim_end().to_string();
                app.cursor = app.input.len();
            }
            Err(e) => app.push(Entry::Notice(format!("could not read it back: {e}"))),
        },
    }
    let _ = std::fs::remove_file(&path);
}

/// Hand the terminal to a harness's own sign-in flow, and take it back.
///
/// `/login` exists in here rather than only on the command line because of
/// where the failure is met. A run dies unauthenticated in the console, the
/// advice it prints names a command, and quitting a full-screen interface to
/// run that command loses the conversation you were in the middle of. This is
/// the same handover `$EDITOR` gets, with the same discipline — `restore`,
/// child, `enter`, and the panic hook that `enter` reinstalls.
///
/// Jod reads nothing the flow produces. It runs the harness's command, in this
/// process's environment, and then asks the harness what happened — which is
/// the same question a run will ask, from the same place.
fn sign_in(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    kind: HarnessKind,
) {
    let Some(bin) = kind.locate() else {
        app.push(Entry::Notice(format!(
            "{} is not installed, so there is nothing to sign in to",
            kind.label()
        )));
        return;
    };
    let Some(args) = kind.login_args() else {
        app.push(Entry::Notice(format!(
            "{} has no sign-in command Jod can run — start it once by hand and sign in there",
            kind.label()
        )));
        return;
    };

    restore();
    println!("{} — signing in", kind.label());
    if let Some(hint) = kind.profile_hint() {
        // The line that explains the whole failure when somebody has signed in
        // to a different profile: this is the directory the account will land
        // in, and the one every run Jod starts will read.
        println!("reading {hint}");
    }
    let status = std::process::Command::new(&bin).args(args).status();
    match enter() {
        Ok(fresh) => *terminal = fresh,
        Err(e) => {
            app.push(Entry::Notice(format!(
                "could not take the terminal back: {e}"
            )));
            return;
        }
    }
    let _ = terminal.clear();

    match status {
        Err(e) => app.push(Entry::Notice(format!(
            "could not run {}: {e}",
            bin.display()
        ))),
        // Asked rather than believed, and asked even when the flow exited
        // badly — somebody who backed out of it is owed the state they are
        // actually in, not a report of which key they pressed.
        Ok(_) => {
            let state = kind.auth();
            app.push(Entry::Notice(format!(
                "{} — {}",
                kind.label(),
                state.describe()
            )));
        }
    }
}

/// What a running `/update` sends back to the loop.
enum UpdateMsg {
    /// One line the installer wrote, as it wrote it, and the job it belongs to.
    Line { job: usize, line: String },
    /// It finished. `replaced` is what decides whether a reload is worth
    /// offering — an update that found itself already current changed nothing
    /// to reload into.
    Done {
        job: usize,
        said: String,
        ok: bool,
        replaced: bool,
    },
}

/// Which of the two ways to take a newer Jod a background job is running.
///
/// They share every part of this machinery — the job slot, the streamed
/// output, the "still running the old build" summary — and differ only in
/// which script does the work and what it is called on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Take {
    /// Rebuild from the checkout: patch-only, needs git and cargo, minutes.
    Update,
    /// Download the newest release: any major/minor, needs neither, seconds.
    Upgrade,
}

impl Take {
    /// The word this is called by, at a shell and on screen alike.
    fn verb(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Upgrade => "upgrade",
        }
    }
}

/// `/update` and `/upgrade` — install a newer Jod, from inside the Jod you are
/// running.
///
/// The console on the VPS is where Jod is used, so it is where noticing that
/// Jod is out of date happens; a command that could only be typed at a shell
/// would mean quitting the thing you wanted to keep. It runs as a background
/// job: the script's output streams into the transcript, and the console stays
/// usable — agents keep streaming, screens keep refreshing — for the several
/// minutes a cold `cargo build` takes, or the seconds a download does.
///
/// Safe to run against yourself. Both scripts rename each new binary over the
/// old one, so nothing here is writing the file this process is executing.
fn start_take(
    app: &mut App,
    tx: &tokio::sync::mpsc::UnboundedSender<UpdateMsg>,
    check: bool,
    how: Take,
) {
    let verb = how.verb();
    // One at a time, derived from the job table rather than tracked beside it:
    // two installers writing the same binaries is a race with a corrupt
    // install at the end of it. Both verbs are checked, not just this one —
    // they write the same files, so an update racing an upgrade is the same
    // race as two updates.
    if app.jobs.iter().any(|j| {
        j.is_running() && (j.label.starts_with("update") || j.label.starts_with("upgrade"))
    }) {
        app.push(Entry::Notice(format!(
            "an {verb} is already running — Ctrl-G j shows it"
        )));
        return;
    }
    let label = if check {
        format!("{verb} --check")
    } else {
        verb.to_string()
    };
    let label = label.as_str();
    let job = app.job_start(label, app.now_ms);
    app.push(Entry::Notice(format!(
        "{label} running in the background · Ctrl-G j lists background shells"
    )));

    let tx = tx.clone();
    let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    // Forwarded through this task rather than sent to the loop directly, so
    // the loop sees one channel and every line still arrives before the
    // `Done` that summarises it.
    let forward = tx.clone();
    tokio::spawn(async move {
        while let Some(line) = lines_rx.recv().await {
            if forward.send(UpdateMsg::Line { job, line }).is_err() {
                break;
            }
        }
    });
    // Blocking on purpose: the script is a subprocess this waits on, and a
    // blocking wait on a runtime worker would starve the console it is meant
    // to leave usable.
    tokio::task::spawn_blocking(move || {
        let ran = match how {
            Take::Update => crate::update::run_streaming(check, None, false, lines_tx),
            Take::Upgrade => crate::upgrade::run_streaming(check, None, false, lines_tx),
        };
        let msg = match ran {
            Ok(o) if o.replaced => UpdateMsg::Done {
                job,
                said: format!(
                    "{verb} installed — this console is still running the build it started with"
                ),
                ok: true,
                replaced: true,
            },
            Ok(_) if check => UpdateMsg::Done {
                job,
                said: "checked — nothing was installed".to_string(),
                ok: true,
                replaced: false,
            },
            Ok(_) => UpdateMsg::Done {
                job,
                said: format!("{verb} finished — this console is already running that build"),
                ok: true,
                replaced: false,
            },
            Err(e) => UpdateMsg::Done {
                job,
                said: format!("{verb} failed: {e:#}"),
                ok: false,
                replaced: false,
            },
        };
        let _ = tx.send(msg);
    });
}

/// `/reload` — become the `jod` that is on disk now.
///
/// An `exec`, not a spawn-and-exit: the console keeps its terminal, its
/// process id and its place in whatever tmux window or SSH session is showing
/// it, which is what makes this usable as *the* way the always-on VPS console
/// takes an update. Nothing is lost that was not already on disk — agents are
/// their own process groups, and the conversation lives in SQLite.
///
/// On success this function does not return.
fn reload(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) {
    use std::os::unix::process::CommandExt;

    let exe = match crate::update::running_binary() {
        Ok(exe) => exe,
        Err(e) => {
            app.push(Entry::Notice(format!("cannot reload: {e:#}")));
            return;
        }
    };
    // The same arguments this console was started with, so a reload lands on
    // the same screen, harness and conversation rather than on the defaults.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // The terminal has to go back to the shell's own state *before* the exec,
    // because after it there is no code of ours left to do it.
    restore();
    let failed = std::process::Command::new(&exe).args(&args).exec();

    // Only reachable if the exec failed — take the screen back and say so,
    // rather than leaving a console that looks hung on a restored terminal.
    match enter() {
        Ok(fresh) => *terminal = fresh,
        Err(e) => {
            app.push(Entry::Notice(format!(
                "reload failed ({failed}) and the terminal could not be taken back: {e}"
            )));
            return;
        }
    }
    let _ = terminal.clear();
    app.push(Entry::Notice(format!(
        "could not reload into {}: {failed}",
        exe.display()
    )));
}

/// The fleet's rows, read out of this process's runs.
///
/// Public for the same reason `data` and `ui` are: `examples/screens.rs`
/// renders every screen off a real database and has to fill `App::agents` the
/// way the tick does. It called none of this and drew an empty fleet on a
/// database full of runs, which is worse than an error because it looks like
/// an answer.
/// Re-read the fleet, including the runs this process did not start.
///
/// `Jod::agents` reads an in-memory map, and every agent a project manager
/// starts is spawned by an MCP server in **another process**. `rehydrate` at
/// launch is what put the existing runs in that map, and nothing called it
/// again — so a manager's engineer appeared in the tree, which is built from
/// SQL, and was absent from the agent list, which is not.
///
/// The consequence was not cosmetic. `App::selected_agent` resolves a session
/// row through that list, so it answered `None` for a row visibly spinning, and
/// every run verb on it — `s`, `r`, `a`, `d` and the thread keys — refused with
/// "that row is a session with nothing running on it". There was no way to stop
/// an agent a manager had started, which is nearly every agent in the fleet.
///
/// Cheap to repeat by design: `rehydrate` checks the map before replaying
/// anything, and its own comment says that check exists because a full replay
/// would be "ruinous on a two-second timer". Only genuinely new runs cost.
///
/// A store that cannot be read leaves the list as it was and says so, rather
/// than emptying a fleet that is still out there working.
pub async fn refresh_fleet(jod: &Arc<Jod>, app: &mut App) {
    if let Err(e) = jod.rehydrate(200).await {
        app.push(Entry::Notice(format!(
            "could not read the fleet back from the store: {e}"
        )));
        return;
    }
    app.agents = list_agents(jod).await;
}

pub async fn list_agents(jod: &Arc<Jod>) -> Vec<AgentLine> {
    // Read once for the whole listing rather than per row. Without a store
    // every run reads as `Nothing`, which is the honest answer: with no ledger
    // there is nothing that could have failed to arrive.
    let store = jod.store();
    let mut lines: Vec<AgentLine> = jod
        .agents()
        .await
        .into_iter()
        .map(|a| AgentLine {
            // Asked before `a.id` is moved, and asked here rather than in the
            // renderer because `draw` is a pure function of state and must not
            // touch the database. This is the tick's job.
            delivery: store
                .as_ref()
                .map(|s| delivery::verdict_of_run(s, &a.id))
                .unwrap_or(delivery::Verdict::Nothing),
            id: a.id,
            name: a.name,
            harness: a.harness_label,
            status: format!("{:?}", a.status).to_lowercase(),
            session: a.session_id,
            created_at_ms: a.created_at_ms,
            cwd: a.cwd,
            cost_usd: a.usage.cost_usd,
            last: a.last_message,
        })
        .collect();
    // Running first, then newest: the panel's top row should be the thing most
    // likely to need attention, not whatever was launched first three days ago.
    lines.sort_by(|a, b| {
        b.is_running()
            .cmp(&a.is_running())
            .then(b.created_at_ms.cmp(&a.created_at_ms))
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_on(harness: HarnessKind) -> App {
        App::new(harness, None, Resume::Fresh)
    }

    /// `/harness` no longer moves the app itself — it hands the switch back,
    /// because carrying the conversation across needs a run first. The move it
    /// eventually makes is [`point_at`], and everything this test asserted about
    /// the old arm still has to hold there.
    #[test]
    fn switching_harness_drops_the_old_model() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        // A turn ran, so the harness reported what it used.
        app.reported_model = Some("claude-opus-5".into());
        app.model = Some("opus".into());
        app.cost_usd = 0.11;

        let action = apply_slash(&mut app, command::Slash::Harness(HarnessKind::OpenCode));
        assert_eq!(action, Some(Action::SwitchHarness(HarnessKind::OpenCode)));
        point_at(
            &mut app,
            &mut Thread::default(),
            HarnessKind::OpenCode,
            None,
            None,
        );

        // Neither name may survive: OpenCode rejects both, and passing either
        // made the switch look like it had not happened at all.
        assert_eq!(app.model, None);
        assert_eq!(app.reported_model, None);
        assert_eq!(app.cost_usd, 0.0);
        assert_eq!(app.harness, HarnessKind::OpenCode);
        // The harness session belonged to the harness being left.
        assert_eq!(app.session, None);
        assert_eq!(app.resume, Resume::Fresh);
    }

    /// `/login` with nothing after it means the harness on screen. That is the
    /// one that just failed to authenticate, and asking a person to retype its
    /// name in the console that is already showing it is asking for the wrong
    /// name to be typed.
    #[test]
    fn login_with_no_argument_signs_in_to_the_harness_on_screen() {
        let mut app = app_on(HarnessKind::OpenCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Login(None)),
            Some(Action::SignIn(HarnessKind::OpenCode))
        );
    }

    /// Named explicitly, it goes where it was told — a conversation on one
    /// harness is a perfectly good place to fix another.
    #[test]
    fn login_with_a_name_signs_in_to_that_harness() {
        let mut app = app_on(HarnessKind::OpenCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Login(Some(HarnessKind::ClaudeCode))),
            Some(Action::SignIn(HarnessKind::ClaudeCode))
        );
        assert_eq!(
            app.harness,
            HarnessKind::OpenCode,
            "signing in to one harness must not move the conversation to it"
        );
    }

    /// The action travels to the loop because the flow needs the real
    /// terminal. Anything else that receives it has to say so rather than
    /// start a sign-in nobody can see — the same rule `$EDITOR` follows.
    #[tokio::test]
    async fn a_sign_in_that_reaches_the_wrong_handler_says_where_it_belongs() {
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::SignIn(HarnessKind::ClaudeCode),
        )
        .await;
        let said = app.transcript.iter().rev().find_map(|e| match e {
            Entry::Notice(text) => Some(text.clone()),
            _ => None,
        });
        assert!(
            said.as_deref()
                .is_some_and(|t| t.contains("jod login claude-code")),
            "a handler that cannot lend the terminal has to name the one that \
             can: {said:?}"
        );
    }

    /// The model was just dropped, so the next question is what the new harness
    /// takes — and the answer is a list only the completion popup holds. The
    /// popup opens on the text in the input box, so a prefilled `/model ` *is*
    /// the picker appearing.
    #[test]
    fn arriving_at_a_new_harness_opens_the_model_picker() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        point_at(
            &mut app,
            &mut Thread::default(),
            HarnessKind::OpenCode,
            None,
            None,
        );

        assert_eq!(app.input, "/model ");
        assert_eq!(app.cursor, app.input.len(), "the cursor goes after it");
        assert!(
            !command::completions(&app.input, &app).is_empty(),
            "and that is a line the popup opens on"
        );
    }

    /// `harness-eats-prompt`: typing a plain word right after a switch used to
    /// land inside the auto-offered `/model ` line instead of starting a
    /// prompt of its own, so "hello" became "/model hello" — a model rename,
    /// not a turn — and the reply that finally *did* spawn failed a run later,
    /// naming neither Jod nor the cause.
    ///
    /// One word on purpose, not a sentence: `/model` now refuses anything with
    /// a space in it (see `command::parse`'s validation), which already turns
    /// a multi-word prompt into a visible, immediate refusal instead of a
    /// silent swallow. A bare word is indistinguishable from a real model name
    /// at that layer — `hello`, `continue`, `go`, `yes` are all one token —
    /// so it sails straight through and is exactly the case still left for
    /// this fix to close: the offer must never reach the parser as an
    /// argument in the first place.
    ///
    /// The offer is a hint, not something the user asked to type into, so the
    /// first character typed after it must begin a fresh line and the whole
    /// word must reach `Action::Send`.
    #[test]
    fn typing_after_a_switch_starts_a_prompt_not_a_model_name() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();
        point_at(&mut app, &mut thread, HarnessKind::OpenCode, None, None);
        assert_eq!(app.input, "/model ", "the offer landed as documented");
        assert!(thread.model_offer_unread, "and nobody has read it yet");

        for c in "hello".chars() {
            on_key(
                &mut app,
                &mut thread,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                20,
            );
        }
        assert_eq!(app.input, "hello", "not \"/model hello\"");
        assert!(!thread.model_offer_unread, "the first keystroke reads it");

        let action = on_key(
            &mut app,
            &mut thread,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            20,
        );
        assert_eq!(
            action,
            Some(Action::Send("hello".to_string())),
            "a run should spawn with this prompt, not a model change"
        );
    }

    /// A half-typed prompt is worth more than a hint. The switch can finish at
    /// any moment — the summariser is a whole run — and landing on somebody
    /// mid-sentence must not eat the sentence.
    #[test]
    fn the_picker_never_overwrites_something_being_typed() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.input = "port the parser to".into();
        app.cursor = app.input.len();

        point_at(
            &mut app,
            &mut Thread::default(),
            HarnessKind::OpenCode,
            None,
            None,
        );
        assert_eq!(app.input, "port the parser to");
    }

    /// `/config harness` is about new conversations, so its offer is the
    /// preference and not the live command.
    #[test]
    fn choosing_a_default_harness_offers_the_default_model_next() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::Slash::Config(config::Request::Set(
                config::Pref::Harness,
                config::Value::Harness(HarnessKind::ClaudeCode),
            )),
        );
        assert_eq!(app.input, "/config model ");
    }

    /// ...but only when the harness chosen is the one this session is on. The
    /// loaded list belongs to `app.harness`; offering it for a preference about
    /// a *different* harness would suggest names that harness rejects, which is
    /// the failure the live list exists to prevent.
    #[test]
    fn no_picker_for_a_harness_whose_models_are_not_the_ones_loaded() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::Slash::Config(config::Request::Set(
                config::Pref::Harness,
                config::Value::Harness(HarnessKind::Agy),
            )),
        );
        assert!(app.input.is_empty(), "{}", app.input);
    }

    /// Setting anything else leaves the box alone.
    #[test]
    fn the_other_preferences_do_not_open_a_picker() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::Slash::Config(config::Request::Set(
                config::Pref::Mode,
                config::Value::Mode(PermissionPolicy::Plan),
            )),
        );
        assert!(app.input.is_empty(), "{}", app.input);
    }

    #[test]
    fn a_reported_model_never_becomes_the_next_request() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.apply(&AgentEvent::Started {
            session_id: Some("s1".into()),
            model: Some("claude-opus-5".into()),
        });
        // Reported, so it shows; requested, so it does not.
        assert_eq!(app.reported_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(app.model, None);
        assert!(app.status().contains("claude-opus-5"));
    }

    /// And it stops showing the moment another model is asked for. `status`
    /// prefers the reported model over the requested one, so leaving the old
    /// one in place made `/model` print "model: haiku" into a transcript whose
    /// status bar still read `claude-opus-5` — the switch had worked, and every
    /// visible sign said it had not.
    #[test]
    fn choosing_a_model_retires_the_one_the_last_run_reported() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.apply(&AgentEvent::Started {
            session_id: Some("s1".into()),
            model: Some("claude-opus-5".into()),
        });

        apply_slash(&mut app, command::Slash::Model(Some("haiku".into())));
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert_eq!(app.reported_model, None);
        let status = app.status();
        assert!(status.contains("haiku"), "{status}");
        assert!(!status.contains("claude-opus-5"), "{status}");
    }

    /// Handing the choice back to the harness is the same rule: the model the
    /// last run reported is not what the next one will pick, so the status bar
    /// must claim nothing rather than claim the old name.
    #[test]
    fn clearing_the_model_retires_the_reported_one_too() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.apply(&AgentEvent::Started {
            session_id: Some("s1".into()),
            model: Some("claude-opus-5".into()),
        });

        apply_slash(&mut app, command::Slash::Model(None));
        assert_eq!(app.model, None);
        assert_eq!(app.reported_model, None);
        assert!(!app.status().contains("claude-opus-5"), "{}", app.status());
    }

    /// An OpenCode session whose model list has loaded.
    fn opencode_with_list() -> App {
        let mut app = app_on(HarnessKind::OpenCode);
        app.models = jod_core::harness::models::parse(
            HarnessKind::OpenCode,
            "opencode/claude-opus-5\nopencode/hy3-free\n",
        );
        app.models_for = Some(HarnessKind::OpenCode);
        app
    }

    /// `/model claude-opus-5` on OpenCode is the mistake that broke main: the
    /// model is right, the spelling belongs to Claude Code, and OpenCode
    /// answers every turn after it with a server error that names nothing. It
    /// has to be caught where it is typed, and the id that would have worked
    /// has to be on the screen.
    #[test]
    fn a_model_the_harness_does_not_have_is_refused_rather_than_stored() {
        let mut app = opencode_with_list();
        let action = apply_slash(&mut app, command::Slash::Model(Some("claude-opus-5".into())));

        assert!(action.is_none(), "a refused name must not be written down");
        assert_eq!(app.model, None, "the working choice is left alone");
        match app.transcript.last() {
            Some(Entry::Notice(said)) => {
                assert!(said.contains("no model called claude-opus-5"), "{said}");
                assert!(said.contains("/model opencode/claude-opus-5"), "{said}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The refusal must not cost the model that was already working. Somebody
    /// mistyping a name should end the command where they started it.
    #[test]
    fn a_refused_name_leaves_the_previous_model_in_place() {
        let mut app = opencode_with_list();
        app.model = Some("opencode/hy3-free".into());
        apply_slash(&mut app, command::Slash::Model(Some("claude-opus-5".into())));
        assert_eq!(app.model.as_deref(), Some("opencode/hy3-free"));
    }

    /// A name the harness does have goes through exactly as it always did.
    #[test]
    fn a_model_on_the_harnesss_own_list_is_still_stored() {
        let mut app = opencode_with_list();
        let action = apply_slash(
            &mut app,
            command::Slash::Model(Some("opencode/claude-opus-5".into())),
        );
        assert!(action.is_some(), "an accepted name is written down");
        assert_eq!(app.model.as_deref(), Some("opencode/claude-opus-5"));
    }

    /// The list is an aid, not a gate. When the harness could not be asked —
    /// no binary, a failed `models`, an answer still in flight — `/model`
    /// accepts whatever it is given, which is how it behaved before any of
    /// this and is the only safe reading of an empty list.
    #[test]
    fn a_model_is_accepted_unchecked_when_no_list_ever_loaded() {
        let mut app = app_on(HarnessKind::OpenCode);
        let action = apply_slash(&mut app, command::Slash::Model(Some("claude-opus-5".into())));
        assert!(action.is_some());
        assert_eq!(app.model.as_deref(), Some("claude-opus-5"));
    }

    /// Re-selecting the harness you are on must change nothing at all. It used
    /// to reset the session cursor either way — a "no-op" that silently threw
    /// away the conversation you were in the middle of — and it must not become
    /// an error instead: `Store::switch_harness` refuses a same-harness move,
    /// and passing that refusal to the screen would answer a harmless keystroke
    /// with a failure.
    #[test]
    fn re_selecting_the_same_harness_keeps_the_chosen_model() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.model = Some("haiku".into());
        app.session = Some("claude-session-1".into());
        app.resume = Resume::Session("claude-session-1".into());
        app.cost_usd = 0.11;

        let action = apply_slash(&mut app, command::Slash::Harness(HarnessKind::ClaudeCode));
        assert_eq!(action, Some(Action::SwitchHarness(HarnessKind::ClaudeCode)));
        assert_eq!(
            crossing(None, &app, &Thread::default(), HarnessKind::ClaudeCode),
            Crossing::Stay,
            "nothing to summarise, nothing to hand over, nothing to reset"
        );

        // Nothing crossed a harness boundary, so the choice stands.
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert_eq!(app.session.as_deref(), Some("claude-session-1"));
        assert_eq!(app.cost_usd, 0.11);
    }

    // ---- switching harness ----------------------------------------------

    /// A conversation with something in it, bound to the chat box the way a
    /// handoff binds one.
    fn talking_into(s: &RealStore, harness: HarnessKind) -> (App, Thread) {
        use jod_core::conversation::NewMessage;
        let c = s.new_conversation(harness, "/tmp", Some("opus")).unwrap();
        s.append_message(&c.id, NewMessage::user("port the parser"))
            .unwrap();
        s.append_message(
            &c.id,
            NewMessage::new(jod_core::conversation::Role::Assistant, "ported it"),
        )
        .unwrap();
        let mut app = app_on(harness);
        app.session = Some("session-1".into());
        app.resume = Resume::Session("session-1".into());
        (
            app,
            Thread {
                conversation: Some(c.id),
                ..Thread::default()
            },
        )
    }

    fn pending(thread: &Thread, intent: Summarising) -> PendingSummary {
        PendingSummary {
            intent,
            run: "run-summariser".into(),
            conversation: thread.conversation.clone().expect("a bound conversation"),
        }
    }

    /// The whole point of the verb: end up somewhere else, holding what you had.
    /// Before this, `/harness` set `resume = Fresh`, `session = None`,
    /// `model = None` and walked away from the conversation entirely.
    #[test]
    fn a_switch_lands_on_a_new_conversation_on_the_target_harness() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let was = thread.conversation.clone().unwrap();

        let switch = pending(&thread, Summarising::Handover(HarnessKind::OpenCode));
        finish_summary(
            &s,
            &mut app,
            &mut thread,
            &switch,
            "the parser is ported; tests are green",
        );

        assert_eq!(app.harness, HarnessKind::OpenCode);
        let now = thread.conversation.clone().expect("bound to the new one");
        assert_ne!(now, was, "a new conversation, not the old one relabelled");

        let landed = s.conversation(&now).unwrap().unwrap();
        assert_eq!(landed.harness_kind(), Some(HarnessKind::OpenCode));
        assert_eq!(landed.forked_from.as_deref(), Some(was.as_str()));
        // ...and the summary is in it, visibly, not only in the replay.
        let live = s.live_window(&now).unwrap();
        assert_eq!(live.len(), 1, "the thread became one turn: {live:?}");
        assert!(live[0].text.contains("the parser is ported"));

        // The original is still there and still on its own harness.
        let left = s.conversation(&was).unwrap().unwrap();
        assert_eq!(left.harness_kind(), Some(HarnessKind::ClaudeCode));
    }

    /// Landing on the new conversation is not enough on its own: the new harness
    /// has no session to be resumed into, and nothing in `runner` can stream a
    /// transcript at one. So the summary has to travel in the next spawn's
    /// system framing, exactly once.
    #[test]
    fn the_summary_crosses_into_the_first_turn_and_not_the_second() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);

        let before = thread
            .conversation
            .clone()
            .expect("a conversation to leave");
        let switch = pending(&thread, Summarising::Handover(HarnessKind::OpenCode));
        finish_summary(
            &s,
            &mut app,
            &mut thread,
            &switch,
            "the parser is ported; tests are green",
        );

        let carried = thread.carried.clone().expect("context for the first turn");
        assert!(carried.contains("the parser is ported"));
        assert!(
            carried.contains("not instructions to"),
            "framed as a record rather than a fresh instruction: {carried}"
        );
        // The binding moved with the switch, so the next turn does not
        // fragment the thread the moment it crosses.
        let after = thread
            .conversation
            .clone()
            .expect("bound to the conversation the switch minted");
        assert_ne!(before, after, "the binding should have followed the switch");

        // `orchestrate` drops it after one hand-over. From there the new
        // harness has a session of its own and is holding the context itself.
        thread.carried = None;
        assert_eq!(thread.carried, None);
    }

    /// AGY has no import path, so its context can only arrive as prose. That is
    /// the one loss a user could still avoid by choosing another target, so it
    /// is said before the move — and asked of the store, so there is no second
    /// copy of the rule to drift.
    #[test]
    fn a_lossy_target_says_so_before_the_move_and_a_lossless_one_stays_quiet() {
        let s = store();
        let (_, thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let id = thread.conversation.unwrap();

        let warned = lossy_warning(&s, &id, HarnessKind::Agy).expect("AGY loses structure");
        assert!(warned.contains("no import path"), "{warned}");
        assert_eq!(lossy_warning(&s, &id, HarnessKind::OpenCode), None);
    }

    /// The failure that matters most. A half-completed switch — new harness, no
    /// context — is strictly worse than one that did not happen, because the
    /// conversation is still there and the user no longer has a way back to it.
    ///
    /// And nothing is invented to fill the gap: `Store::switch_harness` treats an
    /// empty summary as an error precisely so a thread cannot be compacted into
    /// nothing, and a placeholder here would walk straight through that guard.
    #[test]
    fn a_summary_that_came_back_empty_leaves_the_conversation_where_it_was() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let was = thread.conversation.clone().unwrap();

        let switch = pending(&thread, Summarising::Handover(HarnessKind::OpenCode));
        finish_summary(&s, &mut app, &mut thread, &switch, "   \n  ");

        assert_eq!(app.harness, HarnessKind::ClaudeCode, "still where it was");
        assert_eq!(app.session.as_deref(), Some("session-1"), "still resumable");
        assert_eq!(thread.conversation.as_deref(), Some(was.as_str()));
        assert_eq!(thread.carried, None);
        assert_eq!(s.conversations(10).unwrap().len(), 1, "nothing was minted");
        assert_eq!(s.live_window(&was).unwrap().len(), 2, "nothing compacted");
        assert!(s.compactions(&was).unwrap().is_empty());
        assert!(
            app.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice(n) if n.contains("empty") && n.contains("Claude Code")
            )),
            "and it says so: {:?}",
            app.transcript
        );
    }

    /// Switching before anything has been said is the most ordinary moment to do
    /// it. Spending a model call to summarise nothing would make the cheapest
    /// case the slowest one.
    #[test]
    fn an_empty_thread_crosses_without_paying_for_a_summary() {
        let s = store();
        let app = app_on(HarnessKind::ClaudeCode);
        let empty = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        let bound = Thread {
            conversation: Some(empty.id),
            ..Thread::default()
        };

        assert_eq!(
            crossing(Some(&s), &app, &bound, HarnessKind::OpenCode),
            Crossing::Bare
        );
        // Nothing said and nothing bound is the same answer, reached without
        // touching the database at all.
        assert_eq!(
            crossing(Some(&s), &app, &Thread::default(), HarnessKind::OpenCode),
            Crossing::Bare
        );
        // ...and so is having no database.
        assert_eq!(
            crossing(None, &app, &bound, HarnessKind::OpenCode),
            Crossing::Bare
        );
    }

    /// A thread with something in it owes a summary, and who writes it depends
    /// on whether the harness being left can still be asked. Immediately after a
    /// previous switch it cannot — it has no session — so the record has to
    /// travel in the prompt, and that is exactly the case that would be missed.
    #[test]
    fn a_harness_with_no_session_is_handed_the_record_to_summarise() {
        let s = store();
        let (mut app, thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let id = thread.conversation.clone().unwrap();

        assert_eq!(
            crossing(Some(&s), &app, &thread, HarnessKind::OpenCode),
            Crossing::Summarise {
                conversation: id.clone(),
                material: None,
            },
            "it has the session, so it can be asked about what it is holding"
        );

        app.resume = Resume::Fresh;
        let Crossing::Summarise { material, .. } =
            crossing(Some(&s), &app, &thread, HarnessKind::OpenCode)
        else {
            panic!("a thread with turns in it owes a summary");
        };
        let material = material.expect("nothing to resume, so the record travels");
        assert!(material.contains("port the parser"));
    }

    // ---- compacting ------------------------------------------------------

    /// The whole point of `/compact`: end up on a thread the harness will not
    /// resume, holding what the old one said. The number on the context bar has
    /// to come down with it — it is the reading that started this, and a stale
    /// one would start the next compaction immediately.
    #[test]
    fn compacting_lands_on_a_continuation_the_next_turn_will_not_resume() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let was = thread.conversation.clone().unwrap();
        app.context_tokens = 180_000;
        app.cost_usd = 0.42;
        app.model = Some("opus".into());

        let job = pending(&thread, Summarising::Compaction { asked: true });
        finish_summary(
            &s,
            &mut app,
            &mut thread,
            &job,
            "the parser is ported; tests are green",
        );

        assert_eq!(
            app.harness,
            HarnessKind::ClaudeCode,
            "staying put is what makes it a compaction rather than a switch"
        );
        assert_eq!(app.resume, Resume::Fresh, "the long session is left behind");
        assert_eq!(app.session, None);
        assert_eq!(app.context_tokens, 0, "the new session holds nothing yet");
        // The harness did not change, so nothing about it is invalidated. A
        // switch drops these; a compaction has no reason to.
        assert_eq!(app.model.as_deref(), Some("opus"));
        assert_eq!(app.cost_usd, 0.42, "the spend is the same conversation's");

        let now = thread.conversation.clone().expect("bound to the new one");
        assert_ne!(now, was, "a new thread, not the old one relabelled");
        let landed = s.conversation(&now).unwrap().unwrap();
        assert_eq!(landed.harness_kind(), Some(HarnessKind::ClaudeCode));
        assert_eq!(landed.forked_from.as_deref(), Some(was.as_str()));

        // The summary is in it, visibly, and reaches the next turn's prompt
        // because there is no session to resume it into.
        let live = s.live_window(&now).unwrap();
        assert_eq!(live.len(), 1, "the thread became one turn: {live:?}");
        assert!(live[0].text.contains("the parser is ported"));
        let carried = thread.carried.clone().expect("context for the first turn");
        assert!(carried.contains("the parser is ported"), "{carried}");

        // Compacted, not deleted. The turns the summary stands in for are still
        // rows, and still findable — that is what makes an early compaction a
        // cost rather than a loss.
        assert_eq!(s.thread(&was).unwrap().len(), 2);
        assert!(
            s.search_messages("port the parser", 10)
                .unwrap()
                .iter()
                .any(|hit| hit.message.text == "port the parser"),
            "the compacted turn is still searchable"
        );
    }

    /// The failure that matters most, and the same one a switch has: a thread
    /// pointed at a fresh session with no summary behind it has simply lost its
    /// context. Nothing is invented to fill the gap.
    ///
    /// And an automatic pass that failed stops being automatic. The threshold
    /// that started it is met on every turn once it is crossed, so a failure
    /// that repeats is a model call after every turn forever.
    #[test]
    fn an_empty_summary_leaves_the_conversation_alone_and_stops_the_automatic_pass() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);
        let was = thread.conversation.clone().unwrap();

        let job = pending(&thread, Summarising::Compaction { asked: false });
        finish_summary(&s, &mut app, &mut thread, &job, "  \n ");

        assert_eq!(app.resume, Resume::Session("session-1".into()));
        assert_eq!(thread.conversation.as_deref(), Some(was.as_str()));
        assert_eq!(thread.carried, None);
        assert_eq!(s.conversations(10).unwrap().len(), 1, "nothing was minted");
        assert_eq!(s.live_window(&was).unwrap().len(), 2, "nothing compacted");
        assert!(!app.auto_compact, "it will not keep trying on its own");
        assert!(
            app.transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice(n) if n.contains("/compact"))),
            "and it says the command is still there: {:?}",
            app.transcript
        );
    }

    /// A person asking is not a loop, so a `/compact` that failed leaves the
    /// automatic pass exactly as it was.
    #[test]
    fn a_compaction_somebody_asked_for_does_not_switch_the_automatic_one_off() {
        let s = store();
        let (mut app, mut thread) = talking_into(&s, HarnessKind::ClaudeCode);

        let job = pending(&thread, Summarising::Compaction { asked: true });
        finish_summary(&s, &mut app, &mut thread, &job, "");

        assert!(app.auto_compact);
    }

    /// Two triggers, one action — and a long list of moments where firing would
    /// be wrong. Each `false` below is a model call not spent.
    #[test]
    fn compacting_on_its_own_waits_for_a_reason_and_for_the_screen_to_be_idle() {
        use super::app::{COMPACT_AT, CONTEXT_WINDOW};
        let ours = || Thread {
            watching_own_turn: true,
            ..Thread::default()
        };
        let idle = ours();

        let mut app = app_on(HarnessKind::ClaudeCode);
        assert!(!compaction_is_due(&app, &idle), "an empty window is no reason");

        // What the harness is holding.
        app.context_tokens = (CONTEXT_WINDOW as f64 * COMPACT_AT) as u64;
        assert!(compaction_is_due(&app, &idle));

        // ...and the orchestrator's own verdict on the main chat, which fires on
        // a long idle as well as on size and is measured somewhere else
        // entirely.
        let owed = Thread {
            compaction_owed: true,
            ..ours()
        };
        let quiet = app_on(HarnessKind::ClaudeCode);
        assert!(compaction_is_due(&quiet, &owed));

        // Mid-turn: it would summarise a thread still being written to.
        let mut busy = app_on(HarnessKind::ClaudeCode);
        busy.context_tokens = CONTEXT_WINDOW;
        busy.busy = true;
        assert!(!compaction_is_due(&busy, &idle));

        // Already summarising: a second one would overwrite the first.
        let under_way = Thread {
            summarising: Some(PendingSummary {
                intent: Summarising::Handover(HarnessKind::OpenCode),
                run: "run-1".into(),
                conversation: "c-1".into(),
            }),
            ..ours()
        };
        assert!(!compaction_is_due(&app, &under_way));

        // Somebody else's run on screen. The bar is reading their window, and
        // compacting it would take a running agent's thread out from under it.
        assert!(!compaction_is_due(&app, &Thread::default()));

        // Given up on after a failure. `/compact` still works; this does not.
        let mut off = app_on(HarnessKind::ClaudeCode);
        off.context_tokens = CONTEXT_WINDOW;
        off.auto_compact = false;
        assert!(!compaction_is_due(&off, &owed));
    }

    /// It used to print "the main chat is due for compaction (size) — 128400
    /// chars live", which named a problem and left the reader to fix it with a
    /// command that did not exist. It is a trigger now, not a line.
    #[test]
    fn the_main_chat_being_due_for_compaction_is_recorded_rather_than_announced() {
        let mut thread = Thread {
            watching_own_turn: true,
            ..Thread::default()
        };
        let app = app_on(HarnessKind::ClaudeCode);
        assert!(!compaction_is_due(&app, &thread), "nothing said so yet");

        thread.compaction_owed = true;
        assert!(compaction_is_due(&app, &thread));
    }

    /// `/compact` reaches the same flow `/harness` does, and for the same
    /// reason: a summary needs a model, and Jod has none.
    #[test]
    fn the_compact_command_asks_for_a_compaction() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Compact),
            Some(Action::Compact)
        );
    }

    /// The binding is Jod's conversation, not the harness's session, and it does
    /// not follow the cursor when the cursor moves somewhere else. Without this,
    /// a conversation handed over by `/harness` would keep collecting the turns
    /// of the fresh one that replaced it — and it is also the way *out* of the
    /// main chat, which binds the same field.
    #[test]
    fn starting_or_resuming_something_else_drops_the_binding() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::New),
            Some(Action::NewThread)
        );
        assert_eq!(
            apply_slash(&mut app, command::Slash::Resume("s-99".into())),
            Some(Action::NewThread)
        );

        // An unbound thread records each turn in a conversation of its own,
        // which is what every turn outside the main chat does.
        assert_eq!(Thread::default().binding(), RunConversation::New);
    }

    /// `/new` and `/clear` both mean "start again from the top", and the top is
    /// the splash. Emptying the transcript is not enough on its own: the splash
    /// belongs to the chat workspace and is not drawn while a run is being
    /// watched, so either command typed from the fleet used to clear a screen
    /// you were not looking at and leave you standing in the fleet.
    #[test]
    fn new_and_clear_both_land_back_on_the_splash() {
        for slash in [command::Slash::New, command::Slash::Clear] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            app.go(Workspace::Fleet);
            app.watching = Some("aaa11111".into());
            app.overlay = Overlay::Jobs;
            app.push(Entry::You("port the parser".into()));

            apply_slash(&mut app, slash.clone());

            assert_eq!(app.workspace, Workspace::Chat, "{slash:?} goes home");
            assert!(app.watching.is_none(), "{slash:?} stops watching");
            assert!(
                matches!(app.overlay, Overlay::None),
                "{slash:?} drops the overlay"
            );
            // `/new` leaves its own line behind, and it is a *hint* — Jod
            // talking on its own account — which is the only class of entry
            // the splash may still cover. A notice here would be output from
            // something the user asked for, and would (correctly) take the
            // screen off the splash.
            assert!(
                !app.transcript.iter().any(|e| !matches!(e, Entry::Hint(_))),
                "{slash:?} leaves nothing but hints, which is what the splash needs"
            );
        }
    }

    /// The fault this command was reported for: it emptied the screen and left
    /// the session alone, so the next message picked up the whole conversation
    /// the user had just watched disappear.
    ///
    /// These three fields are what an ordinary turn resumes from, and clearing
    /// them is the entire reset outside the main chat — `send_turn` hands
    /// `app.resume` to `spawn` and the harness is given nothing to continue.
    #[test]
    fn clearing_starts_the_next_turn_with_no_context() {
        let mut app = App::new(HarnessKind::ClaudeCode, None, Resume::Session("s-1".into()));
        app.session = Some("s-1".into());
        app.cost_usd = 1.25;

        assert_eq!(
            apply_slash(&mut app, command::Slash::Clear),
            Some(Action::Clear),
            "the database half of the same command"
        );

        assert_eq!(app.resume, Resume::Fresh, "nothing to continue");
        assert_eq!(app.session, None);
        assert_eq!(app.cost_usd, 0.0, "a fresh context costs nothing yet");
    }

    /// `/clear` does not mean `/new`. It starts the conversation over where you
    /// are standing, so the binding survives it — otherwise clearing the main
    /// chat would quietly walk you out of it and the next line you typed would
    /// land in a conversation of its own.
    #[test]
    fn clearing_keeps_you_where_you_are_standing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_ne!(
            apply_slash(&mut app, command::Slash::Clear),
            Some(Action::NewThread),
            "leaving the conversation is what /new is for"
        );
    }

    /// The main chat keeps its resume cursor in the database rather than on the
    /// app, so clearing at the desk has to be a write. Without one the screen
    /// went blank and `hand_to_orchestrator` read the same session id straight
    /// back off the row on the very next message.
    ///
    /// What it drops is the harness session, which is the only thing carrying a
    /// model's context window. Jod's own transcript stays: Jod owns the record,
    /// and a reset that destroyed it would leave the main chat unauditable from
    /// whichever surface reset it last.
    #[test]
    fn clearing_the_main_chat_drops_its_session_and_keeps_its_transcript() {
        use jod_core::conversation::{NewMessage, Role};
        let s = store();
        let id = s
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        s.set_conversation_session(&id, Some("sess-9")).unwrap();
        s.append_message(&id, NewMessage::new(Role::User, "count them"))
            .unwrap();

        let inside = Thread {
            conversation: Some(id.clone()),
            ..Thread::default()
        };
        let said = forget_bound_session(&s, &inside);

        assert_eq!(
            s.resume_for(&id, HarnessKind::ClaudeCode).unwrap(),
            Resume::Fresh,
            "the next turn in the main chat starts with nothing behind it"
        );
        assert_eq!(
            s.live_window(&id).unwrap().len(),
            1,
            "Jod keeps what was said"
        );
        // The main chat is shared with `jod main` and the phone, so clearing it
        // here clears it there. The user hears that rather than discovering it.
        assert!(
            matches!(said, Some(Entry::Hint(line)) if line.contains("every surface")),
            "the shared chat says so"
        );
    }

    /// An ordinary conversation is cleared just as thoroughly and says nothing
    /// about it, because there is no second surface for the news to be about.
    #[test]
    fn clearing_an_ordinary_conversation_is_quiet_about_it() {
        let s = store();
        let other = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.set_conversation_session(&other.id, Some("sess-9"))
            .unwrap();

        let elsewhere = Thread {
            conversation: Some(other.id.clone()),
            ..Thread::default()
        };
        assert!(
            forget_bound_session(&s, &elsewhere).is_none(),
            "nothing worth a line"
        );
        assert_eq!(
            s.resume_for(&other.id, HarnessKind::ClaudeCode).unwrap(),
            Resume::Fresh
        );
    }

    /// `/clear` stops looking; it does not touch what it was looking at.
    ///
    /// An unbound thread watching somebody else's run is the case that made
    /// this worth its own function. Resolving the conversation the way the rest
    /// of the file does — `current_conversation`, which falls back to the
    /// watched run — would have reached into that agent and forgotten *its*
    /// session, ending a run the user only meant to stop reading.
    #[test]
    fn clearing_never_reaches_into_the_run_it_was_watching() {
        let s = store();
        let theirs = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.set_conversation_session(&theirs.id, Some("sess-9"))
            .unwrap();

        assert!(forget_bound_session(&s, &Thread::default()).is_none());
        assert_eq!(
            s.conversation(&theirs.id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("sess-9"),
            "the agent on screen carries on with the session it had"
        );
    }

    /// Entering the chat has to *show* it, or the pinned row is a move to a
    /// blank screen and the conversation is still only readable from the CLI.
    #[test]
    fn entering_the_chat_replays_what_is_in_it() {
        use jod_core::conversation::{NewMessage, Role};
        let s = store();
        let id = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();

        // Empty: a sentence saying so, not a blank screen.
        let empty = replay(&s.live_window(&id).unwrap(), "the main chat", true);
        assert!(
            matches!(&empty[..], [Entry::Notice(n)] if n.contains("nothing said yet")),
            "{empty:?}"
        );

        s.append_message(&id, NewMessage::new(Role::User, "count the rust files"))
            .unwrap();
        s.append_message(&id, NewMessage::new(Role::Assistant, "started an agent"))
            .unwrap();

        let entries = replay(&s.live_window(&id).unwrap(), "the main chat", true);
        // Your turns read as yours and the chat's as the chat's — a replay that
        // flattened both to prose would be a transcript of nobody.
        assert!(matches!(&entries[0], Entry::You(t) if t == "count the rust files"));
        assert!(matches!(&entries[1], Entry::Agent(t) if t == "started an agent"));
        assert!(
            matches!(entries.last(), Some(Entry::Notice(n)) if n.contains("2 messages")
                && n.contains("live window")),
            "and says how much of it the harness is actually carrying: {entries:?}"
        );
    }

    /// The other half of "your turns read as yours": reasoning is neither, and
    /// replaying it as the chat's own prose put words in the chat's mouth.
    #[test]
    fn replayed_reasoning_reads_as_reasoning_and_goes_when_it_is_off() {
        use jod_core::conversation::{NewMessage, Role};
        let s = store();
        let id = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        s.append_message(&id, NewMessage::new(Role::User, "count them"))
            .unwrap();
        s.append_message(&id, NewMessage::new(Role::Thinking, "two ways to do this"))
            .unwrap();
        s.append_message(&id, NewMessage::new(Role::Assistant, "started an agent"))
            .unwrap();
        let live = s.live_window(&id).unwrap();

        let shown = replay(&live, "the main chat", true);
        assert!(
            matches!(&shown[1], Entry::Thinking(t) if t == "two ways to do this"),
            "{shown:?}"
        );

        let hidden = replay(&live, "the main chat", false);
        assert!(
            !hidden.iter().any(|e| matches!(e, Entry::Thinking(_))),
            "{hidden:?}"
        );
        // Hiding it drops the line, not the turn it belonged to.
        assert!(matches!(&hidden[1], Entry::Agent(t) if t == "started an agent"));
        // And the window is still reported at its true size — the chat carries
        // the reasoning whether or not this screen is drawing it.
        assert!(
            matches!(hidden.last(), Some(Entry::Notice(n)) if n.contains("3 messages")),
            "{hidden:?}"
        );
    }

    /// A stored tool call, the way a harness leaves one behind.
    fn call(name: &str, input: serde_json::Value) -> jod_core::conversation::NewMessage {
        use jod_core::conversation::{NewMessage, Role};
        NewMessage {
            // What the store keeps: a truncated rendering of the arguments,
            // which is the JSON that used to be painted as the chat's prose.
            text: input.to_string(),
            tool_name: Some(name.to_string()),
            tool_input: Some(input),
            ..NewMessage::new(Role::ToolCall, "")
        }
    }

    /// A stored tool result. The error flag rides in `tool_input`, because
    /// `0006_conversations` has no column for it.
    fn result(name: &str, text: &str, failed: bool) -> jod_core::conversation::NewMessage {
        use jod_core::conversation::{NewMessage, Role};
        NewMessage {
            tool_name: Some(name.to_string()),
            tool_input: failed.then(|| serde_json::json!({ "is_error": true })),
            ..NewMessage::new(Role::ToolResult, text)
        }
    }

    /// The bug as Reljod met it: open the console, and the chat you were in
    /// yesterday comes back as pages of `{"command":"mkdir -p …"}` and raw `ls`
    /// output, with `Ctrl-O` unable to do anything about it.
    ///
    /// The steps are the same steps whether you are watching them happen or
    /// reading them back, so they have to arrive as the same entries. `hidden`
    /// folds `Tool` and `ToolOut` and nothing else — replayed as `Agent`, this
    /// JSON was not a step the transcript could fold, it was the chat talking.
    #[test]
    fn a_replayed_tool_call_is_a_step_and_not_the_chats_own_words() {
        use jod_core::conversation::{NewMessage, Role};
        let s = store();
        let id = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        s.append_message(&id, NewMessage::new(Role::User, "make the folder"))
            .unwrap();
        s.append_message(
            &id,
            call("Bash", serde_json::json!({ "command": "mkdir -p racing-3d" })),
        )
        .unwrap();
        s.append_message(&id, result("Bash", "total 0\ndrwxr-xr-x  2 staff", false))
            .unwrap();
        s.append_message(&id, NewMessage::new(Role::Assistant, "made it"))
            .unwrap();

        let entries = replay(&s.live_window(&id).unwrap(), "the main chat", true);
        assert!(
            matches!(&entries[1], Entry::Tool { name, detail, step: _ }
                if name == "Bash" && detail.as_deref() == Some("mkdir -p racing-3d")),
            "the call reads as `⚙ Bash · mkdir -p racing-3d`: {entries:?}"
        );
        assert!(
            matches!(&entries[2], Entry::ToolOut { text, failed: false } if text.starts_with("total 0")),
            "and its output as output: {entries:?}"
        );
        assert!(
            !entries
                .iter()
                .any(|e| matches!(e, Entry::Agent(t) if t.contains("mkdir"))),
            "and nowhere as the chat's own prose: {entries:?}"
        );

        // The fold is the point of classifying them, so assert the fold and
        // not only the entry: a `Tool` line that still drew would have fixed
        // the colour of the noise and left the noise.
        let mut app = App::new(HarnessKind::ClaudeCode, None, Resume::Fresh);
        for entry in entries {
            app.push(entry);
        }
        let shown = |a: &App| (0..a.transcript.len()).filter(|i| !a.hidden(*i)).count();
        let quiet = shown(&app);
        app.expand_details = true;
        assert!(
            shown(&app) > quiet,
            "and Ctrl-O brings the steps back that entering the chat folded away"
        );
    }

    /// The same rule the live stream keeps: a failure is why the answer is
    /// about to be wrong, so it survives the fold. A transcript that hid the
    /// one step that went wrong would be worse than one that hid nothing.
    #[test]
    fn a_replayed_failure_is_never_folded() {
        let s = store();
        let id = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        s.append_message(&id, call("Bash", serde_json::json!({ "command": "ls /nope" })))
            .unwrap();
        s.append_message(&id, result("Bash", "No such file or directory", true))
            .unwrap();

        let entries = replay(&s.live_window(&id).unwrap(), "the main chat", true);
        let mut app = App::new(HarnessKind::ClaudeCode, None, Resume::Fresh);
        for entry in entries {
            app.push(entry);
        }
        let out = app
            .transcript
            .iter()
            .position(|e| matches!(e, Entry::ToolOut { failed: true, .. }))
            .expect("the failure is in the transcript");
        assert!(!app.hidden(out), "and on the screen: {:?}", app.transcript);
    }

    /// A plan replays as the plan block, not as a stack of `⚙ TodoWrite` rows.
    ///
    /// Every revision is its own stored call, so the arm that folds them into
    /// one block has to run on the way back too — a turn that ticked off five
    /// items otherwise returns as five identical lists.
    #[test]
    fn a_replayed_plan_is_one_block_however_often_it_was_revised() {
        let s = store();
        let id = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        for status in ["pending", "in_progress", "completed"] {
            s.append_message(
                &id,
                call(
                    "TodoWrite",
                    serde_json::json!({ "todos": [{ "content": "scaffold", "status": status }] }),
                ),
            )
            .unwrap();
        }

        let entries = replay(&s.live_window(&id).unwrap(), "the main chat", true);
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, Entry::Plan(_)))
                .count(),
            1,
            "one block: {entries:?}"
        );
        assert!(
            !entries.iter().any(|e| matches!(e, Entry::Tool { .. })),
            "and no `⚙ TodoWrite` beside it: {entries:?}"
        );
    }

    /// A manager's transcript must not sign off as the main chat.
    ///
    /// `enter_conversation` serves both, and the footer naming the wrong one is
    /// how you end up typing into tetris's manager believing you are in the
    /// chat that routes work — the one distinction the whole screen turns on.
    #[test]
    fn the_footer_names_the_conversation_it_is_actually_under() {
        use jod_core::conversation::{NewMessage, Role};
        let s = store();
        let id = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap()
            .id;
        s.append_message(&id, NewMessage::new(Role::User, "scaffold it"))
            .unwrap();

        let entries = replay(&s.live_window(&id).unwrap(), "racing-3d's manager", true);
        assert!(
            matches!(entries.last(), Some(Entry::Notice(n))
                if n.starts_with("racing-3d's manager ·") && !n.contains("main chat")),
            "{entries:?}"
        );
    }

    /// Which conversation you are in is what decides where a typed line goes,
    /// and exactly one of them is the main chat.
    ///
    /// Derived from the store rather than remembered on a flag, because the pin
    /// moves under this process — `/harness` carries it to the conversation it
    /// mints — and a flag set on entry would then point at the thread that was
    /// handed away.
    #[test]
    fn only_the_pinned_conversation_counts_as_being_in_the_main_chat() {
        let s = store();
        let pinned = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
        let other = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();

        let inside = Thread {
            conversation: Some(pinned),
            ..Thread::default()
        };
        assert!(inside.in_main(Some(&s)));

        let elsewhere = Thread {
            conversation: Some(other.id),
            ..Thread::default()
        };
        assert!(!elsewhere.in_main(Some(&s)), "an ordinary conversation");

        // Unbound is an ordinary thread, not the main chat.
        assert!(!Thread::default().in_main(Some(&s)));
        // And with no database there is no main chat to be in.
        assert!(!inside.in_main(None));
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        // A fresh, throwaway `Thread` every press: none of the many callers of
        // this helper care about state that lives on `Thread`, and giving each
        // press its own means adding `model_offer_unread` here never had to
        // touch the 200+ call sites that only ever wanted a keystroke and an
        // `App`. Tests that *do* need the flag to survive across presses call
        // `on_key` directly with a `Thread` they hold onto.
        on_key(
            app,
            &mut Thread::default(),
            KeyEvent::new(code, KeyModifiers::NONE),
            20,
        )
    }

    fn type_line(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// The grant that lets "schedule this for me" be carried out rather than
    /// merely acknowledged. Before this, a turn from the chat box was handed no
    /// Jod tools at all, so the harness could describe arming a schedule and
    /// had no verb with which to arm one.
    ///
    /// The chat box's half of it moved: its turns are `hand_to_orchestrator`'s
    /// now, which names [`ToolAccess::Orchestrate`] at the same place it names
    /// the conversation and the permission mode. Asserted against that enum
    /// rather than a TUI constant, because the TUI no longer holds one.
    #[test]
    fn a_turn_you_are_watching_may_schedule_and_a_delegation_may_not() {
        let watched = ToolAccess::Orchestrate;
        assert!(
            watched.may_orchestrate(),
            "cannot create a schedule or a goal"
        );
        assert!(watched.may_delegate());

        // The compounding failure `ToolAccess::unattended` exists to prevent: a
        // background agent that can create background agents has no bound at
        // all, and it multiplies while nobody is reading.
        let delegated = DELEGATED.expect("a delegation still gets to look");
        assert!(
            !delegated.may_orchestrate(),
            "an unwatched run could arm a schedule that spends every night"
        );
        assert!(
            !delegated.may_delegate(),
            "an unwatched run could start more unwatched runs"
        );
        assert_eq!(
            delegated,
            ToolAccess::unattended(),
            "the delegation path must track the codebase's own answer, not a copy of it"
        );
    }

    /// Tab is the mode key, and it works on every screen because "how much may
    /// this do" is a question you have everywhere, not only in the chat box.
    #[test]
    fn tab_moves_to_the_next_permission_mode() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.mode = PermissionPolicy::Plan;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.mode, PermissionPolicy::Ask);

        // And on a list screen too, without a transcript to write into.
        app.go(Workspace::Schedules);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.mode, PermissionPolicy::AcceptEdits);
    }

    /// Tab still finishes a half-typed command. One key doing two jobs is only
    /// safe while the layer that owns it is on screen — and the popup is.
    #[test]
    fn tab_completes_a_command_rather_than_changing_the_mode() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let before = app.mode;
        type_line(&mut app, "/harn");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/harness ", "the popup still owns Tab");
        assert_eq!(app.mode, before, "and the mode was left alone");
    }

    // ---- dictation ----
    //
    // The microphone is a switch and sentences arrive on their own, so these
    // are about what happens to a sentence *after* it has been transcribed —
    // the part that decides whether hands-free is safe.

    fn listening() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.dictation = app::Dictation::Listening {
            since_ms: 0,
            backend: "arecord".into(),
            pending: 0,
            speaking: false,
            level: 0.0,
            heard: 0,
        };
        app
    }

    /// Ordinary speech goes into the composer and nowhere else.
    #[test]
    fn a_dictated_sentence_lands_in_the_box_and_is_not_sent() {
        let mut app = listening();
        let sent = heard_utterance(&mut app, "i-refactor natin yung parser");
        assert!(sent.is_none(), "plain dictation was sent");
        assert_eq!(app.input, "i-refactor natin yung parser");
    }

    /// Sentences arrive one after another while listening, so they have to
    /// build a paragraph rather than weld together.
    #[test]
    fn consecutive_sentences_are_spaced_apart() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        heard_utterance(&mut app, "and run the tests");
        assert_eq!(app.input, "fix the parser and run the tests");
    }

    #[test]
    fn the_first_sentence_does_not_start_with_a_space() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        assert!(!app.input.starts_with(' '));
    }

    // ---- sending by voice, which is the whole point ----

    /// The request: say "go ahead" and it goes.
    #[test]
    fn saying_go_ahead_sends_what_is_in_the_box() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        let sent = heard_utterance(&mut app, "go ahead");
        assert_eq!(sent.as_deref(), Some("fix the parser"));
        assert!(app.input.is_empty(), "the box was not cleared after sending");
    }

    /// Saying the instruction and the command in one breath is how this is
    /// actually used.
    #[test]
    fn an_instruction_and_the_command_in_one_breath_both_land() {
        let mut app = listening();
        let sent = heard_utterance(&mut app, "run the tests, go ahead");
        assert_eq!(sent.as_deref(), Some("run the tests"));
    }

    /// The command word must never reach the orchestrator as part of the work.
    #[test]
    fn the_command_phrase_is_not_part_of_what_is_sent() {
        let mut app = listening();
        let sent = heard_utterance(&mut app, "deploy the api, sige na").unwrap();
        assert!(!sent.to_lowercase().contains("sige"), "{sent:?}");
    }

    /// The misfire that would matter most: this sentence is about work and
    /// must not dispatch anything.
    #[test]
    fn go_ahead_inside_a_sentence_does_not_send() {
        let mut app = listening();
        let sent = heard_utterance(&mut app, "let's go ahead and refactor the parser");
        assert!(sent.is_none(), "a sentence about work was sent");
        assert!(app.input.contains("refactor"));
    }

    /// Nothing to send is not an error worth dispatching an empty prompt over.
    #[test]
    fn sending_an_empty_box_sends_nothing() {
        let mut app = listening();
        assert!(heard_utterance(&mut app, "go ahead").is_none());
    }

    // ---- taking things back, hands-free ----

    #[test]
    fn saying_scratch_that_empties_the_box() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        heard_utterance(&mut app, "scratch that");
        assert!(app.input.is_empty());
    }

    /// A mis-heard sentence has to be cheap to remove without a keyboard.
    #[test]
    fn saying_undo_takes_back_only_the_last_sentence() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        heard_utterance(&mut app, "and delete the database");
        heard_utterance(&mut app, "undo that");
        assert_eq!(app.input, "fix the parser");
    }

    /// Undo works off remembered sentences, so it must refuse rather than
    /// guess once the tail is no longer the sentence it would remove.
    #[test]
    fn undo_refuses_when_the_line_has_been_edited_since() {
        let mut app = listening();
        heard_utterance(&mut app, "fix the parser");
        app.input.push_str(" by hand");
        heard_utterance(&mut app, "undo that");
        assert_eq!(app.input, "fix the parser by hand", "it guessed anyway");
    }

    /// The command that has to work when nothing else does.
    #[test]
    fn saying_stop_listening_asks_the_loop_to_switch_off() {
        let mut app = listening();
        heard_utterance(&mut app, "stop listening");
        assert!(app.stop_listening_requested);
    }

    /// Cancelling and sending in one breath must cancel: one reading loses a
    /// sentence, the other starts agents on a sentence just withdrawn.
    #[test]
    fn cancelling_beats_sending_when_both_are_heard() {
        let mut app = listening();
        heard_utterance(&mut app, "delete everything");
        let sent = heard_utterance(&mut app, "scratch that, go ahead");
        assert!(sent.is_none(), "a withdrawn instruction was sent");
        assert!(app.input.is_empty());
    }

    /// A transcript can legitimately be empty — whisper returns nothing for a
    /// door closing — and that must not put a stray space in the prompt.
    #[test]
    fn an_empty_transcript_changes_nothing() {
        let mut app = listening();
        heard_utterance(&mut app, "   ");
        assert!(app.input.is_empty());
    }

    // ---- the switch ----

    #[test]
    fn alt_v_is_handled_by_the_loop_because_it_owns_the_recorder() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert!(matches!(alt(&mut app, KeyCode::Char('v')), Some(Action::Dictate)));
    }

    /// Escape has a dozen meanings depending on the screen. While the
    /// microphone is live it has exactly one, and it must not depend on where
    /// the cursor happens to be.
    #[test]
    fn escape_stops_listening_from_anywhere() {
        let mut app = listening();
        assert!(matches!(
            press(&mut app, KeyCode::Esc),
            Some(Action::CancelDictation)
        ));
    }

    /// ...and no meaning at all when the microphone is off, or Escape would
    /// stop closing overlays.
    #[test]
    fn escape_is_left_alone_when_the_microphone_is_off() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert!(!matches!(
            press(&mut app, KeyCode::Esc),
            Some(Action::CancelDictation)
        ));
    }

    /// `Ctrl-P` reaches into the panel for one box. The sessions list and the
    /// context bar beside it are `Shift-Tab`'s, and a key that took all three
    /// off screen would be that key wearing a different letter.
    #[test]
    fn the_catalog_is_collapsed_without_closing_the_panel() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.panel = true;
        assert!(app.projects_open);

        // The first press takes the keyboard — the box is already on screen.
        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.panel_focused);
        assert!(app.projects_open);

        // The second puts it away, and leaves the panel behind it.
        ctrl(&mut app, KeyCode::Char('p'));
        assert!(!app.projects_open);
        assert!(!app.panel_focused);
        assert!(app.panel, "collapsing the catalog closed the whole panel");

        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.projects_open, "the same key opens it");
        assert!(app.panel_focused, "and hands it the keyboard");
    }

    #[test]
    fn shift_tab_opens_and_closes_the_side_panel() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert!(!app.panel);
        press(&mut app, KeyCode::BackTab);
        assert!(app.panel);
        press(&mut app, KeyCode::BackTab);
        assert!(!app.panel, "the same key closes it");
    }

    /// Closing the whole panel takes the keyboard with it. Otherwise the bare
    /// keys stay the catalog's with no catalog on screen to explain them —
    /// the same trap hiding the rail was fixed for.
    #[test]
    fn shift_tab_takes_the_keyboard_back_from_the_catalog() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.panel_focused);
        press(&mut app, KeyCode::BackTab);
        assert!(!app.panel);
        assert!(!app.panel_focused, "no panel, no panel keys");
        type_line(&mut app, "hello");
        assert_eq!(app.input, "hello", "the letters are the chat's again");
    }

    /// Cycling has to reach every mode and come back, or Tab would be a way to
    /// leave a mode and not return to it.
    #[test]
    fn cycling_with_tab_reaches_every_mode() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.mode = PermissionPolicy::Plan;
        let mut seen = vec![app.mode];
        for _ in 0..PermissionPolicy::ALL.len() {
            press(&mut app, KeyCode::Tab);
            seen.push(app.mode);
        }
        for mode in PermissionPolicy::ALL {
            assert!(seen.contains(&mode), "{mode:?} is unreachable by Tab");
        }
        assert_eq!(app.mode, PermissionPolicy::Plan, "and it wraps");
    }

    #[test]
    fn slash_mode_names_a_mode_or_cycles_without_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Mode(Some(PermissionPolicy::Plan)));
        assert_eq!(app.mode, PermissionPolicy::Plan);

        apply_slash(&mut app, command::Slash::Mode(None));
        assert_eq!(app.mode, PermissionPolicy::Ask, "no argument cycles");
    }

    /// The launch flag is a ceiling, not a starting point that can be argued
    /// with. `jod tui --permission plan` is somebody saying "not on this
    /// machine"; a Tab press must not be able to talk them out of it.
    #[test]
    fn the_launch_mode_is_a_ceiling_the_tui_cannot_exceed() {
        assert_eq!(
            bounded(PermissionPolicy::Plan, PermissionPolicy::Bypass),
            PermissionPolicy::Plan,
            "the TUI escalated past its own ceiling"
        );
        // Downwards needs no permission at all.
        assert_eq!(
            bounded(PermissionPolicy::Bypass, PermissionPolicy::Plan),
            PermissionPolicy::Plan
        );
        for mode in PermissionPolicy::ALL {
            assert_eq!(bounded(mode, mode), mode);
        }
    }

    /// What occupies the window is everything the model was shown, cache reads
    /// included — and it is *this turn's* total, not a running sum. Adding them
    /// up counts the same history once per turn and reports a full window after
    /// about four turns.
    #[test]
    fn context_is_the_last_turns_total_rather_than_a_running_sum() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let turn = |input: u64, cached: u64| AgentEvent::Finished {
            text: None,
            exit_code: Some(0),
            is_error: false,
            usage: jod_core::Usage {
                input_tokens: Some(input),
                cache_read_tokens: Some(cached),
                ..Default::default()
            },
        };
        app.apply(&turn(1_000, 9_000));
        assert_eq!(app.context_tokens, 10_000);
        app.apply(&turn(1_200, 14_000));
        assert_eq!(app.context_tokens, 15_200, "replaced, not accumulated");
    }

    #[test]
    fn compaction_is_recommended_before_the_window_is_full() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.context_tokens = 0;
        assert!(!app.should_compact());
        app.context_tokens = (app::CONTEXT_WINDOW as f64 * app::COMPACT_AT) as u64 + 1;
        assert!(app.should_compact());
        assert!(
            app.context_fraction() < 1.0,
            "the advice must arrive while there is still room to write a summary"
        );
    }

    /// Enter on a command that still needs an argument must say so, not
    /// silently append a space and leave the text sitting in the box.
    #[test]
    fn enter_on_an_argumentless_command_reports_the_usage() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/resume");
        assert!(press(&mut app, KeyCode::Enter).is_none());

        assert_eq!(app.input, "", "the line must be consumed");
        let last = last_notice(&app);
        assert!(
            last.contains("usage"),
            "expected a usage notice, got {last}"
        );
    }

    /// The next command typed must not inherit the previous one's text.
    #[test]
    fn a_command_needing_an_argument_does_not_corrupt_the_next_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/resume");
        press(&mut app, KeyCode::Enter);
        type_line(&mut app, "/model haiku");
        press(&mut app, KeyCode::Enter);

        // Not `Resume("/model haiku")`, which is what the leftover text caused.
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert_eq!(app.session, None);
    }

    /// Enter still completes when completing would actually change the line.
    #[test]
    fn enter_completes_a_half_typed_command() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "/thi");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.input, "/thinking");
        // A second Enter runs it — asserted as a flip, not a value, so the
        // test says "the command fired" rather than restating the default.
        let before = app.show_thinking;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.show_thinking, !before);
    }

    fn agent_line(id: &str, session: Option<&str>) -> AgentLine {
        AgentLine {
            delivery: delivery::Verdict::Nothing,
            id: id.into(),
            name: "work".into(),
            harness: "Claude Code".into(),
            status: "completed".into(),
            session: session.map(str::to_string),
            created_at_ms: 0,
            cost_usd: None,
            cwd: "/srv/reljod/repo".into(),
            last: None,
        }
    }

    /// The panel shows a shortened *agent* id and tells you to `/resume` it,
    /// but the harness needs its own conversation id. The prefix must resolve.
    #[test]
    fn resume_accepts_the_shortened_id_the_panel_shows() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12-3456-7890", Some("sess-xyz"))];

        apply_slash(&mut app, command::Slash::Resume("abcdef12".into()));

        assert_eq!(app.resume, Resume::Session("sess-xyz".into()));
        assert_eq!(app.session.as_deref(), Some("sess-xyz"));
    }

    #[test]
    fn resume_still_takes_a_session_id_typed_in_full() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12-3456", Some("sess-xyz"))];
        apply_slash(&mut app, command::Slash::Resume("sess-xyz".into()));
        assert_eq!(app.resume, Resume::Session("sess-xyz".into()));
    }

    /// An id from another machine or a log is still legitimate to type.
    #[test]
    fn an_unrecognised_id_is_passed_through_untouched() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Resume("from-elsewhere".into()));
        assert_eq!(app.resume, Resume::Session("from-elsewhere".into()));
    }

    #[test]
    fn an_ambiguous_prefix_changes_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![
            agent_line("ab111111", Some("s1")),
            agent_line("ab222222", Some("s2")),
        ];
        apply_slash(&mut app, command::Slash::Resume("ab".into()));

        assert_eq!(app.resume, Resume::Fresh, "must not guess");
        assert!(last_notice(&app).contains("matches 2"));
    }

    /// Resuming an agent that never reported a conversation would quietly start
    /// a fresh one — the amnesia case.
    #[test]
    fn an_agent_with_no_conversation_is_refused() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("abcdef12", None)];
        apply_slash(&mut app, command::Slash::Resume("abcdef12".into()));

        assert_eq!(app.resume, Resume::Fresh);
        assert!(last_notice(&app).contains("has not reported a conversation"));
    }

    #[test]
    fn an_explicit_model_still_reaches_the_harness() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Model(Some("haiku".into())));
        assert_eq!(app.model.as_deref(), Some("haiku"));
        assert!(app.status().contains("haiku"));
    }

    // ---- running several agents at once ----

    fn ctrl(app: &mut App, code: KeyCode) -> Option<Action> {
        on_key(
            app,
            &mut Thread::default(),
            KeyEvent::new(code, KeyModifiers::CONTROL),
            20,
        )
    }

    fn alt(app: &mut App, code: KeyCode) -> Option<Action> {
        on_key(
            app,
            &mut Thread::default(),
            KeyEvent::new(code, KeyModifiers::ALT),
            20,
        )
    }

    fn running(id: &str, name: &str) -> AgentLine {
        AgentLine {
            delivery: delivery::Verdict::Nothing,
            id: id.into(),
            name: name.into(),
            harness: "Claude Code".into(),
            status: "running".into(),
            session: Some(format!("sess-{id}")),
            created_at_ms: 0,
            cost_usd: None,
            cwd: "/srv/reljod/repo".into(),
            last: None,
        }
    }

    /// The old behaviour left the line sitting in a blocked box with a
    /// scolding, so the only thing to do while an agent worked was to sit still.
    #[test]
    fn a_prompt_typed_mid_turn_is_queued_rather_than_refused() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.busy = true;
        type_line(&mut app, "and then deploy it");

        assert_eq!(press(&mut app, KeyCode::Enter), None, "not sent yet");
        assert_eq!(app.queued, vec!["and then deploy it".to_string()]);
        assert_eq!(app.input, "", "and the box is clear for the next thought");
    }

    #[test]
    fn a_queued_prompt_is_still_recallable_with_the_arrows() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.busy = true;
        type_line(&mut app, "queued thought");
        press(&mut app, KeyCode::Enter);
        app.history_prev();
        assert_eq!(app.input, "queued thought");
    }

    /// `←` backs out of a run on the one press. The confirmation this replaces
    /// asked whether to leave a run that was already detached, which is a
    /// question with no answer worth giving.
    #[test]
    fn left_on_an_empty_line_backgrounds_the_run_without_asking() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc12345".into());
        press(&mut app, KeyCode::Left);
        assert_eq!(app.overlay, Overlay::None, "nothing to answer");
        assert_eq!(app.watching, None, "the view detached on the one press");
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    /// The rule that keeps the shortcut from costing anything: with text in the
    /// box, `←` is the cursor. A version of this that grabbed the key
    /// unconditionally would make the input box unusable and pass every test
    /// that only ever pressed it on an empty line.
    #[test]
    fn left_with_something_typed_still_moves_the_cursor() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc12345".into());
        type_line(&mut app, "hello");
        press(&mut app, KeyCode::Left);
        assert_eq!(app.overlay, Overlay::None, "must not confirm anything");
        assert_eq!(app.cursor, 4, "the cursor moved back over the `o`");
    }

    /// Nothing to leave means nothing to ask about.
    #[test]
    fn left_does_nothing_special_when_no_run_is_in_flight() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = None;
        app.busy = false;
        press(&mut app, KeyCode::Left);
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Backgrounding detaches the view and lands on the agents list. The run
    /// itself is a detached process group and is deliberately left alone — and
    /// the notice is what tells you so, now that no question does.
    #[test]
    fn backgrounding_stops_watching_and_says_the_run_survives() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc12345".into());
        assert_eq!(press(&mut app, KeyCode::Left), None);
        assert_eq!(app.watching, None, "the view detached");
        assert_eq!(app.workspace, Workspace::Fleet);
        assert_eq!(app.overlay, Overlay::None);
        assert!(
            spoken(&app).iter().any(|said| said.contains("keeps running")),
            "the run's survival is said, not asked about: {:?}",
            spoken(&app)
        );
    }

    /// The promise the notice makes, kept — and what the confirmation this
    /// replaces was really protecting. Backgrounding stops *this window*
    /// looking; the run is a detached process group and is still on the fleet,
    /// under the two keys the notice names.
    #[test]
    fn a_backgrounded_run_is_still_there_to_reopen() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("abc12345", "the one on screen")];
        app.watching = Some("abc12345".into());
        press(&mut app, KeyCode::Left);
        assert_eq!(app.workspace, Workspace::Fleet);

        app.list_mut(Workspace::Fleet).selected = Some("abc12345".into());
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Watch("abc12345".into())),
            "⏎ on the row did not reopen what ← left"
        );
    }

    #[test]
    fn a_prompt_typed_while_idle_is_sent_straight_away() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "go");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Send("go".into()))
        );
        assert!(app.queued.is_empty());
    }

    /// The key that makes several jobs at once possible without leaving the UI.
    #[test]
    fn ctrl_b_delegates_the_typed_line_to_a_background_agent() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "audit the dependencies");
        assert_eq!(
            ctrl(&mut app, KeyCode::Char('b')),
            Some(Action::Delegate("audit the dependencies".into()))
        );
        assert_eq!(app.input, "");
    }

    #[test]
    fn delegating_an_empty_line_does_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(ctrl(&mut app, KeyCode::Char('b')), None);
    }

    /// Ctrl-C is quit, so interrupting a run needs a key of its own — otherwise
    /// the only way to stop a runaway agent is to leave the UI entirely.
    #[test]
    fn ctrl_x_stops_the_run_being_watched() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc123".into());
        app.busy = true;
        assert_eq!(
            ctrl(&mut app, KeyCode::Char('x')),
            Some(Action::Stop("abc123".into()))
        );
    }

    #[test]
    fn ctrl_x_with_nothing_running_says_so_rather_than_killing_something() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("abc123".into());
        assert_eq!(ctrl(&mut app, KeyCode::Char('x')), None);
        assert!(last_notice(&app).contains("nothing running"));
    }

    /// Walking out on four background jobs without being told is the same
    /// mistake, four times over.
    #[test]
    fn quitting_warns_about_every_running_agent_not_just_the_watched_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("a", "one"), running("b", "two")];
        assert!(!app.busy, "none of them is on screen");

        ctrl(&mut app, KeyCode::Char('c'));
        assert!(!app.should_quit, "the first press must not leave");
        assert!(last_notice(&app).contains("2 agents"));

        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit, "the second press goes anyway");
    }

    #[test]
    fn quitting_with_nothing_running_needs_no_confirmation() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit);
    }

    // ---- the fleet as a control surface ----

    fn panel_with_agents() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("aaa11111", "port the parser"), {
            let mut done = running("bbb22222", "write the docs");
            done.status = "completed".into();
            done
        }];
        app.go(Workspace::Fleet);
        app
    }

    /// The id the fleet cursor is on, which is what every key acts on.
    fn fleet_at(app: &App) -> String {
        app.selected_agent()
            .map(|a| a.id.clone())
            .unwrap_or_default()
    }

    /// Put the fleet cursor on the pinned chat, the way `k` from the top agent
    /// would.
    fn select_main(app: &mut App) {
        app.list_mut(Workspace::Fleet).selected = Some(app::MAIN_ROW.to_string());
        assert!(app.main_selected());
    }

    /// The point of the pinned row: one keystroke from the fleet into the
    /// conversation, without having to remember a command.
    ///
    /// `EnterMain` and not `Watch`. Watching a run puts its output on screen
    /// and leaves the chat box wherever it was; this *goes into* the chat, so
    /// what you type next is an instruction to it.
    #[test]
    fn enter_on_the_pinned_row_goes_into_the_chat() {
        for key in [KeyCode::Enter, KeyCode::Right] {
            let mut app = panel_with_agents();
            select_main(&mut app);
            assert_eq!(press(&mut app, key), Some(Action::EnterMain), "{key:?}");
        }
    }

    /// `/main` with an instruction sends one; `/main` alone goes there. It used
    /// to refuse the second form as a missing argument, which left the pinned
    /// chat with no keyboard route into it at all.
    #[test]
    fn bare_main_enters_the_chat_and_main_with_words_sends_to_it() {
        assert_eq!(command::parse("/main"), Some(command::Slash::EnterMain));
        assert_eq!(
            command::parse("/main count the rust files"),
            Some(command::Slash::Main("count the rust files".into()))
        );

        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::EnterMain),
            Some(Action::EnterMain)
        );
    }

    /// A row whose keys silently do nothing is a row that teaches people the
    /// footer is decorative. None of stop, attach or resume means anything to a
    /// conversation, so each says why instead.
    #[test]
    fn the_run_verbs_explain_themselves_on_the_pinned_row() {
        for key in ['s', 'a', 'r'] {
            let mut app = panel_with_agents();
            select_main(&mut app);
            assert_eq!(press(&mut app, KeyCode::Char(key)), None, "`{key}`");
            assert!(
                spoken(&app).iter().any(|n| n.contains("not an agent")),
                "`{key}` should say why: {:?}",
                spoken(&app)
            );
        }
    }

    /// The list's own verbs still work with the cursor on the pinned row —
    /// `c` especially, which is the way out of a fleet holding nothing else.
    #[test]
    fn the_pinned_row_does_not_disable_the_screen_it_sits_on() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Fleet);
        assert!(app.main_selected(), "an empty fleet selects the chat");
        assert_eq!(press(&mut app, KeyCode::Char('c')), None);
        assert!(
            matches!(app.overlay, Overlay::Sessions(_)),
            "`c` opens the session list from the pinned row too"
        );
    }

    /// Opening the fleet means managing the work, so the cursor starts on the
    /// work. The chat is drawn above it and reached with one `k`.
    #[test]
    fn the_fleet_cursor_starts_on_the_first_agent_not_on_the_chat() {
        let app = panel_with_agents();
        assert!(!app.main_selected());
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    #[test]
    fn the_panel_arrows_move_its_cursor_rather_than_the_transcript() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(fleet_at(&app), "bbb22222");
        assert_eq!(app.scroll, 0, "the transcript did not move");
        press(&mut app, KeyCode::Up);
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    /// `j` and `k` do the same, on every workspace, so vim fingers work without
    /// vim modes.
    #[test]
    fn jk_move_the_cursor_wherever_the_arrows_do() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(fleet_at(&app), "bbb22222");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    #[test]
    fn enter_on_the_panel_puts_that_agent_on_screen_and_closes_the_panel() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Watch("bbb22222".into()))
        );
        assert_eq!(
            app.workspace,
            Workspace::Chat,
            "you asked to read it, so show it"
        );
    }

    #[test]
    fn s_stops_the_selected_agent() {
        let mut app = panel_with_agents();
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::Stop("aaa11111".into()))
        );
    }

    /// Killing a finished run only reclaims its tmux session, which is not what
    /// the key looks like it does.
    #[test]
    fn s_on_a_finished_agent_explains_itself_instead() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        assert!(last_notice(&app).contains("nothing to stop"));
    }

    #[test]
    fn a_asks_how_to_attach_to_the_selected_agent() {
        let mut app = panel_with_agents();
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            Some(Action::Attach("aaa11111".into()))
        );
    }

    /// How an unattended run gets picked up and corrected: point the next turn
    /// at its conversation without leaving the UI.
    #[test]
    fn r_points_the_next_turn_at_the_selected_agents_conversation() {
        let mut app = panel_with_agents();
        // And drops the binding, so pressing it from inside the main chat sends
        // the next turn to this agent rather than to the orchestrator.
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            Some(Action::NewThread)
        );
        assert_eq!(app.resume, Resume::Session("sess-aaa11111".into()));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    #[test]
    fn r_on_an_agent_with_no_conversation_refuses_rather_than_starting_a_fresh_one() {
        let mut app = panel_with_agents();
        app.agents[0].session = None;
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.resume, Resume::Fresh);
        assert!(last_notice(&app).contains("never reported"));
    }

    /// A run that finished is the ordinary target of `r`, and the half of this
    /// gate that matters more: a screen that refuses the case the key exists
    /// for is worse than one that never checked.
    ///
    /// Pressed on the second row, which `panel_with_agents` leaves `completed`.
    #[test]
    fn r_still_continues_a_run_that_finished_cleanly() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            Some(Action::NewThread)
        );
        assert_eq!(app.resume, Resume::Session("sess-bbb22222".into()));
    }

    /// The asymmetry this fixes: `s` four lines above looks at how the run
    /// ended, and `r` did not. Someone stops a run that is going wrong, the
    /// cursor is still on it, and `r` is the obvious next key — which pointed
    /// the next turn at the session that stop had just cut in half.
    #[test]
    fn r_on_a_killed_agent_refuses_rather_than_resuming_a_cut_off_session() {
        let mut app = panel_with_agents();
        app.agents[0].status = "killed".into();

        assert_eq!(press(&mut app, KeyCode::Char('r')), None);
        assert_eq!(app.resume, Resume::Fresh, "the dead session was bound");
        let said = last_notice(&app);
        assert!(said.contains("killed"), "the refusal is silent about why: {said}");
        assert!(
            said.contains("press d"),
            "the refusal does not say what to press instead: {said}"
        );
        assert_eq!(
            app.workspace,
            Workspace::Fleet,
            "refused, then left on a screen where `d` does not exist"
        );
    }

    /// The commoner half of the same fault. `rehydrate` marks any run `failed`
    /// whose process group has gone, so most dead sessions reach this screen as
    /// `failed` rather than as `killed`.
    #[test]
    fn r_on_a_failed_agent_refuses_rather_than_resuming_a_cut_off_session() {
        let mut app = panel_with_agents();
        app.agents[0].status = "failed".into();

        assert_eq!(press(&mut app, KeyCode::Char('r')), None);
        assert_eq!(app.resume, Resume::Fresh, "the dead session was bound");
        assert!(last_notice(&app).contains("failed"));
    }

    /// The decision on its own, over all four statuses at once, so that the two
    /// that go through are asserted as deliberately as the two that do not.
    #[test]
    fn only_a_killed_or_failed_run_is_turned_away_by_the_fleet_status_gate() {
        for dead in ["killed", "failed"] {
            let refusal = refusal_to_continue("port the parser", dead)
                .unwrap_or_else(|| panic!("`{dead}` was let through"));
            assert!(
                refusal.contains(dead),
                "a refusal that does not name the status: {refusal}"
            );
            assert!(
                refusal.contains("press d"),
                "a refusal that does not say what to do instead: {refusal}"
            );
        }
        for alive in ["running", "completed"] {
            assert_eq!(
                refusal_to_continue("port the parser", alive),
                None,
                "`{alive}` is a run a follow-up should reach"
            );
        }
    }

    /// The panel is modal, so its letters are commands. Typing must not leak
    /// into the input box behind it.
    #[test]
    fn letters_typed_at_an_open_panel_do_not_reach_the_input_box() {
        let mut app = panel_with_agents();
        type_line(&mut app, "hello");
        assert_eq!(app.input, "");
    }

    #[test]
    fn esc_closes_the_panel() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    #[test]
    fn the_team_panel_marks_the_selected_task_done() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Team);
        app.tasks = vec![
            jod_core::team::TeamTask {
                id: "t1".into(),
                title: "port the parser".into(),
                owner: None,
                status: "open".into(),
            },
            jod_core::team::TeamTask {
                id: "t2".into(),
                title: "write the docs".into(),
                owner: None,
                status: "done".into(),
            },
        ];
        // The cursor is an id, so it has to be placed once the rows exist —
        // which is what the refresh does in the loop.
        app.reconcile();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::FinishTask("t1".into()))
        );

        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), None, "already done");
    }

    // ---- naming an agent without retyping a uuid ----

    #[test]
    fn an_id_prefix_is_enough_to_name_an_agent() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("abcdef12-3456", "work")];
        assert_eq!(
            apply_slash(&mut app, command::Slash::Stop("abcdef".into())),
            Some(Action::Stop("abcdef12-3456".into()))
        );
    }

    /// Stopping the wrong agent is not an undoable mistake, so an ambiguous
    /// prefix must refuse rather than guess.
    #[test]
    fn an_ambiguous_prefix_stops_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("ab111111", "one"), running("ab222222", "two")];
        assert_eq!(
            apply_slash(&mut app, command::Slash::Stop("ab".into())),
            None
        );
        assert!(last_notice(&app).contains("matches 2"));
    }

    #[test]
    fn naming_an_agent_that_does_not_exist_says_so() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Watch("zz".into())),
            None
        );
        assert!(last_notice(&app).contains("no agent"));
    }

    #[test]
    fn delegate_reaches_the_loop_as_a_background_spawn() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Delegate("do it".into())),
            Some(Action::Delegate("do it".into()))
        );
    }

    /// The TUI is the interface that matters, so the orchestrator has to be
    /// reachable from it — a headline feature only the CLI can reach is a
    /// feature most of its use will never see.
    #[test]
    fn the_orchestrator_reaches_the_loop_as_its_own_action() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Main("sweep the PRs daily".into())),
            Some(Action::Orchestrate("sweep the PRs daily".into()))
        );
    }

    // ---- board ids ----

    fn board(ids: &[&str]) -> Vec<jod_core::team::TeamTask> {
        ids.iter()
            .map(|id| jod_core::team::TeamTask {
                id: (*id).into(),
                title: "x".into(),
                owner: None,
                status: "open".into(),
            })
            .collect()
    }

    /// The id has to mean something: a teammate claims it by name from the
    /// command line, where `task-7` tells you nothing.
    #[test]
    fn a_board_id_is_derived_from_the_title() {
        assert_eq!(
            task_id("Port the parser to Rust", &[]),
            "port-the-parser-to"
        );
        assert_eq!(task_id("Ship it!", &[]), "ship-it");
    }

    /// Two identical ids on one board means `jod team claim` picks the wrong
    /// row, and nothing about that failure is visible.
    #[test]
    fn a_colliding_board_id_is_given_a_suffix() {
        let existing = board(&["ship-it"]);
        assert_eq!(task_id("Ship it", &existing), "ship-it-2");
        let existing = board(&["ship-it", "ship-it-2"]);
        assert_eq!(task_id("Ship it", &existing), "ship-it-3");
    }

    #[test]
    fn a_title_with_no_usable_characters_still_gets_an_id() {
        assert_eq!(task_id("!!! ???", &[]), "task");
    }

    // ---- scrolling and recall live on different keys ----

    /// The arrows recall what was sent, as every shell does. Scrolling the
    /// transcript keeps PageUp/PageDown and a Ctrl form.
    #[test]
    fn the_arrows_recall_history_and_ctrl_arrows_scroll() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "first prompt");
        press(&mut app, KeyCode::Enter);
        for i in 0..30 {
            app.push(Entry::Agent(format!("line {i}")));
        }

        press(&mut app, KeyCode::Up);
        assert_eq!(app.input, "first prompt");
        assert_eq!(app.scroll, 0, "the transcript stayed put");

        ctrl(&mut app, KeyCode::Up);
        assert_eq!(app.scroll, 1, "Ctrl with an arrow still scrolls");
    }

    /// Typing over a recalled line makes it the user's own draft; ↓ must not
    /// then replace what they are editing.
    #[test]
    fn editing_a_recalled_line_leaves_the_history() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "original");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(app.input, "original!");

        press(&mut app, KeyCode::Down);
        assert_eq!(app.input, "original!", "the edit survived");
    }

    // ---- the which-key menu ----

    /// The discoverability spine. One free chord, a menu of every screen, and
    /// recognition instead of recall.
    #[test]
    fn ctrl_g_opens_the_which_key_menu() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        assert_eq!(app.overlay, Overlay::WhichKey);
    }

    #[test]
    fn a_which_key_letter_reaches_its_workspace() {
        for (letter, expected) in [
            ('f', Workspace::Fleet),
            ('m', Workspace::Memory),
            ('s', Workspace::Schedules),
            ('g', Workspace::Goals),
            ('h', Workspace::Hooks),
            ('t', Workspace::Tasks),
            ('a', Workspace::Activity),
            ('w', Workspace::Team),
            ('c', Workspace::Chat),
        ] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            ctrl(&mut app, KeyCode::Char('g'));
            press(&mut app, KeyCode::Char(letter));
            assert_eq!(app.workspace, expected, "Ctrl-K {letter}");
            assert_eq!(app.overlay, Overlay::None, "and the menu closed");
        }
    }

    /// Any key the menu does not know cancels silently rather than doing
    /// something surprising.
    #[test]
    fn an_unknown_which_key_letter_cancels_without_going_anywhere() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.workspace, Workspace::Chat);
        assert_eq!(app.input, "", "and it certainly is not typed into the box");
    }

    #[test]
    fn esc_cancels_the_which_key_menu() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// `Ctrl-K n s` is the two-key route into making a schedule.
    #[test]
    fn the_new_submenu_lands_on_the_screen_and_opens_its_prompt() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.overlay, Overlay::WhichKeyNew);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.workspace, Workspace::Schedules);
        assert!(
            matches!(app.overlay, Overlay::Prompt { .. }),
            "{:?}",
            app.overlay
        );
    }

    #[test]
    fn ctrl_g_question_mark_opens_the_keymap() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
    }

    /// Clearing the transcript kept its letter and lost its chord: `Ctrl-L` is
    /// a tmux pane here, and `l` is still what clears a screen everywhere else.
    #[test]
    fn the_menus_l_empties_the_transcript() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.push(Entry::Notice("something to clear".into()));
        assert!(!app.transcript.is_empty());

        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        assert!(app.transcript.is_empty(), "{:?}", app.transcript);
        assert_eq!(app.overlay, Overlay::None, "and the menu closed");
    }

    /// Six of the menu's letters are verbs rather than screens, and they only
    /// stay reachable while no workspace claims the letter — `from_letter` is
    /// checked *after* them in `on_which_key`, so a new screen taking `u` would
    /// not collide loudly, it would silently shadow the verb.
    ///
    /// The real cost is asymmetric, which is why this is a test and not a
    /// comment: the screen would still be reachable by its digit and by `/`,
    /// while the verb would have nowhere left to go at all.
    #[test]
    fn a_which_key_verb_does_not_shadow_a_screen() {
        for verb in ['n', 'e', 'j', 'r', 'u', 'l', 'd'] {
            assert!(
                Workspace::from_letter(verb).is_none(),
                "the menu spends `{verb}` on a verb, but a workspace now claims it — one of \
                 the two is unreachable and the workspace still has its digit"
            );
        }
    }

    /// `Ctrl-F` is the one screen with a chord of its own, and pressing it
    /// again comes home. Every other screen is one letter past the leader —
    /// the team included, which used to have `Alt-G` and gave the letter up
    /// when `g` became the leader.
    #[test]
    fn the_fleet_toggles_on_its_chord_and_every_other_screen_is_behind_the_leader() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('f'));
        assert_eq!(app.workspace, Workspace::Fleet);
        ctrl(&mut app, KeyCode::Char('f'));
        assert_eq!(app.workspace, Workspace::Chat);

        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.workspace, Workspace::Team);
    }

    /// Alt is unpressable on a stock macOS terminal, which is why nothing
    /// prints it — but the terminals that *are* configured to send it, and the
    /// fingers that learned the release it was the only spelling in, both still
    /// land. The alias is on the letter the verb has now, not the one it had.
    #[test]
    fn the_alt_spelling_of_a_verb_still_fires_unadvertised() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        alt(&mut app, KeyCode::Char('g'));
        assert_eq!(app.overlay, Overlay::WhichKey, "Alt-G is the leader too");

        let mut app = app_on(HarnessKind::ClaudeCode);
        alt(&mut app, KeyCode::Char('f'));
        assert_eq!(app.workspace, Workspace::Fleet, "and Alt-F is the fleet");
    }

    /// `Ctrl-A` is readline's, and is also this tmux's prefix. Either reason
    /// alone would be enough to keep a verb off it; it once opened the fleet.
    #[test]
    fn ctrl_a_is_the_start_of_the_line_again_and_not_the_fleet() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "hello");
        assert_eq!(app.cursor, 5);

        ctrl(&mut app, KeyCode::Char('a'));
        assert_eq!(app.workspace, Workspace::Chat, "it must not open the fleet");
        assert_eq!(app.cursor, 0, "it is readline's start-of-line again");

        ctrl(&mut app, KeyCode::Char('e'));
        assert_eq!(app.cursor, 5, "and Ctrl-E is still the end of it");
    }

    /// Alt-Z is nobody's binding here. Falling through to the chat handler
    /// would have typed a `z` into a line the user was about to send, which is
    /// worse than the key doing nothing.
    ///
    /// Was `Alt-D` until the project catalog claimed it, and `d` is in fact
    /// free again now that the catalog sits behind the leader — but the
    /// assertion is about an *unclaimed* chord, and `z` is the one letter no
    /// keymap here can ever want: the terminal owns `Ctrl-Z` outright.
    #[test]
    fn an_alt_chord_nothing_claims_does_not_become_typed_text() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "ship it");
        alt(&mut app, KeyCode::Char('z'));
        assert_eq!(app.input, "ship it");
    }

    // ---- the printed keymap and the dispatch cannot disagree ----
    //
    // `keys.rs` is a display table and `on_chord` is a hand-written `match`.
    // Nothing but attention has ever held them together, and the failure is
    // silent: the keybar keeps promising a chord that quietly stopped working,
    // and the person at the terminal concludes the key is broken rather than
    // the docs. These two tests make that a build failure in both directions.

    /// Did this press reach a handler at all?
    ///
    /// `on_chord` answers for everything except quitting, which `on_key` takes
    /// ahead of every layer so that a key which cannot always leave is never a
    /// trap — so quitting is spelled out here rather than left looking
    /// unhandled.
    fn dispatches(app: &mut App, code: KeyCode, modifier: KeyModifiers) -> bool {
        if modifier == KeyModifiers::CONTROL
            && matches!(code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return true;
        }
        on_chord(app, KeyEvent::new(code, modifier)).is_some()
    }

    /// Every press this crate could plausibly bind, so the reverse direction
    /// has something to sweep.
    fn every_candidate_press() -> Vec<(KeyCode, KeyModifiers)> {
        let mut codes: Vec<KeyCode> = ('a'..='z').map(KeyCode::Char).collect();
        codes.extend(('0'..='9').map(KeyCode::Char));
        codes.extend([
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Tab,
            KeyCode::BackTab,
        ]);
        codes
            .into_iter()
            .flat_map(|code| [(code, KeyModifiers::CONTROL), (code, KeyModifiers::ALT)])
            .collect()
    }

    /// Forwards: a chord on the keybar or in the `?` overlay that nothing
    /// dispatches is a lie printed on screen at all times.
    #[test]
    fn every_chord_the_screens_advertise_is_one_the_dispatch_answers() {
        let advertised = keys::all_documented_chords();
        assert!(!advertised.is_empty(), "the scan found nothing to check");
        for label in advertised {
            let presses = keys::press_of(&label);
            assert!(
                !presses.is_empty(),
                "{label} is printed but does not parse as a press"
            );
            for (code, modifier) in presses {
                let mut app = app_on(HarnessKind::ClaudeCode);
                assert!(
                    dispatches(&mut app, code, modifier),
                    "{label} is printed on screen but {modifier:?} {code:?} reaches no handler"
                );
            }
        }
    }

    /// Backwards: a chord that works but no screen ever names is a feature
    /// only its author can find.
    ///
    /// The deliberate exception is the *spelling*, not the binding — `Alt-T`
    /// still toggles reasoning while only `Ctrl-T` is printed, because
    /// advertising the Alt form would advertise a chord a stock macOS terminal
    /// cannot send. So the requirement is that the same key is named in one
    /// spelling or the other, which still fails the moment a binding exists in
    /// neither.
    ///
    /// It cannot see a verb that has no chord at all: `$EDITOR`, search, the
    /// jobs panel, the oldest unread and clearing the transcript are bare
    /// letters behind the leader now, and bare letters are not chords. What
    /// keeps *those* honest is `draw_which_key`, which prints every one of
    /// them, and `a_which_key_verb_does_not_shadow_a_screen`.
    #[test]
    fn every_chord_the_dispatch_answers_is_one_some_screen_names() {
        let named: std::collections::HashSet<(KeyCode, KeyModifiers)> =
            keys::all_documented_chords()
                .iter()
                .flat_map(|label| keys::press_of(label))
                .collect();
        for (code, modifier) in every_candidate_press() {
            let mut app = app_on(HarnessKind::ClaudeCode);
            if !dispatches(&mut app, code, modifier) {
                continue;
            }
            let other = if modifier == KeyModifiers::CONTROL {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            };
            assert!(
                named.contains(&(code, modifier)) || named.contains(&(code, other)),
                "{modifier:?} {code:?} is dispatched but no screen names it in either spelling"
            );
        }
    }

    /// `on_chord` is only ever reached through `on_key`, so a keymap that is
    /// perfect inside `on_chord` and unreachable from the router is the same
    /// bug wearing a disguise — and the router is where the Ctrl-only gate
    /// used to be.
    #[test]
    fn an_alt_chord_reaches_the_dispatch_through_the_router() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        alt(&mut app, KeyCode::Char('g'));
        assert_eq!(app.overlay, Overlay::WhichKey);
    }

    // ---- Esc goes back exactly one level ----

    fn with_memory() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.memory = vec![
            memory_node("prefers-spec-first"),
            memory_node("linear-is-truth"),
        ];
        app.go(Workspace::Memory);
        app
    }

    fn memory_node(name: &str) -> data::MemoryNode {
        data::MemoryNode {
            id: name.into(),
            name: name.into(),
            kind: data::MemoryKind::Belief,
            confidence: 0.9,
            degree: 3,
            age_ms: 0,
            seen: 1,
            body: "a belief".into(),
            contradicted: false,
            in_edges: vec![data::MemoryEdge {
                kind: "supports".into(),
                other: "linear-is-truth".into(),
                other_name: "linear-is-truth".into(),
                other_kind: data::MemoryKind::Belief,
                warn: false,
            }],
            out_edges: vec![],
            provenance: vec![],
        }
    }

    /// One back key, one meaning, and the bottom is always chat.
    #[test]
    fn esc_unwinds_exactly_one_level_and_ends_at_chat() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Memory, "one level, not two");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat);

        // And chat's own Esc is unchanged: follow the tail again.
        app.scroll_up(3, 10);
        press(&mut app, KeyCode::Esc);
        assert!(app.following());
    }

    /// An active filter is a level of its own: `Esc` clears it before it takes
    /// you anywhere, so you never lose the screen by trying to clear the box.
    #[test]
    fn esc_clears_an_active_filter_before_it_leaves_the_screen() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "spec");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.here().filter.as_deref(), Some("spec"));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.here().filter, None, "the filter went");
        assert_eq!(app.workspace, Workspace::Memory, "and the screen stayed");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// A top-level jump forgets the way back deliberately: `Esc` twice landing
    /// in a memory node you forgot opening is a maze, not a back button.
    #[test]
    fn jumping_to_another_workspace_resets_the_way_back() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);

        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.workspace, Workspace::Schedules);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Chat, "straight home");
    }

    #[test]
    fn q_is_a_synonym_for_esc_in_a_workspace() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    // ---- direct jumps ----

    #[test]
    fn a_digit_jumps_straight_to_its_workspace_from_another_one() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Fleet);
        for (digit, expected) in [
            ('1', Workspace::Chat),
            ('3', Workspace::Memory),
            ('4', Workspace::Schedules),
            ('8', Workspace::Activity),
        ] {
            app.go(Workspace::Fleet);
            press(&mut app, KeyCode::Char(digit));
            assert_eq!(app.workspace, expected, "digit {digit}");
        }
    }

    /// Digits stay literal text in chat. For digits to be navigation, digits
    /// would have to stop being text — which is a mode, and this is not one.
    #[test]
    fn digits_are_still_text_in_the_chat_box() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "run 4 agents");
        assert_eq!(app.input, "run 4 agents");
        assert_eq!(app.workspace, Workspace::Chat);
    }

    // ---- the `?` overlay ----

    /// Claude Code's rule exactly, including the edge case: backspacing down to
    /// a lone `?` must not fire it, because the key only acts on an *empty*
    /// input.
    #[test]
    fn question_mark_opens_the_keymap_only_on_an_empty_input() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
        press(&mut app, KeyCode::Esc);

        type_line(&mut app, "what?");
        assert_eq!(
            app.overlay,
            Overlay::None,
            "with text typed it is a character"
        );
        assert_eq!(app.input, "what?");

        // Backspacing down to a lone `?` leaves it as text, not as a command:
        // the rule is about the keypress, not about what is in the box after
        // it.
        app.clear_line();
        app.input = "?x".into();
        app.cursor = app.input.len();
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input, "?");
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn question_mark_opens_the_keymap_on_a_workspace_too() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Keymap);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.overlay, Overlay::None, "any key closes it");
    }

    // ---- filtering ----

    /// While the `/` line is being typed it owns the keyboard, letters and all
    /// — otherwise filtering for "stop" would stop something.
    #[test]
    fn a_filter_being_typed_swallows_the_letters_that_are_otherwise_commands() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "stop");
        assert_eq!(app.here().filter.as_deref(), Some("stop"));
        // `spoken` and not the transcript: this screen's answers are flashes, so
        // an empty transcript would prove nothing about whether `s` ran.
        assert!(
            spoken(&app).is_empty(),
            "nothing was stopped: {:?}",
            spoken(&app)
        );
    }

    /// The other half of the pair: `←` backs out to this list, `→` goes into
    /// the run under the cursor. It must reach the same action `⏎` does, or the
    /// two keys would drift apart the first time either one changed.
    #[test]
    fn right_enters_the_selected_run_exactly_as_enter_does() {
        let mut by_enter = panel_with_agents();
        let mut by_arrow = panel_with_agents();
        let expected = press(&mut by_enter, KeyCode::Enter);
        assert!(
            matches!(expected, Some(Action::Watch(_))),
            "the fixture should open a run, got {expected:?}"
        );
        assert_eq!(press(&mut by_arrow, KeyCode::Right), expected);
        assert_eq!(by_arrow.workspace, Workspace::Chat);
    }

    /// The filter narrows the agents. The pinned chat is not one of them and
    /// stays — "always on top" that a search term can remove is a row you have
    /// to remember how to get back, which is the thing it exists not to be.
    #[test]
    fn a_filter_narrows_the_agents_and_leaves_the_pinned_chat_alone() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "docs");
        assert_eq!(
            app.row_ids(Workspace::Fleet),
            vec![app::MAIN_ROW.to_string(), "bbb22222".to_string()]
        );
        // And the cursor lands on the match rather than on the chat.
        assert_eq!(fleet_at(&app), "bbb22222");
    }

    /// `⏎` keeps the filter and hands the letters back; only `Esc` clears it.
    #[test]
    fn accepting_a_filter_keeps_it_and_returns_the_letters_to_being_commands() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "port");
        press(&mut app, KeyCode::Enter);
        assert!(!app.here().editing_filter);
        assert_eq!(app.here().filter.as_deref(), Some("port"));

        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::Stop("aaa11111".into())),
            "letters are commands again"
        );
    }

    #[test]
    fn backspace_edits_the_filter_rather_than_leaving_the_screen() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "port");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.here().filter.as_deref(), Some("por"));
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    // ---- sorting ----

    #[test]
    fn capital_s_cycles_the_sort_and_says_which_one_is_in_force() {
        let mut app = panel_with_agents();
        assert_eq!(app.here().sort, 0);
        press(&mut app, KeyCode::Char('S'));
        assert_eq!(app.here().sort, 1);
        assert!(
            last_notice(&app).contains("sorted by"),
            "{:?}",
            app.flash
        );
    }

    /// Re-sorting must not move the cursor onto a different row.
    #[test]
    fn re_sorting_keeps_the_cursor_on_the_same_item() {
        let mut app = panel_with_agents();
        press(&mut app, KeyCode::Down);
        assert_eq!(fleet_at(&app), "bbb22222");
        press(&mut app, KeyCode::Char('S'));
        assert_eq!(fleet_at(&app), "bbb22222");
    }

    // ---- destructive verbs ----

    fn with_schedules() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.schedules = vec![data::ScheduleRow {
            name: "nightly-inbox".into(),
            gloss: "02:00 every day".into(),
            cron: "0 2 * * *".into(),
            timezone: "Asia/Manila".into(),
            next_ms: Some(1),
            last_ms: None,
            state: data::ScheduleState::Armed,
            history: vec![],
            prompt: "Triage the inbox.".into(),
            runs_as: "Claude Code".into(),
            policy: "overlap: skip".into(),
            recent: vec![],
        }];
        app.go(Workspace::Schedules);
        app
    }

    /// `x` deleting a webhook silently is one fat-fingered `Ctrl-K h x` away
    /// from losing a secret, so it asks first — and the question names the
    /// thing.
    #[test]
    fn x_asks_before_it_deletes_and_names_what_it_would_delete() {
        let mut app = with_schedules();
        assert_eq!(press(&mut app, KeyCode::Char('x')), None, "nothing yet");
        match &app.overlay {
            Overlay::Confirm { verb, what, .. } => {
                assert_eq!(verb, "delete");
                assert_eq!(what, "nightly-inbox");
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
    }

    #[test]
    fn anything_that_is_not_a_yes_cancels_the_deletion() {
        let mut app = with_schedules();
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.overlay, Overlay::None, "and it did not delete");

        press(&mut app, KeyCode::Char('x'));
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn y_confirms_the_deletion() {
        let mut app = with_schedules();
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::DeleteSchedule("nightly-inbox".into()))
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    /// A run is not edited and an activity line is not deleted, so the keys
    /// fall through rather than offering a verb that cannot exist.
    #[test]
    fn edit_and_delete_do_nothing_on_the_screens_that_have_no_such_verb() {
        let mut app = panel_with_agents();
        assert_eq!(press(&mut app, KeyCode::Char('e')), None);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(
            app.overlay,
            Overlay::None,
            "and nothing asked to delete a run"
        );

        let mut app = with_activity();
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Memory is forgotten, not deleted — the word on the screen is what it
    /// does to you, not what it does to a row.
    #[test]
    fn forgetting_a_memory_is_called_forgetting() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('x'));
        match &app.overlay {
            Overlay::Confirm { verb, .. } => assert_eq!(verb, "forget"),
            other => panic!("expected a confirmation, got {other:?}"),
        }
    }

    // ---- the local graph ----

    #[test]
    fn g_drills_from_the_memory_list_into_the_local_graph_of_the_selected_node() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.workspace, Workspace::MemoryGraph);
        assert_eq!(app.graph.focus, "prefers-spec-first");
    }

    /// `⏎` re-centres on the highlighted neighbour and pushes the old focus;
    /// `Backspace` pops it.
    #[test]
    fn enter_re_centres_the_graph_and_backspace_walks_back() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.graph.focus, "linear-is-truth");
        assert_eq!(app.graph.trail, vec!["prefers-spec-first".to_string()]);

        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.graph.focus, "prefers-spec-first");
        assert!(app.graph.trail.is_empty());
        assert_eq!(app.workspace, Workspace::MemoryGraph, "still in the graph");
    }

    /// Walking back past the beginning leaves the graph rather than sitting
    /// there doing nothing.
    #[test]
    fn backspace_on_an_empty_visit_stack_leaves_the_graph() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.workspace, Workspace::Memory);
    }

    /// Coming back out of the graph should land on the node you were last
    /// looking at, not the one you left from.
    #[test]
    fn re_centring_moves_the_list_cursor_to_follow_the_eye() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Memory);
        assert_eq!(
            app.list(Workspace::Memory).selected.as_deref(),
            Some("linear-is-truth")
        );
    }

    #[test]
    fn h_toggles_between_one_hop_and_two() {
        let mut app = with_memory();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.graph.hops, 1);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.graph.hops, 2);
    }

    // ---- activity ----

    fn with_activity() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.activity = vec![
            activity_item("newest", 100, true, None),
            activity_item(
                "oldest",
                1,
                true,
                Some((Workspace::Fleet, "aaa11111".into())),
            ),
        ];
        app.go(Workspace::Activity);
        app
    }

    fn activity_item(
        id: &str,
        at_ms: i64,
        unread: bool,
        jump_to: Option<(Workspace, String)>,
    ) -> data::ActivityItem {
        data::ActivityItem {
            id: id.into(),
            at_ms,
            source: data::Source::Cron,
            text: format!("{id} happened"),
            unread,
            needs_you: false,
            jump_to,
        }
    }

    #[test]
    fn m_marks_one_item_read_and_capital_m_marks_them_all() {
        let mut app = with_activity();
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.unread(), 1);
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(app.unread(), 0);
    }

    #[test]
    fn u_hides_what_has_already_been_read() {
        let mut app = with_activity();
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('u'));
        assert!(app.unread_only);
        assert_eq!(app.row_ids(Workspace::Activity), vec!["oldest".to_string()]);
    }

    /// `⏎` jumps to the thing the event is about rather than showing a copy of
    /// it, which is what makes the feed a control surface.
    #[test]
    fn enter_on_an_activity_item_jumps_to_the_object_it_is_about() {
        let mut app = with_activity();
        app.agents = vec![running("aaa11111", "port the parser")];
        app.reconcile();
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.workspace, Workspace::Fleet);
        assert_eq!(fleet_at(&app), "aaa11111");
    }

    /// `Ctrl-N` exists because an ending that arrived while you were away has
    /// to be reachable without hunting for it.
    #[test]
    fn the_menus_u_lands_on_the_oldest_thing_you_have_not_read() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.activity = vec![
            activity_item("newest", 100, true, None),
            activity_item("oldest", 1, true, None),
        ];
        app.reconcile();
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.workspace, Workspace::Activity);
        assert_eq!(
            app.list(Workspace::Activity).selected.as_deref(),
            Some("oldest")
        );
    }

    #[test]
    fn the_menus_u_with_nothing_unread_says_so_rather_than_pretending() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('u'));
        assert!(last_notice(&app).contains("nothing unread"));
    }

    // ---- tasks ----

    fn with_board() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.tasks = vec![jod_core::team::TeamTask {
            id: "port-the-parser".into(),
            title: "Port the parser to the new AST".into(),
            owner: None,
            status: "open".into(),
        }];
        app.go(Workspace::Tasks);
        app
    }

    /// The verb that makes the board worth a screen: a task becomes a run,
    /// seeded with what it is and how you would know it was done.
    #[test]
    fn d_turns_the_selected_task_into_an_agent_run() {
        let mut app = with_board();
        match press(&mut app, KeyCode::Char('d')) {
            Some(Action::Delegate(prompt)) => {
                assert!(
                    prompt.contains("Port the parser to the new AST"),
                    "{prompt}"
                );
            }
            other => panic!("expected a delegation, got {other:?}"),
        }
    }

    #[test]
    fn a_delegated_task_carries_its_runnable_check_into_the_prompt() {
        let task = data::TaskRow {
            id: "migrate-store".into(),
            title: "Move the run transport into SQLite".into(),
            owner: None,
            state: data::TaskState::Open,
            run: None,
            age_ms: 0,
            what: "…".into(),
            check: "cargo test -p jod-core".into(),
            blocked_by: vec![],
            blocks: vec![],
            spec: Some("SPEC.md".into()),
            history: vec![],
        };
        let prompt = delegation_prompt(&task);
        assert!(prompt.contains("cargo test -p jod-core"), "{prompt}");
        assert!(prompt.contains("SPEC.md"), "{prompt}");
    }

    #[test]
    fn enter_on_the_tasks_screen_marks_it_done_as_the_board_always_has() {
        let mut app = with_board();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::FinishTask("port-the-parser".into()))
        );
    }

    #[test]
    fn n_on_the_tasks_screen_asks_for_a_title_and_puts_it_on_the_board() {
        let mut app = with_board();
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.overlay, Overlay::Prompt { .. }));
        type_line(&mut app, "write the docs");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AddTask("write the docs".into()))
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn cancelling_a_prompt_adds_nothing() {
        let mut app = with_board();
        press(&mut app, KeyCode::Char('n'));
        type_line(&mut app, "never mind");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    // ---- the palette reaches the same places ----

    /// A screen you can open one way and not the other is a screen half the
    /// users never find.
    #[test]
    fn every_workspace_is_reachable_by_a_slash_command_as_well_as_a_letter() {
        for (line, expected) in [
            ("/agents", Workspace::Fleet),
            ("/memory", Workspace::Memory),
            ("/schedules", Workspace::Schedules),
            ("/goals", Workspace::Goals),
            ("/hooks", Workspace::Hooks),
            ("/tasks", Workspace::Tasks),
            ("/activity", Workspace::Activity),
            ("/team", Workspace::Team),
        ] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            let slash = command::parse(line).unwrap_or_else(|| panic!("{line} did not parse"));
            apply_slash(&mut app, slash);
            assert_eq!(app.workspace, expected, "{line}");
        }
    }

    /// Typing the command for the screen you are already on takes you home,
    /// exactly as pressing `Ctrl-A` twice does.
    #[test]
    fn a_workspace_command_typed_twice_comes_back_to_chat() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Open(Workspace::Fleet));
        assert_eq!(app.workspace, Workspace::Fleet);
        apply_slash(&mut app, command::Slash::Open(Workspace::Fleet));
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// `/memory prefers` should land you looking at the answer, not at the
    /// list with the query still to type.
    #[test]
    fn memory_with_a_query_opens_the_list_already_filtered() {
        let mut app = with_memory();
        app.go(Workspace::Chat);
        apply_slash(&mut app, command::Slash::Memory(Some("linear".into())));
        assert_eq!(app.workspace, Workspace::Memory);
        assert_eq!(
            app.row_ids(Workspace::Memory),
            vec!["linear-is-truth".to_string()]
        );
    }

    /// Naming a row is what makes `/schedule <name>` worth having beside
    /// `/schedules`.
    #[test]
    fn naming_a_row_lands_the_cursor_on_it() {
        let mut app = with_schedules();
        app.go(Workspace::Chat);
        apply_slash(
            &mut app,
            command::Slash::OpenNamed(Workspace::Schedules, "nightly".into()),
        );
        assert_eq!(app.workspace, Workspace::Schedules);
        assert_eq!(
            app.list(Workspace::Schedules).selected.as_deref(),
            Some("nightly-inbox")
        );
    }

    #[test]
    fn naming_a_row_that_is_not_there_says_so_rather_than_guessing() {
        let mut app = with_schedules();
        apply_slash(
            &mut app,
            command::Slash::OpenNamed(Workspace::Schedules, "nope".into()),
        );
        assert!(last_notice(&app).contains("no schedules called nope"));
    }

    /// A verb the store cannot carry out yet is named, not silently ignored:
    /// a key that appears to do nothing is worse than one that says what it is
    /// waiting for.
    #[test]
    fn a_verb_the_store_cannot_do_yet_says_what_it_is_waiting_for() {
        let mut app = with_schedules();
        match press(&mut app, KeyCode::Char('e')) {
            Some(Action::Pending { verb, needs }) => {
                assert!(verb.contains("nightly-inbox"), "{verb}");
                assert!(!needs.is_empty(), "it has to name the missing call");
            }
            other => panic!("expected a named to-do, got {other:?}"),
        }
    }

    /// The editor handoff is a decision the key handler makes and the loop
    /// carries out, so the decision is testable without a terminal.
    #[test]
    fn the_menus_e_asks_for_the_editor() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        assert_eq!(press(&mut app, KeyCode::Char('e')), Some(Action::Editor));
    }

    #[test]
    fn ctrl_g_e_is_the_menus_route_to_the_editor() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        assert_eq!(press(&mut app, KeyCode::Char('e')), Some(Action::Editor));
    }

    /// Quitting is ahead of every layer, because a key that cannot always leave
    /// is a trap.
    #[test]
    fn ctrl_c_still_leaves_from_inside_an_overlay() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        ctrl(&mut app, KeyCode::Char('c'));
        assert!(app.should_quit);
    }

    // ---- the verbs, as keys ----

    fn with_goals() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.goals = vec![data::GoalRow {
            name: "ship-the-tui".into(),
            cadence: "every hour".into(),
            last_ms: None,
            next_ms: Some(1),
            state: data::GoalState::Running,
            iteration: 3,
            objective: "Wire every verb to the store".into(),
            checks: vec![],
            stop_if: "5 iterations move nothing".into(),
            spent_usd: 0.0,
            budget_usd: 0.0,
            iterations: vec![],
            escalation: None,
        }];
        app.go(Workspace::Goals);
        app
    }

    fn with_hooks(last_run: Option<&str>) -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.hooks = vec![data::HookRow {
            name: "ci-failed".into(),
            repo: "Reljod/Jod".into(),
            event: "workflow_run.completed".into(),
            runs: "Claude Code".into(),
            deliveries_24h: 1,
            last_ms: Some(1),
            last_outcome: data::Outcome::Ok,
            state: data::HookState::Armed,
            endpoint: "POST /webhooks/github".into(),
            secret: "✓ verified".into(),
            match_rule: "github on Reljod/Jod".into(),
            runs_as: "Claude Code · /tmp".into(),
            prompt: "Fix {{title}}".into(),
            policy: "untrusted payload".into(),
            created: "2026-08-01".into(),
            total: 1,
            deliveries: vec![data::Delivery {
                at_ms: 1,
                id: "d-1".into(),
                what: "workflow_run.completed".into(),
                accepted: true,
                run: last_run.map(str::to_string),
                verdict: "accepted".into(),
            }],
        }];
        app.go(Workspace::Hooks);
        app
    }

    /// `r` must not spawn. Bringing the next instant forward is what makes a
    /// hand-started run pick up the same overlap policy and failure count as a
    /// timed one.
    #[test]
    fn r_on_a_schedule_brings_its_next_fire_forward_rather_than_spawning() {
        let mut app = with_schedules();
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            Some(Action::RunSchedule("nightly-inbox".into()))
        );
    }

    #[test]
    fn p_on_a_schedule_asks_the_store_to_flip_its_state() {
        let mut app = with_schedules();
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Some(Action::ToggleSchedule("nightly-inbox".into()))
        );
    }

    #[test]
    fn enter_on_a_schedule_opens_the_run_its_last_fire_started() {
        let mut app = with_schedules();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::OpenScheduleRun("nightly-inbox".into()))
        );
    }

    /// The dry run reads nothing and writes nothing, so it answers on the spot.
    #[test]
    fn t_on_a_schedule_prints_its_next_fire_times_without_asking_the_store() {
        let mut app = with_schedules();
        assert_eq!(press(&mut app, KeyCode::Char('t')), None);
        // Every line of one answer collects into one flash rather than each
        // replacing the last — the whole point of `App::notify`'s tick check.
        let printed = format!("{:?}", app.flash);
        assert!(printed.contains("0 2 * * *"), "{printed}");
        assert_eq!(
            app.flash.as_ref().map(|f| f.lines.len()),
            Some(DRY_RUN_FIRES + 1),
            "one heading and five times: {printed}"
        );
    }

    /// A cron expression the store would never have accepted still has to
    /// produce a line, or the key looks broken rather than the schedule.
    #[test]
    fn a_dry_run_of_an_unreadable_expression_says_so_rather_than_printing_nothing() {
        let lines = next_fires("0 2 * * *", "Mars/Olympus", 0, DRY_RUN_FIRES);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("cannot be read"), "{lines:?}");
    }

    #[test]
    fn r_and_p_on_a_goal_reach_the_goal_loop_rather_than_the_scheduler() {
        let mut app = with_goals();
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            Some(Action::RunGoal("ship-the-tui".into()))
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Some(Action::ToggleGoal("ship-the-tui".into()))
        );
    }

    #[test]
    fn p_on_a_webhook_turns_the_rule_off() {
        let mut app = with_hooks(None);
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Some(Action::ToggleHook("ci-failed".into()))
        );
    }

    /// A delivery already names the run it started, so `⏎` needs no store call.
    #[test]
    fn enter_on_a_webhook_opens_the_run_its_last_delivery_started() {
        let mut app = with_hooks(Some("run-77"));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Watch("run-77".into()))
        );
    }

    #[test]
    fn enter_on_a_webhook_that_has_started_no_run_says_so_rather_than_nothing() {
        let mut app = with_hooks(None);
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        let last = last_notice(&app);
        assert!(last.contains("has started a run"), "{last}");
    }

    /// One confirmation, four screens: what `y` means is read off the screen the
    /// question was asked on.
    #[test]
    fn y_deletes_the_kind_of_thing_the_screen_is_showing() {
        for (mut app, expected) in [
            (with_goals(), Action::DeleteGoal("ship-the-tui".into())),
            (with_hooks(None), Action::DeleteHook("ci-failed".into())),
            (with_memory(), Action::Forget("prefers-spec-first".into())),
        ] {
            press(&mut app, KeyCode::Char('x'));
            assert!(
                matches!(app.overlay, Overlay::Confirm { .. }),
                "it must ask first"
            );
            assert_eq!(press(&mut app, KeyCode::Char('y')), Some(expected));
        }
    }

    #[test]
    fn n_on_the_confirmation_deletes_nothing_on_any_screen() {
        for mut app in [with_goals(), with_hooks(None), with_memory()] {
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(press(&mut app, KeyCode::Char('n')), None);
            assert_eq!(app.overlay, Overlay::None);
        }
    }

    // ---- the verbs, as slash commands ----

    fn with_both_screens() -> App {
        let mut app = with_schedules();
        app.goals = with_goals().goals;
        app.go(Workspace::Chat);
        app
    }

    #[test]
    fn run_typed_at_a_name_reaches_whichever_kind_owns_it() {
        let mut app = with_both_screens();
        assert_eq!(
            apply_slash(&mut app, command::Slash::Run("nightly-inbox".into())),
            Some(Action::RunSchedule("nightly-inbox".into()))
        );
        assert_eq!(
            apply_slash(&mut app, command::Slash::Run("ship-the-tui".into())),
            Some(Action::RunGoal("ship-the-tui".into()))
        );
    }

    /// `/pause` and `/unpause` are the same verb: a command that reports success
    /// having changed nothing is worse than one that toggles.
    #[test]
    fn pause_and_unpause_both_flip_whichever_state_the_thing_is_in() {
        let mut app = with_both_screens();
        assert_eq!(
            apply_slash(&mut app, command::Slash::Pause("nightly-inbox".into())),
            Some(Action::ToggleSchedule("nightly-inbox".into()))
        );
        assert_eq!(
            apply_slash(&mut app, command::Slash::Unpause("ship-the-tui".into())),
            Some(Action::ToggleGoal("ship-the-tui".into()))
        );
    }

    #[test]
    fn pausing_a_name_that_is_neither_a_schedule_nor_a_goal_says_so() {
        let mut app = with_both_screens();
        assert_eq!(
            apply_slash(&mut app, command::Slash::Pause("nope".into())),
            None
        );
        let last = last_notice(&app);
        assert!(last.contains("no schedule or goal called nope"), "{last}");
    }

    /// Pausing the wrong one of two things sharing a name is invisible until the
    /// thing that should have happened does not.
    #[test]
    fn a_name_that_is_both_a_schedule_and_a_goal_is_refused_rather_than_guessed() {
        let mut app = with_both_screens();
        app.goals[0].name = "nightly-inbox".into();
        assert_eq!(
            apply_slash(&mut app, command::Slash::Pause("nightly-inbox".into())),
            None
        );
        let last = last_notice(&app);
        assert!(last.contains("both a schedule and a goal"), "{last}");
    }

    /// Typed or pointed at, forgetting is the same irreversible thing.
    #[test]
    fn forget_typed_as_a_command_still_asks_before_it_forgets() {
        let mut app = with_memory();
        app.go(Workspace::Chat);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Forget("linear-is-truth".into())),
            None
        );
        assert_eq!(
            app.workspace,
            Workspace::Memory,
            "and it shows you what it means"
        );
        match &app.overlay {
            Overlay::Confirm { verb, what, .. } => {
                assert_eq!(verb, "forget");
                assert_eq!(what, "linear-is-truth");
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Forget("linear-is-truth".into()))
        );
    }

    // ---- the verbs, against a real store ----

    use jod_core::schedule::{Goal, Misfire, Overlap, Schedule};
    use jod_core::store::{NewFact, Store as RealStore};
    use jod_core::webhook::{Conditions, Rule};

    fn store() -> RealStore {
        RealStore::in_memory().expect("an in-memory store")
    }

    fn a_schedule(name: &str) -> Schedule {
        Schedule {
            id: format!("sch-{name}"),
            name: name.to_string(),
            prompt: "Triage the inbox.".into(),
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 2 * * *".into(),
            timezone: "UTC".into(),
            state: ScheduleState::Armed,
            misfire: Misfire::default(),
            overlap: Overlap::default(),
            grace_ms: 60_000,
            jitter_ms: 0,
            next_fire_at_ms: None,
            last_fire_at_ms: None,
            consecutive_failures: 0,
            created_at_ms: 0,
        }
    }

    fn a_goal(name: &str) -> Goal {
        Goal {
            id: format!("goal-{name}"),
            name: name.to_string(),
            objective: "Wire every verb to the store".into(),
            done_when: None,
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            cron: "0 * * * *".into(),
            timezone: "UTC".into(),
            state: GoalState::Running,
            iteration: 0,
            max_iterations: None,
            budget_usd: None,
            spent_usd: 0.0,
            stall_after: 5,
            no_progress: 0,
            next_fire_at_ms: None,
            created_at_ms: 0,
        }
    }

    fn a_rule(name: &str) -> Rule {
        Rule {
            id: format!("wr-{name}"),
            name: name.to_string(),
            source: "github".into(),
            repo: "Reljod/Jod".into(),
            event: "pull_request".into(),
            action: None,
            conditions: Conditions::default(),
            prompt: "Look at {{title}}".into(),
            harness: "claude_code".into(),
            cwd: "/tmp".into(),
            model: None,
            enabled: true,
            created_at_ms: 0,
        }
    }

    #[test]
    fn pausing_a_schedule_stops_it_and_pressing_p_again_arms_it() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();

        let said = toggle_schedule(&store, "nightly-inbox");
        assert!(said.contains("paused"), "{said}");
        let paused = store.schedule_named("nightly-inbox").unwrap().unwrap();
        assert_eq!(paused.state, ScheduleState::Paused);

        let said = toggle_schedule(&store, "nightly-inbox");
        assert!(said.contains("armed"), "{said}");
        let armed = store.schedule_named("nightly-inbox").unwrap().unwrap();
        assert_eq!(armed.state, ScheduleState::Armed);
        assert!(
            armed.next_fire_at_ms.is_some(),
            "and it knows when it fires next"
        );
    }

    /// A broken schedule is what a person is most likely to be pressing `p` at,
    /// so it must take one press rather than two.
    #[test]
    fn arming_a_broken_schedule_takes_one_press_not_two() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        store
            .set_schedule_state("nightly-inbox", ScheduleState::Broken)
            .unwrap();

        let said = toggle_schedule(&store, "nightly-inbox");
        assert!(said.contains("armed"), "{said}");
        let armed = store.schedule_named("nightly-inbox").unwrap().unwrap();
        assert_eq!(armed.state, ScheduleState::Armed);
        assert_eq!(
            armed.consecutive_failures, 0,
            "arming believes it will work now"
        );
    }

    #[test]
    fn running_a_schedule_now_makes_it_due_now() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();

        let said = run_schedule(&store, "nightly-inbox", 1_700_000_000_000);
        assert!(said.contains("due now"), "{said}");
        let s = store.schedule_named("nightly-inbox").unwrap().unwrap();
        assert_eq!(s.next_fire_at_ms, Some(1_700_000_000_000));
    }

    /// The store refuses a schedule that is not armed, and a refusal nobody is
    /// told about is indistinguishable from a key that does nothing.
    #[test]
    fn running_a_paused_schedule_reports_the_refusal_and_changes_nothing() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        store
            .set_schedule_state("nightly-inbox", ScheduleState::Paused)
            .unwrap();
        let before = store.schedule_named("nightly-inbox").unwrap().unwrap();

        let said = run_schedule(&store, "nightly-inbox", 1_700_000_000_000);
        assert!(said.contains("paused"), "{said}");
        assert!(said.contains("nightly-inbox"), "{said}");

        let after = store.schedule_named("nightly-inbox").unwrap().unwrap();
        assert_eq!(
            after.next_fire_at_ms, before.next_fire_at_ms,
            "nothing moved"
        );
    }

    #[test]
    fn deleting_a_schedule_removes_it_and_deleting_it_twice_says_it_is_gone() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();

        assert!(delete_schedule(&store, "nightly-inbox").contains("deleted"));
        assert!(store.schedule_named("nightly-inbox").unwrap().is_none());
        assert!(delete_schedule(&store, "nightly-inbox").contains("no schedule called"));
    }

    /// Every verb has to survive being pointed at nothing: a row can be deleted
    /// in another process between the tick that drew it and the key.
    #[test]
    fn a_verb_pointed_at_something_that_is_gone_says_so_rather_than_failing_silently() {
        let store = store();
        assert!(run_schedule(&store, "ghost", 0).contains("no schedule called ghost"));
        assert!(toggle_schedule(&store, "ghost").contains("no schedule called ghost"));
        assert!(run_goal(&store, "ghost", 0).contains("no goal called ghost"));
        assert!(toggle_goal(&store, "ghost").contains("no goal called ghost"));
        assert!(delete_goal(&store, "ghost").contains("no goal called ghost"));
        assert!(toggle_hook(&store, "ghost").contains("no webhook called ghost"));
        assert!(delete_hook(&store, "ghost").contains("no webhook called ghost"));
        assert_eq!(
            last_run_of(&store, "ghost"),
            Err("no schedule called ghost".into())
        );
    }

    #[test]
    fn pausing_a_goal_stops_it_and_a_stalled_goal_starts_again_on_one_press() {
        let store = store();
        store.add_goal(&a_goal("ship-the-tui")).unwrap();

        assert!(toggle_goal(&store, "ship-the-tui").contains("paused"));
        assert_eq!(
            store.goal_named("ship-the-tui").unwrap().unwrap().state,
            GoalState::Paused
        );

        store
            .set_goal_state("ship-the-tui", GoalState::Stalled)
            .unwrap();
        let said = toggle_goal(&store, "ship-the-tui");
        assert!(said.contains("running again"), "{said}");
        assert_eq!(
            store.goal_named("ship-the-tui").unwrap().unwrap().state,
            GoalState::Running
        );
    }

    #[test]
    fn running_a_paused_goal_reports_the_refusal_and_changes_nothing() {
        let store = store();
        store.add_goal(&a_goal("ship-the-tui")).unwrap();
        store
            .set_goal_state("ship-the-tui", GoalState::Paused)
            .unwrap();
        let before = store.goal_named("ship-the-tui").unwrap().unwrap();

        let said = run_goal(&store, "ship-the-tui", 1_700_000_000_000);
        assert!(said.contains("paused"), "{said}");
        assert_eq!(
            store
                .goal_named("ship-the-tui")
                .unwrap()
                .unwrap()
                .next_fire_at_ms,
            before.next_fire_at_ms
        );
    }

    #[test]
    fn a_webhook_turned_off_stays_a_rule_and_can_be_turned_back_on() {
        let store = store();
        store.add_webhook_rule(&a_rule("ci-failed")).unwrap();

        let said = toggle_hook(&store, "ci-failed");
        assert!(said.contains("off"), "{said}");
        assert!(!store.webhook_rule("ci-failed").unwrap().unwrap().enabled);

        assert!(toggle_hook(&store, "ci-failed").contains("on"));
        assert!(store.webhook_rule("ci-failed").unwrap().unwrap().enabled);
    }

    #[test]
    fn deleting_a_webhook_removes_the_rule() {
        let store = store();
        store.add_webhook_rule(&a_rule("ci-failed")).unwrap();
        assert!(delete_hook(&store, "ci-failed").contains("deleted"));
        assert!(store.webhook_rule("ci-failed").unwrap().is_none());
    }

    /// Forgetting only the predicate that happens to be showing would leave the
    /// rest of the node readable while the screen claimed it was gone.
    #[test]
    fn forgetting_a_node_destroys_every_predicate_of_it() {
        let store = store();
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
        store
            .remember(NewFact::new("reljod", "works-on", "jod"))
            .unwrap();
        store
            .remember(NewFact::new("someone-else", "prefers", "notion"))
            .unwrap();

        let said = forget_about(&store, "reljod");
        assert!(said.contains("forgot 2 things"), "{said}");
        assert!(store.facts_about("reljod").unwrap().is_empty());
        assert_eq!(
            store.facts_about("someone-else").unwrap().len(),
            1,
            "and it forgot nothing about anybody else"
        );
    }

    #[test]
    fn forgetting_something_nothing_is_known_about_says_so_rather_than_claiming_a_deletion() {
        let store = store();
        let said = forget_about(&store, "never-heard-of-it");
        assert!(said.contains("nothing is recorded"), "{said}");
    }

    /// A schedule that has fired without ever starting a run — every fire
    /// skipped — has nothing to open, and saying so beats opening the wrong run.
    #[test]
    fn opening_a_schedule_that_has_started_no_run_says_so() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        assert_eq!(
            last_run_of(&store, "nightly-inbox"),
            Err("nightly-inbox has not started a run yet".into())
        );
    }

    #[test]
    fn opening_a_schedule_finds_the_newest_fire_that_started_a_run() {
        use jod_core::schedule::{Fire, FireOutcome};
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        for (fired_at_ms, run_id, outcome) in [
            (1_000, Some("run-old"), FireOutcome::Ran),
            // Newer, but it started nothing — so it must not be the answer.
            (2_000, None, FireOutcome::SkippedOverlap),
        ] {
            store
                .record_fire(&Fire {
                    id: 0,
                    schedule_id: "sch-nightly-inbox".into(),
                    due_at_ms: fired_at_ms,
                    fired_at_ms,
                    run_id: run_id.map(str::to_string),
                    outcome,
                    detail: None,
                })
                .unwrap();
        }
        assert_eq!(last_run_of(&store, "nightly-inbox"), Ok("run-old".into()));
    }

    // ---- the loop carries them out ----

    /// The field is filled from the store, not left at its default.
    ///
    /// This is the test the whole branch keeps needing. Every failure it has
    /// found has the same shape — something complete, tested, and connected to
    /// nothing — and a `delivery` field that compiles, renders and is never
    /// populated would be the next one. It would look right on every screen,
    /// because `Nothing` is the correct answer for most runs and the wrong one
    /// silently.
    #[tokio::test]
    async fn a_run_whose_reply_was_lost_says_so_on_its_fleet_row() {
        use jod_core::ledger::NewMessage;

        let store = RealStore::in_memory().unwrap();
        let owner = jod_core::ledger::Owner::new("here", 1);
        // A run that owed a reply, given up on.
        let id = store
            .record_obligation(
                &NewMessage::new("telegram:7:1", "telegram", "7", "your build broke")
                    .about_run("run-lost"),
                &owner,
                1_000,
            )
            .unwrap();
        store.mark_attempting(id, &owner, 1_000).unwrap();
        // Past `MAX_ATTEMPTS`, so this settles as failed rather than pending.
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            store.mark_failed(id, "chat is gone", 2_000).unwrap();
            store.mark_attempting(id, &owner, 2_000).unwrap();
        }
        store.mark_failed(id, "chat is gone", 2_000).unwrap();

        assert_eq!(
            delivery::verdict_of_run(&store, "run-lost"),
            delivery::Verdict::Lost,
            "the ledger must call this lost before the row can"
        );
        // And a run that owed nobody anything is not quietly called fine.
        assert_eq!(
            delivery::verdict_of_run(&store, "run-quiet"),
            delivery::Verdict::Nothing
        );
    }

    fn jod_with(store: RealStore) -> Arc<Jod> {
        Jod::with_store(Arc::new(store))
    }

    /// The console has to see the agents it did not start.
    ///
    /// `Jod::agents` reads an in-memory map, and every engineer a project
    /// manager hires is spawned by an MCP server in another process, which
    /// never touches this one's map. `rehydrate` ran once at launch and was
    /// never called again, so those runs were in the tree — built from SQL —
    /// and missing from the agent list. `selected_agent` resolves a session row
    /// through that list, so it answered `None` for a row visibly spinning and
    /// every run verb refused: there was no way to stop an agent a manager had
    /// started.
    ///
    /// The run here is written straight to the store and never announced,
    /// because that is exactly what another process looks like from in here.
    #[tokio::test]
    async fn the_console_picks_up_a_run_another_process_started() {
        let store = RealStore::in_memory().expect("an in-memory store");
        let summary = jod_core::service::AgentSummary {
            id: "run-elsewhere".into(),
            name: "gamma-engineer".into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "claude-code".into(),
            status: jod_core::service::AgentStatus::Running,
            cwd: "/tmp".into(),
            model: None,
            permission: jod_core::PermissionPolicy::AcceptEdits,
            pid: None,
            pgid: None,
            process_alive: true,
            watch_command: String::new(),
            created_at_ms: 1,
            session_id: Some("a-session".into()),
            usage: Default::default(),
            event_count: 0,
            last_message: None,
        };
        store
            .save_run(&jod_core::store::StoredRun {
                id: "run-elsewhere".into(),
                name: "gamma-engineer".into(),
                harness: "claude-code".into(),
                status: "running".into(),
                cwd: "/tmp".into(),
                session_id: Some("a-session".into()),
                pid: None,
                pgid: None,
                created_at_ms: 1,
                summary: serde_json::to_value(&summary).unwrap(),
            })
            .expect("a run written by somebody else");

        let jod = jod_with(store);
        let mut app = app_on(HarnessKind::ClaudeCode);

        // The bug, stated: asking this process what it knows finds nothing.
        app.agents = list_agents(&jod).await;
        assert!(
            !app.agents.iter().any(|a| a.id == "run-elsewhere"),
            "the premise: the map holds only what this process started",
        );

        refresh_fleet(&jod, &mut app).await;
        assert!(
            app.agents.iter().any(|a| a.id == "run-elsewhere"),
            "the console has to see it to be able to stop it: {:?}",
            app.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
        );
    }

    // ---- the model and the mode are written down ----

    /// A conversation with a turn in it, and a chat box bound to it.
    fn talking_about(s: &RealStore, run: &str) -> (App, Thread) {
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.append_prompt(&c.id, run, "go").unwrap();
        (
            app_on(HarnessKind::ClaudeCode),
            Thread {
                conversation: Some(c.id),
                ..Thread::default()
            },
        )
    }

    /// What the next spawn would actually send, asked of the same function
    /// `Jod::spawn_agent_in` asks.
    fn next_turn(s: &RealStore, conversation: &str) -> SpawnRequest {
        let mut req = SpawnRequest {
            name: "n".into(),
            harness: HarnessKind::ClaudeCode,
            prompt: "the next thing".into(),
            system: None,
            cwd: PathBuf::from("/tmp"),
            // A fresh process, carrying nothing but its defaults — which is the
            // state that used to lose the choice.
            model: None,
            permission: PermissionPolicy::Bypass,
            resume: Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
        };
        let row = s.conversation(conversation).unwrap().unwrap();
        jod_core::service::prefer_conversation_settings(&mut req, &row);
        req
    }

    /// The gap the wiring audit found: the read side was built and nothing ever
    /// wrote the column, so `/model` still evaporated on resume and the tests
    /// passed because they set the column themselves.
    ///
    /// Driven through the real path — the slash command hands back an action,
    /// the loop carries it out — and read back through the real function a spawn
    /// uses, so nothing here is a stand-in for the thing being tested.
    #[tokio::test]
    async fn a_model_chosen_in_the_chat_box_survives_into_the_next_turn() {
        let jod = jod_with(store());
        let s = jod.store().unwrap().clone();
        let (mut app, mut thread) = talking_about(&s, "run-1");
        let id = thread.conversation.clone().unwrap();

        let action = apply_slash(&mut app, command::Slash::Model(Some("sonnet".into())))
            .expect("/model is written down, not only applied");
        perform(&jod, &mut app, &options(), &mut thread, action).await;

        // It applied here...
        assert_eq!(app.model.as_deref(), Some("sonnet"));
        // ...and it is still the answer for a spawn that asked for nothing.
        assert_eq!(next_turn(&s, &id).model.as_deref(), Some("sonnet"));
    }

    /// The console opens *in* the main chat rather than beside it.
    ///
    /// The binding used to start derived — "the conversation the run on screen
    /// wrote" — so on a cold start the first sentence typed went to whichever
    /// agent this machine had most recently finished.
    #[tokio::test]
    async fn the_launch_position_is_the_main_chat() {
        let jod = jod_with(store());
        let opts = options();
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();

        enter_main(&jod, &mut app, &opts, &mut thread, true).await;

        let main = jod
            .store()
            .unwrap()
            .main_conversation(app.harness, &opts.cwd.display().to_string())
            .unwrap();
        assert_eq!(thread.conversation.as_deref(), Some(main.as_str()));
        assert_eq!(app.workspace, Workspace::Chat);
        // And nothing written to the screen, so the splash keeps the column:
        // `fresh` shows it only while the transcript holds nothing but hints,
        // and the empty-state line would replace the wordmark on every launch.
        assert!(app.transcript.is_empty(), "{:?}", app.transcript);
    }

    /// Bound is not the same as *looking at* it. The chat box stays bound to
    /// the main conversation while you walk the fleet, so `⏎` on the tree's
    /// pinned row has to move even though the binding does not change — and
    /// tested on the binding alone it answered "already in the main chat" from
    /// a screen that is plainly not it.
    #[tokio::test]
    async fn entering_main_from_another_screen_moves_even_when_already_bound() {
        let jod = jod_with(store());
        let opts = options();
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();
        enter_main(&jod, &mut app, &opts, &mut thread, true).await;

        app.go(Workspace::Fleet);
        enter_main(&jod, &mut app, &opts, &mut thread, false).await;
        assert_eq!(
            app.workspace,
            Workspace::Chat,
            "the fleet's pinned row is a dead key"
        );

        // Pressed again from the chat itself it says so, rather than clearing
        // the transcript to replay the same lines back into it.
        enter_main(&jod, &mut app, &opts, &mut thread, false).await;
        let said = last_notice(&app);
        assert!(said.contains("already in the main chat"), "{said}");
    }

    /// Same for the mode, which before this was fixed once at `jod tui` launch
    /// and could not be changed at all — then could be changed and not kept.
    #[tokio::test]
    async fn a_mode_chosen_in_the_chat_box_survives_into_the_next_turn() {
        let jod = jod_with(store());
        let s = jod.store().unwrap().clone();
        let (mut app, mut thread) = talking_about(&s, "run-1");
        let id = thread.conversation.clone().unwrap();

        let action = apply_slash(&mut app, command::Slash::Mode(Some(PermissionPolicy::Plan)))
            .expect("/mode is written down too");
        perform(&jod, &mut app, &options(), &mut thread, action).await;

        assert_eq!(app.mode, PermissionPolicy::Plan);
        let next = next_turn(&s, &id);
        assert_eq!(next.permission, PermissionPolicy::Plan);
        assert!(!next.permission.may_act(), "plan still means plan tomorrow");
    }

    /// `/mode` with no argument cycles, and what has to be recorded is the mode
    /// it arrived at — not the argument, which is exactly the one that is
    /// missing.
    #[tokio::test]
    async fn cycling_the_mode_records_the_one_it_landed_on() {
        let jod = jod_with(store());
        let s = jod.store().unwrap().clone();
        let (mut app, mut thread) = talking_about(&s, "run-1");
        let id = thread.conversation.clone().unwrap();
        app.mode = PermissionPolicy::Plan;

        let action =
            apply_slash(&mut app, command::Slash::Mode(None)).expect("cycling is a choice");
        perform(&jod, &mut app, &options(), &mut thread, action).await;

        assert_eq!(app.mode, PermissionPolicy::Ask, "no argument cycles");
        assert_eq!(next_turn(&s, &id).permission, PermissionPolicy::Ask);
    }

    /// "The harness default" is a choice like any other. Without this, `/model`
    /// with no argument could set a model but never unset one, and the only way
    /// back would be to start a new conversation.
    #[tokio::test]
    async fn clearing_the_model_is_stored_as_a_choice_rather_than_ignored() {
        let jod = jod_with(store());
        let s = jod.store().unwrap().clone();
        let (mut app, mut thread) = talking_about(&s, "run-1");
        let id = thread.conversation.clone().unwrap();
        for chosen in [Some("sonnet".to_string()), None] {
            let action = apply_slash(&mut app, command::Slash::Model(chosen)).unwrap();
            perform(&jod, &mut app, &options(), &mut thread, action).await;
        }

        assert_eq!(app.model, None);
        assert_eq!(s.conversation(&id).unwrap().unwrap().model, None);
        // ...and a caller with an opinion is left with it, because the
        // conversation now says it has none.
        let mut req = next_turn(&s, &id);
        req.model = Some("opus".into());
        jod_core::service::prefer_conversation_settings(
            &mut req,
            &s.conversation(&id).unwrap().unwrap(),
        );
        assert_eq!(req.model.as_deref(), Some("opus"));
    }

    /// There is no conversation before the first turn — one is minted by the
    /// first *run*, not by opening the program. The choice must not be dropped
    /// on the floor for being early, and it must not be dropped from the first
    /// spawn either.
    #[tokio::test]
    async fn a_choice_made_before_the_first_turn_is_kept_until_there_is_somewhere_to_put_it() {
        let jod = jod_with(store());
        let s = jod.store().unwrap().clone();
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();

        let action = apply_slash(&mut app, command::Slash::Mode(Some(PermissionPolicy::Plan)))
            .expect("still a choice");
        perform(&jod, &mut app, &options(), &mut thread, action).await;
        assert_eq!(
            thread.pending,
            vec![Setting::Mode(PermissionPolicy::Plan)],
            "nowhere to write it yet, so it waits"
        );
        // The first spawn still gets it: `spawn` reads the app, not the store.
        assert_eq!(
            bounded(options().ceiling(), app.mode),
            PermissionPolicy::Plan
        );

        // The first turn mints a conversation; the run is the handle on it.
        let c = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        s.append_prompt(&c.id, "run-1", "go").unwrap();
        flush_pending(&jod, &mut app, &mut thread, "run-1");

        assert!(thread.pending.is_empty(), "written down and let go of");
        assert_eq!(next_turn(&s, &c.id).permission, PermissionPolicy::Plan);
    }

    /// A write that lands nowhere is a failure, not a silent nothing. It means
    /// the chat box is bound to a conversation that is not there — precisely the
    /// state in which a setting quietly failing to stick could not be diagnosed.
    #[tokio::test]
    async fn a_setting_that_could_not_be_stored_says_so() {
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread {
            conversation: Some("no-such-conversation".into()),
            ..Thread::default()
        };

        let action = apply_slash(&mut app, command::Slash::Model(Some("sonnet".into()))).unwrap();
        perform(&jod, &mut app, &options(), &mut thread, action).await;

        let last = last_notice(&app);
        assert!(last.contains("this turn only"), "{last}");
    }

    /// A launch that asked for nothing — both `None`, which is what "no flags
    /// were given" now means and what lets a stored preference apply.
    fn options() -> Options {
        Options {
            harness: None,
            team: None,
            cwd: PathBuf::from("/tmp"),
            model: None,
            permission: None,
            resume: Resume::Fresh,
        }
    }

    /// A launch that insisted, which a stored preference must not override.
    fn options_launched_on(harness: HarnessKind, mode: PermissionPolicy) -> Options {
        Options {
            harness: Some(harness),
            permission: Some(mode),
            ..options()
        }
    }

    /// The row has to reflect what just happened. Waiting for the next tick to
    /// correct it makes a working key look like a broken one for four seconds.
    #[tokio::test]
    async fn acting_on_a_row_refreshes_it_rather_than_leaving_it_stale() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        let jod = jod_with(store);
        let mut app = app_on(HarnessKind::ClaudeCode);
        refresh_workspaces(&jod, &mut app);
        assert_eq!(app.schedules[0].state, data::ScheduleState::Armed);

        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::ToggleSchedule("nightly-inbox".into()),
        )
        .await;

        assert_eq!(app.schedules[0].state, data::ScheduleState::Paused);
        let last = last_notice(&app);
        assert!(last.contains("paused"), "{last}");
    }

    #[tokio::test]
    async fn deleting_through_the_loop_takes_the_row_off_the_screen() {
        let store = store();
        store.add_schedule(&a_schedule("nightly-inbox")).unwrap();
        let jod = jod_with(store);
        let mut app = app_on(HarnessKind::ClaudeCode);
        refresh_workspaces(&jod, &mut app);

        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::DeleteSchedule("nightly-inbox".into()),
        )
        .await;

        assert!(app.schedules.is_empty());
    }

    // ---- the traffic log is reachable, and is fed ----

    /// A work with one session and one run under it, exactly as
    /// `Store::forest_of` flattens them.
    fn forest_of_one_work() -> Vec<jod_core::tree::Node> {
        use jod_core::tree::{Node, NodeId, NodeKind};
        let node = |id: NodeId, parent: Option<NodeId>, kind, depth, label: &str| Node {
            id,
            parent,
            kind,
            depth,
            label: label.into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 0,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
            expanded: true,
            has_children: false,
        };
        let mut work = node(NodeId::work("w1"), None, NodeKind::Work, 0, "port the parser");
        work.has_children = true;
        let mut session = node(
            NodeId::session("s1"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            1,
            "port the lexer",
        );
        session.has_children = true;
        let run = node(
            NodeId::run("r1"),
            Some(NodeId::session("s1")),
            NodeKind::Run,
            2,
            "run one",
        );
        vec![work, session, run]
    }

    /// The app as a refresh leaves it, with the forest folded the way the
    /// screen folds it.
    ///
    /// Through `fleet::condense` rather than straight onto `app.forest`,
    /// because these tests press keys on rows: the work rows and the run rows
    /// are gone by the time anything is drawn, and a test that put them back
    /// would be pressing keys on a tree nobody can see.
    fn on_the_tree(selected: jod_core::tree::NodeId) -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let folded = jod_core::tree::condense(&forest_of_one_work(), &std::collections::HashSet::new());
        app.forest = folded.nodes;
        app.work_of = folded.works;
        app.tree_runs = folded.runs;
        app.run_of = folded.run_of;
        app.go(Workspace::Fleet);
        app.tree.selected = Some(selected);
        app
    }

    /// The cursor drawn on a fleet holding a tree is the *tree's*, so the keys
    /// that move a cursor have to move that one. Pressed through the router,
    /// because the bug this pins was entirely in the routing: the list spine
    /// answered `↑`/`↓` first and stepped the flat list nobody was looking at,
    /// leaving the highlight where it was — a key that looks dead.
    #[test]
    fn the_cursor_keys_move_the_tree_on_a_fleet_that_has_one() {
        use jod_core::tree::NodeId;
        for (down, up) in [
            (KeyCode::Down, KeyCode::Up),
            (KeyCode::Char('j'), KeyCode::Char('k')),
        ] {
            let mut app = on_the_tree(NodeId::work("w1"));
            press(&mut app, down);
            assert_eq!(
                app.tree.selected,
                Some(NodeId::session("s1")),
                "{down:?} did not move the tree cursor"
            );
            press(&mut app, up);
            assert_eq!(
                app.tree.selected,
                Some(NodeId::work("w1")),
                "{up:?} did not move the tree cursor"
            );
        }
    }

    // ---- the pane below the tree ----
    //
    // `Store::forest_of` only reads conversations that belong to a work, so a
    // run started by `delegate` has no node and is drawn in a second pane under
    // the tree. That pane was a picture of a list: no row in it could be
    // highlighted, `↓` stopped at the last node above it, and every verb the
    // keybar advertises read the cursor, found no node, and did nothing.

    /// A tree plus one run that belongs to no work, which is what puts a row in
    /// the loose pane.
    fn with_a_loose_run() -> App {
        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        // `r1` is a run the forest accounted for — folded onto the session that
        // started it — so it is *not* loose; `r2` reached no session and is what
        // the pane below the tree draws.
        app.agents = vec![agent_line("r1", None), agent_line("r2", Some("sess-2"))];
        app.reconcile();
        app
    }

    #[test]
    fn the_cursor_walks_out_of_the_bottom_of_the_tree_into_the_loose_pane() {
        let mut app = with_a_loose_run();
        assert_eq!(
            app.loose_rows().iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["r2"],
            "the fixture has exactly one row in the lower pane"
        );

        // Down through main · work · session, and then one more.
        let rows = app.tree_rows();
        app.tree.selected = rows.get(rows.len() - 2).cloned();
        assert_eq!(app.loose_selected(), None, "still in the tree");

        press(&mut app, KeyCode::Down);

        assert_eq!(
            app.loose_selected(),
            Some(0),
            "one press past the last node lands in the pane below it"
        );
        press(&mut app, KeyCode::Up);
        assert_eq!(app.loose_selected(), None, "and comes back out");
    }

    #[test]
    fn end_reaches_the_loose_pane_rather_than_the_last_node() {
        let mut app = with_a_loose_run();
        press(&mut app, KeyCode::End);
        assert_eq!(app.loose_selected(), Some(0));
    }

    /// The half that makes the pane worth reaching. Every one of these reads
    /// `selected_agent`, which answered `None` on a loose row — so the keys the
    /// bar prints were all silent there.
    #[test]
    fn the_run_verbs_act_on_the_loose_row_under_the_cursor() {
        for (key, expected) in [
            (KeyCode::Char('a'), Action::Attach("r2".into())),
            (KeyCode::Enter, Action::Watch("r2".into())),
        ] {
            let mut app = with_a_loose_run();
            press(&mut app, KeyCode::End);
            assert_eq!(press(&mut app, key), Some(expected), "{key:?}");
        }
    }

    /// `s` on a finished run answers with a sentence rather than an action, and
    /// that sentence is the proof the key found the row: silence is what it did
    /// before, and silence is indistinguishable from a dead key.
    #[test]
    fn stopping_a_finished_loose_run_names_the_run_it_found() {
        let mut app = with_a_loose_run();
        press(&mut app, KeyCode::End);

        assert_eq!(press(&mut app, KeyCode::Char('s')), None);

        let said = format!("{:?}", spoken(&app));
        assert!(said.contains("nothing to stop"), "{said}");
        assert!(said.contains("r2"), "and it names the loose run: {said}");
    }

    /// The guard that refuses a run verb on a work heading must not catch these:
    /// a loose row *is* a run, it just has no node.
    #[test]
    fn a_loose_row_is_not_refused_as_a_row_with_no_process_on_it() {
        let mut app = with_a_loose_run();
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Char('s'));
        // Read through `spoken`, not the transcript. This is a *negative*
        // assertion on a fleet key, and the fleet's answers are flashes — read
        // off an empty transcript it would pass without the key being pressed.
        let said = format!("{:?}", spoken(&app));
        assert!(
            !said.contains("not a run"),
            "the loose row was refused as if it were a heading: {said}"
        );
        assert!(
            said.contains("nothing to stop"),
            "and it did answer, so the check above is not vacuous: {said}"
        );
    }

    /// The rest of the cursor set, which the same routing swallowed: a tree
    /// deep enough to need `End` is exactly the one where walking it row by row
    /// is not an answer.
    ///
    /// `Home` lands on the pinned chat rather than on the first work, because
    /// the pinned chat *is* the top row — see [`crate::tui::fleet::main_id`].
    #[test]
    fn home_end_and_the_page_keys_move_the_tree_too() {
        use jod_core::tree::NodeId;
        let main = crate::tui::fleet::main_id();
        let last = NodeId::session("s1");
        let mut app = on_the_tree(NodeId::work("w1"));
        press(&mut app, KeyCode::End);
        assert_eq!(app.tree.selected, Some(last.clone()));
        press(&mut app, KeyCode::Home);
        assert_eq!(app.tree.selected, Some(main.clone()));
        press(&mut app, KeyCode::PageDown);
        assert_eq!(
            app.tree.selected,
            Some(last),
            "a page past the end clamps"
        );
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.tree.selected, Some(main));
    }

    /// The other half of the same fault: a moving cursor is no use if the verbs
    /// act on a different row. `s` stops the run the highlight is on, which on
    /// a tree is the tree's row and not the flat list's.
    ///
    /// The row is the *agent's* now rather than a run's. The fold puts the run
    /// away and hands its verbs to the session that took it, so this is also the
    /// check that an agent's row still stops the process it is showing.
    #[test]
    fn the_run_verbs_act_on_the_row_the_tree_cursor_is_on() {
        use jod_core::tree::NodeId;
        let mut app = on_the_tree(NodeId::work("w1"));
        app.agents = vec![running("r1", "run one"), running("other", "not this one")];
        // The flat list points somewhere else entirely, which is exactly the
        // state that used to decide what `s` stopped.
        app.list_mut(Workspace::Fleet).selected = Some("other".into());

        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.tree.selected,
            Some(NodeId::session("s1")),
            "on the agent's row"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::Stop("r1".into())),
            "it stopped whatever the invisible list cursor was on"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            Some(Action::Attach("r1".into()))
        );
    }

    /// A heading is not a process, and neither is an agent that has never taken
    /// a run. The verbs say so rather than going quiet, the way the pinned
    /// chat's do.
    ///
    /// An agent that *has* a run is the other case, and it is
    /// `the_run_verbs_act_on_the_row_the_tree_cursor_is_on`: the fold hands a
    /// run's verbs to the row showing it, so "no process here" is now a claim
    /// about the row rather than about its kind.
    #[test]
    fn a_run_verb_on_a_row_that_holds_no_run_says_why() {
        use jod_core::tree::NodeId;
        // The same work and session, with the run taken out — an agent that has
        // been opened and has not been given anything to do yet.
        let idle: Vec<jod_core::tree::Node> = forest_of_one_work()
            .into_iter()
            .filter(|n| n.kind != jod_core::tree::NodeKind::Run)
            .collect();
        for (row, word, forest) in [
            (NodeId::work("w1"), "a work", forest_of_one_work()),
            (NodeId::session("s1"), "a session", idle),
        ] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            let folded = jod_core::tree::condense(&forest, &std::collections::HashSet::new());
            app.forest = folded.nodes;
            app.work_of = folded.works;
            app.tree_runs = folded.runs;
            app.run_of = folded.run_of;
            app.go(Workspace::Fleet);
            app.tree.selected = Some(row.clone());
            app.agents = vec![running("r1", "run one")];
            assert_eq!(press(&mut app, KeyCode::Char('s')), None, "from {row:?}");
            let said = last_notice(&app);
            assert!(said.contains(word), "from {row:?}: {said}");
            assert!(said.contains("not a run"), "from {row:?}: {said}");
        }
    }

    /// The way back into the main chat from a fleet that has grown a tree.
    ///
    /// The forest is works and what hangs off them, so the main chat has no node
    /// in it — and the tree replaces the flat list whole, taking the pinned row
    /// with it. Without this the fleet was a screen you could walk into and not
    /// back out of except by `Ctrl-G`.
    #[test]
    fn enter_on_the_trees_pinned_row_goes_into_the_main_chat() {
        for key in [KeyCode::Enter, KeyCode::Right] {
            let mut app = on_the_tree(crate::tui::fleet::main_id());
            assert_eq!(press(&mut app, key), Some(Action::EnterMain), "{key:?}");
        }
    }

    /// It is the first row, and `↑` from the top work arrives on it — which is
    /// what makes it findable without being told it is there.
    #[test]
    fn the_pinned_chat_is_the_trees_first_row() {
        let main = crate::tui::fleet::main_id();
        let app = on_the_tree(main.clone());
        assert_eq!(app.tree_rows().first(), Some(&main));

        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.tree.selected, Some(main));
    }

    /// Drawn first, but not where the cursor starts. The chat is the anchor;
    /// the cursor belongs on the work, because managing the work is what
    /// opening this screen means — the same rule the flat list follows for the
    /// same pinned row, and the reason `reconcile_to` takes a fallback.
    #[test]
    fn the_trees_cursor_starts_on_the_first_work_not_the_pinned_chat() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_of_one_work();
        app.go(Workspace::Fleet);
        assert_eq!(
            app.tree.selected,
            Some(jod_core::tree::NodeId::work("w1")),
            "opening the fleet parked the cursor on the pinned chat"
        );
    }

    /// A conversation has no process, so the run verbs answer rather than
    /// reaching for one. Left to fall through they found the *flat* list's
    /// cursor — an agent nobody could see, stopped by a key pressed on a row
    /// that is not it.
    #[test]
    fn a_run_verb_on_the_trees_pinned_row_says_it_is_the_chat() {
        let mut app = on_the_tree(crate::tui::fleet::main_id());
        app.agents = vec![running("r1", "run one")];
        app.list_mut(Workspace::Fleet).selected = Some("r1".into());

        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        let said = last_notice(&app);
        assert!(said.contains("the main chat"), "{said}");
    }

    /// Nothing to fold and no parent to climb to. Answered on the row rather
    /// than passed to `collapse_or_parent`, which would act on whichever node
    /// it found — not the one that is highlighted.
    #[test]
    fn left_on_the_trees_pinned_row_stays_put() {
        let main = crate::tui::fleet::main_id();
        let mut app = on_the_tree(main.clone());
        assert_eq!(press(&mut app, KeyCode::Left), None);
        assert_eq!(app.tree.selected, Some(main));
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    /// The tree takes the cursor keys and nothing else: `/` still opens the
    /// fleet's filter line, which is the one the tree reads its needle from.
    #[test]
    fn the_tree_does_not_swallow_the_spines_own_keys() {
        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        press(&mut app, KeyCode::Char('/'));
        assert!(app.here().editing_filter, "the filter line never opened");
    }

    /// One project and its manager row, built the way core builds them.
    ///
    /// `has_children: false` on the manager is copied from `tree::forest` and is
    /// not incidental: it is the reason `→` needed an arm of its own, because
    /// `expand_or_descend` returns immediately on a childless row.
    fn forest_with_a_manager(conversation: &str) -> Vec<jod_core::tree::Node> {
        use jod_core::tree::{Node, NodeId, NodeKind};
        vec![
            Node {
                id: NodeId::project("p1"),
                parent: None,
                kind: NodeKind::Project,
                depth: 0,
                label: "tetris".into(),
                summary: String::new(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                stalled: 0,
                colour: "cyan".into(),
                branch: None,
                worktree: None,
                expanded: true,
                has_children: true,
            },
            Node {
                id: NodeId::manager(conversation),
                parent: Some(NodeId::project("p1")),
                kind: NodeKind::Manager,
                depth: 1,
                label: "manager".into(),
                summary: String::new(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                stalled: 0,
                colour: "cyan".into(),
                branch: None,
                worktree: None,
                expanded: true,
                has_children: false,
            },
        ]
    }

    /// The console as it stands after `⏎` on a manager row: the chat screen,
    /// the chat box bound to that conversation, and the row it came from still
    /// in the forest.
    fn in_a_manager(conversation: &str) -> (App, Thread) {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_with_a_manager(conversation);
        app.go(Workspace::Chat);
        let thread = Thread {
            conversation: Some(conversation.to_string()),
            ..Default::default()
        };
        (app, thread)
    }

    /// [`press`], for the handful of tests that care which conversation the chat
    /// box is bound to. The throwaway `Thread` the ordinary helper mints is
    /// bound to nothing, which is exactly the state these are contrasting with.
    fn press_in(app: &mut App, thread: &mut Thread, code: KeyCode) -> Option<Action> {
        on_key(app, thread, KeyEvent::new(code, KeyModifiers::NONE), 20)
    }

    /// Check 16. `⏎` on a manager row goes *into* that conversation, the same
    /// movement the pinned row makes into the main chat.
    ///
    /// Not `Watch` and not `Sessions(Open)`. Watching puts a run's output on
    /// screen and leaves the chat box where it was; `Sessions(Open)` prints a
    /// conversation's contents as notice lines. Neither binds the chat box, and
    /// binding it is the whole point — what you type next has to be an
    /// instruction to that manager.
    #[test]
    fn enter_on_a_manager_row_goes_into_that_conversation() {
        use jod_core::tree::NodeId;
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_with_a_manager("conv-9");
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::manager("conv-9"));

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::EnterManager("conv-9".into())),
            "the row has to carry the conversation to bind to"
        );
    }

    /// `→` on a manager row was a printed key that did nothing. Core builds the
    /// row with no children, so `expand_or_descend` had nothing to open and
    /// nothing to descend into — while the pinned chat row one level up has
    /// always taken `→` as "go into this conversation".
    #[test]
    fn right_on_a_manager_row_goes_into_that_conversation() {
        use jod_core::tree::NodeId;
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_with_a_manager("conv-9");
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::manager("conv-9"));

        assert_eq!(
            press(&mut app, KeyCode::Right),
            Some(Action::EnterManager("conv-9".into())),
            "→ on a manager row is still the dead key it was"
        );
    }

    /// And `→` everywhere else is still the tree's own key. A project has
    /// children, so it opens rather than binding the chat box to anything.
    #[test]
    fn right_on_a_project_row_still_opens_the_branch() {
        use jod_core::tree::NodeId;
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_with_a_manager("conv-9");
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::project("p1"));
        app.tree.collapsed.insert(NodeId::project("p1"));

        assert_eq!(press(&mut app, KeyCode::Right), None);
        assert!(
            app.tree
                .is_expanded(&NodeId::project("p1"), &app.closed_works),
            "→ on a branch stopped opening it"
        );
    }

    /// The bug. Sitting in a manager with nothing running, `←` did nothing at
    /// all: the arm above it only fires while a run is on screen, and a manager
    /// waiting for you to type has none. So the one key that means "out" did not
    /// go out, from a screen you reached with one keystroke.
    #[test]
    fn left_out_of_an_idle_manager_goes_back_to_the_fleet() {
        let (mut app, mut thread) = in_a_manager("conv-9");
        assert!(!app.busy);
        assert_eq!(app.watching, None, "nothing is running to back out of");

        assert_eq!(press_in(&mut app, &mut thread, KeyCode::Left), None);
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    /// `←` then `⏎` is a round trip. With the cursor left where it was, `⏎`
    /// would reopen whichever row the fleet last pointed at — very likely a run
    /// belonging to something else entirely.
    #[test]
    fn left_out_of_a_manager_lands_on_the_row_that_opened_it() {
        use jod_core::tree::NodeId;
        let (mut app, mut thread) = in_a_manager("conv-9");
        press_in(&mut app, &mut thread, KeyCode::Left);

        assert_eq!(app.tree.selected, Some(NodeId::manager("conv-9")));
        assert_eq!(
            press_in(&mut app, &mut thread, KeyCode::Enter),
            Some(Action::EnterManager("conv-9".into())),
            "⏎ did not reopen what ← left"
        );
    }

    /// Pointing at the row is not enough when the project above it is folded
    /// shut: the row is not drawn, and `reconcile_to` drops a cursor that is on
    /// nothing at the next refresh. Backing out opens the way back as well as
    /// pointing at it.
    #[test]
    fn backing_out_opens_the_project_folded_over_the_manager() {
        use jod_core::tree::NodeId;
        let (mut app, mut thread) = in_a_manager("conv-9");
        app.tree.collapsed.insert(NodeId::project("p1"));

        press_in(&mut app, &mut thread, KeyCode::Left);
        app.reconcile();

        assert_eq!(
            app.tree.selected,
            Some(NodeId::manager("conv-9")),
            "the cursor was parked on a row nothing draws"
        );
    }

    /// The rule that keeps the shortcut free, in a manager exactly as anywhere
    /// else: with text in the box, `←` is the cursor. A version of this that
    /// grabbed the key unconditionally would make the input box unusable.
    #[test]
    fn left_with_something_typed_in_a_manager_still_moves_the_cursor() {
        let (mut app, mut thread) = in_a_manager("conv-9");
        for c in "hello".chars() {
            press_in(&mut app, &mut thread, KeyCode::Char(c));
        }

        press_in(&mut app, &mut thread, KeyCode::Left);
        assert_eq!(app.workspace, Workspace::Chat, "the box owns the key");
        assert_eq!(app.cursor, 4, "the cursor moved back over the `o`");
    }

    /// The main chat keeps the old behaviour, because it is home rather than
    /// somewhere you went — there is nothing to back out *to*. Only a
    /// conversation the forest knows as a manager row gets the new arm.
    #[test]
    fn left_in_a_conversation_that_is_not_a_manager_stays_put() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = forest_with_a_manager("conv-9");
        app.go(Workspace::Chat);
        let mut thread = Thread {
            conversation: Some("the-main-chat".into()),
            ..Default::default()
        };

        assert_eq!(press_in(&mut app, &mut thread, KeyCode::Left), None);
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// `x` on a project row untracks that repository, and carries the row's own
    /// id rather than only its name.
    ///
    /// The id is the point. Two checkouts called `proj` make a name ambiguous
    /// and the typed route refuses on it, but the cursor is already on exactly
    /// one of them — a console that answered "which one did you mean" to a
    /// finger pointing at the answer would be arguing with the user.
    #[test]
    fn x_on_a_project_row_untracks_that_repository() {
        use jod_core::tree::{Node, NodeId, NodeKind};
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = vec![
            Node {
                id: NodeId::project("p1"),
                parent: None,
                kind: NodeKind::Project,
                depth: 0,
                label: "tetris".into(),
                summary: String::new(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                stalled: 0,
                colour: "cyan".into(),
                branch: None,
                worktree: None,
                expanded: true,
                has_children: true,
            },
            Node {
                id: NodeId::work("w1"),
                parent: Some(NodeId::project("p1")),
                kind: NodeKind::Work,
                depth: 1,
                label: "port the parser".into(),
                summary: String::new(),
                running: false,
                status: None,
                stalled_for_ms: None,
                cards: 0,
                blocked: 0,
                stalled: 0,
                colour: "cyan".into(),
                branch: None,
                worktree: None,
                expanded: true,
                has_children: false,
            },
        ];
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::project("p1"));

        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Some(Action::UntrackProject {
                id: Some("p1".into()),
                name: "tetris".into(),
            }),
        );

        // On a work row it refuses and says where to press it instead. `T`
        // climbs to the work above the cursor and that is right for a key that
        // navigates; a mutating verb that climbed would untrack a whole
        // repository from a keystroke aimed at one job inside it.
        //
        // It names the project rather than describing where it is. "The top row
        // of the group this one is in" is a direction the reader has to follow
        // to find out what it points at, and the screen already knows.
        app.tree.selected = Some(NodeId::work("w1"));
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        let said = last_notice(&app);
        assert!(said.contains("project row"), "{said}");
        assert!(said.contains("tetris"), "{said}");

        // The pinned chat is a sentinel and not a row in the forest, so
        // `selected_node` answers None for it. Its own sentence, because it is
        // in no group and being told to look at the top row of one is an
        // instruction it cannot follow.
        app.tree.selected = Some(crate::tui::fleet::main_id());
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        let said = last_notice(&app);
        assert!(said.contains("main chat"), "{said}");
        assert!(said.contains("project row"), "{said}");
    }

    /// `x` on a work that belongs to no project says so, instead of naming a
    /// row above it that does not exist.
    ///
    /// A work with a null `project_id` is drawn at depth 0, beside the project
    /// rows rather than under one, so on screen it is indistinguishable from a
    /// repository. Told to press `x` on "the top row of the group this one is
    /// in", the reader is being sent to the row they are already on — which is
    /// what made the key look broken rather than refused.
    #[test]
    fn x_on_a_work_with_no_project_says_it_has_none() {
        use jod_core::tree::{Node, NodeId, NodeKind};
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = vec![Node {
            id: NodeId::work("w1"),
            parent: None,
            kind: NodeKind::Work,
            depth: 0,
            label: "port the parser".into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 0,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
            expanded: true,
            has_children: false,
        }];
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::work("w1"));

        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        let said = last_notice(&app);
        assert!(said.contains("no catalogued repository"), "{said}");
        assert!(said.contains("/project add"), "{said}");
        assert!(
            !said.contains("the group this one is in"),
            "there is no group above a top-level row: {said}"
        );
    }

    /// `x` is advertised on the keybar whether or not there is a tree, and a
    /// printed key that does nothing and says nothing is the failure the two
    /// guards at the top of `on_fleet_key` exist to stop. With no works there
    /// are no project rows at all, so the sentence has to say that rather than
    /// send the reader looking for a row that is not drawn.
    #[test]
    fn x_on_a_fleet_with_no_tree_says_there_is_nothing_to_untrack() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("r1", "run one")];
        app.go(Workspace::Fleet);
        assert!(!app.has_tree(), "this case only exists without a forest");

        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        let said = last_notice(&app);
        assert!(said.contains("no projects on the fleet"), "{said}");
        assert!(said.contains("/project add"), "{said}");
    }

    /// And a project row is a heading, so it folds rather than pretending to
    /// open something — the same thing a work row does.
    #[test]
    fn enter_on_a_project_row_folds_it() {
        use jod_core::tree::{Node, NodeId, NodeKind};
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = vec![Node {
            id: NodeId::project("p1"),
            parent: None,
            kind: NodeKind::Project,
            depth: 0,
            label: "tetris".into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 0,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
            expanded: true,
            has_children: true,
        }];
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::project("p1"));

        assert_eq!(press(&mut app, KeyCode::Enter), None, "a heading opens nothing");
    }

    /// **G5.S2.** The screen is opened from the tree, through the router — not
    /// by calling the handler, which would prove only that the handler works.
    #[test]
    fn t_on_a_work_in_the_tree_opens_that_works_traffic() {
        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        assert_eq!(press(&mut app, KeyCode::Char('T')), None);
        assert_eq!(app.workspace, Workspace::Traffic);
        assert_eq!(
            app.traffic_of,
            Some(traffic::Watching::work("w1")),
            "the screen opened on some other scope than the row under the cursor"
        );
    }

    /// From an agent's row as well, and it is the *work's* bus: a session's
    /// half of a conversation is not a conversation.
    ///
    /// The tree has no work row above it to climb to any more, so this is also
    /// the check that the fold remembered which work the agent came out of.
    #[test]
    fn t_on_a_session_opens_the_bus_of_the_work_it_came_out_of() {
        let row = jod_core::tree::NodeId::session("s1");
        let mut app = on_the_tree(row.clone());
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.workspace, Workspace::Traffic, "from {row:?}");
        assert_eq!(
            app.traffic_of,
            Some(traffic::Watching::work("w1")),
            "from {row:?}"
        );
    }

    /// Drilled rather than jumped to, so `Esc` comes back to the tree you were
    /// reading rather than to the chat — the same relationship memory's local
    /// graph has to its list.
    #[test]
    fn escape_comes_back_from_the_traffic_log_to_the_tree() {
        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.workspace, Workspace::Fleet);
    }

    /// Opening a work's traffic must not show the last one's conversation for
    /// the frame before the tick catches up.
    #[test]
    fn opening_a_works_traffic_starts_from_an_empty_log() {
        let mut app = on_the_tree(jod_core::tree::NodeId::work("w1"));
        press(&mut app, KeyCode::Char('T'));
        app.traffic.title = "port the parser".into();
        app.traffic.messages = vec![];
        app.traffic.used = 12;

        app.go(Workspace::Fleet);
        app.tree.selected = Some(jod_core::tree::NodeId::work("w1"));
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.traffic, traffic::Log::default(), "{:?}", app.traffic);
    }

    /// `T` on a session that belongs to no work has nothing to open, and says
    /// so rather than looking broken.
    #[test]
    fn t_on_a_row_with_no_work_above_it_explains_itself() {
        use jod_core::tree::{Node, NodeId, NodeKind};
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.forest = vec![Node {
            id: NodeId::session("orphan"),
            parent: None,
            kind: NodeKind::Session,
            depth: 0,
            label: "started before works existed".into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 0,
            colour: String::new(),
            branch: None,
            worktree: None,
            expanded: true,
            has_children: false,
        }];
        app.go(Workspace::Fleet);
        app.tree.selected = Some(NodeId::session("orphan"));
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.workspace, Workspace::Fleet, "nothing to open");
        assert!(app.traffic_of.is_none());
        let said = last_notice(&app);
        assert!(said.contains("no work"), "{said}");
    }

    /// The keybar prints `T traffic` on the fleet whatever the fleet holds, so
    /// the one state where there is no tree to press it on has to answer rather
    /// than do nothing.
    #[test]
    fn t_on_a_fleet_with_no_works_in_it_says_why_there_is_nothing_to_read() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Fleet);
        assert!(!app.has_tree());
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.workspace, Workspace::Fleet);
        let said = last_notice(&app);
        assert!(said.contains("traffic is a work's bus"), "{said}");
    }

    /// **The screen is fed by the tick**, not by the keypress that opened it.
    /// Agents write to this bus from other processes, so a log read once at
    /// open would be stale by the second message — and a screen nothing
    /// refreshes is a screen that quietly shows yesterday.
    #[tokio::test]
    async fn the_tick_loads_the_traffic_of_whichever_work_is_open() {
        use jod_core::team::{Post, Scope};
        let store = store();
        let work = store.create_work("port the parser").unwrap();
        store
            .join_scope(
                Scope::Work,
                &work.id,
                "asker",
                HarnessKind::ClaudeCode,
                "engineer",
                None,
            )
            .unwrap();
        store
            .join_scope(
                Scope::Work,
                &work.id,
                "answerer",
                HarnessKind::ClaudeCode,
                "engineer",
                None,
            )
            .unwrap();
        store
            .post(&Post::new(Scope::Work, &work.id, "asker", "where is the lexer?").to("answerer"))
            .unwrap();

        let jod = jod_with(store);
        let mut app = app_on(HarnessKind::ClaudeCode);

        // Nothing open: the tick must not invent a scope, and must not pay for
        // a query nobody asked for.
        refresh_workspaces(&jod, &mut app);
        assert!(app.traffic.messages.is_empty());

        app.traffic_of = Some(traffic::Watching::work(&work.id));
        refresh_workspaces(&jod, &mut app);
        assert_eq!(app.traffic.messages.len(), 1, "the tick did not read the bus");
        assert_eq!(app.traffic.messages[0].message.from, "asker");
        assert_eq!(app.traffic.budget, jod_core::works::DEFAULT_MESSAGE_BUDGET);
        assert_eq!(app.traffic.used, 1, "and it read the budget the bus enforces");
        assert_eq!(
            app.row_ids(Workspace::Traffic).len(),
            1,
            "the cursor has a row to sit on"
        );
    }

    /// `f` is the screen's own verb and it reaches the screen's own handler.
    /// Pressed through the router, because a handler nothing routes to is the
    /// bug this suite exists to catch.
    #[test]
    fn f_on_the_traffic_log_cycles_which_states_are_shown() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.traffic_of = Some(traffic::Watching::work("w1"));
        app.go(Workspace::Traffic);
        assert_eq!(app.traffic_shown, traffic::Shown::Everything);

        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.traffic_shown, traffic::Shown::Problems);
        let said = last_notice(&app);
        assert!(said.contains(traffic::Shown::Problems.label()), "{said}");

        for _ in 1..traffic::Shown::ALL.len() {
            press(&mut app, KeyCode::Char('f'));
        }
        assert_eq!(app.traffic_shown, traffic::Shown::Everything, "the cycle closes");
    }

    /// `⏎` puts the whole message in the transcript, reason and all — the row
    /// is one line and a message is prose.
    #[tokio::test]
    async fn enter_on_a_refused_message_prints_the_reason_it_was_refused() {
        use jod_core::team::{Post, Scope};
        let store = store();
        let work = store.create_work("port the parser").unwrap();
        store
            .post(&Post::new(Scope::Work, &work.id, "asker", "are you free?").to("nobody-here"))
            .unwrap();

        let jod = jod_with(store);
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.traffic_of = Some(traffic::Watching::work(&work.id));
        refresh_workspaces(&jod, &mut app);
        app.go(Workspace::Traffic);

        press(&mut app, KeyCode::Enter);
        let said = last_notice(&app);
        assert!(said.contains("asker"), "{said}");
        assert!(said.contains("are you free?"), "the message itself: {said}");
        assert!(
            said.contains("`nobody-here` is not a member of this work"),
            "and why nobody read it: {said}"
        );
    }

    /// A TUI with no database must lose the keypress, not the session.
    #[tokio::test]
    async fn a_verb_with_no_database_behind_it_is_a_notice_rather_than_a_crash() {
        let jod = Jod::new();
        let mut app = app_on(HarnessKind::ClaudeCode);
        for action in [
            Action::RunSchedule("nightly-inbox".into()),
            Action::ToggleGoal("ship-the-tui".into()),
            Action::DeleteHook("ci-failed".into()),
            Action::Forget("reljod".into()),
            Action::OpenScheduleRun("nightly-inbox".into()),
        ] {
            perform(&jod, &mut app, &options(), &mut Thread::default(), action).await;
            let last = last_notice(&app);
            assert!(last.contains(NO_STORE), "{last}");
        }
        assert!(!app.should_quit, "and the session is still up");
    }

    // ---- a cold session answers out loud ----

    /// What is actually on the screen, drawn by the real renderer.
    ///
    /// The whole class of bug this section exists for is output that is
    /// produced correctly and then never drawn, so an assertion about the
    /// transcript vector would have passed throughout — the transcript was
    /// always right. Only the frame can tell.
    fn on_screen(app: &App, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                ui::draw(f, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first thing a new user does must not render as nothing.
    ///
    /// From a cold `jod tui`, `/root` used to print absolutely nothing: the
    /// roots reached the transcript, the splash kept the column because every
    /// entry so far was a `Notice`, and the answer was painted over. The
    /// command was correct and the screen was blank, which is the worst
    /// possible pairing — it teaches you the program is broken on the first
    /// keystroke you try.
    ///
    /// Driven through `perform` and drawn with `ui::draw` on purpose. A test
    /// that asserted `fresh()` or counted transcript entries is what let this
    /// ship.
    #[tokio::test]
    async fn a_cold_session_shows_what_a_notice_only_command_answered() {
        let store = store();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap();
        store
            .add_root(
                &conversation.id,
                jod_core::roots::NewRoot::reading("/srv/reljod/notes"),
            )
            .unwrap();

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.conversation = Some(conversation.id.clone());
        // Exactly the state a cold launch leaves: the hint and nothing else.
        app.push(startup_hint());
        let before = on_screen(&app, 120, 40);
        assert!(
            before.contains("an orchestrator, not a chat window"),
            "the splash owns a session that has done nothing:\n{before}"
        );

        let jod = jod_with(store);
        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::ListRoots,
        )
        .await;

        let after = on_screen(&app, 120, 40);
        assert!(
            after.contains("/srv/reljod/notes"),
            "the roots `/root` listed have to be on the screen:\n{after}"
        );
        assert!(
            !after.contains("an orchestrator, not a chat window"),
            "and the splash has to have got out of the way:\n{after}"
        );
        // Spelled out, as `jod root ls` already does. `ro` was two letters
        // nothing on screen explained, and it is the one fact on the line worth
        // reading: a checkout is read-only, which is *why* an agent's edits
        // land in a worktree rather than where the person is looking.
        assert!(
            after.contains("read-only") || after.contains("writable"),
            "the line says what it means rather than abbreviating it:\n{after}"
        );
    }

    /// The startup hint is the one line that must *not* dismiss the splash.
    ///
    /// The narrow edge of the fix: "any notice drops the splash" would have
    /// been simpler and would have meant no new session ever saw a wordmark,
    /// because Jod says something the moment it opens.
    #[test]
    fn the_line_jod_opens_with_does_not_count_as_an_answer() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.push(startup_hint());
        let frame = on_screen(&app, 120, 40);
        assert!(
            frame.contains("an orchestrator, not a chat window"),
            "the splash survives Jod's own opening line:\n{frame}"
        );
    }

    /// `Ctrl-B` spends money on an agent nobody is watching, so the transcript
    /// says which agent, what it was told, and where it was pointed — on the
    /// cold screen where delegation is most often the very first thing tried.
    #[test]
    fn delegating_says_which_agent_what_it_was_told_and_where() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.push(startup_hint());
        // Built by the same function the delegate path pushes, and given the
        // directory through `Options` exactly as `perform` does — a fixture
        // assembled here would still pass on the day the wiring changed.
        let launched_in = Options {
            cwd: PathBuf::from("/srv/reljod/tetris"),
            ..options()
        };
        app.push(delegated(
            "87e84b92f1c04d".into(),
            "Build a working Tetris game in Node and Vite".into(),
            &launched_in,
        ));
        let frame = on_screen(&app, 120, 40);
        assert!(frame.contains("87e84b92"), "the agent id:\n{frame}");
        assert!(
            frame.contains("Build a working Tetris game in Node and Vite"),
            "what it was told, in full:\n{frame}"
        );
        assert!(
            frame.contains("/srv/reljod/tetris"),
            "and the directory it may write in:\n{frame}"
        );
        assert!(
            !frame.contains("an orchestrator, not a chat window"),
            "a delegation is not a session that has done nothing:\n{frame}"
        );
    }

    // ---- preferences ----

    /// The gap the whole config layer was written to close: `/thinking` used
    /// to flip a bool and nothing else, so the choice was made again at every
    /// launch.
    #[test]
    fn toggling_thinking_changes_the_screen_and_asks_for_the_choice_to_be_kept() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert!(app.show_thinking, "shown by default");

        let action = apply_slash(&mut app, command::Slash::Thinking);
        assert!(!app.show_thinking, "the screen changed now");
        assert_eq!(
            action,
            Some(Action::Config(config::Request::Set(
                config::Pref::Thinking,
                config::Value::Flag(false)
            ))),
            "and the choice went to the store"
        );

        // Back on again, and that is a choice too.
        let action = apply_slash(&mut app, command::Slash::Thinking);
        assert!(app.show_thinking);
        assert_eq!(
            action,
            Some(Action::Config(config::Request::Set(
                config::Pref::Thinking,
                config::Value::Flag(true)
            )))
        );
    }

    #[test]
    fn toggling_tool_output_records_its_choice_too() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let action = apply_slash(&mut app, command::Slash::Details);
        assert!(!app.show_details);
        assert_eq!(
            action,
            Some(Action::Config(config::Request::Set(
                config::Pref::Details,
                config::Value::Flag(false)
            )))
        );
    }

    /// The end-to-end claim: turn it off, close the TUI, open it again, and it
    /// is still off. The restart is a second `App` over the same store.
    #[tokio::test]
    async fn a_toggle_survives_a_restart() {
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        let action = apply_slash(&mut app, command::Slash::Thinking).unwrap();
        perform(&jod, &mut app, &options(), &mut Thread::default(), action).await;

        let mut next = app_on(HarnessKind::ClaudeCode);
        assert!(next.show_thinking, "a fresh app starts at the default");
        load_preferences(&mut next, jod.store().unwrap(), &options());
        assert!(!next.show_thinking, "the stored choice won");
    }

    /// `None` from `Store::setting` means "no opinion", which has to leave the
    /// built-in default standing rather than becoming false, empty or unset.
    #[test]
    fn an_unset_preference_falls_back_to_the_default_rather_than_to_nothing() {
        let store = store();
        let mut app = app_on(HarnessKind::ClaudeCode);
        load_preferences(&mut app, &store, &options());

        assert!(app.show_thinking, "still shown");
        assert!(app.show_details, "still shown");
        assert_eq!(app.harness, HarnessKind::ClaudeCode);
        assert!(
            app.transcript.is_empty(),
            "and nothing was said about settings nobody has touched"
        );
    }

    /// A launch flag is about this session; a preference is about every one
    /// that follows. Naming the harness on the command line has to win.
    #[test]
    fn a_stored_harness_yields_to_the_harness_named_at_launch() {
        let store = store();
        config::write(
            &store,
            config::Pref::Harness,
            &config::Value::Harness(HarnessKind::Agy),
        )
        .unwrap();

        let mut app = app_on(HarnessKind::ClaudeCode);
        load_preferences(&mut app, &store, &options());
        assert_eq!(
            app.harness,
            HarnessKind::Agy,
            "nothing was asked for, so the choice wins"
        );

        let launched_on = options_launched_on(HarnessKind::OpenCode, PermissionPolicy::default());
        let mut app = app_on(HarnessKind::OpenCode);
        load_preferences(&mut app, &store, &launched_on);
        assert_eq!(
            app.harness,
            HarnessKind::OpenCode,
            "-H opencode wins over the choice"
        );
    }

    /// The point of the preference: choose the model once and every later
    /// session opens on it, instead of typing `/model` at each launch.
    #[test]
    fn a_stored_model_applies_when_the_command_line_asked_for_nothing() {
        let store = store();
        config::write(
            &store,
            config::Pref::Model,
            &config::Value::Model(Some("haiku".into())),
        )
        .unwrap();

        let mut app = app_on(HarnessKind::ClaudeCode);
        load_preferences(&mut app, &store, &options());
        assert_eq!(app.model.as_deref(), Some("haiku"));

        // `--model` is somebody overruling their own preference for one session.
        let mut insisted = options();
        insisted.model = Some("opus".into());
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.model = Some("opus".into());
        load_preferences(&mut app, &store, &insisted);
        assert_eq!(
            app.model.as_deref(),
            Some("opus"),
            "--model must win over the stored choice"
        );
    }

    /// The half `/config` cannot enforce on its own. It drops the model when the
    /// *stored* harness changes, but `-H opencode` changes the harness for this
    /// launch only — and `haiku` handed to OpenCode fails the first turn of the
    /// session with a name it has never heard of.
    #[test]
    fn a_stored_model_is_not_handed_to_a_harness_named_at_launch() {
        let store = store();
        config::write(
            &store,
            config::Pref::Harness,
            &config::Value::Harness(HarnessKind::ClaudeCode),
        )
        .unwrap();
        config::write(
            &store,
            config::Pref::Model,
            &config::Value::Model(Some("haiku".into())),
        )
        .unwrap();

        let launched_on = options_launched_on(HarnessKind::OpenCode, PermissionPolicy::default());
        let mut app = app_on(HarnessKind::OpenCode);
        load_preferences(&mut app, &store, &launched_on);
        assert_eq!(
            app.model, None,
            "haiku was Claude Code's name, and OpenCode is what is running"
        );

        // ...and still applies when the launch flag names the harness it was
        // chosen for, which is not a change at all.
        let same = options_launched_on(HarnessKind::ClaudeCode, PermissionPolicy::default());
        let mut app = app_on(HarnessKind::ClaudeCode);
        load_preferences(&mut app, &store, &same);
        assert_eq!(app.model.as_deref(), Some("haiku"));
    }

    /// A stored mode has to apply, and it must not stop applying because a
    /// constant somewhere else changed.
    ///
    /// The bug this closes was silent and had two halves. `load_preferences`
    /// decided "did the user ask for a mode" by comparing the launch option
    /// against `PermissionPolicy::Ask` — the clap default at the time. When the
    /// default moved to `Bypass`, every launch stopped matching, and every
    /// stored mode preference was ignored from then on with nothing to show
    /// for it. Comparing against a default is a guess; `Option` is knowledge.
    #[test]
    fn a_stored_mode_applies_when_the_command_line_asked_for_nothing() {
        let store = store();
        config::write(
            &store,
            config::Pref::Mode,
            &config::Value::Mode(PermissionPolicy::Plan),
        )
        .unwrap();

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.mode = PermissionPolicy::default();
        load_preferences(&mut app, &store, &options());
        assert_eq!(
            app.mode,
            PermissionPolicy::Plan,
            "nothing was asked for, so the stored mode wins"
        );

        // ...and must not, when it did ask. `--permission auto` is somebody
        // overruling their own stored preference for this one session.
        let insisted = options_launched_on(HarnessKind::ClaudeCode, PermissionPolicy::Bypass);
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.mode = PermissionPolicy::Bypass;
        load_preferences(&mut app, &store, &insisted);
        assert_eq!(
            app.mode,
            PermissionPolicy::Bypass,
            "-p auto must win over the stored choice"
        );
    }

    /// Saying nothing is not a ceiling. Only an explicit `--permission` bounds
    /// what the Tab key may reach, or omitting the flag would quietly pin every
    /// session to the default and make the mode key half-broken.
    #[test]
    fn only_an_explicit_flag_becomes_a_ceiling() {
        assert_eq!(
            options().ceiling(),
            PermissionPolicy::Bypass,
            "no flag must leave every mode reachable"
        );
        assert_eq!(
            options_launched_on(HarnessKind::ClaudeCode, PermissionPolicy::Plan).ceiling(),
            PermissionPolicy::Plan,
        );
        assert_eq!(
            bounded(options().ceiling(), PermissionPolicy::Bypass),
            PermissionPolicy::Bypass
        );
        assert_eq!(
            bounded(
                options_launched_on(HarnessKind::ClaudeCode, PermissionPolicy::Plan).ceiling(),
                PermissionPolicy::Bypass
            ),
            PermissionPolicy::Plan,
            "the TUI escalated past a ceiling somebody set on purpose"
        );
    }

    /// A preference the build cannot read must not pass as "never set", or a
    /// setting silently stops applying and nobody is told.
    #[test]
    fn a_preference_this_build_cannot_read_is_said_out_loud_at_startup() {
        let store = store();
        store.set_setting(config::Pref::Mode.key(), "yolo").unwrap();

        let mut app = app_on(HarnessKind::ClaudeCode);
        load_preferences(&mut app, &store, &options());

        let said = format!("{:?}", app.transcript);
        assert!(said.contains("yolo"), "{said}");
    }

    #[tokio::test]
    async fn config_lists_every_preference_and_says_which_were_chosen() {
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Thinking);
        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::Config(config::Request::Set(
                config::Pref::Thinking,
                config::Value::Flag(false),
            )),
        )
        .await;
        app.transcript.clear();

        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::Config(config::Request::List),
        )
        .await;
        assert_eq!(app.transcript.len(), config::Pref::ALL.len());
        let listed = format!("{:?}", app.transcript);
        for pref in config::Pref::ALL {
            assert!(
                listed.contains(pref.name()),
                "{} is missing: {listed}",
                pref.name()
            );
        }
        assert!(listed.contains("chosen"), "{listed}");
        assert!(listed.contains("default"), "{listed}");
    }

    /// A preference set through `/config` has to bite now as well as next time,
    /// or the command looks like it did nothing.
    #[test]
    fn setting_a_visible_preference_changes_the_screen_immediately() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let action = apply_slash(
            &mut app,
            command::Slash::Config(config::Request::Set(
                config::Pref::Details,
                config::Value::Flag(false),
            )),
        );
        assert!(
            !app.show_details,
            "the transcript stops showing tool output now"
        );
        assert!(
            matches!(action, Some(Action::Config(_))),
            "and it is recorded"
        );
    }

    /// `/config harness agy` is about the *next* session; `/harness agy` is
    /// about this one. Confusing the two would silently switch harness
    /// mid-conversation.
    #[test]
    fn setting_the_default_harness_does_not_switch_the_conversation_you_are_in() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::Slash::Config(config::Request::Set(
                config::Pref::Harness,
                config::Value::Harness(HarnessKind::Agy),
            )),
        );
        assert_eq!(app.harness, HarnessKind::ClaudeCode);
    }

    #[test]
    fn a_refused_command_says_why_rather_than_help_lists_them() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::Slash::Refused("mode does not take “yolo”".into()),
        );
        let said = last_notice(&app);
        assert!(said.contains("yolo"), "{said}");
        assert!(!said.contains("/help"), "{said}");
    }

    #[tokio::test]
    async fn a_preference_with_no_database_behind_it_lasts_the_session_only() {
        let jod = Jod::new();
        let mut app = app_on(HarnessKind::ClaudeCode);
        perform(
            &jod,
            &mut app,
            &options(),
            &mut Thread::default(),
            Action::Config(config::Request::List),
        )
        .await;
        let said = last_notice(&app);
        assert!(said.contains(NO_STORE), "{said}");
    }

    // ---- /remember ----

    /// It used to be an `Action::Pending` — an admission that the command did
    /// nothing. A triple is what the store takes, so a triple is what the
    /// command asks for.
    #[test]
    fn remembering_a_fact_writes_it_and_says_what_was_written() {
        let store = store();
        let said = remember_fact(&store, "reljod", "prefers", "linear for tasks");
        assert!(said.contains("reljod prefers linear for tasks"), "{said}");

        let facts = store.facts_about("reljod").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "prefers");
        assert_eq!(facts[0].object, "linear for tasks");
        assert_eq!(
            facts[0].origin,
            jod_core::store::Origin::Owner,
            "typed at this terminal means Reljod said it"
        );
    }

    /// Written, and then findable — a fact that lands in a scope nothing
    /// searches is one that was not really remembered.
    #[test]
    fn a_fact_typed_at_the_tui_can_be_recalled_afterwards() {
        let store = store();
        remember_fact(&store, "reljod", "prefers", "linear for tasks");
        let found = store.recall("linear", 10).unwrap();
        assert!(
            found.iter().any(|f| f.subject == "reljod"),
            "recall found {found:?}"
        );
    }

    #[test]
    fn remember_reaches_the_store_rather_than_a_to_do() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(
                &mut app,
                command::Slash::Remember {
                    subject: "reljod".into(),
                    predicate: "prefers".into(),
                    object: "linear".into(),
                }
            ),
            Some(Action::Remember {
                subject: "reljod".into(),
                predicate: "prefers".into(),
                object: "linear".into(),
            })
        );
    }

    // ---- the conversation graph, from the fleet ----

    fn on_fleet_with_a_run() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![agent_line("run-7", None)];
        app.go(Workspace::Fleet);
        app.reconcile();
        app
    }

    /// The audit's complaint was that `tips`, `branch_at`, `children` and
    /// `sibling_pager` had no production call site. These five keys are that
    /// call site, and this asserts the keypress produces the verb rather than
    /// `None` — the state the whole feature was in before.
    #[test]
    fn the_fleet_keys_reach_the_conversation_graph() {
        for (key, expected) in [
            ('b', sessions::Request::Open("run-7".into())),
            ('u', sessions::Request::Rewind("run-7".into())),
            ('U', sessions::Request::Restore("run-7".into())),
            ('f', sessions::Request::Fork("run-7".into())),
            ('t', sessions::Request::Retry("run-7".into())),
            ('m', sessions::Request::Delivery("run-7".into())),
        ] {
            let mut app = on_fleet_with_a_run();
            assert_eq!(
                press(&mut app, KeyCode::Char(key)),
                Some(Action::Sessions(expected)),
                "`{key}` on the fleet"
            );
        }
    }

    /// `g` has to carry *which thread* as well as which branch.
    ///
    /// Its first cut handed the typed number to `Request::Restore`, which takes
    /// a conversation rather than a message. Conversation ids are uuids and
    /// uuids are hex, so a plain number can prefix-match one — the key could
    /// move the head of a thread the user was not looking at, and it never
    /// consulted the fleet cursor at all. Both halves are asserted here because
    /// the shape, not the arithmetic, is what stops it coming back.
    #[test]
    fn going_to_a_branch_carries_the_thread_it_was_read_off() {
        let mut app = on_fleet_with_a_run();

        assert_eq!(press(&mut app, KeyCode::Char('g')), None, "`g` asks first");
        assert!(
            matches!(
                app.overlay,
                Overlay::Prompt {
                    intent: PromptIntent::Branch,
                    ..
                }
            ),
            "got {:?}",
            app.overlay
        );

        type_line(&mut app, "57");
        let action = press(&mut app, KeyCode::Enter);

        assert_eq!(
            action,
            Some(Action::Sessions(sessions::Request::GoTo {
                conversation: "run-7".into(),
                branch: "57".into(),
            })),
            "the run under the cursor travels with the branch number"
        );
    }

    /// The gap this key closes: the fleet says `completed` and is silent about
    /// whether the person it was for ever heard anything.
    ///
    /// Asserted end to end, because the join is the part that can be quietly
    /// wrong — the ledger keys its rows on the run id that `about_run` recorded,
    /// and a fleet row *is* that id. If those two ever stop being the same
    /// thing, every run reads as owing nobody anything, which is the most
    /// reassuring possible way to be broken.
    #[test]
    fn the_fleet_can_ask_whether_a_finished_run_s_reply_ever_arrived() {
        let store = store();
        let id = store
            .record_obligation(
                &jod_core::ledger::NewMessage::new(
                    "telegram:7:1",
                    "telegram",
                    "7",
                    "the nightly digest",
                )
                .about_run("run-7"),
                &jod_core::ledger::Owner::new("jod-cloud", 4821),
                1_000,
            )
            .expect("an obligation");
        for _ in 0..jod_core::ledger::MAX_ATTEMPTS {
            store
                .mark_attempting(id, &jod_core::ledger::Owner::new("jod-cloud", 4821), 2_000)
                .unwrap();
            store.mark_failed(id, "chat not found", 2_000).unwrap();
        }

        let mut app = on_fleet_with_a_run();
        let Some(Action::Sessions(request)) = press(&mut app, KeyCode::Char('m')) else {
            panic!("`m` on the fleet asks about the selected run's delivery");
        };

        let said = sessions::apply(&store, &request, 5_000).join("\n");
        assert!(
            said.contains("never arrived"),
            "the run under the cursor found its lost reply: {said}"
        );
        assert!(
            said.contains("chat not found"),
            "and the reason came with it: {said}"
        );
    }

    /// `u` is undo on every screen that has one.
    ///
    /// This shipped inverted for a while — `v` undid and `u` redid — which put
    /// `u` on undo in memory and on *redo* in the fleet. The usual defence for
    /// one letter meaning two things does not apply to a verb and its inverse:
    /// `a` attaching here and answering an escalation in goals are unrelated,
    /// so nothing transfers, while undo and redo are one verb inverted and the
    /// habit transfers exactly — onto a screen where the neighbouring keys stop
    /// and fork things.
    #[test]
    fn undo_is_the_lower_case_key_on_every_screen_that_has_one() {
        let mut app = on_fleet_with_a_run();
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            Some(Action::Sessions(sessions::Request::Rewind("run-7".into()))),
            "lower-case `u` undoes"
        );

        // The binding this pair had to agree with. It is still a named to-do
        // rather than a store call, but the *letter* is already spoken for and
        // that is what the fleet had to match.
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.memory = vec![memory_node("linear")];
        app.go(Workspace::Memory);
        app.reconcile();
        let said = press(&mut app, KeyCode::Char('u'));
        assert!(
            matches!(said, Some(Action::Pending { ref verb, .. }) if verb.contains("undo")),
            "memory's `u` is an undo too, got {said:?}"
        );
    }

    /// The run under the cursor is what these act on, so with no cursor they
    /// must do nothing — not act on a thread the user cannot see. `c` is the
    /// exception by design: the list is the way *out* of an empty fleet.
    #[test]
    fn a_conversation_verb_with_no_run_selected_does_nothing_at_all() {
        for key in ['b', 'u', 'U', 'f', 't', 'm'] {
            let mut app = app_on(HarnessKind::ClaudeCode);
            app.go(Workspace::Fleet);
            assert_eq!(press(&mut app, KeyCode::Char(key)), None, "`{key}`");
        }
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(Workspace::Fleet);
        assert_eq!(press(&mut app, KeyCode::Char('c')), None);
        assert!(matches!(app.overlay, Overlay::Sessions(_)));
    }

    // ---- the session list ----
    //
    // The fleet lists runs and the chat is one conversation, so from either
    // one there was no way to reach the list of every thread and pick one:
    // `/sessions` printed directions to the fleet, and the fleet printed ids
    // to type back into `/resume`. These cover the three routes in and what
    // choosing a row does.

    fn a_session(title: &str, session: Option<&str>) -> sessions::SessionRow {
        sessions::SessionRow {
            id: format!("conv-{title}"),
            short: format!("conv-{title}"),
            title: title.to_string(),
            harness: "claude".to_string(),
            model: None,
            session_id: session.map(str::to_string),
            messages: 4,
            updated_at_ms: 0,
            forked_from: None,
            abandoned: 0,
        }
    }

    /// On `ws` with one row loaded and the cursor on it, so `⏎` has something
    /// to land on without the loop having run.
    ///
    /// The workspace is set before the overlay because `App::go` closes
    /// whatever is open — going somewhere is how you leave an overlay.
    fn holding(ws: Workspace, row: sessions::SessionRow) -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.go(ws);
        app.overlay = Overlay::Sessions(sessions::Browser {
            rows: vec![row],
            loaded: true,
            ..Default::default()
        });
        app
    }

    #[test]
    fn the_session_list_opens_from_the_fleet_the_chat_and_the_menu() {
        let mut from_fleet = app_on(HarnessKind::ClaudeCode);
        from_fleet.go(Workspace::Fleet);
        press(&mut from_fleet, KeyCode::Char('c'));
        assert!(matches!(from_fleet.overlay, Overlay::Sessions(_)), "`c`");

        let mut typed = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut typed, command::Slash::Sessions);
        assert!(matches!(typed.overlay, Overlay::Sessions(_)), "/sessions");

        // From the chat, where every bare letter is text, so the leader is the
        // only route that can exist.
        let mut from_chat = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut from_chat, KeyCode::Char('g'));
        press(&mut from_chat, KeyCode::Char('r'));
        assert!(matches!(from_chat.overlay, Overlay::Sessions(_)), "Ctrl-G r");
    }

    /// The point of the whole screen: choosing a row aims the next turn at that
    /// conversation, rather than printing it back at you.
    #[test]
    fn choosing_a_session_points_the_next_turn_at_it() {
        let mut app = holding(Workspace::Fleet, a_session("port the parser", Some("sess-abc")));

        assert_eq!(press(&mut app, KeyCode::Enter), Some(Action::NewThread));

        assert_eq!(app.resume, Resume::Session("sess-abc".into()));
        assert_eq!(app.session.as_deref(), Some("sess-abc"));
        assert_eq!(app.workspace, Workspace::Chat, "and it takes you there");
        assert_eq!(app.overlay, Overlay::None);
        let said = format!("{:?}", app.transcript);
        assert!(said.contains("port the parser"), "{said}");
    }

    /// A fork does not inherit its parent's harness session — `Store` says so
    /// in as many words — so a row can have no id to resume from. Starting a
    /// fresh session under the old title would be the one failure the person
    /// who came here to go back to something could not spot.
    #[test]
    fn choosing_a_session_the_harness_never_named_refuses_out_loud() {
        let mut app = holding(Workspace::Chat, a_session("a fresh fork", None));

        assert_eq!(press(&mut app, KeyCode::Enter), None);

        assert_eq!(app.resume, Resume::Fresh, "nothing was pointed anywhere");
        let said = format!("{:?}", app.transcript);
        assert!(said.contains("nothing to continue"), "{said}");
    }

    /// While the list is up it owns the keyboard, letters and all — otherwise
    /// typing the name of a thread would stop a run behind it.
    #[test]
    fn typing_in_the_session_list_narrows_it_rather_than_running_a_verb() {
        let mut app = holding(Workspace::Fleet, a_session("port the parser", Some("sess-abc")));
        app.agents = vec![agent_line("run-7", None)];
        app.reconcile();

        for key in ['s', 'p', 'o'] {
            assert_eq!(press(&mut app, KeyCode::Char(key)), None, "`{key}`");
        }

        let Overlay::Sessions(browser) = &app.overlay else {
            panic!("the list is still up: {:?}", app.overlay);
        };
        assert_eq!(
            browser.query, "spo",
            "the letters went into the filter, and `s` in particular did not \
             stop the run behind the list"
        );
    }

    #[test]
    fn escape_closes_the_session_list_and_leaves_the_screen_alone() {
        let mut app = holding(Workspace::Fleet, a_session("port the parser", Some("sess-abc")));

        assert_eq!(press(&mut app, KeyCode::Esc), None);

        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.workspace, Workspace::Fleet);
        assert_eq!(app.resume, Resume::Fresh);
    }

    /// The five letters must stay letters everywhere they are not verbs. `f`
    /// especially: it is the first letter of half the sentences typed into the
    /// box, and a fork triggered from the chat line would be unexplainable.
    #[test]
    fn the_conversation_keys_are_still_text_in_the_chat_box() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "fix the buggy version urgently, cheers");
        assert_eq!(app.input, "fix the buggy version urgently, cheers");
        assert_eq!(app.workspace, Workspace::Chat);
    }

    /// Reachability all the way to the database: the key produces the action,
    /// and the action's payload is one `sessions::apply` away from a sentence.
    /// This is the join the two halves are otherwise tested either side of.
    #[test]
    fn the_key_a_user_presses_and_the_store_it_lands_in_are_the_same_thread() {
        let store = store();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation");
        store
            .append_message(
                &conversation.id,
                jod_core::conversation::NewMessage::user("port the parser")
                    .from_run("run-7")
                    .at_seq(1),
            )
            .expect("a message from run-7");

        let mut app = on_fleet_with_a_run();
        let Some(Action::Sessions(request)) = press(&mut app, KeyCode::Char('b')) else {
            panic!("`b` on the fleet asks for the selected run's thread");
        };

        let said = sessions::apply(&store, &request, 0).join("\n");
        assert!(
            said.contains("port the parser"),
            "the run id under the cursor found its thread: {said}"
        );
    }

    // ---- background shells: /update, /jobs, /reload ----

    /// The console has to be able to update the binary it is running from.
    /// That is what makes an always-on TUI on a VPS maintainable at all: the
    /// alternative is quitting the thing you wanted to keep in order to
    /// replace it.
    #[test]
    fn update_is_a_command_and_a_background_job() {
        assert_eq!(
            command::parse("/update"),
            Some(command::Slash::Update { check: false })
        );
        assert_eq!(
            command::parse("/update check"),
            Some(command::Slash::Update { check: true })
        );
        assert_eq!(
            command::parse("/update --check"),
            Some(command::Slash::Update { check: true })
        );
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Update { check: false }),
            Some(Action::Update { check: false }),
            "/update hands the work back to the loop, which owns the job table"
        );
    }

    /// A version argument would be a minor/major move decided mid-session, in
    /// the console the move would replace. It is refused with the sentence
    /// that says where that decision does belong.
    #[test]
    fn update_refuses_a_version_rather_than_guessing_at_it() {
        assert!(matches!(
            command::parse("/update v2.0.0"),
            Some(command::Slash::Refused(_))
        ));
        assert!(matches!(
            command::parse("/upgrade v2.0.0"),
            Some(command::Slash::Refused(_))
        ));
    }

    /// `/upgrade` used to be a silent alias for `/update`, from before there
    /// was anything else for it to mean. There is now: at a shell the two
    /// words name two different acts — rebuild the newest patch from a
    /// checkout, or download the newest release — and a console where
    /// `/upgrade` quietly did the first would make one word mean two things
    /// depending on where it was typed.
    #[test]
    fn upgrade_is_its_own_command_and_not_an_alias_of_update() {
        assert_eq!(
            command::parse("/upgrade"),
            Some(command::Slash::Upgrade { check: false })
        );
        assert_eq!(
            command::parse("/upgrade check"),
            Some(command::Slash::Upgrade { check: true })
        );
        assert_eq!(
            command::parse("/upgrade --check"),
            Some(command::Slash::Upgrade { check: true })
        );
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Upgrade { check: false }),
            Some(Action::Upgrade { check: false }),
            "/upgrade hands the work back to the loop, which owns the job table"
        );
    }

    /// Both verbs write the same binaries, so they contend for the same job
    /// slot. An upgrade started while an update is mid-`cargo build` would
    /// have two processes renaming over the same files.
    #[test]
    fn an_upgrade_will_not_start_on_top_of_a_running_update() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.job_start("update", 1_000);

        start_take(&mut app, &tx, false, Take::Upgrade);
        assert_eq!(
            app.running_jobs(),
            1,
            "the upgrade queued a second installer over a running update"
        );
        let said = app.transcript.iter().rev().find_map(|e| match e {
            Entry::Notice(text) => Some(text.clone()),
            _ => None,
        });
        assert!(
            said.as_deref().is_some_and(|t| t.contains("already running")),
            "refusing has to say why: {said:?}"
        );
    }

    /// Backgrounding something and giving no way to look at it is asking to be
    /// trusted about work that is never shown.
    #[test]
    fn background_shells_are_reachable_by_command_and_from_the_menu() {
        assert_eq!(command::parse("/jobs"), Some(command::Slash::Jobs));
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut app, command::Slash::Jobs);
        assert_eq!(app.overlay, Overlay::Jobs, "/jobs opens the panel");

        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.overlay, Overlay::Jobs, "Ctrl-G j opens the panel");
        // Read-only, like the keymap: any key closes it.
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn a_job_records_its_last_line_and_how_it_ended() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let job = app.job_start("update", 1_000);
        assert_eq!(app.running_jobs(), 1);
        app.job_line(job, "→ Building v1.2.3");
        app.job_line(job, "   ");
        assert_eq!(
            app.jobs[job].last.as_deref(),
            Some("→ Building v1.2.3"),
            "a blank line must not erase what the job was last seen doing"
        );
        app.job_done(job, true, 4_000);
        assert_eq!(app.running_jobs(), 0);
        assert!(!app.jobs[job].is_running());
        assert_eq!(app.jobs[job].elapsed_ms(9_999), 3_000, "a finished job stops ageing");
        assert_eq!(app.jobs[job].mark(), "✓");
    }

    /// The one thing an update cannot do to itself. Asked rather than taken —
    /// and a keystroke that is not `y` leaves the console where it is.
    #[test]
    fn a_finished_update_offers_a_reload_and_takes_no_for_an_answer() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.overlay = Overlay::ConfirmReload;
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.overlay, Overlay::None);

        app.overlay = Overlay::ConfirmReload;
        assert_eq!(press(&mut app, KeyCode::Char('y')), Some(Action::Reload));
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn reload_is_also_a_command_of_its_own() {
        assert_eq!(command::parse("/reload"), Some(command::Slash::Reload));
        let mut app = app_on(HarnessKind::ClaudeCode);
        assert_eq!(
            apply_slash(&mut app, command::Slash::Reload),
            Some(Action::Reload)
        );
    }
    // ---- the decision rail ----

    fn a_card(id: i64, title: &str, blocking: bool, options: &[&str]) -> jod_core::cards::Card {
        jod_core::cards::Card {
            id,
            conversation_id: "conv".into(),
            work_id: None,
            run_id: None,
            kind: jod_core::cards::CardKind::Question,
            importance: jod_core::cards::Importance::Normal,
            blocking,
            status: jod_core::cards::Status::Open,
            delivery: jod_core::cards::Delivery::None,
            title: title.into(),
            body: String::new(),
            options: options.iter().map(|o| (*o).to_string()).collect(),
            chosen: None,
            answer: None,
            secret_name: None,
            secret_scope: None,
            source: jod_core::cards::Source::Mcp,
            created_at_ms: 0,
            updated_at_ms: 0,
            answered_at_ms: None,
            delivered_at_ms: None,
            dedupe_key: None,
        }
    }

    fn with_cards() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.cards = vec![
            a_card(1, "which port for the API?", true, &["8080", "3000"]),
            a_card(2, "chat DB: chose SQLite", false, &[]),
        ];
        app.reconcile_rail();
        app
    }

    fn last_notice(app: &App) -> String {
        match spoken(app).pop() {
            Some(text) => text,
            None => panic!("expected a notice, got {:?}", app.transcript.last()),
        }
    }

    /// Everything Jod has said in its own voice, wherever it landed.
    ///
    /// A notice raised off the chat screen becomes a flash instead of a
    /// transcript line — see `App::push` — so a test that reads only the
    /// transcript asserts which container the words went into rather than
    /// whether they were said at all. Ordered: the transcript is what was said
    /// earlier, and the flash is what was said just now.
    fn spoken(app: &App) -> Vec<String> {
        app.transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice(text) | Entry::Hint(text) => Some(text.clone()),
                _ => None,
            })
            .chain(app.flash.iter().flat_map(|f| f.lines.iter().cloned()))
            .collect()
    }

    /// The constraint E2.S3 states outright: the way into the rail must not cost
    /// the sentence you were typing. A chord cannot reach `App::insert`, which
    /// is the whole reason the rail's way in is one.
    #[test]
    fn the_rail_chord_focuses_without_touching_the_typed_line() {
        let mut app = with_cards();
        type_line(&mut app, "ship the parser");

        ctrl(&mut app, KeyCode::Char('n'));
        assert!(app.rail.shown && app.rail.focused);
        assert_eq!(app.rail.selected, Some(1), "on the most pressing card");
        assert_eq!(app.input, "ship the parser", "the sentence is untouched");

        // Stepping is the arrows' job now, and it costs the sentence nothing
        // either — the rail has the keyboard, so `j` never reaches the box.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.rail.selected, Some(2), "and the cursor steps on");
        assert_eq!(app.input, "ship the parser");
    }

    /// The key that opened it closes it. Before this the only way out was
    /// `Ctrl-R`, which the rail's own keybar never printed — so the rail was a
    /// thing you opened and then had to go looking for a way to leave.
    #[test]
    fn the_same_chord_opens_the_rail_and_puts_it_away() {
        let mut app = with_cards();
        type_line(&mut app, "half a sentence");

        ctrl(&mut app, KeyCode::Char('n'));
        assert!(app.rail.shown && app.rail.focused);

        ctrl(&mut app, KeyCode::Char('n'));
        assert!(!app.rail.shown, "the same key left it on screen");
        assert!(!app.rail.focused, "and left it holding the bare keys");
        assert_eq!(app.input, "half a sentence", "and cost the sentence");

        type_line(&mut app, "!");
        assert_eq!(app.input, "half a sentence!", "the letters are the chat's again");
    }

    /// Hiding the rail has to hand the keyboard back with it, or the bare keys
    /// stay the rail's with no rail on screen to explain them.
    #[test]
    fn alt_r_shows_and_hides_the_rail_and_takes_the_focus_with_it() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        assert!(app.rail.focused);

        ctrl(&mut app, KeyCode::Char('r'));
        assert!(!app.rail.shown);
        assert!(!app.rail.focused, "no rail, no rail keys");

        type_line(&mut app, "hello");
        assert_eq!(app.input, "hello", "the letters are the chat's again");
    }

    /// The focus is what makes the bare letters safe, and it has to be total:
    /// a letter that reached both would be a letter that did two things.
    #[test]
    fn letters_typed_at_a_focused_rail_do_not_reach_the_input_box() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        type_line(&mut app, "jk");
        assert_eq!(app.input, "");
    }

    /// `Esc` peels the rail's layers and closes it, with the line exactly as it
    /// was. The last layer used to stop at un-focusing, which left the rail on
    /// screen and the way to take it off screen unadvertised.
    #[test]
    fn esc_closes_the_rail_with_the_line_intact() {
        let mut app = with_cards();
        type_line(&mut app, "half a sentence");
        ctrl(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        assert!(app.rail.expanded);

        press(&mut app, KeyCode::Esc);
        assert!(!app.rail.expanded, "the expanded card first");
        press(&mut app, KeyCode::Esc);
        assert!(!app.rail.focused, "then the rail itself");
        assert!(!app.rail.shown, "and it leaves the screen with it");
        assert_eq!(app.input, "half a sentence");
    }

    /// A digit picks the option it is printed beside. The label on screen *is*
    /// the keystroke, so nobody counts rows to find out what `2` does.
    #[test]
    fn a_digit_in_the_rail_answers_the_option_it_names() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Char('2')),
            Some(Action::AnswerCard {
                id: 1,
                chosen: Some("3000".into()),
                answer: None,
            })
        );
    }

    /// A rail as one frame drew it, so a click can be aimed at a real row.
    fn drawn(app: &App, w: u16, h: u16) -> ui::RailHits {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut out = ui::Painted::default();
        terminal.draw(|f| out = ui::draw(f, app)).unwrap();
        out.rail
    }

    /// The gesture the pointer exists for. On a phone the rail is a panel under
    /// the chat, the bare keys belong to the composer, and reaching a card took
    /// a chord — on a keyboard that is not there. A tap has to do it.
    #[test]
    fn a_tap_on_a_card_opens_it_and_hands_the_rail_the_keyboard() {
        let mut app = with_cards();
        app.rail.shown = true;
        let hits = drawn(&app, 78, 30);
        let hit = *hits.cards.last().expect("a card was drawn");

        assert_eq!(on_click(&mut app, &hits, &ui::PanelHits::default(), hits.area.unwrap().x + 2, hit.top + 1), None);
        assert_eq!(app.rail.selected, Some(hit.id), "the card that was tapped");
        assert!(app.rail.expanded, "opened, not merely selected");
        assert!(app.rail.focused, "and the digits are the rail's now");
    }

    /// The option is the label on screen, whether it is pressed or tapped.
    #[test]
    fn a_tap_on_an_option_answers_the_card_with_it() {
        let mut app = with_cards();
        app.rail.shown = true;
        app.rail.focused = true;
        app.rail.expanded = true;
        app.rail.look_at(1);

        let hits = drawn(&app, 150, 40);
        let second = hits
            .options
            .iter()
            .find(|hit| hit.at == 1)
            .copied()
            .expect("the second option is drawn");
        assert_eq!(
            on_click(&mut app, &hits, &ui::PanelHits::default(), hits.area.unwrap().x + 4, second.row),
            Some(Action::AnswerCard {
                id: 1,
                chosen: Some("3000".into()),
                answer: None,
            }),
            "the same answer the digit gives"
        );
    }

    /// The rest of an expanded card is prose being read. A stray tap on it must
    /// not answer the card or put it away — that is the one mistake a pointer
    /// could introduce that the keyboard never could.
    #[test]
    fn a_tap_on_the_body_of_a_card_does_nothing_at_all() {
        let mut app = with_cards();
        app.rail.shown = true;
        app.rail.expanded = true;
        app.rail.look_at(1);

        let hits = drawn(&app, 150, 40);
        let area = hits.area.expect("a rail");
        // The row under the title, which is the card's own facts line — inside
        // the card, on nothing that can be pressed.
        assert_eq!(on_click(&mut app, &hits, &ui::PanelHits::default(), area.x + 4, area.y + 2), None);
        assert!(app.rail.expanded, "still open");
        assert_eq!(app.rail.selected, Some(1), "and still the same card");
    }

    /// The way back for somebody with no `Esc` key.
    #[test]
    fn a_tap_on_the_expanded_cards_title_puts_it_back_in_the_stack() {
        let mut app = with_cards();
        app.rail.shown = true;
        app.rail.expanded = true;
        app.rail.look_at(1);

        let hits = drawn(&app, 78, 30);
        let back = hits.back.expect("a way back");
        assert_eq!(on_click(&mut app, &hits, &ui::PanelHits::default(), hits.area.unwrap().x + 3, back), None);
        assert!(!app.rail.expanded, "back to the stack");
        assert_eq!(app.rail.selected, Some(1), "on the card that was open");
    }

    /// A click outside the rail is the transcript's business, and must not
    /// answer whatever card happens to share its row.
    #[test]
    fn a_click_outside_the_rail_is_not_the_rails() {
        let mut app = with_cards();
        app.rail.shown = true;
        let hits = drawn(&app, 150, 40);
        let before = app.rail.clone();
        assert_eq!(on_click(&mut app, &hits, &ui::PanelHits::default(), 140, 5), None);
        assert_eq!(app.rail, before, "the rail did not move");
    }

    /// The wheel over the stack walks it, and the window follows the cursor —
    /// which is how the cards past the fifth are reached at all.
    #[test]
    fn the_wheel_over_the_stack_walks_the_cards() {
        let mut app = with_cards();
        app.rail.shown = true;
        let hits = drawn(&app, 150, 40);
        assert_eq!(app.rail.selected, Some(1));

        on_rail_wheel(&mut app, &hits, 1);
        assert_eq!(app.rail.selected, Some(2), "down one card");
        on_rail_wheel(&mut app, &hits, 1);
        assert_eq!(app.rail.selected, Some(2), "and it stops at the end");
        on_rail_wheel(&mut app, &hits, -1);
        assert_eq!(app.rail.selected, Some(1));
    }

    /// The wheel over an expanded card scrolls the card, because that is the
    /// only way to read the part of it below the fold on a phone.
    #[test]
    fn the_wheel_over_an_expanded_card_scrolls_its_text() {
        let mut app = with_cards();
        app.cards[0].body = "a paragraph of context\n".repeat(20);
        app.rail.shown = true;
        app.rail.expanded = true;
        app.rail.look_at(1);

        let hits = drawn(&app, 78, 30);
        assert!(hits.past > 0, "the card is taller than the panel");
        on_rail_wheel(&mut app, &hits, 1);
        assert_eq!(app.rail.scroll, 1);
        on_rail_wheel(&mut app, &hits, -5);
        assert_eq!(app.rail.scroll, 0, "and it stops at the first line");
    }

    /// The offset belongs to the card under the cursor. Carried onto the next
    /// one, it would open that card halfway down.
    #[test]
    fn moving_off_a_scrolled_card_puts_the_next_one_at_its_first_line() {
        let mut app = with_cards();
        app.cards[0].body = "a paragraph of context\n".repeat(20);
        app.rail.shown = true;
        app.rail.expanded = true;
        app.rail.look_at(1);

        let hits = drawn(&app, 78, 30);
        on_rail_wheel(&mut app, &hits, 3);
        assert!(app.rail.scroll > 0);
        app.rail.look_at(2);
        assert_eq!(app.rail.scroll, 0);
    }

    /// A digit naming no option says so. Silence would leave the reader
    /// believing the answer went in — and the run is still blocked.
    #[test]
    fn a_digit_that_names_no_option_says_so_rather_than_doing_nothing() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        assert_eq!(press(&mut app, KeyCode::Char('7')), None);
        let said = last_notice(&app);
        assert!(said.contains("press 1–2"), "{said}");

        // ...and on a card with no options at all, it points at the prose key.
        app.rail.selected = Some(2);
        assert_eq!(press(&mut app, KeyCode::Char('1')), None);
        let said = last_notice(&app);
        assert!(said.contains("in prose"), "{said}");
    }

    /// No confirmation, unlike `x` on a list screen: dismissing is a state
    /// change and not a deletion, and a card that costs two keys to put down is
    /// a rail nobody clears.
    #[test]
    fn x_in_the_rail_dismisses_without_a_confirmation() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Some(Action::DismissCard(1))
        );
        assert_eq!(app.overlay, Overlay::None, "nothing to confirm");
    }

    /// The prose answer goes to the card the prompt was opened on, not to
    /// whatever the cursor is on when `⏎` lands — the rail re-queries
    /// underneath an overlay, and an answer on the wrong card wakes the wrong
    /// agent.
    #[test]
    fn a_prose_answer_lands_on_the_card_the_prompt_was_opened_on() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('a'));
        let Overlay::Prompt { intent, .. } = app.overlay.clone() else {
            panic!("`a` opens a prompt, got {:?}", app.overlay);
        };
        assert_eq!(intent, PromptIntent::AnswerCard(1));

        // The rail moves under the overlay, as a tick would move it.
        app.rail.selected = Some(2);
        for c in "8080".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AnswerCard {
                id: 1,
                chosen: None,
                answer: Some("8080".into()),
            }),
            "the id came from the prompt, not from the cursor"
        );
    }

    /// E2.S5, through the keys: the filter, the sort and both filters are held
    /// in state, so leaving the rail and coming back finds them where they were.
    #[test]
    fn the_rails_filter_and_sort_survive_leaving_it_and_coming_back() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('S'));
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "port");
        press(&mut app, KeyCode::Enter);
        let asked = app.rail.query(Some("conv".into()));

        // Put it away and bring it back. Deliberately not by `Esc`, which
        // clears the filter on purpose — it is the key that undoes one level,
        // and undoing the filter is exactly what it should do there.
        ctrl(&mut app, KeyCode::Char('n'));
        assert!(!app.rail.shown, "the chord did not put the rail away");
        ctrl(&mut app, KeyCode::Char('f'));
        assert_eq!(app.workspace, Workspace::Fleet);
        app.go(Workspace::Chat);
        ctrl(&mut app, KeyCode::Char('n'));

        assert_eq!(app.rail.query(Some("conv".into())), asked);
        assert_eq!(asked.text.as_deref(), Some("port"));
        assert_ne!(asked.sort, jod_core::cards::Sort::default());
        assert!(asked.kind.is_some());
        assert_ne!(asked.status, Some(jod_core::cards::Status::Open));
    }

    /// A `/` line being typed into owns the letters, or filtering for "stop"
    /// would sort, toggle and dismiss on the way past.
    #[test]
    fn the_rails_filter_line_owns_the_letters_while_it_is_being_typed() {
        let mut app = with_cards();
        ctrl(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('/'));
        type_line(&mut app, "sqlite");
        assert_eq!(app.rail.filter.as_deref(), Some("sqlite"));
        assert_eq!(app.rail.stack_now(), jod_core::cards::Status::Open);
        assert_eq!(app.input, "");
    }

    /// D2, end to end and against a real store: answering a card while a turn
    /// is in flight records the answer, *queues* the delivery, and touches
    /// nothing about the run. Ten answers during one turn all sit queued.
    #[test]
    fn answering_queues_the_answer_rather_than_interrupting_the_turn() {
        let s = store();
        let conversation = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let ids: Vec<i64> = (0..10)
            .map(|n| {
                s.raise_card(jod_core::cards::NewCard {
                    conversation_id: conversation.clone(),
                    title: format!("question {n}"),
                    ..Default::default()
                })
                .expect("a card")
                .id
            })
            .collect();

        for id in &ids {
            let said = answered_card(&s, *id, None, Some("yes"));
            assert!(said.contains("queued"), "{said}");
        }

        for id in &ids {
            let card = s.card(*id).unwrap().expect("the card");
            assert_eq!(card.status, jod_core::cards::Status::Answered);
            assert_eq!(
                card.delivery,
                jod_core::cards::Delivery::Queued,
                "answered is not delivered"
            );
        }
        assert_eq!(
            s.pending_for(&conversation).unwrap().len(),
            10,
            "ten answers are ten queued deliveries, waiting for one turn boundary"
        );
    }

    /// Dismissing queues nothing. A dismissal the agent heard would be
    /// indistinguishable from an answer, and it would act on a decision nobody
    /// made.
    #[test]
    fn dismissing_tells_the_agent_nothing_at_all() {
        let s = store();
        let conversation = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let card = s
            .raise_card(jod_core::cards::NewCard {
                conversation_id: conversation.clone(),
                title: "shall I rename the column?".into(),
                ..Default::default()
            })
            .expect("a card");
        s.dismiss_card(card.id).expect("dismissing");
        assert!(s.pending_for(&conversation).unwrap().is_empty());
    }

    /// A refusal from the store reaches the reader as a sentence rather than as
    /// silence — answering twice is refused, and the second press must say why
    /// rather than looking like a key that stopped working.
    #[test]
    fn a_refused_answer_says_why() {
        let s = store();
        let conversation = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let card = s
            .raise_card(jod_core::cards::NewCard {
                conversation_id: conversation,
                title: "which port?".into(),
                ..Default::default()
            })
            .expect("a card");
        answered_card(&s, card.id, None, Some("8080"));
        let again = answered_card(&s, card.id, None, Some("3000"));
        assert!(again.contains("not answered"), "{again}");
        assert!(again.contains("already"), "{again}");
    }

    // ---- interrupting a turn without killing the session ----

    /// Mid-turn, reached the way a turn is reached.
    ///
    /// `begin_turn` is the call both `orchestrate` and `send_turn` make, so
    /// this fixture cannot claim a state the real entry points do not produce.
    /// It used to set `watching`, `busy` and `turn_started_ms` by hand, which
    /// is the same three fields — but only by coincidence, and a fixture that
    /// agrees with the code by coincidence is how a feature broken in its
    /// default state stays green.
    fn mid_turn() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.agents = vec![running("run-1", "port the parser")];
        app.begin_turn("run-1", 0);
        app.session = Some("sess-abc".into());
        app.resume = Resume::Session("sess-abc".into());
        app
    }

    /// What the harness reports when a run it was told to stop stops: an
    /// ending, and an error, because a process killed by a signal is an error
    /// to anything reading an exit status.
    fn killed_ending() -> AgentEvent {
        AgentEvent::Finished {
            text: None,
            exit_code: None,
            is_error: true,
            usage: jod_core::Usage::default(),
        }
    }

    /// **E7.S1's stated check**: the session id is unchanged across the
    /// interruption. That is the whole difference between this and `Ctrl-X` —
    /// the run is one process of many in a conversation, so ending it ends the
    /// turn and leaves the conversation exactly where it was.
    #[test]
    fn escape_interrupts_the_turn_without_losing_the_session() {
        let mut app = mid_turn();
        let before = app.session.clone();

        assert_eq!(
            press(&mut app, KeyCode::Esc),
            Some(Action::Interrupt("run-1".into())),
            "Escape interrupts rather than backing out"
        );

        assert_eq!(app.session, before, "the harness conversation survives");
        assert_eq!(
            app.session.as_deref(),
            Some("sess-abc"),
            "and it is the same one, not a fresh id"
        );
        assert_eq!(
            app.resume,
            Resume::Session("sess-abc".into()),
            "so the next turn continues it rather than starting over"
        );
        assert!(!app.busy, "the turn is over");
        assert_eq!(app.turn_started_ms, None);
    }

    /// The correction is usually already half-written by the time you reach for
    /// Escape, so the key that stops the run must not also clear the line.
    #[test]
    fn interrupting_leaves_you_typing_the_correction() {
        let mut app = mid_turn();
        type_line(&mut app, "no, use the other parser");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input, "no, use the other parser");
        assert_eq!(app.cursor, app.input.len());
    }

    /// A partial turn dropped in silence leaves the transcript claiming the
    /// agent simply stopped talking, and the next reader cannot tell an
    /// interruption from a crash.
    #[test]
    fn an_interrupted_turn_is_recorded_as_what_it_was() {
        let mut app = mid_turn();
        app.now_ms = 9_000;
        press(&mut app, KeyCode::Esc);
        let said = format!("{:?}", app.transcript);
        assert!(said.contains("interrupted"), "{said}");
        assert!(
            !said.contains("failed: true"),
            "an interruption is not a failure: {said}"
        );
    }

    /// The second Escape is the old behaviour, unchanged: with nothing running
    /// it follows the tail again.
    #[test]
    fn a_second_escape_with_nothing_running_is_the_old_back_behaviour() {
        let mut app = mid_turn();
        press(&mut app, KeyCode::Esc);
        assert!(!app.busy);

        app.scroll_up(3, 10);
        assert_eq!(press(&mut app, KeyCode::Esc), None, "nothing left to stop");
        assert!(app.following(), "it follows the tail again");
    }

    /// **BUG-17.1.** Stopping is not instant: the signal goes out, the harness
    /// winds down, and its ending arrives seconds later. For those seconds the
    /// status bar said `working` with the elapsed counter stopped — a frozen
    /// clock, which is what a hung program looks like, so the reader presses
    /// the key again or reaches for Ctrl-C. The keypress has to be
    /// acknowledged by the keypress.
    #[test]
    fn the_status_bar_says_a_stop_is_under_way_from_the_keypress() {
        let mut app = mid_turn();
        app.now_ms = 10_000;
        assert!(app.activity().contains("working"), "{}", app.activity());

        press(&mut app, KeyCode::Esc);

        let said = app.activity();
        assert!(
            said.contains("interrupting"),
            "the keypress must be acknowledged at once, not when the kill \
             lands: {said}"
        );
        assert!(!said.contains("working"), "the turn is not still working: {said}");
    }

    /// **BUG-17.2.** The run's own ending arrives after the interrupt and reads
    /// as an error, because being killed is one. Written out as it comes, it
    /// put a red `✗ failed` under the green `✓ done · interrupted` for the same
    /// turn — two verdicts that disagree, at the moment the reader is checking
    /// whether their stop worked.
    #[test]
    fn an_interrupted_turn_is_not_also_reported_as_a_failure() {
        let mut app = mid_turn();
        app.now_ms = 10_000;
        press(&mut app, KeyCode::Esc);

        // What the loop feeds in when the run it just stopped ends.
        app.apply(&killed_ending());

        let said = format!("{:?}", app.transcript);
        assert!(said.contains("interrupted"), "the stop is still recorded: {said}");
        assert!(
            !said.contains("failed: true"),
            "a deliberate stop is not a failure: {said}"
        );
        assert!(!app.busy, "and the turn is over either way");
        assert_eq!(
            app.activity(),
            "ready",
            "the stop has landed, so the status bar stops saying it is under way"
        );
    }

    /// The other half of the same rule: a turn that failed on its own must
    /// still say so. Suppressing the ending whenever one had ever been asked
    /// for would hide real failures behind a key pressed minutes earlier.
    #[test]
    fn a_turn_that_failed_on_its_own_is_still_reported_failed() {
        let mut app = mid_turn();
        app.apply(&killed_ending());
        let said = format!("{:?}", app.transcript);
        assert!(
            said.contains("failed: true"),
            "nothing was interrupted, so this is a failure: {said}"
        );
    }

    /// `Ctrl-X` stops the same turn by the other key, so it must be written
    /// down the same way: its "stopped" notice and a red `✗ failed` under it
    /// are the same contradiction.
    #[test]
    fn a_turn_killed_outright_is_not_reported_as_a_failure_either() {
        let mut app = mid_turn();
        assert_eq!(
            ctrl(&mut app, KeyCode::Char('x')),
            Some(Action::Stop("run-1".into()))
        );
        app.apply(&killed_ending());
        let said = format!("{:?}", app.transcript);
        assert!(
            !said.contains("failed: true"),
            "a deliberate stop is not a failure: {said}"
        );
    }

    /// A stop that was never heard back about must not outlive the turn it was
    /// aimed at — the next turn is working, not interrupting.
    #[test]
    fn a_new_turn_clears_a_stop_that_was_never_answered() {
        let mut app = mid_turn();
        press(&mut app, KeyCode::Esc);
        assert!(app.interrupting.is_some());

        app.begin_turn("run-2", 0);
        app.now_ms = 3_000;
        assert!(app.activity().contains("working"), "{}", app.activity());
    }

    /// Escape with nothing in flight never becomes an interrupt, or every way
    /// out of a screen would try to kill something.
    #[test]
    fn escape_while_idle_interrupts_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.watching = Some("run-1".into());
        app.busy = false;
        assert_eq!(press(&mut app, KeyCode::Esc), None);
    }

    // ---- a repository's own commands reach the palette ----
    //
    // These are wiring tests on purpose. `commands.rs` works and `jod commands
    // ls` proves it; what was missing was any TUI file calling it. So each of
    // these fails if the *call* is deleted, not merely if the function behind
    // it breaks — deleting `repo_commands` from `completions`, or the
    // `repo_invocation` check in `on_chat_key`, turns them red.

    fn found(name: &str, kind: jod_core::commands::Kind, harness: HarnessKind) -> jod_core::commands::Discovered {
        jod_core::commands::Discovered {
            id: 1,
            root: PathBuf::from("/home/reljod/repo/Jod"),
            scope: jod_core::commands::Scope::Root,
            kind,
            name: name.into(),
            description: "open a pull request with evidence".into(),
            path: PathBuf::from(".claude/commands/create-pr.md"),
            harness: harness.id().to_string(),
            body: String::new(),
            scanned_at_ms: 0,
        }
    }

    fn with_repo_commands(harness: HarnessKind) -> App {
        let mut app = app_on(harness);
        app.discovered = vec![found("create-pr", jod_core::commands::Kind::Command, harness)];
        app
    }

    /// The digit the menu prints beside each letter is a route it answers to.
    ///
    /// Every row reads `schedules … or 4`, and pressing `4` there closed the
    /// menu and did nothing. The digit works from inside another workspace,
    /// which is the one place you are not while reading this menu — so the hint
    /// was printed only where it was false.
    #[test]
    fn the_which_key_menu_answers_to_the_digits_it_prints() {
        for ws in Workspace::MENU {
            let Some(digit) = ws.digit() else { continue };
            let mut app = app_on(HarnessKind::ClaudeCode);
            app.overlay = Overlay::WhichKey;
            press(&mut app, KeyCode::Char(digit));
            assert_eq!(
                app.workspace, ws,
                "`{digit}` is printed against {} and has to go there",
                ws.menu_name(),
            );
            assert!(matches!(app.overlay, Overlay::None), "and closes the menu");
        }
    }

    /// Escape puts the palette away, and typing brings it back.
    ///
    /// The popup is derived from the input rather than stored, so there was
    /// nothing for Escape to close: it fell through to `back()`, the list
    /// stayed up, and its own header — `Tab completes · ↑↓ choose` — offered no
    /// key that dismissed it. The only way out was to edit the line.
    #[test]
    fn escape_puts_the_command_palette_away_without_touching_the_line() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.input = "/mo".into();
        app.cursor = app.input.len();
        assert!(
            !command::completions(&app.input, &app).is_empty(),
            "the premise: `/mo` offers something",
        );

        press(&mut app, KeyCode::Esc);
        assert!(app.completions_dismissed, "Escape closed it");
        assert_eq!(app.input, "/mo", "and left what was typed alone");

        // The next keystroke is a new question about what to complete, so the
        // dismissal must not outlive the line it was about.
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.completions_dismissed, "typing asks again");
        assert_eq!(app.input, "/mod");
    }

    /// The gap this closes: the palette was a hardcoded enum, so a repository's
    /// own commands were invisible in the one place they would be used.
    #[test]
    fn a_repo_command_is_offered_in_the_palette_beside_jods_own() {
        let mut app = with_repo_commands(HarnessKind::ClaudeCode);
        app.input = "/c".into();
        let offered = command::completions(&app.input, &app);
        assert!(
            offered.iter().any(|c| c.line.starts_with("/create-pr")),
            "the repo's command is missing: {offered:?}"
        );
        assert!(
            offered.iter().any(|c| c.line.starts_with("/config")),
            "and Jod's own are still there: {offered:?}"
        );
    }

    /// `/review` from Jod and `/review` from the checkout are different things,
    /// so which one a row is has to be on the row.
    #[test]
    fn a_repo_command_says_where_it_came_from() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.discovered = vec![
            found("create-pr", jod_core::commands::Kind::Command, HarnessKind::ClaudeCode),
            found("write-spec", jod_core::commands::Kind::Skill, HarnessKind::ClaudeCode),
        ];
        app.input = "/".into();
        let offered = command::completions(&app.input, &app);
        let pr = offered
            .iter()
            .find(|c| c.line.starts_with("/create-pr"))
            .expect("the command");
        assert!(pr.hint.contains("repo"), "{}", pr.hint);
        assert!(pr.hint.contains("command"), "{}", pr.hint);
        assert!(
            pr.hint.contains("open a pull request"),
            "the description is real now: {}",
            pr.hint
        );
        let spec = offered
            .iter()
            .find(|c| c.line.starts_with("/write-spec"))
            .expect("the skill");
        assert!(spec.hint.contains("skill"), "{}", spec.hint);
    }

    /// D7's measurement, forwarded rather than reimplemented: Claude Code takes
    /// `/name` in the prompt.
    #[test]
    fn enter_on_a_repo_command_forwards_it_in_the_prompt_for_claude_code() {
        let mut app = with_repo_commands(HarnessKind::ClaudeCode);
        type_line(&mut app, "/create-pr the rail");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::RunCommand {
                prompt: "/create-pr the rail".into(),
                command: None,
            })
        );
        assert_eq!(app.input, "", "the line is consumed");
    }

    /// ...and OpenCode needs the name in a flag. Given `/name` in the message
    /// it passed the literal text to the model, which went hunting and answered
    /// correctly — right for the wrong reason, which is the failure this
    /// spelling prevents.
    #[test]
    fn enter_on_a_repo_command_uses_the_flag_for_opencode() {
        let mut app = with_repo_commands(HarnessKind::OpenCode);
        type_line(&mut app, "/create-pr the rail");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::RunCommand {
                prompt: "the rail".into(),
                command: Some("create-pr".into()),
            })
        );
    }

    /// A `.claude/commands/foo.md` has no OpenCode equivalent. The palette
    /// filters by harness at the query; this is the backstop, and it drops back
    /// to the ordinary prose path rather than forwarding a spelling that cannot
    /// resolve.
    #[test]
    fn a_command_from_another_harnesss_convention_is_not_forwarded() {
        let mut app = app_on(HarnessKind::OpenCode);
        app.discovered = vec![found(
            "create-pr",
            jod_core::commands::Kind::Command,
            HarnessKind::ClaudeCode,
        )];
        type_line(&mut app, "/create-pr");
        let action = press(&mut app, KeyCode::Enter);
        assert!(
            !matches!(action, Some(Action::RunCommand { .. })),
            "it must not be forwarded: {action:?}"
        );
    }

    /// Jod's own commands still win — a repo shipping a `/model` must not
    /// shadow the built-in, because the built-in is what the rest of the
    /// program documents.
    #[test]
    fn jods_own_commands_are_unaffected_by_a_repo_of_the_same_name() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.discovered = vec![found("model", jod_core::commands::Kind::Command, HarnessKind::ClaudeCode)];
        type_line(&mut app, "/model opus");
        let action = press(&mut app, KeyCode::Enter);
        assert!(
            !matches!(action, Some(Action::RunCommand { .. })),
            "Jod's own /model must still be Jod's: {action:?}"
        );
    }

    // ---- searching every transcript ----

    fn searching(hits: &[(&str, &str, &str)]) -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('/'));
        if let Overlay::Search { hits: into, .. } = &mut app.overlay {
            *into = hits
                .iter()
                .map(|(conversation, title, text)| data::Hit {
                    conversation_id: (*conversation).to_string(),
                    title: (*title).to_string(),
                    who: "agent".into(),
                    text: (*text).to_string(),
                })
                .collect();
        }
        app
    }

    #[test]
    fn the_menus_slash_opens_the_search() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        ctrl(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('/'));
        assert!(matches!(app.overlay, Overlay::Search { .. }));
    }

    /// The overlay owns every key, so a query never becomes a prompt.
    #[test]
    fn typing_into_the_search_never_reaches_the_chat_box() {
        let mut app = searching(&[]);
        for c in "parser".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let Overlay::Search { query, .. } = &app.overlay else {
            panic!("still open");
        };
        assert_eq!(query, "parser");
        assert_eq!(app.input, "");
    }

    /// Finding the hit is not the job; getting to it is.
    #[test]
    fn enter_opens_the_conversation_holding_the_hit() {
        let mut app = searching(&[
            ("conv-a", "the parser", "port the lexer"),
            ("conv-b", "the deploy", "fix the CI"),
        ]);
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Sessions(sessions::Request::Open("conv-b".into())))
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn the_arrows_stop_at_both_ends_of_the_hits() {
        let mut app = searching(&[("conv-a", "one", "first"), ("conv-b", "two", "second")]);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        let Overlay::Search { selected, .. } = &app.overlay else {
            panic!("open");
        };
        assert_eq!(*selected, 0, "it does not run off the top");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        let Overlay::Search { selected, .. } = &app.overlay else {
            panic!("open");
        };
        assert_eq!(*selected, 1, "nor off the bottom");
    }

    #[test]
    fn esc_closes_the_search_without_going_anywhere() {
        let mut app = searching(&[("conv-a", "the parser", "port the lexer")]);
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Enter with nothing found closes rather than looking dead.
    #[test]
    fn enter_with_no_hits_closes_the_search() {
        let mut app = searching(&[]);
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    // ---- the full-screen picker ----

    fn at_the_picker() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.conversation = Some("conv".into());
        app.overlay = Overlay::Picker(picker::Picker::new(
            PathBuf::from("/home/reljod/repo"),
            vec![".".into(), "Jod/cli".into(), "Jod/core".into()],
            false,
        ));
        app
    }

    /// Press the leader, then `d` — the route the picker moved to when the
    /// catalog took `Ctrl-P`.
    fn picker_chord(app: &mut App) {
        ctrl(app, KeyCode::Char('g'));
        press(app, KeyCode::Char('d'));
    }

    /// The key opens it against the directory you launched in, which is what
    /// E1.S4 means by "starting at the current directory".
    #[test]
    fn the_picker_key_opens_it_at_the_current_directory() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        picker_chord(&mut app);
        let Overlay::Picker(p) = &app.overlay else {
            panic!("Ctrl-G d opens the picker, got {:?}", app.overlay);
        };
        assert_eq!(
            p.base,
            std::env::current_dir().expect("a working directory")
        );
        assert!(
            p.rows.iter().any(|r| r.path == "."),
            "the directory you are in is the first offer"
        );
    }

    /// `/add-dir` is the folder-first name for the same picker, so it must
    /// land in exactly the state the key does — one picker, three doors.
    #[test]
    fn add_dir_opens_the_same_picker_the_key_does() {
        let mut typed = app_on(HarnessKind::ClaudeCode);
        apply_slash(&mut typed, command::parse("/add-dir").expect("parses"));
        let mut chorded = app_on(HarnessKind::ClaudeCode);
        picker_chord(&mut chorded);
        assert_eq!(typed.overlay, chorded.overlay);
    }

    /// The argument is a *base*, and this is the capability that did not exist
    /// before: a console launched inside one repository can be pointed at a
    /// tree that has nothing to do with it.
    #[test]
    fn add_dir_with_a_path_walks_that_tree_rather_than_the_launch_directory() {
        let base = std::env::temp_dir().join(format!("jod-add-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("notes")).expect("a fixture tree");

        let mut app = app_on(HarnessKind::ClaudeCode);
        let line = format!("/add-dir {}", base.display());
        apply_slash(&mut app, command::parse(&line).expect("parses"));

        let Overlay::Picker(p) = &app.overlay else {
            panic!("/add-dir <path> opens the picker, got {:?}", app.overlay);
        };
        assert_eq!(p.base, std::fs::canonicalize(&base).expect("a real path"));
        assert_ne!(
            p.base,
            std::env::current_dir().expect("a working directory"),
            "the point of the argument is to leave where you are"
        );
        assert!(
            p.entries.contains(&"notes".to_string()),
            "and to walk the named tree: {:?}",
            p.entries
        );
        // `.` is still the first row, so "this exact folder" costs one `⏎`.
        assert_eq!(p.rows[0].path, ".");
        assert_eq!(p.chosen().as_deref(), Some(p.base.as_path()));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A name that is not a directory is said out loud. An empty picker would
    /// read as "there is nothing here" when the truth is "that is not a
    /// place", and the next keystroke would look like it might help.
    #[test]
    fn add_dir_somewhere_that_does_not_exist_says_so_and_opens_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        apply_slash(
            &mut app,
            command::parse("/add-dir /no/such/folder/anywhere").expect("parses"),
        );
        assert_eq!(app.overlay, Overlay::None);
        assert!(
            last_notice(&app).contains("not a directory"),
            "got {:?}",
            last_notice(&app)
        );
    }

    // ---- the catalog, from inside the console ----

    /// What is actually on the screen, so a test can assert on the panel
    /// rather than on the field behind it.
    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                ui::draw(f, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A directory to catalog, named after the test so two cannot collide.
    fn a_checkout(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("jod-project-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a fixture checkout");
        // Canonical, because `add_project` normalises what it stores and the
        // temp directory is a symlink on macOS.
        std::fs::canonicalize(&path).expect("a real path")
    }

    /// BUG-5. The capability existed in `core` and in `jod project add`, and
    /// the console could reach neither: the catalog that resolves an
    /// unqualified instruction could only be filled from a second terminal.
    ///
    /// Driven from a default `App` and a real store, through `parse` — no
    /// field set by hand, and the panel opened by the key that opens it —
    /// because the state going non-empty is only half of "it visibly worked".
    #[tokio::test]
    async fn a_project_typed_into_the_console_reaches_the_catalog_and_the_panel() {
        let checkout = a_checkout("tetris");
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();

        // The panel a fresh console has, opened the way a user opens it.
        assert!(!app.panel, "a fresh console has the panel shut");
        press(&mut app, KeyCode::BackTab);
        assert!(app.projects.is_empty(), "and an empty catalog");

        let line = format!("/project add {}", checkout.display());
        let action = apply_slash(
            &mut app,
            command::parse(&line).expect("/project add parses"),
        )
        .expect("/project add is a store action, not a screen one");
        perform(&jod, &mut app, &options(), &mut thread, action).await;

        assert_eq!(
            app.projects.len(),
            1,
            "the catalog is still empty after /project add"
        );
        let name = checkout.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(app.projects[0].name, name);
        assert_eq!(app.projects[0].path, checkout);

        // And it is on the screen, not merely in the struct.
        let after = screen(&app, 100, 30);
        assert!(
            after.contains(&name),
            "the panel does not show it:\n{after}"
        );

        // The store is the system of record, so it survives this console.
        let listed = jod.store().unwrap().projects(false).unwrap();
        assert_eq!(listed.len(), 1, "and it was written down");

        let _ = std::fs::remove_dir_all(&checkout);
    }

    /// The empty state has to name its own remedy, the way the roots one does.
    /// `nothing set` named none, and there was none to name.
    #[tokio::test]
    async fn the_empty_catalog_says_how_to_fill_itself() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        press(&mut app, KeyCode::BackTab);
        assert!(app.projects.is_empty());

        // Expanded, which is how a fresh console draws it...
        assert!(app.projects_open);
        let open = screen(&app, 100, 30);
        assert!(
            open.contains("/project add"),
            "the expanded empty state names no remedy:\n{open}"
        );

        // ...and collapsed, which is two keypresses away — the first takes the
        // keyboard, the second puts the box away — and used to say only
        // `nothing set`.
        ctrl(&mut app, KeyCode::Char('p'));
        ctrl(&mut app, KeyCode::Char('p'));
        assert!(!app.projects_open);
        let shut = screen(&app, 100, 30);
        assert!(
            shut.contains("/project add"),
            "the collapsed empty state names no remedy:\n{shut}"
        );
    }

    // ---- moving a cursor through the catalog ----

    /// A catalogued project, with a manager unless one is asked for.
    fn project(name: &str, manager: Option<&str>) -> jod_core::projects::Project {
        jod_core::projects::Project {
            id: name.into(),
            name: name.into(),
            path: PathBuf::from(format!("/home/reljod/repo/{name}")),
            remote: None,
            aliases: Vec::new(),
            state: jod_core::projects::State::Active,
            colour: "cyan".into(),
            notes: String::new(),
            created_at_ms: 0,
            last_touched_ms: 0,
            manager_conversation_id: manager.map(str::to_string),
        }
    }

    /// A console with a catalog in the panel and nothing focused yet.
    fn with_projects() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.projects = vec![
            project("tetris", Some("conv-tetris")),
            project("zephyr", None),
        ];
        app.current_project = Some(app::Current {
            id: "tetris".into(),
            name: "tetris".into(),
            how: jod_core::projects::How::Inferred,
        });
        app
    }

    /// The complaint this answers, word for word: *I cannot navigate to the
    /// panel.* The catalog had been drawn on the panel since it was added and
    /// there was no way to put a cursor in it.
    #[test]
    fn the_projects_chord_takes_the_keyboard_and_the_arrows_move_the_cursor() {
        let mut app = with_projects();
        type_line(&mut app, "half a sentence");

        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.panel && app.projects_open && app.panel_focused);
        assert_eq!(
            app.selected_project().map(|p| p.name.clone()),
            Some("tetris".into()),
            "the cursor starts on the project this conversation is about"
        );
        assert_eq!(app.input, "half a sentence", "the chord cost the sentence");

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_project().map(|p| p.name.clone()), Some("zephyr".into()));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected_project().map(|p| p.name.clone()), Some("tetris".into()));
        assert_eq!(app.input, "half a sentence", "and neither did the arrows");
    }

    /// The cursor stops at both ends. Arrows that wrapped would read as the
    /// list having jumped.
    #[test]
    fn the_catalog_cursor_stops_at_both_ends() {
        let mut app = with_projects();
        ctrl(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected_project().map(|p| p.name.clone()), Some("tetris".into()));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected_project().map(|p| p.name.clone()), Some("zephyr".into()));
    }

    /// `⏎` goes into the project's manager — the conversation that owns that
    /// repository's work, and the reason a project row is worth pointing at
    /// rather than merely reading.
    #[test]
    fn enter_on_a_project_goes_into_its_manager() {
        let mut app = with_projects();
        ctrl(&mut app, KeyCode::Char('p'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::EnterManager("conv-tetris".into()))
        );
    }

    /// A project catalogued before managers existed has none. Saying so is the
    /// difference between a key that is not applicable here and a key that is
    /// broken — and from the outside those look identical.
    #[test]
    fn enter_on_a_project_with_no_manager_says_why_nothing_happened() {
        let mut app = with_projects();
        ctrl(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(
            last_notice(&app).contains("no manager"),
            "got {:?}",
            last_notice(&app)
        );
    }

    /// The focus is what makes the bare letters safe, and it has to be total.
    #[test]
    fn letters_typed_at_a_focused_catalog_do_not_reach_the_input_box() {
        let mut app = with_projects();
        ctrl(&mut app, KeyCode::Char('p'));
        type_line(&mut app, "jk");
        assert_eq!(app.input, "");
    }

    /// `Esc` puts the catalog away and gives the keyboard back with the typed
    /// line exactly as it was — the same contract the rail's `Esc` has.
    #[test]
    fn esc_closes_the_catalog_with_the_line_intact() {
        let mut app = with_projects();
        type_line(&mut app, "half a sentence");
        ctrl(&mut app, KeyCode::Char('p'));

        press(&mut app, KeyCode::Esc);
        assert!(!app.panel_focused);
        assert!(!app.projects_open);
        assert!(app.panel, "Esc took the whole panel with it");
        assert_eq!(app.input, "half a sentence");

        type_line(&mut app, "!");
        assert_eq!(app.input, "half a sentence!", "the letters are the chat's again");
    }

    /// Two things cannot hold the bare keys at once. The router checks the rail
    /// first, so a rail left focused would swallow every key meant for the
    /// catalog and the catalog would look inert.
    #[test]
    fn the_rail_and_the_catalog_never_hold_the_keyboard_at_once() {
        let mut app = with_projects();
        app.cards = vec![a_card(1, "which port for the API?", true, &["8080"])];
        app.reconcile_rail();

        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.panel_focused);
        ctrl(&mut app, KeyCode::Char('n'));
        assert!(app.rail.focused);
        assert!(!app.panel_focused, "the rail took the keys and the catalog kept them");

        ctrl(&mut app, KeyCode::Char('p'));
        assert!(app.panel_focused);
        assert!(!app.rail.focused, "the catalog took the keys and the rail kept them");
    }

    /// A tap on a project row puts the cursor on it and stops there. Entering a
    /// manager rebinds the chat box to another conversation, and a stray click
    /// that moved the sentence you were typing into a different repository is
    /// exactly the mistake the panel exists to prevent.
    #[test]
    fn a_click_on_a_project_selects_it_without_entering_it() {
        let mut app = with_projects();
        app.panel = true;
        let hits = drawn_panel(&app, 140, 30);
        let area = hits.catalog.expect("the catalog was drawn");
        let row = hits
            .projects
            .iter()
            .find(|h| h.id == "zephyr")
            .expect("zephyr was drawn")
            .row;

        assert_eq!(
            on_click(&mut app, &ui::RailHits::default(), &hits, area.x + 4, row),
            None,
            "a click entered a conversation on its own"
        );
        assert!(app.panel_focused, "a click did not hand over the keyboard");
        assert_eq!(app.project_selected.as_deref(), Some("zephyr"));
    }

    /// ...and then `⏎` commits, which is the second half of "select, then act".
    #[test]
    fn a_click_then_enter_opens_the_project_that_was_clicked() {
        let mut app = with_projects();
        app.panel = true;
        let hits = drawn_panel(&app, 140, 30);
        let area = hits.catalog.expect("the catalog was drawn");
        let row = hits.projects[0].row;
        on_click(&mut app, &ui::RailHits::default(), &hits, area.x + 4, row);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::EnterManager("conv-tetris".into()))
        );
    }

    /// A cursor left on a project another session untracked would make `⏎` open
    /// nothing and say nothing, which reads as a broken key.
    #[test]
    fn a_cursor_on_a_project_that_has_gone_moves_to_one_that_has_not() {
        let mut app = with_projects();
        ctrl(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_selected.as_deref(), Some("zephyr"));

        app.projects.retain(|p| p.name != "zephyr");
        app.reconcile_catalog();
        assert_eq!(app.project_selected.as_deref(), Some("tetris"));
    }

    /// The catalog as one frame drew it, so a click can be aimed at a real row.
    fn drawn_panel(app: &App, w: u16, h: u16) -> ui::PanelHits {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut out = ui::Painted::default();
        terminal.draw(|f| out = ui::draw(f, app)).unwrap();
        out.panel
    }

    /// `/project` with nothing after it lists, and brings the box it is
    /// listing into with it — on a fresh session the transcript is not on
    /// screen at all, so a notice alone would be a command with no visible
    /// answer.
    #[tokio::test]
    async fn listing_the_catalog_brings_the_panel_with_it() {
        let checkout = a_checkout("listed");
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        let mut thread = Thread::default();

        let empty = apply_slash(&mut app, command::parse("/project").expect("parses"))
            .expect("/project lists");
        perform(&jod, &mut app, &options(), &mut thread, empty).await;
        assert!(app.panel, "listing an empty catalog still shows the box");
        assert!(
            last_notice(&app).contains("/project add"),
            "got {:?}",
            last_notice(&app)
        );

        let line = format!("/project add {}", checkout.display());
        let add = apply_slash(&mut app, command::parse(&line).expect("parses")).expect("adds");
        perform(&jod, &mut app, &options(), &mut thread, add).await;

        let listed = apply_slash(&mut app, command::parse("/project ls").expect("parses"))
            .expect("/project ls lists");
        perform(&jod, &mut app, &options(), &mut thread, listed).await;
        let name = checkout.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            last_notice(&app).contains(&name),
            "the listing does not name it: {:?}",
            last_notice(&app)
        );

        let _ = std::fs::remove_dir_all(&checkout);
    }

    /// A typo must not become a row. The catalog is matched against later, so
    /// a project pointing nowhere is a mention that resolves to nothing —
    /// worse than no project, because it looks like it worked.
    #[test]
    fn cataloguing_somewhere_that_does_not_exist_says_so_and_stores_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        let action = apply_slash(
            &mut app,
            command::parse("/project add /no/such/checkout/anywhere").expect("parses"),
        );
        assert!(action.is_none(), "a typo must not reach the store");
        assert!(
            last_notice(&app).contains("not a directory"),
            "got {:?}",
            last_notice(&app)
        );
    }

    /// The same four keys as the `@` popup, which is the whole claim of "one
    /// picker at two sizes".
    #[test]
    fn the_picker_narrows_on_every_keystroke_and_moves_with_the_arrows() {
        let mut app = at_the_picker();
        for c in "cli".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let Overlay::Picker(p) = &app.overlay else {
            panic!("still up");
        };
        assert_eq!(p.query, "cli");
        assert_eq!(p.rows[0].path, "Jod/cli", "{:?}", p.rows);
        assert_eq!(app.input, "", "and none of it reached the chat box");
    }

    #[test]
    fn enter_adds_the_highlighted_directory_as_a_read_only_root() {
        let mut app = at_the_picker();
        for c in "core".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AddRoot(PathBuf::from("/home/reljod/repo/Jod/core")))
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Escape leaves nothing behind — the same promise the popup makes.
    #[test]
    fn escape_closes_the_picker_without_adding_anything() {
        let mut app = at_the_picker();
        for c in "cli".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.input, "");
    }

    /// Nothing matched means nothing to add, and `⏎` still closes — an enter
    /// that appeared dead would be worse than one that does nothing visible.
    #[test]
    fn enter_with_no_match_closes_without_adding() {
        let mut app = at_the_picker();
        for c in "zzzz".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    // ---- the secret card ----
    //
    // These assert the *terminal's* half of E3.S4: that a credential never
    // becomes part of the UI. The storage half — that the value reaches the
    // file and appears nowhere in the database — is core's, and is asserted
    // against a real `JOD_HOME` in `core/src/secrets.rs` and
    // `supervisor/tests/secrets_never_reach_the_record.rs`. Repeating it here
    // would mean setting `JOD_HOME` from a test thread that runs in parallel
    // with every other test in this binary, which is a race, and a racy
    // security test is worse than none: it goes green for the wrong reason.

    fn secret_card(id: i64, name: &str) -> jod_core::cards::Card {
        let mut card = a_card(id, "a credential is needed", true, &[]);
        card.kind = jod_core::cards::CardKind::Secret;
        card.secret_name = Some(name.into());
        card.secret_scope = Some("work".into());
        card
    }

    fn at_a_secret_card() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.cards = vec![secret_card(9, "GITHUB_TOKEN")];
        app.reconcile_rail();
        ctrl(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('a'));
        app
    }

    /// `Overlay::Prompt` echoes what is typed and hands it on as an ordinary
    /// `String`. Correct for a schedule's name, disqualifying for a token.
    #[test]
    fn a_secret_card_opens_a_masked_field_rather_than_the_ordinary_prompt() {
        let app = at_a_secret_card();
        match &app.overlay {
            Overlay::Secret { card, name, scope, .. } => {
                assert_eq!(*card, 9);
                assert_eq!(name, "GITHUB_TOKEN");
                assert_eq!(*scope, jod_core::secrets::Scope::Work);
            }
            other => panic!("a secret card must not open a plain prompt: {other:?}"),
        }
    }

    /// The copies people forget about: the input line, the transcript, and the
    /// `↑` history. A credential in any of them can be sent to an agent by
    /// pressing enter twice.
    #[test]
    fn the_typed_credential_reaches_no_part_of_the_ui() {
        let mut app = at_a_secret_card();
        for c in "sk-live-abcdef123456".chars() {
            press(&mut app, KeyCode::Char(c));
        }

        assert_eq!(app.input, "", "not the input line");
        assert!(app.history.is_empty(), "not the recall history");
        let transcript = format!("{:?}", app.transcript);
        assert!(!transcript.contains("sk-live"), "not the transcript");
        // And not a `{:?}` of the overlay, which is one stray diagnostic away
        // from a log file.
        let printed = format!("{:?}", app.overlay);
        assert!(!printed.contains("sk-live"), "not a Debug rendering: {printed}");
        assert!(printed.contains("20 chars"), "only its length: {printed}");
    }

    /// Enter moves the value into the action and leaves the overlay empty, so
    /// no frame drawn afterwards has anything left to draw.
    #[test]
    fn storing_moves_the_value_out_and_closes_the_field() {
        let mut app = at_a_secret_card();
        for c in "sk-live-abcdef123456".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let Some(Action::PutSecret { card, name, scope, value, .. }) =
            press(&mut app, KeyCode::Enter)
        else {
            panic!("⏎ stores the credential");
        };
        assert_eq!(card, 9);
        assert_eq!(name, "GITHUB_TOKEN");
        assert_eq!(scope, jod_core::secrets::Scope::Work);
        assert_eq!(value.reveal(), "sk-live-abcdef123456");
        assert_eq!(app.overlay, Overlay::None, "the field is gone");
        // The action itself must not print the value either — it is the thing
        // that travels furthest from here.
        let printed = format!("{:?}", Action::PutSecret {
            card,
            name: "GITHUB_TOKEN".into(),
            scope,
            scope_id: String::new(),
            value,
        });
        assert!(!printed.contains("sk-live"), "{printed}");
    }

    /// An empty field is refused rather than stored: `put_secret` would reject
    /// it anyway, but a card answered with nothing looks answered while the run
    /// stays blocked.
    #[test]
    fn an_empty_credential_is_refused_rather_than_stored() {
        let mut app = at_a_secret_card();
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(matches!(app.overlay, Overlay::Secret { .. }), "the field stays up");
    }

    #[test]
    fn escape_discards_the_credential_and_keeps_no_copy() {
        let mut app = at_a_secret_card();
        for c in "sk-live-abc".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.input, "");
        let whole = format!("{:?} {:?}", app.overlay, app.transcript);
        assert!(!whole.contains("sk-live"), "{whole}");
    }

    #[test]
    fn backspace_shortens_the_credential_rather_than_leaking_it() {
        let mut app = at_a_secret_card();
        for c in "abcd".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Backspace);
        let Overlay::Secret { value, .. } = &app.overlay else {
            panic!("still collecting");
        };
        assert_eq!(value.len(), 3);
    }

    /// The sentence the *model* reads. `Card::answer_body` turns a card's
    /// answer into the delivery, so whatever is stored as the answer is what
    /// the agent is told — which is why it is a name and a scope and nothing
    /// that could reconstruct a value.
    #[test]
    fn what_the_agent_is_told_about_a_secret_is_a_name_and_a_scope() {
        let s = store();
        let conversation = s
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let card = s
            .raise_card(jod_core::cards::NewCard {
                conversation_id: conversation.clone(),
                kind: Some(jod_core::cards::CardKind::Secret),
                title: "GITHUB_TOKEN is missing".into(),
                secret_name: Some("GITHUB_TOKEN".into()),
                secret_scope: Some("work".into()),
                ..Default::default()
            })
            .expect("a card");

        // Exactly what `stored_secret` writes once the value is safely down.
        let answered = s
            .answer_card(
                card.id,
                None,
                Some(&secret::stored_summary(
                    "GITHUB_TOKEN",
                    jod_core::secrets::Scope::Work,
                )),
            )
            .expect("answering");

        let told = answered.answer_body();
        assert!(told.contains("GITHUB_TOKEN"), "the name, so it knows what to reach for");
        assert!(told.contains("work scope"), "{told}");
        assert!(!told.contains('•'), "not even a masked value: {told}");

        // And the queued delivery — the thing that actually reaches a prompt —
        // carries the same words and no others.
        let pending = s.pending_for(&conversation).expect("queued");
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0].body.contains("stored GITHUB_TOKEN"),
            "{}",
            pending[0].body
        );
    }

    // ---- where the console is standing ----

    /// A console opened inside a repository already knows where it is — every
    /// turn's harness process starts there — and until this ran, the one part
    /// of the program that asks *which directories may I search* did not. `@`
    /// in a fresh session said "no folder to search" about the repository you
    /// were standing in.
    #[tokio::test]
    async fn the_directory_the_console_was_opened_in_becomes_a_root() {
        let store = store();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let jod = jod_with(store);

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.conversation = Some(conversation.clone());
        app.cwd = std::env::current_dir().expect("a working directory");
        let mut granted = HashSet::new();
        ensure_launch_root(&jod, &mut app, &mut granted);

        let store = jod.store().expect("the store");
        let roots = store.roots(&conversation).expect("roots");
        assert_eq!(roots.len(), 1, "{roots:?}");
        // Against the normalised form, because `add_root` canonicalises and the
        // path that comes back is the one every later match is made against.
        assert_eq!(roots[0].path, jod_core::roots::normalise(&app.cwd));
        assert!(!roots[0].writable, "read-only, like every root Jod adds");
        assert!(
            app.transcript.is_empty(),
            "and silent about it: {:?}",
            app.transcript
        );
    }

    /// The case the unit tests above would have missed and a real launch found:
    /// a machine where nothing has ever run has no conversation for the grant
    /// to land on, so the console opens the one it already falls back to. Left
    /// out, `jod tui` on a fresh install ran for minutes with an empty
    /// `conversations` table and `@` still saying there was nothing to search.
    #[tokio::test]
    async fn a_console_on_a_fresh_machine_opens_the_main_chat_to_have_somewhere_to_put_it() {
        let jod = jod_with(store());
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.cwd = std::env::current_dir().expect("a working directory");
        // As `bind_rail` leaves it when there is no pinned chat to find.
        app.conversation = None;
        let mut granted = HashSet::new();
        ensure_launch_root(&jod, &mut app, &mut granted);

        let store = jod.store().expect("the store");
        let conversation = app.conversation.clone().expect("a conversation to talk into");
        assert_eq!(
            store.pinned_conversation().expect("the pinned chat"),
            Some(conversation.clone()),
            "the main chat, not a loose one nothing else will find"
        );
        let roots = store.roots(&conversation).expect("roots");
        assert_eq!(roots.len(), 1, "{roots:?}");
        assert_eq!(roots[0].path, jod_core::roots::normalise(&app.cwd));
    }

    /// The half that makes it a grant rather than a policy. `add_root` is
    /// idempotent, so re-adding on the next tick would cost nothing and undo
    /// `/root remove` four times a second — a console that puts back what you
    /// took away is worse than one that never offered the directory.
    #[tokio::test]
    async fn removing_the_launch_directory_makes_it_stay_removed() {
        let store = store();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let jod = jod_with(store);

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.conversation = Some(conversation.clone());
        app.cwd = std::env::current_dir().expect("a working directory");
        let mut granted = HashSet::new();
        ensure_launch_root(&jod, &mut app, &mut granted);

        let store = jod.store().expect("the store");
        assert!(
            store
                .remove_root(&conversation, &app.cwd)
                .expect("removing what was just added"),
            "it was there to remove"
        );

        // Every remaining tick of the session, as the loop would run them.
        for _ in 0..8 {
            ensure_launch_root(&jod, &mut app, &mut granted);
        }
        assert!(
            store.roots(&conversation).expect("roots").is_empty(),
            "the removal holds for the rest of the session"
        );
    }

    /// The conversation on screen changes — `/new`, `/resume`, a harness
    /// switch — and each one that arrives is a conversation that has never been
    /// told where this console is.
    #[tokio::test]
    async fn a_conversation_bound_later_is_told_where_the_console_is_too() {
        let store = store();
        let first = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let second = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("another conversation")
            .id;
        let jod = jod_with(store);

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.cwd = std::env::current_dir().expect("a working directory");
        let mut granted = HashSet::new();

        app.conversation = Some(first.clone());
        ensure_launch_root(&jod, &mut app, &mut granted);
        app.conversation = Some(second.clone());
        ensure_launch_root(&jod, &mut app, &mut granted);

        let store = jod.store().expect("the store");
        for conversation in [&first, &second] {
            assert_eq!(
                store.roots(conversation).expect("roots").len(),
                1,
                "{conversation} was left without the directory it is being typed into"
            );
        }
    }

    /// A fixture with no launch directory is not a session standing anywhere,
    /// and `""` as a root reads as `/` to anything that joins a path onto it —
    /// the same trap `ensure_inherited_root` refuses.
    #[tokio::test]
    async fn a_session_with_no_launch_directory_grants_nothing() {
        let store = store();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let jod = jod_with(store);

        let mut app = app_on(HarnessKind::ClaudeCode);
        app.conversation = Some(conversation.clone());
        let mut granted = HashSet::new();
        ensure_launch_root(&jod, &mut app, &mut granted);

        let roots = jod
            .store()
            .expect("the store")
            .roots(&conversation)
            .expect("roots");
        assert!(roots.is_empty(), "{roots:?}");
        assert!(
            granted.is_empty(),
            "and nothing was written down as granted"
        );
    }

    // ---- the `@` picker ----

    fn with_a_root() -> App {
        let mut app = app_on(HarnessKind::ClaudeCode);
        app.roots = vec![jod_core::roots::Root {
            id: 1,
            conversation_id: "conv".into(),
            path: PathBuf::from("/home/reljod/repo/jod"),
            writable: false,
            position: 0,
            origin: jod_core::roots::Origin::Human,
            added_at_ms: 0,
        }];
        app.candidates = vec![Arc::new(vec![
            "cli/src/tui/mod.rs".to_string(),
            "core/src/rank.rs".to_string(),
        ])];
        app
    }

    #[test]
    fn typing_at_opens_the_picker_and_every_keystroke_re_ranks_it() {
        let mut app = with_a_root();
        type_line(&mut app, "look at @");
        assert!(app.mention.is_some(), "the sign opens it");

        type_line(&mut app, "rank");
        let popup = app.mention.as_ref().expect("still open");
        assert_eq!(popup.query, "rank");
        assert_eq!(
            popup.rows[0].path, "core/src/rank.rs",
            "ranked, not filtered"
        );
        assert!(
            !popup.rows[0].positions.is_empty(),
            "and it says which characters matched"
        );
    }

    /// D1's fourth requirement, and the one people notice: a picker that wipes
    /// the line when you change your mind is a picker you stop opening.
    #[test]
    fn esc_closes_the_picker_and_leaves_what_you_typed_alone() {
        let mut app = with_a_root();
        type_line(&mut app, "look at @rank");
        press(&mut app, KeyCode::Esc);
        assert!(app.mention.is_none());
        assert_eq!(app.input, "look at @rank");
    }

    #[test]
    fn enter_puts_the_chosen_path_into_the_line() {
        let mut app = with_a_root();
        type_line(&mut app, "read @rank");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, "read @core/src/rank.rs ");
        assert_eq!(app.cursor, app.input.len());
        assert!(app.mention.is_none());
    }

    /// With several roots the inserted path says which one it is from, because
    /// `src/main.rs` names two files when a session can see two repositories.
    #[test]
    fn several_roots_insert_a_root_qualified_path() {
        let mut app = with_a_root();
        app.roots.push(jod_core::roots::Root {
            id: 2,
            conversation_id: "conv".into(),
            path: PathBuf::from("/home/reljod/repo/notes"),
            writable: false,
            position: 1,
            origin: jod_core::roots::Origin::Human,
            added_at_ms: 0,
        });
        app.candidates.push(Arc::new(vec!["rank.md".to_string()]));
        type_line(&mut app, "read @rank.md");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, "read @notes/rank.md ");
    }

    /// E1.S3 in one line: with no roots it says so and accepts nothing. `⏎`
    /// must not fall through and send the half-written line either.
    #[test]
    fn with_no_roots_the_picker_accepts_nothing_and_sends_nothing() {
        let mut app = app_on(HarnessKind::ClaudeCode);
        type_line(&mut app, "read @main");
        let popup = app.mention.as_ref().expect("the popup is up");
        assert!(!popup.rooted);
        assert!(popup.rows.is_empty());

        assert_eq!(press(&mut app, KeyCode::Enter), None, "nothing is sent");
        assert_eq!(app.input, "read @main", "and nothing is inserted");
    }

    /// A mention is a word, so whitespace ends it — otherwise the popup stays
    /// up ranking the rest of the sentence.
    #[test]
    fn a_space_ends_the_mention() {
        let mut app = with_a_root();
        type_line(&mut app, "@rank and");
        assert!(app.mention.is_none());
    }

    /// Backspacing over the sign closes it, which is the other way out and the
    /// one people find by accident.
    #[test]
    fn backspacing_over_the_sign_closes_the_picker() {
        let mut app = with_a_root();
        type_line(&mut app, "@r");
        assert!(app.mention.is_some());
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert!(app.mention.is_none());
        assert_eq!(app.input, "");
    }
}
