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
    let suggestions = crate::tui::command::completions(&app.input, &app.agents);
    if suggestions.is_empty() {
        return;
    }

    // Every row is now the same shape — mark, padded name, hint — so the width
    // is the widest of each part rather than the widest whole row.
    let widest_name = suggestions
        .iter()
        .map(|c| c.line.trim_end().chars().count())
        .max()
        .unwrap_or(0);
    let widest_hint = suggestions
        .iter()
        .map(|c| c.hint.chars().count())
        .max()
        .unwrap_or(0);
    let w = ((widest_name + widest_hint + 8) as u16).clamp(24, 72).min(input.width);
    // Only as tall as it needs to be, and never taller than the space above.
    let h = ((suggestions.len() + 2) as u16).min(input.y.saturating_sub(1)).max(3);
    let panel = Rect {
        x: input.x,
        y: input.y.saturating_sub(h),
        width: w,
        height: h,
    };

    let selected = app.suggestion.min(suggestions.len().saturating_sub(1));
    // Hints in a column. Ragged ones made a list of eighteen commands read as
    // noise, because the eye had no edge to run down.
    let names = suggestions
        .iter()
        .map(|c| c.line.trim_end().chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = suggestions
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (mark, style) = if i == selected {
                ("▸ ", Style::default().fg(USER).add_modifier(Modifier::BOLD))
            } else {
                ("  ", Style::default())
            };
            let name = c.line.trim_end();
            let pad = " ".repeat(names.saturating_sub(name.chars().count()) + 2);
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(name.to_string(), style),
                Span::styled(format!("{pad}{}", c.hint), Style::default().fg(MUTED)),
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

    if app.tasks.is_empty() {
        items.push(ListItem::new(Span::styled(
            "── board ── empty · /todo <title> adds one",
            Style::default().fg(MUTED),
        )));
    } else {
        items.push(ListItem::new(Span::styled(
            "── board ──",
            Style::default().fg(MUTED),
        )));
        let selected = app.task_sel.min(app.tasks.len().saturating_sub(1));
        for (i, t) in app.tasks.iter().enumerate() {
            // Open / claimed / done, so progress is readable at a glance.
            let (mark, colour) = if t.is_done() {
                ("✓", GOOD)
            } else if t.is_claimed() {
                ("◐", WARN)
            } else {
                ("○", MUTED)
            };
            let chosen = i == selected;
            items.push(ListItem::new(Line::from(vec![
                Span::styled(if chosen { "▸" } else { " " }, Style::default().fg(USER)),
                Span::styled(format!("{mark} "), Style::default().fg(colour)),
                Span::styled(format!("{:<10}", t.id), Style::default().fg(MUTED)),
                Span::styled(
                    t.title.clone(),
                    if chosen {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
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
        Some(name) => format!(" team {name} "),
        None => " team ".to_string(),
    };

    // Clear first: this floats over the transcript.
    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(USER))
                .title(title)
                .title_bottom(" ↑↓ pick · ⏎ mark done · /todo adds · Esc close "),
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

    // Naming what is on screen matters once several agents exist: a transcript
    // that could belong to any of them is a transcript you cannot trust.
    let watching = app
        .watching
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .map(|a| format!(" jod · {} ", a.name))
        .unwrap_or_else(|| " jod ".to_string());
    let title = if app.following() {
        watching
    } else {
        format!(
            "{}— scrolled up {} · Esc to follow ",
            watching, app.scroll
        )
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
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        // Leading spaces are content, not separators. Splitting on ' ' throws
        // them away, which silently reflowed every code block the agent
        // printed — `def f():` and its body ended up in the same column.
        let lead: String = raw.chars().take_while(|c| *c == ' ').collect();
        let body = raw[lead.len()..].to_string();
        let width = width.saturating_sub(lead.chars().count()).max(1);
        let before = out.len();
        let mut line = String::new();
        for word in body.split(' ') {
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
        // Re-apply the indent to every line this source line produced.
        for produced in out.iter_mut().skip(before) {
            if !produced.is_empty() {
                produced.insert_str(0, &lead);
            }
        }
    }
    out
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    // The box stays live while an agent works — a prompt typed now is queued,
    // not refused — so it keeps its colour and says what will happen instead.
    let title = match (app.busy, app.queued.len()) {
        (_, n) if n > 0 => format!(" you · {n} queued "),
        (true, _) => format!(
            " you · sends after this turn{} ",
            app.elapsed().map(|t| format!(" ({t})")).unwrap_or_default()
        ),
        _ => " you ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.busy { WARN } else { USER }))
        .title(title);

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
    // Working is the state worth colouring: it is the one the eye returns to.
    let left = Span::styled(
        app.status(),
        Style::default().fg(if app.busy { WARN } else { MUTED }),
    );
    // The hints follow what the pane can actually do. A panel's own keys are on
    // its footer, so here the bar offers the way out rather than repeating them.
    let hints = match app.pane {
        Pane::Chat if app.busy => "Ctrl-X stop · Ctrl-B delegate · Ctrl-A agents · Ctrl-C quit",
        Pane::Chat => "Ctrl-B delegate · Ctrl-A agents · Ctrl-G team · Ctrl-C quit",
        _ => "Esc closes this panel · Ctrl-C quit",
    };
    // The status grows — a spinner, a clock, a background count, a queue — so
    // the hints have to yield rather than collide with it. Running them
    // together produced `1 queuedCtrl-X stop`, which reads as neither.
    let used = left.content.chars().count() + 2;
    let room = (area.width as usize).saturating_sub(used);
    let mut spans = vec![Span::raw(" "), left];
    if room >= hints.chars().count() + 2 {
        spans.push(Span::raw(" ".repeat(room - hints.chars().count())));
        spans.push(Span::styled(hints, Style::default().fg(MUTED)));
    }
    spans.push(Span::raw(" "));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The agents panel: every delegation this process knows about, and the cursor
/// that manages them.
///
/// This is where unattended work is actually run, so it shows the two things a
/// list of names cannot — how long each has been going, and what it last said —
/// and it says which keys act on the selected row. A panel you can only look at
/// makes you leave the UI to do anything about what you saw.
fn draw_agents(f: &mut Frame, app: &App, area: Rect) {
    // Never larger than the terminal. Clamping to a comfortable minimum alone
    // can produce a panel wider than a narrow window, which would place the
    // rect outside the buffer.
    let w = area.width.saturating_sub(6).clamp(20, 96).min(area.width);
    let h = ((app.agents.len() + 3) as u16)
        .clamp(5, area.height.saturating_sub(2).max(5))
        .min(area.height);
    let panel = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Two rows go to the border and one to the footer of key hints.
    let rows = panel.height.saturating_sub(3).max(1) as usize;
    let selected = app.agent_sel.min(app.agents.len().saturating_sub(1));
    let first = window_start(selected, rows, app.agents.len());

    let items: Vec<ListItem> = if app.agents.is_empty() {
        vec![ListItem::new(Span::styled(
            "no agents yet — Ctrl-B delegates one",
            Style::default().fg(MUTED),
        ))]
    } else {
        app.agents
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, a)| {
                let colour = status_colour(&a.status);
                let chosen = i == selected;
                let name = Style::default().fg(if chosen { USER } else { AGENT }).add_modifier(
                    if chosen { Modifier::BOLD } else { Modifier::empty() },
                );
                // The age of a finished run is how long ago it started, which is
                // still the right number to sort your attention by.
                let age = crate::tui::app::short_duration(app.now_ms.saturating_sub(a.created_at_ms));
                let watched = app.watching.as_deref() == Some(a.id.as_str());
                ListItem::new(Line::from(vec![
                    Span::styled(if chosen { "▸ " } else { "  " }, Style::default().fg(USER)),
                    Span::styled(format!("{:<9}", short(&a.id)), Style::default().fg(MUTED)),
                    Span::styled(format!("{:<10}", a.status), Style::default().fg(colour)),
                    Span::styled(format!("{age:>7}  "), Style::default().fg(MUTED)),
                    // Thirteen, not eleven: "Claude Code" is exactly eleven
                    // characters, so a tighter column ran the harness straight
                    // into the name — `Claude CodeHow are you running?`.
                    Span::styled(
                        format!("{:<13}", a.harness),
                        Style::default().fg(MUTED),
                    ),
                    Span::styled(a.name.clone(), name),
                    Span::styled(
                        if watched { "  ← on screen" } else { "" },
                        Style::default().fg(USER),
                    ),
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
                .title(format!(
                    " agents · {} running of {} ",
                    app.running(),
                    app.agents.len()
                ))
                .title_bottom(" ↑↓ pick · ⏎ watch · s stop · r resume · a attach · Esc close "),
        ),
        panel,
    );
}

fn status_colour(status: &str) -> Color {
    match status {
        "running" => WARN,
        "completed" => GOOD,
        "failed" => BAD,
        _ => MUTED,
    }
}

/// The first visible row of a list `len` long showing `rows` at a time with
/// `selected` in view. Keeps the cursor on screen without jumping the window
/// about, which is what makes a long fleet navigable at all.
pub fn window_start(selected: usize, rows: usize, len: usize) -> usize {
    if len <= rows {
        return 0;
    }
    let last_start = len - rows;
    selected.saturating_sub(rows / 2).min(last_start)
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

    /// Regression: agents print code, and splitting on spaces threw the
    /// leading ones away — `def f():` and its body came out in one column.
    #[test]
    fn code_indentation_survives_wrapping() {
        let code = "def greet(name):\n    return f\"Hello, {name}!\"";
        let lines = wrap(code, 60, 0);
        assert_eq!(lines[0], "def greet(name):");
        assert_eq!(lines[1], "    return f\"Hello, {name}!\"");
    }

    #[test]
    fn a_wrapped_indented_line_keeps_its_indent_on_every_row() {
        let lines = wrap("    alpha beta gamma delta", 14, 0);
        assert!(lines.len() > 1, "should have wrapped: {lines:?}");
        for line in &lines {
            assert!(line.starts_with("    "), "lost the indent: {line:?}");
        }
    }

    #[test]
    fn deeper_indentation_is_preserved_too() {
        let lines = wrap("        deeply nested", 40, 0);
        assert_eq!(lines[0], "        deeply nested");
    }

    #[test]
    fn a_whitespace_only_line_stays_blank() {
        assert_eq!(wrap("a\n   \nb", 20, 0), vec!["a", "", "b"]);
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
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            last: None,
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
                session: None,
                created_at_ms: 0,
                cost_usd: None,
                last: None,
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

    fn agent_line(id: &str, name: &str, status: &str) -> super::super::AgentLine {
        super::super::AgentLine {
            id: id.into(),
            name: name.into(),
            harness: "Claude Code".into(),
            status: status.into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }
    }

    /// The panel is a control surface, not a list of names: it has to say what
    /// the selected row is and which keys act on it.
    #[test]
    fn the_agents_panel_marks_its_selection_and_offers_its_keys() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = vec![
            agent_line("aaa11111", "port the parser", "running"),
            agent_line("bbb22222", "write the docs", "completed"),
        ];
        a.agent_sel = 1;
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("▸"), "the selection must be visible:\n{screen}");
        assert!(screen.contains("⏎ watch"), "the keys must be stated:\n{screen}");
        assert!(screen.contains("s stop"));
        assert!(screen.contains("1 running of 2"), "{screen}");
    }

    /// How long a run has been going is the number that decides whether to look
    /// at it, and a list of names cannot tell you.
    #[test]
    fn the_agents_panel_shows_how_long_each_run_has_been_going() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.advance(125_000);
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("2m05s"), "expected an age:\n{screen}");
    }

    /// Regression: the harness column was exactly as wide as "Claude Code", so
    /// it ran straight into the name — `Claude CodeHow are you running?`.
    #[test]
    fn the_panel_columns_do_not_run_into_each_other() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = vec![agent_line("aaa11111", "How are you running?", "completed")];
        let screen = rendered(&a, 100, 20);
        assert!(
            screen.contains("Claude Code  How are you running?"),
            "columns must be separated:\n{screen}"
        );
    }

    /// Eighteen ragged rows read as noise; the eye needs an edge to run down.
    #[test]
    fn the_completion_hints_line_up_in_a_column() {
        let mut a = app();
        a.input = "/".into();
        let screen = rendered(&a, 100, 30);
        // Counted in characters, not bytes: the selection marker is three bytes
        // wide and one column wide, and a byte index would call the two rows
        // misaligned when they are not.
        let column = |line: &str, hint: &str| {
            line.find(hint).map(|byte| line[..byte].chars().count())
        };
        let starts: Vec<usize> = screen
            .lines()
            .filter_map(|l| column(l, "this list").or_else(|| column(l, "the team panel")))
            .collect();
        assert_eq!(starts.len(), 2, "expected both rows:\n{screen}");
        assert_eq!(starts[0], starts[1], "hints must share a column:\n{screen}");
    }

    #[test]
    fn the_agents_panel_says_which_run_is_on_screen() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.watching = Some("aaa11111".into());
        assert!(rendered(&a, 100, 20).contains("on screen"));
    }

    /// A fleet longer than the panel must scroll to keep the cursor visible,
    /// not silently hide the row being acted on.
    #[test]
    fn a_long_fleet_scrolls_to_keep_the_selection_in_view() {
        let mut a = app();
        a.pane = Pane::Agents;
        a.agents = (0..40)
            .map(|i| agent_line(&format!("id{i:06}"), &format!("job {i}"), "running"))
            .collect();
        a.agent_sel = 39;
        let screen = rendered(&a, 100, 16);
        assert!(screen.contains("job 39"), "the selected row must show:\n{screen}");
        assert!(!screen.contains("job 0 "), "the top must have scrolled off");
    }

    #[test]
    fn the_visible_window_keeps_the_cursor_inside_it() {
        assert_eq!(window_start(0, 10, 5), 0, "a short list never scrolls");
        assert_eq!(window_start(0, 10, 40), 0);
        assert_eq!(window_start(20, 10, 40), 15, "centred on the cursor");
        assert_eq!(window_start(39, 10, 40), 30, "clamped to the last page");
    }

    /// The whole point of delegating is that the work continues off screen.
    #[test]
    fn the_status_bar_reports_agents_working_off_screen() {
        let mut a = app();
        a.agents = vec![agent_line("bbb22222", "audit the deps", "running")];
        assert!(rendered(&a, 100, 12).contains("1 in background"));
    }

    /// Regression: the status and the hints were run together on a narrow
    /// terminal — `1 queuedCtrl-X stop`, which reads as neither.
    #[test]
    fn the_status_bar_drops_its_hints_rather_than_colliding_with_them() {
        let mut a = app();
        a.busy = true;
        a.turn_started_ms = Some(0);
        a.advance(5_000);
        a.queue("next".into());
        let screen = rendered(&a, 60, 12);
        let bar = screen.lines().last().unwrap();
        assert!(bar.contains("1 queued"), "the status wins: {bar}");
        assert!(!bar.contains("queuedCtrl"), "they must not run together: {bar}");

        // With room for both, the hints come back.
        assert!(rendered(&a, 140, 12).lines().last().unwrap().contains("Ctrl-X stop"));
    }

    #[test]
    fn the_input_box_says_a_prompt_is_waiting_rather_than_looking_broken() {
        let mut a = app();
        a.busy = true;
        a.queue("next thing".into());
        assert!(rendered(&a, 80, 12).contains("1 queued"));
    }

    #[test]
    fn a_busy_input_box_says_when_the_line_will_be_sent() {
        let mut a = app();
        a.busy = true;
        assert!(rendered(&a, 80, 12).contains("sends after this turn"));
    }

    /// A transcript that could belong to any of several agents is one you
    /// cannot trust.
    #[test]
    fn the_transcript_names_the_agent_it_is_showing() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.watching = Some("aaa11111".into());
        assert!(rendered(&a, 80, 12).contains("port the parser"));
    }

    #[test]
    fn the_team_panel_offers_its_keys_too() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        a.tasks = vec![task("t1", "port the parser", None, "open")];
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("mark done"), "{screen}");
        assert!(screen.contains("▸"), "the selection must be visible:\n{screen}");
    }

    #[test]
    fn an_empty_board_says_how_to_add_to_it() {
        let mut a = app();
        a.pane = Pane::Team;
        a.team = Some("crew".into());
        assert!(rendered(&a, 100, 20).contains("/todo"));
    }

    #[test]
    fn multibyte_text_renders_without_panicking() {
        let mut a = app();
        a.push(Entry::Agent("café ☕ — naïve".into()));
        assert!(rendered(&a, 40, 10).contains("café"));
    }
}
