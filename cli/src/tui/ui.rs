//! Drawing the TUI. Returns the transcript viewport height so the caller can
//! make PageUp move by exactly one screen.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, Entry, Pane};

const USER: Color = Color::Cyan;
const AGENT: Color = Color::Reset;
const MUTED: Color = Color::DarkGray;
const BAD: Color = Color::Red;
const GOOD: Color = Color::Green;
const WARN: Color = Color::Yellow;

pub fn draw(f: &mut Frame, app: &App) -> usize {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // transcript
            Constraint::Length(3), // input
            Constraint::Length(1), // status
        ])
        .split(f.area());

    let height = draw_transcript(f, app, chunks[0]);
    draw_input(f, app, chunks[1]);
    draw_status(f, app, chunks[2]);

    if app.pane == Pane::Agents {
        draw_agents(f, app, f.area());
    }
    height
}

fn draw_transcript(f: &mut Frame, app: &App, area: Rect) -> usize {
    let width = area.width.saturating_sub(2).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        lines.extend(render(entry, width));
    }

    let viewport = area.height.saturating_sub(2).max(1) as usize;
    // `scroll` counts up from the bottom, but Paragraph scrolls down from the
    // top, so convert — and clamp, because the transcript can be shorter than
    // the window.
    let total = lines.len();
    let max_offset = total.saturating_sub(viewport);
    let offset = max_offset.saturating_sub(app.scroll.min(max_offset));

    let title = if app.following() {
        " jod ".to_string()
    } else {
        format!(" jod — scrolled up {} · Esc to follow ", app.scroll)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED))
        .title(title);

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((offset as u16, 0)),
        area,
    );
    viewport
}

/// One transcript entry as styled lines, already wrapped to `width`.
fn render(entry: &Entry, width: u16) -> Vec<Line<'static>> {
    let (prefix, style, body) = match entry {
        Entry::You(t) => (
            "› ",
            Style::default().fg(USER).add_modifier(Modifier::BOLD),
            t.clone(),
        ),
        Entry::Agent(t) => ("", Style::default().fg(AGENT), t.clone()),
        Entry::Thinking(t) => (
            "  ",
            Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
            t.clone(),
        ),
        Entry::Tool { name, failed } => {
            let mark = if *failed { "✗ " } else { "⚙ " };
            let style = Style::default().fg(if *failed { BAD } else { MUTED });
            (mark, style, name.clone())
        }
        Entry::Done { text, failed } => {
            let mark = if *failed { "✗ failed" } else { "✓ done" };
            let style = Style::default().fg(if *failed { BAD } else { GOOD });
            let body = if text.is_empty() {
                mark.to_string()
            } else {
                format!("{mark} · {text}")
            };
            ("", style, body)
        }
        Entry::Notice(t) => ("• ", Style::default().fg(WARN), t.clone()),
        Entry::Raw(t) => ("", Style::default().fg(MUTED), t.clone()),
    };

    wrap(&body, width as usize, prefix.chars().count())
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let lead = if i == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix.chars().count())
            };
            Line::from(vec![Span::styled(lead, style), Span::styled(text, style)])
        })
        .collect()
}

/// Break text to fit, on word boundaries where possible.
///
/// Done here rather than by `Paragraph`'s own wrapping because scroll offsets
/// are counted in *rendered* lines: if the widget wrapped after we computed the
/// offset, scrolling would drift further out with every long message.
pub fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.saturating_sub(indent).max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in raw.split(' ') {
            // A word longer than the line gets hard-split rather than
            // overflowing the pane.
            if word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let mut chunk = String::new();
                for c in word.chars() {
                    if chunk.chars().count() == width {
                        out.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(c);
                }
                line = chunk;
                continue;
            }
            let needed = if line.is_empty() {
                word.chars().count()
            } else {
                line.chars().count() + 1 + word.chars().count()
            };
            if needed > width {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
        }
        out.push(line);
    }
    out
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border = if app.busy { MUTED } else { USER };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(if app.busy { " working… " } else { " you " });

    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    // Keep the cursor on screen by scrolling the field horizontally once the
    // line outgrows the box.
    let col = app.cursor_column();
    let shift = col.saturating_sub(inner_width.saturating_sub(1));
    let visible: String = app.input.chars().skip(shift).take(inner_width).collect();

    f.render_widget(Paragraph::new(visible).block(block), area);
    f.set_cursor_position((area.x + 1 + (col - shift) as u16, area.y + 1));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let left = Span::styled(app.status(), Style::default().fg(MUTED));
    let right = Span::styled(
        "Ctrl-A agents · Ctrl-T thinking · Ctrl-C quit",
        Style::default().fg(MUTED),
    );
    let gap = (area.width as usize)
        .saturating_sub(left.content.chars().count() + right.content.chars().count() + 2);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            left,
            Span::raw(" ".repeat(gap)),
            right,
            Span::raw(" "),
        ])),
        area,
    );
}

fn draw_agents(f: &mut Frame, app: &App, area: Rect) {
    // Never larger than the terminal. Clamping to a comfortable 20..70 alone
    // can produce a panel wider than a narrow window, which would place the
    // rect outside the buffer.
    let w = area.width.saturating_sub(8).clamp(20, 70).min(area.width);
    let h = ((app.agents.len() + 2) as u16)
        .clamp(5, area.height.saturating_sub(4).max(5))
        .min(area.height);
    let panel = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    let items: Vec<ListItem> = if app.agents.is_empty() {
        vec![ListItem::new(Span::styled(
            "no agents yet",
            Style::default().fg(MUTED),
        ))]
    } else {
        app.agents
            .iter()
            .map(|a| {
                let colour = match a.status.as_str() {
                    "running" => WARN,
                    "completed" => GOOD,
                    "failed" => BAD,
                    _ => MUTED,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<9}", short(&a.id)), Style::default().fg(MUTED)),
                    Span::styled(format!("{:<10}", a.status), Style::default().fg(colour)),
                    Span::styled(format!("{:<13}", a.harness), Style::default().fg(MUTED)),
                    Span::raw(a.name.clone()),
                ]))
            })
            .collect()
    };

    // Clear first: this floats over the transcript, and without it the text
    // underneath shows through the gaps.
    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(USER))
                .title(" agents · Ctrl-A to close "),
        ),
        panel,
    );
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Unused today, kept because `Wrap` is part of the widget import surface and
/// removing it silently changes how long lines behave if re-enabled.
#[allow(dead_code)]
fn _wrap_marker(_: Wrap) {}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::{HarnessKind, Resume};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> App {
        App::new(HarnessKind::ClaudeCode, None, Resume::Fresh)
    }

    fn rendered(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                draw(f, app);
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

    #[test]
    fn wrapping_breaks_on_spaces_and_fits_the_width() {
        let lines = wrap("the quick brown fox jumps", 11, 0);
        assert!(lines.iter().all(|l| l.chars().count() <= 11), "{lines:?}");
        assert_eq!(lines[0], "the quick");
    }

    /// A pasted URL or a base64 blob has no spaces and must not overflow.
    #[test]
    fn a_word_longer_than_the_line_is_split_rather_than_overflowing() {
        let lines = wrap(&"x".repeat(25), 10, 0);
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        assert_eq!(lines.concat(), "x".repeat(25));
    }

    #[test]
    fn wrapping_accounts_for_the_prefix_indent() {
        let lines = wrap("aaa bbb ccc", 10, 4);
        assert!(lines.iter().all(|l| l.chars().count() <= 6), "{lines:?}");
    }

    #[test]
    fn explicit_newlines_are_preserved() {
        assert_eq!(wrap("a\nb", 40, 0), vec!["a", "b"]);
    }

    #[test]
    fn a_zero_width_pane_does_not_panic_or_loop_forever() {
        let lines = wrap("hello world", 0, 0);
        assert!(!lines.is_empty());
    }

    #[test]
    fn the_frame_draws_without_panicking_and_shows_the_input_box() {
        let out = rendered(&app(), 60, 12);
        assert!(out.contains("you"), "input box must be labelled:\n{out}");
        assert!(out.contains("jod"), "transcript must be titled:\n{out}");
    }

    #[test]
    fn what_the_user_typed_appears_in_the_transcript() {
        let mut a = app();
        a.push(Entry::You("summarise my inbox".into()));
        let out = rendered(&a, 60, 12);
        assert!(out.contains("summarise my inbox"), "{out}");
    }

    #[test]
    fn the_agent_reply_appears() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        assert!(rendered(&a, 60, 12).contains("here is the summary"));
    }

    #[test]
    fn the_status_bar_names_the_harness() {
        assert!(rendered(&app(), 80, 12).contains("Claude Code"));
    }

    #[test]
    fn scrolling_up_says_so_in_the_title_so_the_view_is_not_silently_stale() {
        let mut a = app();
        for i in 0..40 {
            a.push(Entry::Agent(format!("line {i}")));
        }
        a.scroll_up(5, 40);
        assert!(rendered(&a, 60, 12).contains("scrolled up"));
    }

    #[test]
    fn the_agents_panel_opens_over_the_transcript() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = vec![super::super::AgentLine {
            id: "abcdef1234".into(),
            name: "do the thing".into(),
            harness: "AGY".into(),
            status: "running".into(),
        }];
        let out = rendered(&a, 80, 16);
        assert!(out.contains("agents"), "{out}");
        assert!(out.contains("do the thing"), "{out}");
        assert!(out.contains("abcdef12"), "the id is shortened:\n{out}");
    }

    /// Every status the panel can show needs its own colour branch; a status
    /// string the panel has never seen must still render rather than fall off.
    #[test]
    fn the_agents_panel_shows_every_status_including_one_it_does_not_know() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = ["running", "completed", "failed", "killed", "something-new"]
            .iter()
            .enumerate()
            .map(|(i, status)| super::super::AgentLine {
                id: format!("id{i}00000000"),
                name: format!("agent-{status}"),
                harness: "Claude Code".into(),
                status: (*status).into(),
            })
            .collect();

        let out = rendered(&a, 100, 20);
        for status in ["running", "completed", "failed", "killed", "something-new"] {
            assert!(out.contains(&format!("agent-{status}")), "missing {status}:\n{out}");
        }
    }

    // --- one line per entry kind ----------------------------------------

    fn first_line(entry: &Entry) -> String {
        render(entry, 60)
            .first()
            .expect("an entry always renders at least one line")
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn the_users_own_line_is_marked_as_theirs() {
        assert!(first_line(&Entry::You("ask".into())).starts_with("› "));
    }

    #[test]
    fn thinking_is_indented_rather_than_prefixed() {
        assert_eq!(first_line(&Entry::Thinking("weighing".into())), "  weighing");
    }

    #[test]
    fn a_tool_call_and_a_failed_tool_call_are_told_apart() {
        assert!(first_line(&Entry::Tool { name: "bash".into(), failed: false }).starts_with("⚙ "));
        assert!(first_line(&Entry::Tool { name: "bash".into(), failed: true }).starts_with("✗ "));
    }

    #[test]
    fn a_finished_turn_reports_how_it_went() {
        assert_eq!(
            first_line(&Entry::Done { text: String::new(), failed: false }),
            "✓ done"
        );
        assert_eq!(
            first_line(&Entry::Done { text: String::new(), failed: true }),
            "✗ failed"
        );
        assert_eq!(
            first_line(&Entry::Done { text: "12 files".into(), failed: false }),
            "✓ done · 12 files"
        );
    }

    #[test]
    fn a_notice_is_bulleted_and_raw_output_is_left_alone() {
        assert!(first_line(&Entry::Notice("heads up".into())).starts_with("• "));
        assert_eq!(first_line(&Entry::Raw("warning: x".into())), "warning: x");
    }

    /// A wrapped entry keeps its prefix on the first line only, and indents the
    /// rest to line up under it.
    #[test]
    fn a_wrapped_entry_indents_its_continuation_lines() {
        let lines = render(&Entry::You("aaaa bbbb cccc dddd eeee".into()), 12);
        assert!(lines.len() > 1, "this should have wrapped");
        let text = |i: usize| -> String {
            lines[i].spans.iter().map(|s| s.content.to_string()).collect()
        };
        assert!(text(0).starts_with("› "));
        assert!(text(1).starts_with("  "), "got {:?}", text(1));
    }

    /// A blank line inside a message is part of the message — dropping it runs
    /// paragraphs together.
    #[test]
    fn a_blank_line_inside_a_message_is_kept() {
        assert_eq!(wrap("a\n\nb", 40, 0), vec!["a", "", "b"]);
    }

    #[test]
    fn an_empty_agents_panel_says_so_rather_than_showing_a_blank_box() {
        let mut a = app();
        a.pane = Pane::Agents;
        assert!(rendered(&a, 80, 16).contains("no agents yet"));
    }

    /// A terminal can be dragged to almost nothing; the layout must survive it.
    #[test]
    fn a_tiny_terminal_does_not_panic() {
        let a = app();
        for (w, h) in [(20, 5), (10, 4), (80, 6), (200, 60)] {
            let _ = rendered(&a, w, h);
        }
    }

    /// Regression: the panel sized itself to a comfortable minimum width
    /// regardless of the terminal, so a narrow window produced a rect wider
    /// than the buffer it was drawn into.
    #[test]
    fn the_agents_panel_never_outgrows_a_narrow_terminal() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = (0..30)
            .map(|i| super::super::AgentLine {
                id: format!("id{i}"),
                name: format!("agent {i}"),
                harness: "AGY".into(),
                status: "running".into(),
            })
            .collect();
        for (w, h) in [(10, 4), (12, 5), (18, 6), (21, 7), (40, 8)] {
            let _ = rendered(&a, w, h);
        }
    }

    #[test]
    fn a_very_long_line_is_wrapped_into_the_pane_not_truncated() {
        let mut a = app();
        a.push(Entry::Agent("word ".repeat(60).trim().to_string()));
        let out = rendered(&a, 40, 20);
        assert!(out.lines().all(|l| l.chars().count() <= 40));
        assert!(
            out.matches("word").count() > 10,
            "text must survive:\n{out}"
        );
    }

    #[test]
    fn multibyte_text_renders_without_panicking() {
        let mut a = app();
        a.push(Entry::Agent("café ☕ — naïve".into()));
        assert!(rendered(&a, 40, 10).contains("café"));
    }
}
