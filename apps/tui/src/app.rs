//! The TUI's state, and every transition it can make.
//!
//! Deliberately free of `ratatui` types and of I/O: the whole of this file is a
//! pure function from "what happened" to "what is on screen", so the behaviour
//! that matters can be tested without a terminal. `ui.rs` draws it; `main.rs`
//! feeds it. → the design rule in `docs/jod-tui.md`

use std::collections::HashMap;

use jod_core::event::AgentEnvelope;
use jod_core::{AgentEvent, AgentStatus, AgentSummary, HarnessKind, Member, Task};

/// Which half of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Fleet,
    Stream,
}

/// What the right-hand pane is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// One agent's live event stream.
    Stream,
    /// The selected team: members, the task board, the message bus.
    Team,
    /// Key bindings.
    Help,
}

/// What typing does right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Composing a follow-up turn for the selected agent.
    Compose,
    /// Composing a new agent's prompt.
    Spawn,
    /// Composing a message to send onto a team's bus.
    Message,
}

/// One rendered line of an agent's stream, classified so the UI can style it
/// without re-deriving meaning from the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Reasoning,
    Message,
    ToolCall,
    ToolOk,
    ToolError,
    /// Output the adapter could not classify. Shown, never dropped.
    Raw,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLine {
    pub kind: LineKind,
    pub text: String,
}

impl StreamLine {
    fn new(kind: LineKind, text: impl Into<String>) -> Self {
        Self { kind, text: text.into() }
    }
}

/// What a keypress asked the outside world to do. `main.rs` performs these;
/// the app itself never touches the service, so every transition stays pure.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    /// Send a follow-up turn to an agent, resuming its harness session.
    Follow { agent_id: String, prompt: String },
    /// Start a new agent.
    Spawn { harness: HarnessKind, prompt: String },
    Kill { agent_id: String },
    /// Put a message on the selected team's bus.
    Broadcast { team: String, text: String },
    /// Copy the tmux command for the selected agent into the status line.
    Attach { agent_id: String },
}

pub struct App {
    pub agents: Vec<AgentSummary>,
    pub events: HashMap<String, Vec<AgentEnvelope>>,
    pub selected: usize,
    pub focus: Focus,
    pub view: View,
    pub mode: Mode,
    pub input: String,
    /// Reasoning is the reason this client exists, so it starts visible.
    pub show_reasoning: bool,
    pub scroll: u16,
    /// Sticks to the newest output until the user scrolls up.
    pub follow_tail: bool,
    pub status: Option<String>,
    pub team_name: Option<String>,
    pub members: Vec<Member>,
    pub tasks: Vec<Task>,
    /// Which harness a new agent will use, cycled with `h`.
    pub spawn_harness: HarnessKind,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            agents: vec![],
            events: HashMap::new(),
            selected: 0,
            focus: Focus::Fleet,
            view: View::Stream,
            mode: Mode::Normal,
            input: String::new(),
            show_reasoning: true,
            scroll: 0,
            follow_tail: true,
            status: None,
            team_name: None,
            members: vec![],
            tasks: vec![],
            spawn_harness: HarnessKind::ClaudeCode,
            should_quit: false,
        }
    }
}

impl App {
    pub fn selected_agent(&self) -> Option<&AgentSummary> {
        self.agents.get(self.selected)
    }

    /// Fold a live event into state, exactly as the service's own reducer does
    /// for the summary — the TUI keeps the transcript, the service keeps state.
    pub fn ingest(&mut self, envelope: AgentEnvelope) {
        if let Some(agent) = self.agents.iter_mut().find(|a| a.id == envelope.agent_id) {
            match &envelope.event {
                AgentEvent::Started { session_id, model } => {
                    if agent.session_id.is_none() {
                        agent.session_id.clone_from(session_id);
                    }
                    if let Some(model) = model {
                        agent.model = Some(model.clone());
                    }
                }
                AgentEvent::Message { text } => agent.last_message = Some(text.clone()),
                AgentEvent::Finished { is_error, usage, text, .. } => {
                    if agent.status == AgentStatus::Running {
                        agent.status = if *is_error {
                            AgentStatus::Failed
                        } else {
                            AgentStatus::Completed
                        };
                    }
                    if !usage.is_empty() {
                        agent.usage = usage.clone();
                    }
                    if let Some(text) = text {
                        agent.last_message = Some(text.clone());
                    }
                }
                _ => {}
            }
            agent.event_count += 1;
        }
        self.events
            .entry(envelope.agent_id.clone())
            .or_default()
            .push(envelope);
    }

    /// The selected agent's stream, classified and filtered for display.
    pub fn stream_lines(&self) -> Vec<StreamLine> {
        let Some(agent) = self.selected_agent() else {
            return vec![StreamLine::new(
                LineKind::System,
                "No agents yet. Press n to start one.",
            )];
        };
        let Some(events) = self.events.get(&agent.id) else {
            return vec![StreamLine::new(LineKind::System, "Waiting for output…")];
        };

        let mut lines = vec![];
        // Every harness reports its final answer twice: once as the last
        // assistant message, and again in the terminal record. Rendering both
        // shows the answer twice, so the echo is dropped.
        let mut last_message: Option<&str> = None;
        for envelope in events {
            match &envelope.event {
                AgentEvent::Started { session_id, model } => {
                    let model = model.as_deref().unwrap_or("default model");
                    let session = session_id.as_deref().unwrap_or("no session id");
                    lines.push(StreamLine::new(
                        LineKind::System,
                        format!("started · {model} · {session}"),
                    ));
                }
                AgentEvent::Thinking { text } => {
                    if self.show_reasoning {
                        for line in text.lines() {
                            lines.push(StreamLine::new(LineKind::Reasoning, line));
                        }
                    }
                }
                AgentEvent::Message { text } => {
                    last_message = Some(text);
                    for line in text.lines() {
                        lines.push(StreamLine::new(LineKind::Message, line));
                    }
                }
                AgentEvent::ToolCall { name, input } => {
                    let args = input
                        .as_ref()
                        .map(|i| one_line(&i.to_string(), 100))
                        .unwrap_or_default();
                    lines.push(StreamLine::new(
                        LineKind::ToolCall,
                        format!("→ {name} {args}"),
                    ));
                }
                AgentEvent::ToolResult { name, summary, is_error } => {
                    let kind = if *is_error { LineKind::ToolError } else { LineKind::ToolOk };
                    let body = summary.as_deref().unwrap_or("(no output)");
                    lines.push(StreamLine::new(
                        kind,
                        format!("← {name} {}", one_line(body, 100)),
                    ));
                }
                AgentEvent::Raw { line } => {
                    lines.push(StreamLine::new(LineKind::Raw, one_line(line, 160)));
                }
                AgentEvent::Error { message } => {
                    lines.push(StreamLine::new(LineKind::Error, message.clone()));
                }
                AgentEvent::Finished { text, exit_code, is_error, usage } => {
                    if let Some(text) = text.as_deref().filter(|t| Some(*t) != last_message) {
                        for line in text.lines() {
                            lines.push(StreamLine::new(LineKind::Message, line));
                        }
                    }
                    let kind = if *is_error { LineKind::Error } else { LineKind::System };
                    lines.push(StreamLine::new(
                        kind,
                        format!(
                            "finished · exit {} · {}",
                            exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                            describe_usage(usage)
                        ),
                    ));
                }
            }
        }

        // A harness that reports reasoning tokens but no reasoning text must say
        // so, or an empty pane reads as a bug in Jod. → docs/jod-tui.md
        if self.show_reasoning && !lines.iter().any(|l| l.kind == LineKind::Reasoning) {
            if let Some(tokens) = agent.usage.thinking_tokens.filter(|t| *t > 0) {
                lines.push(StreamLine::new(
                    LineKind::System,
                    format!(
                        "{} reasoned for {tokens} tokens but does not expose the text.",
                        agent.harness_label
                    ),
                ));
            }
        }
        lines
    }

    // ---- transitions ---------------------------------------------------

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
            self.reset_scroll();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + self.agents.len() - 1) % self.agents.len();
            self.reset_scroll();
        }
    }

    fn reset_scroll(&mut self) {
        self.scroll = 0;
        self.follow_tail = true;
    }

    /// Scrolling up stops the pane chasing new output, so a run that is still
    /// producing lines cannot yank the view away from what is being read.
    pub fn scroll_up(&mut self, amount: u16) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn cycle_harness(&mut self) {
        let all = HarnessKind::ALL;
        let at = all.iter().position(|k| *k == self.spawn_harness).unwrap_or(0);
        self.spawn_harness = all[(at + 1) % all.len()];
    }

    /// Handle a key in whichever mode is active. Returns what the caller must
    /// do; the app never performs the action itself.
    pub fn on_key(&mut self, key: Key) -> Action {
        match self.mode {
            Mode::Normal => self.on_key_normal(key),
            _ => self.on_key_editing(key),
        }
    }

    fn on_key_normal(&mut self, key: Key) -> Action {
        self.status = None;
        match key {
            Key::Char('q') => {
                self.should_quit = true;
                Action::Quit
            }
            Key::Char('j') | Key::Down => {
                match self.focus {
                    Focus::Fleet => self.select_next(),
                    Focus::Stream => self.scroll_down(1),
                }
                Action::None
            }
            Key::Char('k') | Key::Up => {
                match self.focus {
                    Focus::Fleet => self.select_prev(),
                    Focus::Stream => self.scroll_up(1),
                }
                Action::None
            }
            Key::Tab => {
                self.focus = match self.focus {
                    Focus::Fleet => Focus::Stream,
                    Focus::Stream => Focus::Fleet,
                };
                Action::None
            }
            // The headline toggle: reasoning on/off.
            Key::Char('r') => {
                self.show_reasoning = !self.show_reasoning;
                self.status = Some(format!(
                    "reasoning {}",
                    if self.show_reasoning { "shown" } else { "hidden" }
                ));
                Action::None
            }
            Key::Char('t') => {
                self.view = if self.view == View::Team { View::Stream } else { View::Team };
                Action::None
            }
            Key::Char('?') => {
                self.view = if self.view == View::Help { View::Stream } else { View::Help };
                Action::None
            }
            Key::Char('i') | Key::Enter => {
                if self.selected_agent().is_some() {
                    self.mode = Mode::Compose;
                    self.input.clear();
                }
                Action::None
            }
            Key::Char('n') => {
                self.mode = Mode::Spawn;
                self.input.clear();
                Action::None
            }
            Key::Char('m') => {
                if self.team_name.is_some() {
                    self.mode = Mode::Message;
                    self.input.clear();
                }
                Action::None
            }
            Key::Char('h') => {
                self.cycle_harness();
                self.status = Some(format!("new agents use {}", self.spawn_harness.label()));
                Action::None
            }
            Key::Char('x') => match self.selected_agent() {
                Some(agent) => Action::Kill { agent_id: agent.id.clone() },
                None => Action::None,
            },
            Key::Char('a') => match self.selected_agent() {
                Some(agent) => Action::Attach { agent_id: agent.id.clone() },
                None => Action::None,
            },
            _ => Action::None,
        }
    }

    fn on_key_editing(&mut self, key: Key) -> Action {
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                self.input.clear();
                Action::None
            }
            Key::Backspace => {
                self.input.pop();
                Action::None
            }
            Key::Char(c) => {
                self.input.push(c);
                Action::None
            }
            Key::Enter => {
                let text = self.input.trim().to_string();
                let mode = self.mode.clone();
                self.mode = Mode::Normal;
                self.input.clear();
                if text.is_empty() {
                    return Action::None;
                }
                match mode {
                    Mode::Compose => match self.selected_agent() {
                        Some(agent) => Action::Follow {
                            agent_id: agent.id.clone(),
                            prompt: text,
                        },
                        None => Action::None,
                    },
                    Mode::Spawn => Action::Spawn {
                        harness: self.spawn_harness,
                        prompt: text,
                    },
                    Mode::Message => match &self.team_name {
                        Some(team) => Action::Broadcast { team: team.clone(), text },
                        None => Action::None,
                    },
                    Mode::Normal => Action::None,
                }
            }
            _ => Action::None,
        }
    }
}

/// The subset of key events the app understands, so tests do not need a
/// terminal backend to press a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
}

/// Collapse whitespace and truncate, so one tool payload cannot own the pane.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max).collect();
    format!("{head}…")
}

/// The one-line summary along the bottom of the screen.
///
/// Lives here rather than in `ui.rs` so the wording is covered by tests — a
/// status bar is the easiest place for a quiet formatting lie to survive.
pub fn status_line(app: &App) -> String {
    if let Some(status) = &app.status {
        return status.clone();
    }
    let spend: f64 = app.agents.iter().filter_map(|a| a.usage.cost_usd).sum();
    let running = app
        .agents
        .iter()
        .filter(|a| a.status == AgentStatus::Running)
        .count();
    // `-0.0` is a real f64 that formats as "-0.0000". Normalise it, or an idle
    // fleet reports a negative spend.
    let spend = if spend == 0.0 { 0.0 } else { spend };

    format!(
        "{running} running · {} total · ${spend:.4} · reasoning {} · new agents: {}",
        app.agents.len(),
        if app.show_reasoning { "on" } else { "off" },
        app.spawn_harness.label(),
    )
}

/// Render usage honestly: a harness that reports no cost shows tokens, never a
/// zero that would read as "this was free".
pub fn describe_usage(usage: &jod_core::Usage) -> String {
    let mut parts = vec![];
    if let Some(input) = usage.input_tokens {
        parts.push(format!("{input} in"));
    }
    if let Some(output) = usage.output_tokens {
        parts.push(format!("{output} out"));
    }
    if let Some(thinking) = usage.thinking_tokens.filter(|t| *t > 0) {
        parts.push(format!("{thinking} reasoning"));
    }
    match usage.cost_usd {
        Some(cost) => parts.push(format!("${cost:.4}")),
        None => parts.push("cost n/a".into()),
    }
    if parts.is_empty() {
        return "no usage reported".into();
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::{PermissionPolicy, Usage};

    fn agent(id: &str, harness: HarnessKind) -> AgentSummary {
        AgentSummary {
            id: id.into(),
            name: format!("agent-{id}"),
            harness,
            harness_label: harness.label().into(),
            status: AgentStatus::Running,
            cwd: "/tmp".into(),
            model: None,
            permission: PermissionPolicy::Ask,
            tmux_session: format!("jod-{id}"),
            attach_command: format!("tmux attach -t jod-{id}"),
            switch_command: format!("tmux switch-client -t jod-{id}"),
            session_closed: false,
            created_at_ms: 0,
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
            stream_path: "/tmp/s".into(),
        }
    }

    fn app_with_one() -> App {
        App {
            agents: vec![agent("a", HarnessKind::ClaudeCode)],
            ..Default::default()
        }
    }

    fn env(id: &str, event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope { agent_id: id.into(), at_ms: 0, seq: 0, event }
    }

    fn kinds(lines: &[StreamLine]) -> Vec<LineKind> {
        lines.iter().map(|l| l.kind.clone()).collect()
    }

    #[test]
    fn an_empty_fleet_says_so_rather_than_rendering_nothing() {
        let app = App::default();
        let lines = app.stream_lines();
        assert_eq!(kinds(&lines), vec![LineKind::System]);
        assert!(lines[0].text.contains("Press n"));
    }

    /// The whole point of the client: reasoning is visible while the run is
    /// still going, and it is distinguishable from the answer.
    #[test]
    fn reasoning_is_shown_by_default_and_marked_as_reasoning() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Thinking { text: "let me check".into() }));
        app.ingest(env("a", AgentEvent::Message { text: "the answer".into() }));

        let lines = app.stream_lines();
        assert_eq!(kinds(&lines), vec![LineKind::Reasoning, LineKind::Message]);
        assert_eq!(lines[0].text, "let me check");
    }

    #[test]
    fn toggling_reasoning_hides_only_the_reasoning() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Thinking { text: "hmm".into() }));
        app.ingest(env("a", AgentEvent::Message { text: "answer".into() }));

        app.on_key(Key::Char('r'));
        assert!(!app.show_reasoning);
        assert_eq!(kinds(&app.stream_lines()), vec![LineKind::Message]);

        app.on_key(Key::Char('r'));
        assert_eq!(
            kinds(&app.stream_lines()),
            vec![LineKind::Reasoning, LineKind::Message]
        );
    }

    /// Antigravity reports reasoning tokens and no reasoning text. An empty
    /// pane would read as a broken Jod, so the gap is stated in place.
    #[test]
    fn a_harness_that_hides_reasoning_says_so_instead_of_showing_nothing() {
        let mut app = App {
            agents: vec![agent("a", HarnessKind::Antigravity)],
            ..Default::default()
        };
        app.ingest(env(
            "a",
            AgentEvent::Finished {
                text: Some("391".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage { thinking_tokens: Some(216), ..Default::default() },
            },
        ));

        let lines = app.stream_lines();
        let note = lines.iter().find(|l| l.text.contains("does not expose")).unwrap();
        assert!(note.text.contains("Antigravity"));
        assert!(note.text.contains("216"));
    }

    #[test]
    fn a_harness_that_does_expose_reasoning_gets_no_apology() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Thinking { text: "real reasoning".into() }));
        assert!(!app
            .stream_lines()
            .iter()
            .any(|l| l.text.contains("does not expose")));
    }

    #[test]
    fn multi_line_reasoning_becomes_one_line_each() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Thinking { text: "one\ntwo\nthree".into() }));
        assert_eq!(app.stream_lines().len(), 3);
    }

    #[test]
    fn tool_calls_and_results_are_classified_separately() {
        let mut app = app_with_one();
        app.ingest(env(
            "a",
            AgentEvent::ToolCall {
                name: "Bash".into(),
                input: Some(serde_json::json!({"command": "ls"})),
            },
        ));
        app.ingest(env(
            "a",
            AgentEvent::ToolResult {
                name: "Bash".into(),
                summary: Some("a b".into()),
                is_error: false,
            },
        ));
        app.ingest(env(
            "a",
            AgentEvent::ToolResult {
                name: "Bash".into(),
                summary: Some("boom".into()),
                is_error: true,
            },
        ));
        assert_eq!(
            kinds(&app.stream_lines()),
            vec![LineKind::ToolCall, LineKind::ToolOk, LineKind::ToolError]
        );
    }

    #[test]
    fn raw_output_is_rendered_rather_than_dropped() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Raw { line: "who knows".into() }));
        let lines = app.stream_lines();
        assert_eq!(kinds(&lines), vec![LineKind::Raw]);
        assert!(lines[0].text.contains("who knows"));
    }

    #[test]
    fn a_huge_tool_payload_cannot_take_over_the_pane() {
        let mut app = app_with_one();
        app.ingest(env(
            "a",
            AgentEvent::ToolResult {
                name: "Read".into(),
                summary: Some("x".repeat(5000)),
                is_error: false,
            },
        ));
        assert!(app.stream_lines()[0].text.chars().count() < 140);
    }

    #[test]
    fn ingesting_a_finish_updates_the_agent_in_the_fleet_list() {
        let mut app = app_with_one();
        app.ingest(env(
            "a",
            AgentEvent::Finished {
                text: Some("done".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage { cost_usd: Some(0.02), ..Default::default() },
            },
        ));
        assert_eq!(app.agents[0].status, AgentStatus::Completed);
        assert_eq!(app.agents[0].usage.cost_usd, Some(0.02));
        assert_eq!(app.agents[0].last_message.as_deref(), Some("done"));
    }

    /// Regression, caught in a live run: the final answer arrived once as a
    /// Message and again in Finished, so "42" was rendered twice on screen.
    #[test]
    fn the_final_answer_is_not_echoed_twice() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Message { text: "42".into() }));
        app.ingest(env(
            "a",
            AgentEvent::Finished {
                text: Some("42".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage::default(),
            },
        ));
        let answers = app
            .stream_lines()
            .iter()
            .filter(|l| l.text == "42")
            .count();
        assert_eq!(answers, 1, "the answer must appear once");
    }

    /// …but a genuinely different closing text is still shown.
    #[test]
    fn a_final_answer_that_differs_is_still_rendered() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Message { text: "working".into() }));
        app.ingest(env(
            "a",
            AgentEvent::Finished {
                text: Some("the real answer".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage::default(),
            },
        ));
        assert!(app
            .stream_lines()
            .iter()
            .any(|l| l.text == "the real answer"));
    }

    #[test]
    fn a_failing_run_is_marked_failed() {
        let mut app = app_with_one();
        app.ingest(env(
            "a",
            AgentEvent::Finished {
                text: None,
                exit_code: Some(1),
                is_error: true,
                usage: Usage::default(),
            },
        ));
        assert_eq!(app.agents[0].status, AgentStatus::Failed);
    }

    #[test]
    fn events_for_an_unknown_agent_are_kept_not_dropped() {
        let mut app = app_with_one();
        app.ingest(env("ghost", AgentEvent::Message { text: "hi".into() }));
        assert_eq!(app.events.get("ghost").map(Vec::len), Some(1));
    }

    #[test]
    fn started_records_the_session_and_model_and_announces_them() {
        let mut app = app_with_one();
        app.ingest(env(
            "a",
            AgentEvent::Started {
                session_id: Some("ses-1".into()),
                model: Some("claude-opus-5".into()),
            },
        ));
        assert_eq!(app.agents[0].session_id.as_deref(), Some("ses-1"));
        assert_eq!(app.agents[0].model.as_deref(), Some("claude-opus-5"));

        let line = &app.stream_lines()[0];
        assert_eq!(line.kind, LineKind::System);
        assert!(line.text.contains("claude-opus-5"));
        assert!(line.text.contains("ses-1"));
    }

    /// A second Started must not overwrite the session already reported, or a
    /// resumed run would lose the id the follow-up needs.
    #[test]
    fn a_later_started_does_not_replace_the_first_session() {
        let mut app = app_with_one();
        for id in ["first", "second"] {
            app.ingest(env(
                "a",
                AgentEvent::Started { session_id: Some(id.into()), model: None },
            ));
        }
        assert_eq!(app.agents[0].session_id.as_deref(), Some("first"));
    }

    #[test]
    fn a_started_without_details_still_renders() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Started { session_id: None, model: None }));
        let text = &app.stream_lines()[0].text;
        assert!(text.contains("default model"));
        assert!(text.contains("no session id"));
    }

    #[test]
    fn an_error_event_is_shown_as_an_error() {
        let mut app = app_with_one();
        app.ingest(env("a", AgentEvent::Error { message: "spawn failed".into() }));
        let lines = app.stream_lines();
        assert_eq!(lines[0].kind, LineKind::Error);
        assert_eq!(lines[0].text, "spawn failed");
    }

    #[test]
    fn an_agent_with_no_events_yet_says_it_is_waiting() {
        let app = app_with_one();
        let lines = app.stream_lines();
        assert_eq!(lines[0].kind, LineKind::System);
        assert!(lines[0].text.contains("Waiting"));
    }

    #[test]
    fn scrolling_down_moves_the_offset() {
        let mut app = app_with_one();
        app.focus = Focus::Stream;
        app.on_key(Key::Down);
        app.on_key(Key::Down);
        assert_eq!(app.scroll, 2);
        app.on_key(Key::Up);
        assert_eq!(app.scroll, 1);
    }

    #[test]
    fn scrolling_up_from_the_top_stops_rather_than_wrapping() {
        let mut app = app_with_one();
        app.focus = Focus::Stream;
        app.scroll_up(10);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn enter_also_opens_the_follow_up_box() {
        let mut app = app_with_one();
        app.on_key(Key::Enter);
        assert_eq!(app.mode, Mode::Compose);
    }

    #[test]
    fn there_is_nothing_to_follow_up_with_an_empty_fleet() {
        let mut app = App::default();
        app.on_key(Key::Enter);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn help_toggles_back_to_the_stream() {
        let mut app = app_with_one();
        app.on_key(Key::Char('?'));
        assert_eq!(app.view, View::Help);
        app.on_key(Key::Char('?'));
        assert_eq!(app.view, View::Stream);
    }

    #[test]
    fn an_unbound_key_does_nothing() {
        let mut app = app_with_one();
        assert_eq!(app.on_key(Key::Char('z')), Action::None);
        assert_eq!(app.on_key(Key::Backspace), Action::None);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut app = App {
            agents: vec![
                agent("a", HarnessKind::ClaudeCode),
                agent("b", HarnessKind::OpenCode),
            ],
            ..Default::default()
        };
        app.select_next();
        assert_eq!(app.selected, 1);
        app.select_next();
        assert_eq!(app.selected, 0);
        app.select_prev();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn selecting_with_no_agents_cannot_panic() {
        let mut app = App::default();
        app.select_next();
        app.select_prev();
        assert_eq!(app.selected, 0);
    }

    /// A live run appends constantly. Scrolling up must pin the view, or the
    /// thing being read is yanked away.
    #[test]
    fn scrolling_up_stops_the_pane_chasing_new_output() {
        let mut app = app_with_one();
        assert!(app.follow_tail);
        app.focus = Focus::Stream;
        app.on_key(Key::Up);
        assert!(!app.follow_tail);

        // Changing agent starts following again.
        app.focus = Focus::Fleet;
        app.on_key(Key::Down);
        assert!(app.follow_tail);
    }

    #[test]
    fn tab_moves_focus_between_the_two_panes() {
        let mut app = app_with_one();
        assert_eq!(app.focus, Focus::Fleet);
        app.on_key(Key::Tab);
        assert_eq!(app.focus, Focus::Stream);
        app.on_key(Key::Tab);
        assert_eq!(app.focus, Focus::Fleet);
    }

    /// The multi-turn conversation: a follow-up names the agent, and `main`
    /// resumes its harness session.
    #[test]
    fn composing_a_follow_up_yields_a_follow_action() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        assert_eq!(app.mode, Mode::Compose);
        for c in "and now?".chars() {
            app.on_key(Key::Char(c));
        }
        let action = app.on_key(Key::Enter);
        assert_eq!(
            action,
            Action::Follow { agent_id: "a".into(), prompt: "and now?".into() }
        );
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.input.is_empty());
    }

    #[test]
    fn an_empty_follow_up_does_nothing() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        app.on_key(Key::Char(' '));
        assert_eq!(app.on_key(Key::Enter), Action::None);
    }

    #[test]
    fn escape_abandons_what_was_typed() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        app.on_key(Key::Char('x'));
        app.on_key(Key::Esc);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.input.is_empty());
    }

    #[test]
    fn backspace_edits_the_buffer() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        for c in "abc".chars() {
            app.on_key(Key::Char(c));
        }
        app.on_key(Key::Backspace);
        assert_eq!(app.input, "ab");
    }

    /// In compose mode, `q` is a letter, not quit — the classic TUI bug.
    #[test]
    fn typing_q_while_composing_does_not_quit() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        app.on_key(Key::Char('q'));
        assert!(!app.should_quit);
        assert_eq!(app.input, "q");
    }

    #[test]
    fn spawning_uses_the_currently_selected_harness() {
        let mut app = App::default();
        app.on_key(Key::Char('h'));
        assert_eq!(app.spawn_harness, HarnessKind::OpenCode);
        app.on_key(Key::Char('h'));
        assert_eq!(app.spawn_harness, HarnessKind::Antigravity);
        app.on_key(Key::Char('h'));
        assert_eq!(app.spawn_harness, HarnessKind::ClaudeCode, "cycles back round");

        app.on_key(Key::Char('n'));
        for c in "go".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(
            app.on_key(Key::Enter),
            Action::Spawn { harness: HarnessKind::ClaudeCode, prompt: "go".into() }
        );
    }

    #[test]
    fn every_harness_can_be_reached_by_cycling() {
        let mut app = App::default();
        let mut seen = vec![app.spawn_harness];
        for _ in 0..HarnessKind::ALL.len() {
            app.cycle_harness();
            seen.push(app.spawn_harness);
        }
        for kind in HarnessKind::ALL {
            assert!(seen.contains(&kind), "{kind:?} must be spawnable from the TUI");
        }
    }

    #[test]
    fn kill_and_attach_name_the_selected_agent() {
        let mut app = app_with_one();
        assert_eq!(
            app.on_key(Key::Char('x')),
            Action::Kill { agent_id: "a".into() }
        );
        assert_eq!(
            app.on_key(Key::Char('a')),
            Action::Attach { agent_id: "a".into() }
        );
    }

    #[test]
    fn kill_with_no_agents_is_a_no_op() {
        let mut app = App::default();
        assert_eq!(app.on_key(Key::Char('x')), Action::None);
    }

    #[test]
    fn a_team_message_needs_a_team() {
        let mut app = app_with_one();
        app.on_key(Key::Char('m'));
        assert_eq!(app.mode, Mode::Normal, "no team selected, so nothing to send to");

        app.team_name = Some("crew".into());
        app.on_key(Key::Char('m'));
        assert_eq!(app.mode, Mode::Message);
        for c in "status?".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(
            app.on_key(Key::Enter),
            Action::Broadcast { team: "crew".into(), text: "status?".into() }
        );
    }

    #[test]
    fn t_and_question_mark_switch_views_and_back() {
        let mut app = app_with_one();
        app.on_key(Key::Char('t'));
        assert_eq!(app.view, View::Team);
        app.on_key(Key::Char('t'));
        assert_eq!(app.view, View::Stream);
        app.on_key(Key::Char('?'));
        assert_eq!(app.view, View::Help);
    }

    #[test]
    fn q_quits() {
        let mut app = app_with_one();
        assert_eq!(app.on_key(Key::Char('q')), Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn usage_without_a_cost_says_so_rather_than_showing_zero() {
        let rendered = describe_usage(&Usage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            thinking_tokens: Some(5),
            ..Default::default()
        });
        assert!(rendered.contains("100 in"));
        assert!(rendered.contains("5 reasoning"));
        assert!(rendered.contains("cost n/a"), "got {rendered}");
        assert!(!rendered.contains("$0.0000"));
    }

    /// Regression: an idle fleet rendered "$-0.0000" in the live TUI, because
    /// summing an empty set of costs can land on negative zero.
    #[test]
    fn an_idle_fleet_never_reports_a_negative_spend() {
        let line = status_line(&App::default());
        assert!(!line.contains("-0.0000"), "got {line}");
        assert!(line.contains("$0.0000"), "got {line}");
    }

    #[test]
    fn the_status_line_reports_the_fleet_and_the_toggles() {
        let mut app = app_with_one();
        app.agents[0].usage.cost_usd = Some(0.5);
        let line = status_line(&app);
        assert!(line.contains("1 running"));
        assert!(line.contains("1 total"));
        assert!(line.contains("$0.5000"));
        assert!(line.contains("reasoning on"));
        assert!(line.contains("Claude Code"));

        app.show_reasoning = false;
        assert!(status_line(&app).contains("reasoning off"));
    }

    #[test]
    fn a_transient_status_replaces_the_summary() {
        let mut app = app_with_one();
        app.status = Some("killed".into());
        assert_eq!(status_line(&app), "killed");
    }

    #[test]
    fn usage_with_a_cost_shows_it() {
        let rendered = describe_usage(&Usage { cost_usd: Some(0.1234), ..Default::default() });
        assert!(rendered.contains("$0.1234"));
    }
}
