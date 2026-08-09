//! Drawing. Every decision about *what* to show lives in `app.rs`; this file
//! only turns that into widgets, so the rules stay testable without a terminal.

use jod_core::AgentStatus;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{describe_usage, status_line, App, Focus, LineKind, Mode, StreamLine, View};

/// Reasoning is dimmed and italic rather than loud: it is context for the
/// answer, and must never compete with the answer for attention.
fn style_for(kind: &LineKind) -> Style {
    match kind {
        LineKind::Reasoning => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::ITALIC | Modifier::DIM),
        LineKind::Message => Style::default().fg(Color::White),
        LineKind::ToolCall => Style::default().fg(Color::Cyan),
        LineKind::ToolOk => Style::default().fg(Color::Green),
        LineKind::ToolError => Style::default().fg(Color::Red),
        LineKind::Raw => Style::default().fg(Color::DarkGray),
        LineKind::System => Style::default().fg(Color::Blue).add_modifier(Modifier::DIM),
        LineKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn status_glyph(status: AgentStatus) -> (&'static str, Color) {
    match status {
        AgentStatus::Running => ("●", Color::Yellow),
        AgentStatus::Completed => ("✓", Color::Green),
        AgentStatus::Failed => ("✗", Color::Red),
        AgentStatus::Killed => ("■", Color::DarkGray),
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let editing = app.mode != Mode::Normal;
    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(if editing { 3 } else { 0 }),
    ])
    .split(frame.area());

    let columns =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).split(rows[0]);

    draw_fleet(frame, app, columns[0]);
    match app.view {
        View::Stream => draw_stream(frame, app, columns[1]),
        View::Team => draw_team(frame, app, columns[1]),
        View::Help => draw_help(frame, columns[1]),
    }
    draw_status(frame, app, rows[1]);
    if editing {
        draw_input(frame, app, rows[2]);
    }
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_fleet(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .agents
        .iter()
        .map(|agent| {
            let (glyph, colour) = status_glyph(agent.status);
            let header = Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(colour)),
                Span::styled(
                    agent.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", agent.harness_label),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let detail = Line::from(Span::styled(
                format!("   {}", describe_usage(&agent.usage)),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(vec![header, detail])
        })
        .collect();

    let title = format!(" fleet ({}) ", app.agents.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(focus_border(app.focus == Focus::Fleet))
                .title(title),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let mut state = ListState::default();
    if !app.agents.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_stream(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .stream_lines()
        .iter()
        .map(|StreamLine { kind, text }| {
            Line::from(Span::styled(text.clone(), style_for(kind)))
        })
        .collect();

    let title = match app.selected_agent() {
        Some(agent) => format!(
            " {} · {} · {} ",
            agent.name,
            agent.harness_label,
            agent.model.as_deref().unwrap_or("default")
        ),
        None => " stream ".to_string(),
    };

    // Following the tail means scrolling to the bottom; the widget takes a
    // top-line offset, so it has to be computed from the height available.
    let height = area.height.saturating_sub(2);
    let scroll = if app.follow_tail {
        (lines.len() as u16).saturating_sub(height)
    } else {
        app.scroll
    };

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(focus_border(app.focus == Focus::Stream))
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_team(frame: &mut Frame, app: &App, area: Rect) {
    let halves = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let members: Vec<Line> = if app.members.is_empty() {
        vec![Line::from(Span::styled(
            "no members yet",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.members
            .iter()
            .map(|m| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<12}", m.name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{:<14}", m.harness.label()),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(format!("{:?}", m.status), Style::default().fg(Color::Yellow)),
                    Span::styled(format!("  {}", m.role), Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect()
    };

    let title = format!(
        " team: {} ",
        app.team_name.as_deref().unwrap_or("none — press m to message")
    );
    frame.render_widget(
        Paragraph::new(members)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false }),
        halves[0],
    );

    let tasks: Vec<Line> = if app.tasks.is_empty() {
        vec![Line::from(Span::styled(
            "no tasks on the board",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.tasks
            .iter()
            .map(|t| {
                let (mark, colour) = if t.done {
                    ("✓", Color::Green)
                } else if t.claimed_by.is_some() {
                    ("◐", Color::Yellow)
                } else {
                    ("○", Color::DarkGray)
                };
                Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(colour)),
                    Span::raw(t.title.clone()),
                    Span::styled(
                        t.claimed_by
                            .as_ref()
                            .map(|w| format!("  ({w})"))
                            .unwrap_or_default(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            })
            .collect()
    };

    frame.render_widget(
        Paragraph::new(tasks)
            .block(Block::default().borders(Borders::ALL).title(" task board "))
            .wrap(Wrap { trim: false }),
        halves[1],
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let keys = [
        ("j / k, ↑ ↓", "move in the fleet, or scroll the stream"),
        ("Tab", "switch focus between fleet and stream"),
        ("i / Enter", "follow up — another turn on the same session"),
        ("n", "start a new agent"),
        ("h", "cycle which harness a new agent uses"),
        ("r", "show or hide reasoning"),
        ("t", "team view: members and the task board"),
        ("m", "put a message on the team bus"),
        ("a", "show the tmux attach command"),
        ("x", "kill the selected agent's session"),
        ("?", "this help"),
        ("q", "quit — running agents keep going in tmux"),
    ];
    let lines: Vec<Line> = keys
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<12}"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(*what),
            ])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" keys ")),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let left = status_line(app);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {left}"), Style::default().fg(Color::Gray)),
            Span::styled("  ?  for keys", Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.mode {
        Mode::Compose => " follow up (Enter sends · Esc cancels) ",
        Mode::Spawn => " new agent prompt (Enter starts · Esc cancels) ",
        Mode::Message => " message the team (Enter sends · Esc cancels) ",
        Mode::Normal => " ",
    };
    frame.render_widget(
        Paragraph::new(app.input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
    // Park the cursor after what has been typed, so the terminal shows a caret.
    let x = area.x + 1 + (app.input.chars().count() as u16 % area.width.saturating_sub(2).max(1));
    frame.set_cursor_position((x, area.y + 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, Key};
    use jod_core::event::AgentEnvelope;
    use jod_core::{
        AgentEvent, HarnessKind, Member, MemberStatus, PermissionPolicy, Task, Usage,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render the app and return the screen as lines of text, so assertions can
    /// be about what a person would actually see.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn flat(app: &App) -> String {
        screen(app, 120, 24).join("\n")
    }

    fn agent(id: &str, harness: HarnessKind) -> jod_core::AgentSummary {
        jod_core::AgentSummary {
            id: id.into(),
            name: format!("agent-{id}"),
            harness,
            harness_label: harness.label().into(),
            status: jod_core::AgentStatus::Running,
            cwd: "/tmp".into(),
            model: None,
            permission: PermissionPolicy::Ask,
            tmux_session: format!("jod-{id}"),
            attach_command: String::new(),
            switch_command: String::new(),
            session_closed: false,
            created_at_ms: 0,
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
            stream_path: String::new(),
        }
    }

    fn app_with_one() -> App {
        App {
            agents: vec![agent("a", HarnessKind::ClaudeCode)],
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_fleet_still_draws_both_panes_and_the_status_bar() {
        let text = flat(&App::default());
        assert!(text.contains("fleet (0)"));
        assert!(text.contains("No agents yet"));
        assert!(text.contains("reasoning on"));
    }

    /// The headline feature has to survive all the way to the screen, not just
    /// to `stream_lines`.
    #[test]
    fn reasoning_reaches_the_screen_and_disappears_when_toggled_off() {
        let mut app = app_with_one();
        app.ingest(AgentEnvelope {
            agent_id: "a".into(),
            at_ms: 0,
            seq: 0,
            event: AgentEvent::Thinking { text: "weighing options".into() },
        });
        assert!(flat(&app).contains("weighing options"));

        app.on_key(Key::Char('r'));
        assert!(!flat(&app).contains("weighing options"));
    }

    #[test]
    fn the_fleet_shows_each_agent_with_its_harness() {
        let app = App {
            agents: vec![
                agent("a", HarnessKind::ClaudeCode),
                agent("b", HarnessKind::Antigravity),
            ],
            ..Default::default()
        };
        let text = flat(&app);
        assert!(text.contains("agent-a"));
        assert!(text.contains("agent-b"));
        assert!(text.contains("Antigravity"));
        assert!(text.contains("fleet (2)"));
    }

    #[test]
    fn every_agent_status_renders_a_glyph() {
        for status in [
            jod_core::AgentStatus::Running,
            jod_core::AgentStatus::Completed,
            jod_core::AgentStatus::Failed,
            jod_core::AgentStatus::Killed,
        ] {
            let mut app = app_with_one();
            app.agents[0].status = status;
            let (glyph, _) = status_glyph(status);
            assert!(flat(&app).contains(glyph), "{status:?} lost its glyph");
        }
    }

    #[test]
    fn the_team_view_lists_members_and_the_task_board() {
        let app = App {
            view: View::Team,
            team_name: Some("crew".into()),
            members: vec![Member {
                name: "scout".into(),
                harness: HarnessKind::OpenCode,
                role: "research".into(),
                status: MemberStatus::Busy,
                agent_id: None,
                session_id: None,
            }],
            tasks: vec![
                Task { id: "t1".into(), title: "port parser".into(), claimed_by: Some("scout".into()), done: false },
                Task { id: "t2".into(), title: "write docs".into(), claimed_by: None, done: true },
            ],
            ..Default::default()
        };
        let text = flat(&app);
        assert!(text.contains("crew"));
        assert!(text.contains("scout"));
        assert!(text.contains("Busy"));
        assert!(text.contains("port parser"));
        assert!(text.contains("write docs"));
    }

    #[test]
    fn an_empty_team_says_so_in_both_halves() {
        let app = App { view: View::Team, ..Default::default() };
        let text = flat(&app);
        assert!(text.contains("no members yet"));
        assert!(text.contains("no tasks on the board"));
    }

    #[test]
    fn the_help_view_lists_every_binding() {
        let app = App { view: View::Help, ..Default::default() };
        let text = flat(&app);
        for expected in ["show or hide reasoning", "team view", "quit"] {
            assert!(text.contains(expected), "help is missing {expected:?}");
        }
    }

    #[test]
    fn composing_opens_an_input_box_with_what_was_typed() {
        let mut app = app_with_one();
        app.on_key(Key::Char('i'));
        for c in "hello there".chars() {
            app.on_key(Key::Char(c));
        }
        let text = flat(&app);
        assert!(text.contains("follow up"));
        assert!(text.contains("hello there"));
    }

    #[test]
    fn each_editing_mode_names_itself() {
        let mut app = app_with_one();
        app.on_key(Key::Char('n'));
        assert!(flat(&app).contains("new agent prompt"));

        let mut app = app_with_one();
        app.team_name = Some("crew".into());
        app.on_key(Key::Char('m'));
        assert!(flat(&app).contains("message the team"));
    }

    #[test]
    fn a_transient_status_is_shown_along_the_bottom() {
        let mut app = app_with_one();
        assert_eq!(app.on_key(Key::Char('a')), Action::Attach { agent_id: "a".into() });
        app.status = Some("run: tmux attach -t jod-a".into());
        assert!(flat(&app).contains("tmux attach -t jod-a"));
    }

    #[test]
    fn tool_calls_and_errors_reach_the_screen() {
        let mut app = app_with_one();
        for event in [
            AgentEvent::ToolCall { name: "Bash".into(), input: None },
            AgentEvent::ToolResult { name: "Bash".into(), summary: Some("ok".into()), is_error: false },
            AgentEvent::Error { message: "it broke".into() },
            AgentEvent::Raw { line: "mystery".into() },
        ] {
            app.ingest(AgentEnvelope { agent_id: "a".into(), at_ms: 0, seq: 0, event });
        }
        let text = flat(&app);
        assert!(text.contains("Bash"));
        assert!(text.contains("it broke"));
        assert!(text.contains("mystery"));
    }

    /// Every line kind must have a style; a missing arm would be a panic or a
    /// silently unstyled line.
    #[test]
    fn every_line_kind_has_a_style() {
        for kind in [
            LineKind::Reasoning,
            LineKind::Message,
            LineKind::ToolCall,
            LineKind::ToolOk,
            LineKind::ToolError,
            LineKind::Raw,
            LineKind::System,
            LineKind::Error,
        ] {
            let _ = style_for(&kind);
        }
        // Reasoning is dimmed so it never competes with the answer.
        assert!(style_for(&LineKind::Reasoning)
            .add_modifier
            .contains(Modifier::ITALIC));
    }

    #[test]
    fn focus_is_visible_on_whichever_pane_has_it() {
        assert_ne!(focus_border(true), focus_border(false));
    }

    /// A tiny terminal must not panic — layouts get squeezed, not broken.
    #[test]
    fn a_very_small_terminal_still_renders() {
        let app = app_with_one();
        let _ = screen(&app, 20, 6);
        let _ = screen(&app, 8, 4);
    }

    #[test]
    fn a_long_stream_scrolls_rather_than_overflowing() {
        let mut app = app_with_one();
        for i in 0..200 {
            app.ingest(AgentEnvelope {
                agent_id: "a".into(),
                at_ms: 0,
                seq: i,
                event: AgentEvent::Message { text: format!("line {i}") },
            });
        }
        // Following the tail shows the end, not the beginning.
        let text = flat(&app);
        assert!(text.contains("line 199"), "the newest output must be visible");

        app.focus = Focus::Stream;
        app.scroll_up(500);
        let text = flat(&app);
        assert!(text.contains("line 0"), "scrolling up reaches the start");
    }
}
