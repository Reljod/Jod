//! The TUI's state, and every decision it makes.
//!
//! Deliberately free of rendering and of I/O: everything here is a pure
//! transformation of state, so the behaviour that is easy to get subtly wrong —
//! cursor movement, scrollback, which turn a message belongs to — is testable
//! without a terminal.

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
    /// A tool the agent called.
    Tool { name: String, failed: bool },
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
    pub model: Option<String>,
    pub session: Option<String>,
    pub resume: Resume,
    pub cost_usd: f64,
    pub show_thinking: bool,
    pub pane: Pane,
    /// True while an agent is working, so the UI can refuse a second prompt.
    pub busy: bool,
    pub agents: Vec<AgentLine>,
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

impl App {
    pub fn new(harness: HarnessKind, model: Option<String>, resume: Resume) -> App {
        App {
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            harness,
            model,
            session: None,
            resume,
            cost_usd: 0.0,
            show_thinking: false,
            pane: Pane::Chat,
            busy: false,
            agents: Vec::new(),
            should_quit: false,
            confirm_quit: false,
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
                    self.model = Some(m.clone());
                }
            }
            AgentEvent::Thinking { text } => {
                if self.show_thinking {
                    self.push(Entry::Thinking(text.clone()));
                }
            }
            AgentEvent::Message { text } => self.push(Entry::Agent(text.clone())),
            AgentEvent::ToolCall { name, .. } => self.push(Entry::Tool {
                name: name.clone(),
                failed: false,
            }),
            AgentEvent::ToolResult { name, is_error, .. } => {
                // A tool that worked is noise; one that failed is the reason
                // the answer is about to be wrong.
                if *is_error {
                    self.push(Entry::Tool {
                        name: name.clone(),
                        failed: true,
                    });
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
        if let Some(m) = &self.model {
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
                failed: true
            }]
        );
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
        assert_eq!(a.model.as_deref(), Some("some-model"));
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
