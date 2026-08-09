//! Drawing the TUI. Returns the transcript viewport height so the caller can
//! make PageUp move by exactly one screen.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use jod_core::team::MemberStatus;

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
    if app.pane == Pane::Team {
        draw_team(f, app, f.area());
    }
    // Last, so it floats over everything including the panels.
    draw_completions(f, app, chunks[1]);
    height
}

/// The slash-command popup, sitting directly above the input box.
///
/// Above rather than below because the input is already near the bottom of the
/// screen, and a list that grows downwards would be clipped exactly when it is
/// longest.
fn draw_completions(f: &mut Frame, app: &App, input: Rect) {
    let suggestions = crate::tui::command::completions(&app.input);
    if suggestions.is_empty() {
        return;
    }

    let widest = suggestions
        .iter()
        .map(|c| c.line.chars().count() + c.hint.chars().count() + 6)
        .max()
        .unwrap_or(20);
    let w = (widest as u16).clamp(24, 64).min(input.width);
    // Only as tall as it needs to be, and never taller than the space above.
    let h = ((suggestions.len() + 2) as u16).min(input.y.saturating_sub(1)).max(3);
    let panel = Rect {
        x: input.x,
        y: input.y.saturating_sub(h),
        width: w,
        height: h,
    };

    let selected = app.suggestion.min(suggestions.len().saturating_sub(1));
    let items: Vec<ListItem> = suggestions
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (mark, style) = if i == selected {
                ("▸ ", Style::default().fg(USER).add_modifier(Modifier::BOLD))
            } else {
                ("  ", Style::default())
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(c.line.clone(), style),
                Span::styled(format!("  {}", c.hint), Style::default().fg(MUTED)),
            ]))
        })
        .collect();

    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(MUTED))
                .title(" Tab completes · ↑↓ choose "),
        ),
        panel,
    );
}

/// The team panel: who is on it, and what each of them is doing.
///
/// Members and the task board share one floating panel because they are one
/// question — "is this team making progress" — and splitting them across two
/// keystrokes would make the answer take two looks.
fn draw_team(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.members.len() + app.tasks.len() + 3;
    let w = area.width.saturating_sub(8).clamp(24, 76).min(area.width);
    let h = (rows as u16)
        .clamp(6, area.height.saturating_sub(4).max(6))
        .min(area.height);
    let panel = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    let mut items: Vec<ListItem> = Vec::new();

    if app.team.is_none() {
        items.push(ListItem::new(Span::styled(
            "no team — start one with `jod tui --team <name>`",
            Style::default().fg(MUTED),
        )));
    } else if app.members.is_empty() {
        items.push(ListItem::new(Span::styled(
            "no members yet",
            Style::default().fg(MUTED),
        )));
    } else {
        for m in &app.members {
            let colour = match m.status {
                MemberStatus::Busy => WARN,
                MemberStatus::Ready => GOOD,
                MemberStatus::Error => BAD,
                _ => MUTED,
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{:<12}", m.name), Style::default().fg(USER)),
                Span::styled(
                    format!("{:<11}", m.status.as_str()),
                    Style::default().fg(colour),
                ),
                Span::styled(
                    format!("{:<13}", m.harness.label()),
                    Style::default().fg(MUTED),
                ),
                Span::raw(m.role.clone()),
            ])));
        }
    }

    if !app.tasks.is_empty() {
        items.push(ListItem::new(Span::styled(
            "── board ──",
            Style::default().fg(MUTED),
        )));
        for t in &app.tasks {
            // Open / claimed / done, so progress is readable at a glance.
            let (mark, colour) = if t.is_done() {
                ("✓", GOOD)
            } else if t.is_claimed() {
                ("◐", WARN)
            } else {
                ("○", MUTED)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{mark} "), Style::default().fg(colour)),
                Span::raw(t.title.clone()),
                Span::styled(
                    t.owner
                        .as_ref()
                        .map(|o| format!("  ({o})"))
                        .unwrap_or_default(),
                    Style::default().fg(MUTED),
                ),
            ])));
        }
    }

    let title = match &app.team {
        Some(name) => format!(" team {name} · Ctrl-G to close "),
        None => " team · Ctrl-G to close ".to_string(),
    };

    // Clear first: this floats over the transcript.
    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(USER))
                .title(title),
        ),
        panel,
    );
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
        Entry::Tool {
            name,
            detail,
            failed,
        } => {
            let mark = if *failed { "✗ " } else { "⚙ " };
            let style = Style::default().fg(if *failed { BAD } else { MUTED });
            let body = match detail {
                Some(d) => format!("{name} · {d}"),
                None => name.clone(),
            };
            (mark, style, body)
        }
        // Indented under its call, so output reads as belonging to the tool
        // above it rather than as the agent speaking.
        Entry::ToolOut { text, failed } => (
            "  └ ",
            Style::default().fg(if *failed { BAD } else { MUTED }),
            text.clone(),
        ),
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
        "Ctrl-A agents · Ctrl-G team · Ctrl-T thinking · Ctrl-C quit",
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
    use jod_core::team::{Member, TeamTask};
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

    fn member(name: &str, harness: HarnessKind, status: MemberStatus) -> Member {
        Member {
            team: "crew".into(),
            name: name.into(),
            harness,
            role: "research".into(),
            status,
            agent_id: None,
            session_id: None,
        }
    }

    fn task(id: &str, title: &str, owner: Option<&str>, status: &str) -> TeamTask {
        TeamTask {
            id: id.into(),
            title: title.into(),
            owner: owner.map(str::to_string),
            status: status.into(),
        }
    }

    #[test]
    fn typing_a_slash_opens_the_completion_popup() {
        let mut a = app();
        assert!(!rendered(&a, 100, 20).contains("Tab completes"));

        a.input = "/".into();
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("Tab completes"));
        assert!(screen.contains("/help"));
        assert!(screen.contains("/harness"));
    }

    #[test]
    fn the_popup_narrows_as_you_type_and_shows_the_hint() {
        let mut a = app();
        a.input = "/th".into();
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("/thinking"));
        assert!(screen.contains("show or hide reasoning"), "the hint is shown");
        assert!(!screen.contains("/help"));
    }

    #[test]
    fn the_highlighted_suggestion_is_marked() {
        let mut a = app();
        a.input = "/".into();
        a.suggestion = 1;
        assert!(rendered(&a, 100, 20).contains("▸"));
    }

    #[test]
    fn harness_arguments_are_offered_in_the_popup() {
        let mut a = app();
        a.input = "/harness ".into();
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("claude"));
        assert!(screen.contains("agy"));
    }

    #[test]
    fn a_plain_prompt_shows_no_popup() {
        let mut a = app();
        a.input = "what is in this repo".into();
        assert!(!rendered(&a, 100, 20).contains("Tab completes"));
    }

    /// The popup sits above the input, so it must not be drawn outside the
    /// buffer on a short terminal.
    #[test]
    fn the_popup_fits_a_short_terminal() {
        let mut a = app();
        a.input = "/".into();
        let _ = rendered(&a, 60, 8);
        let _ = rendered(&a, 30, 6);
        let _ = rendered(&a, 24, 5);
    }

    #[test]
    fn the_team_panel_lists_members_with_their_harness_and_status() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        a.members = vec![
            member("lead", HarnessKind::ClaudeCode, MemberStatus::Busy),
            member("scout", HarnessKind::Agy, MemberStatus::Ready),
        ];
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("team crew"));
        assert!(screen.contains("lead"));
        assert!(screen.contains("scout"));
        assert!(screen.contains("busy"));
        assert!(screen.contains("Antigravity") || screen.contains("AGY"));
    }

    /// The board has to distinguish open, claimed and done at a glance, or it
    /// is just a list of strings.
    #[test]
    fn the_task_board_shows_progress_and_who_owns_what() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        a.members = vec![member("scout", HarnessKind::OpenCode, MemberStatus::Busy)];
        a.tasks = vec![
            task("t1", "port the parser", Some("scout"), "open"),
            task("t2", "write the docs", None, "open"),
            task("t3", "ship it", Some("lead"), "done"),
        ];
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("board"));
        assert!(screen.contains("port the parser"));
        assert!(screen.contains("(scout)"), "a claimed task names its owner");
        assert!(screen.contains("write the docs"));
        assert!(screen.contains("✓"), "a done task is marked");
        assert!(screen.contains("○"), "an unclaimed task is marked");
    }

    /// Without a team the panel must explain itself rather than show an empty
    /// box that reads as a bug.
    #[test]
    fn the_team_panel_says_so_when_there_is_no_team() {
        let mut a = app();
        a.pane = Pane::Team;
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("no team"), "got:\n{screen}");
    }

    #[test]
    fn a_team_with_no_members_yet_says_so() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("no members yet"));
    }

    #[test]
    fn the_team_panel_is_hidden_unless_its_pane_is_open() {
        let mut a = app();
        a.team = Some("crew".into());
        a.members = vec![member("scout", HarnessKind::OpenCode, MemberStatus::Ready)];
        assert!(!rendered(&a, 100, 20).contains("scout"));

        a.pane = Pane::Team;
        assert!(rendered(&a, 100, 20).contains("scout"));
    }

    /// The panel floats over the transcript, so it must fit a small terminal
    /// rather than being placed outside the buffer.
    #[test]
    fn the_team_panel_fits_a_small_terminal() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        a.members = (0..40)
            .map(|i| {
                member(
                    &format!("m{i}"),
                    HarnessKind::ClaudeCode,
                    MemberStatus::Ready,
                )
            })
            .collect();
        let _ = rendered(&a, 30, 8);
        let _ = rendered(&a, 12, 5);
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
