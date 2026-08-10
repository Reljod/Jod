//! Drawing the TUI. Returns the transcript viewport height so the caller can
//! make PageUp move by exactly one screen.
//!
//! Two rules hold everywhere below. **`draw()` is a pure function of state** —
//! nothing here reads a clock, a store or a file, because the 250 ms tick is
//! what refreshes and a render that can fail is a UI that can die. And **colour
//! is never the only channel**: every state carries a glyph, `NO_COLOR` is
//! honoured, and only the eight named ANSI colours are used, because those are
//! the ones the user's own theme controls and Jod runs on other people's boxes
//! over SSH.

use std::sync::LazyLock;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use jod_core::team::MemberStatus;

use super::app::{absolute, since, until, App, Entry, Overlay};
use super::data::{Outcome, Source};
use super::graph::{self, Direction as EdgeDirection};
use super::keys;
use super::workspace::Workspace;

const USER: Color = Color::Cyan;
const AGENT: Color = Color::Reset;
const MUTED: Color = Color::DarkGray;
const BAD: Color = Color::Red;
const GOOD: Color = Color::Green;
const WARN: Color = Color::Yellow;

/// `NO_COLOR` is not optional: software that adds ANSI colour by default has to
/// check for it. Read once rather than per span, because `draw()` may not do
/// I/O.
static COLOURLESS: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()));

/// A foreground colour, or nothing at all when the user asked for nothing at
/// all. Every call site still has a glyph, so `NO_COLOR` loses decoration and
/// never information.
fn fg(colour: Color) -> Style {
    if *COLOURLESS {
        Style::default()
    } else {
        Style::default().fg(colour)
    }
}

fn bold(colour: Color) -> Style {
    fg(colour).add_modifier(Modifier::BOLD)
}

pub fn draw(f: &mut Frame, app: &App) -> usize {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.workspace == Workspace::Chat {
            [
                Constraint::Min(3),    // transcript
                Constraint::Length(3), // input
                Constraint::Length(1), // keybar
                Constraint::Length(1), // status
            ]
        } else {
            [
                Constraint::Min(3),    // the workspace itself
                Constraint::Length(0), // no input box outside chat
                Constraint::Length(1), // keybar
                Constraint::Length(1), // status
            ]
        })
        .split(f.area());

    let height = if app.workspace == Workspace::Chat {
        let height = draw_transcript(f, app, chunks[0]);
        draw_input(f, app, chunks[1]);
        height
    } else {
        draw_workspace(f, app, chunks[0]);
        // A workspace list pages by its own height, not the transcript's.
        chunks[0].height.saturating_sub(4).max(1) as usize
    };
    draw_keybar(f, app, chunks[2]);
    draw_status(f, app, chunks[3]);

    // Last, so they float over everything.
    draw_completions(f, app, chunks[1]);
    draw_overlay(f, app);
    height
}

// ---- chrome ------------------------------------------------------------

/// The context-sensitive keybar: this screen's verbs on the left, the way out
/// on the right.
///
/// It is on every screen, always, because the same letter deliberately means
/// different things on different ones — and that is only safe while both are
/// printed.
fn draw_keybar(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let ws = app.workspace;
    let (left, right) = match &app.overlay {
        Overlay::WhichKey => ("Ctrl-K … waiting for a key".to_string(), "Esc cancels"),
        Overlay::WhichKeyNew => ("Ctrl-K n … s schedule · g goal · h hook · m memory · t task".to_string(), "Esc cancels"),
        Overlay::Keymap => ("the keymap — any key closes it".to_string(), "Esc closes"),
        Overlay::Confirm { .. } => ("y confirms · anything else cancels".to_string(), "Esc cancels"),
        Overlay::Prompt { .. } => ("⏎ accepts · Esc cancels".to_string(), "Esc cancels"),
        Overlay::None if app.here().editing_filter => {
            ("typing filters this list".to_string(), "⏎ keeps it · Esc clears it")
        }
        Overlay::None => (keys::keybar(ws), keys::keybar_exit(ws)),
    };
    f.render_widget(Paragraph::new(two_ends(&left, right, area.width, MUTED)), area);
}

/// The status bar: where you are on the left, what is waiting on the right.
fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let ws = app.workspace;
    let left = if ws == Workspace::Chat {
        app.status()
    } else {
        format!("{} · {}", ws.title(), app.count_for(ws))
    };
    // Endings that arrive while you are away have to survive until you look.
    let badge = match app.unread() {
        0 => String::new(),
        n => format!("⚑ {n} unread"),
    };
    let style = if app.busy && ws == Workspace::Chat {
        fg(WARN)
    } else {
        fg(MUTED)
    };
    let mut spans = vec![Span::raw(" "), Span::styled(left.clone(), style)];
    // The status grows — a spinner, a clock, a background count, a queue — so
    // the badge has to yield rather than collide with it. Running them together
    // produced `1 queuedCtrl-X stop`, which reads as neither.
    let used = left.chars().count() + 2;
    let room = (area.width as usize).saturating_sub(used);
    if !badge.is_empty() && room >= badge.chars().count() + 2 {
        spans.push(Span::raw(" ".repeat(room - badge.chars().count())));
        spans.push(Span::styled(badge, fg(WARN)));
    }
    spans.push(Span::raw(" "));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Two strings on one row, the right one dropped rather than allowed to
/// collide.
fn two_ends(left: &str, right: &str, width: u16, colour: Color) -> Line<'static> {
    let used = left.chars().count() + 2;
    let room = (width as usize).saturating_sub(used);
    let mut spans = vec![Span::raw(" "), Span::styled(left.to_string(), fg(colour))];
    if room >= right.chars().count() + 2 {
        spans.push(Span::raw(" ".repeat(room - right.chars().count())));
        spans.push(Span::styled(right.to_string(), fg(colour)));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The slash-command popup, sitting directly above the input box.
///
/// Above rather than below because the input is already near the bottom of the
/// screen, and a list that grows downwards would be clipped exactly when it is
/// longest.
fn draw_completions(f: &mut Frame, app: &App, input: Rect) {
    if app.workspace != Workspace::Chat {
        return;
    }
    let suggestions = crate::tui::command::completions(&app.input, app);
    if suggestions.is_empty() {
        return;
    }

    // Every row is the same shape — mark, padded name, hint — so the width is
    // the widest of each part rather than the widest whole row.
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
    let w = ((widest_name + widest_hint + 8) as u16)
        .clamp(24, 72)
        .min(input.width);
    // Only as tall as it needs to be, and never taller than the space above.
    let h = ((suggestions.len() + 2) as u16)
        .min(input.y.saturating_sub(1))
        .max(3);
    let panel = Rect {
        x: input.x,
        y: input.y.saturating_sub(h),
        width: w,
        height: h,
    };

    let selected = app.suggestion.min(suggestions.len().saturating_sub(1));
    // With thirty-odd commands the list outgrows the space above the input, so
    // it scrolls to keep the highlighted row visible — otherwise pressing ↓
    // past the fold moves a cursor nobody can see.
    let rows = panel.height.saturating_sub(2).max(1) as usize;
    let first = window_start(selected, rows, suggestions.len());
    let items: Vec<ListItem> = suggestions
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, c)| {
            let (mark, style) = if i == selected {
                ("▸ ", bold(USER))
            } else {
                ("  ", Style::default())
            };
            let name = c.line.trim_end();
            let pad = " ".repeat(widest_name.saturating_sub(name.chars().count()) + 2);
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(name.to_string(), style),
                Span::styled(format!("{pad}{}", c.hint), fg(MUTED)),
            ]))
        })
        .collect();

    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" Tab completes · ↑↓ choose "),
        ),
        panel,
    );
}

// ---- overlays ----------------------------------------------------------

fn draw_overlay(f: &mut Frame, app: &App) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::WhichKey | Overlay::WhichKeyNew => draw_which_key(f, app),
        Overlay::Keymap => draw_keymap(f, app),
        Overlay::Confirm { verb, what, .. } => draw_confirm(f, verb, what),
        Overlay::Prompt { label, value, .. } => draw_prompt(f, label, value),
    }
}

/// The which-key menu: every workspace, one letter each, with a **live count**
/// beside it — so the menu is also a dashboard and you often get your answer
/// without pressing the second key.
fn draw_which_key(f: &mut Frame, app: &App) {
    let making = app.overlay == Overlay::WhichKeyNew;
    let mut rows: Vec<(String, String)> = Vec::new();
    if making {
        rows.push(("s".into(), "schedule".into()));
        rows.push(("g".into(), "goal".into()));
        rows.push(("h".into(), "hook".into()));
        rows.push(("m".into(), "memory".into()));
        rows.push(("t".into(), "task".into()));
    } else {
        for ws in Workspace::MENU {
            // The digit is printed beside the letter so the two routes are
            // visibly the same destination rather than two things to learn.
            rows.push((
                ws.letter().unwrap_or(' ').to_string(),
                format!(
                    "{:<12}{:<44}{}",
                    ws.menu_name(),
                    app.count_for(ws),
                    ws.digit().map(|d| format!("or {d}")).unwrap_or_default()
                ),
            ));
        }
        rows.push((String::new(), String::new()));
        rows.push((
            "n".into(),
            "new…        n s sched · n g goal · n h hook · n t task".into(),
        ));
        rows.push(("e".into(), "editor      the input in $EDITOR".into()));
        rows.push(("?".into(), "keys        the whole keymap".into()));
    }

    let widest = rows.iter().map(|(_, t)| t.chars().count()).max().unwrap_or(0);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(letter, text)| {
            if letter.is_empty() {
                return ListItem::new(Line::from(""));
            }
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {letter}  "), bold(USER)),
                Span::styled(text.clone(), fg(AGENT)),
            ]))
        })
        .collect();

    let title = if making {
        " Ctrl-K n · new… "
    } else {
        " Ctrl-K "
    };
    let panel = centred(f.area(), (widest + 10) as u16, (rows.len() + 2) as u16);
    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(title)
                .title_bottom(" Esc cancels · any other key is ignored "),
        ),
        panel,
    );
}

/// The `?` overlay, showing the screen you are on first. Help that omits the
/// focused screen's verbs sends you to the source.
fn draw_keymap(f: &mut Frame, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for (heading, bindings) in keys::keymap(app.workspace) {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(heading, bold(USER))));
        for binding in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<12}", binding.key), fg(WARN)),
                Span::styled(binding.what.to_string(), fg(AGENT)),
            ]));
        }
    }
    let widest = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(20);
    let panel = centred(f.area(), (widest + 6) as u16, (lines.len() + 2) as u16);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(" keys ")
                .title_bottom(" any key closes this "),
        ),
        panel,
    );
}

/// A destructive verb on a bare letter is one fat-fingered `Ctrl-K h x` away
/// from losing a secret, so the confirmation **names the thing**.
fn draw_confirm(f: &mut Frame, verb: &str, what: &str) {
    let question = format!("{verb} {what}?");
    let panel = centred(f.area(), (question.chars().count() + 8) as u16, 5);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(format!("  {question}"), bold(BAD))),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(BAD))
                .title(" this cannot be undone ")
                .title_bottom(" y confirms · anything else cancels "),
        ),
        panel,
    );
}

/// Tier 1 of the form ladder: one value, no screen change, no context lost.
fn draw_prompt(f: &mut Frame, label: &str, value: &str) {
    let width = (label.chars().count() + value.chars().count() + 16).max(40);
    let panel = centred(f.area(), width as u16, 5);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("  {label} ▸ "), fg(MUTED)),
                Span::styled(value.to_string(), fg(AGENT)),
                Span::styled("▏", fg(USER)),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title_bottom(" ⏎ accepts · Esc cancels "),
        ),
        panel,
    );
}

/// A panel of at most `w` × `h`, centred, and never larger than the terminal —
/// clamping to a comfortable minimum alone produces a rect outside the buffer
/// on a narrow window.
fn centred(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.clamp(12, area.width.max(12)).min(area.width);
    let h = h.clamp(3, area.height.max(3)).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

// ---- workspaces --------------------------------------------------------

fn draw_workspace(f: &mut Frame, app: &App, area: Rect) {
    match app.workspace {
        Workspace::Fleet => draw_fleet(f, app, area),
        Workspace::Memory => draw_memory(f, app, area),
        Workspace::MemoryGraph => draw_graph(f, app, area),
        Workspace::Schedules => draw_schedules(f, app, area),
        Workspace::Goals => draw_goals(f, app, area),
        Workspace::Hooks => draw_hooks(f, app, area),
        Workspace::Tasks => draw_tasks(f, app, area),
        Workspace::Activity => draw_activity(f, app, area),
        Workspace::Team => draw_team(f, app, area),
        Workspace::Chat => {}
    }
}

/// A master/detail split, or the master alone when there is not room for both.
///
/// Below 90 columns the detail pane is the first thing to go: a 40-column
/// detail pane holds nothing worth reading, and clipping the master to make
/// room for it is the anti-pattern.
fn split(area: Rect) -> (Rect, Option<Rect>) {
    if area.width < 90 {
        return (area, None);
    }
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(area);
    (halves[0], Some(halves[1]))
}

/// The list rows a pane can show, and where the window starts.
fn window(area: Rect, selected: usize, len: usize) -> (usize, usize) {
    let rows = area.height.saturating_sub(3).max(1) as usize;
    (window_start(selected, rows, len), rows)
}

/// The filter line, printed under a list so an active filter is never invisible.
fn filter_line(app: &App) -> Option<Line<'static>> {
    let list = app.here();
    let filter = list.filter.as_ref()?;
    let cursor = if list.editing_filter { "▏" } else { "" };
    // A filter that is open but empty hides nothing, so it says so rather than
    // claiming a match count that is really the whole list.
    let what = if list.filtering() {
        format!("   ▸ filter · {} match", app.row_ids(app.workspace).len())
    } else {
        "   ▸ type to filter".to_string()
    };
    Some(Line::from(vec![
        Span::styled(format!(" /{filter}{cursor}"), fg(USER)),
        Span::styled(what, fg(MUTED)),
    ]))
}

fn titled(ws: Workspace, app: &App) -> String {
    format!(" {} · {} ", ws.title(), app.count_for(ws))
}

fn body<'a>(ws: Workspace, app: &App, items: Vec<ListItem<'a>>) -> List<'a> {
    List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(fg(USER))
            .title(titled(ws, app))
            .title_bottom(keys::footer(ws)),
    )
}

fn empty(what: &str) -> Vec<ListItem<'static>> {
    vec![ListItem::new(Span::styled(what.to_string(), fg(MUTED)))]
}

/// The fleet: every delegation this process knows about, and the cursor that
/// manages them.
///
/// A panel you can only look at makes you leave the UI to do anything about
/// what you saw, so it says how long each has been going, what it last said,
/// and which keys act on the selected row.
fn draw_fleet(f: &mut Frame, app: &App, area: Rect) {
    let (left, right) = split(area);
    let rows = app.fleet_rows();
    let selected = app.list(Workspace::Fleet).index(&app.row_ids(Workspace::Fleet));
    let (first, height) = window(left, selected, rows.len());
    // Declared drop order: detail pane → harness → id, name last.
    let inner = left.width.saturating_sub(2) as usize;
    let show_id = inner >= 34;
    let show_harness = inner >= 30;

    let items: Vec<ListItem> = if rows.is_empty() {
        empty("no agents yet — Ctrl-B delegates one")
    } else {
        rows.iter()
            .enumerate()
            .skip(first)
            .take(height)
            .map(|(i, a)| {
                let chosen = i == selected;
                let age = super::app::short_duration(app.now_ms.saturating_sub(a.created_at_ms));
                let watched = app.watching.as_deref() == Some(a.id.as_str());
                let mut spans = vec![
                    Span::styled(if chosen { "▸ " } else { "  " }, fg(USER)),
                    Span::styled(
                        format!("{} ", run_glyph(&a.status)),
                        fg(status_colour(&a.status)),
                    ),
                ];
                if show_id {
                    spans.push(Span::styled(format!("{:<9}", short(&a.id)), fg(MUTED)));
                }
                spans.push(Span::styled(
                    format!("{:<9}", a.status),
                    fg(status_colour(&a.status)),
                ));
                spans.push(Span::styled(format!("{age:>7} "), fg(MUTED)));
                if show_harness {
                    spans.push(Span::styled(format!("{:<4}", code(&a.harness)), fg(MUTED)));
                }
                spans.push(Span::styled(
                    a.name.clone(),
                    if chosen { bold(USER) } else { fg(AGENT) },
                ));
                if watched {
                    spans.push(Span::styled("  ← on screen", fg(USER)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect()
    };
    let mut items = items;
    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }
    f.render_widget(body(Workspace::Fleet, app, items), left);

    let Some(right) = right else { return };
    let lines = match app.selected_agent() {
        None => vec![Line::from(Span::styled("nothing selected", fg(MUTED)))],
        Some(a) => {
            let mut lines = vec![
                Line::from(Span::styled(a.name.clone(), bold(AGENT))),
                Line::from(Span::styled(a.id.clone(), fg(MUTED))),
                Line::from(""),
                field("harness", &a.harness),
                field("status", &a.status),
                field(
                    "started",
                    &super::app::short_duration(app.now_ms.saturating_sub(a.created_at_ms)),
                ),
                field(
                    "session",
                    a.session.as_deref().unwrap_or("none reported yet"),
                ),
                field(
                    "spend",
                    &a.cost_usd
                        .map(|c| format!("${c:.2}"))
                        .unwrap_or_else(|| "—".into()),
                ),
                Line::from(""),
            ];
            match &a.last {
                Some(text) => {
                    lines.push(Line::from(Span::styled("last", fg(MUTED))));
                    for chunk in wrap(text, right.width.saturating_sub(4) as usize, 2) {
                        lines.push(Line::from(Span::styled(format!("  {chunk}"), fg(AGENT))));
                    }
                }
                None => lines.push(Line::from(Span::styled(
                    "it has not said anything yet",
                    fg(MUTED),
                ))),
            }
            lines
        }
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" run ")
                .title_bottom(" ⏎ watch · s stop · r resume · a attach "),
        ),
        right,
    );
}

fn field(name: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<9}"), fg(MUTED)),
        Span::styled(value.to_string(), fg(AGENT)),
    ])
}

/// Memory as a list, type carried by a three-letter tag *and* a glyph so
/// neither has to work alone. `deg` is the node's degree — the cheapest honest
/// answer to "is this memory load-bearing?".
fn draw_memory(f: &mut Frame, app: &App, area: Rect) {
    let (left, right) = split(area);
    let rows = app.memory_rows();
    let selected = app
        .list(Workspace::Memory)
        .index(&app.row_ids(Workspace::Memory));
    let (first, height) = window(left, selected, rows.len());
    let inner = left.width.saturating_sub(2) as usize;
    // Drop order: detail pane → degree → confidence.
    let show_degree = inner >= 40;
    let show_conf = inner >= 34;

    let mut items: Vec<ListItem> = if rows.is_empty() {
        empty("nothing remembered yet — /remember writes one")
    } else {
        rows.iter()
            .enumerate()
            .skip(first)
            .take(height)
            .map(|(i, n)| {
                let chosen = i == selected;
                let mut spans = vec![
                    Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
                    Span::styled(format!("{} ", n.kind.glyph()), fg(kind_colour(n))),
                    Span::styled(format!("{:<5}", n.kind.tag()), fg(MUTED)),
                    Span::styled(
                        format!("{:<22}", cut(&n.name, 22)),
                        if chosen { bold(USER) } else { fg(AGENT) },
                    ),
                ];
                if show_conf {
                    spans.push(Span::styled(format!("{:>5.2} ", n.confidence), fg(MUTED)));
                }
                if show_degree {
                    spans.push(Span::styled(format!("{:>4} ", n.degree), fg(MUTED)));
                }
                // `!` marks a node in an unresolved contradiction, so the state
                // survives a monochrome terminal.
                spans.push(Span::styled(
                    if n.contradicted { "!" } else { " " },
                    fg(BAD),
                ));
                ListItem::new(Line::from(spans))
            })
            .collect()
    };
    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }
    if let Some(kind) = app.memory_type {
        items.push(ListItem::new(Span::styled(
            format!(" showing {} only · t cycles", kind.label()),
            fg(WARN),
        )));
    }
    f.render_widget(body(Workspace::Memory, app, items), left);

    let Some(right) = right else { return };
    let lines = match app.selected_memory() {
        None => vec![Line::from(Span::styled("nothing selected", fg(MUTED)))],
        Some(n) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(n.name.clone(), bold(AGENT)),
                    Span::styled(format!("   {}", n.kind.label()), fg(MUTED)),
                ]),
                Line::from(Span::styled(
                    format!(
                        "conf {:.2} · {} edges · seen {}×",
                        n.confidence,
                        n.degree,
                        n.seen
                    ),
                    fg(MUTED),
                )),
                Line::from(""),
            ];
            for chunk in wrap(&n.body, right.width.saturating_sub(4) as usize, 0) {
                lines.push(Line::from(Span::styled(chunk, fg(AGENT))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("▲ linked from ({})", n.in_edges.len()),
                fg(MUTED),
            )));
            for edge in &n.in_edges {
                lines.push(edge_line(edge));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("▼ links to ({})", n.out_edges.len()),
                fg(MUTED),
            )));
            for edge in &n.out_edges {
                lines.push(edge_line(edge));
            }
            if !n.provenance.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("provenance", fg(MUTED))));
                for source in &n.provenance {
                    lines.push(Line::from(Span::styled(format!("  {source}"), fg(MUTED))));
                }
            }
            lines
        }
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" node ")
                .title_bottom(" g local graph · e edit · x forget "),
        ),
        right,
    );
}

fn edge_line(edge: &super::data::MemoryEdge) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<14}", edge.kind), fg(MUTED)),
        Span::styled(format!("{} ", edge.other_kind.glyph()), fg(AGENT)),
        Span::styled(edge.other_name.clone(), fg(AGENT)),
        Span::styled(if edge.warn { "  ⚠" } else { "" }, fg(BAD)),
    ])
}

/// The local graph: one node, in-edges above, out-edges below, and the trail of
/// where you have been along the bottom. No layout algorithm anywhere.
fn draw_graph(f: &mut Frame, app: &App, area: Rect) {
    let node = app.focused_memory();
    let mut lines: Vec<Line> = Vec::new();

    let Some(node) = node else {
        f.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                "  that node is gone — Esc goes back to the list",
                fg(MUTED),
            ))])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(USER))
                    .title(" memory · local graph "),
            ),
            area,
        );
        return;
    };

    let rows = graph::neighbours(node, app.graph.edge_kind.as_deref());
    let ins: Vec<(usize, &graph::Neighbour)> = rows
        .iter()
        .enumerate()
        .filter(|(_, n)| n.direction == EdgeDirection::In)
        .collect();
    let outs: Vec<(usize, &graph::Neighbour)> = rows
        .iter()
        .enumerate()
        .filter(|(_, n)| n.direction == EdgeDirection::Out)
        .collect();

    lines.push(Line::from(Span::styled(
        format!("   ▲  linked from — {}", ins.len()),
        fg(MUTED),
    )));
    for (i, n) in &ins {
        lines.push(neighbour_line(*i == app.graph.sel, n));
    }
    lines.push(Line::from(""));
    // The focus, boxed, so the eye never has to hunt for the middle.
    lines.push(Line::from(vec![
        Span::styled("   ┏━ ", fg(USER)),
        Span::styled(format!("{} {}", node.kind.glyph(), node.name), bold(USER)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "   ┃  {} · conf {:.2} · seen {}×",
            node.kind.label(),
            node.confidence,
            node.seen
        ),
        fg(MUTED),
    )));
    for chunk in wrap(&node.body, area.width.saturating_sub(10) as usize, 0) {
        lines.push(Line::from(Span::styled(
            format!("   ┃  {chunk}"),
            fg(AGENT),
        )));
    }
    lines.push(Line::from(Span::styled("   ┗━", fg(USER))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("   ▼  links to — {}", outs.len()),
        fg(MUTED),
    )));
    for (i, n) in &outs {
        lines.push(neighbour_line(*i == app.graph.sel, n));
    }

    lines.push(Line::from(""));
    // A pane that silently truncates leaves you believing you have seen the
    // whole neighbourhood.
    lines.push(Line::from(Span::styled(
        format!(
            "   {}  ↑↓ walks the edges · ⏎ re-centres on one",
            graph::coverage(app.graph.hops, rows.len(), node.degree.max(rows.len()))
        ),
        fg(MUTED),
    )));
    if let Some(kind) = &app.graph.edge_kind {
        lines.push(Line::from(Span::styled(
            format!("   showing {kind} edges only · f cycles"),
            fg(WARN),
        )));
    }
    lines.push(Line::from(vec![
        Span::styled(format!("   {}", app.graph.trail_line()), fg(MUTED)),
        Span::styled("   ← where you have been", fg(MUTED)),
    ]));

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(format!(" memory · local graph · {} ", node.name))
                .title_bottom(" ↑↓ neighbour · ⏎ re-centre · Backspace back · h hops · g list "),
        ),
        area,
    );
}

fn neighbour_line(chosen: bool, n: &graph::Neighbour) -> Line<'static> {
    let arrow = match n.direction {
        EdgeDirection::In => "──▶",
        EdgeDirection::Out => "◀──",
    };
    Line::from(vec![
        Span::styled(if chosen { "  ▸ " } else { "    " }, fg(USER)),
        Span::styled(format!("{} ", n.edge.other_kind.glyph()), fg(AGENT)),
        Span::styled(
            format!("{:<26}", cut(&n.edge.other_name, 26)),
            if chosen { bold(USER) } else { fg(AGENT) },
        ),
        Span::styled(format!("{arrow} {}", n.edge.kind), fg(MUTED)),
        Span::styled(if n.edge.warn { "  ⚠" } else { "" }, fg(BAD)),
    ])
}

/// Schedules in the `systemctl list-timers` shape — **when** (a human gloss),
/// **next** (absolute), **in** (relative), **last**, **ago** — plus a seven-cell
/// outcome strip. The raw cron expression lives in the detail block: a column
/// of `0 2 * * *` is a column nobody can read at a glance.
fn draw_schedules(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.schedule_rows();
    let selected = app
        .list(Workspace::Schedules)
        .index(&app.row_ids(Workspace::Schedules));
    let width = area.width.saturating_sub(2) as usize;
    // Declared drop order: the 7-day strip → the LAST/AGO pair → the gloss.
    let show_strip = width >= 92;
    let show_last = width >= 74;
    let show_gloss = width >= 56;

    let mut lines: Vec<Line> = Vec::new();
    let mut header = vec![format!("  {:<18}", "name")];
    if show_gloss {
        header.push(format!("{:<20}", "when"));
    }
    header.push(format!("{:<14}", "next"));
    header.push(format!("{:>8} ", "in"));
    if show_last {
        header.push(format!("{:<14}{:>6} ", "last", "ago"));
    }
    if show_strip {
        header.push("7d".into());
    }
    lines.push(Line::from(Span::styled(header.concat(), fg(MUTED))));

    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing scheduled yet — n makes one, /new schedule too",
            fg(MUTED),
        )));
    }
    let (first, height) = window(area, selected, rows.len());
    for (i, s) in rows.iter().enumerate().skip(first).take(height.min(12)) {
        let chosen = i == selected;
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(format!("{} ", s.state.glyph()), fg(schedule_colour(s.state))),
            Span::styled(
                format!("{:<17}", cut(&s.name, 17)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
        ];
        if show_gloss {
            spans.push(Span::styled(format!("{:<20}", cut(&s.gloss, 19)), fg(AGENT)));
        }
        spans.push(Span::styled(
            format!("{:<14}", s.next_ms.map(absolute).unwrap_or_else(|| "— paused".into())),
            fg(MUTED),
        ));
        spans.push(Span::styled(
            format!("{:>8} ", until(app.now_ms, s.next_ms)),
            fg(AGENT),
        ));
        if show_last {
            spans.push(Span::styled(
                format!(
                    "{:<14}{:>6} ",
                    s.last_ms.map(absolute).unwrap_or_else(|| "—".into()),
                    since(app.now_ms, s.last_ms)
                ),
                fg(MUTED),
            ));
        }
        if show_strip {
            spans.push(strip_span(&s.history));
        }
        lines.push(Line::from(spans));
    }

    if let Some(line) = filter_line(app) {
        lines.push(Line::from(""));
        lines.push(line);
    }

    if let Some(s) = app.selected_schedule() {
        lines.push(Line::from(""));
        lines.push(rule(width));
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", s.name), bold(AGENT)),
            Span::styled(format!("   cron {} · {}", s.cron, s.timezone), fg(MUTED)),
        ]));
        lines.push(Line::from(""));
        lines.push(detail("prompt", &s.prompt));
        lines.push(detail("runs as", &s.runs_as));
        lines.push(detail("policy", &s.policy));
        if !s.recent.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  history", fg(MUTED))));
            for run in s.recent.iter().take(5) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "    {}  {}  {:>7}  ${:<5.2} {}",
                        absolute(run.at_ms),
                        run.outcome.mark(),
                        super::app::short_duration(run.duration_ms),
                        run.cost_usd,
                        run.note
                    ),
                    fg(if run.outcome == Outcome::Failed { BAD } else { MUTED }),
                )));
            }
        }
    }

    f.render_widget(page(Workspace::Schedules, app, lines), area);
}

/// A goal is a schedule with a **denominator**: "done when" is a checklist, so
/// there is a real percent-done. That is why goals get a progress bar and
/// nothing else does.
fn draw_goals(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.goal_rows();
    let selected = app
        .list(Workspace::Goals)
        .index(&app.row_ids(Workspace::Goals));
    let width = area.width.saturating_sub(2) as usize;
    // Drop order: iteration number → cadence → the bar (the percent stays).
    let show_iteration = width >= 88;
    let show_cadence = width >= 70;
    let show_bar = width >= 60;

    let mut lines: Vec<Line> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no goals yet — n makes one, /new goal too",
            fg(MUTED),
        )));
    }
    let (first, height) = window(area, selected, rows.len());
    for (i, g) in rows.iter().enumerate().skip(first).take(height.min(10)) {
        let chosen = i == selected;
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(format!("{} ", g.state.glyph()), fg(goal_colour(g.state))),
            Span::styled(
                format!("{:<20}", cut(&g.name, 20)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
        ];
        if show_cadence {
            spans.push(Span::styled(format!("{:<12}", cut(&g.cadence, 11)), fg(MUTED)));
        }
        if show_bar {
            spans.push(Span::styled(bar(g.percent(), 10), fg(GOOD)));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!("{:>3}%  ", g.percent()), fg(AGENT)));
        spans.push(Span::styled(
            format!("{:>8}  ", until(app.now_ms, g.next_ms)),
            fg(MUTED),
        ));
        spans.push(Span::styled(
            format!("{:<10}", g.state.label()),
            fg(goal_colour(g.state)),
        ));
        if show_iteration {
            spans.push(Span::styled(format!("iter {:>4}", g.iteration), fg(MUTED)));
        }
        lines.push(Line::from(spans));
    }

    if let Some(line) = filter_line(app) {
        lines.push(Line::from(""));
        lines.push(line);
    }

    if let Some(g) = app.selected_goal() {
        lines.push(Line::from(""));
        lines.push(rule(width));
        lines.push(Line::from(Span::styled(format!("  {}", g.name), bold(AGENT))));
        lines.push(Line::from(""));
        lines.push(detail("objective", &g.objective));
        lines.push(Line::from(Span::styled("  done when", fg(MUTED))));
        for check in &g.checks {
            let mark = if check.done { "☑" } else { "☐" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {mark} {}", check.text),
                    fg(if check.done { GOOD } else { AGENT }),
                ),
                Span::styled(
                    check.note.as_ref().map(|n| format!("   ← {n}")).unwrap_or_default(),
                    fg(WARN),
                ),
            ]));
        }
        lines.push(detail("stop if", &g.stop_if));
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<10}", "budget"), fg(MUTED)),
            Span::styled(
                format!("${:.2} of ${:.2}  ", g.spent_usd, g.budget_usd),
                fg(AGENT),
            ),
            Span::styled(
                bar(
                    if g.budget_usd > 0.0 {
                        ((g.spent_usd / g.budget_usd) * 100.0) as u16
                    } else {
                        0
                    },
                    10,
                ),
                fg(WARN),
            ),
        ]));
        if !g.iterations.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  iterations", fg(MUTED))));
            for it in g.iterations.iter().take(5) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "    {:>4}  {}  {:<44} {:>7}  ${:<5.2} {}",
                        it.n,
                        absolute(it.at_ms),
                        cut(&it.note, 44),
                        super::app::short_duration(it.duration_ms),
                        it.cost_usd,
                        it.outcome.mark()
                    ),
                    fg(if it.outcome == Outcome::Failed { BAD } else { MUTED }),
                )));
            }
        }
        // A looping objective that quietly needs you and never says so is worse
        // than no goal at all.
        if let Some(escalation) = &g.escalation {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  needs you   {escalation}"),
                bold(WARN),
            )));
        }
    }

    f.render_widget(page(Workspace::Goals, app, lines), area);
}

/// Webhooks answer three questions without a drill-down: is it armed, is the
/// secret still verifying, and what did the last delivery actually start.
fn draw_hooks(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.hook_rows();
    let selected = app
        .list(Workspace::Hooks)
        .index(&app.row_ids(Workspace::Hooks));
    let width = area.width.saturating_sub(2) as usize;
    // Drop order: 24 h count → repo → the event filter's detail.
    let show_24h = width >= 90;
    let show_repo = width >= 76;

    let mut lines: Vec<Line> = Vec::new();
    let mut header = vec![format!("  {:<16}", "name")];
    if show_repo {
        header.push(format!("{:<20}", "repo"));
    }
    header.push(format!("{:<28}{:<12}", "event", "runs"));
    if show_24h {
        header.push(format!("{:>4}  {:>6} ", "24h", "last"));
    }
    lines.push(Line::from(Span::styled(header.concat(), fg(MUTED))));

    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no webhooks yet — n makes one, /new hook too",
            fg(MUTED),
        )));
    }
    let (first, height) = window(area, selected, rows.len());
    for (i, h) in rows.iter().enumerate().skip(first).take(height.min(10)) {
        let chosen = i == selected;
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(format!("{} ", h.state.glyph()), fg(hook_colour(h.state))),
            Span::styled(
                format!("{:<15}", cut(&h.name, 15)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
        ];
        if show_repo {
            spans.push(Span::styled(format!("{:<20}", cut(&h.repo, 19)), fg(MUTED)));
        }
        spans.push(Span::styled(format!("{:<28}", cut(&h.event, 27)), fg(AGENT)));
        spans.push(Span::styled(format!("{:<12}", cut(&h.runs, 11)), fg(MUTED)));
        if show_24h {
            spans.push(Span::styled(
                format!("{:>4}  {:>6} ", h.deliveries_24h, since(app.now_ms, h.last_ms)),
                fg(MUTED),
            ));
            spans.push(Span::styled(
                h.last_outcome.mark(),
                fg(if h.last_outcome == Outcome::Failed { BAD } else { GOOD }),
            ));
        }
        lines.push(Line::from(spans));
    }

    if let Some(line) = filter_line(app) {
        lines.push(Line::from(""));
        lines.push(line);
    }

    if let Some(h) = app.selected_hook() {
        lines.push(Line::from(""));
        lines.push(rule(width));
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", h.name), bold(AGENT)),
            Span::styled(
                format!("   created {} · {} deliveries", h.created, h.total),
                fg(MUTED),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<10}", "endpoint"), fg(MUTED)),
            Span::styled(h.endpoint.clone(), fg(AGENT)),
            Span::styled(format!("   secret {}", h.secret), fg(MUTED)),
        ]));
        lines.push(detail("match", &h.match_rule));
        lines.push(detail("runs", &h.runs_as));
        lines.push(detail("prompt", &h.prompt));
        lines.push(detail("policy", &h.policy));
        if !h.deliveries.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  deliveries", fg(MUTED))));
            for d in h.deliveries.iter().take(5) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "    {}  {:<8} {:<32} {}  → {}  {}",
                        absolute(d.at_ms),
                        cut(&d.id, 8),
                        cut(&d.what, 32),
                        if d.accepted { "✓ 202" } else { "✗" },
                        d.run.clone().unwrap_or_else(|| "—".into()),
                        d.verdict
                    ),
                    fg(MUTED),
                )));
            }
        }
    }

    f.render_widget(page(Workspace::Hooks, app, lines), area);
}

/// The board, promoted out of the team panel into a screen of its own. The verb
/// that makes it worth a screen is `d`: it turns a task into an agent run, so
/// the board is where work *starts* rather than a list kept in parallel with
/// the fleet.
fn draw_tasks(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.task_rows();
    let selected = app
        .list(Workspace::Tasks)
        .index(&app.row_ids(Workspace::Tasks));
    let width = area.width.saturating_sub(2) as usize;
    // Drop order: run → age → owner. The state glyph always stays.
    let show_run = width >= 86;
    let show_age = width >= 72;
    let show_owner = width >= 62;

    let mut lines: Vec<Line> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  the board is empty — n adds a task, /todo does too",
            fg(MUTED),
        )));
    }
    let (first, height) = window(area, selected, rows.len());
    for (i, t) in rows.iter().enumerate().skip(first).take(height.min(12)) {
        let chosen = i == selected;
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(format!("{} ", t.state.glyph()), fg(task_colour(t.state))),
            Span::styled(format!("{:<18}", cut(&t.id, 18)), fg(MUTED)),
            Span::styled(
                format!("{:<38}", cut(&t.title, 38)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
        ];
        if show_owner {
            spans.push(Span::styled(
                format!("{:<9}", t.owner.clone().unwrap_or_else(|| "—".into())),
                fg(MUTED),
            ));
        }
        spans.push(Span::styled(
            format!("{:<9}", t.state.label()),
            fg(task_colour(t.state)),
        ));
        if show_run {
            spans.push(Span::styled(
                format!("{:<10}", t.run.as_deref().map(short).unwrap_or_else(|| "—".into())),
                fg(MUTED),
            ));
        }
        if show_age {
            spans.push(Span::styled(
                super::app::short_duration(t.age_ms),
                fg(MUTED),
            ));
        }
        lines.push(Line::from(spans));
    }

    if let Some(line) = filter_line(app) {
        lines.push(Line::from(""));
        lines.push(line);
    }

    if let Some(t) = app.selected_board_task() {
        lines.push(Line::from(""));
        lines.push(rule(width));
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", t.id), bold(AGENT)),
            Span::styled(
                format!("   {} · {}", t.state.label(), t.owner.clone().unwrap_or_else(|| "unclaimed".into())),
                fg(MUTED),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(detail("what", &t.what));
        lines.push(detail(
            "check",
            if t.check.trim().is_empty() {
                "none yet — a task without a runnable check has no stop signal"
            } else {
                &t.check
            },
        ));
        if !t.blocked_by.is_empty() {
            lines.push(detail("blocked", &t.blocked_by.join(", ")));
        }
        if !t.blocks.is_empty() {
            lines.push(detail("blocks", &t.blocks.join(", ")));
        }
        for entry in t.history.iter().take(4) {
            lines.push(Line::from(Span::styled(format!("    {entry}"), fg(MUTED))));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  d delegates this to an agent: a fresh run seeded with the title, the check and the spec.",
            fg(MUTED),
        )));
    }

    f.render_widget(page(Workspace::Tasks, app, lines), area);
}

/// The durable answer to "what happened while I was away". A toast-only ending
/// that scrolled off the transcript did not happen.
fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.activity_rows();
    let selected = app
        .list(Workspace::Activity)
        .index(&app.row_ids(Workspace::Activity));
    let width = area.width.saturating_sub(2) as usize;
    // Drop order: the source column (the glyph already carries it) → seconds.
    let show_source = width >= 70;

    let mut lines: Vec<Line> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing has happened yet — cron, hooks and goals write here",
            fg(MUTED),
        )));
    }
    let (first, height) = window(area, selected, rows.len());
    for (i, item) in rows.iter().enumerate().skip(first).take(height.min(16)) {
        let chosen = i == selected;
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            // An unread dot in the gutter, so unread survives without colour.
            Span::styled(if item.unread { "● " } else { "  " }, fg(WARN)),
            Span::styled(format!("{}  ", clock(item.at_ms)), fg(MUTED)),
        ];
        if show_source {
            spans.push(Span::styled(
                format!("{:<8}", item.source.label()),
                fg(source_colour(item.source)),
            ));
        }
        spans.push(Span::styled(
            format!("{} ", item.source.glyph()),
            fg(source_colour(item.source)),
        ));
        spans.push(Span::styled(
            item.text.clone(),
            if chosen { bold(AGENT) } else { fg(AGENT) },
        ));
        if item.needs_you {
            spans.push(Span::styled("  ← needs you", bold(WARN)));
        }
        lines.push(Line::from(spans));
    }

    if let Some(line) = filter_line(app) {
        lines.push(Line::from(""));
        lines.push(line);
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  filter  {}      only unread: {}      f cycles · u toggles",
            app.activity_source
                .map(|s| s.label().to_string())
                .unwrap_or_else(|| "[all]".into()),
            if app.unread_only { "on" } else { "off" }
        ),
        fg(MUTED),
    )));

    f.render_widget(page(Workspace::Activity, app, lines), area);
}

/// The team: who is on it, and what each of them is doing.
///
/// Members and the board share one screen because they are one question — "is
/// this team making progress" — and splitting them would make the answer take
/// two looks.
fn draw_team(f: &mut Frame, app: &App, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();

    if app.team.is_none() {
        items.push(ListItem::new(Span::styled(
            "no team — start one with `jod tui --team <name>`",
            fg(MUTED),
        )));
    } else if app.members.is_empty() {
        items.push(ListItem::new(Span::styled("no members yet", fg(MUTED))));
    } else {
        for m in &app.members {
            let colour = match m.status {
                MemberStatus::Busy => WARN,
                MemberStatus::Ready => GOOD,
                MemberStatus::Error => BAD,
                _ => MUTED,
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{:<12}", m.name), fg(USER)),
                Span::styled(format!("{:<11}", m.status.as_str()), fg(colour)),
                Span::styled(format!("{:<13}", m.harness.label()), fg(MUTED)),
                Span::raw(m.role.clone()),
            ])));
        }
    }

    if app.tasks.is_empty() {
        items.push(ListItem::new(Span::styled(
            "── board ── empty · /todo <title> adds one",
            fg(MUTED),
        )));
    } else {
        items.push(ListItem::new(Span::styled("── board ──", fg(MUTED))));
        let selected = app
            .list(Workspace::Team)
            .index(&app.row_ids(Workspace::Team));
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
                Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
                Span::styled(format!("{mark} "), fg(colour)),
                Span::styled(format!("{:<10}", t.id), fg(MUTED)),
                Span::styled(
                    t.title.clone(),
                    if chosen {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    t.owner.as_ref().map(|o| format!("  ({o})")).unwrap_or_default(),
                    fg(MUTED),
                ),
            ])));
        }
    }

    let title = match &app.team {
        Some(name) => format!(" team {name} "),
        None => " team ".to_string(),
    };

    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(title)
                .title_bottom(" ↑↓ pick · ⏎ mark done · /todo adds · Esc back "),
        ),
        area,
    );
}

/// A table-over-detail screen, boxed and titled.
fn page<'a>(ws: Workspace, app: &App, lines: Vec<Line<'a>>) -> Paragraph<'a> {
    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(fg(USER))
            .title(titled(ws, app))
            .title_bottom(keys::footer(ws)),
    )
}

fn detail(name: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {name:<10}"), fg(MUTED)),
        Span::styled(value.to_string(), fg(AGENT)),
    ])
}

fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", "─".repeat(width.saturating_sub(4).min(120))),
        fg(MUTED),
    ))
}

/// A run-history strip: seven glyphs that say "healthy / flaky / dead" faster
/// than seven timestamps, with `✗` for failure so colour is never load-bearing.
fn strip_span(history: &[Outcome]) -> Span<'static> {
    let cells: String = history.iter().rev().take(7).collect::<Vec<_>>().into_iter().rev()
        .map(|o| o.cell())
        .collect();
    let failing = history.iter().rev().take(7).any(|o| *o == Outcome::Failed);
    Span::styled(cells, fg(if failing { BAD } else { GOOD }))
}

/// A progress bar in block characters, so it reads without colour.
fn bar(percent: u16, width: usize) -> String {
    let filled = ((percent.min(100) as usize * width) / 100).min(width);
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}

/// A wall clock, for a feed where the date is already the group heading.
fn clock(at_ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(at_ms) {
        Some(t) => t.with_timezone(&chrono::Local).format("%H:%M").to_string(),
        None => "--:--".to_string(),
    }
}

/// Truncate to `width` columns, saying so.
fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    format!("{}…", s.chars().take(width.saturating_sub(1)).collect::<String>())
}

/// A harness as a two- or three-letter code, so the column costs four cells
/// rather than thirteen. The full name is in the detail pane.
fn code(harness: &str) -> &'static str {
    match harness.to_ascii_lowercase().as_str() {
        h if h.starts_with("claude") => "cc",
        h if h.starts_with("open") => "oc",
        h if h.starts_with("agy") || h.starts_with("anti") => "agy",
        _ => "?",
    }
}

fn run_glyph(status: &str) -> &'static str {
    match status {
        "running" => "●",
        "completed" => "✓",
        "failed" => "✗",
        "killed" => "■",
        _ => "○",
    }
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
        format!("{}— scrolled up {} · Esc to follow ", watching, app.scroll)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(MUTED))
        .title(title);

    f.render_widget(
        Paragraph::new(lines).block(block).scroll((offset as u16, 0)),
        area,
    );
    viewport
}

/// One transcript entry as styled lines, already wrapped to `width`.
fn render(entry: &Entry, width: u16) -> Vec<Line<'static>> {
    let (prefix, style, body) = match entry {
        Entry::You(t) => ("› ", bold(USER), t.clone()),
        Entry::Agent(t) => ("", fg(AGENT), t.clone()),
        Entry::Thinking(t) => (
            "  ",
            fg(MUTED).add_modifier(Modifier::ITALIC),
            t.clone(),
        ),
        Entry::Tool {
            name,
            detail,
            failed,
        } => {
            let mark = if *failed { "✗ " } else { "⚙ " };
            let style = fg(if *failed { BAD } else { MUTED });
            let body = match detail {
                Some(d) => format!("{name} · {d}"),
                None => name.clone(),
            };
            (mark, style, body)
        }
        // Indented under its call, so output reads as belonging to the tool
        // above it rather than as the agent speaking.
        Entry::ToolOut { text, failed } => {
            ("  └ ", fg(if *failed { BAD } else { MUTED }), text.clone())
        }
        Entry::Done { text, failed } => {
            let mark = if *failed { "✗ failed" } else { "✓ done" };
            let style = fg(if *failed { BAD } else { GOOD });
            let body = if text.is_empty() {
                mark.to_string()
            } else {
                format!("{mark} · {text}")
            };
            ("", style, body)
        }
        Entry::Notice(t) => ("• ", fg(WARN), t.clone()),
        Entry::Raw(t) => ("", fg(MUTED), t.clone()),
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
    if area.height == 0 {
        return;
    }
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
        .border_style(fg(if app.busy { WARN } else { USER }))
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

fn status_colour(status: &str) -> Color {
    match status {
        "running" => WARN,
        "completed" => GOOD,
        "failed" => BAD,
        _ => MUTED,
    }
}

fn schedule_colour(state: super::data::ScheduleState) -> Color {
    match state {
        super::data::ScheduleState::Armed => GOOD,
        super::data::ScheduleState::Paused => MUTED,
        super::data::ScheduleState::Failing => BAD,
    }
}

fn goal_colour(state: super::data::GoalState) -> Color {
    match state {
        super::data::GoalState::Running => WARN,
        super::data::GoalState::Satisfied => GOOD,
        super::data::GoalState::Waiting => MUTED,
        super::data::GoalState::Blocked => BAD,
        super::data::GoalState::Paused => MUTED,
    }
}

fn hook_colour(state: super::data::HookState) -> Color {
    match state {
        super::data::HookState::Armed => GOOD,
        super::data::HookState::Idle => MUTED,
        super::data::HookState::Failing => BAD,
    }
}

fn task_colour(state: super::data::TaskState) -> Color {
    match state {
        super::data::TaskState::Running => WARN,
        super::data::TaskState::Claimed => WARN,
        super::data::TaskState::Open => MUTED,
        super::data::TaskState::Blocked => BAD,
        super::data::TaskState::Done => GOOD,
    }
}

fn source_colour(source: Source) -> Color {
    match source {
        Source::Run => GOOD,
        Source::Cron => USER,
        Source::Goal => WARN,
        Source::Hook => WARN,
        Source::Memory => MUTED,
    }
}

fn kind_colour(node: &super::data::MemoryNode) -> Color {
    if node.contradicted {
        BAD
    } else {
        USER
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
