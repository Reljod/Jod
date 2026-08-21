//! The TUI's state, and every decision it makes.
//!
//! Deliberately free of rendering and of I/O: everything here is a pure
//! transformation of state, so the behaviour that is easy to get subtly wrong —
//! cursor movement, scrollback, which turn a message belongs to — is testable
//! without a terminal.

use std::cmp::Reverse;

use jod_core::team::{Member, TeamTask};
use jod_core::{AgentEvent, HarnessKind, Model, PermissionPolicy, Resume};

use super::data::{
    ActivityItem, GoalRow, Hit, HookRow, MemoryKind, MemoryNode, ScheduleRow, Source, TaskRow,
    TaskState,
};
use super::delivery::Verdict;
use super::graph::GraphView;
use super::diff;
use super::fleet::{is_loose, loose_id, main_id, TreeState};
use super::todo;
use super::mention::Mention;
use super::picker::Picker;
use super::rail::RailState;
use super::secret::Typed;
use super::sessions::Browser;
use super::traffic;
use super::workspace::{matches, ListState, Workspace};
use jod_core::cards::Card;
use jod_core::commands::Discovered;
use jod_core::projects::{How, Project};
use jod_core::roots::Root;
use jod_core::secrets::Scope;
use jod_core::tree::{Node, NodeId, NodeKind};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// One line in the transcript, tagged with what produced it so the renderer can
/// style it without re-inspecting the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// What the user typed.
    You(String),
    /// Assistant prose.
    Agent(String),
    /// The agent's reasoning, shown only when thinking is toggled on.
    Thinking(String),
    /// A tool the agent called, with a one-line summary of its argument —
    /// `Bash · cargo test`, not a bare `Bash`.
    Tool {
        name: String,
        detail: Option<String>,
        step: Step,
    },
    /// What a tool gave back. Shown when details are on, which is the point of
    /// watching a harness work rather than waiting for its conclusion.
    ToolOut { text: String, failed: bool },
    /// A run finished: the summary line.
    Done { text: String, failed: bool },
    /// Something Jod itself wants to say *because you asked it something* —
    /// the roots list, a confirmation, an error.
    ///
    /// The renderer treats this as output, which is what separates it from
    /// [`Entry::Hint`]: a session holding one of these has answered a question
    /// and must not be painted over by the splash.
    Notice(String),
    /// Where a typed line was sent — the hand-off to the orchestrator, and the
    /// id of the run now carrying it.
    ///
    /// Its own variant rather than a [`Entry::Notice`] because it answers a
    /// question with a short shelf life. While the turn is in flight it is the
    /// only thing on screen saying the message went anywhere at all; once the
    /// reply is under it, it is a plumbing detail repeated at every turn. A
    /// notice cannot be told apart from a warning, and warnings must not be
    /// swept up with it.
    Routing(String),
    /// Something Jod says on its own account, before anyone has asked
    /// anything: the startup keymap line, and `/new`'s "new conversation".
    ///
    /// Its own variant rather than a `Notice` because `ui::fresh` has to tell
    /// the two apart, and the only other ways to do that are to sniff the
    /// notice's text or to carry a "has the user run anything" flag beside the
    /// transcript that every producer must remember to set. Both can be got
    /// wrong silently; a variant cannot — the entry either was pushed as a
    /// hint or it was not.
    Hint(String),
    /// A file edit, as a diff rather than as a one-line summary.
    ///
    /// Its own entry rather than a decorated `Tool`, because it is the one tool
    /// call whose *arguments* are the interesting part. Everything else is
    /// summarised to one line on purpose; an edit summarised to one line is the
    /// difference between watching an agent work and being able to trust it
    /// afterwards.
    ///
    /// The body folds behind `Ctrl-O`; the summary line does not. Collapsed,
    /// this is the file-change list — `± edited  ui.rs  +1 -0` — and that is
    /// the level most reading happens at. Expanded, it is the review.
    Diff { edit: diff::Edit, step: Step },
    /// The agent's plan, updating in place.
    ///
    /// One block per turn, not one per revision — see `todo.rs`. `App::apply`
    /// replaces the existing block rather than pushing a new one.
    Plan(Vec<todo::Item>),
    /// A line the harness printed that we could not classify.
    Raw(String),
    /// `Ctrl-B` sent work off to a background agent.
    ///
    /// Not a `Notice`, and the distinction is the whole point. Delegation is
    /// the one key that spends money unattended, and its old confirmation was
    /// a single notice — invisible on a cold session, and a line at the bottom
    /// of the status bar otherwise. It gets a block of its own naming all
    /// three things you would want to check afterwards: which agent, what it
    /// was told, and which directory it was pointed at.
    Delegated {
        id: String,
        prompt: String,
        dir: String,
    },
}

/// A notice raised somewhere the transcript is not on screen.
///
/// The transcript is drawn on the chat screen and nowhere else, so a notice
/// pushed while the cursor is on the fleet is invisible at the moment it is
/// meant to be read and is still there, out of context, the next time chat is
/// opened. Pressing `x` on the wrong fleet row eleven times used to put eleven
/// identical paragraphs in the conversation, and `c` put the whole session list
/// there — neither of which anybody asked the chat for.
///
/// So a notice raised outside chat becomes one of these instead: drawn over the
/// screen that raised it, and gone on its own after a few seconds. Nothing about
/// it reaches the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flash {
    /// One entry per notice line. Answers that come in lines — the session list,
    /// a branch listing — arrive here as one `push` per line and are collected
    /// rather than each replacing the last.
    pub lines: Vec<String>,
    /// When it was raised, so it can expire without anyone pressing anything.
    pub at_ms: i64,
    /// The animation frame it was raised on, which is how a second line of the
    /// same answer is told apart from an unrelated later notice. Every line of
    /// one answer is pushed between two ticks; a later keypress is at least one
    /// tick away.
    pub tick: u64,
}

/// How long a flash stays up: a base, plus reading time per line, capped.
///
/// A one-line refusal needs a glance. The session list is fifty rows and is
/// unreadable in the four seconds that is plenty for the refusal.
pub fn flash_ms(lines: usize) -> i64 {
    (4_000 + 1_200 * lines as i64).min(20_000)
}

/// What is drawn over everything else, and owns the keyboard while it is.
///
/// Overlays are the third layer of the navigation model: chat, workspace,
/// overlay. `Esc` always cancels one, and never does anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// `Ctrl-G` — the which-key menu, waiting for one letter. Any key it does
    /// not know cancels silently rather than doing something surprising.
    WhichKey,
    /// `Ctrl-G n` — waiting for the kind of thing to make.
    WhichKeyNew,
    /// `?` — the keymap, showing the current screen's verbs first.
    Keymap,
    /// `x` — deleting something that cannot be undone, so the prompt names it.
    Confirm {
        verb: String,
        what: String,
    },
    /// The background shells this console started — `/jobs`, or `Ctrl-G j`.
    Jobs,
    /// Offered when an update has installed a new binary: restart into it now,
    /// or stay on the build this process started with.
    ///
    /// Its own overlay rather than an [`Overlay::Confirm`], because that one
    /// is titled "this cannot be undone" and means it. Reloading is neither
    /// destructive nor irreversible, and a question that borrows a warning it
    /// does not need teaches people to ignore the warning.
    ConfirmReload,
    /// Tier 1 of the form ladder: one value, typed on a line where the keybar
    /// was, with no screen change and no context lost.
    Prompt {
        /// What is being asked for, shown to the left of the field.
        label: String,
        /// The line being typed.
        value: String,
        /// What to do with it once `⏎` is pressed.
        intent: PromptIntent,
    },
    /// A credential being collected for a `Secret` card.
    ///
    /// Deliberately **not** an `Overlay::Prompt`. A prompt's `value` is an
    /// ordinary `String` that the renderer echoes and that `accept_prompt`
    /// hands around as text — both correct for a schedule's name and both
    /// disqualifying for a token. This variant masks its field, keeps the value
    /// in a [`Typed`] that cannot print itself, and moves rather than copies it
    /// on the way out. See `secret.rs` for the full rule.
    Secret {
        /// The card this answers, carried rather than read off the rail's
        /// cursor for the reason [`PromptIntent::AnswerCard`] gives.
        card: i64,
        /// The environment variable's name, already validated by whoever
        /// raised the card.
        name: String,
        scope: Scope,
        /// The value, so far. Never rendered, never logged, never recalled.
        value: Typed,
    },
    /// The full-screen directory picker — the big half of the one picker `@`
    /// is the small half of. See `picker.rs`.
    Picker(Picker),
    /// Full-text search over every transcript.
    ///
    /// An overlay rather than a workspace because it is a way *to* somewhere:
    /// you open it, find the turn, and land in the conversation holding it. A
    /// screen you navigate to would be a place you then have to leave.
    Search {
        query: String,
        selected: usize,
        /// Filled by the loop, which is the only layer that may touch the
        /// store. Empty until the first keystroke has been searched for.
        hits: Vec<Hit>,
    },
    /// Every conversation you could go back into, as a list with a cursor.
    ///
    /// An overlay for the same reason [`Overlay::Search`] is one: it is a way
    /// *to* somewhere. The fleet lists runs and the chat is one conversation,
    /// so neither of them is the place to keep this — and until it existed,
    /// getting back into an old session from either screen meant already
    /// knowing its id. See [`super::sessions::Browser`].
    Sessions(Browser),
}

/// What a tier-1 prompt is collecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptIntent {
    /// Make a new thing of this workspace's kind.
    New(Workspace),
    /// Link the selected memory node to another, named here.
    Link(String),
    /// Go to one specific abandoned branch, named by the `#id` printed beside
    /// it.
    ///
    /// The redo key takes the newest tip, which covers undo-then-changed-my-mind
    /// — the only case most people ever have. This is for the rest: three or
    /// more branches set aside, and the one you want is not the last one you
    /// left. Without it those branches are listed, numbered, and unreachable,
    /// which is a worse state than not listing them at all.
    Branch,
    /// Answer a card in prose rather than by picking one of its options.
    ///
    /// Carries the card id rather than reading it off the rail's cursor, which
    /// is the one place here that departs from the "an overlay owns the
    /// keyboard, so the selection cannot have moved" rule that `confirmed` and
    /// [`PromptIntent::Branch`] rely on. It has to: the rail re-queries on the
    /// tick *underneath* the prompt, so an answer that landed on whatever card
    /// had sorted to the cursor by the time `⏎` was pressed would be an answer
    /// given to the wrong agent.
    AnswerCard(i64),
}

impl Overlay {
    pub fn is_open(&self) -> bool {
        *self != Overlay::None
    }
}

/// What the microphone is doing.
///
/// ## The microphone is a switch, not a button
///
/// `Ctrl-V` turns listening on and it stays on. Everything said while it is on
/// streams into the composer, sentence by sentence, and `⏎` is replaced by
/// saying so — "go ahead", "sige". The point is coding with your hands
/// somewhere else entirely.
///
/// Two things follow from that, and both are why this is a state rather than a
/// boolean:
///
/// * **It has to be obvious that it is on.** A microphone nobody remembers
///   switching on is a microphone in a room having a different conversation,
///   so the state carries what it needs to say so on every frame.
/// * **Utterances overlap.** A sentence is transcribed while the next one is
///   being spoken, so "listening" and "transcribing" are not exclusive —
///   `pending` counts what is in flight rather than replacing the state.
///
/// A hold-to-talk key was never available anyway: terminals report key
/// *presses*, and releases arrive only under the kitty keyboard protocol,
/// which most terminals and every plain SSH session lack.
#[derive(Debug, Clone, PartialEq)]
pub enum Dictation {
    Off,
    Listening {
        /// When the microphone was switched on.
        since_ms: i64,
        /// The recorder program, so a wrong input device is diagnosable.
        backend: String,
        /// What is transcribing right now. Utterances overlap, so this is a
        /// count rather than a flag.
        pending: usize,
        /// Whether he is speaking at this instant, for the meter.
        speaking: bool,
        /// Loudest frame in the last poll, for the meter.
        level: f32,
        /// How many sentences have landed in the composer this session.
        heard: usize,
    },
}

impl Dictation {
    pub fn is_active(&self) -> bool {
        !matches!(self, Dictation::Off)
    }

    /// Whether anything is still being transcribed.
    ///
    /// Read when the microphone is switched off: sentences in flight still
    /// have to land, and a session that dropped them would lose the last thing
    /// said every time.
    pub fn pending(&self) -> usize {
        match self {
            Dictation::Off => 0,
            Dictation::Listening { pending, .. } => *pending,
        }
    }

    pub fn note_pending(&mut self, delta: i64) {
        if let Dictation::Listening { pending, .. } = self {
            *pending = pending.saturating_add_signed(delta as isize);
        }
    }

    pub fn note_heard(&mut self) {
        if let Dictation::Listening { heard, .. } = self {
            *heard += 1;
        }
    }

    pub fn note_level(&mut self, at: f32, talking: bool) {
        if let Dictation::Listening {
            level, speaking, ..
        } = self
        {
            *level = at;
            *speaking = talking;
        }
    }
}

/// The spinner shown while a turn is in flight. Ten frames at four a second is
/// slow enough not to strobe and fast enough to prove the UI is alive — the
/// thing a static "working…" cannot do.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Evidence that a turn is still working, gathered from whichever harness
/// event proves it — a long turn can sit silent for more than one reason, and
/// the spinner alone cannot tell the reader which.
///
/// Two kinds, so far. Reasoning: `AgentEvent::Progress` ticks while the model
/// thinks with nothing else on the wire yet. Generation: `AgentEvent::Delta`
/// fragments while a long assistant message is being written — a message with
/// several `tool_use` blocks can take minutes to produce with no `Thinking` or
/// `Progress` event in between, because the model is not reasoning in that
/// window, it is emitting. Nothing about this type needs renaming to add a
/// third: the field is "the latest evidence", not "the token count".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A running reasoning-token count. Set, never incremented — the event
    /// carries the total so far, not a delta.
    Thinking(u64),
    /// A `Delta` fragment arrived: the model is mid-generation. Carries no
    /// count — `Delta` is an incremental fragment, not a running total, and
    /// this file does not own the transcript-side accounting that would be
    /// needed to turn a stream of fragments into a meaningful number. Proving
    /// the wire is alive does not require one.
    Generating,
}

impl Liveness {
    /// How this reads on the status line, or nothing if the reader has asked
    /// not to see it.
    ///
    /// `show_thinking` is passed in rather than read off `App` so each
    /// variant can answer independently — a generation signal is the model
    /// producing the answer it was asked for, not the reasoning behind it, so
    /// hiding reasoning must not also hide that.
    fn describe(self, show_thinking: bool) -> Option<String> {
        match self {
            Liveness::Thinking(tokens) => {
                show_thinking.then(|| format!("{tokens} thinking tokens"))
            }
            Liveness::Generating => Some("writing…".into()),
        }
    }
}

/// A duration as the shortest thing that still reads as a duration.
///
/// Watching agents means reading a column of these, so they are padded to a
/// consistent shape rather than spelled out: `9s`, `4m12s`, `2h05m`.
pub fn short_duration(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Which project the conversation on screen is about, and how that was decided.
///
/// The **id** is here because the name is not unique and the catalog knows it:
/// two checkouts called `api` are two rows, and `/project untrack api` already
/// refuses to guess between them. The panel used to mark the current project by
/// comparing names, so both rows got the `▸` and the box said two different
/// repositories were the one the next sentence would land in — which is the one
/// question it exists to answer.
///
/// The `how` is carried rather than dropped because it is the whole point of
/// showing this: a project he *named* needs no attention, and one that merely
/// carried over is the one worth a glance before an agent starts working in the
/// wrong repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub id: String,
    pub name: String,
    pub how: How,
}

pub struct App {
    pub transcript: Vec<Entry>,
    pub input: String,
    /// Cursor position as a *byte* index into `input`, always on a char
    /// boundary. Bytes rather than chars because every edit slices the string.
    pub cursor: usize,
    /// How many lines the view is scrolled up from the bottom. 0 = following.
    pub scroll: usize,
    pub harness: HarnessKind,
    /// The model to *ask* for, or `None` for whatever the harness picks itself.
    /// Only `/model` and the `-m` flag set this.
    pub model: Option<String>,
    /// The model the harness said it was using. Display only — it must never
    /// feed back into a spawn, because a name one harness reports (say
    /// `claude-sonnet-4-5`) is not a name another harness accepts.
    pub reported_model: Option<String>,
    /// What `/model` offers, in the current harness's own spelling. Loaded off
    /// the render path — asking OpenCode or AGY costs a subprocess, and AGY
    /// asks the network — so it is a field rather than a call.
    pub models: Vec<Model>,
    /// Which harness `models` belongs to, or `None` while nothing has been
    /// loaded. A list is only correct for the harness that produced it, so the
    /// loader compares this against `harness` rather than assuming: `/harness`
    /// mid-session makes the list on screen the wrong one.
    pub models_for: Option<HarnessKind>,
    pub session: Option<String>,
    pub resume: Resume,
    pub cost_usd: f64,
    /// Whether reasoning is shown. On by default, for the same reason tool
    /// output is: watching a harness think is most of why you sit in front of
    /// it. `/thinking` and `Ctrl-T` turn it off when the noise wins.
    pub show_thinking: bool,
    /// Whether tool output is shown. On by default: the reason to watch a
    /// harness work is to see what it is doing.
    pub show_details: bool,
    /// Whether the plumbing of turns that have already finished is drawn.
    ///
    /// Off by default, and that is the whole point. A tool call, what it
    /// returned and the line saying where a message was routed are worth
    /// watching while they happen, and are clutter the moment the answer is
    /// in — a transcript you scroll back through should read as the
    /// conversation, not as a log of every step that produced it. `Ctrl-O`
    /// turns the steps back on when you want to audit them.
    pub expand_details: bool,
    /// The screen you are on. Chat is home, and everything else is somewhere
    /// you go *from* it.
    pub workspace: Workspace,
    /// The workspaces you came through to get here, shallowest first. `Esc`
    /// pops exactly one, and an empty stack means the next `Esc` lands on chat.
    pub back_stack: Vec<Workspace>,
    /// One cursor, filter and sort per workspace, kept while you are away so
    /// coming back lands where you left.
    pub lists: Vec<ListState>,
    pub overlay: Overlay,
    /// What a keypress made outside chat has to say for itself, if anything.
    ///
    /// See [`Flash`]. `None` means the last one has expired or nothing has
    /// happened yet.
    pub flash: Option<Flash>,
    /// The local graph's focus, visit stack and neighbour cursor.
    pub graph: GraphView,
    /// True while the conversation on screen is mid-turn. Other agents may be
    /// working at the same time; this is only about the one being watched.
    pub busy: bool,
    /// The run a stop has been asked for and not yet heard back about.
    ///
    /// Stopping is not instant — the signal goes out, the harness winds down,
    /// and its own ending arrives afterwards — so there is a gap between the
    /// keypress and the end of the turn. Without a word for that gap the status
    /// bar had only `working` and `ready` to choose from, and it said `working`
    /// with the elapsed counter stopped: a frozen clock, which reads as a hung
    /// program rather than as a stop in progress.
    ///
    /// It also decides how the turn is written down. The ending of a run that
    /// was killed arrives as an error, because to anything reading an exit
    /// status that is what a signal is; rendered as-is it printed a red
    /// `✗ failed` directly beneath the `✓ done · interrupted` this file had
    /// just written, two verdicts on one turn that disagreed. A deliberate stop
    /// is not a failure.
    pub interrupting: Option<String>,
    pub agents: Vec<AgentLine>,
    /// Prompts typed while the watched conversation was still working. They are
    /// sent in order as the turn ends, so thinking ahead is not punished by
    /// having to wait with a finished sentence in your hands.
    pub queued: Vec<String>,
    /// Everything sent this session, oldest first, for ↑/↓ recall.
    pub history: Vec<String>,
    /// How far back through `history` the user has walked. `None` means "in the
    /// line I am writing", which is what typing anything returns to.
    pub history_at: Option<usize>,
    /// The half-written line ↑ was pressed on, restored by walking back down.
    pub draft: String,
    /// What the memory list is showing, or all of it. Cycled by `t`.
    pub memory_type: Option<MemoryKind>,
    /// What the activity feed is showing, or all of it. Cycled by `f`.
    pub activity_source: Option<Source>,
    /// Whether the activity feed hides what has been read. Toggled by `u`.
    pub unread_only: bool,
    /// Wall clock, refreshed on every tick so everything that renders an age
    /// stays a pure function of state.
    pub now_ms: i64,
    /// When the turn on screen started, for the elapsed counter.
    pub turn_started_ms: Option<i64>,
    /// The latest [`Liveness`] evidence for the turn on screen, or `None`
    /// before any has arrived.
    ///
    /// Only ever overwritten by newer evidence, never incremented or
    /// animated: if the harness stops producing it, this stops changing, and
    /// a status line built on it freezes exactly the way a genuinely wedged
    /// run should. Cleared at the start and end of every turn so evidence
    /// from a previous think can never bleed into the next one.
    pub liveness: Option<Liveness>,
    /// Ticks since start, which is all the spinner needs.
    pub tick: u64,
    /// Whether this console has already said that nothing is sweeping
    /// heartbeats.
    ///
    /// Once per session, not once per tick. The tick runs every second and the
    /// condition it tests stays true until somebody starts the daemon, so a
    /// notice without this flag would be a line a second — which is not a
    /// warning, it is the feed being destroyed.
    pub said_nothing_is_sweeping: bool,
    /// Which entry of the slash-command popup is highlighted. Meaningless when
    /// there is no popup, and clamped every time the input changes.
    pub suggestion: usize,
    /// The team this session is watching, if any. `None` means teams are not
    /// in play and the panel says so rather than showing an empty box.
    pub team: Option<String>,
    pub members: Vec<Member>,
    pub tasks: Vec<TeamTask>,
    /// What the workspaces show. Each is refreshed on the tick, off the render
    /// path, so `draw()` stays a pure function of state.
    pub memory: Vec<MemoryNode>,
    /// Entities and relations in the whole graph, which is not what `memory`
    /// holds: that is capped at the most-connected few hundred. Kept beside the
    /// list so the status bar can admit to showing a part of it, because a
    /// memory browser that counts its own rows claims to show everything.
    pub graph_size: (usize, usize),
    pub schedules: Vec<ScheduleRow>,
    pub goals: Vec<GoalRow>,
    pub hooks: Vec<HookRow>,
    pub activity: Vec<ActivityItem>,
    /// The task board as a screen of its own — richer than `tasks`, which is
    /// what the team panel reads.
    pub board: Vec<TaskRow>,
    pub should_quit: bool,
    /// Set when the user asks to leave while an agent is still running.
    pub confirm_quit: bool,
    /// The agent whose output fills the transcript. Every other agent keeps
    /// running and reports in through the panel and a notice when it ends —
    /// which is what makes this an orchestrator rather than a chat window.
    pub watching: Option<String>,
    /// How much the next turn may do without asking.
    ///
    /// On the app rather than only on `Options`, because it has to be
    /// changeable *while you are talking*: the mode you want depends on what
    /// you are about to ask for, and a setting fixed at launch means quitting
    /// the program to change your mind. Read at every spawn, so a change takes
    /// effect on the next turn and never mid-run.
    pub mode: PermissionPolicy,
    /// Whether the right-hand panel is showing. Shift-Tab opens and closes it.
    pub panel: bool,
    /// Tokens the watched conversation is carrying, as best the harness has
    /// reported them.
    ///
    /// "As best" is the honest framing: this is the last turn's input plus
    /// cache reads, which is what a harness reports and what actually occupies
    /// the window. It is an estimate and the screen must say so rather than
    /// print a precise-looking number nobody can check.
    pub context_tokens: u64,
    /// Whether the console still compacts on its own once the window fills.
    ///
    /// True until an automatic pass fails, and then false for the rest of the
    /// session. The trigger is a threshold that is met on *every* turn once it
    /// is crossed, so a compaction that cannot succeed — a store that refuses, a
    /// summariser that keeps coming back empty — would otherwise spawn a model
    /// call after every single turn and never stop. Giving up once and saying so
    /// is the only ending that does not quietly bill somebody.
    ///
    /// `/compact` is unaffected: a person asking for it is not a loop.
    pub auto_compact: bool,
    /// Shell jobs this console started and is not watching — an update
    /// building in the background, and whatever joins it later.
    ///
    /// Distinct from `agents`, which is the fleet: an agent is a harness with
    /// a transcript, and a job is a subprocess with output. Both are "things
    /// running that you are not looking at", and a console that can start one
    /// without offering a way to see it is asking to be trusted about work it
    /// never shows.
    pub jobs: Vec<Job>,
    /// The directory `jod tui` was launched in.
    ///
    /// Where every turn's harness process starts, and — see
    /// [`super::ensure_launch_root`] — the first root of the conversation on
    /// screen, so `@` searches the repository you are standing in rather than
    /// telling you to go and pick one.
    ///
    /// Empty in fixtures that do not care, which is what the header band reads
    /// to decide whether it has a directory worth printing.
    pub cwd: PathBuf,

    // ---- the decision rail, and the `@` picker --------------------------
    /// The conversation the rail and the `@` picker belong to.
    ///
    /// Kept here rather than derived at each use because both need it and
    /// neither may do I/O: the loop works it out — the conversation the chat
    /// box is bound to, or the pinned main chat when it is bound to nothing —
    /// and writes it down.
    pub conversation: Option<String>,
    /// The cards the rail is showing, already filtered and ordered by the
    /// store. Refreshed on the tick, off the render path, so `draw()` stays a
    /// pure function of state.
    pub cards: Vec<Card>,
    pub rail: RailState,
    /// The conversation's roots, in the user's own order. The first is the one
    /// an unqualified mention resolves against.
    pub roots: Vec<Root>,
    /// Every root's candidate paths, positionally aligned with `roots`.
    ///
    /// Shared rather than owned: a hundred thousand paths is a few megabytes,
    /// and `@` re-ranks on every keystroke. `Arc` is what makes that a pointer
    /// copy rather than a stall — see [`jod_core::rank::candidates_shared`].
    pub candidates: Vec<Arc<Vec<String>>>,
    /// The `@` popup, while it is up.
    pub mention: Option<Mention>,
    /// The slash commands and skills this repository offers, already filtered
    /// to the harness on screen. Refreshed on the tick, off the render path.
    pub discovered: Vec<Discovered>,

    // ---- the project catalog --------------------------------------------
    /// Reljod's repositories, most recently worked in first. Refreshed on the
    /// tick like every other list, because another session touching a project
    /// reorders it and a copy read once at open would be stale by the second
    /// instruction.
    pub projects: Vec<Project>,
    /// Which project the conversation on screen is about, and how that was
    /// decided.
    pub current_project: Option<Current>,
    /// Whether the catalog section of the panel is expanded.
    ///
    /// Open by default and collapsible, rather than hidden by default: the
    /// point of putting it on screen is that he can see which repository a
    /// dictated sentence will land in without asking. A twenty-project catalog
    /// would eat the panel, though, which is what the collapse is for.
    pub projects_open: bool,
    /// Whether the catalog has the keyboard, so its bare keys are its own.
    ///
    /// The same arrangement the rail has, and for the same reason: the chat box
    /// turns every bare key into text, so a catalog with a chord per verb would
    /// spend letters the keymap does not have. `Ctrl-P` hands it the keyboard,
    /// the keybar says so while it holds it, and `Esc` gives it back with the
    /// typed line untouched.
    pub panel_focused: bool,
    /// The project the catalog's cursor is on, by id.
    ///
    /// By id rather than by row, because the catalog reorders underneath the
    /// cursor: the current project is drawn first whatever the store's order is,
    /// and the tick rewrites the rest by recency. A row index would move the
    /// cursor to a different repository without anybody pressing anything.
    pub project_selected: Option<String>,

    // ---- dictation -------------------------------------------------------
    /// What the microphone is doing, if anything.
    pub dictation: Dictation,
    /// The sentences dictated into the composer, newest last.
    ///
    /// Kept so "undo that" can take back exactly the last sentence rather than
    /// a guessed number of words — the thing that makes a mis-heard sentence
    /// cheap to fix without reaching for the keyboard.
    pub dictated: Vec<String>,
    /// Set when a spoken "stop listening" was heard.
    ///
    /// A flag rather than an action because the recorder is owned by the event
    /// loop, and the transcript that carries this command arrives on a channel
    /// rather than through the key handler.
    pub stop_listening_requested: bool,

    // ---- the fleet tree -------------------------------------------------
    /// The projects and the agents in them, as the fleet draws them.
    ///
    /// Core flattens the whole forest in one pass and `fleet::condense` folds
    /// it to those two levels before it lands here, so everything that reads
    /// this field is reading the tree that is on the screen. Empty until a work
    /// exists, which is what keeps the fleet's older flat list meaningful for a
    /// session that belongs to no work.
    pub forest: Vec<Node>,
    /// Which of those works are closed. Core's answer, not an inference: a
    /// [`Node`] carries no state.
    pub closed_works: HashSet<NodeId>,
    /// The work each row of the forest came out of.
    ///
    /// The fold leaves no work rows to climb to, so this is how `T` still finds
    /// the bus belonging to the agent under the cursor.
    pub work_of: HashMap<NodeId, String>,
    /// The runs the tree accounts for, folded onto the sessions that started
    /// them. What is *not* in here is what the loose pane below the tree draws.
    pub tree_runs: HashSet<String>,
    /// The run each agent's row answers for. The fold leaves no run rows, so
    /// this is where `s`, `a` and `t` find the process to act on.
    pub run_of: HashMap<NodeId, String>,
    pub tree: TreeState,

    // ---- the traffic log ------------------------------------------------
    /// Which scope's bus the traffic screen is reading, or `None` before one
    /// has been opened from the tree.
    ///
    /// The *request*, kept apart from the loaded [`traffic::Log`] on purpose:
    /// the log is rebuilt from the store on every tick, so a scope stored only
    /// on the data would be forgotten by the first refresh after opening the
    /// screen.
    pub traffic_of: Option<traffic::Watching>,
    /// That scope's messages, refreshed on the tick like every other list.
    pub traffic: traffic::Log,
    /// Which states the log is narrowed to. `f` cycles it.
    pub traffic_shown: traffic::Shown,
}

/// One background shell this console started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// What it is, in the words the user typed: `update`, `update --check`.
    pub label: String,
    /// When it started, so the list can age it against `now_ms`.
    pub started_ms: i64,
    /// When it ended, or `None` while it is still going.
    pub ended_ms: Option<i64>,
    pub state: JobState,
    /// The last line it printed — the difference between "still running" and
    /// "still running, and here is what it is doing".
    pub last: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Ok,
    Failed,
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.state == JobState::Running
    }

    /// How long it has been going, or how long it took. One function so a
    /// finished job's duration stops growing, which a bare `now - started`
    /// would not.
    pub fn elapsed_ms(&self, now_ms: i64) -> i64 {
        (self.ended_ms.unwrap_or(now_ms) - self.started_ms).max(0)
    }

    pub fn mark(&self) -> &'static str {
        match self.state {
            JobState::Running => "•",
            JobState::Ok => "✓",
            JobState::Failed => "✗",
        }
    }
}

/// Where the context bar turns from information into advice.
///
/// Compaction is cheap and losing a conversation to a hard context error is
/// not, so the recommendation comes well before the wall. Claude Code compacts
/// around here too, for the same reason: the summary is better when there is
/// still room to write it.
pub const COMPACT_AT: f64 = 0.75;

/// The window to measure `context_tokens` against.
///
/// A single number for every harness and model, which is a deliberate
/// simplification: Jod cannot know the real limit — it varies by model, the
/// harness does not report it, and guessing per model would be a table that is
/// wrong the week a model ships. What the bar is for is "am I near the point
/// where I should compact", and a fixed generous denominator answers that
/// honestly as long as the screen calls it an estimate.
pub const CONTEXT_WINDOW: u64 = 200_000;

/// What every run the main chat spawns for itself is named.
///
/// Set by `hand_to_orchestrator`, one run per instruction. It is the only
/// handle the fleet has on "this row is the chat, not work the chat started" —
/// a run carries its id and its name, not the conversation it wrote into.
pub const ORCHESTRATOR: &str = "main";

/// The row id the pinned chat occupies in the fleet list.
///
/// Deliberately not a uuid, because it is not a run: it stands for the
/// conversation, which outlives every run underneath it. Nothing can collide
/// with it — every real row id is a uuid — and [`App::selected_agent`]
/// therefore answers `None` on it, which is the correct answer to "which agent
/// is selected" when the answer is "none, the chat is".
pub const MAIN_ROW: &str = "main";

/// The pinned chat as one fleet row.
///
/// A row about a *conversation*, standing over rows about runs. Collapsing
/// every `main` run into it is the point rather than a tidy-up: one instruction
/// is one run, so a week of use puts dozens of identical `main` rows in the
/// list, burying the delegated work the list exists to show — and none of them
/// is the chat, which is the thing you actually want to get back to.
#[derive(Debug, Clone, PartialEq)]
pub struct MainRow {
    /// `running` while an instruction is still being routed, `idle` otherwise.
    ///
    /// Not the status of the *work*: the orchestrator delegates and stops, so
    /// this goes idle while what it started keeps going. That is the honest
    /// reading — the chat is free, the agents below it are busy.
    pub status: String,
    /// When the most recent instruction was handed over, or `0` for a chat
    /// nothing has been said to yet.
    pub last_ms: i64,
    /// How many instructions this chat has routed in the runs still on screen.
    pub turns: usize,
    pub harness: String,
}

impl MainRow {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentLine {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub status: String,
    /// The harness's own conversation id, once it has reported one. This is
    /// what `/resume` actually needs — the panel shows Jod's agent id, which is
    /// a different thing entirely.
    pub session: Option<String>,
    /// When it was launched, so the panel can show how long it has been going.
    pub created_at_ms: i64,
    /// The directory the run was launched in.
    ///
    /// Carried onto the row because it is the one fact that distinguishes a
    /// run that did the work from a run that did the work *somewhere else*.
    /// The store has recorded it since the first migration and the summary has
    /// always carried it; it was dropped here, so no screen could show it, and
    /// a delegated run that wrote its whole output outside every declared root
    /// looked identical to one that had not.
    pub cwd: String,
    pub cost_usd: Option<f64>,
    /// The last thing it said, which is the only summary an unattended run
    /// offers of what it actually did.
    pub last: Option<String>,
    /// Whether the message this run owed somebody actually reached them.
    ///
    /// On the row rather than behind a key, and that distinction is the whole
    /// feature. The ledger could always answer "did that reply arrive" — but
    /// only if asked, and you have to already suspect a problem to ask. "The
    /// run says completed" is precisely the state in which nobody suspects
    /// anything, so an answer available on request is an answer nobody gets.
    ///
    /// [`Verdict::Nothing`] for the great majority of runs, which owed nobody
    /// a message at all. Deliberately not the same as `Fine`: saying
    /// "delivered" about a message that never existed is how a reader learns
    /// to distrust the good news.
    pub delivery: Verdict,
}

impl AgentLine {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }

    /// A turn of the main chat rather than work the main chat delegated.
    ///
    /// By name, which is the only signal a run carries. An agent someone
    /// deliberately names `main` would be folded into the pinned row — the cost
    /// of not adding a column to every run to record what one row needs, and
    /// visible rather than silent, since the row it folds into says how many
    /// turns it counted.
    pub fn is_orchestrator(&self) -> bool {
        self.name == ORCHESTRATOR
    }
}

/// What `/resume <id>` turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A conversation the harness can continue.
    Session(String),
    /// Not recognised here; hand it to the harness as typed.
    Verbatim(String),
    /// Matches a known agent that has no conversation id yet.
    NoSession(String),
    /// Matches this many agents, so it names none of them.
    Ambiguous(usize),
}

/// The most useful single field of a tool's arguments.
///
/// Harnesses name things differently, so the common keys are tried in order of
/// how much they tell a reader, and anything unrecognised falls back to compact
/// JSON rather than being dropped.
pub(super) fn tool_detail(input: &serde_json::Value) -> Option<String> {
    // Compared with case and underscores ignored, so `file_path`, `filePath`
    // and `FilePath` all match one entry. The harnesses genuinely disagree
    // here: AGY names its parameters `TargetFile` and `DirectoryPath`, and
    // without them its calls rendered as raw JSON.
    const KEYS: [&str; 12] = [
        "command",
        "cmd",
        "filepath",
        "path",
        "targetfile",
        "directorypath",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "searchterm",
    ];
    fn normalise(key: &str) -> String {
        key.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect()
    }
    if let Some(map) = input.as_object() {
        for key in KEYS {
            for (found, value) in map {
                if normalise(found) != key {
                    continue;
                }
                if let Some(v) = value.as_str() {
                    if !v.trim().is_empty() {
                        return Some(one_line(without_heredoc_bodies(v), 90));
                    }
                }
            }
        }
    }
    match input {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.trim().is_empty() => None,
        serde_json::Value::String(s) => Some(one_line(s, 90)),
        other => {
            let text = other.to_string();
            (text != "{}").then(|| one_line(&text, 90))
        }
    }
}

/// Collapse to one line and truncate, so a payload cannot own the transcript.
/// A command with the *contents* of its heredocs left off.
///
/// `cat > src/car.js <<'EOF'` followed by two hundred lines of JavaScript is
/// one shell command, and flattening it whole spends the summary line — and
/// often a second wrapped one — on the first ninety characters of a file that
/// is already rendered underneath as a diff. The interesting part of such a
/// command is its first line; the body has a better place to be.
fn without_heredoc_bodies(command: &str) -> &str {
    match command.find("<<") {
        // Keep the redirect and the delimiter, which is where the line stops
        // being about the shell and starts being about the file.
        Some(at) => match command[at..].find('\n') {
            Some(nl) => &command[..at + nl],
            None => command,
        },
        None => command,
    }
}

pub fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

/// A team-board task as a row of the tasks screen.
///
/// The screen wants a run, a runnable check and a blocked-by pair that
/// `TeamTask` does not carry yet, so those come back empty and the screen says
/// so — rather than the board being invisible until the store catches up.
fn task_row_from(task: &TeamTask) -> TaskRow {
    let state = if task.is_done() {
        TaskState::Done
    } else if task.is_claimed() {
        TaskState::Claimed
    } else {
        TaskState::Open
    };
    TaskRow {
        id: task.id.clone(),
        title: task.title.clone(),
        owner: task.owner.clone(),
        state,
        run: None,
        age_ms: 0,
        what: task.title.clone(),
        check: String::new(),
        blocked_by: Vec::new(),
        blocks: Vec::new(),
        spec: None,
        history: Vec::new(),
    }
}

/// A count and its noun, agreeing. A title bar reading `1 runs` is a small
/// thing that makes the whole screen look unfinished.
pub fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// An instant as a date and a clock — the answer to "when", which a countdown
/// alone never gives you.
pub fn absolute(at_ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(at_ms) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%b %d %H:%M")
            .to_string(),
        None => "—".to_string(),
    }
}

/// The gap to an instant, as the countdown a table's `in` column wants. `None`
/// is an em dash rather than a blank, so a paused row reads as deliberate.
pub fn until(now_ms: i64, at_ms: Option<i64>) -> String {
    match at_ms {
        Some(at) if at >= now_ms => short_duration(at - now_ms),
        Some(_) => "due".to_string(),
        None => "—".to_string(),
    }
}

/// The gap since an instant, for an `ago` column.
pub fn since(now_ms: i64, at_ms: Option<i64>) -> String {
    match at_ms {
        Some(at) => short_duration(now_ms.saturating_sub(at)),
        None => "—".to_string(),
    }
}

/// Keep the first `n` lines of tool output, saying how much was left.
pub(super) fn first_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.trim_end().to_string();
    }
    format!(
        "{}\n… (+{} more lines)",
        lines[..n].join("\n"),
        lines.len() - n
    )
}

/// Whether an entry is a step rather than the conversation.
///
/// The three kinds of line that say *how* an answer was produced. They are the
/// point of watching a run and the noise of reading one back, which is why they
/// are the only entries the transcript ever folds away.
fn is_plumbing(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::Tool { .. } | Entry::ToolOut { .. } | Entry::Routing(_)
    )
}

/// Whether a step is reporting something that went wrong.
fn failed(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::Tool {
            step: Step::Failed,
            ..
        } | Entry::ToolOut { failed: true, .. }
            | Entry::Diff {
                step: Step::Failed,
                ..
            }
    )
}

/// Whether a step has not come back yet.
///
/// The one thing that must never fold. A step still running is the answer to
/// "what is it doing right now", which is the question a reader asks *while*
/// waiting — and folding it away leaves a screen that has gone quiet for two
/// minutes with nothing on it to say why.
fn running(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::Tool {
            step: Step::Running,
            ..
        } | Entry::Diff {
            step: Step::Running,
            ..
        }
    )
}

/// Mark the call a result belongs to as finished, in any entry list.
///
/// Free rather than a method so the replay path — which rebuilds a transcript
/// from stored rows before there is an `App` to hold it — settles calls by
/// exactly the rule the live path uses. Two copies of this would drift, and the
/// symptom would be a replayed conversation whose steps spin forever.
pub fn settle_in(entries: &mut [Entry], name: &str, failed: bool) -> bool {
    let step = if failed { Step::Failed } else { Step::Ok };
    let at = entries
        .iter()
        .rposition(|e| matches!(e, Entry::Tool { name: n, step: Step::Running, .. } if n == name));
    let Some(at) = at else {
        return false;
    };
    for entry in entries[at..].iter_mut() {
        match entry {
            Entry::Tool { step: s, .. } | Entry::Diff { step: s, .. } if *s == Step::Running => {
                *s = step
            }
            _ => {}
        }
    }
    true
}

/// How far a step has got.
///
/// Replaces the plain `failed: bool` a tool line used to carry, because that
/// flag could only tell two of the three states apart: it said nothing about
/// whether the call had come back at all, so a command still running and a
/// command that had just succeeded rendered identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Called, with no result yet.
    Running,
    Ok,
    Failed,
}

/// Whether this tool's call already has a line among `entries`, so its result
/// does not need to add one.
///
/// A free function rather than a method because the live stream and the replay
/// of a stored conversation both have to answer it, and they hold their entries
/// in different places. Two copies of this rule is how the two views drift.
///
/// Not `entries.last()`, which is where this started: a call does not always
/// leave its line at the tail. An edit pushes its diff *underneath* its line,
/// and a plan call is folded into the plan block and pushes no line at all — so
/// the tail check answered "nobody announced this" for both, and the result arm
/// obligingly announced them a second time. A detail-less `⚙ Edit` appeared
/// under every diff and a `⚙ TodoWrite` under every plan revision, and neither
/// is distinguishable from a fresh anonymous call: a burst of writes read as a
/// stack of them.
pub(super) fn announced_in(entries: &[Entry], name: &str) -> bool {
    // The plan block is revised in place rather than re-pushed, so a todo call
    // leaves nothing near the tail to find. It is announced all the same — by
    // the block itself.
    if todo::names_a_plan(name) && entries.iter().any(|e| matches!(e, Entry::Plan(_))) {
        return true;
    }
    entries
        .iter()
        .rev()
        // Step over what the call pushed *below* its own line.
        .find(|e| !matches!(e, Entry::Diff { .. }))
        .is_some_and(|e| matches!(e, Entry::Tool { name: n, .. } if n == name))
}

/// Put a revised plan into the block already among `entries`, reporting whether
/// there was one. `false` means the caller still has to add it.
///
/// Split this way so the live stream keeps its scroll-aware `push` while replay
/// appends directly, without either of them owning a second copy of the
/// one-block-per-turn rule.
pub(super) fn replace_plan(entries: &mut [Entry], plan: &[todo::Item]) -> bool {
    match entries.iter_mut().rfind(|e| matches!(e, Entry::Plan(_))) {
        Some(existing) => {
            *existing = Entry::Plan(plan.to_vec());
            true
        }
        None => false,
    }
}

impl App {
    pub fn new(harness: HarnessKind, model: Option<String>, resume: Resume) -> App {
        App {
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            harness,
            model,
            reported_model: None,
            models: Vec::new(),
            models_for: None,
            session: None,
            resume,
            cost_usd: 0.0,
            show_thinking: true,
            show_details: true,
            expand_details: false,
            workspace: Workspace::Chat,
            back_stack: Vec::new(),
            lists: vec![ListState::default(); Workspace::ALL.len()],
            overlay: Overlay::None,
            flash: None,
            graph: GraphView::new(String::new()),
            busy: false,
            interrupting: None,
            agents: Vec::new(),
            queued: Vec::new(),
            history: Vec::new(),
            history_at: None,
            draft: String::new(),
            memory_type: None,
            activity_source: None,
            unread_only: false,
            now_ms: 0,
            turn_started_ms: None,
            liveness: None,
            tick: 0,
            said_nothing_is_sweeping: false,
            suggestion: 0,
            team: None,
            members: Vec::new(),
            tasks: Vec::new(),
            memory: Vec::new(),
            graph_size: (0, 0),
            schedules: Vec::new(),
            goals: Vec::new(),
            hooks: Vec::new(),
            activity: Vec::new(),
            board: Vec::new(),
            should_quit: false,
            confirm_quit: false,
            watching: None,
            mode: PermissionPolicy::default(),
            panel: false,
            context_tokens: 0,
            auto_compact: true,
            jobs: Vec::new(),
            // Set by the event loop from `Options::cwd`, like the mode and the
            // team beside it: `new` builds an app, the launch flags fill it in.
            cwd: PathBuf::new(),
            conversation: None,
            cards: Vec::new(),
            rail: RailState::default(),
            roots: Vec::new(),
            candidates: Vec::new(),
            mention: None,
            discovered: Vec::new(),
            projects: Vec::new(),
            current_project: None,
            // Open, because the catalog is only useful if seeing it costs
            // nothing — the point is to know where a dictated sentence lands
            // without having to ask.
            projects_open: true,
            panel_focused: false,
            project_selected: None,
            dictation: Dictation::Off,
            dictated: Vec::new(),
            stop_listening_requested: false,
            forest: Vec::new(),
            closed_works: HashSet::new(),
            work_of: HashMap::new(),
            tree_runs: HashSet::new(),
            run_of: HashMap::new(),
            tree: TreeState::default(),
            traffic_of: None,
            traffic: traffic::Log::default(),
            traffic_shown: traffic::Shown::Everything,
        }
    }

    // ---- the fleet tree --------------------------------------------------

    /// The visible tree rows, in the order they are drawn.
    ///
    /// The filter comes from the fleet screen's own `ListState`, not from a
    /// second one on `TreeState`. `/` is already wired there, on every list
    /// screen, with its own `Esc` and its own line under the box — a private
    /// copy would have been a filter the key never reached, which is exactly
    /// what it was until a render test caught it.
    ///
    /// The pinned chat comes first, always, exactly as [`App::row_ids`] puts it
    /// first in the flat list — and *outside* that filter rather than inside it,
    /// because `/` narrows the fleet, and the one row that is not part of the
    /// fleet is also the row you most need when a filter has emptied the screen.
    ///
    /// The loose runs come last, because that is where they are drawn: the
    /// pane below the tree is part of the same column and part of the same
    /// cursor, so `↓` off the bottom of the tree walks into it and `↑` walks
    /// back out. Any other arrangement makes the lower pane a place you can see
    /// and not go — see [`fleet::loose_id`].
    ///
    /// Empty when there is no tree, so the cursor is not parked on a row of a
    /// screen that is not being drawn.
    pub fn tree_rows(&self) -> Vec<NodeId> {
        if !self.has_tree() {
            return Vec::new();
        }
        std::iter::once(main_id())
            .filter(|_| !self.forest_holds_main())
            .chain(
                self.tree
                    .row_ids(&self.forest, &self.closed_works, self.tree_filter()),
            )
            .chain(self.loose_rows().iter().map(|a| loose_id(&a.id)))
            .collect()
    }

    /// Whether core's forest already carries the pinned chat's row.
    ///
    /// [`fleet::main_id`] was minted when it could not: the forest was works and
    /// what hangs off them, so the chat had no node to be and the fleet needed a
    /// sentinel or it became a screen you could walk into and not back out of.
    /// `forest_of` emits a [`NodeKind::Main`] row now, and two rows for one chat
    /// is worse than the problem the sentinel solved.
    ///
    /// So the sentinel became the fallback rather than the answer. It still
    /// appears when the forest has no such row — a store with nothing pinned
    /// yet — which keeps the guarantee it was added for without ever doubling
    /// the row it guarantees. The real node is preferred because it is the one
    /// carrying a conversation id, its runs, and its liveness.
    pub fn forest_holds_main(&self) -> bool {
        self.forest.iter().any(|n| n.kind == NodeKind::Main)
    }

    /// Where the cursor is within the pane below the tree, if it is in there.
    ///
    /// An index rather than the row itself, because the renderer needs it to
    /// scroll that pane — a cursor on the twentieth loose run has to bring the
    /// twentieth loose run on screen, and a pane that always drew its first
    /// three rows would let the selection walk off the bottom of a box that
    /// never moved.
    pub fn loose_selected(&self) -> Option<usize> {
        let id = self.tree.selected.as_ref()?;
        if !is_loose(id) {
            return None;
        }
        self.loose_rows().iter().position(|a| a.id == id.id)
    }

    /// Whether the tree's cursor is on the pinned chat rather than on a node.
    ///
    /// Distinct from [`App::main_selected`], which reads the *flat* list's
    /// cursor: the two screens keep separate selections, and the fleet draws
    /// only one of them at a time.
    pub fn tree_main_selected(&self) -> bool {
        if !self.has_tree() {
            return false;
        }
        let Some(id) = self.tree.selected.as_ref() else {
            return false;
        };
        // Either row means the same place. Which of the two is on screen depends
        // on whether core minted one — see [`App::forest_holds_main`] — and a
        // caller asking "is the cursor on the chat" must not have to know.
        *id == main_id()
            || self
                .forest
                .iter()
                .any(|n| n.kind == NodeKind::Main && n.id == *id)
    }

    /// What the fleet's `/` line currently holds.
    pub fn tree_filter(&self) -> Option<&str> {
        self.list(Workspace::Fleet).filter.as_deref()
    }

    /// The node under the cursor.
    pub fn selected_node(&self) -> Option<&Node> {
        let id = self.tree.selected.as_ref()?;
        self.forest.iter().find(|n| n.id == *id)
    }

    /// The name of the project row this node hangs under, if it has one.
    ///
    /// Walks the parent chain rather than reading `depth`, because a work with
    /// no project is drawn at depth 0 exactly like a project row is — the two
    /// are siblings on the screen and only the chain tells them apart. `None`
    /// means the row is genuinely outside every project, which is what a work
    /// opened in an uncatalogued directory looks like.
    ///
    /// The walk is bounded by the forest's own length: a parent chain is
    /// acyclic by construction, and the bound is there so that a malformed one
    /// refuses rather than hangs the interface.
    pub fn project_above(&self, node: &Node) -> Option<String> {
        let mut at = node;
        for _ in 0..self.forest.len() {
            if at.kind == jod_core::tree::NodeKind::Project {
                return Some(at.label.clone());
            }
            let parent = at.parent.as_ref()?;
            at = self.forest.iter().find(|n| n.id == *parent)?;
        }
        None
    }

    /// Whether the fleet shows the tree rather than its older flat list.
    ///
    /// A session that belongs to no work has no node in the forest, so the flat
    /// list is not legacy — it is what the screen shows when there is no tree
    /// to show, which is every session started before works existed.
    pub fn has_tree(&self) -> bool {
        !self.forest.is_empty()
    }

    // ---- the project catalog ---------------------------------------------

    /// The catalog in the order the panel draws it.
    ///
    /// The current project leads whatever the store's order is, because the one
    /// fact the box exists to show — which repository a dictated sentence lands
    /// in — must not be scrolled out of it by a catalog longer than the box is
    /// tall. Everything else keeps the store's recency order.
    ///
    /// One function rather than one per caller: the renderer draws these rows,
    /// the cursor steps over them and a click resolves against them, and three
    /// copies of the sort is how a click comes to open the row above the one
    /// under the pointer.
    pub fn catalog(&self) -> Vec<&Project> {
        let current = self.current_project.as_ref();
        let mut rows: Vec<&Project> = self.projects.iter().collect();
        rows.sort_by_key(|p| current.map(|c| p.id != c.id).unwrap_or(true));
        rows
    }

    /// The project the catalog's cursor is on, or the first row when it is on
    /// nothing — a focused list with no cursor has no obvious thing to do.
    pub fn selected_project(&self) -> Option<&Project> {
        let rows = self.catalog();
        match &self.project_selected {
            Some(id) => rows
                .iter()
                .find(|p| &p.id == id)
                .or_else(|| rows.first())
                .copied(),
            None => rows.first().copied(),
        }
    }

    /// Where the cursor sits in the drawn order, so the renderer can window
    /// around it and `step` can move it.
    pub fn project_index(&self) -> usize {
        let rows = self.catalog();
        self.project_selected
            .as_ref()
            .and_then(|id| rows.iter().position(|p| &p.id == id))
            .unwrap_or(0)
    }

    /// Move the catalog's cursor, stopping at both ends.
    ///
    /// Clamped rather than wrapping, unlike the rail's `Ctrl-N`: this is a
    /// cursor being driven by the arrows, and arrows that wrap read as the list
    /// having jumped.
    pub fn step_project(&mut self, delta: isize) {
        let ids: Vec<String> = self.catalog().iter().map(|p| p.id.clone()).collect();
        if ids.is_empty() {
            self.project_selected = None;
            return;
        }
        let at = self.project_index() as isize;
        let landed = (at + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.project_selected = Some(ids[landed].clone());
    }

    /// Give the catalog the keyboard, opening whatever it takes to make it
    /// visible first.
    ///
    /// Opening rather than toggling the boxes around it, for the reason the
    /// projects key already had: a key whose promise is *show me the projects*
    /// must mean that from either state, and a shut panel is the state every
    /// user starts in.
    pub fn focus_catalog(&mut self) {
        self.panel = true;
        self.projects_open = true;
        self.panel_focused = true;
        // Two things cannot hold the bare keys at once. The router checks the
        // rail first, so a rail left focused would swallow every key meant for
        // the catalog and the catalog would look inert.
        self.rail.focused = false;
        if self.project_selected.is_none() {
            self.project_selected = self.catalog().first().map(|p| p.id.clone());
        }
    }

    /// Hand the keyboard back and put the catalog away — what the same key
    /// pressed twice means, and what `Esc` means once.
    ///
    /// The panel itself is left alone. It holds the sessions and the context bar
    /// as well, and a key that reached in to collapse one box has no business
    /// taking the other two off screen; `Shift-Tab` is the one that owns the
    /// whole panel.
    pub fn close_catalog(&mut self) {
        self.panel_focused = false;
        self.projects_open = false;
    }

    /// Keep the catalog's cursor on a project that still exists.
    ///
    /// Untracking one from the fleet, or another session archiving it, leaves
    /// the cursor naming a row nothing draws — and then `⏎` opens nothing and
    /// says nothing, which reads as a broken key.
    pub fn reconcile_catalog(&mut self) {
        let Some(id) = self.project_selected.clone() else {
            return;
        };
        if self.projects.iter().any(|p| p.id == id) {
            return;
        }
        self.project_selected = self.catalog().first().map(|p| p.id.clone());
    }

    // ---- the decision rail ----------------------------------------------

    /// The cards on screen, by id, in the order they are drawn. This is what
    /// the rail's cursor moves over.
    pub fn card_ids(&self) -> Vec<i64> {
        self.cards.iter().map(|c| c.id).collect()
    }

    pub fn selected_card(&self) -> Option<&Card> {
        let id = self.rail.selected?;
        self.cards.iter().find(|c| c.id == id)
    }

    /// Keep the rail's cursor on a card that still exists.
    ///
    /// Separate from [`App::reconcile`], which walks the workspaces: the rail
    /// is drawn beside all of them and refreshes on its own query, so it is
    /// reconciled whenever *its* cards change rather than whenever a list does.
    pub fn reconcile_rail(&mut self) {
        let ids = self.card_ids();
        self.rail.reconcile(&ids);
    }

    // ---- the `@` picker --------------------------------------------------

    /// Open the popup for an `@` that has just been typed.
    ///
    /// `at` is the byte index of the `@` itself, which is the cursor *before*
    /// the character was inserted — the popup replaces the sign along with the
    /// query, so it has to know where the sign is.
    pub fn open_mention(&mut self, at: usize) {
        let mut popup = Mention::new(at);
        popup.refresh(&self.cwd, &self.roots, &self.candidates);
        self.mention = Some(popup);
    }

    /// Re-derive the popup from the line as it now stands, closing it if the
    /// text no longer supports one.
    ///
    /// Derived rather than tracked, because every edit key would otherwise have
    /// to remember to keep the popup in step — and the one that forgot would
    /// leave a popup ranking a query the line no longer contains. The rule is
    /// that a mention runs from its `@` to the cursor and holds no whitespace;
    /// backspacing over the `@`, or moving the cursor before it, ends it.
    pub fn sync_mention(&mut self) {
        let Some(popup) = &self.mention else {
            return;
        };
        let at = popup.at;
        let ended = at >= self.cursor
            || !self.input.is_char_boundary(at)
            || self.input[at..].chars().next() != Some('@')
            || self.input[at + 1..self.cursor]
                .chars()
                .any(char::is_whitespace);
        if ended {
            self.mention = None;
            return;
        }
        let query = self.input[at + 1..self.cursor].to_string();
        let Some(popup) = &mut self.mention else {
            return;
        };
        if popup.query == query {
            return;
        }
        popup.query = query;
        let (cwd, roots, candidates) = (
            self.cwd.clone(),
            self.roots.clone(),
            self.candidates.clone(),
        );
        if let Some(popup) = &mut self.mention {
            popup.refresh(&cwd, &roots, &candidates);
        }
    }

    /// Put the highlighted path into the line, replacing the `@` and the query.
    ///
    /// A trailing space is added because a mention is a word in a sentence and
    /// the next thing typed is the rest of it. Answers `false` when there was
    /// nothing to accept — which is what zero roots means, per E1.S3.
    pub fn accept_mention(&mut self) -> bool {
        let Some(popup) = &self.mention else {
            return false;
        };
        let Some(row) = popup.acceptable() else {
            return false;
        };
        let span = popup.span();
        let inserted = format!("@{} ", row.insertion());
        // Clamped rather than trusted: the span is derived from the line, but
        // an edit key that landed between the derivation and this call would
        // otherwise panic on a slice that is no longer inside the string.
        let end = span.end.min(self.input.len());
        if span.start > end || !self.input.is_char_boundary(span.start) {
            self.mention = None;
            return false;
        }
        self.input.replace_range(span.start..end, &inserted);
        self.cursor = span.start + inserted.len();
        self.mention = None;
        true
    }

    /// Move to the next permission mode, and say what happened.
    ///
    /// Returns the notice rather than pushing it, so the caller decides where
    /// it goes — the same call serves the chat transcript and the status line.
    ///
    /// Two clocks, and the wording has to be honest about both: Jod's own MCP
    /// tools are checked per call and change immediately, while the harness's
    /// native tools are bounded by the `--permission-mode` flag chosen when the
    /// process was spawned. A turn already in flight keeps the mode it started
    /// in; there is no way to tell a running harness otherwise.
    pub fn cycle_mode(&mut self) -> String {
        self.mode = self.mode.next();
        let when = if self.busy {
            " — from the next turn; this one keeps the mode it started in"
        } else {
            ""
        };
        format!("mode: {}{when}", self.mode.label())
    }

    /// Why this model name will not work here, when the harness's own list is
    /// able to say so.
    ///
    /// `None` means "no objection", and it covers two different things on
    /// purpose. Either the name is on the list, or there is no list to check it
    /// against — the binary is missing, `models` failed, the answer has not
    /// arrived yet, or it belongs to a harness that has since been swapped out.
    /// Only a loaded list for the harness we are actually on can convict a
    /// name, because "not in a list I could not read" is not a fact about the
    /// name.
    ///
    /// This exists because the harness's own complaint is useless. A name
    /// OpenCode does not have makes its server answer `UnknownError: Unexpected
    /// server error. Check server logs for details.`, which names neither the
    /// model nor the problem, and Jod printed it verbatim. Reljod asked main
    /// "what's the weather today" and got that, twice, with no way to tell from
    /// the screen that a model name set some days earlier was the reason.
    pub fn model_objection(&self, name: &str) -> Option<String> {
        if self.models_for != Some(self.harness) || self.models.is_empty() {
            return None;
        }
        if jod_core::harness::models::accepts(name, &self.models) {
            return None;
        }
        let harness = self.harness.label();
        let near = jod_core::harness::models::nearest(name, &self.models);
        Some(match near.as_slice() {
            // Nothing close enough to name. All that is left is to point at
            // the list and at the way out of choosing altogether.
            [] => format!(
                "{harness} has no model called {name}. Type /model and a space to \
                 see what it does have, or /model default to let it choose."
            ),
            // The common case, and the one worth being specific about: the
            // model is right and only its spelling is wrong, so the sentence
            // ends with something that can be typed straight back.
            [only] => format!(
                "{harness} has no model called {name}. It calls that one {only} — \
                 try /model {only}."
            ),
            _ => format!(
                "{harness} has no model called {name}. The closest it has are {}. \
                 Type /model and a space to see the whole list.",
                near.join(", ")
            ),
        })
    }

    /// A turn has started on `run`: watch it, and say the conversation is busy.
    ///
    /// One function because there are two ways in — a line typed into the main
    /// chat, which goes to the orchestrator, and a line typed into any other
    /// conversation, which goes straight to a harness — and they must leave the
    /// app in the same state. Nothing enforced that before, so a fixture could
    /// assemble a "mid-turn" app by hand and be believed while the state a real
    /// turn produces drifted away from it.
    pub fn begin_turn(&mut self, run: impl Into<String>, at_ms: i64) {
        self.watching = Some(run.into());
        self.busy = true;
        self.turn_started_ms = Some(at_ms);
        // Evidence left over from the turn before would read as this turn
        // already having done that much work before it asked its first
        // question.
        self.liveness = None;
        // Whatever was being stopped, this is not it. A stop that was never
        // heard back about would otherwise leave the status bar saying
        // `interrupting…` over a turn that has already started again.
        self.interrupting = None;
    }

    /// Whether `id` is the run a stop is waiting on, taking the wait with it.
    ///
    /// Called with a run's ending, so the answer is "this ending is the one the
    /// stop asked for" — which is what makes it a stop rather than a failure.
    pub fn claims_interrupt(&mut self, id: &str) -> bool {
        if self.interrupting.as_deref() == Some(id) {
            self.interrupting = None;
            return true;
        }
        false
    }

    /// How full the context window is, as a fraction — capped at 1.0 so a bar
    /// drawn from it cannot overflow its box.
    pub fn context_fraction(&self) -> f64 {
        (self.context_tokens as f64 / CONTEXT_WINDOW as f64).min(1.0)
    }

    /// Whether it is worth compacting yet.
    pub fn should_compact(&self) -> bool {
        self.context_fraction() >= COMPACT_AT
    }

    /// Replace the input with a chosen completion, cursor at the end.
    pub fn accept_completion(&mut self, line: &str) {
        self.input = line.to_string();
        self.cursor = self.input.len();
        self.suggestion = 0;
    }

    /// Keep the highlight inside the list as it shrinks under the cursor.
    pub fn clamp_suggestion(&mut self, count: usize) {
        if count == 0 {
            self.suggestion = 0;
        } else if self.suggestion >= count {
            self.suggestion = count - 1;
        }
    }

    pub fn next_suggestion(&mut self, count: usize) {
        if count > 0 {
            self.suggestion = (self.suggestion + 1) % count;
        }
    }

    pub fn prev_suggestion(&mut self, count: usize) {
        if count > 0 {
            self.suggestion = (self.suggestion + count - 1) % count;
        }
    }

    /// Record a background shell as started, and answer where it lives so its
    /// output can be routed back to it.
    ///
    /// Finished jobs stay in the list, oldest trimmed first: "what happened to
    /// that update" is asked *after* it ended, and a list that forgot on
    /// completion could never answer it.
    pub fn job_start(&mut self, label: impl Into<String>, now_ms: i64) -> usize {
        const KEEP: usize = 20;
        while self.jobs.len() >= KEEP {
            let Some(oldest) = self.jobs.iter().position(|j| !j.is_running()) else {
                break;
            };
            self.jobs.remove(oldest);
        }
        self.jobs.push(Job {
            label: label.into(),
            started_ms: now_ms,
            ended_ms: None,
            state: JobState::Running,
            last: None,
        });
        self.jobs.len() - 1
    }

    /// The most recent line a job printed. Blank lines are dropped rather than
    /// stored: a job whose "current activity" is an empty string reads as one
    /// that has stopped saying anything.
    pub fn job_line(&mut self, at: usize, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if let Some(job) = self.jobs.get_mut(at) {
            job.last = Some(line.trim().to_string());
        }
    }

    pub fn job_done(&mut self, at: usize, ok: bool, now_ms: i64) {
        if let Some(job) = self.jobs.get_mut(at) {
            job.state = if ok { JobState::Ok } else { JobState::Failed };
            job.ended_ms = Some(now_ms);
        }
    }

    pub fn running_jobs(&self) -> usize {
        self.jobs.iter().filter(|j| j.is_running()).count()
    }

    /// Replace the plan on screen, or start one.
    ///
    /// In place, and *where it already was*. A harness rewrites its todo list
    /// once per item finished, so appending would put fifteen near-identical
    /// lists between two sentences — and moving the block to the bottom on each
    /// revision would be a second kind of noise in place of the first. Its
    /// position says when the agent started planning, which does not change.
    pub fn revise_plan(&mut self, plan: Vec<todo::Item>) {
        if !replace_plan(&mut self.transcript, &plan) {
            self.push(Entry::Plan(plan));
        }
    }

    /// Whether this tool's call already has a line in the transcript, so its
    /// result does not need to add one. See [`announced_in`] for the rule.
    fn announced(&self, name: &str) -> bool {
        announced_in(&self.transcript, name)
    }

    /// Mark the call this result belongs to as finished, and report whether one
    /// was found.
    ///
    /// Matched by name, walking back from the end to the nearest call of that
    /// name still in flight. A result carries no call id — none of the three
    /// harnesses sends one on the normalised event — so "the most recent
    /// unfinished call with this name" is the whole of the available evidence.
    /// It is right whenever calls of one name settle in order, which is the
    /// only case a transcript can depict anyway.
    ///
    /// Every diff pushed *after* that call settles with it: one `Bash` writing
    /// four files produces four diffs under one call line, and they all become
    /// real at the moment the command returns.
    fn settle(&mut self, name: &str, failed: bool) -> bool {
        settle_in(&mut self.transcript, name, failed)
    }

    pub fn push(&mut self, entry: Entry) {
        // Feedback for something you did on another screen belongs on that
        // screen. Only notices and hints are diverted: they are Jod answering a
        // keypress, and the keypress happened where the cursor is. Everything
        // else here is the harness talking, and that is the conversation's own
        // record whatever screen you were looking at when it arrived.
        if self.workspace != Workspace::Chat {
            if let Entry::Notice(text) | Entry::Hint(text) = &entry {
                self.notify(text.clone());
                return;
            }
        }
        self.transcript.push(entry);
        // New output pulls the view back to the bottom only if it was already
        // there. Scrolling up to read something must not be undone by an agent
        // that keeps talking.
        if self.scroll == 0 {
            return;
        }
        self.scroll += 1;
    }

    /// Say one line over the current screen rather than in the conversation.
    ///
    /// Lines raised on the same animation frame are one answer and collect into
    /// one flash — `Action::Sessions` pushes the session list a row at a time,
    /// and fifty flashes each replacing the last would show only the last row.
    /// A later keypress is at least one tick away and starts a fresh one.
    pub fn notify(&mut self, line: String) {
        match &mut self.flash {
            Some(flash) if flash.tick == self.tick => {
                flash.lines.push(line);
                flash.at_ms = self.now_ms;
            }
            _ => {
                self.flash = Some(Flash {
                    lines: vec![line],
                    at_ms: self.now_ms,
                    tick: self.tick,
                })
            }
        }
    }

    /// Take the typed line, clearing the input. `None` if there was nothing.
    ///
    /// Remembering it here rather than at the call site means every route out of
    /// the input box — a prompt, a background delegation, a slash command — ends
    /// up recallable with ↑.
    pub fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.remember_typed(&text);
        self.input.clear();
        self.cursor = 0;
        Some(text)
    }

    /// Add a line to the recall history, newest last.
    ///
    /// A line identical to the previous one is not stored twice: re-running the
    /// same prompt is common, and a history of repeats makes ↑ useless.
    pub fn remember_typed(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
        }
        self.history_at = None;
        self.draft.clear();
    }

    /// Walk back through what has been sent. The line being written is kept, so
    /// walking forward again returns to it rather than losing it.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_at {
            None => {
                self.draft = self.input.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(at) => at - 1,
        };
        self.history_at = Some(next);
        self.input = self.history[next].clone();
        self.cursor = self.input.len();
    }

    /// Walk forward again, ending on the half-written line.
    pub fn history_next(&mut self) {
        let Some(at) = self.history_at else {
            return;
        };
        if at + 1 >= self.history.len() {
            self.history_at = None;
            self.input = std::mem::take(&mut self.draft);
        } else {
            self.history_at = Some(at + 1);
            self.input = self.history[at + 1].clone();
        }
        self.cursor = self.input.len();
    }

    // ---- queued prompts -------------------------------------------------

    /// Hold a prompt until the turn on screen finishes.
    pub fn queue(&mut self, prompt: String) {
        self.queued.push(prompt);
    }

    /// The next queued prompt, if any.
    pub fn next_queued(&mut self) -> Option<String> {
        if self.queued.is_empty() {
            return None;
        }
        Some(self.queued.remove(0))
    }

    // ---- navigation -----------------------------------------------------

    /// This workspace's cursor, filter and sort.
    pub fn list(&self, ws: Workspace) -> &ListState {
        &self.lists[ws.slot()]
    }

    pub fn list_mut(&mut self, ws: Workspace) -> &mut ListState {
        &mut self.lists[ws.slot()]
    }

    /// The list on the screen you are looking at.
    pub fn here(&self) -> &ListState {
        self.list(self.workspace)
    }

    pub fn here_mut(&mut self) -> &mut ListState {
        let ws = self.workspace;
        self.list_mut(ws)
    }

    /// Go to a workspace from the top: `Ctrl-G`, a digit, or a slash command.
    ///
    /// Top-level travel forgets the way back on purpose. Jumping from the local
    /// graph to schedules and then pressing `Esc` twice to end up in a memory
    /// node you have forgotten opening is not "back one level", it is a maze.
    pub fn go(&mut self, ws: Workspace) {
        self.back_stack.clear();
        self.workspace = ws;
        self.overlay = Overlay::None;
        // A flash belongs to the screen that raised it. Carrying it across would
        // mean a refusal about a fleet row hanging over the memory list, and
        // would put it back on screen when you returned to the fleet minutes
        // later, which reads as the thing having just happened again.
        self.flash = None;
        self.reconcile();
    }

    /// Go one level *deeper*, remembering where from — the memory list to its
    /// local graph is the only such move today.
    pub fn drill(&mut self, ws: Workspace) {
        if ws != self.workspace {
            self.back_stack.push(self.workspace);
        }
        self.workspace = ws;
        self.overlay = Overlay::None;
        self.flash = None;
        self.reconcile();
    }

    /// `Esc`: back exactly one level, and never anything else.
    ///
    /// An open overlay goes first, then an active filter, then a level of
    /// nesting, and the bottom is always chat.
    pub fn back(&mut self) {
        if self.overlay.is_open() {
            self.overlay = Overlay::None;
            return;
        }
        if self.workspace == Workspace::Chat {
            // Chat's own Esc is unchanged: follow the tail again.
            self.scroll_to_bottom();
            return;
        }
        if self.here().filter.is_some() {
            let list = self.here_mut();
            list.filter = None;
            list.editing_filter = false;
            self.reconcile();
            return;
        }
        self.workspace = self.back_stack.pop().unwrap_or(Workspace::Chat);
        self.flash = None;
        self.reconcile();
    }

    // ---- rows -----------------------------------------------------------

    /// The ids of the rows currently visible on a workspace, in the order they
    /// are drawn. This is what the cursor moves over, so filtering and sorting
    /// are automatically accounted for.
    pub fn row_ids(&self, ws: Workspace) -> Vec<String> {
        match ws {
            // The pinned chat first, always, and before the sort rather than
            // inside it: "running first, then newest" is a rule about work, and
            // the chat is the thing the work hangs off. A top row that moves is
            // a top row you have to look for.
            Workspace::Fleet => std::iter::once(MAIN_ROW.to_string())
                .chain(self.fleet_rows().iter().map(|a| a.id.clone()))
                .collect(),
            Workspace::Memory => self.memory_rows().iter().map(|n| n.id.clone()).collect(),
            Workspace::Schedules => self
                .schedule_rows()
                .iter()
                .map(|s| s.name.clone())
                .collect(),
            Workspace::Goals => self.goal_rows().iter().map(|g| g.name.clone()).collect(),
            Workspace::Hooks => self.hook_rows().iter().map(|h| h.name.clone()).collect(),
            Workspace::Tasks => self.task_rows().iter().map(|t| t.id.clone()).collect(),
            Workspace::Activity => self.activity_rows().iter().map(|a| a.id.clone()).collect(),
            Workspace::Team => self.tasks.iter().map(|t| t.id.clone()).collect(),
            // A message id is a number and every other list here keys on a
            // string, so it is spelled as one. The cursor is still the id and
            // never the row: the log reshapes under it every tick as agents
            // answer each other.
            Workspace::Traffic => self
                .traffic_rows()
                .iter()
                .map(|e| e.message.id.to_string())
                .collect(),
            Workspace::Chat | Workspace::MemoryGraph => Vec::new(),
        }
    }

    /// Keep every cursor on a row that still exists.
    ///
    /// Called after every refresh, filter change and sort change. Because the
    /// selection is an id rather than an index, a list that re-sorts under the
    /// cursor keeps the cursor on the *item* — which is the whole reason the
    /// fleet can refresh every four ticks without the selection wandering.
    pub fn reconcile(&mut self) {
        for ws in Workspace::ALL {
            if !ws.is_list() {
                continue;
            }
            let ids = self.row_ids(ws);
            // The fleet's top row is the pinned chat, and a cursor with nowhere
            // to go belongs on the first *agent* rather than on it — see
            // `ListState::reconcile_to`.
            if ws == Workspace::Fleet {
                let first_agent = self.fleet_rows().first().map(|a| a.id.clone());
                self.list_mut(ws).reconcile_to(&ids, first_agent);
                continue;
            }
            self.list_mut(ws).reconcile(&ids);
        }
        // The tree's cursor too, and for the same reason: `/` changes what is
        // visible, and a cursor left on a filtered-out node would put the
        // detail pane on something the list no longer shows.
        let rows = self.tree_rows();
        // Row 0 is the pinned chat, so the cursor's home is row 1 — the first
        // node of the forest. See `TreeState::reconcile_to`, and the flat list's
        // `first_agent` two blocks up, which is the same rule on the same
        // screen.
        let first_node = rows.get(1).cloned();
        self.tree.reconcile_to(&rows, first_node);
    }

    fn keep(&self, ws: Workspace, text: &str) -> bool {
        match self.list(ws).filter.as_deref() {
            Some(needle) => matches(needle, text),
            None => true,
        }
    }

    /// The pinned chat, collapsed out of however many runs it has taken.
    ///
    /// Always answers. The chat exists before anything has been said to it, and
    /// a top row that appears only once you have used it is a top row you never
    /// find — which is the whole failure this replaces.
    pub fn main_row(&self) -> MainRow {
        let turns = self.agents.iter().filter(|a| a.is_orchestrator());
        MainRow {
            status: if self
                .agents
                .iter()
                .any(|a| a.is_orchestrator() && a.is_running())
            {
                "running".into()
            } else {
                "idle".into()
            },
            last_ms: turns.clone().map(|a| a.created_at_ms).max().unwrap_or(0),
            turns: turns.count(),
            harness: self.harness.label().to_string(),
        }
    }

    /// Whether the cursor is on the pinned chat rather than on an agent.
    pub fn main_selected(&self) -> bool {
        self.list(Workspace::Fleet).selected.as_deref() == Some(MAIN_ROW)
    }

    pub fn fleet_rows(&self) -> Vec<&AgentLine> {
        let mut rows: Vec<&AgentLine> = self
            .agents
            .iter()
            // The chat's own runs belong to the pinned row above this list, not
            // in it. Left in, one instruction per row, they are the majority of
            // the fleet within a day and every one of them is the same chat.
            .filter(|a| !a.is_orchestrator())
            .filter(|a| {
                self.keep(
                    Workspace::Fleet,
                    &format!("{} {} {}", a.name, a.id, a.status),
                )
            })
            .collect();
        match self.list(Workspace::Fleet).sort % 4 {
            1 => rows.sort_by_key(|a| Reverse(a.created_at_ms)),
            2 => rows.sort_by_key(|a| a.name.clone()),
            3 => rows.sort_by(|a, b| {
                b.cost_usd
                    .unwrap_or(0.0)
                    .total_cmp(&a.cost_usd.unwrap_or(0.0))
            }),
            // Running first, then newest: the top row should be the thing most
            // likely to need attention.
            _ => rows.sort_by(|a, b| {
                b.is_running()
                    .cmp(&a.is_running())
                    .then(b.created_at_ms.cmp(&a.created_at_ms))
            }),
        }
        rows
    }

    /// The fleet's runs that the tree has no node for.
    ///
    /// `Store::forest_of` reads only conversations that belong to a work, so a
    /// run started by `delegate` never reaches the forest — by design, because
    /// a work is what the tree is a tree *of*. These are the rows the tree
    /// cannot draw, and the fleet shows them beside it rather than dropping
    /// them: a run nothing on screen accounts for is a run nobody stops.
    ///
    /// Asked of [`App::tree_runs`] rather than of the rows on screen. The tree
    /// draws no run rows any more — a run is folded onto the session that
    /// started it — so looking for one would find nothing and call every run in
    /// the fleet loose, which would make this pane a second copy of the list.
    ///
    /// Reads the same [`App::fleet_rows`] the flat list does, so the fleet's
    /// filter and sort apply here too.
    pub fn loose_rows(&self) -> Vec<&AgentLine> {
        self.fleet_rows()
            .into_iter()
            .filter(|a| !self.tree_runs.contains(&a.id))
            // Jod's own titlers and compactions write into no conversation, so
            // they have no node in the forest and land here — the pane for
            // runs somebody delegated that happen to belong to no work. On a
            // fleet with four projects on it, five of the six rows in this
            // pane were housekeeping, and the runs it exists to show were the
            // ones scrolled out of sight.
            .filter(|a| !jod_core::works::is_housekeeping_run(&a.name))
            .collect()
    }

    pub fn memory_rows(&self) -> Vec<&MemoryNode> {
        let mut rows: Vec<&MemoryNode> = self
            .memory
            .iter()
            .filter(|n| self.memory_type.is_none_or(|k| n.kind == k))
            .filter(|n| self.keep(Workspace::Memory, &format!("{} {}", n.name, n.body)))
            .collect();
        match self.list(Workspace::Memory).sort % 4 {
            1 => rows.sort_by(|a, b| b.confidence.total_cmp(&a.confidence)),
            2 => rows.sort_by_key(|a| a.name.clone()),
            3 => rows.sort_by_key(|a| a.age_ms),
            _ => rows.sort_by_key(|a| Reverse(a.degree)),
        }
        rows
    }

    pub fn schedule_rows(&self) -> Vec<&ScheduleRow> {
        let mut rows: Vec<&ScheduleRow> = self
            .schedules
            .iter()
            .filter(|s| self.keep(Workspace::Schedules, &format!("{} {}", s.name, s.gloss)))
            .collect();
        match self.list(Workspace::Schedules).sort % 3 {
            1 => rows.sort_by_key(|a| a.name.clone()),
            2 => rows.sort_by_key(|a| Reverse(a.last_ms)),
            // A paused schedule has no next fire, so it sorts last rather than
            // first, which is what a bare `None` would do.
            _ => rows.sort_by_key(|a| a.next_ms.unwrap_or(i64::MAX)),
        }
        rows
    }

    pub fn goal_rows(&self) -> Vec<&GoalRow> {
        let mut rows: Vec<&GoalRow> = self
            .goals
            .iter()
            .filter(|g| self.keep(Workspace::Goals, &format!("{} {}", g.name, g.objective)))
            .collect();
        match self.list(Workspace::Goals).sort % 3 {
            1 => rows.sort_by_key(|a| a.name.clone()),
            2 => rows.sort_by_key(|a| a.next_ms.unwrap_or(i64::MAX)),
            _ => rows.sort_by_key(|a| Reverse(a.percent())),
        }
        rows
    }

    pub fn hook_rows(&self) -> Vec<&HookRow> {
        let mut rows: Vec<&HookRow> = self
            .hooks
            .iter()
            .filter(|h| {
                self.keep(
                    Workspace::Hooks,
                    &format!("{} {} {}", h.name, h.repo, h.event),
                )
            })
            .collect();
        match self.list(Workspace::Hooks).sort % 3 {
            1 => rows.sort_by_key(|a| a.name.clone()),
            2 => rows.sort_by_key(|a| Reverse(a.last_ms)),
            _ => rows.sort_by_key(|a| Reverse(a.deliveries_24h)),
        }
        rows
    }

    /// The board, as its own screen.
    ///
    /// Built from the team board when the richer loader has nothing yet, so
    /// promoting tasks to a screen never *removes* the board that exists today.
    pub fn task_rows(&self) -> Vec<TaskRow> {
        let source: Vec<TaskRow> = if self.board.is_empty() {
            self.tasks.iter().map(task_row_from).collect()
        } else {
            self.board.clone()
        };
        let mut rows: Vec<TaskRow> = source
            .into_iter()
            .filter(|t| self.keep(Workspace::Tasks, &format!("{} {}", t.id, t.title)))
            .collect();
        match self.list(Workspace::Tasks).sort % 3 {
            1 => rows.sort_by_key(|a| a.id.clone()),
            2 => rows.sort_by_key(|a| Reverse(a.age_ms)),
            // Being worked, then claimed, then open, then blocked, then done —
            // the order attention should travel in.
            _ => rows.sort_by_key(|t| match t.state {
                TaskState::Running => 0,
                TaskState::Claimed => 1,
                TaskState::Open => 2,
                TaskState::Blocked => 3,
                TaskState::Done => 4,
            }),
        }
        rows
    }

    pub fn activity_rows(&self) -> Vec<&ActivityItem> {
        let mut rows: Vec<&ActivityItem> = self
            .activity
            .iter()
            .filter(|a| !self.unread_only || a.unread)
            .filter(|a| self.activity_source.is_none_or(|s| a.source == s))
            .filter(|a| self.keep(Workspace::Activity, &a.text))
            .collect();
        match self.list(Workspace::Activity).sort % 3 {
            1 => rows.sort_by_key(|a| (Reverse(a.unread), Reverse(a.at_ms))),
            2 => rows.sort_by_key(|a| a.source.label()),
            _ => rows.sort_by_key(|a| Reverse(a.at_ms)),
        }
        rows
    }

    /// The traffic on screen: filtered, threaded and in the order it is drawn.
    ///
    /// The `/` filter and the sort come out of the screen's own [`ListState`],
    /// like every other list here, so `Esc` clears it and the line under the
    /// box reports it without anything extra being wired.
    pub fn traffic_rows(&self) -> Vec<&jod_core::team::Envelope> {
        let list = self.list(Workspace::Traffic);
        traffic::rows(
            &self.traffic.messages,
            &self.traffic.held,
            self.traffic_shown,
            list.filter.as_deref(),
            list.sort,
        )
    }

    pub fn selected_message(&self) -> Option<&jod_core::team::Envelope> {
        let id: i64 = self
            .list(Workspace::Traffic)
            .selected
            .as_deref()?
            .parse()
            .ok()?;
        self.traffic.messages.iter().find(|e| e.message.id == id)
    }

    /// Whether the selected message is one nobody will ever read.
    pub fn selected_is_held(&self) -> bool {
        self.selected_message()
            .is_some_and(|e| self.traffic.held.contains(&e.message.id))
    }

    /// The run every fleet verb acts on — read off whichever cursor the screen
    /// is actually drawing.
    ///
    /// On a tree that is `TreeState`, not this list. Taking the list's row
    /// there was the other half of the two-cursor fault: `s` stopped a run the
    /// highlight was nowhere near, which is worse than a key that does nothing
    /// because it looks like it worked. A run node's id *is* the run id, which
    /// is what these verbs take; a work is a heading and a session is a
    /// conversation, and neither is a process, so both answer `None` and let
    /// the caller say so.
    pub fn selected_agent(&self) -> Option<&AgentLine> {
        if self.has_tree() {
            // A row in the pane below the tree is a run like any other — it is
            // only *drawn* apart because the forest has no node for it. Read
            // before `selected_node`, which answers `None` for a sentinel and
            // would leave every verb on that pane silent.
            if let Some(id) = self.tree.selected.as_ref().filter(|id| is_loose(id)) {
                return self.agents.iter().find(|a| a.id == id.id);
            }
            let node = self.selected_node()?;
            // An agent's row *is* its process now. The fold takes the run rows
            // away, so a session that has a run answers for it — which is also
            // how the row reads: it says an agent is running, and `s` on a row
            // that says that should stop it.
            let run = match node.kind {
                NodeKind::Run => node.id.id.clone(),
                NodeKind::Session => self.run_of.get(&node.id)?.clone(),
                _ => return None,
            };
            return self.agents.iter().find(|a| a.id == run);
        }
        let id = self.list(Workspace::Fleet).selected.as_deref()?;
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn selected_task(&self) -> Option<&TeamTask> {
        let id = self.list(Workspace::Team).selected.as_deref()?;
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn selected_memory(&self) -> Option<&MemoryNode> {
        let id = self.list(Workspace::Memory).selected.as_deref()?;
        self.memory.iter().find(|n| n.id == id)
    }

    /// The node the local graph is centred on.
    pub fn focused_memory(&self) -> Option<&MemoryNode> {
        self.memory.iter().find(|n| n.id == self.graph.focus)
    }

    pub fn selected_schedule(&self) -> Option<&ScheduleRow> {
        let id = self.list(Workspace::Schedules).selected.as_deref()?;
        self.schedules.iter().find(|s| s.name == id)
    }

    pub fn selected_goal(&self) -> Option<&GoalRow> {
        let id = self.list(Workspace::Goals).selected.as_deref()?;
        self.goals.iter().find(|g| g.name == id)
    }

    pub fn selected_hook(&self) -> Option<&HookRow> {
        let id = self.list(Workspace::Hooks).selected.as_deref()?;
        self.hooks.iter().find(|h| h.name == id)
    }

    pub fn selected_activity(&self) -> Option<&ActivityItem> {
        let id = self.list(Workspace::Activity).selected.as_deref()?;
        self.activity.iter().find(|a| a.id == id)
    }

    pub fn selected_board_task(&self) -> Option<TaskRow> {
        let id = self.list(Workspace::Tasks).selected.clone()?;
        self.task_rows().into_iter().find(|t| t.id == id)
    }

    /// Endings that arrived while nobody was looking, for the `⚑ n` badge.
    pub fn unread(&self) -> usize {
        self.activity.iter().filter(|a| a.unread).count()
    }

    /// What the which-key menu prints beside a workspace, which is what makes
    /// the menu a dashboard as well as a menu — you often get the answer
    /// without pressing the second key.
    pub fn count_for(&self, ws: Workspace) -> String {
        match ws {
            Workspace::Chat => "the conversation".to_string(),
            Workspace::Fleet => {
                if self.agents.is_empty() {
                    return "nothing delegated yet".into();
                }
                let failed = self.agents.iter().filter(|a| a.status == "failed").count();
                format!(
                    "{} · {} running · {failed} failed",
                    plural(self.agents.len(), "run"),
                    self.running()
                )
            }
            Workspace::Memory | Workspace::MemoryGraph => {
                if self.memory.is_empty() {
                    return "nothing remembered yet".into();
                }
                let (nodes, edges) = self.graph_size;
                let clashes = self.memory.iter().filter(|n| n.contradicted).count();
                // The list is capped at the most-connected few hundred, so it
                // says which part of the graph it is when it is a part. A
                // browser that counts its own rows tells you it is showing
                // everything, which is the one thing it must not get wrong.
                let shown = if nodes > self.memory.len() {
                    format!("{} of {}", self.memory.len(), plural(nodes, "node"))
                } else {
                    plural(self.memory.len(), "node")
                };
                format!(
                    "{shown} · {} · {}",
                    plural(edges, "edge"),
                    plural(clashes, "contradiction")
                )
            }
            Workspace::Schedules => {
                if self.schedules.is_empty() {
                    return "none yet".into();
                }
                let next = self
                    .schedules
                    .iter()
                    .filter_map(|s| s.next_ms.map(|at| (at, s.name.clone())))
                    .min();
                match next {
                    Some((at, name)) => format!(
                        "{} · next {name} in {}",
                        self.schedules.len(),
                        short_duration(at.saturating_sub(self.now_ms))
                    ),
                    None => format!("{} · none armed", self.schedules.len()),
                }
            }
            Workspace::Goals => {
                if self.goals.is_empty() {
                    return "none yet".into();
                }
                let blocked = self
                    .goals
                    .iter()
                    .filter(|g| g.state == crate::tui::data::GoalState::Blocked)
                    .count();
                let waiting = self.goals.iter().filter(|g| g.escalation.is_some()).count();
                format!(
                    "{} · {blocked} blocked · {waiting} needs you",
                    self.goals.len()
                )
            }
            Workspace::Hooks => {
                if self.hooks.is_empty() {
                    return "none yet".into();
                }
                let failing = self
                    .hooks
                    .iter()
                    .filter(|h| h.state == crate::tui::data::HookState::Failing)
                    .count();
                format!(
                    "{} · {failing} failing",
                    plural(self.hooks.len(), "webhook")
                )
            }
            Workspace::Tasks => {
                let rows = self.task_rows();
                if rows.is_empty() {
                    return "the board is empty".into();
                }
                let claimed = rows
                    .iter()
                    .filter(|t| t.state == TaskState::Claimed)
                    .count();
                let blocked = rows
                    .iter()
                    .filter(|t| t.state == TaskState::Blocked)
                    .count();
                let open = rows.iter().filter(|t| t.state != TaskState::Done).count();
                format!("{open} open · {claimed} claimed · {blocked} blocked")
            }
            Workspace::Activity => match self.unread() {
                0 => "nothing new".to_string(),
                n => format!("{n} unread"),
            },
            Workspace::Team => match &self.team {
                None => "no team — start one with --team".to_string(),
                Some(name) => {
                    let busy = self
                        .members
                        .iter()
                        .filter(|m| m.status == jod_core::team::MemberStatus::Busy)
                        .count();
                    format!(
                        "{name} · {} · {busy} busy",
                        plural(self.members.len(), "member")
                    )
                }
            },
            // The budget is in the count line rather than only in the pane,
            // because G4.S5 asks for it to be seen *before* it is spent and the
            // status bar is the one row that is always on screen.
            Workspace::Traffic => {
                if self.traffic_of.is_none() {
                    return "no work chosen — T on a fleet row opens one".into();
                }
                let mut line = format!(
                    "{} · {} in {}",
                    self.traffic.title,
                    plural(self.traffic.messages.len(), "message"),
                    plural(self.traffic.threads(), "thread")
                );
                let troubled = self.traffic.troubled();
                if troubled > 0 {
                    line.push_str(&format!(" · {troubled} undelivered"));
                }
                line.push_str(&format!(
                    " · {} of {} budget left",
                    self.traffic.budget_left(),
                    self.traffic.budget
                ));
                line
            }
        }
    }

    // ---- liveness -------------------------------------------------------

    /// One animation frame. `now_ms` is passed in rather than read here so the
    /// whole of the UI stays testable without a clock.
    pub fn advance(&mut self, now_ms: i64) {
        self.tick = self.tick.wrapping_add(1);
        self.now_ms = now_ms;
        // A flash goes away by itself, which is the whole difference between it
        // and a transcript line. Dropped from the state rather than merely left
        // undrawn, so nothing can bring an old one back.
        if let Some(flash) = &self.flash {
            if now_ms.saturating_sub(flash.at_ms) >= flash_ms(flash.lines.len()) {
                self.flash = None;
            }
        }
    }

    pub fn spinner(&self) -> &'static str {
        SPINNER[(self.tick as usize) % SPINNER.len()]
    }

    /// How long the turn on screen has been going.
    pub fn elapsed(&self) -> Option<String> {
        let started = self.turn_started_ms?;
        Some(short_duration(self.now_ms.saturating_sub(started)))
    }

    /// Point the next turn at the harness that produced a run.
    ///
    /// Resuming an OpenCode conversation while the UI is set to Claude Code
    /// hands the session id to a harness that has never heard of it, so picking
    /// up a run has to bring its harness with it.
    pub fn harness_from_label(&mut self, label: &str) {
        if let Some(kind) = HarnessKind::ALL
            .into_iter()
            .find(|k| k.label().eq_ignore_ascii_case(label))
        {
            if kind != self.harness {
                self.harness = kind;
                self.model = None;
                self.reported_model = None;
            }
        }
    }

    /// How many agents are working, watched or not.
    pub fn running(&self) -> usize {
        self.agents.iter().filter(|a| a.is_running()).count()
    }

    // ---- input editing -------------------------------------------------

    pub fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_boundary(self.cursor);
        self.input.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let next = self.next_boundary(self.cursor);
        self.input.replace_range(self.cursor..next, "");
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    pub fn right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor = self.next_boundary(self.cursor);
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Delete the word before the cursor, as Ctrl-W does in a shell.
    pub fn delete_word(&mut self) {
        let mut at = self.cursor;
        while at > 0 {
            let prev = self.prev_boundary(at);
            if !self.input[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        while at > 0 {
            let prev = self.prev_boundary(at);
            if self.input[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        self.input.replace_range(at..self.cursor, "");
        self.cursor = at;
    }

    pub fn clear_line(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Where the cursor sits in *columns*, which is what the renderer needs.
    pub fn cursor_column(&self) -> usize {
        self.input[..self.cursor].chars().count()
    }

    fn prev_boundary(&self, from: usize) -> usize {
        let mut i = from - 1;
        while i > 0 && !self.input.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_boundary(&self, from: usize) -> usize {
        let mut i = from + 1;
        while i < self.input.len() && !self.input.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    // ---- scrolling -----------------------------------------------------

    /// `max` is how far up the view can go, given the viewport height.
    pub fn scroll_up(&mut self, lines: usize, max: usize) {
        self.scroll = (self.scroll + lines).min(max);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    pub fn following(&self) -> bool {
        self.scroll == 0
    }

    // ---- what the transcript shows ---------------------------------------

    /// Where the turn on screen begins, or `None` when nothing is running.
    ///
    /// The last ending is the boundary: everything below it belongs to the turn
    /// in flight, and everything above it is history. Once the run stops there
    /// is no live turn at all, which is what makes the steps of a finished turn
    /// fold themselves away without anything having to go back and mark them.
    pub fn live_from(&self) -> Option<usize> {
        if !self.busy {
            return None;
        }
        Some(
            self.transcript
                .iter()
                .rposition(|e| matches!(e, Entry::Done { .. }))
                .map_or(0, |i| i + 1),
        )
    }

    /// Whether the entry at `i` is left out of the drawn transcript.
    ///
    /// Only ever plumbing: a tool call, what a tool returned, the line naming
    /// where a message was routed. Prose, plans, diffs and endings are the
    /// conversation itself and are always drawn.
    pub fn hidden(&self, i: usize) -> bool {
        let Some(entry) = self.transcript.get(i) else {
            return false;
        };
        // A failure is the reason the answer is about to be wrong, so it stays
        // on screen under every setting — the same rule that already decides a
        // failed tool result is recorded whether or not details are on. A step
        // that has not come back yet stays for the opposite reason: it is the
        // only thing on screen saying what is happening now.
        if !is_plumbing(entry) || failed(entry) || running(entry) {
            return false;
        }
        if self.expand_details {
            return false;
        }
        // Details off means the steps are not shown even while they happen;
        // details on shows the turn in flight and nothing older.
        if !self.show_details {
            return true;
        }
        self.live_from().is_none_or(|start| i < start)
    }

    // ---- agent events --------------------------------------------------

    /// Fold one event from the harness into the transcript.
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Started { session_id, model } => {
                if let Some(s) = session_id {
                    self.session = Some(s.clone());
                    // Next turn continues this exact conversation rather than
                    // "the most recent", which could be someone else's.
                    self.resume = Resume::Session(s.clone());
                }
                if let Some(m) = model {
                    self.reported_model = Some(m.clone());
                }
            }
            // The exact inverse of the arm above, and it has to be here rather
            // than only in the database: this cursor is held in memory, so a
            // console that had already advanced onto a session goes on asking
            // for it every turn no matter what the row says. That is what three
            // identical one-second failures in a row looked like from the
            // outside — the same dead id, resent by the same live `App`.
            //
            // Only when it is *this* chat's session. A background delegation
            // resuming something of its own is not evidence about the cursor
            // here, and clearing on it would drop a live thread's continuity.
            //
            // `Fresh` rather than `Last`: the next turn carries the transcript
            // Jod itself holds, which is a thing Jod can prove, where "the most
            // recent conversation in this directory" is the harness guessing.
            AgentEvent::SessionLost { session_id } => {
                if self.session.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.session = None;
                self.resume = Resume::Fresh;
                self.push(Entry::Notice(
                    "that harness session is gone — the next message replays \
                     this thread into a fresh one"
                        .into(),
                ));
            }
            AgentEvent::Thinking { text } => {
                if self.show_thinking {
                    self.push(Entry::Thinking(text.clone()));
                }
            }
            AgentEvent::Message { text } => self.push(Entry::Agent(text.clone())),
            AgentEvent::ToolCall { name, input } => {
                // A todo call *is* the plan block, and gets no summary line of
                // its own. The line would be pushed again on every revision —
                // a dozen `⚙ TodoWrite` rows around one block — which is the
                // noise this whole slice exists to remove.
                if let Some(plan) = input.as_ref().and_then(|i| todo::from_tool(name, i)) {
                    self.revise_plan(plan);
                    return;
                }
                // A file edit becomes a diff; everything else keeps its
                // one-line summary. An edit *does* keep its `Tool` line above
                // the diff — unlike the plan it is pushed once per edit, and it
                // makes the transcript read as "it did this, and here is what
                // it was".
                self.push(Entry::Tool {
                    name: name.clone(),
                    detail: input.as_ref().and_then(tool_detail),
                    step: Step::Running,
                });
                if let Some(edit) = input.as_ref().and_then(|i| diff::from_tool(name, i)) {
                    self.push(Entry::Diff {
                        edit,
                        step: Step::Running,
                    });
                }
                // A shell can write files without ever touching the edit tool,
                // and agents that build a project out of `cat > f <<'EOF'` do
                // exactly that. Without this the file-change list is empty for
                // a whole run that wrote fifty files. See `diff::from_shell`.
                for edit in input.as_ref().map(|i| diff::from_shell(name, i)).unwrap_or_default() {
                    self.push(Entry::Diff {
                        edit,
                        step: Step::Running,
                    });
                }
            }
            AgentEvent::ToolResult {
                name,
                summary,
                is_error,
            } => {
                // A failure is always shown: it is the reason the answer is
                // about to be wrong. Success is shown when details are on.
                //
                // A result also needs a call line above it when none was
                // announced. OpenCode reports a fast tool as already
                // `completed`, so no ToolCall ever arrives and the output was
                // rendered as a bare `└ Wrote file successfully.` — an answer
                // with its question missing.
                let announced = self.announced(name);
                // The call that is coming back stops being "in flight" here.
                // Done before the fallback below so a harness that never
                // announced the call does not settle some older one by name.
                let settled = self.settle(name, *is_error);
                if *is_error && settled {
                    // The failure is already on the call's own line, which is
                    // still on screen because a failed step never folds. A
                    // second, detail-less `✗ Bash` under it says nothing the
                    // first did not, and loses the argument in the process.
                } else if *is_error || !announced {
                    self.push(Entry::Tool {
                        name: name.clone(),
                        detail: None,
                        step: if *is_error { Step::Failed } else { Step::Ok },
                    });
                }
                if let Some(text) = summary.as_ref().filter(|s| !s.trim().is_empty()) {
                    if *is_error || self.show_details {
                        self.push(Entry::ToolOut {
                            text: first_lines(text, 6),
                            failed: *is_error,
                        });
                    }
                }
            }
            AgentEvent::Finished {
                is_error, usage, ..
            } => {
                if let Some(c) = usage.cost_usd {
                    self.cost_usd += c;
                }
                // What actually occupies the window: everything the model was
                // shown this turn, whether it was sent fresh or read from
                // cache. Assigned rather than accumulated — each turn resends
                // the whole conversation, so adding them up would count the
                // same history once per turn and hit the wall almost at once.
                let shown = usage.input_tokens.unwrap_or(0) + usage.cache_read_tokens.unwrap_or(0);
                if shown > 0 {
                    self.context_tokens = shown;
                }
                let mut bits = vec![];
                if let Some(t) = usage.output_tokens {
                    bits.push(format!("{t} out"));
                }
                if let Some(c) = usage.cost_usd {
                    bits.push(format!("${c:.4}"));
                }
                if let Some(t) = self.elapsed() {
                    bits.push(t);
                }
                // A run stopped on purpose ends here too, and its ending is an
                // error to anything reading an exit status — a signal is how it
                // was stopped. But the interrupt already wrote what happened,
                // so printing this one put a red `✗ failed` under the green
                // `✓ done · interrupted` for the same turn, the transcript
                // contradicting itself at the exact moment the reader is
                // checking whether their stop worked.
                //
                // `apply` is only ever fed the events of the run being watched,
                // so a stop outstanding on that run is a stop on this ending.
                let stopped_on_purpose = match self.watching.clone() {
                    Some(id) => self.claims_interrupt(&id),
                    None => false,
                };
                if !stopped_on_purpose {
                    self.push(Entry::Done {
                        text: bits.join(" · "),
                        failed: *is_error,
                    });
                }
                // Nothing is in flight once the run has ended, so any step
                // still marked as running is a result that never arrived —
                // a killed run, or a harness that simply does not report one.
                // Left alone it would spin on screen for the rest of the
                // session and, because a running step never folds, would keep
                // a finished turn's plumbing permanently open.
                //
                // Settled as `Ok` rather than `Failed`: a missing result is not
                // evidence that the tool went wrong, and the turn's own ending
                // is printed directly above to carry the bad news when there is
                // any.
                for entry in self.transcript.iter_mut() {
                    match entry {
                        Entry::Tool { step, .. } | Entry::Diff { step, .. }
                            if *step == Step::Running =>
                        {
                            *step = Step::Ok
                        }
                        _ => {}
                    }
                }
                self.busy = false;
                self.turn_started_ms = None;
                self.liveness = None;
            }
            // Nothing in the transcript, on purpose — a tick every few seconds
            // for nine minutes would be nine minutes of scrollback saying
            // "still working", and PR #92 just finished scrubbing spurious
            // entries out of exactly this transcript.
            //
            // The evidence is *stored*, not drawn, here: `activity()` below
            // reads `self.liveness` back for the status bar, which
            // `cli/src/tui/ui.rs` renders. Only ever overwritten by a later
            // tick with a count of its own — a bare tick (no count) still
            // proves the harness is alive without erasing the last number we
            // had — so a run that stops ticking leaves this frozen rather than
            // this code inventing motion for it.
            //
            // This is reasoning silence specifically — see [`Liveness`] for
            // generation silence, right below.
            AgentEvent::Progress { thinking_tokens } => {
                if let Some(t) = thinking_tokens {
                    self.liveness = Some(Liveness::Thinking(*t));
                }
            }
            // Generation silence: a long assistant message — often several
            // `tool_use` blocks in a row, each one's arguments streamed as its
            // own run of these — with no `Thinking`/`Progress` event in
            // between, because the model is not reasoning in that window, it
            // is emitting. Also kept out of the transcript, for the same
            // reason as `Progress` above: one `Delta` per token-ish fragment
            // would flood it, and the complete block still lands there once,
            // as its own `Message`/`ToolCall`, when it finishes.
            AgentEvent::Delta { .. } => {
                self.liveness = Some(Liveness::Generating);
            }
            // The harness's own words first, then Jod's, when Jod can see
            // something the harness did not say. A model this harness has no
            // name for fails every turn identically and says nothing about
            // itself, so the transcript has to carry the reason: the message
            // above is the same one whatever went wrong, and on its own it
            // sends the reader to a server log they do not have.
            AgentEvent::Error { message } => {
                self.push(Entry::Notice(message.clone()));
                if let Some(objection) = self.model.clone().and_then(|m| self.model_objection(&m)) {
                    self.push(Entry::Notice(objection));
                }
            }
            AgentEvent::Raw { line } => {
                if !line.trim().is_empty() {
                    self.push(Entry::Raw(line.clone()));
                }
            }
        }
    }

    /// Turn what the user typed at `/resume` into a harness session id.
    ///
    /// The agents panel shows a *shortened Jod agent id*, and `/sessions` tells
    /// you to feed it to `/resume` — but `/resume` hands its argument to the
    /// harness as a conversation id, which an agent id never is. So a prefix of
    /// either is accepted and translated, and anything unrecognised is passed
    /// through untouched, because a session id copied from elsewhere is still a
    /// legitimate thing to type.
    pub fn resolve_session(&self, typed: &str) -> Resolved {
        let exact_session = self
            .agents
            .iter()
            .find(|a| a.session.as_deref() == Some(typed));
        if let Some(a) = exact_session {
            return Resolved::Session(a.session.clone().unwrap());
        }

        let matches: Vec<&AgentLine> = self
            .agents
            .iter()
            .filter(|a| {
                a.id.starts_with(typed)
                    || a.session.as_deref().is_some_and(|s| s.starts_with(typed))
            })
            .collect();

        match matches.as_slice() {
            [] => Resolved::Verbatim(typed.to_string()),
            [only] => match &only.session {
                Some(s) => Resolved::Session(s.clone()),
                // Known agent, but it never reported a conversation — resuming
                // it would silently start a fresh one instead.
                None => Resolved::NoSession(only.id.clone()),
            },
            many => Resolved::Ambiguous(many.len()),
        }
    }

    /// The one-line summary shown in the status bar.
    ///
    /// Two halves, and they are separate functions because the chat header
    /// prints them on two lines — who is answering above what he is doing about
    /// it. Splitting this string back apart at a `·` would be a second place
    /// that has to know the shape of the first, and the two would drift the
    /// moment either half grew a field.
    pub fn status(&self) -> String {
        format!("{} · {}", self.identity(), self.activity())
    }

    /// Who is answering: the harness, the model it actually ran, and what the
    /// conversation has cost so far.
    pub fn identity(&self) -> String {
        let mut parts = vec![self.harness.label().to_string()];
        // What the harness actually ran beats what was asked for; before the
        // first turn there is nothing to report, so the request stands in.
        if let Some(m) = self.reported_model.as_ref().or(self.model.as_ref()) {
            parts.push(m.clone());
        }
        if self.cost_usd > 0.0 {
            parts.push(format!("${:.4}", self.cost_usd));
        }
        parts.join(" · ")
    }

    /// What he is doing about it: this turn, the runs behind it, and the
    /// prompts waiting their turn.
    pub fn activity(&self) -> String {
        let mut parts = vec![if self.interrupting.is_some() {
            // Said the moment the key is pressed, because the stop is not
            // instant and the alternative was a status that still read
            // `working` over a clock that had stopped ticking. Four seconds of
            // that and the reader presses the key again, or reaches for Ctrl-C.
            format!("{} interrupting…", self.spinner())
        } else if self.busy {
            // The spinner and the elapsed time are the difference between "this
            // is working" and "this has hung", which a static word cannot tell
            // you during a run that legitimately takes ten minutes.
            match self.elapsed() {
                Some(t) => format!("{} working {t}", self.spinner()),
                None => format!("{} working", self.spinner()),
            }
        } else {
            "ready".into()
        }];
        // The one thing on the wire during a long, silent turn — see
        // [`Liveness`]. Shown only while genuinely mid-turn (not while a stop
        // is already winding one down) and only once some evidence has
        // actually arrived, so a harness that never sends any leaves this
        // exactly as quiet as before. Each variant decides for itself whether
        // `show_thinking` hides it — today's only variant is reasoning, which
        // is "the same information as `Thinking`, counted rather than quoted"
        // (see `cli/src/render.rs`) and so follows the same flag.
        if self.busy && self.interrupting.is_none() {
            if let Some(note) = self.liveness.and_then(|l| l.describe(self.show_thinking)) {
                parts.push(note);
            }
        }
        // Background work is the reason this is an orchestrator, so it is stated
        // even when the conversation on screen is idle. Only the agents *other*
        // than the watched one are counted: the watched one already said so.
        let background = self
            .agents
            .iter()
            .filter(|a| a.is_running() && Some(a.id.as_str()) != self.watching.as_deref())
            .count();
        if background > 0 {
            parts.push(format!("{background} in background"));
        }
        // Somebody has stopped and is waiting on an answer. This is the one
        // state that costs real time to miss: a blocked agent is not working
        // and will not start again on its own, and until now the only place
        // that said so was the rail — a panel that is closed by default, so the
        // news reached exactly the readers who already knew to go looking.
        if let Some(waiting) = self.waiting_on_you() {
            parts.push(waiting);
        }
        if !self.queued.is_empty() {
            parts.push(format!("{} queued", self.queued.len()));
        }
        parts.join(" · ")
    }

    /// Who has stopped and is waiting for an answer, if anyone.
    ///
    /// A manager is named rather than counted, because "a manager is waiting"
    /// and "something is waiting" prompt different actions: the manager is the
    /// one that has a project's work queued behind it. Managers are recognised
    /// by their conversation, which is the only thing that makes a manager a
    /// manager — there is no manager record to consult.
    pub fn waiting_on_you(&self) -> Option<String> {
        let blocked: Vec<&Card> = self
            .cards
            .iter()
            .filter(|c| c.blocking && c.is_open())
            .collect();
        if blocked.is_empty() {
            return None;
        }
        let managers = blocked
            .iter()
            .filter(|c| self.is_manager_conversation(&c.conversation_id))
            .count();
        Some(match (managers, blocked.len()) {
            (0, n) => format!("{n} waiting on you"),
            (1, 1) => "a manager is waiting on you".to_string(),
            (m, n) if m == n => format!("{m} managers waiting on you"),
            (_, n) => format!("{n} waiting on you, one a manager"),
        })
    }

    /// Whether this conversation is some project's manager.
    ///
    /// Asked of the forest rather than the store: a manager row is already
    /// keyed by the conversation it stands for — see [`NodeId::manager`] — so
    /// the answer is here, and a second query would be a slower way to learn
    /// what the screen already knows.
    ///
    /// Public because the key handler asks it too: `←` on an empty line backs
    /// out of a manager and is the cursor everywhere else, and this is what
    /// tells the two apart.
    pub fn is_manager_conversation(&self, conversation_id: &str) -> bool {
        self.forest
            .iter()
            .any(|n| n.id == NodeId::manager(conversation_id))
    }

    /// Which conversation the composer is about to send to, in words.
    ///
    /// Every one of these looks the same from the chair. The banner, the
    /// composer's prompt and the status bar are identical in the main chat and
    /// in any manager, and the transcript is titled after the *run* being
    /// watched — so a manager, which is entered and then sits there with no run
    /// of its own, was titled plainly `jod`. Walk away, come back, and there is
    /// nothing on screen that says whether the next thing typed routes across
    /// every project or lands in one of them.
    ///
    /// That is worth a word in the title because the mistake it prevents is
    /// silent: an instruction meant for main, typed into beta's manager, is not
    /// refused — it is carried out, in beta.
    ///
    /// `None` when the answer would be noise: no conversation bound yet, or an
    /// ordinary session, which the run's own name already covers.
    pub fn where_you_are(&self) -> Option<String> {
        let conversation = self.conversation.as_deref()?;
        if self
            .forest
            .iter()
            .any(|n| n.id == NodeId::main(conversation))
        {
            return Some("main".to_string());
        }
        if !self.is_manager_conversation(conversation) {
            return None;
        }
        // Named after its project rather than "manager", because "manager" is
        // the answer to a question nobody asked — there is one per project and
        // which project is the whole point.
        let project = self
            .projects
            .iter()
            .find(|p| p.manager_conversation_id.as_deref() == Some(conversation))
            .map(|p| p.name.clone());
        Some(match project {
            Some(name) => format!("{name} · manager"),
            // The forest says it is a manager and the catalog does not say
            // whose. Saying "a manager" is still worth more than saying
            // nothing, because the thing being prevented is thinking you are
            // in main.
            None => "a project manager".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::Usage;

    fn app() -> App {
        App::new(HarnessKind::ClaudeCode, None, Resume::Fresh)
    }

    fn blocking_card(conversation: &str) -> Card {
        use jod_core::cards::{CardKind, Delivery, Importance, Source, Status};
        Card {
            id: 1,
            conversation_id: conversation.into(),
            work_id: None,
            run_id: None,
            kind: CardKind::Question,
            importance: Importance::Normal,
            blocking: true,
            status: Status::Open,
            delivery: Delivery::None,
            title: "which database?".into(),
            body: String::new(),
            options: vec![],
            chosen: None,
            answer: None,
            secret_name: None,
            secret_scope: None,
            source: Source::Mcp,
            created_at_ms: 0,
            updated_at_ms: 0,
            answered_at_ms: None,
            delivered_at_ms: None,
            dedupe_key: None,
        }
    }

    fn manager_node(conversation: &str) -> Node {
        Node {
            id: NodeId::manager(conversation),
            parent: None,
            kind: NodeKind::Manager,
            depth: 1,
            label: "manager".into(),
            summary: String::new(),
            running: false,
            status: None,
            stalled_for_ms: None,
            cards: 1,
            blocked: 1,
            stalled: 0,
            colour: String::new(),
            branch: None,
            worktree: None,
            expanded: false,
            has_children: false,
        }
    }

    // ---- which conversation the composer is pointed at ----

    /// Main and a manager are the same picture, and typing does different
    /// things in them. An instruction meant for main, typed into beta's
    /// manager, is not refused — it is carried out, in beta — so the screen has
    /// to say which one is under the cursor before the Enter, not after.
    #[test]
    fn the_screen_says_whether_you_are_in_main_or_in_a_managers_chat() {
        use jod_core::projects::{Project, State};

        let main_node = |conversation: &str| Node {
            id: NodeId::main(conversation),
            parent: None,
            kind: NodeKind::Main,
            depth: 0,
            label: "jod".into(),
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
        };

        let mut a = app();
        a.forest = vec![main_node("c-main"), manager_node("c-alpha")];
        a.projects = vec![Project {
            id: "p-alpha".into(),
            name: "alpha".into(),
            path: "/tmp/alpha".into(),
            remote: None,
            aliases: Vec::new(),
            state: State::Active,
            colour: String::new(),
            notes: String::new(),
            created_at_ms: 0,
            last_touched_ms: 0,
            manager_conversation_id: Some("c-alpha".into()),
        }];

        a.conversation = Some("c-main".into());
        assert_eq!(a.where_you_are().as_deref(), Some("main"));

        a.conversation = Some("c-alpha".into());
        assert_eq!(
            a.where_you_are().as_deref(),
            Some("alpha · manager"),
            "named after its project, because which project is the whole point",
        );

        // An ordinary session says nothing: the run's own name already titles
        // the transcript, and a second label would be noise.
        a.conversation = Some("c-someone-else".into());
        assert_eq!(a.where_you_are(), None);

        // Nothing bound yet is not "main" — saying so would be a claim about
        // where an Enter lands that nothing has decided.
        a.conversation = None;
        assert_eq!(a.where_you_are(), None);
    }

    // ---- notices raised off the chat screen ----

    /// The fault this exists to fix, stated as a test. Eleven presses of `x` on
    /// the wrong fleet row put eleven identical paragraphs into a conversation
    /// that had not been part of any of it.
    #[test]
    fn a_notice_raised_off_the_chat_screen_stays_off_the_conversation() {
        let mut a = app();
        a.go(Workspace::Fleet);
        for _ in 0..11 {
            a.push(Entry::Notice("`x` works on a project row".into()));
            // A separate keypress each time, which is what a tick stands for.
            a.advance(a.now_ms + 250);
        }
        assert!(a.transcript.is_empty(), "{:?}", a.transcript);
        let flash = a.flash.expect("the last one is still on screen");
        assert_eq!(
            flash.lines,
            vec!["`x` works on a project row".to_string()],
            "one press, one line — the ten before it have gone"
        );
    }

    /// Chat is unchanged. The transcript is on screen there, so the words are
    /// already where they can be read and a flash would be them twice.
    #[test]
    fn a_notice_raised_in_chat_still_goes_into_the_conversation() {
        let mut a = app();
        a.push(Entry::Notice("roots: /srv/jod".into()));
        assert_eq!(a.transcript, vec![Entry::Notice("roots: /srv/jod".into())]);
        assert!(a.flash.is_none());
    }

    /// Only Jod's own voice moves. A harness that answers while the cursor is
    /// on the fleet is still the conversation's record, and losing it after
    /// four seconds would lose the run's output.
    #[test]
    fn what_the_harness_says_reaches_the_conversation_from_any_screen() {
        let mut a = app();
        a.go(Workspace::Fleet);
        a.push(Entry::Agent("the parser is ported".into()));
        assert_eq!(a.transcript, vec![Entry::Agent("the parser is ported".into())]);
        assert!(a.flash.is_none());
    }

    /// `Action::Sessions` pushes its answer one row at a time. Each replacing
    /// the last would show the fiftieth conversation and none of the other
    /// forty-nine.
    #[test]
    fn every_line_of_one_answer_collects_into_one_flash() {
        let mut a = app();
        a.go(Workspace::Fleet);
        for row in ["46 conversations", "a2ddcf7c  build a racing game", "b7239986  racing-3d"] {
            a.push(Entry::Notice(row.into()));
        }
        let flash = a.flash.expect("a flash");
        assert_eq!(flash.lines.len(), 3, "{:?}", flash.lines);
        assert!(flash.lines[0].contains("46 conversations"));
    }

    /// And a later keypress is a different answer, not more of the last one.
    #[test]
    fn a_later_keypress_starts_a_fresh_flash() {
        let mut a = app();
        a.go(Workspace::Fleet);
        a.push(Entry::Notice("sorted by newest".into()));
        a.advance(a.now_ms + 250);
        a.push(Entry::Notice("closed works hidden".into()));
        assert_eq!(
            a.flash.expect("a flash").lines,
            vec!["closed works hidden".to_string()]
        );
    }

    /// The whole difference between a flash and a transcript line: nobody has
    /// to press anything to be rid of it.
    #[test]
    fn a_flash_goes_away_on_its_own() {
        let mut a = app();
        a.go(Workspace::Fleet);
        a.push(Entry::Notice("nothing to stop".into()));
        a.advance(a.now_ms + flash_ms(1) - 1);
        assert!(a.flash.is_some(), "still inside its time");
        a.advance(a.now_ms + 2);
        assert!(a.flash.is_none(), "and gone after it");
    }

    /// A long answer is up for longer, because four seconds is a glance and the
    /// session list is fifty rows.
    #[test]
    fn a_longer_answer_stays_up_for_longer() {
        assert!(flash_ms(40) > flash_ms(1));
        assert!(flash_ms(1_000) <= 20_000, "and never indefinitely");
    }

    /// It belongs to the screen that raised it. Carried across, a refusal about
    /// a fleet row would hang over the memory list, and would be back on screen
    /// the next time the fleet was opened — which reads as it happening again.
    #[test]
    fn leaving_the_screen_takes_the_flash_with_it() {
        let mut a = app();
        a.go(Workspace::Fleet);
        a.push(Entry::Notice("nothing to stop".into()));
        assert!(a.flash.is_some());
        a.go(Workspace::Memory);
        assert!(a.flash.is_none());
    }

    fn main_node(conversation: &str) -> Node {
        Node {
            id: NodeId::main(conversation),
            parent: None,
            kind: NodeKind::Main,
            depth: 0,
            label: "jod".into(),
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
        }
    }

    /// One row for the chat, whichever of the two provides it.
    ///
    /// `fleet::main_id` is a sentinel from when core's forest could not carry
    /// the pinned chat. It can now, and prepending the sentinel unconditionally
    /// put two rows for one conversation at the top of the fleet.
    #[test]
    fn the_pinned_chat_gets_one_row_when_core_already_minted_it() {
        let mut a = app();
        a.forest = vec![main_node("conv-1")];

        let rows = a.tree_rows();
        assert_eq!(
            rows.iter().filter(|id| id.kind_tag == "main").count(),
            1,
            "one chat, one row: {rows:?}"
        );
        assert_eq!(rows[0], NodeId::main("conv-1"), "core's row, not the sentinel");
    }

    /// And the sentinel is still there when core has nothing pinned to offer,
    /// which is the guarantee it was added for: a fleet with no row for the
    /// chat is a screen you can walk into and not back out of.
    #[test]
    fn the_pinned_chat_falls_back_to_the_sentinel_when_core_has_no_row() {
        let mut a = app();
        a.forest = vec![manager_node("c-1")];

        let rows = a.tree_rows();
        assert_eq!(rows[0], main_id(), "{rows:?}");
    }

    /// Either row means the same place, so the cursor test has to accept both —
    /// otherwise every verb keyed off "is the cursor on the chat" goes quiet the
    /// moment core starts minting the row.
    #[test]
    fn the_cursor_is_on_the_chat_on_either_of_its_two_rows() {
        let mut a = app();
        a.forest = vec![main_node("conv-1")];

        a.tree.selected = Some(NodeId::main("conv-1"));
        assert!(a.tree_main_selected(), "core's row");

        a.tree.selected = Some(main_id());
        assert!(a.tree_main_selected(), "the sentinel");

        a.tree.selected = Some(NodeId::manager("c-1"));
        assert!(!a.tree_main_selected(), "and nothing else is");
    }

    /// A heredoc body is a file, and it already has a place to be — the diff
    /// underneath. Flattened into the summary it spent the line, and often a
    /// second wrapped one, on the first ninety characters of that file.
    #[test]
    fn a_summary_line_leaves_out_what_a_heredoc_was_filled_with() {
        let detail = tool_detail(&serde_json::json!({
            "command": "cat > src/car.js <<'EOF'\nexport const DRAG = 0.98;\nexport const GRIP = 1.2;\nEOF",
        }))
        .unwrap();
        assert_eq!(detail, "cat > src/car.js <<'EOF'");
    }

    /// A command with no heredoc is untouched.
    #[test]
    fn an_ordinary_command_keeps_its_summary() {
        let detail = tool_detail(&serde_json::json!({ "command": "pnpm test --run" })).unwrap();
        assert_eq!(detail, "pnpm test --run");
    }

    /// A blocked agent is not working and will not start again on its own, and
    /// the only place that said so was a panel closed by default.
    #[test]
    fn a_blocked_agent_says_so_on_the_line_that_is_always_visible() {
        let mut a = app();
        assert!(a.waiting_on_you().is_none(), "nothing is blocked yet");

        a.cards = vec![blocking_card("c-1")];
        assert_eq!(a.waiting_on_you().as_deref(), Some("1 waiting on you"));
        assert!(
            a.activity().contains("waiting on you"),
            "and it reaches the status line: {}",
            a.activity()
        );
    }

    /// "A manager is waiting" and "something is waiting" prompt different
    /// actions: a manager has a project's work queued behind it.
    #[test]
    fn a_waiting_manager_is_named_rather_than_counted() {
        let mut a = app();
        a.cards = vec![blocking_card("c-1")];
        a.forest = vec![manager_node("c-1")];
        assert_eq!(
            a.waiting_on_you().as_deref(),
            Some("a manager is waiting on you")
        );
    }

    /// An answered card is not still waiting for anybody.
    #[test]
    fn a_card_that_has_been_answered_stops_counting_as_waiting() {
        let mut a = app();
        let mut card = blocking_card("c-1");
        card.status = jod_core::cards::Status::Answered;
        a.cards = vec![card];
        assert!(a.waiting_on_you().is_none());
    }

    /// The memory list holds the most-connected few hundred, so the count says
    /// which part of the graph that is. A browser that counted its own rows
    /// would claim to be showing everything, which is the one thing it must
    /// never get wrong.
    #[test]
    fn the_memory_count_admits_when_it_is_showing_part_of_the_graph() {
        let mut a = app();
        a.memory = vec![memory_node("reljod"), memory_node("linear")];

        a.graph_size = (2, 1);
        let whole = a.count_for(Workspace::Memory);
        assert!(whole.starts_with("2 nodes"), "{whole}");
        assert!(
            !whole.contains(" of "),
            "nothing is hidden, so say nothing: {whole}"
        );

        a.graph_size = (142, 96);
        let part = a.count_for(Workspace::Memory);
        assert!(part.starts_with("2 of 142 nodes"), "{part}");
        assert!(
            part.contains("96 edges"),
            "the edges are the graph's too: {part}"
        );
    }

    fn memory_node(name: &str) -> MemoryNode {
        MemoryNode {
            id: name.into(),
            name: name.into(),
            kind: MemoryKind::Belief,
            confidence: 1.0,
            degree: 1,
            age_ms: 0,
            seen: 1,
            body: String::new(),
            contradicted: false,
            in_edges: vec![],
            out_edges: vec![],
            provenance: vec![],
        }
    }

    fn typed(text: &str) -> App {
        let mut a = app();
        for c in text.chars() {
            a.insert(c);
        }
        a
    }

    #[test]
    fn typing_advances_the_cursor_to_the_end() {
        let a = typed("hello");
        assert_eq!(a.input, "hello");
        assert_eq!(a.cursor_column(), 5);
    }

    #[test]
    fn inserting_mid_line_lands_where_the_cursor_is() {
        let mut a = typed("helo");
        a.left();
        a.insert('l');
        assert_eq!(a.input, "hello");
    }

    #[test]
    fn backspace_removes_the_character_before_the_cursor() {
        let mut a = typed("hello");
        a.backspace();
        assert_eq!(a.input, "hell");
        assert_eq!(a.cursor_column(), 4);
    }

    #[test]
    fn backspace_at_the_start_is_harmless() {
        let mut a = app();
        a.backspace();
        assert_eq!(a.input, "");
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn delete_forward_removes_the_character_under_the_cursor() {
        let mut a = typed("hello");
        a.home();
        a.delete_forward();
        assert_eq!(a.input, "ello");
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn delete_forward_at_the_end_is_harmless() {
        let mut a = typed("hi");
        a.delete_forward();
        assert_eq!(a.input, "hi");
    }

    /// A prompt is as likely to contain "café" or an emoji as ASCII, and a
    /// byte-indexed cursor that lands mid-character panics on slicing.
    #[test]
    fn editing_multibyte_text_never_splits_a_character() {
        let mut a = typed("café ☕");
        a.backspace();
        assert_eq!(a.input, "café ");
        a.backspace();
        assert_eq!(a.input, "café");
        a.backspace();
        assert_eq!(a.input, "caf");
    }

    #[test]
    fn moving_across_multibyte_text_lands_on_boundaries() {
        let mut a = typed("é☕x");
        a.home();
        a.right();
        a.right();
        a.insert('!');
        assert_eq!(a.input, "é☕!x");
    }

    #[test]
    fn the_cursor_column_counts_characters_not_bytes() {
        let a = typed("é☕");
        assert_eq!(a.cursor_column(), 2, "two characters, four-plus bytes");
    }

    #[test]
    fn home_and_end_move_to_the_extremes() {
        let mut a = typed("hello");
        a.home();
        assert_eq!(a.cursor, 0);
        a.end();
        assert_eq!(a.cursor_column(), 5);
    }

    #[test]
    fn deleting_a_word_removes_it_and_the_space_before_the_cursor() {
        let mut a = typed("summarise the inbox");
        a.delete_word();
        assert_eq!(a.input, "summarise the ");
    }

    #[test]
    fn deleting_a_word_from_trailing_space_removes_the_previous_word() {
        let mut a = typed("hello world   ");
        a.delete_word();
        assert_eq!(a.input, "hello ");
    }

    #[test]
    fn deleting_a_word_on_an_empty_line_is_harmless() {
        let mut a = app();
        a.delete_word();
        assert_eq!(a.input, "");
    }

    #[test]
    fn clearing_the_line_empties_it_and_resets_the_cursor() {
        let mut a = typed("throw this away");
        a.clear_line();
        assert_eq!(a.input, "");
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn taking_input_returns_the_text_and_empties_the_box() {
        let mut a = typed("  do the thing  ");
        assert_eq!(a.take_input().as_deref(), Some("do the thing"));
        assert_eq!(a.input, "");
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn taking_blank_input_yields_nothing_and_sends_no_prompt() {
        let mut a = typed("    ");
        assert_eq!(a.take_input(), None);
    }

    // ---- scrollback ----

    /// Reading back through the transcript must not be yanked away by an agent
    /// that is still producing output.
    #[test]
    fn new_output_does_not_drag_a_scrolled_up_view_back_down() {
        let mut a = app();
        a.push(Entry::Agent("one".into()));
        a.scroll_up(1, 10);
        let was = a.scroll;
        a.push(Entry::Agent("two".into()));
        assert!(a.scroll > was, "the view must hold its place in the text");
        assert!(!a.following());
    }

    #[test]
    fn a_view_at_the_bottom_keeps_following_new_output() {
        let mut a = app();
        a.push(Entry::Agent("one".into()));
        assert!(a.following());
        a.push(Entry::Agent("two".into()));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn scrolling_cannot_go_above_the_top_or_below_the_bottom() {
        let mut a = app();
        a.scroll_up(500, 3);
        assert_eq!(a.scroll, 3, "clamped to the top of the transcript");
        a.scroll_down(500);
        assert_eq!(a.scroll, 0, "clamped to the bottom");
    }

    #[test]
    fn jumping_to_the_bottom_resumes_following() {
        let mut a = app();
        a.scroll_up(5, 10);
        a.scroll_to_bottom();
        assert!(a.following());
    }

    // ---- events ----

    #[test]
    fn a_message_becomes_a_transcript_entry() {
        let mut a = app();
        a.apply(&AgentEvent::Message { text: "hi".into() });
        assert_eq!(a.transcript, vec![Entry::Agent("hi".into())]);
    }

    // ---- a model name the harness has no model for ----

    /// What OpenCode really answers when it is asked for a name it does not
    /// have. It names neither the model nor the problem, which is the whole
    /// reason Jod has to.
    const OPAQUE: &str = "UnknownError: Unexpected server error. Check server logs for details.";

    /// An OpenCode session whose model list has loaded, cut down to two rows.
    fn opencode(model: Option<&str>) -> App {
        let mut a = App::new(
            HarnessKind::OpenCode,
            model.map(str::to_string),
            Resume::Fresh,
        );
        a.models = jod_core::harness::models::parse(
            HarnessKind::OpenCode,
            "opencode/claude-opus-5\nopencode/hy3-free\n",
        );
        a.models_for = Some(HarnessKind::OpenCode);
        a
    }

    /// The failure in full. Main was on OpenCode holding `claude-opus-5`, which
    /// is Claude Code's spelling, so every turn died on the server error above.
    /// Naming the id OpenCode does use is the difference between a dead end and
    /// a fix, so the objection has to carry it.
    #[test]
    fn a_model_the_harness_lacks_is_named_along_with_the_one_it_has() {
        let said = opencode(None).model_objection("claude-opus-5").unwrap();
        assert!(
            said.contains("OpenCode has no model called claude-opus-5"),
            "{said}"
        );
        assert!(said.contains("/model opencode/claude-opus-5"), "{said}");
    }

    /// Nothing to object to. A name on the list is the ordinary case and must
    /// stay silent, or the objection is noise on every turn.
    #[test]
    fn a_model_the_harness_has_draws_no_objection() {
        assert_eq!(opencode(None).model_objection("opencode/hy3-free"), None);
    }

    /// An empty list is a harness that could not be asked — no binary, a failed
    /// subcommand, an answer still in flight. Convicting a name on that would
    /// refuse every `/model` on a machine where `opencode models` happens not
    /// to run, which is worse than the bug this fixes.
    #[test]
    fn a_list_that_never_loaded_convicts_nothing() {
        let mut a = opencode(None);
        a.models.clear();
        assert_eq!(a.model_objection("claude-opus-5"), None);
    }

    /// `/harness` mid-session leaves the previous harness's list in place until
    /// the new one answers. Checking a name against it would judge it by the
    /// wrong harness, which is exactly the confusion being fixed.
    #[test]
    fn a_list_belonging_to_another_harness_convicts_nothing() {
        let mut a = opencode(None);
        a.models_for = Some(HarnessKind::ClaudeCode);
        assert_eq!(a.model_objection("claude-opus-5"), None);
    }

    /// The transcript keeps the harness's own words and adds the reason under
    /// them. This is what unsticks a conversation whose model was set before
    /// anything checked it: the error alone repeats forever and explains
    /// nothing.
    #[test]
    fn the_opaque_harness_error_is_followed_by_the_reason_for_it() {
        let mut a = opencode(Some("claude-opus-5"));
        a.apply(&AgentEvent::Error {
            message: OPAQUE.into(),
        });
        assert_eq!(a.transcript.len(), 2, "{:?}", a.transcript);
        assert_eq!(a.transcript[0], Entry::Notice(OPAQUE.into()));
        match &a.transcript[1] {
            Entry::Notice(said) => {
                assert!(said.contains("/model opencode/claude-opus-5"), "{said}")
            }
            other => panic!("expected the reason, got {other:?}"),
        }
    }

    /// An error with nothing wrong with the model is somebody else's problem —
    /// a rate limit, a dropped connection, a bad tool call. Blaming the model
    /// for those would send the reader after the one thing that is fine.
    #[test]
    fn an_error_under_a_model_the_harness_has_gains_nothing() {
        let mut a = opencode(Some("opencode/hy3-free"));
        a.apply(&AgentEvent::Error {
            message: "APIError: rate limited".into(),
        });
        assert_eq!(
            a.transcript,
            vec![Entry::Notice("APIError: rate limited".into())]
        );
    }

    /// The database half of this fix cannot reach a console that is already
    /// running: the cursor it launches turns from is this field. Without this
    /// arm the row is repaired and the very next keystroke sends the dead id
    /// again — which is what three identical failures in a row actually were.
    #[test]
    fn a_lost_session_drops_the_cursor_that_keeps_asking_for_it() {
        let mut a = app();
        a.apply(&AgentEvent::Started {
            session_id: Some("sess-abc".into()),
            model: None,
        });
        assert_eq!(a.resume, Resume::Session("sess-abc".into()));

        a.apply(&AgentEvent::SessionLost {
            session_id: "sess-abc".into(),
        });
        assert_eq!(
            a.resume,
            Resume::Fresh,
            "the console would go on resuming a session the harness has lost"
        );
        assert_eq!(a.session, None);
        assert!(
            a.transcript.iter().any(|e| matches!(e, Entry::Notice(_))),
            "a thread that silently starts over is a thread that lost its \
             memory without telling anyone"
        );
    }

    /// A background delegation losing its own session says nothing about this
    /// chat's cursor. Clearing on it would drop a live thread's continuity to
    /// repair one that was never broken.
    #[test]
    fn another_runs_lost_session_leaves_this_chats_cursor_alone() {
        let mut a = app();
        a.apply(&AgentEvent::Started {
            session_id: Some("sess-mine".into()),
            model: None,
        });
        a.apply(&AgentEvent::SessionLost {
            session_id: "sess-somebody-elses".into(),
        });
        assert_eq!(a.resume, Resume::Session("sess-mine".into()));
        assert_eq!(a.session.as_deref(), Some("sess-mine"));
    }

    #[test]
    fn thinking_is_shown_without_being_asked_for() {
        let mut a = app();
        a.apply(&AgentEvent::Thinking { text: "hmm".into() });
        assert_eq!(a.transcript, vec![Entry::Thinking("hmm".into())]);
    }

    #[test]
    fn thinking_can_still_be_turned_off() {
        let mut a = app();
        a.show_thinking = false;
        a.apply(&AgentEvent::Thinking { text: "hmm".into() });
        assert!(a.transcript.is_empty());
    }

    /// A tool already announced adds nothing when it merely worked; a failure
    /// always earns its line, because it explains the answer.
    #[test]
    fn an_announced_tool_that_worked_adds_no_second_line() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "read".into(),
            input: None,
        });
        a.apply(&AgentEvent::ToolResult {
            name: "read".into(),
            summary: None,
            is_error: false,
        });
        assert_eq!(
            a.transcript,
            vec![Entry::Tool {
                name: "read".into(),
                detail: None,
                step: Step::Ok
            }],
            "the result must not repeat the call line"
        );

        a.apply(&AgentEvent::ToolResult {
            name: "write".into(),
            summary: None,
            is_error: true,
        });
        assert_eq!(a.transcript.len(), 2);
        assert!(matches!(
            a.transcript[1],
            Entry::Tool {
                step: Step::Failed,
                ..
            }
        ));
    }

    /// OpenCode reports a fast tool as already `completed`, so no call is ever
    /// announced. Without a line of its own the output rendered as a bare
    /// `└ …` — an answer with its question missing.
    #[test]
    fn a_result_with_no_announced_call_still_names_its_tool() {
        let mut a = app();
        a.apply(&AgentEvent::ToolResult {
            name: "write".into(),
            summary: Some("Wrote file successfully.".into()),
            is_error: false,
        });
        assert_eq!(
            a.transcript,
            vec![
                Entry::Tool {
                    name: "write".into(),
                    detail: None,
                    step: Step::Ok
                },
                Entry::ToolOut {
                    text: "Wrote file successfully.".into(),
                    failed: false
                },
            ]
        );
    }

    /// An edit pushes a diff between its call line and its result, so "was this
    /// announced?" cannot be answered by looking only one entry back. It was,
    /// and the result pushed a second, detail-less `⚙ Edit` under the diff —
    /// which reads as a *new* anonymous call rather than as the old one
    /// finishing. A burst of writes became a stack of them.
    #[test]
    fn an_edit_result_adds_no_second_line_under_its_diff() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Edit".into(),
            input: Some(serde_json::json!({
                "file_path": "/src/game.ts",
                "old_string": "a\nb\nc",
                "new_string": "a\nz\nc",
            })),
        });
        a.apply(&AgentEvent::ToolResult {
            name: "Edit".into(),
            summary: None,
            is_error: false,
        });
        assert_eq!(
            a.transcript.len(),
            2,
            "the call line and its diff, and nothing else: {:#?}",
            a.transcript
        );
        assert!(matches!(a.transcript[0], Entry::Tool { .. }));
        assert!(matches!(a.transcript[1], Entry::Diff { .. }));
    }

    /// The same blindness, one step further back: a plan call is folded into the
    /// plan block and pushes no `Tool` line at all, so its result announced
    /// itself as a bare `⚙ TodoWrite` beneath the plan it just revised.
    #[test]
    fn a_plan_result_adds_no_line_under_its_plan() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "TodoWrite".into(),
            input: Some(serde_json::json!({
                "todos": [{"content": "ship it", "status": "pending"}],
            })),
        });
        a.apply(&AgentEvent::ToolResult {
            name: "TodoWrite".into(),
            summary: None,
            is_error: false,
        });
        assert_eq!(
            a.transcript.len(),
            1,
            "the plan block, and nothing else: {:#?}",
            a.transcript
        );
        assert!(matches!(a.transcript[0], Entry::Plan(_)));
    }

    /// The harnesses disagree on how a parameter is spelled. AGY's PascalCase
    /// names fell through and its calls rendered as raw JSON.
    #[test]
    fn a_parameter_is_found_however_the_harness_spells_it() {
        for input in [
            serde_json::json!({"file_path": "/tmp/sq.py"}),
            serde_json::json!({"filePath": "/tmp/sq.py"}),
            serde_json::json!({"TargetFile": "/tmp/sq.py"}),
        ] {
            assert_eq!(tool_detail(&input).as_deref(), Some("/tmp/sq.py"));
        }
        assert_eq!(
            tool_detail(&serde_json::json!({"DirectoryPath": "/home"})).as_deref(),
            Some("/home")
        );
    }

    /// The point of watching a harness work: the transcript must say what the
    /// tool was actually asked to do, not just its name.
    #[test]
    fn a_tool_call_shows_its_most_useful_argument() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Bash".into(),
            input: Some(serde_json::json!({"command": "cargo test --workspace"})),
        });
        assert_eq!(
            a.transcript,
            vec![Entry::Tool {
                name: "Bash".into(),
                detail: Some("cargo test --workspace".into()),
                step: Step::Running
            }]
        );
    }

    #[test]
    fn a_file_tool_shows_its_path() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Read".into(),
            input: Some(serde_json::json!({"file_path": "/src/main.rs"})),
        });
        match &a.transcript[0] {
            Entry::Tool { detail, .. } => assert_eq!(detail.as_deref(), Some("/src/main.rs")),
            other => panic!("expected a tool, got {other:?}"),
        }
    }

    #[test]
    fn an_unrecognised_argument_shape_is_still_shown() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Odd".into(),
            input: Some(serde_json::json!({"wibble": 3})),
        });
        match &a.transcript[0] {
            Entry::Tool { detail, .. } => {
                assert!(detail.as_deref().unwrap().contains("wibble"))
            }
            other => panic!("expected a tool, got {other:?}"),
        }
    }

    #[test]
    fn a_tool_with_no_arguments_shows_just_its_name() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Ls".into(),
            input: Some(serde_json::json!({})),
        });
        match &a.transcript[0] {
            Entry::Tool { detail, .. } => assert!(detail.is_none()),
            other => panic!("expected a tool, got {other:?}"),
        }
    }

    #[test]
    fn tool_output_is_shown_by_default() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Bash".into(),
            input: None,
        });
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("test result: ok. 93 passed".into()),
            is_error: false,
        });
        assert_eq!(
            a.transcript[1],
            Entry::ToolOut {
                text: "test result: ok. 93 passed".into(),
                failed: false
            }
        );
    }

    #[test]
    fn tool_output_can_be_turned_off() {
        let mut a = app();
        a.show_details = false;
        a.apply(&AgentEvent::ToolCall {
            name: "Bash".into(),
            input: None,
        });
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("chatter".into()),
            is_error: false,
        });
        assert_eq!(a.transcript.len(), 1, "the call, but not its output");
    }

    /// A failure is shown whether or not details are on: it is the reason the
    /// answer is about to be wrong.
    #[test]
    fn a_failure_is_shown_even_with_details_off() {
        let mut a = app();
        a.show_details = false;
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("command not found".into()),
            is_error: true,
        });
        assert_eq!(a.transcript.len(), 2, "the failed tool and its output");
        assert!(matches!(
            a.transcript[1],
            Entry::ToolOut { failed: true, .. }
        ));
    }

    /// One turn, run to completion: the user's line, the hand-off, a tool call
    /// and its output, some prose, and the ending.
    fn one_finished_turn() -> App {
        let mut a = app();
        a.push(Entry::You("what is the progress of zuma?".into()));
        a.push(Entry::Routing("→ 16b9a192 · handed to the orchestrator".into()));
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::ToolCall {
            name: "project_switch".into(),
            input: Some(serde_json::json!({"project": "zuma"})),
        });
        a.apply(&AgentEvent::ToolResult {
            name: "project_switch".into(),
            summary: Some("switched".into()),
            is_error: false,
        });
        a.apply(&AgentEvent::Message {
            text: "Zuma is on its third milestone.".into(),
        });
        a
    }

    /// Which entries the transcript would draw, by their debug shape.
    fn shown(a: &App) -> Vec<&Entry> {
        a.transcript
            .iter()
            .enumerate()
            .filter(|(i, _)| !a.hidden(*i))
            .map(|(_, e)| e)
            .collect()
    }

    /// Watching a run is watching it work, so nothing is folded while it is
    /// still going.
    #[test]
    fn the_steps_of_the_turn_in_flight_are_shown() {
        let a = one_finished_turn();
        assert!(a.busy, "the fixture is mid-turn");
        assert_eq!(shown(&a).len(), a.transcript.len(), "nothing folded yet");
    }

    /// The point of the whole slice: once the answer is in, the steps that
    /// produced it stop being the transcript.
    #[test]
    fn the_steps_fold_away_when_the_turn_ends() {
        let mut a = one_finished_turn();
        a.apply(&AgentEvent::Finished {
            text: None,
            exit_code: Some(0),
            is_error: false,
            usage: Usage::default(),
        });
        let left = shown(&a);
        assert!(
            left.iter().all(|e| !matches!(
                e,
                Entry::Tool { .. } | Entry::ToolOut { .. } | Entry::Routing(_)
            )),
            "a step survived the ending: {left:?}"
        );
        assert_eq!(left.len(), 3, "the question, the answer and the ending");
    }

    /// And `Ctrl-O` brings them back, which is what makes folding them safe.
    #[test]
    fn ctrl_o_unfolds_the_steps_of_a_finished_turn() {
        let mut a = one_finished_turn();
        a.busy = false;
        a.expand_details = true;
        assert_eq!(shown(&a).len(), a.transcript.len(), "all of it, again");
    }

    /// Details off is about the steps, not about the failures. A tool that
    /// failed is the reason the answer is about to be wrong.
    #[test]
    fn a_failed_step_is_never_folded() {
        let mut a = app();
        a.show_details = false;
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("command not found".into()),
            is_error: true,
        });
        assert_eq!(shown(&a).len(), 2, "the failed call and what it said");
    }

    /// With details off the steps are not shown even as they happen — which is
    /// the difference between that setting and the fold.
    #[test]
    fn details_off_hides_the_call_line_while_it_runs() {
        let mut a = one_finished_turn();
        a.show_details = false;
        assert!(a.busy);
        let left = shown(&a);
        assert_eq!(left.len(), 2, "the question and the answer: {left:?}");
    }

    #[test]
    fn long_tool_output_is_cut_down_with_a_count() {
        let mut a = app();
        let long = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.apply(&AgentEvent::ToolCall {
            name: "Bash".into(),
            input: None,
        });
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some(long),
            is_error: false,
        });
        match &a.transcript[1] {
            Entry::ToolOut { text, .. } => {
                assert!(text.contains("line 0"));
                assert!(text.contains("more lines"), "got {text}");
                assert!(text.lines().count() <= 8);
            }
            other => panic!("expected output, got {other:?}"),
        }
    }

    #[test]
    fn empty_tool_output_is_not_shown() {
        let mut a = app();
        a.apply(&AgentEvent::ToolCall {
            name: "Bash".into(),
            input: None,
        });
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("   ".into()),
            is_error: false,
        });
        assert_eq!(a.transcript.len(), 1, "the call, and no empty output line");
    }

    /// The next turn must continue *this* conversation, not "the most recent",
    /// which another process could have changed underneath us.
    #[test]
    fn starting_pins_the_next_turn_to_this_exact_session() {
        let mut a = app();
        a.apply(&AgentEvent::Started {
            session_id: Some("sess-1".into()),
            model: Some("some-model".into()),
        });
        assert_eq!(a.resume, Resume::Session("sess-1".into()));
        assert_eq!(a.session.as_deref(), Some("sess-1"));
        // Reported for display, never folded into the request.
        assert_eq!(a.reported_model.as_deref(), Some("some-model"));
    }

    #[test]
    fn finishing_clears_busy_and_accumulates_cost() {
        let mut a = app();
        a.busy = true;
        a.apply(&AgentEvent::Finished {
            text: None,
            exit_code: Some(0),
            is_error: false,
            usage: Usage {
                cost_usd: Some(0.25),
                ..Default::default()
            },
        });
        assert!(!a.busy);
        assert_eq!(a.cost_usd, 0.25);
        assert!(matches!(a.transcript[0], Entry::Done { failed: false, .. }));
    }

    #[test]
    fn cost_accumulates_across_turns() {
        let mut a = app();
        for _ in 0..2 {
            a.apply(&AgentEvent::Finished {
                text: None,
                exit_code: Some(0),
                is_error: false,
                usage: Usage {
                    cost_usd: Some(0.10),
                    ..Default::default()
                },
            });
        }
        assert!((a.cost_usd - 0.20).abs() < 1e-9);
    }

    #[test]
    fn a_failed_run_is_marked_as_such() {
        let mut a = app();
        a.apply(&AgentEvent::Finished {
            text: None,
            exit_code: Some(1),
            is_error: true,
            usage: Usage::default(),
        });
        assert!(matches!(a.transcript[0], Entry::Done { failed: true, .. }));
    }

    #[test]
    fn an_unclassified_harness_line_is_shown_not_dropped() {
        let mut a = app();
        a.apply(&AgentEvent::Raw {
            line: "something odd".into(),
        });
        assert_eq!(a.transcript, vec![Entry::Raw("something odd".into())]);
    }

    #[test]
    fn a_blank_raw_line_is_not_worth_a_row() {
        let mut a = app();
        a.apply(&AgentEvent::Raw { line: "   ".into() });
        assert!(a.transcript.is_empty());
    }

    #[test]
    fn the_status_line_names_the_harness_and_whether_it_is_working() {
        let mut a = app();
        assert!(a.status().contains("Claude Code"));
        assert!(a.status().contains("ready"));
        a.busy = true;
        assert!(a.status().contains("working"));
    }

    #[test]
    fn the_status_line_shows_cost_only_once_there_is_some() {
        let mut a = app();
        assert!(!a.status().contains('$'));
        a.cost_usd = 0.5;
        assert!(a.status().contains("$0.5000"));
    }

    // ---- recalling what was sent ----

    /// Retyping a prompt to change one word is the commonest thing there is,
    /// and without recall the only way to do it is to type it again.
    /// Type a line and send it, as pressing Enter would.
    fn send(a: &mut App, text: &str) {
        for c in text.chars() {
            a.insert(c);
        }
        a.take_input();
    }

    #[test]
    fn the_arrows_walk_back_through_what_was_sent() {
        let mut a = app();
        send(&mut a, "first");
        send(&mut a, "second");

        a.history_prev();
        assert_eq!(a.input, "second", "the newest comes back first");
        a.history_prev();
        assert_eq!(a.input, "first");
        a.history_prev();
        assert_eq!(a.input, "first", "the top of the history is a floor");
        a.history_next();
        assert_eq!(a.input, "second");
    }

    /// Walking into the history and back out must return the half-written line,
    /// not an empty box — losing a sentence to a stray ↑ is unforgivable.
    #[test]
    fn walking_back_out_of_the_history_restores_the_line_being_written() {
        let mut a = app();
        send(&mut a, "done");
        for c in "half writ".chars() {
            a.insert(c);
        }
        a.history_prev();
        assert_eq!(a.input, "done");
        a.history_next();
        assert_eq!(a.input, "half writ", "the draft survived the detour");
    }

    #[test]
    fn the_cursor_lands_at_the_end_of_a_recalled_line() {
        let mut a = typed("summarise the inbox");
        a.take_input();
        a.history_prev();
        assert_eq!(a.cursor, a.input.len());
    }

    /// A history of repeats makes ↑ useless: pressing it four times to get past
    /// four identical retries is worse than no history.
    #[test]
    fn sending_the_same_line_twice_stores_it_once() {
        let mut a = app();
        send(&mut a, "again");
        send(&mut a, "again");
        assert_eq!(a.history, vec!["again".to_string()]);
    }

    #[test]
    fn recall_on_an_empty_history_does_nothing() {
        let mut a = typed("mine");
        a.history_prev();
        assert_eq!(a.input, "mine");
        a.history_next();
        assert_eq!(a.input, "mine");
    }

    // ---- queueing ----

    #[test]
    fn queued_prompts_come_back_in_the_order_they_were_typed() {
        let mut a = app();
        a.queue("one".into());
        a.queue("two".into());
        assert_eq!(a.next_queued().as_deref(), Some("one"));
        assert_eq!(a.next_queued().as_deref(), Some("two"));
        assert_eq!(a.next_queued(), None);
    }

    // ---- the fleet ----

    fn line(id: &str, status: &str) -> AgentLine {
        AgentLine {
            delivery: Verdict::Nothing,
            id: id.into(),
            name: format!("job {id}"),
            harness: "Claude Code".into(),
            status: status.into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            cwd: "/srv/reljod/repo".into(),
            last: None,
        }
    }

    /// Move the fleet cursor the way a keypress would.
    fn move_fleet(a: &mut App, delta: isize) {
        let ids = a.row_ids(Workspace::Fleet);
        a.list_mut(Workspace::Fleet).step(delta, &ids);
    }

    #[test]
    fn the_selection_stops_at_both_ends_rather_than_wrapping() {
        let mut a = app();
        a.agents = vec![line("a", "running"), line("b", "completed")];
        a.reconcile();
        // The cursor starts on the work, not on the chat above it.
        assert_eq!(a.selected_agent().unwrap().id, "a");

        // Up from the first agent reaches the pinned chat, and stops there —
        // it is the top of the list, not a row above the top.
        move_fleet(&mut a, -1);
        assert!(
            a.main_selected(),
            "the chat is the row above the first agent"
        );
        move_fleet(&mut a, -1);
        assert!(a.main_selected(), "already at the top");

        move_fleet(&mut a, 1);
        move_fleet(&mut a, 1);
        move_fleet(&mut a, 1);
        assert_eq!(
            a.selected_agent().unwrap().id,
            "b",
            "cannot fall off the bottom"
        );
    }

    #[test]
    fn selecting_in_an_empty_panel_is_harmless() {
        let mut a = app();
        move_fleet(&mut a, 1);
        assert!(a.selected_agent().is_none());
    }

    /// Agents disappear from the list as older runs age out. A cursor left on
    /// one that is gone would act on nothing, or on the wrong row.
    ///
    /// Updated from asserting an index to asserting an *id*: the cursor is now
    /// tracked by id, so "pulled back" means "put on a row that still exists"
    /// rather than "clamped to the last index".
    #[test]
    fn the_cursor_is_pulled_back_when_the_list_shrinks() {
        let mut a = app();
        a.agents = vec![
            line("a", "running"),
            line("b", "running"),
            line("c", "running"),
        ];
        a.reconcile();
        move_fleet(&mut a, 2);
        assert_eq!(a.selected_agent().unwrap().id, "c");
        a.agents.truncate(1);
        a.reconcile();
        assert_eq!(a.selected_agent().unwrap().id, "a");
    }

    /// The pinned chat is the first row whatever the sort, whatever the filter,
    /// and whether or not anything has been delegated. A top row that is only
    /// sometimes there is a top row nobody relies on.
    #[test]
    fn the_pinned_chat_is_always_the_first_fleet_row() {
        let mut a = app();
        assert_eq!(a.row_ids(Workspace::Fleet), vec![MAIN_ROW.to_string()]);

        a.agents = vec![line("a", "running"), line("b", "completed")];
        a.reconcile();
        assert_eq!(a.row_ids(Workspace::Fleet)[0], MAIN_ROW);

        // Through every sort the list offers.
        for sort in 0..4 {
            a.list_mut(Workspace::Fleet).sort = sort;
            assert_eq!(
                a.row_ids(Workspace::Fleet)[0],
                MAIN_ROW,
                "sort {sort} moved the pinned row"
            );
        }
    }

    /// One instruction is one run, so a week of use puts dozens of identical
    /// `main` rows in the fleet — burying the delegated work the list is for,
    /// and none of them being the chat you wanted to get back to.
    #[test]
    fn the_chats_own_runs_collapse_into_the_pinned_row() {
        let mut a = app();
        let mut turn_one = line("r1", "completed");
        turn_one.name = ORCHESTRATOR.into();
        turn_one.created_at_ms = 1_000;
        let mut turn_two = line("r2", "running");
        turn_two.name = ORCHESTRATOR.into();
        turn_two.created_at_ms = 5_000;
        a.agents = vec![turn_one, turn_two, line("delegated", "running")];
        a.reconcile();

        // The list holds the delegated work and nothing else.
        let ids: Vec<&str> = a.fleet_rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["delegated"]);

        let row = a.main_row();
        assert_eq!(row.turns, 2);
        assert_eq!(
            row.last_ms, 5_000,
            "the most recent instruction, not the first"
        );
        assert!(row.is_running(), "one of its turns is still being routed");
    }

    /// The chat exists before anything has been said to it, so the row does
    /// too — and it says so rather than showing a zero that reads as "just now".
    #[test]
    fn an_unused_chat_still_has_a_row_and_claims_nothing() {
        let a = app();
        let row = a.main_row();
        assert_eq!(row.turns, 0);
        assert_eq!(row.last_ms, 0);
        assert!(!row.is_running());
        assert_eq!(row.status, "idle");
    }

    /// The fleet re-sorts under the cursor every four ticks. Tracking a row
    /// index would move the selection onto a different run the moment one
    /// finished — which is exactly when you are about to press a key.
    #[test]
    fn the_fleet_cursor_stays_on_the_run_when_one_finishes_and_the_list_re_sorts() {
        let mut a = app();
        a.agents = vec![
            line("first", "running"),
            line("second", "running"),
            line("third", "running"),
        ];
        a.reconcile();
        move_fleet(&mut a, 2);
        assert_eq!(a.selected_agent().unwrap().id, "third");

        // The first one finishes, so it sorts below the two still running.
        a.agents[0].status = "completed".into();
        a.reconcile();
        assert_eq!(
            a.selected_agent().unwrap().id,
            "third",
            "the cursor followed the run, not the row"
        );
    }

    /// The whole point of delegating is that work continues off screen, so the
    /// status bar has to account for it even when the conversation is idle.
    #[test]
    fn the_status_line_counts_the_agents_working_off_screen() {
        let mut a = app();
        a.agents = vec![
            line("watched", "running"),
            line("other", "running"),
            line("old", "completed"),
        ];
        a.watching = Some("watched".into());
        let status = a.status();
        assert!(
            status.contains("1 in background"),
            "the watched one is not background: {status}"
        );
        assert_eq!(a.running(), 2, "but it is still running");
    }

    #[test]
    fn the_status_line_says_how_many_prompts_are_waiting() {
        let mut a = app();
        a.queue("next".into());
        assert!(a.status().contains("1 queued"), "{}", a.status());
    }

    /// A run that legitimately takes ten minutes is indistinguishable from a
    /// hung one unless something on screen moves.
    #[test]
    fn a_working_status_carries_a_moving_spinner_and_a_clock() {
        let mut a = app();
        a.busy = true;
        a.turn_started_ms = Some(0);
        a.advance(65_000);
        let status = a.status();
        assert!(status.contains("working"), "{status}");
        assert!(status.contains("1m05s"), "elapsed must show: {status}");

        let first = a.spinner();
        a.advance(65_250);
        assert_ne!(first, a.spinner(), "the spinner must actually turn");
    }

    /// A nine-minute think emits nothing else — no text, no tool call — so the
    /// `Progress` tick is the only thing on the wire that can prove the turn is
    /// still moving. Before this the status bar only had the spinner and the
    /// clock, neither of which changes because a real answer is coming versus
    /// because the process died.
    #[test]
    fn a_progress_tick_shows_up_in_the_working_status() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        assert!(
            !a.status().contains("thinking"),
            "nothing to show before the first tick: {}",
            a.status()
        );

        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(1408),
        });
        let status = a.status();
        assert!(
            status.contains("1408 thinking"),
            "the running total must show: {status}"
        );
    }

    /// Ticks carry a running total, not a delta — a later, smaller-looking tick
    /// still replaces the one before it because it is the more current count.
    /// And a run that stops sending ticks must leave the count exactly where it
    /// was, not decay or reset it — a stall has to look like a stall.
    #[test]
    fn a_stalled_run_keeps_showing_its_last_known_count() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(500),
        });
        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(900),
        });
        assert!(a.status().contains("900 thinking"), "{}", a.status());

        // No more ticks arrive — the wire went quiet, same as a real wedge.
        a.advance(1000);
        assert!(
            a.status().contains("900 thinking"),
            "the count must not move on its own: {}",
            a.status()
        );
    }

    /// A tick without a count still proves the harness is alive, but there is
    /// nothing new to report — so the last known count survives it rather than
    /// being blanked out.
    #[test]
    fn a_bare_tick_does_not_erase_the_last_count() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(500),
        });
        a.apply(&AgentEvent::Progress {
            thinking_tokens: None,
        });
        assert!(a.status().contains("500 thinking"), "{}", a.status());
    }

    /// The status bar is not the transcript: watching a harness think is a
    /// choice (`show_thinking`), and turning it off should hide the token
    /// count exactly as it hides `Entry::Thinking` blocks.
    #[test]
    fn turning_off_thinking_hides_the_token_count_too() {
        let mut a = app();
        a.show_thinking = false;
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(1408),
        });
        assert!(
            !a.status().contains("thinking"),
            "hidden by the same flag as the transcript: {}",
            a.status()
        );
    }

    /// A fresh turn must not carry the previous turn's count — otherwise the
    /// status bar would say a new question had already been thought about
    /// before the harness said a word.
    #[test]
    fn a_new_turn_starts_the_count_over() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::Progress {
            thinking_tokens: Some(1408),
        });
        a.apply(&AgentEvent::Finished {
            text: None,
            exit_code: Some(0),
            is_error: false,
            usage: Usage::default(),
        });
        assert_eq!(a.liveness, None, "cleared when the turn ends");

        a.begin_turn("run-2", 0);
        assert!(
            !a.status().contains("thinking"),
            "the new turn starts silent: {}",
            a.status()
        );
    }

    /// The other half of the freeze `Liveness` exists for: a long assistant
    /// message with several tool calls in it produces no `Thinking`/`Progress`
    /// ticks at all — the model is emitting, not reasoning — so `Delta` has to
    /// be enough on its own to keep the status bar honest.
    #[test]
    fn a_delta_fragment_shows_up_as_writing_in_the_status() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        assert!(!a.status().contains("writing"), "{}", a.status());

        a.apply(&AgentEvent::Delta {
            text: "{\"file_path\": \"package.json".into(),
        });
        assert!(
            a.status().contains("writing"),
            "a streamed fragment must prove the turn is still moving: {}",
            a.status()
        );
    }

    /// Unlike the reasoning count, showing that generation is happening at all
    /// is not something a reader would want hidden with `show_thinking` — it
    /// is not reasoning being shown, it is the answer itself arriving.
    #[test]
    fn writing_shows_even_with_thinking_hidden() {
        let mut a = app();
        a.show_thinking = false;
        a.begin_turn("run-1", 0);
        a.apply(&AgentEvent::Delta {
            text: "frag".into(),
        });
        assert!(a.status().contains("writing"), "{}", a.status());
    }

    /// `Delta` must not land in the transcript — the complete block it is a
    /// fragment of shows up there once, on its own, when it finishes.
    #[test]
    fn a_delta_fragment_never_reaches_the_transcript() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        let before = a.transcript.len();
        a.apply(&AgentEvent::Delta {
            text: "some text".into(),
        });
        assert_eq!(
            a.transcript.len(),
            before,
            "a streaming fragment belongs on the status line, not the transcript"
        );
    }

    #[test]
    fn a_duration_is_written_at_the_scale_it_deserves() {
        assert_eq!(short_duration(0), "0s");
        assert_eq!(short_duration(9_400), "9s");
        assert_eq!(short_duration(252_000), "4m12s");
        assert_eq!(short_duration(7_500_000), "2h05m");
        assert_eq!(short_duration(-5), "0s", "a clock that went backwards");
    }

    /// How long a turn took is the number you want when deciding whether to
    /// delegate the next one, and it is gone the moment the run ends.
    #[test]
    fn a_finished_turn_reports_how_long_it_took() {
        let mut a = app();
        a.turn_started_ms = Some(0);
        a.advance(30_000);
        a.apply(&AgentEvent::Finished {
            text: None,
            exit_code: Some(0),
            is_error: false,
            usage: Usage::default(),
        });
        match &a.transcript[0] {
            Entry::Done { text, .. } => assert!(text.contains("30s"), "got {text}"),
            other => panic!("expected a done line, got {other:?}"),
        }
        assert_eq!(a.turn_started_ms, None, "the clock stops");
    }

    /// Picking up an OpenCode run while the UI is set to Claude Code would hand
    /// the session id to a harness that has never heard of it.
    #[test]
    fn continuing_a_run_switches_to_the_harness_that_produced_it() {
        let mut a = app();
        a.model = Some("opus".into());
        a.harness_from_label("OpenCode");
        assert_eq!(a.harness, HarnessKind::OpenCode);
        assert_eq!(a.model, None, "the model name does not cross the seam");
    }

    #[test]
    fn continuing_a_run_from_the_same_harness_keeps_the_chosen_model() {
        let mut a = app();
        a.model = Some("haiku".into());
        a.harness_from_label("Claude Code");
        assert_eq!(a.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn an_unrecognised_harness_label_changes_nothing() {
        let mut a = app();
        a.harness_from_label("something else entirely");
        assert_eq!(a.harness, HarnessKind::ClaudeCode);
    }
}
