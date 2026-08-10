//! The TUI's state, and every decision it makes.
//!
//! Deliberately free of rendering and of I/O: everything here is a pure
//! transformation of state, so the behaviour that is easy to get subtly wrong —
//! cursor movement, scrollback, which turn a message belongs to — is testable
//! without a terminal.

use jod_core::team::{Member, TeamTask};
use jod_core::{AgentEvent, HarnessKind, Resume};

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
        failed: bool,
    },
    /// What a tool gave back. Shown when details are on, which is the point of
    /// watching a harness work rather than waiting for its conclusion.
    ToolOut { text: String, failed: bool },
    /// A run finished: the summary line.
    Done { text: String, failed: bool },
    /// Something Jod itself wants to say.
    Notice(String),
    /// A line the harness printed that we could not classify.
    Raw(String),
}

/// Which pane has focus. The agents list is a side view, not a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Chat,
    Agents,
    /// The team: who is on it, and what they are each doing.
    Team,
}

/// The spinner shown while a turn is in flight. Ten frames at four a second is
/// slow enough not to strobe and fast enough to prove the UI is alive — the
/// thing a static "working…" cannot do.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
    pub pane: Pane,
    /// True while the conversation on screen is mid-turn. Other agents may be
    /// working at the same time; this is only about the one being watched.
    pub busy: bool,
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
    /// Which row of the agents panel is selected.
    pub agent_sel: usize,
    /// Which row of the team board is selected.
    pub task_sel: usize,
    /// Wall clock, refreshed on every tick so everything that renders an age
    /// stays a pure function of state.
    pub now_ms: i64,
    /// When the turn on screen started, for the elapsed counter.
    pub turn_started_ms: Option<i64>,
    /// Ticks since start, which is all the spinner needs.
    pub tick: u64,
    /// Which entry of the slash-command popup is highlighted. Meaningless when
    /// there is no popup, and clamped every time the input changes.
    pub suggestion: usize,
    /// The team this session is watching, if any. `None` means teams are not
    /// in play and the panel says so rather than showing an empty box.
    pub team: Option<String>,
    pub members: Vec<Member>,
    pub tasks: Vec<TeamTask>,
    pub should_quit: bool,
    /// Set when the user asks to leave while an agent is still running.
    pub confirm_quit: bool,
    /// The agent whose output fills the transcript. Every other agent keeps
    /// running and reports in through the panel and a notice when it ends —
    /// which is what makes this an orchestrator rather than a chat window.
    pub watching: Option<String>,
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
    pub cost_usd: Option<f64>,
    /// The last thing it said, which is the only summary an unattended run
    /// offers of what it actually did.
    pub last: Option<String>,
}

impl AgentLine {
    pub fn is_running(&self) -> bool {
        self.status == "running"
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
fn tool_detail(input: &serde_json::Value) -> Option<String> {
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
                        return Some(one_line(v, 90));
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
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

/// Move a list cursor by `delta`, clamped to `len`.
fn step(at: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let last = len - 1;
    let moved = at as isize + delta;
    moved.clamp(0, last as isize) as usize
}

/// Keep the first `n` lines of tool output, saying how much was left.
fn first_lines(s: &str, n: usize) -> String {
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
            session: None,
            resume,
            cost_usd: 0.0,
            show_thinking: true,
            show_details: true,
            pane: Pane::Chat,
            busy: false,
            agents: Vec::new(),
            queued: Vec::new(),
            history: Vec::new(),
            history_at: None,
            draft: String::new(),
            agent_sel: 0,
            task_sel: 0,
            now_ms: 0,
            turn_started_ms: None,
            tick: 0,
            suggestion: 0,
            team: None,
            members: Vec::new(),
            tasks: Vec::new(),
            should_quit: false,
            confirm_quit: false,
            watching: None,
        }
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

    pub fn push(&mut self, entry: Entry) {
        self.transcript.push(entry);
        // New output pulls the view back to the bottom only if it was already
        // there. Scrolling up to read something must not be undone by an agent
        // that keeps talking.
        if self.scroll == 0 {
            return;
        }
        self.scroll += 1;
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

    // ---- panel selection ------------------------------------------------

    /// Move the agents-panel cursor, stopping at both ends rather than wrapping:
    /// in a list that changes under you, wrapping means overshooting lands
    /// somewhere unrelated.
    pub fn select_agent(&mut self, delta: isize) {
        self.agent_sel = step(self.agent_sel, delta, self.agents.len());
    }

    pub fn selected_agent(&self) -> Option<&AgentLine> {
        self.agents.get(self.agent_sel.min(self.agents.len().saturating_sub(1)))
    }

    pub fn select_task(&mut self, delta: isize) {
        self.task_sel = step(self.task_sel, delta, self.tasks.len());
    }

    pub fn selected_task(&self) -> Option<&TeamTask> {
        self.tasks.get(self.task_sel.min(self.tasks.len().saturating_sub(1)))
    }

    /// Keep both panel cursors inside their lists as those lists change.
    pub fn clamp_selection(&mut self) {
        self.agent_sel = self.agent_sel.min(self.agents.len().saturating_sub(1));
        self.task_sel = self.task_sel.min(self.tasks.len().saturating_sub(1));
    }

    // ---- liveness -------------------------------------------------------

    /// One animation frame. `now_ms` is passed in rather than read here so the
    /// whole of the UI stays testable without a clock.
    pub fn advance(&mut self, now_ms: i64) {
        self.tick = self.tick.wrapping_add(1);
        self.now_ms = now_ms;
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
            AgentEvent::Thinking { text } => {
                if self.show_thinking {
                    self.push(Entry::Thinking(text.clone()));
                }
            }
            AgentEvent::Message { text } => self.push(Entry::Agent(text.clone())),
            AgentEvent::ToolCall { name, input } => self.push(Entry::Tool {
                name: name.clone(),
                detail: input.as_ref().and_then(tool_detail),
                failed: false,
            }),
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
                let announced = matches!(
                    self.transcript.last(),
                    Some(Entry::Tool { name: n, .. }) if n == name
                );
                if *is_error || !announced {
                    self.push(Entry::Tool {
                        name: name.clone(),
                        detail: None,
                        failed: *is_error,
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
                self.push(Entry::Done {
                    text: bits.join(" · "),
                    failed: *is_error,
                });
                self.busy = false;
                self.turn_started_ms = None;
            }
            AgentEvent::Error { message } => self.push(Entry::Notice(message.clone())),
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
    pub fn status(&self) -> String {
        let mut parts = vec![self.harness.label().to_string()];
        // What the harness actually ran beats what was asked for; before the
        // first turn there is nothing to report, so the request stands in.
        if let Some(m) = self.reported_model.as_ref().or(self.model.as_ref()) {
            parts.push(m.clone());
        }
        if self.cost_usd > 0.0 {
            parts.push(format!("${:.4}", self.cost_usd));
        }
        parts.push(if self.busy {
            // The spinner and the elapsed time are the difference between "this
            // is working" and "this has hung", which a static word cannot tell
            // you during a run that legitimately takes ten minutes.
            match self.elapsed() {
                Some(t) => format!("{} working {t}", self.spinner()),
                None => format!("{} working", self.spinner()),
            }
        } else {
            "ready".into()
        });
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
        if !self.queued.is_empty() {
            parts.push(format!("{} queued", self.queued.len()));
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::Usage;

    fn app() -> App {
        App::new(HarnessKind::ClaudeCode, None, Resume::Fresh)
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
                failed: false
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
            Entry::Tool { failed: true, .. }
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
                    failed: false
                },
                Entry::ToolOut {
                    text: "Wrote file successfully.".into(),
                    failed: false
                },
            ]
        );
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
                failed: false
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
        assert!(matches!(a.transcript[1], Entry::ToolOut { failed: true, .. }));
    }

    #[test]
    fn long_tool_output_is_cut_down_with_a_count() {
        let mut a = app();
        let long = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
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
            id: id.into(),
            name: format!("job {id}"),
            harness: "Claude Code".into(),
            status: status.into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }
    }

    #[test]
    fn the_selection_stops_at_both_ends_rather_than_wrapping() {
        let mut a = app();
        a.agents = vec![line("a", "running"), line("b", "completed")];
        a.select_agent(-1);
        assert_eq!(a.agent_sel, 0, "already at the top");
        a.select_agent(1);
        a.select_agent(1);
        assert_eq!(a.agent_sel, 1, "cannot fall off the bottom");
    }

    #[test]
    fn selecting_in_an_empty_panel_is_harmless() {
        let mut a = app();
        a.select_agent(1);
        assert_eq!(a.agent_sel, 0);
        assert!(a.selected_agent().is_none());
    }

    /// Agents disappear from the list as older runs age out. A cursor left past
    /// the end would then act on nothing, or on the wrong row.
    #[test]
    fn the_cursor_is_pulled_back_when_the_list_shrinks() {
        let mut a = app();
        a.agents = vec![line("a", "running"), line("b", "running"), line("c", "running")];
        a.agent_sel = 2;
        a.agents.truncate(1);
        a.clamp_selection();
        assert_eq!(a.agent_sel, 0);
        assert_eq!(a.selected_agent().unwrap().id, "a");
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
