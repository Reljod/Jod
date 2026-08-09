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
    pub show_thinking: bool,
    /// Whether tool output is shown. On by default: the reason to watch a
    /// harness work is to see what it is doing.
    pub show_details: bool,
    pub pane: Pane,
    /// True while an agent is working, so the UI can refuse a second prompt.
    pub busy: bool,
    pub agents: Vec<AgentLine>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLine {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub status: String,
}

/// The most useful single field of a tool's arguments.
///
/// Harnesses name things differently, so the common keys are tried in order of
/// how much they tell a reader, and anything unrecognised falls back to compact
/// JSON rather than being dropped.
fn tool_detail(input: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 10] = [
        "command",
        "cmd",
        "file_path",
        "path",
        "filePath",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
    ];
    for key in KEYS {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                return Some(one_line(v, 90));
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
            show_thinking: false,
            show_details: true,
            pane: Pane::Chat,
            busy: false,
            agents: Vec::new(),
            suggestion: 0,
            team: None,
            members: Vec::new(),
            tasks: Vec::new(),
            should_quit: false,
            confirm_quit: false,
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
    pub fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        Some(text)
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
                if *is_error {
                    self.push(Entry::Tool {
                        name: name.clone(),
                        detail: None,
                        failed: true,
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
                self.push(Entry::Done {
                    text: bits.join(" · "),
                    failed: *is_error,
                });
                self.busy = false;
            }
            AgentEvent::Error { message } => self.push(Entry::Notice(message.clone())),
            AgentEvent::Raw { line } => {
                if !line.trim().is_empty() {
                    self.push(Entry::Raw(line.clone()));
                }
            }
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
            "working".into()
        } else {
            "ready".into()
        });
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
    fn thinking_is_hidden_until_it_is_asked_for() {
        let mut a = app();
        a.apply(&AgentEvent::Thinking { text: "hmm".into() });
        assert!(a.transcript.is_empty());
        a.show_thinking = true;
        a.apply(&AgentEvent::Thinking { text: "hmm".into() });
        assert_eq!(a.transcript, vec![Entry::Thinking("hmm".into())]);
    }

    /// A tool that worked is noise. A tool that failed explains the answer.
    #[test]
    fn only_failing_tool_results_reach_the_transcript() {
        let mut a = app();
        a.apply(&AgentEvent::ToolResult {
            name: "read".into(),
            summary: None,
            is_error: false,
        });
        assert!(a.transcript.is_empty());
        a.apply(&AgentEvent::ToolResult {
            name: "write".into(),
            summary: None,
            is_error: true,
        });
        assert_eq!(
            a.transcript,
            vec![Entry::Tool {
                name: "write".into(),
                detail: None,
                failed: true
            }]
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
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("test result: ok. 93 passed".into()),
            is_error: false,
        });
        assert_eq!(
            a.transcript,
            vec![Entry::ToolOut {
                text: "test result: ok. 93 passed".into(),
                failed: false
            }]
        );
    }

    #[test]
    fn tool_output_can_be_turned_off() {
        let mut a = app();
        a.show_details = false;
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("chatter".into()),
            is_error: false,
        });
        assert!(a.transcript.is_empty());
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
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some(long),
            is_error: false,
        });
        match &a.transcript[0] {
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
        a.apply(&AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("   ".into()),
            is_error: false,
        });
        assert!(a.transcript.is_empty());
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
}
