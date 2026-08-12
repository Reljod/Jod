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

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use jod_core::team::MemberStatus;
use jod_core::PermissionPolicy;

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

/// The widest a chat column is allowed to get.
///
/// Prose stops being readable somewhere past a hundred columns — the eye loses
/// its place coming back to the left edge — and Jod is run full-screen on
/// 200-column terminals. Tables are the opposite: a workspace wants every
/// column it can get, so the cap is chat's alone.
const MEASURE: u16 = 96;

/// The side gutter. One column reads as a rendering slip; two reads as a margin.
const GUTTER: u16 = 2;

/// The right-hand panel's width, and the narrowest body that can hold it
/// *beside* a chat column rather than on top of one.
const PANEL: u16 = 34;
const PANEL_BESIDE: u16 = 88;

pub fn draw(f: &mut Frame, app: &App) -> usize {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // everything with a box round it
            Constraint::Length(1), // keybar
            Constraint::Length(1), // status
        ])
        .split(f.area());

    // The two bars stay flush with the screen edge: they are chrome, and an
    // inset chrome row reads as content that has lost its border.
    let (body, side) = beside(app, pad(rows[0]));

    // The completion popup is positioned against the input box, which is no
    // longer at a fixed place — the splash moves it — so the rect travels back
    // out rather than being recomputed from the layout.
    let mut input = Rect::new(0, 0, 0, 0);
    let height = if app.workspace == Workspace::Chat {
        let column = measure(body);
        if fresh(app) {
            let (height, box_) = draw_splash(f, app, column);
            input = box_;
            height
        } else {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(column);
            input = parts[1];
            let height = draw_transcript(f, app, parts[0]);
            draw_input(f, app, parts[1]);
            height
        }
    } else {
        draw_workspace(f, app, body);
        // A workspace list pages by its own height, not the transcript's.
        body.height.saturating_sub(4).max(1) as usize
    };

    if let Some(side) = side {
        draw_panel(f, app, side);
    }
    draw_keybar(f, app, rows[1]);
    draw_status(f, app, rows[2]);

    // Last, so they float over everything.
    if app.panel && side.is_none() {
        draw_floating_panel(f, app, body);
    }
    draw_completions(f, app, input);
    draw_overlay(f, app);
    height
}

// ---- layout ------------------------------------------------------------

/// Insets an area by the gutter and by one row at the top.
///
/// Skipped whole on a small terminal: at forty columns the margin costs more
/// than the breathing room buys, and a list that has already dropped three
/// columns to fit should not lose a fourth to whitespace.
fn pad(area: Rect) -> Rect {
    if area.width < 40 || area.height < 6 {
        return area;
    }
    Rect {
        x: area.x + GUTTER,
        y: area.y + 1,
        width: area.width - GUTTER * 2,
        height: area.height - 1,
    }
}

/// Caps a chat column at a readable measure and centres what is left over.
fn measure(area: Rect) -> Rect {
    if area.width <= MEASURE {
        return area;
    }
    Rect {
        x: area.x + (area.width - MEASURE) / 2,
        width: MEASURE,
        ..area
    }
}

/// Splits the panel off the right of the body, or says it will not fit.
///
/// Below `PANEL_BESIDE` there is no honest side-by-side: taking 34 columns off
/// an 80-column terminal leaves a chat column narrower than the panel. The
/// caller floats the panel over the body instead, so Shift-Tab always does
/// something visible rather than appearing broken on a laptop.
fn beside(app: &App, area: Rect) -> (Rect, Option<Rect>) {
    if !app.panel || area.width < PANEL_BESIDE {
        return (area, None);
    }
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(PANEL)])
        .split(area);
    // One column of air between the two boxes, so their borders do not touch.
    let body = Rect {
        width: halves[0].width.saturating_sub(1),
        ..halves[0]
    };
    (body, Some(halves[1]))
}

// ---- the splash --------------------------------------------------------

/// One letter of the wordmark, six columns wide and five rows tall.
///
/// Assembled from per-letter blocks rather than written out as five 37-column
/// string literals: a glyph can then be fixed without recounting the spaces on
/// either side of it, which is how block lettering usually ends up crooked.
type Glyph = [&'static str; 5];

const BIG_J: Glyph = ["    ██", "    ██", "    ██", "██  ██", " ████ "];
const BIG_O: Glyph = ["      ", "      ", " ████ ", "██  ██", " ████ "];
const BIG_D: Glyph = ["    ██", "    ██", " █████", "██  ██", " █████"];
const BIG_SPACE: Glyph = ["  ", "  ", "  ", "  ", "  "];
const BIG_A: Glyph = [" ████ ", "██  ██", "██████", "██  ██", "██  ██"];
const BIG_I: Glyph = ["██████", "  ██  ", "  ██  ", "  ██  ", "██████"];

const WORDMARK: [Glyph; 6] = [BIG_J, BIG_O, BIG_D, BIG_SPACE, BIG_A, BIG_I];

/// How wide the assembled wordmark is: six blocks and five single-column gaps.
const BANNER_WIDTH: u16 = 6 + 1 + 6 + 1 + 6 + 1 + 2 + 1 + 6 + 1 + 6;

fn banner() -> Vec<String> {
    (0..5)
        .map(|row| {
            WORDMARK
                .iter()
                .map(|glyph| glyph[row])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The line under the wordmark, in the longest form that fits.
///
/// It always contains the program's name in the same lowercase the transcript
/// box uses, because on a fresh session that title bar is not on screen and a
/// full-screen program that never says what it is is a program you have to
/// remember you launched.
fn caption(width: usize) -> &'static str {
    const LINES: [&str; 3] = [
        "jod · an orchestrator, not a chat window · Alt-K opens every screen",
        "jod · an orchestrator, not a chat window",
        "jod",
    ];
    LINES
        .into_iter()
        .find(|line| line.chars().count() <= width)
        .unwrap_or("jod")
}

/// Whether this counts as a new session for rendering.
///
/// It cannot be "the transcript is empty": `event_loop` pushes a hint notice at
/// startup and `/new` pushes "new conversation", so the transcript is never
/// literally empty and the splash would never appear at all. A session is new
/// while *nothing but notices* has happened — which is true at startup, true
/// again after `/new`, and false the instant the first prompt is sent. Watching
/// another run is excluded outright: that transcript belongs to somebody else's
/// conversation, and its emptiness says the run has not spoken yet, not that
/// you are starting fresh.
fn fresh(app: &App) -> bool {
    app.watching.is_none()
        && !app
            .transcript
            .iter()
            .any(|entry| !matches!(entry, Entry::Notice(_)))
}

/// The new-session screen: the wordmark, large and centred, with the input box
/// under it. Returns the viewport height and where the input box ended up.
fn draw_splash(f: &mut Frame, app: &App, area: Rect) -> (usize, Rect) {
    // Too short for a wordmark and a box both: the input wins, because a screen
    // with no way to type into it is not a screen.
    if area.height < 6 {
        draw_input(f, app, area);
        return (1, area);
    }

    // The completion popup grows *upwards* out of the input box and the command
    // list is thirty-odd rows, so a vertically centred input leaves it half a
    // screen and the list comes out cut in half. While the popup is open the
    // input drops back to the bottom of the column and the wordmark keeps the
    // space above it: a logo that moves beats a list that is truncated.
    let anchored = !crate::tui::command::completions(&app.input, app).is_empty();

    // Big lettering is the first thing to go. Below its width it would be
    // truncated mid-glyph, which reads as a broken screen rather than a logo.
    let art = area.width >= BANNER_WIDTH && area.height >= 11;
    let mut head: Vec<Line> = if art {
        banner()
            .into_iter()
            .map(|row| Line::from(Span::styled(row, bold(USER))))
            .collect()
    } else {
        vec![Line::from(Span::styled("Jod AI", bold(USER)))]
    };
    if area.height >= head.len() as u16 + 5 {
        head.push(Line::from(""));
        head.push(Line::from(Span::styled(
            caption(area.width as usize),
            fg(MUTED),
        )));
    }
    let head_height = head.len() as u16;

    let (top, box_) = if anchored {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);
        (parts[0], parts[1])
    } else {
        let block = (head_height + 1 + 3).min(area.height);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1), // air between the wordmark and the box
                Constraint::Length(3),
            ])
            .split(Rect {
                y: area.y + area.height.saturating_sub(block) / 2,
                height: block,
                ..area
            });
        (parts[0], parts[2])
    };

    // Centred in whatever vertical space it was given, so the wordmark sits in
    // the middle of the empty screen rather than jammed against the input box.
    let top = Rect {
        y: top.y + top.height.saturating_sub(head_height) / 2,
        height: head_height.min(top.height),
        ..top
    };
    f.render_widget(Paragraph::new(head).alignment(Alignment::Center), top);

    // A full-width input box under a centred wordmark reads as two unrelated
    // screens, so the box is centred on the same axis.
    let box_ = narrow(box_, 72);
    draw_input(f, app, box_);
    (top.height.max(1) as usize, box_)
}

/// At most `width` columns, centred in `area`.
fn narrow(area: Rect, width: u16) -> Rect {
    let width = width.min(area.width);
    Rect {
        x: area.x + (area.width - width) / 2,
        width,
        ..area
    }
}

// ---- the right-hand panel ----------------------------------------------

/// How tall the context box is: two borders, the bar, the count, and two rows
/// for the recommendation.
const CONTEXT_HEIGHT: u16 = 7;

/// Sessions above, context below — the two questions a panel that costs a
/// third of the screen has to be worth answering: what else is running, and how
/// much of the window this conversation has eaten.
fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(CONTEXT_HEIGHT)])
        .split(area);
    draw_sessions(f, app, parts[0]);
    draw_context(f, app, parts[1]);
}

/// The panel when the terminal is too narrow to put it beside anything.
///
/// Still on the right and still inside the body, rather than centred over the
/// whole screen: it is *the right-hand panel* whichever way it is drawn, and a
/// float that covered the keybar would hide the keys while you were looking for
/// them.
fn draw_floating_panel(f: &mut Frame, app: &App, body: Rect) {
    let width = PANEL.min(body.width);
    let area = Rect {
        x: body.x + body.width - width,
        width,
        ..body
    };
    f.render_widget(Clear, area);
    draw_panel(f, app, area);
}

/// Every conversation this process knows about, two rows each.
///
/// Two rather than one because the panel is thirty-odd columns: an id, an age
/// and a name on one row would truncate the name to nothing, and the name is
/// the only part that says what the run was *for*.
fn draw_sessions(f: &mut Frame, app: &App, area: Rect) {
    let inner = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = vec![
        ListItem::new(Line::from(vec![
            Span::styled(" mode    ", fg(MUTED)),
            mode_span(app.mode),
            Span::styled("   Tab cycles", fg(MUTED)),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled(" harness ", fg(MUTED)),
            Span::styled(app.harness.label().to_string(), fg(AGENT)),
        ])),
        // A dash rather than `$0.0000`: four decimal places of nothing is four
        // decimal places of noise on a thirty-column panel.
        ListItem::new(Line::from(Span::styled(
            if app.cost_usd > 0.0 {
                format!(" spend   ${:.4}", app.cost_usd)
            } else {
                " spend   —".to_string()
            },
            fg(MUTED),
        ))),
        ListItem::new(Line::from(Span::styled(
            format!(" {}", "─".repeat(inner.saturating_sub(2))),
            fg(MUTED),
        ))),
    ];

    if app.agents.is_empty() {
        for chunk in wrap("no runs yet — Alt-B delegates one", inner, 1) {
            items.push(ListItem::new(Span::styled(format!(" {chunk}"), fg(MUTED))));
        }
    }
    for a in &app.agents {
        let watched = app.watching.as_deref() == Some(a.id.as_str());
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if watched { " ▸ " } else { "   " }, fg(USER)),
            Span::styled(
                format!("{} ", run_glyph(&a.status)),
                fg(status_colour(&a.status)),
            ),
            Span::styled(format!("{:<9}", short(&a.id)), fg(MUTED)),
            Span::styled(
                super::app::short_duration(app.now_ms.saturating_sub(a.created_at_ms)),
                fg(MUTED),
            ),
        ])));
        items.push(ListItem::new(Span::styled(
            format!("     {}", cut(&a.name, inner.saturating_sub(5))),
            if watched { bold(USER) } else { fg(AGENT) },
        )));
    }

    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" sessions ")
                .title_bottom(" Shift-Tab closes "),
        ),
        area,
    );
}

/// How full the context window is, as a bar.
///
/// Every number here is hedged with `≈` and the box says so twice, because
/// `CONTEXT_WINDOW` is one assumed figure for every model and `context_tokens`
/// is the last turn's input as the harness reported it. The question the box
/// answers is "am I near the point where I should compact", and a precise-
/// looking percentage would answer a different question dishonestly.
fn draw_context(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let inner = area.width.saturating_sub(2) as usize;
    let percent = (app.context_fraction() * 100.0).round() as u16;
    let width = inner.saturating_sub(9).clamp(4, 22);
    let colour = if app.should_compact() { WARN } else { GOOD };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!(" {}", bar(percent, width)), fg(colour)),
            Span::styled(format!(" ≈{percent}%"), fg(AGENT)),
        ]),
        Line::from(Span::styled(
            format!(
                " ≈{} of an assumed {}",
                tokens(app.context_tokens),
                tokens(super::app::CONTEXT_WINDOW)
            ),
            fg(MUTED),
        )),
        Line::from(""),
    ];
    // `⚠` as well as the colour, so the advice survives NO_COLOR — it is the
    // one line in this box that asks you to do something.
    if app.should_compact() {
        lines.push(Line::from(Span::styled(
            " ⚠ compact recommended",
            bold(WARN),
        )));
    } else {
        lines.push(Line::from(Span::styled(" room to keep going", fg(MUTED))));
    }

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(if app.should_compact() { WARN } else { MUTED }))
                .title(" context ")
                .title_bottom(" estimated, not measured "),
        ),
        area,
    );
}

/// Tokens as a short human number — `104k` — because the exact figure is not
/// knowable and printing it to the unit would claim that it is.
fn tokens(n: u64) -> String {
    if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
}

// ---- the permission mode -----------------------------------------------

/// The mode as a glyph and a word, coloured by how much it lets through.
fn mode_span(mode: PermissionPolicy) -> Span<'static> {
    Span::styled(
        format!("{} {}", mode_glyph(mode), mode.label()),
        bold(mode_colour(mode)),
    )
}

/// A circle that fills as the mode lets more through, so the four are ordered
/// by shape and not only by hue. `may_act` draws the one line that matters:
/// plan is the only mode that cannot change anything, and it gets the hollow
/// glyph.
fn mode_glyph(mode: PermissionPolicy) -> &'static str {
    if !mode.may_act() {
        return "○";
    }
    match mode {
        PermissionPolicy::Ask => "◔",
        PermissionPolicy::AcceptEdits => "◑",
        _ => "●",
    }
}

/// Green for the mode that cannot break anything, red for the one that
/// approves everything unattended — the word is always printed beside it, so
/// the colour is emphasis rather than the message.
fn mode_colour(mode: PermissionPolicy) -> Color {
    match mode {
        PermissionPolicy::Plan => GOOD,
        PermissionPolicy::Ask => USER,
        PermissionPolicy::AcceptEdits => WARN,
        PermissionPolicy::Bypass => BAD,
    }
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
        // The leader's own spelling comes from `keys`, not from a literal here:
        // these two lines named the chord in prose, so the drift net could not
        // see them and they went on teaching `Ctrl-K` after the keymap moved.
        Overlay::WhichKey => (keys::which_key_hint(false), "Esc cancels"),
        Overlay::WhichKeyNew => (keys::which_key_hint(true), "Esc cancels"),
        Overlay::Keymap => ("the keymap — any key closes it".to_string(), "Esc closes"),
        Overlay::Confirm { .. } => (
            "y confirms · anything else cancels".to_string(),
            "Esc cancels",
        ),
        Overlay::Prompt { .. } => ("⏎ accepts · Esc cancels".to_string(), "Esc cancels"),
        Overlay::None if app.here().editing_filter => (
            "typing filters this list".to_string(),
            "⏎ keeps it · Esc clears it",
        ),
        Overlay::None => (keys::keybar(ws, area.width), keys::keybar_exit(ws)),
    };
    f.render_widget(
        Paragraph::new(two_ends(&left, right, area.width, MUTED)),
        area,
    );
}

/// The status bar: where you are on the left, what is waiting on the right.
///
/// The permission mode leads it on every screen. What the next turn may do
/// without asking changes while you are talking, and a setting you have to
/// press a key to see is one you will be wrong about exactly when it matters.
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
    let mut badge = String::new();
    // The panel holds the context bar, but the panel is shut most of the time
    // and advice nobody can see is not advice — so the recommendation itself
    // rides the one row that is always on screen.
    if app.should_compact() {
        badge.push_str("⚠ compact");
    }
    // Endings that arrive while you are away have to survive until you look.
    if app.unread() > 0 {
        if !badge.is_empty() {
            badge.push_str(" · ");
        }
        badge.push_str(&format!("⚑ {} unread", app.unread()));
    }
    let style = if app.busy && ws == Workspace::Chat {
        fg(WARN)
    } else {
        fg(MUTED)
    };
    let mode = mode_span(app.mode);
    let mode_width = mode.content.chars().count();
    let mut spans = vec![
        Span::raw(" "),
        mode,
        Span::styled(" · ", fg(MUTED)),
        Span::styled(left.clone(), style),
    ];
    // The status grows — a spinner, a clock, a background count, a queue — so
    // the badge has to yield rather than collide with it. Running them together
    // produced `1 queuedAlt-X stop`, which reads as neither.
    let used = mode_width + 3 + left.chars().count() + 2;
    let room = (area.width as usize).saturating_sub(used);
    if !badge.is_empty() && room >= badge.chars().count() + 2 {
        spans.push(Span::raw(" ".repeat(room - badge.chars().count())));
        spans.push(Span::styled(badge, fg(WARN)));
    }
    spans.push(Span::raw(" "));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Two strings on one row: **the right one is reserved first** and the left
/// gets what is left over.
///
/// Regression, and the argument order was the whole bug. This used to measure
/// the left half and drop the right half whole when the rest would not fit —
/// so at 80 columns, an entirely ordinary terminal, Chat, Fleet and Memory
/// printed their verbs and stopped saying how to leave. `keys.rs` states the
/// condition it broke: a screen whose way out is not printed is a trap rather
/// than a shortcut.
///
/// The invariant is therefore the exit, and the verb list is best-effort. A
/// screen showing three of its five verbs is merely terse; a screen with no way
/// out is a screen you have to kill the terminal to leave. The left half is
/// elided rather than truncated, because `keys::keybar` budgets its own text
/// and a half-word cut mid-chord teaches a key that does not exist.
///
/// **`keys::verb_budget` mirrors the arithmetic below** — `width - right - 3`,
/// one space of margin at each end and one between the halves. Two files have
/// to agree on those three columns: widen the padding here and the keybar hands
/// back a string this function then elides *whole*, so a screen loses all its
/// verbs rather than one.
///
/// `two_ends_accepts_a_left_half_of_exactly_the_budgeted_width` is what fails
/// if they drift: it asks `keys::verb_budget` for the number and builds a left
/// half of exactly that width. Rendering a real keybar does not catch it —
/// `keys::keybar` drops whole verbs, so it almost always lands a few columns
/// under its budget and quietly absorbs the disagreement until the one screen
/// whose verbs happen to end on the boundary.
fn two_ends(left: &str, right: &str, width: u16, colour: Color) -> Line<'static> {
    let width = width as usize;
    let left_len = left.chars().count();
    let right_len = right.chars().count();

    // One space of margin at each end; at least one more between the halves, so
    // they can never run together — `1 queuedAlt-X stop` reads as neither.
    let show_right = right_len + 2 <= width;
    let room_for_left = if show_right {
        width.saturating_sub(right_len + 3)
    } else {
        width.saturating_sub(2)
    };
    let show_left = left_len <= room_for_left;

    let mut spans = vec![Span::raw(" ")];
    if show_left {
        spans.push(Span::styled(left.to_string(), fg(colour)));
    }
    if show_right {
        let used = 1 + if show_left { left_len } else { 0 };
        let gap = width.saturating_sub(used + right_len + 1);
        spans.push(Span::raw(" ".repeat(gap)));
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

    let widest = rows
        .iter()
        .map(|(_, t)| t.chars().count())
        .max()
        .unwrap_or(0);
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

    let title = keys::which_key_title(making);
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
///
/// **It spills into columns rather than off the bottom.** Fleet's keymap is 46
/// rows; `centred` clamps a panel to the terminal, so at 80×30 it drew the
/// first 28 and dropped the rest — the whole `anywhere` section — with nothing
/// on screen to say so. The screen's own verbs survived only because
/// `keys::keymap` happens to put them first, which made a discoverability
/// preference silently load-bearing for correctness. It is a preference again:
/// the layout below shows everything that fits in as many columns as the window
/// affords, and *counts what it still cannot show* rather than dropping it
/// quietly. Help that lies about being complete is worse than no help, because
/// you stop looking.
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
    let width_of = |line: &Line| {
        line.spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
    };
    let widest = lines.iter().map(width_of).max().unwrap_or(20);

    let screen = f.area();
    // Two border cells vertically, two horizontally, and two of margin between
    // a column and the next.
    let rows = screen.height.saturating_sub(2).max(1) as usize;
    let column = widest + 2;
    let wanted = lines.len().div_ceil(rows);
    let affordable = ((screen.width.saturating_sub(2)) as usize / column.max(1)).max(1);
    let columns = wanted.min(affordable);
    let shown = (columns * rows).min(lines.len());
    let hidden = lines.len() - shown;

    // Read down each column and then across, the way a keymap is scanned.
    let tall = rows.min(shown);
    let mut composed: Vec<Line> = Vec::with_capacity(tall);
    for row in 0..tall {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..columns {
            let at = col * rows + row;
            if at >= shown {
                break;
            }
            let used = width_of(&lines[at]);
            spans.extend(lines[at].spans.iter().cloned());
            if (col + 1) * rows + row < shown {
                spans.push(Span::raw(" ".repeat(column.saturating_sub(used))));
            }
        }
        composed.push(Line::from(spans));
    }

    let panel = centred(
        screen,
        (columns * column + 2) as u16,
        (composed.len() + 2) as u16,
    );
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(composed).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(" keys ")
                .title_bottom(if hidden > 0 {
                    format!(" {hidden} more — widen the window ")
                } else {
                    " any key closes this ".to_string()
                }),
        ),
        panel,
    );
}

/// A destructive verb on a bare letter is one fat-fingered `Alt-K h x` away
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
        // Rows, less the ones a filter cannot exclude. The fleet's pinned chat
        // is always in `row_ids` and never a match, so counting rows there
        // claimed one more hit than the filter had actually found.
        let unfilterable = usize::from(app.workspace == Workspace::Fleet);
        format!(
            "   ▸ filter · {} match",
            app.row_ids(app.workspace)
                .len()
                .saturating_sub(unfilterable)
        )
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

/// The master half of a split screen.
///
/// Its title is the screen's name alone: the master pane is 48 cells at the
/// design width, so a counted title would be truncated mid-word — and the
/// status bar already carries the count on every screen.
fn body<'a>(ws: Workspace, items: Vec<ListItem<'a>>, width: u16) -> List<'a> {
    List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(fg(USER))
            .title(format!(" {} ", ws.title()))
            .title_bottom(fit_verbs(&keys::footer(ws), width)),
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
    let selected = app
        .list(Workspace::Fleet)
        .index(&app.row_ids(Workspace::Fleet));
    // One longer than the agents: the pinned chat holds index 0, which is why
    // `row_ids` puts it there too. The two orderings have to agree, or the
    // cursor lands one row off its own highlight.
    let (first, height) = window(left, selected, rows.len() + 1);
    // Declared drop order: detail pane → harness → id, name last.
    //
    // Each threshold is one higher than it was, because the delivery gutter
    // took a cell from every row permanently. Left alone, a pane of exactly the
    // old width would spend the whole line on fixed columns and leave the name
    // nothing — the column that says what the run was *for*.
    let inner = left.width.saturating_sub(2) as usize;
    let show_id = inner >= 35;
    let show_harness = inner >= 31;

    // Index 0 is the pinned chat and the rest are agents, so the loop walks
    // positions rather than the agent vector. Everything below the first row
    // reads its agent at `i - 1`.
    let mut items: Vec<ListItem> = Vec::new();
    for i in first..(first + height).min(rows.len() + 1) {
        let chosen = i == selected;
        if i == 0 {
            items.push(ListItem::new(Line::from(main_line(
                app,
                chosen,
                inner,
                show_id,
                show_harness,
            ))));
            continue;
        }
        let a = rows[i - 1];
        let age = super::app::short_duration(app.now_ms.saturating_sub(a.created_at_ms));
        let watched = app.watching.as_deref() == Some(a.id.as_str());
        let mut spans = vec![
            delivery_gutter(a.delivery),
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
        // The name was clipped by the widget with nothing saying so —
        // `port the p` at the design width. It is cut with an ellipsis
        // now, and the marker beside it is reserved first.
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let marker = if watched { "  \u{2190} on screen" } else { "" };
        let (name, marked) = fit_row(used, &a.name, marker, inner);
        spans.push(Span::styled(
            name,
            if chosen { bold(USER) } else { fg(AGENT) },
        ));
        if marked {
            spans.push(Span::styled(marker.to_string(), fg(USER)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    // Said under the pinned row rather than instead of the list, because the
    // list is no longer empty — the chat is always in it, and "no agents yet"
    // as the only line would now be a claim the row above it contradicts.
    if rows.is_empty() {
        // Short enough to survive the master pane at the design width, which is
        // 44 cells inside its border — the longer form was clipped mid-word.
        items.extend(empty("  nothing delegated yet — Alt-B starts one"));
    }
    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }
    f.render_widget(body(Workspace::Fleet, items, left.width), left);

    let Some(right) = right else { return };
    // Its own pane, and its own footer: none of `s stop · r resume · a attach`
    // means anything to a conversation, and offering keys that quietly do
    // nothing is how a list teaches people not to trust its footer.
    if app.main_selected() {
        f.render_widget(
            Paragraph::new(main_detail(app, right.width)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(USER))
                    .title(" the chat ")
                    .title_bottom(fit_verbs(" ⏎ open ", right.width)),
            ),
            right,
        );
        return;
    }
    let lines = match app.selected_agent() {
        None => vec![Line::from(Span::styled(" nothing selected", fg(MUTED)))],
        Some(a) => {
            let mut lines = vec![
                Line::from(Span::styled(format!(" {}", a.name), bold(AGENT))),
                Line::from(Span::styled(format!(" {}", a.id), fg(MUTED))),
                Line::from(""),
                field("harness", &a.harness),
                field(
                    "status",
                    // The master column is 48 cells at the design width, so
                    // the inline `← on screen` marker is the first thing
                    // *dropped* — whole, by `fit_row`, never clipped to
                    // `← on scr`. This pane is where it is always said, which
                    // is why dropping it there costs nothing above 90 columns.
                    &if app.watching.as_deref() == Some(a.id.as_str()) {
                        format!("{} · on screen", a.status)
                    } else {
                        a.status.clone()
                    },
                ),
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
                    lines.push(Line::from(Span::styled(" last", fg(MUTED))));
                    for chunk in wrap(text, right.width.saturating_sub(4) as usize, 2) {
                        lines.push(Line::from(Span::styled(format!("   {chunk}"), fg(AGENT))));
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
                .title_bottom(fit_verbs(
                    " ⏎ watch · s stop · r resume · a attach ",
                    right.width,
                )),
        ),
        right,
    );
}

/// The pinned chat's row, in the fleet's own columns.
///
/// It borrows the agent columns rather than inventing a layout, because two
/// column schemes in one list is two things to read. The id column carries the
/// word `pinned` — the row stands for a conversation and has no run id to show
/// — and the age is time since the last instruction, which is the number you
/// want: how long since you last said anything.
fn main_line(
    app: &App,
    chosen: bool,
    inner: usize,
    show_id: bool,
    show_harness: bool,
) -> Vec<Span<'static>> {
    let row = app.main_row();
    let mut spans = vec![
        // No delivery gutter: a conversation owes nobody a reply. A blank keeps
        // the columns under it aligned.
        Span::raw(" "),
        Span::styled(if chosen { "▸ " } else { "  " }, fg(USER)),
        Span::styled("★ ", fg(USER)),
    ];
    if show_id {
        spans.push(Span::styled(format!("{:<9}", "pinned"), fg(MUTED)));
    }
    spans.push(Span::styled(
        format!("{:<9}", row.status),
        fg(if row.is_running() { WARN } else { MUTED }),
    ));
    // A chat nothing has been said to has no age, and `0s` would be a claim
    // that something just happened.
    let age = match row.last_ms {
        0 => "—".to_string(),
        at => super::app::short_duration(app.now_ms.saturating_sub(at)),
    };
    spans.push(Span::styled(format!("{age:>7} "), fg(MUTED)));
    if show_harness {
        spans.push(Span::styled(
            format!("{:<4}", code(&row.harness)),
            fg(MUTED),
        ));
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let marker = "  ⏎ open";
    let (name, marked) = fit_row(used, "main", marker, inner);
    spans.push(Span::styled(name, bold(USER)));
    if marked {
        spans.push(Span::styled(marker.to_string(), fg(USER)));
    }
    spans
}

/// The pinned chat's detail pane.
///
/// Says the three things the row cannot: that this is where typing goes, how
/// much of the window is spoken for, and how many instructions it has routed.
fn main_detail(app: &App, width: u16) -> Vec<Line<'static>> {
    let row = app.main_row();
    let mut lines = vec![
        Line::from(Span::styled(" main", bold(USER))),
        // Short enough to survive the pane at the design width — 48 cells —
        // rather than being clipped mid-sentence.
        Line::from(Span::styled(
            " one conversation, and it never ends",
            fg(MUTED),
        )),
        Line::from(""),
        field("harness", &row.harness),
        field("status", &row.status),
        // "last said" is exactly the width `field` pads its label to, which
        // left it touching its own value — `last saidnothing yet`.
        field(
            "last",
            &match row.last_ms {
                0 => "nothing yet".to_string(),
                at => format!(
                    "{} ago",
                    super::app::short_duration(app.now_ms.saturating_sub(at))
                ),
            },
        ),
        field(
            "turns",
            &match row.turns {
                0 => "none yet".to_string(),
                1 => "1 instruction routed".to_string(),
                n => format!("{n} instructions routed"),
            },
        ),
        Line::from(""),
    ];
    for chunk in wrap(
        "Everything you type goes here. It never does the work itself — it \
         delegates, continues an agent that already has the context, arms a \
         schedule, or sets a goal, and the agents below are what came of that.",
        width.saturating_sub(4) as usize,
        2,
    ) {
        lines.push(Line::from(Span::styled(format!("  {chunk}"), fg(MUTED))));
    }
    lines
}

/// A row's variable text and its trailing marker, fitted together.
///
/// **The marker is the invariant; the text is best-effort.** This is the
/// `two_ends` ruling one level down. Markers used to be pushed and left for the
/// widget to clip — `← on scr` at eighty columns, a bare `← ` at 150, `← whe`
/// on the graph trail, and worst `← n` for `← needs you`. Dropping them whole
/// fixed the lie but kept the priority backwards: `← needs you` then vanished
/// exactly when a line ran long, which is precisely when something worth
/// saying had happened. A marker whose entire job is to say a person is
/// required cannot be the half that yields.
///
/// So the marker's width is reserved first, and the text is `cut` to what
/// remains. `cut` rather than a silent clip because it *says* it cut: content
/// that shrinks visibly is honest, where a marker that disappears tells you
/// nothing. That asymmetry is the whole reason the two halves are treated
/// differently.
///
/// The one exception is a row too narrow to hold both: below `LEAST_TEXT` the
/// marker is dropped instead, because a row that is all marker and no name has
/// stopped saying *which* run it is talking about, and the marker's meaning
/// depends on knowing that.
fn fit_row(used: usize, text: &str, marker: &str, room: usize) -> (String, bool) {
    let for_text = room.saturating_sub(used);
    let marker_len = marker.chars().count();
    if !marker.is_empty() && marker_len + LEAST_TEXT <= for_text {
        (cut(text, for_text - marker_len), true)
    } else {
        (cut(text, for_text), false)
    }
}

/// The narrowest a name or a line of feed text may be squeezed before the
/// marker beside it gives way instead.
const LEAST_TEXT: usize = 12;

/// The delivery gutter: one cell at the far left of a fleet row, saying that
/// the reply this run owed somebody never arrived, or may have arrived twice.
///
/// **In the gutter, and blank on almost every row.** A run started from the TUI
/// reports into the transcript you are already reading and owes nobody
/// anything, so `Verdict::Nothing` is the common case and gets a space — the
/// same argument that keeps the compaction hint quiet until it is worth having.
/// A marker on every row is a marker nobody reads, and this one has to survive
/// being the only thing on the screen that is wrong.
///
/// Left gutter rather than appended after the name, for two reasons the row
/// itself proves. There is no room at the end: at the design width the master
/// pane gives the name ten cells and is already cutting it. And a fixed column
/// at the start is where a scan begins — the precedent is the activity feed's
/// unread dot, four hundred lines below.
///
/// The glyph comes from `Verdict`, never from a match written here: a second
/// surface inventing its own marks is how two screens come to disagree about
/// what `✗` means. `marks_a_row` decides *whether* to draw, and it is narrower
/// than `is_trouble` on purpose — a reply still in flight is not yet news.
fn delivery_gutter(verdict: super::delivery::Verdict) -> Span<'static> {
    if !verdict.marks_a_row() {
        return Span::raw(" ");
    }
    Span::styled(
        verdict.glyph(),
        // Red for a message nobody got, amber for one somebody may hold twice:
        // the first is a loss, the second is a mess. The glyphs differ too, so
        // `NO_COLOR` loses which *kind* of trouble it is, never that there is
        // trouble.
        bold(match verdict {
            super::delivery::Verdict::Lost => BAD,
            _ => WARN,
        }),
    )
}

/// One `name  value` row of a detail pane, indented off the border — text
/// flush against a box edge reads as a rendering bug even when it is not.
fn field(name: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {name:<9}"), fg(MUTED)),
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
    f.render_widget(body(Workspace::Memory, items, left.width), left);

    let Some(right) = right else { return };
    let lines = match app.selected_memory() {
        None => vec![Line::from(Span::styled(" nothing selected", fg(MUTED)))],
        Some(n) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(format!(" {}", n.name), bold(AGENT)),
                    Span::styled(format!("   {}", n.kind.label()), fg(MUTED)),
                ]),
                Line::from(Span::styled(
                    format!(
                        " conf {:.2} · {} edges · seen {}×",
                        n.confidence, n.degree, n.seen
                    ),
                    fg(MUTED),
                )),
                Line::from(""),
            ];
            for chunk in wrap(&n.body, right.width.saturating_sub(4) as usize, 0) {
                lines.push(Line::from(Span::styled(format!(" {chunk}"), fg(AGENT))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" ▲ linked from ({})", n.in_edges.len()),
                fg(MUTED),
            )));
            for edge in &n.in_edges {
                lines.push(edge_line(edge));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" ▼ links to ({})", n.out_edges.len()),
                fg(MUTED),
            )));
            for edge in &n.out_edges {
                lines.push(edge_line(edge));
            }
            if !n.provenance.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(" provenance", fg(MUTED))));
                for source in &n.provenance {
                    lines.push(Line::from(Span::styled(format!("   {source}"), fg(MUTED))));
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
                .title_bottom(fit_verbs(
                    " g local graph · e edit · x forget ",
                    right.width,
                )),
        ),
        right,
    );
}

fn edge_line(edge: &super::data::MemoryEdge) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("   {:<14}", edge.kind), fg(MUTED)),
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
    let (trail, marked) = fit_row(
        3,
        &app.graph.trail_line(),
        "   \u{2190} where you have been",
        area.width.saturating_sub(2) as usize,
    );
    let mut trail_spans = vec![Span::styled(format!("   {trail}"), fg(MUTED))];
    if marked {
        trail_spans.push(Span::styled("   \u{2190} where you have been", fg(MUTED)));
    }
    lines.push(Line::from(trail_spans));

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
            Span::styled(
                format!("{} ", s.state.glyph()),
                fg(schedule_colour(s.state)),
            ),
            Span::styled(
                format!("{:<17}", cut(&s.name, 17)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
        ];
        if show_gloss {
            spans.push(Span::styled(
                format!("{:<20}", cut(&s.gloss, 19)),
                fg(AGENT),
            ));
        }
        spans.push(Span::styled(
            format!(
                "{:<14}",
                s.next_ms.map(absolute).unwrap_or_else(|| "— paused".into())
            ),
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
                    fg(if run.outcome == Outcome::Failed {
                        BAD
                    } else {
                        MUTED
                    }),
                )));
            }
        }
    }

    f.render_widget(page(Workspace::Schedules, app, lines, area.width), area);
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
            spans.push(Span::styled(
                format!("{:<12}", cut(&g.cadence, 11)),
                fg(MUTED),
            ));
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
        lines.push(Line::from(Span::styled(
            format!("  {}", g.name),
            bold(AGENT),
        )));
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
                    check
                        .note
                        .as_ref()
                        .map(|n| format!("   ← {n}"))
                        .unwrap_or_default(),
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
                    fg(if it.outcome == Outcome::Failed {
                        BAD
                    } else {
                        MUTED
                    }),
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

    f.render_widget(page(Workspace::Goals, app, lines, area.width), area);
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
        spans.push(Span::styled(
            format!("{:<28}", cut(&h.event, 27)),
            fg(AGENT),
        ));
        spans.push(Span::styled(format!("{:<12}", cut(&h.runs, 11)), fg(MUTED)));
        if show_24h {
            spans.push(Span::styled(
                format!(
                    "{:>4}  {:>6} ",
                    h.deliveries_24h,
                    since(app.now_ms, h.last_ms)
                ),
                fg(MUTED),
            ));
            spans.push(Span::styled(
                h.last_outcome.mark(),
                fg(if h.last_outcome == Outcome::Failed {
                    BAD
                } else {
                    GOOD
                }),
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

    f.render_widget(page(Workspace::Hooks, app, lines, area.width), area);
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
                format!(
                    "{:<10}",
                    t.run.as_deref().map(short).unwrap_or_else(|| "—".into())
                ),
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
                format!(
                    "   {} · {}",
                    t.state.label(),
                    t.owner.clone().unwrap_or_else(|| "unclaimed".into())
                ),
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

    f.render_widget(page(Workspace::Tasks, app, lines, area.width), area);
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
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let marker = if item.needs_you {
            "  \u{2190} needs you"
        } else {
            ""
        };
        let (text, marked) = fit_row(used, &item.text, marker, width);
        spans.push(Span::styled(
            text,
            if chosen { bold(AGENT) } else { fg(AGENT) },
        ));
        if marked {
            spans.push(Span::styled(marker.to_string(), bold(WARN)));
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

    f.render_widget(page(Workspace::Activity, app, lines, area.width), area);
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
                    t.owner
                        .as_ref()
                        .map(|o| format!("  ({o})"))
                        .unwrap_or_default(),
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
fn page<'a>(ws: Workspace, app: &App, lines: Vec<Line<'a>>, width: u16) -> Paragraph<'a> {
    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(fg(USER))
            .title(titled(ws, app))
            .title_bottom(fit_verbs(&keys::footer(ws), width)),
    )
}

/// A `·`-joined verb list, cut back to **whole verbs** so it fits inside a box
/// `width` cells wide.
///
/// Regression, same family as the keybar's and worse for being invisible at the
/// design size. `keys::footer(Fleet)` is 58 cells and the master pane is 46 at
/// 100 columns, so the fleet's bottom border read `… r resume · d d` — `d d` is
/// a key that does not exist. It was fine at 80, where the pane is full width
/// because `split` has not engaged, and fine again at 150 where the pane is
/// wide enough; the broken band is roughly 92 to 140, which brackets the
/// stated design width. A clipped title is cosmetic. A clipped *keymap* teaches
/// a chord nobody can press.
///
/// No `? more` marker here, unlike `keys::keybar`. The footer is by
/// construction a repeat of the first few verbs already printed on the bar two
/// rows below, so nothing it drops is only taught here, and a second marker two
/// rows from the first is noise. That is also why the fitting lives in `ui`
/// rather than in `keys`: with no marker to append this is only "make text fit
/// a box", which is a rendering concern and not a keymap one.
fn fit_verbs(text: &str, width: u16) -> String {
    // The two border cells the title is drawn between.
    let room = (width as usize).saturating_sub(2);
    if text.chars().count() <= room {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    for verb in text.trim().split(" · ") {
        let mut next = kept.clone();
        next.push(verb);
        // Measured with the padding put back, because the padding is what
        // actually occupies the border — measuring the join alone lets the two
        // spaces overflow by exactly the amount that looks like a rendering bug.
        if format!(" {} ", next.join(" · ")).chars().count() > room {
            break;
        }
        kept = next;
    }
    if kept.is_empty() {
        return String::new();
    }
    format!(" {} ", kept.join(" · "))
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
    let cells: String = history
        .iter()
        .rev()
        .take(7)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
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
    format!(
        "{}…",
        s.chars().take(width.saturating_sub(1)).collect::<String>()
    )
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

pub(super) fn run_glyph(status: &str) -> &'static str {
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
        Entry::You(t) => ("› ", bold(USER), t.clone()),
        Entry::Agent(t) => ("", fg(AGENT), t.clone()),
        Entry::Thinking(t) => ("  ", fg(MUTED).add_modifier(Modifier::ITALIC), t.clone()),
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

/// The composer's caret. A bordered box with nothing in it looks like a box;
/// a caret is the one column that says the keystrokes land *here*, and unlike
/// the placeholder it keeps saying so once the line is full of text.
const CARET: &str = "› ";

/// What the empty composer says, in the longest form that fits — same shape as
/// [`caption`], and for the same reason: a blank field tells a first-time user
/// nothing about what this program wants from them. It names the two ways in
/// (prose, or `/`) and gets out of the way at the first keystroke. It stops
/// there: the splash caption above it and the status bar below it already
/// teach Alt-K, and a third copy is noise rather than help. Empty when even the
/// shortest form would be truncated, since half a sentence reads as a rendering
/// bug rather than a hint.
fn placeholder(width: usize) -> &'static str {
    const LINES: [&str; 2] = [
        "tell Jod what to do · / for commands",
        "tell Jod what to do",
    ];
    LINES
        .into_iter()
        .find(|line| line.chars().count() <= width)
        .unwrap_or("")
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
    // The caret costs two columns, which a box this narrow does not have to
    // spare: on it the text wins and the caret goes.
    let caret = if inner_width >= CARET.chars().count() + 8 {
        CARET
    } else {
        ""
    };
    let gutter = caret.chars().count();
    let field = inner_width.saturating_sub(gutter).max(1);

    // Keep the cursor on screen by scrolling the field horizontally once the
    // line outgrows the box.
    let col = app.cursor_column();
    let shift = col.saturating_sub(field.saturating_sub(1));
    let visible: String = app.input.chars().skip(shift).take(field).collect();

    // Muted while the field is empty so the caret and the hint read as one
    // piece of furniture; live the moment there is something to send.
    let line = if app.input.is_empty() {
        Line::from(vec![
            Span::styled(caret, fg(MUTED)),
            Span::styled(placeholder(field), fg(MUTED)),
        ])
    } else {
        Line::from(vec![
            Span::styled(caret, fg(if app.busy { WARN } else { USER })),
            Span::raw(visible),
        ])
    };

    f.render_widget(Paragraph::new(line).block(block), area);
    f.set_cursor_position((area.x + 1 + (gutter + col - shift) as u16, area.y + 1));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::{
        ActivityItem, Check, Delivery, GoalRow, GoalState, HookRow, HookState, Iteration,
        MemoryEdge, MemoryKind, MemoryNode, PastRun, ScheduleRow, ScheduleState,
    };
    use crate::tui::delivery::Verdict;
    use crate::tui::graph::GraphView;
    use crate::tui::PromptIntent;
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

    fn agent_line(id: &str, name: &str, status: &str) -> super::super::AgentLine {
        super::super::AgentLine {
            delivery: crate::tui::delivery::Verdict::Nothing,
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

    // ---- fixtures for the workspaces whose loaders are still empty ----

    fn node(name: &str, kind: MemoryKind, degree: usize) -> MemoryNode {
        MemoryNode {
            id: name.into(),
            name: name.into(),
            kind,
            confidence: 0.86,
            degree,
            age_ms: 0,
            seen: 23,
            body: "Non-trivial work starts with a spec, not a plan.".into(),
            contradicted: false,
            in_edges: vec![],
            out_edges: vec![],
            provenance: vec!["AGENTS.md §How work runs".into()],
        }
    }

    fn edge(kind: &str, other: &str) -> MemoryEdge {
        MemoryEdge {
            kind: kind.into(),
            other: other.into(),
            other_name: other.into(),
            other_kind: MemoryKind::Belief,
            warn: kind == "contradicts",
        }
    }

    fn memory_app() -> App {
        let mut a = app();
        let mut focus = node("prefers-spec-first", MemoryKind::Belief, 17);
        focus.in_edges = vec![edge("supports", "linear-is-truth")];
        focus.out_edges = vec![edge("contradicts", "ship-fast-iterate")];
        a.memory = vec![
            focus,
            node("linear-is-truth", MemoryKind::Belief, 9),
            node("ship-fast-iterate", MemoryKind::Fact, 2),
        ];
        a.go(Workspace::Memory);
        a
    }

    fn schedule(name: &str, state: ScheduleState) -> ScheduleRow {
        ScheduleRow {
            name: name.into(),
            gloss: "02:00 every day".into(),
            cron: "0 2 * * *".into(),
            timezone: "Asia/Manila".into(),
            next_ms: Some(9_000_000),
            last_ms: Some(-3_000_000),
            state,
            history: vec![
                Outcome::Ok,
                Outcome::Ok,
                Outcome::Failed,
                Outcome::Ok,
                Outcome::Ok,
                Outcome::Ok,
                Outcome::Ok,
            ],
            prompt: "Triage the Linear inbox.".into(),
            runs_as: "Claude Code · ~/repo/Jod".into(),
            policy: "overlap: skip · timeout 20m".into(),
            recent: vec![PastRun {
                at_ms: -3_000_000,
                outcome: Outcome::Ok,
                duration_ms: 258_000,
                cost_usd: 0.44,
                note: "3 items triaged".into(),
            }],
        }
    }

    fn schedules_app() -> App {
        let mut a = app();
        a.schedules = vec![
            schedule("nightly-inbox", ScheduleState::Armed),
            schedule("deps-audit", ScheduleState::Paused),
        ];
        a.schedules[1].next_ms = None;
        a.go(Workspace::Schedules);
        a
    }

    fn goals_app() -> App {
        let mut a = app();
        a.goals = vec![GoalRow {
            name: "inbox-to-zero".into(),
            cadence: "hourly".into(),
            last_ms: Some(-2_000_000),
            next_ms: Some(1_080_000),
            state: GoalState::Running,
            iteration: 118,
            objective: "Keep the Linear inbox at zero.".into(),
            checks: vec![
                Check {
                    done: true,
                    text: "no item older than 48h".into(),
                    note: None,
                },
                Check {
                    done: false,
                    text: "no item blocked without a reason".into(),
                    note: Some("3 items fail this".into()),
                },
            ],
            stop_if: "budget $25/week spent".into(),
            spent_usd: 11.40,
            budget_usd: 25.0,
            iterations: vec![Iteration {
                n: 118,
                at_ms: -2_000_000,
                note: "+4 items closed".into(),
                duration_ms: 311_000,
                cost_usd: 0.38,
                outcome: Outcome::Ok,
            }],
            escalation: Some("ENG-441 needs a decision from you".into()),
        }];
        a.go(Workspace::Goals);
        a
    }

    fn hooks_app() -> App {
        let mut a = app();
        a.hooks = vec![HookRow {
            name: "pr-opened".into(),
            repo: "Reljod/Jod".into(),
            event: "pull_request.opened".into(),
            runs: "review-pr".into(),
            deliveries_24h: 18,
            last_ms: Some(-120_000),
            last_outcome: Outcome::Ok,
            state: HookState::Armed,
            endpoint: "https://jod.reljod.dev/hooks/gh/pr-opened".into(),
            secret: "✓ verified 2m ago".into(),
            match_rule: "event = pull_request · action = opened".into(),
            runs_as: "review-pr   Claude Code".into(),
            prompt: "Review PR #{{number}} against REVIEW.md.".into(),
            policy: "dedupe by delivery id · retry 3×".into(),
            created: "2026-07-14".into(),
            total: 214,
            deliveries: vec![Delivery {
                at_ms: -120_000,
                id: "8f2a1c".into(),
                what: "PR #212 port the parser".into(),
                accepted: true,
                run: Some("a3f91c22".into()),
                verdict: "running".into(),
            }],
        }];
        a.go(Workspace::Hooks);
        a
    }

    fn activity_app() -> App {
        let mut a = app();
        a.activity = vec![
            ActivityItem {
                id: "e1".into(),
                at_ms: 0,
                source: Source::Hook,
                text: "pr-opened fired (PR #212)".into(),
                unread: true,
                needs_you: false,
                jump_to: Some((Workspace::Hooks, "pr-opened".into())),
            },
            ActivityItem {
                id: "e2".into(),
                at_ms: -60_000,
                source: Source::Cron,
                text: "pr-shepherd ran · 3 PRs swept".into(),
                unread: false,
                needs_you: false,
                jump_to: None,
            },
            ActivityItem {
                id: "e3".into(),
                at_ms: -120_000,
                source: Source::Goal,
                text: "inbox-to-zero iteration 118".into(),
                unread: true,
                needs_you: true,
                jump_to: None,
            },
        ];
        a.go(Workspace::Activity);
        a
    }

    /// Every screen with something on it, for the sweeps that have to see the
    /// populated form of a list as well as its empty state.
    fn populated() -> App {
        let mut a = memory_app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.schedules = schedules_app().schedules;
        a.goals = goals_app().goals;
        a.hooks = hooks_app().hooks;
        a.activity = activity_app().activity;
        a.tasks = vec![task("t1", "port the parser", Some("scout"), "open")];
        a.team = Some("crew".into());
        a.graph = GraphView::new("prefers-spec-first");
        a.push(Entry::Agent("here is the summary".into()));
        a
    }

    // ---- the completion popup ----

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
        assert!(
            screen.contains("show or hide reasoning"),
            "the hint is shown"
        );
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

    /// With thirty-odd commands the list outgrows the space above the input, so
    /// it has to scroll — otherwise ↓ past the fold moves a cursor nobody can
    /// see.
    #[test]
    fn the_popup_scrolls_to_keep_the_highlighted_row_visible() {
        let mut a = app();
        a.input = "/".into();
        a.suggestion = crate::tui::command::HELP.len() - 1;
        let screen = rendered(&a, 100, 16);
        let last = crate::tui::command::HELP.last().unwrap().0;
        assert!(screen.contains(last), "expected {last}:\n{screen}");
    }

    /// Eighteen ragged rows read as noise; the eye needs an edge to run down.
    /// Rendered tall, because the command list has since grown past what a
    /// short terminal's popup can hold.
    #[test]
    fn the_completion_hints_line_up_in_a_column() {
        let mut a = app();
        a.input = "/".into();
        let screen = rendered(&a, 100, 40);
        // Counted in characters, not bytes: the selection marker is three bytes
        // wide and one column wide, and a byte index would call the two rows
        // misaligned when they are not.
        let column =
            |line: &str, hint: &str| line.find(hint).map(|byte| line[..byte].chars().count());
        let starts: Vec<usize> = screen
            .lines()
            .filter_map(|l| column(l, "this list").or_else(|| column(l, "the team panel")))
            .collect();
        assert_eq!(starts.len(), 2, "expected both rows:\n{screen}");
        assert_eq!(starts[0], starts[1], "hints must share a column:\n{screen}");
    }

    // ---- chat ----

    #[test]
    fn the_frame_draws_without_panicking_and_shows_the_input_box() {
        let out = rendered(&app(), 60, 12);
        assert!(out.contains("you"), "input box must be labelled:\n{out}");
        assert!(out.contains("jod"), "transcript must be titled:\n{out}");
    }

    /// An empty bordered box is a box. The caret says the keystrokes land
    /// here; the hint says what to put in it.
    #[test]
    fn the_empty_composer_says_where_to_type_and_what_to_type() {
        let screen = rendered(&app(), 100, 24);
        assert!(
            screen.contains("› tell Jod what to do"),
            "caret and hint:\n{screen}"
        );
        assert!(
            screen.contains("/ for commands"),
            "the other way in:\n{screen}"
        );
    }

    /// The hint is furniture for an empty field — once there is something to
    /// send it would be in the way. The caret is not, and stays.
    #[test]
    fn the_hint_goes_at_the_first_keystroke_and_the_caret_stays() {
        let mut a = app();
        a.input = "summarise my inbox".into();
        a.cursor = a.input.len();
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("› summarise my inbox"), "{screen}");
        assert!(
            !screen.contains("tell Jod what to do"),
            "the hint must not sit under the typing:\n{screen}"
        );
    }

    /// Half a sentence reads as a rendering bug, not a hint.
    #[test]
    fn the_hint_shortens_to_fit_rather_than_being_cut_off() {
        for width in 0..80 {
            assert!(
                placeholder(width).chars().count() <= width,
                "{width}: {:?}",
                placeholder(width)
            );
        }
    }

    /// Two columns of furniture is a lot on a box this narrow, and text you
    /// cannot read is worse than a field you have to guess at.
    #[test]
    fn a_narrow_composer_drops_the_caret_rather_than_the_text() {
        let mut a = app();
        a.input = "abcdefg".into();
        a.cursor = a.input.len();
        let screen = rendered(&a, 10, 8);
        assert!(screen.contains("abcdefg"), "the text survives:\n{screen}");
        assert!(!screen.contains('›'), "no room for a caret:\n{screen}");
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

    /// The whole point of delegating is that the work continues off screen.
    #[test]
    fn the_status_bar_reports_agents_working_off_screen() {
        let mut a = app();
        a.agents = vec![agent_line("bbb22222", "audit the deps", "running")];
        assert!(rendered(&a, 100, 12).contains("1 in background"));
    }

    // ---- the new-session splash ----

    /// A fresh session is a wordmark and somewhere to type, not an empty box
    /// with a title bar — OpenCode's move, and the right one: the first thing
    /// on screen should say what you launched.
    #[test]
    fn a_new_session_shows_the_wordmark_over_a_centred_input_box() {
        let screen = rendered(&app(), 100, 24);
        for row in banner() {
            assert!(
                screen.contains(row.trim_end()),
                "wordmark row missing {row:?}:\n{screen}"
            );
        }
        assert!(screen.contains("an orchestrator"), "the caption:\n{screen}");
        assert!(screen.contains("you"), "and somewhere to type:\n{screen}");
    }

    /// The event loop pushes a hint notice at startup and `/new` pushes one of
    /// its own, so "the transcript is empty" would never be true and the splash
    /// would never appear. The first real turn is what ends it.
    #[test]
    fn the_wordmark_survives_a_notice_and_goes_when_the_conversation_starts() {
        let mut a = app();
        a.push(Entry::Notice("Alt-K opens every screen".into()));
        assert!(rendered(&a, 100, 24).contains("an orchestrator"));

        a.push(Entry::You("summarise my inbox".into()));
        let screen = rendered(&a, 100, 24);
        assert!(!screen.contains("an orchestrator"), "{screen}");
        assert!(screen.contains("summarise my inbox"), "{screen}");
    }

    /// Watching a delegated run is not a new session: that transcript is
    /// somebody else's conversation, and it is empty because the run has not
    /// said anything yet.
    #[test]
    fn watching_a_run_shows_its_transcript_rather_than_the_splash() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.watching = Some("aaa11111".into());
        let screen = rendered(&a, 100, 24);
        assert!(!screen.contains("an orchestrator"), "{screen}");
        assert!(screen.contains("port the parser"), "{screen}");
    }

    /// Below the wordmark's width the block letters would be truncated
    /// mid-glyph, which reads as a broken screen rather than as a logo.
    #[test]
    fn the_splash_falls_back_to_plain_lettering_on_a_narrow_terminal() {
        let screen = rendered(&app(), 30, 14);
        assert!(screen.contains("Jod AI"), "{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 30), "{screen}");
    }

    /// The completion popup grows upwards out of the input box and the command
    /// list is thirty-odd rows, so a centred input would leave it half a screen
    /// and the list would come out cut in half.
    #[test]
    fn the_splash_yields_to_the_completion_popup_rather_than_clipping_it() {
        let mut a = app();
        a.input = "/".into();
        let screen = rendered(&a, 100, 40);
        assert!(screen.contains("this list"), "/help:\n{screen}");
        assert!(
            screen.contains("the team panel"),
            "/team, thirty rows further down:\n{screen}"
        );
    }

    // ---- padding and the readable measure ----

    #[test]
    fn the_screen_has_a_margin_rather_than_starting_in_the_corner() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        let screen = rendered(&a, 100, 20);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(
            rows[0].trim().is_empty(),
            "a row of air on top: {:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("  ┌"),
            "a gutter to the left: {:?}",
            rows[1]
        );
        assert!(rows[1].ends_with("┐  "), "and to the right: {:?}", rows[1]);
    }

    /// A chat column that runs edge to edge on a 200-column terminal is a
    /// column nobody can read a paragraph in.
    #[test]
    fn the_chat_column_is_capped_and_centred_on_a_wide_terminal() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        let screen = rendered(&a, 200, 24);
        let top = screen.lines().find(|l| l.contains("jod")).unwrap();
        let left = top.chars().position(|c| c == '┌').unwrap();
        let right = top.chars().position(|c| c == '┐').unwrap();
        assert!(left > GUTTER as usize, "the box must be inset: {left}");
        // Inclusive of both border columns, which is what `MEASURE` counts.
        let box_width = right - left + 1;
        assert!(
            box_width <= MEASURE as usize,
            "capped at the measure: {box_width} wide, {left}..{right}"
        );
        assert_eq!(left, 199 - right, "and centred in what is left");
    }

    /// A workspace is the opposite of chat: its table wants every column it can
    /// get, so the measure must not follow it there.
    #[test]
    fn a_workspace_table_is_not_capped_at_the_chat_measure() {
        let a = schedules_app();
        let screen = rendered(&a, 200, 30);
        let top = screen.lines().find(|l| l.contains("schedules")).unwrap();
        let left = top.chars().position(|c| c == '┌').unwrap();
        let right = top.chars().position(|c| c == '┐').unwrap();
        assert!(
            right - left + 1 > MEASURE as usize,
            "the table gets the whole width: {left}..{right}"
        );
    }

    // ---- the permission mode ----

    /// What the next turn may do without asking changes while you are talking,
    /// and a setting you have to press a key to see is one you will be wrong
    /// about exactly when it matters.
    #[test]
    fn the_mode_is_named_on_every_screen() {
        for ws in [Workspace::Chat, Workspace::Fleet, Workspace::Schedules] {
            let mut a = app();
            a.mode = PermissionPolicy::AcceptEdits;
            a.go(ws);
            let screen = rendered(&a, 120, 20);
            let status = screen.lines().last().unwrap();
            assert!(status.contains("edits"), "{ws:?}: {status}");
        }
    }

    /// Colour is decoration here, never the message: each mode carries its own
    /// glyph *and* its own word, so `NO_COLOR` loses neither.
    #[test]
    fn every_mode_has_its_own_glyph_and_its_own_word() {
        let mut glyphs = std::collections::HashSet::new();
        let mut words = std::collections::HashSet::new();
        for mode in PermissionPolicy::ALL {
            let mut a = app();
            a.mode = mode;
            let screen = rendered(&a, 120, 20);
            let status = screen.lines().last().unwrap().to_string();
            assert!(status.contains(mode.label()), "{mode:?}: {status}");
            assert!(status.contains(mode_glyph(mode)), "{mode:?}: {status}");
            glyphs.insert(mode_glyph(mode));
            words.insert(mode.label());
        }
        assert_eq!(glyphs.len(), 4, "the glyphs must differ: {glyphs:?}");
        assert_eq!(words.len(), 4, "and so must the words: {words:?}");
    }

    /// The mode that auto-approves everything is the one that can do damage
    /// unattended, so it is the one that reads as dangerous.
    #[test]
    fn the_mode_that_approves_everything_reads_as_the_dangerous_one() {
        assert_eq!(mode_colour(PermissionPolicy::Bypass), BAD);
        assert_eq!(mode_colour(PermissionPolicy::Plan), GOOD);
        assert_eq!(
            mode_glyph(PermissionPolicy::Plan),
            "○",
            "the hollow glyph belongs to the one mode that cannot act"
        );
    }

    // ---- the right-hand panel ----

    #[test]
    fn the_panel_is_drawn_only_when_it_is_open() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        assert!(
            !rendered(&a, 140, 24).contains("sessions"),
            "shut by default"
        );

        a.panel = true;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("sessions"), "{screen}");
        assert!(
            screen.contains("Shift-Tab closes"),
            "the way out:\n{screen}"
        );
        assert!(
            screen.contains("port the parser"),
            "what is running:\n{screen}"
        );
        assert!(screen.contains("aaa11111"), "and its id:\n{screen}");
    }

    /// An empty panel has to say what would fill it rather than show a box with
    /// nothing in it, which reads as a bug.
    #[test]
    fn an_empty_panel_says_how_to_start_a_run() {
        let mut a = app();
        a.panel = true;
        assert!(rendered(&a, 140, 24).contains("no runs yet"));
    }

    #[test]
    fn the_panel_names_the_mode_and_the_key_that_cycles_it() {
        let mut a = app();
        a.panel = true;
        a.mode = PermissionPolicy::Plan;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("Tab cycles"), "{screen}");
        assert!(screen.contains("○ plan"), "{screen}");
    }

    // ---- the context box ----

    /// `CONTEXT_WINDOW` is one assumed figure for every model and
    /// `context_tokens` is the last turn as the harness reported it, so the box
    /// hedges every number rather than printing one that looks measured.
    #[test]
    fn the_context_box_reads_as_an_estimate_not_a_measurement() {
        let mut a = app();
        a.panel = true;
        a.context_tokens = 40_000;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("context"), "{screen}");
        assert!(screen.contains("≈20%"), "{screen}");
        assert!(screen.contains("≈40k of an assumed 200k"), "{screen}");
        assert!(screen.contains("estimated, not measured"), "{screen}");
        assert!(screen.contains('▓'), "a bar, not only a number:\n{screen}");
    }

    /// Compaction is cheap and losing a conversation to a hard context error is
    /// not, so the advice arrives before the wall — and not a moment earlier,
    /// or it is noise.
    #[test]
    fn the_context_box_recommends_compaction_past_the_threshold_and_not_before() {
        use super::super::app::{COMPACT_AT, CONTEXT_WINDOW};
        let mut a = app();
        a.panel = true;

        a.context_tokens = (CONTEXT_WINDOW as f64 * (COMPACT_AT - 0.1)) as u64;
        let quiet = rendered(&a, 140, 24);
        assert!(!quiet.contains("compact"), "too early to say so:\n{quiet}");

        a.context_tokens = (CONTEXT_WINDOW as f64 * COMPACT_AT) as u64;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("⚠ compact recommended"), "{screen}");
    }

    /// The panel is shut most of the time, and advice nobody can see is not
    /// advice — so the recommendation itself rides the row that is always on.
    #[test]
    fn the_compaction_advice_reaches_the_status_bar_with_the_panel_shut() {
        use super::super::app::CONTEXT_WINDOW;
        let mut a = app();
        a.context_tokens = CONTEXT_WINDOW;
        let screen = rendered(&a, 140, 24);
        assert!(
            !screen.contains("estimated"),
            "the panel is shut:\n{screen}"
        );
        assert!(
            screen.lines().last().unwrap().contains("⚠ compact"),
            "{screen}"
        );
    }

    // ---- the panel on a small terminal ----

    /// Taking 34 columns off an 80-column terminal leaves a chat column
    /// narrower than the panel, so below the threshold the panel floats instead
    /// — Shift-Tab must never look broken.
    #[test]
    fn the_panel_floats_rather_than_starving_the_chat_on_a_narrow_terminal() {
        let mut a = app();
        a.panel = true;
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        let screen = rendered(&a, 60, 20);
        assert!(
            screen.contains("sessions"),
            "Shift-Tab must still do something:\n{screen}"
        );
        assert!(screen.lines().all(|l| l.chars().count() <= 60), "{screen}");
    }

    /// `centred` exists because clamping to a comfortable minimum alone
    /// produced rects outside the buffer on a narrow window. The panel, the
    /// splash and the measure must not reintroduce that.
    #[test]
    fn nothing_overflows_a_small_terminal_with_the_panel_open() {
        let mut a = app();
        a.panel = true;
        a.context_tokens = 190_000;
        a.agents = (0..12)
            .map(|i| agent_line(&format!("id{i}"), &format!("job {i}"), "running"))
            .collect();
        for (w, h) in [(60, 20), (40, 12), (30, 10), (20, 6), (12, 5), (200, 60)] {
            let screen = rendered(&a, w, h);
            assert!(
                screen.lines().all(|l| l.chars().count() <= w as usize),
                "{w}×{h} overflowed:\n{screen}"
            );
        }
    }

    // ---- the way out ----

    /// Regression, and the reason it survived in plain sight: `two_ends`
    /// reserved the verb list first and dropped the exit *whole*, so at 80
    /// columns — an entirely ordinary terminal — Chat, Fleet and Memory printed
    /// their verbs and stopped saying how to leave. Nothing failed, because
    /// every render test in the suite was 150 wide.
    ///
    /// 80×24 is the contract, so the contract is what this asserts. The exit is
    /// the invariant; the verbs are best-effort and may be elided.
    #[test]
    fn every_screen_still_says_how_to_leave_at_eighty_columns() {
        for ws in Workspace::ALL {
            let mut a = populated();
            a.go(ws);
            let screen = rendered(&a, 80, 24);
            let rows: Vec<&str> = screen.lines().collect();
            let keybar = rows[rows.len() - 2];
            assert!(
                keybar.contains(keys::keybar_exit(ws)),
                "{ws:?} at 80 columns stopped saying how to leave: {keybar}"
            );

            // Both halves, not one bought with the other. `keys::keybar`
            // budgets its verbs against exactly the room `two_ends` leaves it,
            // so what it hands back is what reaches the screen — if the two
            // ever disagree about the three columns of padding, this is where
            // the whole left half silently vanishes.
            let verbs = keys::keybar(ws, 80);
            assert!(!verbs.is_empty(), "{ws:?} budgeted no verbs at all");
            assert!(
                keybar.contains(&verbs),
                "{ws:?} budgeted {verbs:?} but the bar printed: {keybar}"
            );

            // A bar that dropped something has to admit it, or the screen
            // quietly teaches a subset and you never learn the rest exists.
            // Fleet is the sharp case — twelve verbs truncate at every width.
            if verbs != keys::keybar(ws, 400) {
                assert!(
                    verbs.ends_with("? more"),
                    "{ws:?} dropped verbs without saying so: {verbs:?}"
                );
            }
        }
    }

    /// The exact coupling with `keys::verb_budget`: a left half of *precisely*
    /// the budgeted width must still be printed.
    ///
    /// It asks `keys::verb_budget` for the number rather than repeating it,
    /// which is not merely tidier — it is the only version that catches the
    /// dangerous direction. A hardcoded copy passes when `verb_budget` grows
    /// *more* generous than `two_ends`, and that is exactly the break: the
    /// keybar hands back a string this renderer then elides whole, so a screen
    /// loses all its verbs rather than one.
    ///
    /// Rendering a real keybar does not prove this. `keys::keybar` drops whole
    /// verbs, so it lands under its budget by however much the last dropped
    /// verb was worth and absorbs a padding disagreement of one or two columns
    /// — measured: widening the padding in `two_ends` from 3 to 5 leaves every
    /// eighty-column render passing, and would still lose a screen's entire
    /// verb list the day its verbs ended on the boundary.
    #[test]
    fn two_ends_accepts_a_left_half_of_exactly_the_budgeted_width() {
        for ws in Workspace::ALL {
            for width in [80u16, 100, 120, 150] {
                let right = keys::keybar_exit(ws);
                let budget = keys::verb_budget(ws, width);
                let left = "x".repeat(budget);
                let line = two_ends(&left, right, width, MUTED);
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    text.contains(&left),
                    "{ws:?} at {width}: a left half of exactly the budget was elided: {text:?}"
                );
                assert!(
                    text.contains(right),
                    "{ws:?} at {width}: the exit went: {text:?}"
                );
                assert!(
                    text.chars().count() <= width as usize,
                    "{ws:?} at {width}: overflowed: {text:?}"
                );
            }
        }
    }

    /// The other half of the same rule: the two halves may never touch, at any
    /// width. Whichever one is elided, what is printed stays legible.
    #[test]
    fn the_two_halves_of_a_bar_never_run_together() {
        for width in [200u16, 150, 100, 80, 60, 40, 24, 12] {
            let line = two_ends(
                "Alt-B delegate · Alt-K menu",
                "Alt-X stop · Ctrl-C quit",
                width,
                MUTED,
            );
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().count() <= width as usize,
                "{width}: overflowed with {text:?}"
            );
            assert!(
                !text.contains("menuAlt-X"),
                "{width}: they touched: {text:?}"
            );
            // The exit survives every width that can hold it at all.
            if "Alt-X stop · Ctrl-C quit".len() + 2 <= width as usize {
                assert!(
                    text.contains("Alt-X stop · Ctrl-C quit"),
                    "{width}: the way out went: {text:?}"
                );
            }
        }
    }

    /// Regression: `keys::footer(Fleet)` is 58 cells and the master pane is 46
    /// at 100 columns, so the bottom border read `… r resume · d d`. `d d` is
    /// not a key. Nothing caught it because the suite rendered at 150, where
    /// the pane is wide enough, and at 80, where `split` has not engaged and
    /// the pane is full width — the broken band brackets the design size.
    ///
    /// A cut *keymap* is worse than a cut title: it does not look damaged, it
    /// looks like a shorter key.
    #[test]
    fn a_footer_drops_whole_verbs_rather_than_cutting_one_in_half() {
        for ws in [Workspace::Fleet, Workspace::Memory] {
            for width in [80u16, 90, 100, 110, 120, 150, 200] {
                let mut a = populated();
                a.go(ws);
                let screen = rendered(&a, width, 20);
                let row = screen
                    .lines()
                    .rev()
                    .find(|l| l.contains("pick"))
                    .unwrap_or_else(|| panic!("{ws:?} at {width} printed no footer"));
                // Past 90 columns the detail pane shares this row, so the master
                // pane's own border segment is taken first — otherwise the split
                // below runs across the join and invents a verb out of two.
                let footer = row
                    .split('┘')
                    .find(|segment| segment.contains("pick"))
                    .unwrap_or(row);
                // Every verb on the border must be one `keys::footer` actually
                // offers, matched *whole*. `contains` would not do: `r resu` is
                // a substring of `r resume`, so a prefix — which is exactly the
                // bug — would satisfy it, and this test passed against the
                // unfixed code until it compared for equality instead.
                let full = keys::footer(ws);
                let offered: Vec<&str> = full.trim().split(" · ").collect();
                for verb in footer
                    .split('·')
                    .map(|v| v.trim_matches(|c: char| c == '─' || c == '└' || c == '┘' || c == ' '))
                    .filter(|v| !v.is_empty())
                {
                    assert!(
                        offered.contains(&verb),
                        "{ws:?} at {width}: {verb:?} is not a whole verb of {offered:?} — {footer}"
                    );
                }
            }
        }
    }

    /// Regression: Fleet's keymap is 46 rows and `centred` clamps to the
    /// terminal, so at 80×30 the overlay drew 28 and dropped the rest — the
    /// whole `anywhere` section — with nothing on screen admitting it. Help
    /// that lies about being complete is worse than no help, because you stop
    /// looking.
    #[test]
    fn the_keymap_overlay_shows_every_binding_or_counts_what_it_cannot() {
        for (w, h) in [(150u16, 40u16), (100, 30), (80, 30), (80, 24), (60, 20)] {
            let mut a = populated();
            a.go(Workspace::Fleet);
            a.overlay = Overlay::Keymap;
            let screen = rendered(&a, w, h);
            let total: usize = keys::keymap(Workspace::Fleet)
                .into_iter()
                .map(|(_, bindings)| bindings.len())
                .sum();
            let shown = keys::keymap(Workspace::Fleet)
                .into_iter()
                .flat_map(|(_, bindings)| bindings)
                .filter(|b| screen.contains(b.what))
                .count();
            if shown < total {
                assert!(
                    screen.contains("more — widen the window"),
                    "{w}×{h}: {shown} of {total} shown and the overlay did not say so:\n{screen}"
                );
            }
        }
    }

    /// The size the overlay is actually used at has to be complete, not merely
    /// honest about being incomplete.
    #[test]
    fn the_keymap_overlay_is_complete_at_the_design_size() {
        let mut a = populated();
        a.go(Workspace::Fleet);
        a.overlay = Overlay::Keymap;
        let screen = rendered(&a, 100, 30);
        for (_, bindings) in keys::keymap(Workspace::Fleet) {
            for binding in bindings {
                assert!(
                    screen.contains(binding.what),
                    "{:?} missing from the keymap at 100×30:\n{screen}",
                    binding.what
                );
            }
        }
    }

    /// Regression across three screens: trailing markers were pushed and left
    /// for the widget to clip. The fleet said `← on scr` at 80 and a bare `← `
    /// at 150, the graph trail said `← whe` at 60, and the activity feed said
    /// `← n` — for `← needs you`, the marker whose entire job is to say a
    /// person is required.
    ///
    /// The rule is the keybar's: whole or nothing. A marker that is present is
    /// present in full, and one that will not fit is absent rather than cut,
    /// because a cut phrase teaches a phrase that does not exist.
    #[test]
    fn a_row_marker_is_printed_whole_or_not_at_all() {
        let mut whole = 0usize;
        let mut check = |screen: &str, needle: &str, marker: &str, what: &str, w: u16| {
            for line in screen.lines().filter(|l| l.contains(needle)) {
                if !line.contains('←') {
                    continue;
                }
                assert!(
                    line.contains(marker),
                    "{what} at {w}: marker cut — {line:?}"
                );
                whole += 1;
            }
        };

        for w in [40u16, 60, 80, 100, 110, 120, 150, 200] {
            let mut fleet = app();
            fleet.agents = vec![agent_line(
                "aaa11111",
                "port the parser to the new AST",
                "completed",
            )];
            fleet.watching = Some("aaa11111".into());
            fleet.go(Workspace::Fleet);
            check(
                &rendered(&fleet, w, 14),
                "aaa11111",
                "← on screen",
                "fleet",
                w,
            );

            let mut feed = activity_app();
            feed.activity[2].text =
                "inbox-to-zero iteration 118 stalled on a decision nobody has made yet".into();
            check(
                &rendered(&feed, w, 20),
                "stalled",
                "← needs you",
                "activity",
                w,
            );

            let mut graph = memory_app();
            graph.graph = GraphView::new("linear-is-truth");
            graph.graph.recentre("prefers-spec-first");
            graph.drill(Workspace::MemoryGraph);
            check(
                &rendered(&graph, w, 24),
                "linear-is-truth ⟩",
                "← where you have been",
                "graph trail",
                w,
            );
        }

        // Anti-vacuity: a run where every marker was dropped for want of room
        // has not shown they print whole, only that they can be absent.
        assert!(whole > 0, "no marker was ever printed in full");
    }

    /// The marker is the invariant and the text is best-effort — the `two_ends`
    /// ruling one level down. Dropping markers whole fixed the lie but left the
    /// priority backwards: `← needs you` then vanished exactly when a line ran
    /// long, which is precisely when something worth saying had happened.
    #[test]
    fn a_needs_you_marker_outlives_the_text_it_sits_beside() {
        for w in [80u16, 100, 120, 150] {
            let mut a = activity_app();
            a.activity[2].text =
                "inbox-to-zero iteration 118 stalled on a decision nobody has made yet".into();
            let screen = rendered(&a, w, 20);
            let row = screen
                .lines()
                .find(|l| l.contains("inbox-to-zero"))
                .unwrap();
            assert!(
                row.contains("← needs you"),
                "{w}: the marker yielded instead of the text — {row}"
            );
        }
    }

    /// And whichever half yields has to say so. A silent clip and an ellipsis
    /// are identical to the renderer and completely different to a reader:
    /// `port the p` looks like a short name, `port the…` looks like a cut one.
    #[test]
    fn text_that_was_shortened_says_that_it_was() {
        let mut a = app();
        a.agents = vec![agent_line(
            "aaa11111",
            "port the parser to the new AST",
            "completed",
        )];
        a.go(Workspace::Fleet);
        let screen = rendered(&a, 100, 16);
        let line = screen.lines().find(|l| l.contains("aaa11111")).unwrap();
        // Past 90 columns the detail pane shares the row and prints the name in
        // full, so the master pane's own segment is taken first — otherwise the
        // assertion below reads the *other* pane's copy and never fails.
        let row = line
            .split('│')
            .find(|segment| segment.contains("aaa11111"))
            .unwrap_or(line);
        assert!(row.contains('…'), "a cut name must admit it: {row}");
        assert!(
            !row.contains("port the parser to the new AST"),
            "it really was too narrow to fit: {row}"
        );
    }

    // ---- the delivery gutter ----

    fn owing(id: &str, name: &str, verdict: Verdict) -> super::super::AgentLine {
        let mut line = agent_line(id, name, "completed");
        line.delivery = verdict;
        line
    }

    /// A run whose reply was lost renders identically to one delivered unless
    /// the row says otherwise — and "the run says completed" is exactly the
    /// state in which nobody thinks to look.
    #[test]
    fn a_run_whose_reply_was_lost_is_marked_on_the_row() {
        let mut a = app();
        a.agents = vec![owing("aaa11111", "answer the telegram", Verdict::Lost)];
        a.go(Workspace::Fleet);
        let screen = rendered(&a, 100, 16);
        // By id rather than by name: the detail pane on the right prints the
        // name too, and at this width it shares a screen line with the pinned
        // chat's row — so matching on the name finds the wrong row.
        let row = screen.lines().find(|l| l.contains("aaa11111")).unwrap();
        assert!(row.contains('⊘'), "the lost mark belongs on the row: {row}");
    }

    /// The two facts are different — they got nothing, or they may hold two —
    /// so they must not wear the same mark. The shape of
    /// `render_time::a_settled_row_and_an_owed_one_never_share_a_glyph`.
    #[test]
    fn no_two_delivery_verdicts_share_a_glyph() {
        let marks: Vec<&str> = [
            Verdict::Lost,
            Verdict::Owed,
            Verdict::Twice,
            Verdict::Fine,
            Verdict::Nothing,
        ]
        .iter()
        .map(|v| v.glyph())
        .collect();
        let mut all = marks.clone();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 5, "two verdicts share a glyph: {marks:?}");

        // And none of them collides with the status glyph two cells to its
        // right, or the row would show one character meaning two things.
        for status in ["running", "completed", "failed", "killed", "queued"] {
            for verdict in [Verdict::Lost, Verdict::Twice] {
                assert_ne!(
                    verdict.glyph(),
                    run_glyph(status),
                    "{verdict:?} collides with the {status} glyph"
                );
            }
        }
    }

    /// Rare is the whole basis on which this earns attention: a mark on every
    /// row is a mark nobody reads. `Nothing` is the common case — most runs
    /// report into the transcript and owe nobody anything.
    ///
    /// It asserts the gutter is **blank**, not that it avoids two particular
    /// glyphs. That distinction is the test: the first version checked the row
    /// carried neither `⊘` nor `♻`, and swapping `marks_a_row` for `is_trouble`
    /// passed it — because `Owed` draws `○`, which is neither. A test that
    /// enumerates the marks it forbids cannot see a new one being added.
    #[test]
    fn a_run_that_owed_nobody_anything_wears_no_mark() {
        for quiet in [Verdict::Nothing, Verdict::Fine, Verdict::Owed] {
            assert_eq!(
                delivery_gutter(quiet).content,
                " ",
                "{quiet:?} must leave the gutter empty, whatever glyph it owns"
            );
        }
        for loud in [Verdict::Lost, Verdict::Twice] {
            assert_eq!(
                delivery_gutter(loud).content,
                loud.glyph(),
                "{loud:?} must wear its own mark rather than one invented here"
            );
        }
    }

    /// The gutter is a fixed column, so it must not push the row off the end of
    /// a narrow pane — the failure this file has now had four times.
    #[test]
    fn the_delivery_gutter_costs_a_narrow_row_nothing_it_cannot_spare() {
        let mut a = app();
        a.agents = (0..12)
            .map(|i| owing(&format!("id{i:06}"), &format!("job {i}"), Verdict::Lost))
            .collect();
        a.go(Workspace::Fleet);
        for (w, h) in [
            (150u16, 20u16),
            (100, 16),
            (80, 14),
            (60, 12),
            (40, 10),
            (20, 6),
            (12, 5),
        ] {
            let screen = rendered(&a, w, h);
            assert!(
                screen.lines().all(|l| l.chars().count() <= w as usize),
                "{w}×{h} overflowed:\n{screen}"
            );
        }
        // The name is what the row is for, so it survives to the width where
        // the pane still has one.
        let screen = rendered(&a, 100, 16);
        assert!(screen.contains("job 0"), "the name must survive:\n{screen}");
    }

    // ---- what the screens teach ----

    /// The complement to `keys.rs`'s scan, which walks the tables and the two
    /// which-key accessors — it cannot see a chord named in *prose*, and prose
    /// is exactly where `Ctrl-K` and `Ctrl-B` survived the move to Alt: in the
    /// splash caption and in two empty-state sentences, which no table owns.
    ///
    /// So this one reads the finished screen instead of the source. Anything a
    /// pixel teaches is caught, whatever string it came from. `Ctrl-C` is the
    /// one survivor by design: leaving must never depend on finding the right
    /// table.
    ///
    /// **`Overlay::Keymap` is excluded deliberately, and this is not a gap to
    /// close.** That overlay *is* the global table on display, and the global
    /// table is the one surface where Ctrl is legitimately taught — readline's
    /// `Ctrl-U` and `Ctrl-W` are not ours to move. It is covered from the other
    /// side by `keys::tests::the_verbs_are_advertised_on_alt_and_the_editing_
    /// keys_on_ctrl`, which pins those two present and `Ctrl-K`/`Ctrl-G`/
    /// `Ctrl-B`/`Ctrl-X`/`Ctrl-T`/`Ctrl-O`/`Ctrl-L` absent. Deleting the
    /// exclusion here would delete that coverage and gain nothing.
    ///
    /// Everything renders wide, at 150×40, because clipping makes a
    /// buffer-reading assertion lie in *both* directions: a `Ctrl-C quit` cut
    /// mid-token leaves a bare `Ctrl-` and fails correct code, and a stale
    /// `Ctrl-B` truncated off the right edge passes broken code. The count at
    /// the end is what proves the width was actually enough — a scan that
    /// found nothing has not passed, it has failed to look.
    #[test]
    fn no_screen_teaches_a_ctrl_chord_that_is_not_ctrl_c() {
        let overlays = || {
            [
                Overlay::None,
                Overlay::WhichKey,
                Overlay::WhichKeyNew,
                Overlay::Confirm {
                    verb: "delete".into(),
                    what: "pr-opened".into(),
                },
                Overlay::Prompt {
                    label: "task".into(),
                    value: "port the parser".into(),
                    intent: PromptIntent::New(Workspace::Tasks),
                },
            ]
        };
        let mut kept = 0usize;
        for ws in Workspace::ALL {
            for overlay in overlays() {
                for panel in [false, true] {
                    // A fresh app as well as a populated one, so the splash
                    // caption and the empty states are both on screen.
                    for mut a in [app(), populated()] {
                        a.go(ws);
                        a.overlay = overlay.clone();
                        a.panel = panel;
                        let screen = rendered(&a, 150, 40);
                        for (i, line) in screen.lines().enumerate() {
                            let mut rest = line;
                            while let Some(at) = rest.find("Ctrl-") {
                                let tail = &rest[at + "Ctrl-".len()..];
                                assert!(
                                    tail.starts_with('C'),
                                    "{ws:?} row {i} teaches a Ctrl verb: {line}"
                                );
                                kept += 1;
                                rest = tail;
                            }
                        }
                    }
                }
            }
        }
        // The anti-vacuity guard. `Ctrl-C quit` is on the keybar of every
        // screen, so a run that saw none of it did not prove the screens are
        // clean — it proved the render was too narrow to hold the token, which
        // is exactly the width at which a stale chord would also have been
        // clipped away unseen.
        assert!(
            kept > 0,
            "no Ctrl-C reached the buffer, so nothing was really scanned"
        );
    }

    // ---- the keybar and the status bar ----

    /// Nielsen #6: the four keys you need right now are on screen, so you never
    /// guess. This is the always-on half of the discoverability pair.
    #[test]
    fn every_screen_prints_its_own_keys_above_the_status_bar() {
        for (ws, expected) in [
            // Alt, not Ctrl: Jod's own verbs moved off the chords tmux
            // intercepts. The property here is unchanged — every screen prints
            // its own keys — only the chord it prints them under.
            (Workspace::Chat, "Alt-K menu"),
            (Workspace::Fleet, "s stop"),
            (Workspace::Memory, "g graph"),
            (Workspace::Schedules, "r run now"),
            // The verb, not the whole label: `a answer escalation` may shorten
            // to `a answer` so it fits Goals' fourth slot at eighty columns,
            // and this assertion is about the screen printing its own key
            // rather than about the wording. Matching the stem means the
            // decision needs no coordinated edit here to avoid a red tree.
            (Workspace::Goals, "a answer"),
            (Workspace::Hooks, "t test payload"),
            (Workspace::Tasks, "d delegate"),
            (Workspace::Activity, "m mark read"),
            (Workspace::Team, "⏎ mark done"),
        ] {
            let mut a = app();
            a.go(ws);
            let screen = rendered(&a, 150, 20);
            let rows: Vec<&str> = screen.lines().collect();
            let keybar = rows[rows.len() - 2];
            assert!(
                keybar.contains(expected),
                "{ws:?} keybar should carry {expected}, got: {keybar}"
            );
        }
    }

    /// One back key, one meaning, and every screen has to say so.
    #[test]
    fn every_workspace_says_esc_goes_back() {
        for ws in [Workspace::Fleet, Workspace::Schedules, Workspace::Activity] {
            let mut a = app();
            a.go(ws);
            let screen = rendered(&a, 150, 20);
            assert!(screen.contains("Esc back"), "{ws:?}:\n{screen}");
        }
    }

    /// Regression: the status and the hints were run together on a narrow
    /// terminal — `1 queuedAlt-X stop`, which reads as neither. The hints now
    /// live on their own row, so the rule is tested on both.
    #[test]
    fn the_bars_drop_their_right_hand_side_rather_than_colliding_with_it() {
        let mut a = app();
        a.busy = true;
        a.turn_started_ms = Some(0);
        a.advance(5_000);
        a.queue("next".into());
        let screen = rendered(&a, 60, 12);
        let bar = screen.lines().last().unwrap();
        assert!(bar.contains("1 queued"), "the status wins: {bar}");
        assert!(
            !bar.contains("queuedCtrl"),
            "they must not run together: {bar}"
        );

        // With room for both, the keybar carries the exits again. `Alt-X`
        // since the keymap moved off the chords tmux takes; `Ctrl-C` stayed
        // where every terminal already puts it.
        let wide = rendered(&a, 150, 12);
        let rows: Vec<&str> = wide.lines().collect();
        assert!(rows[rows.len() - 2].contains("Alt-X stop"), "{wide}");
    }

    /// Endings that arrive while you are away have to survive until you look.
    #[test]
    fn unread_activity_raises_a_badge_on_every_screen() {
        let source = activity_app();
        for ws in [Workspace::Chat, Workspace::Fleet, Workspace::Schedules] {
            let mut a = app();
            a.activity = source.activity.clone();
            a.go(ws);
            let screen = rendered(&a, 120, 20);
            assert!(screen.contains("⚑ 2 unread"), "{ws:?}:\n{screen}");
        }
    }

    #[test]
    fn nothing_unread_raises_no_badge() {
        assert!(!rendered(&app(), 120, 20).contains("⚑"));
    }

    // ---- the which-key overlay ----

    /// The discoverability spine: one chord, every screen, one letter each, and
    /// a live count beside it — so the menu is also a dashboard.
    #[test]
    fn the_which_key_menu_lists_every_workspace_with_a_letter() {
        let mut a = app();
        a.overlay = Overlay::WhichKey;
        let screen = rendered(&a, 100, 30);
        for ws in Workspace::MENU {
            assert!(
                screen.contains(ws.menu_name()),
                "{ws:?} missing from the menu:\n{screen}"
            );
        }
        assert!(screen.contains("Alt-K"));
        assert!(screen.contains("Esc cancels"));
    }

    #[test]
    fn the_which_key_menu_carries_a_live_count_beside_each_row() {
        let mut a = app();
        a.agents = vec![
            agent_line("a", "one", "running"),
            agent_line("b", "two", "failed"),
        ];
        a.overlay = Overlay::WhichKey;
        let screen = rendered(&a, 100, 30);
        assert!(
            screen.contains("2 runs · 1 running · 1 failed"),
            "the menu doubles as a dashboard:\n{screen}"
        );
    }

    /// The digit and the letter are the same destination, so the menu prints
    /// both rather than leaving the digits to be found by accident.
    #[test]
    fn the_which_key_menu_prints_the_digit_beside_the_letter() {
        let mut a = app();
        a.overlay = Overlay::WhichKey;
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("or 2"), "fleet's digit:\n{screen}");
        assert!(screen.contains("or 9"), "team's digit:\n{screen}");
    }

    #[test]
    fn the_new_submenu_names_what_it_can_make() {
        let mut a = app();
        a.overlay = Overlay::WhichKeyNew;
        let screen = rendered(&a, 100, 30);
        for kind in ["schedule", "goal", "hook", "memory", "task"] {
            assert!(screen.contains(kind), "{kind} missing:\n{screen}");
        }
    }

    #[test]
    fn the_which_key_overlay_fits_a_small_terminal() {
        let mut a = app();
        a.overlay = Overlay::WhichKey;
        for (w, h) in [(80, 24), (40, 10), (20, 6), (10, 4)] {
            let _ = rendered(&a, w, h);
        }
    }

    // ---- the keymap overlay ----

    /// Help that omits the focused screen's verbs sends you to the source.
    #[test]
    fn the_keymap_overlay_shows_the_screen_you_are_on_first() {
        let mut a = schedules_app();
        a.overlay = Overlay::Keymap;
        let screen = rendered(&a, 100, 45);
        assert!(screen.contains("schedules — this screen"), "{screen}");
        assert!(screen.contains("run now"));
        assert!(
            screen.contains("anywhere"),
            "the global chords are there too"
        );
    }

    #[test]
    fn the_keymap_overlay_is_different_on_a_different_screen() {
        let mut a = memory_app();
        a.overlay = Overlay::Keymap;
        let screen = rendered(&a, 100, 45);
        assert!(screen.contains("memory · list — this screen"), "{screen}");
        assert!(screen.contains("graph"));
    }

    #[test]
    fn the_keymap_overlay_fits_a_small_terminal() {
        let mut a = app();
        a.overlay = Overlay::Keymap;
        for (w, h) in [(80, 24), (40, 10), (10, 4)] {
            let _ = rendered(&a, w, h);
        }
    }

    // ---- confirmation and prompts ----

    /// `x` deleting a webhook silently is one fat-fingered `Alt-K h x` away
    /// from losing a secret, so the confirmation names the thing.
    #[test]
    fn a_destructive_confirmation_names_what_it_is_about_to_destroy() {
        let mut a = hooks_app();
        a.overlay = Overlay::Confirm {
            verb: "delete".into(),
            what: "pr-opened".into(),
        };
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("delete pr-opened?"), "{screen}");
        assert!(screen.contains("cannot be undone"));
        assert!(screen.contains("y confirms"));
    }

    #[test]
    fn a_one_value_prompt_shows_the_field_and_the_way_out() {
        let mut a = app();
        a.overlay = Overlay::Prompt {
            label: "task".into(),
            value: "port the parser".into(),
            intent: PromptIntent::New(Workspace::Tasks),
        };
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("task ▸ port the parser"), "{screen}");
        assert!(screen.contains("Esc cancels"));
    }

    // ---- the fleet ----

    #[test]
    fn the_fleet_screen_lists_the_runs_it_knows_about() {
        let mut a = app();
        a.agents = vec![super::super::AgentLine {
            delivery: crate::tui::delivery::Verdict::Nothing,
            id: "abcdef1234".into(),
            name: "do the thing".into(),
            harness: "AGY".into(),
            status: "running".into(),
            session: None,
            created_at_ms: 0,
            cost_usd: None,
            last: None,
        }];
        a.go(Workspace::Fleet);
        let out = rendered(&a, 100, 16);
        assert!(out.contains("fleet"), "{out}");
        assert!(out.contains("do the thing"), "{out}");
        assert!(out.contains("abcdef12"), "the id is shortened:\n{out}");
    }

    /// The pinned chat is drawn above the agents, in the agents' own columns,
    /// and the cursor is not on it — three separate claims that together are
    /// what "always pinned on top" has to mean on screen.
    #[test]
    fn the_pinned_chat_is_drawn_above_the_agents() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.go(Workspace::Fleet);
        a.reconcile();
        let screen = rendered(&a, 100, 16);
        let lines: Vec<&str> = screen.lines().collect();

        let pinned = lines
            .iter()
            .position(|l| l.contains("pinned"))
            .expect("the pinned row is drawn");
        let agent = lines
            .iter()
            .position(|l| l.contains("aaa11111"))
            .expect("the agent row is drawn");
        assert!(pinned < agent, "the chat belongs above the work:\n{screen}");

        // The cursor marker is on the agent, not on the chat.
        assert!(!lines[pinned].contains('▸'), "{screen}");
        assert!(lines[agent].contains('▸'), "{screen}");
    }

    /// Selecting it swaps the detail pane for one about a conversation, keys
    /// included: `s stop · r resume · a attach` are meaningless here and
    /// offering them is how a footer stops being trusted.
    #[test]
    fn the_pinned_rows_detail_pane_describes_a_chat_not_a_run() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.go(Workspace::Fleet);
        a.list_mut(Workspace::Fleet).selected = Some(crate::tui::app::MAIN_ROW.to_string());
        let screen = rendered(&a, 100, 20);

        assert!(screen.contains("the chat"), "{screen}");
        assert!(screen.contains("⏎ open"), "{screen}");
        assert!(
            screen.contains("one conversation, and it never ends"),
            "it describes a conversation, uncut at the design width:\n{screen}"
        );
        // The detail pane's own footer, which is the half that is about the
        // selected row. The list keeps `s stop · r resume` because those still
        // apply to every other row on the screen.
        let pane: String = screen
            .lines()
            .filter_map(|l| l.split("││").nth(1))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !pane.contains("s stop"),
            "stop must not be offered for a conversation:\n{pane}"
        );
        // And no field runs into its own value.
        assert!(
            pane.contains("last "),
            "the label column is padded:\n{pane}"
        );
    }

    /// A fleet with nothing delegated is not an empty screen any more — the
    /// pinned chat is in it, above the line saying nothing has been started.
    /// Both have to be there: the row, so there is always somewhere to go, and
    /// the sentence, so a list holding one row does not read as the whole story.
    #[test]
    fn an_empty_fleet_shows_the_pinned_chat_and_says_nothing_is_delegated() {
        let mut a = app();
        a.go(Workspace::Fleet);
        let out = rendered(&a, 80, 16);
        assert!(out.contains("nothing delegated yet"), "{out}");
        assert!(
            out.contains("main"),
            "the pinned row is always drawn:\n{out}"
        );
        assert!(out.contains("pinned"), "{out}");
    }

    /// The screen is a control surface, not a list of names: it has to say what
    /// the selected row is and which keys act on it.
    #[test]
    fn the_fleet_marks_its_selection_and_offers_its_keys() {
        let mut a = app();
        a.agents = vec![
            agent_line("aaa11111", "port the parser", "running"),
            agent_line("bbb22222", "write the docs", "completed"),
        ];
        a.go(Workspace::Fleet);
        let ids = a.row_ids(Workspace::Fleet);
        a.list_mut(Workspace::Fleet).step(1, &ids);
        let screen = rendered(&a, 100, 20);
        assert!(
            screen.contains("▸"),
            "the selection must be visible:\n{screen}"
        );
        assert!(
            screen.contains("⏎ watch"),
            "the keys must be stated:\n{screen}"
        );
        assert!(screen.contains("s stop"));
        assert!(screen.contains("1 running"), "{screen}");
    }

    /// How long a run has been going is the number that decides whether to look
    /// at it, and a list of names cannot tell you.
    #[test]
    fn the_fleet_shows_how_long_each_run_has_been_going() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.go(Workspace::Fleet);
        a.advance(125_000);
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("2m05s"), "expected an age:\n{screen}");
    }

    /// Regression: the harness column was exactly as wide as "Claude Code", so
    /// it ran straight into the name — `Claude CodeHow are you running?`. The
    /// harness is now a short code, and the property being tested is unchanged:
    /// two columns must never touch.
    #[test]
    fn the_fleet_columns_do_not_run_into_each_other() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "How are you running?", "completed")];
        a.go(Workspace::Fleet);
        let screen = rendered(&a, 160, 20);
        assert!(
            screen.contains("cc  How are you running?"),
            "columns must be separated:\n{screen}"
        );
    }

    #[test]
    fn the_fleet_says_which_run_is_on_screen() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.watching = Some("aaa11111".into());
        a.go(Workspace::Fleet);
        assert!(rendered(&a, 100, 20).contains("on screen"));
    }

    /// A fleet longer than the pane must scroll to keep the cursor visible, not
    /// silently hide the row being acted on.
    #[test]
    fn a_long_fleet_scrolls_to_keep_the_selection_in_view() {
        let mut a = app();
        a.agents = (0..40)
            .map(|i| agent_line(&format!("id{i:06}"), &format!("job {i}"), "running"))
            .collect();
        a.go(Workspace::Fleet);
        a.list_mut(Workspace::Fleet).selected = Some("id000039".into());
        let screen = rendered(&a, 100, 16);
        assert!(
            screen.contains("job 39"),
            "the selected row must show:\n{screen}"
        );
        assert!(!screen.contains("job 0 "), "the top must have scrolled off");
    }

    /// The detail pane is the first thing dropped at 80 columns: a 40-column
    /// detail pane holds nothing worth reading, and clipping the master to make
    /// room for it is the anti-pattern.
    #[test]
    fn the_fleet_drops_its_detail_pane_before_it_clips_the_list() {
        let mut a = app();
        let mut line = agent_line("aaa11111", "port the parser", "running");
        line.last = Some("rebased cleanly, all tests pass".into());
        a.agents = vec![line];
        a.go(Workspace::Fleet);

        let wide = rendered(&a, 120, 24);
        assert!(
            wide.contains("rebased cleanly"),
            "the detail shows when it fits"
        );

        let narrow = rendered(&a, 80, 24);
        assert!(
            narrow.contains("port the parser"),
            "the list survives:\n{narrow}"
        );
        assert!(
            !narrow.contains("rebased cleanly"),
            "the detail went:\n{narrow}"
        );
        assert!(narrow.lines().all(|l| l.chars().count() <= 80));
    }

    // ---- memory ----

    #[test]
    fn the_memory_list_shows_a_tag_and_a_glyph_for_every_node() {
        let a = memory_app();
        let screen = rendered(&a, 120, 24);
        assert!(screen.contains("prefers-spec-first"), "{screen}");
        assert!(screen.contains("blf"), "the three-letter tag:\n{screen}");
        assert!(screen.contains("◆"), "and the glyph:\n{screen}");
    }

    #[test]
    fn the_memory_detail_shows_both_directions_of_every_edge() {
        let a = memory_app();
        let screen = rendered(&a, 120, 26);
        assert!(screen.contains("linked from"), "{screen}");
        assert!(screen.contains("links to"), "{screen}");
        assert!(screen.contains("linear-is-truth"));
        assert!(screen.contains("ship-fast-iterate"));
    }

    /// A node in an unresolved contradiction is marked with a glyph as well as
    /// a colour, so the state survives `NO_COLOR` and a monochrome terminal.
    #[test]
    fn a_contradicted_memory_is_marked_without_relying_on_colour() {
        let mut a = memory_app();
        a.memory[0].contradicted = true;
        let screen = rendered(&a, 120, 24);
        let row = screen
            .lines()
            .find(|l| l.contains("prefers-spec-first") && l.contains("blf"))
            .unwrap();
        assert!(row.contains('!'), "expected the contradiction mark: {row}");
    }

    #[test]
    fn an_empty_memory_says_how_to_write_one() {
        let mut a = app();
        a.go(Workspace::Memory);
        assert!(rendered(&a, 100, 20).contains("/remember"));
    }

    #[test]
    fn filtering_the_memory_list_by_type_says_so_on_screen() {
        let mut a = memory_app();
        a.memory_type = Some(MemoryKind::Fact);
        a.reconcile();
        let screen = rendered(&a, 120, 24);
        assert!(screen.contains("showing fact only"), "{screen}");
        assert!(screen.contains("ship-fast-iterate"));
        assert!(
            !screen.contains("linear-is-truth"),
            "beliefs are hidden:\n{screen}"
        );
    }

    // ---- the local graph ----

    /// One node, in-edges above, out-edges below. No layout algorithm, no edge
    /// crossings, and it still reads at 80 columns.
    #[test]
    fn the_local_graph_puts_incoming_edges_above_and_outgoing_below() {
        let mut a = memory_app();
        a.graph = GraphView::new("prefers-spec-first");
        a.drill(Workspace::MemoryGraph);
        let screen = rendered(&a, 100, 30);
        let line_of = |needle: &str| {
            screen
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing:\n{screen}"))
        };
        assert!(line_of("linked from") < line_of("linear-is-truth"));
        assert!(line_of("linear-is-truth") < line_of("links to"));
        assert!(line_of("links to") < line_of("ship-fast-iterate"));
    }

    /// A pane that silently truncates leaves you believing you have seen a
    /// node's whole neighbourhood — the wrong belief to hold about a graph.
    #[test]
    fn the_local_graph_says_how_much_of_the_neighbourhood_it_is_showing() {
        let mut a = memory_app();
        a.graph = GraphView::new("prefers-spec-first");
        a.drill(Workspace::MemoryGraph);
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("hop 1 shows 2 of 17 edges"), "{screen}");
    }

    /// The trail along the bottom is where you have been, which is what makes
    /// `Backspace` believable.
    #[test]
    fn the_local_graph_draws_the_trail_of_where_you_have_been() {
        let mut a = memory_app();
        a.graph = GraphView::new("linear-is-truth");
        a.graph.recentre("prefers-spec-first");
        a.drill(Workspace::MemoryGraph);
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("⟨ linear-is-truth ⟩"), "{screen}");
        assert!(screen.contains("where you have been"));
    }

    #[test]
    fn a_graph_focused_on_a_node_that_is_gone_says_so_rather_than_drawing_nothing() {
        let mut a = memory_app();
        a.graph = GraphView::new("vanished");
        a.drill(Workspace::MemoryGraph);
        assert!(rendered(&a, 100, 30).contains("that node is gone"));
    }

    #[test]
    fn the_local_graph_still_reads_at_eighty_columns() {
        let mut a = memory_app();
        a.graph = GraphView::new("prefers-spec-first");
        a.drill(Workspace::MemoryGraph);
        let screen = rendered(&a, 80, 24);
        assert!(screen.contains("prefers-spec-first"), "{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 80));
    }

    // ---- schedules ----

    /// `systemctl list-timers` verbatim: absolute answers "when", relative
    /// answers "soon?", and the LAST/AGO pair is how you spot a dead timer.
    #[test]
    fn the_schedules_table_shows_next_in_last_and_ago() {
        let a = schedules_app();
        let screen = rendered(&a, 100, 30);
        for column in ["name", "when", "next", "in", "last", "ago", "7d"] {
            assert!(screen.contains(column), "{column} missing:\n{screen}");
        }
        assert!(screen.contains("nightly-inbox"));
        assert!(
            screen.contains("02:00 every day"),
            "the gloss, not the cron:\n{screen}"
        );
    }

    /// A cron expression in a table is a puzzle, so it lives in the detail
    /// block where there is room to read it.
    #[test]
    fn the_raw_cron_expression_is_in_the_detail_not_the_table() {
        let a = schedules_app();
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("0 2 * * *"), "{screen}");
        assert!(screen.contains("Asia/Manila"));
        assert!(screen.contains("Triage the Linear inbox"));
    }

    /// Seven glyphs say "healthy / flaky / dead" faster than seven timestamps,
    /// and a failure is a cross rather than a red block.
    #[test]
    fn a_schedule_carries_a_seven_cell_run_history_strip() {
        let a = schedules_app();
        let screen = rendered(&a, 100, 30);
        // The title bar names the next schedule too, so the row is the one
        // that also carries the gloss.
        let row = screen
            .lines()
            .find(|l| l.contains("nightly-inbox") && l.contains("every day"))
            .unwrap();
        assert!(row.contains('▇'), "the strip:\n{row}");
        assert!(
            row.contains('✗'),
            "a failure is a glyph, not just a colour:\n{row}"
        );
    }

    /// `‖`, not `⏸` — U+23F8 is East-Asian Wide and would shear every column to
    /// its right.
    #[test]
    fn a_paused_schedule_says_so_with_a_narrow_glyph() {
        let a = schedules_app();
        let screen = rendered(&a, 100, 30);
        let row = screen.lines().find(|l| l.contains("deps-audit")).unwrap();
        assert!(row.contains('‖'), "{row}");
        assert!(!row.contains('⏸'));
        assert!(row.contains("— paused"), "and in words too: {row}");
    }

    /// Declared drop order: the 7-day strip goes first, then the LAST/AGO pair,
    /// then the gloss. Nothing clips and nothing scrolls sideways.
    #[test]
    fn the_schedules_table_drops_columns_in_order_at_eighty_columns() {
        let a = schedules_app();
        let wide = rendered(&a, 100, 30);
        assert!(wide.contains("7d"), "the strip fits at 100:\n{wide}");

        let narrow = rendered(&a, 80, 24);
        assert!(
            narrow.contains("nightly-inbox"),
            "the name always stays:\n{narrow}"
        );
        assert!(
            !narrow.contains("7d"),
            "the strip is the first to go:\n{narrow}"
        );
        assert!(narrow.lines().all(|l| l.chars().count() <= 80));
    }

    #[test]
    fn an_empty_schedules_screen_says_how_to_make_one() {
        let mut a = app();
        a.go(Workspace::Schedules);
        assert!(rendered(&a, 100, 20).contains("/new schedule"));
    }

    // ---- goals ----

    /// A goal is a schedule with a denominator: "done when" is a checklist, so
    /// there is a real percent-done. That is why goals get a bar.
    #[test]
    fn a_goal_shows_a_progress_bar_because_its_checklist_is_a_denominator() {
        let a = goals_app();
        let screen = rendered(&a, 110, 30);
        assert!(screen.contains("inbox-to-zero"), "{screen}");
        assert!(screen.contains('▓'), "the bar:\n{screen}");
        assert!(
            screen.contains(" 50%"),
            "the percent stays even if the bar goes:\n{screen}"
        );
        assert!(screen.contains('☑'), "a done check:\n{screen}");
        assert!(screen.contains('☐'), "and one still open:\n{screen}");
    }

    /// A looping objective that quietly needs you and never says so is worse
    /// than no goal at all.
    #[test]
    fn a_goal_waiting_on_you_says_so_on_the_screen() {
        let a = goals_app();
        let screen = rendered(&a, 110, 30);
        assert!(screen.contains("needs you"), "{screen}");
        assert!(screen.contains("ENG-441"));
    }

    /// Drop order: iteration number → cadence → the bar. The percent stays.
    #[test]
    fn the_goals_table_keeps_the_percent_when_the_bar_will_not_fit() {
        let a = goals_app();
        let narrow = rendered(&a, 56, 24);
        let row = narrow
            .lines()
            .find(|l| l.contains("inbox-to-zero") && l.contains('%'))
            .unwrap_or_else(|| panic!("no table row:\n{narrow}"));
        assert!(row.contains("50%"), "the percent survives: {row}");
        assert!(!row.contains('▓'), "the bar went: {row}");
        assert!(narrow.lines().all(|l| l.chars().count() <= 56));
    }

    // ---- webhooks ----

    /// Three questions without a drill-down: is it armed, is the secret still
    /// verifying, and what did the last delivery actually start.
    #[test]
    fn a_webhook_answers_armed_secret_and_last_delivery_on_one_screen() {
        let a = hooks_app();
        let screen = rendered(&a, 110, 30);
        assert!(screen.contains("pr-opened"), "{screen}");
        assert!(screen.contains("pull_request.opened"));
        assert!(screen.contains("verified"), "the secret's state:\n{screen}");
        assert!(
            screen.contains("a3f91c22"),
            "the run a delivery started:\n{screen}"
        );
    }

    #[test]
    fn an_empty_webhooks_screen_says_how_to_make_one() {
        let mut a = app();
        a.go(Workspace::Hooks);
        assert!(rendered(&a, 100, 20).contains("/new hook"));
    }

    // ---- tasks ----

    /// The board, promoted out of the team panel. Promoting it must not lose
    /// it: the screen reads the same board the team panel does.
    #[test]
    fn the_tasks_screen_shows_the_board_that_already_exists() {
        let mut a = app();
        a.tasks = vec![
            task(
                "port-the-parser",
                "Port the parser to the new AST",
                None,
                "open",
            ),
            task("write-the-docs", "Write the docs", Some("scout"), "open"),
        ];
        a.go(Workspace::Tasks);
        let screen = rendered(&a, 110, 24);
        assert!(
            screen.contains("Port the parser to the new AST"),
            "{screen}"
        );
        assert!(screen.contains("scout"), "the owner:\n{screen}");
        assert!(
            screen.contains("claimed"),
            "a claimed task reads as claimed:\n{screen}"
        );
    }

    /// The verb that makes the board worth a screen of its own.
    #[test]
    fn the_tasks_screen_says_what_d_does() {
        let mut a = app();
        a.tasks = vec![task("port-the-parser", "Port the parser", None, "open")];
        a.go(Workspace::Tasks);
        let screen = rendered(&a, 110, 24);
        assert!(screen.contains("d delegates this to an agent"), "{screen}");
    }

    /// Without a runnable check, "looks done" is the only stop signal — so the
    /// screen says when there is not one rather than leaving the field blank.
    #[test]
    fn a_task_with_no_runnable_check_says_so() {
        let mut a = app();
        a.tasks = vec![task("port-the-parser", "Port the parser", None, "open")];
        a.go(Workspace::Tasks);
        assert!(rendered(&a, 110, 24).contains("no stop signal"));
    }

    #[test]
    fn an_empty_board_screen_says_how_to_add_to_it() {
        let mut a = app();
        a.go(Workspace::Tasks);
        assert!(rendered(&a, 100, 20).contains("/todo"));
    }

    // ---- activity ----

    /// An ending that scrolled off the transcript while you were away did not
    /// happen. This is the durable record.
    #[test]
    fn the_activity_feed_marks_what_is_unread_in_the_gutter() {
        let a = activity_app();
        let screen = rendered(&a, 100, 24);
        let unread = screen
            .lines()
            .find(|l| l.contains("pr-opened fired"))
            .unwrap();
        let read = screen
            .lines()
            .find(|l| l.contains("pr-shepherd ran"))
            .unwrap();
        assert!(
            unread.contains('●'),
            "an unread dot in the gutter: {unread}"
        );
        assert!(!read.contains('●'), "and none once read: {read}");
    }

    #[test]
    fn an_activity_item_that_needs_a_human_says_so() {
        let a = activity_app();
        assert!(rendered(&a, 100, 24).contains("needs you"));
    }

    #[test]
    fn the_activity_feed_says_which_filters_are_on() {
        let mut a = activity_app();
        a.unread_only = true;
        a.activity_source = Some(Source::Cron);
        a.reconcile();
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("only unread: on"), "{screen}");
        assert!(screen.contains("cron"), "{screen}");
    }

    #[test]
    fn an_empty_activity_feed_says_what_writes_to_it() {
        let mut a = app();
        a.go(Workspace::Activity);
        assert!(rendered(&a, 100, 20).contains("cron, hooks and goals write here"));
    }

    // ---- the filter ----

    /// `/` on every list, and an active filter is never invisible.
    #[test]
    fn an_open_filter_is_drawn_under_the_list_it_filters() {
        let mut a = app();
        a.agents = vec![
            agent_line("aaa11111", "port the parser", "running"),
            agent_line("bbb22222", "write the docs", "running"),
        ];
        a.go(Workspace::Fleet);
        let list = a.list_mut(Workspace::Fleet);
        list.filter = Some("port".into());
        list.editing_filter = true;
        a.reconcile();

        let screen = rendered(&a, 110, 24);
        assert!(screen.contains("/port"), "{screen}");
        assert!(screen.contains("1 match"), "{screen}");
        assert!(
            !screen.contains("write the docs"),
            "the filter really filters:\n{screen}"
        );
    }

    #[test]
    fn an_open_but_empty_filter_says_to_type_rather_than_claiming_a_count() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.go(Workspace::Fleet);
        let list = a.list_mut(Workspace::Fleet);
        list.filter = Some(String::new());
        list.editing_filter = true;
        assert!(rendered(&a, 110, 24).contains("type to filter"));
    }

    // ---- team ----

    #[test]
    fn the_team_screen_lists_members_with_their_harness_and_status() {
        let mut a = app();
        a.team = Some("crew".into());
        a.members = vec![
            member("lead", HarnessKind::ClaudeCode, MemberStatus::Busy),
            member("scout", HarnessKind::Agy, MemberStatus::Ready),
        ];
        a.go(Workspace::Team);
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
        a.team = Some("crew".into());
        a.members = vec![member("scout", HarnessKind::OpenCode, MemberStatus::Busy)];
        a.tasks = vec![
            task("t1", "port the parser", Some("scout"), "open"),
            task("t2", "write the docs", None, "open"),
            task("t3", "ship it", Some("lead"), "done"),
        ];
        a.go(Workspace::Team);
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("board"));
        assert!(screen.contains("port the parser"));
        assert!(screen.contains("(scout)"), "a claimed task names its owner");
        assert!(screen.contains("write the docs"));
        assert!(screen.contains('✓'), "a done task is marked");
        assert!(screen.contains('○'), "an unclaimed task is marked");
    }

    /// Without a team the screen must explain itself rather than show an empty
    /// box that reads as a bug.
    #[test]
    fn the_team_screen_says_so_when_there_is_no_team() {
        let mut a = app();
        a.go(Workspace::Team);
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("no team"), "got:\n{screen}");
    }

    #[test]
    fn a_team_with_no_members_yet_says_so() {
        let mut a = app();
        a.team = Some("crew".into());
        a.go(Workspace::Team);
        assert!(rendered(&a, 100, 20).contains("no members yet"));
    }

    #[test]
    fn the_team_screen_is_hidden_unless_you_are_on_it() {
        let mut a = app();
        a.team = Some("crew".into());
        a.members = vec![member("scout", HarnessKind::OpenCode, MemberStatus::Ready)];
        assert!(!rendered(&a, 100, 20).contains("scout"));

        a.go(Workspace::Team);
        assert!(rendered(&a, 100, 20).contains("scout"));
    }

    #[test]
    fn the_team_screen_offers_its_keys_too() {
        let mut a = app();
        a.team = Some("crew".into());
        a.tasks = vec![task("t1", "port the parser", None, "open")];
        a.go(Workspace::Team);
        let screen = rendered(&a, 100, 20);
        assert!(screen.contains("mark done"), "{screen}");
        assert!(
            screen.contains('▸'),
            "the selection must be visible:\n{screen}"
        );
    }

    #[test]
    fn an_empty_board_says_how_to_add_to_it() {
        let mut a = app();
        a.team = Some("crew".into());
        a.go(Workspace::Team);
        assert!(rendered(&a, 100, 20).contains("/todo"));
    }

    // ---- sizes ----

    /// A terminal can be dragged to almost nothing; the layout must survive it,
    /// on every screen.
    #[test]
    fn no_screen_panics_at_an_absurd_size() {
        for ws in Workspace::ALL {
            let mut a = memory_app();
            a.graph = GraphView::new("prefers-spec-first");
            a.go(ws);
            for (w, h) in [(20, 5), (10, 4), (12, 5), (80, 6), (80, 24), (200, 60)] {
                let _ = rendered(&a, w, h);
            }
        }
    }

    #[test]
    fn no_overlay_panics_at_an_absurd_size() {
        for overlay in [
            Overlay::WhichKey,
            Overlay::WhichKeyNew,
            Overlay::Keymap,
            Overlay::Confirm {
                verb: "delete".into(),
                what: "a-very-long-name-indeed".into(),
            },
            Overlay::Prompt {
                label: "schedule".into(),
                value: "0 2 * * *".into(),
                intent: PromptIntent::New(Workspace::Schedules),
            },
        ] {
            let mut a = schedules_app();
            a.overlay = overlay;
            for (w, h) in [(10, 4), (20, 5), (80, 24), (200, 60)] {
                let _ = rendered(&a, w, h);
            }
        }
    }

    /// 100×30 is the design target; 80×24 is the contract. Nothing may clip and
    /// nothing may scroll sideways.
    #[test]
    fn nothing_overflows_eighty_columns_on_any_screen() {
        for ws in Workspace::ALL {
            let mut a = memory_app();
            a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
            a.schedules = schedules_app().schedules;
            a.goals = goals_app().goals;
            a.hooks = hooks_app().hooks;
            a.activity = activity_app().activity;
            a.tasks = vec![task("t1", "port the parser", Some("scout"), "open")];
            a.team = Some("crew".into());
            a.graph = GraphView::new("prefers-spec-first");
            a.go(ws);
            let screen = rendered(&a, 80, 24);
            for line in screen.lines() {
                assert!(
                    line.chars().count() <= 80,
                    "{ws:?} overflowed 80 columns: {line}"
                );
            }
        }
    }

    /// Regression: the panel sized itself to a comfortable minimum width
    /// regardless of the terminal, so a narrow window produced a rect wider
    /// than the buffer it was drawn into.
    #[test]
    fn a_long_fleet_never_outgrows_a_narrow_terminal() {
        let mut a = app();
        a.agents = (0..30)
            .map(|i| agent_line(&format!("id{i}"), &format!("agent {i}"), "running"))
            .collect();
        a.go(Workspace::Fleet);
        for (w, h) in [(10, 4), (12, 5), (18, 6), (21, 7), (40, 8)] {
            let _ = rendered(&a, w, h);
        }
    }

    #[test]
    fn the_team_screen_fits_a_small_terminal() {
        let mut a = app();
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
        a.go(Workspace::Team);
        let _ = rendered(&a, 30, 8);
        let _ = rendered(&a, 12, 5);
    }

    // ---- wrapping and small helpers ----

    /// Regression: agents print code, and splitting on spaces threw the leading
    /// ones away — `def f():` and its body came out in one column.
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
    fn the_visible_window_keeps_the_cursor_inside_it() {
        assert_eq!(window_start(0, 10, 5), 0, "a short list never scrolls");
        assert_eq!(window_start(0, 10, 40), 0);
        assert_eq!(window_start(20, 10, 40), 15, "centred on the cursor");
        assert_eq!(window_start(39, 10, 40), 30, "clamped to the last page");
    }

    /// A run-history strip is read left to right in time, so it always ends on
    /// the most recent run.
    #[test]
    fn a_run_history_strip_shows_the_last_seven_ending_on_the_newest() {
        let history: Vec<Outcome> = (0..10)
            .map(|i| if i == 9 { Outcome::Failed } else { Outcome::Ok })
            .collect();
        let span = strip_span(&history);
        assert_eq!(span.content.chars().count(), 7);
        assert!(span.content.ends_with('✗'), "{}", span.content);
    }

    #[test]
    fn a_progress_bar_reads_without_colour() {
        assert_eq!(bar(0, 10), "░░░░░░░░░░");
        assert_eq!(bar(50, 10), "▓▓▓▓▓░░░░░");
        assert_eq!(bar(100, 10), "▓▓▓▓▓▓▓▓▓▓");
        assert_eq!(bar(250, 10), "▓▓▓▓▓▓▓▓▓▓", "a runaway percent still fits");
    }

    #[test]
    fn a_harness_code_is_short_enough_for_a_column() {
        assert_eq!(code("Claude Code"), "cc");
        assert_eq!(code("OpenCode"), "oc");
        assert_eq!(code("Antigravity"), "agy");
        assert_eq!(code("something new"), "?");
    }

    #[test]
    fn a_state_glyph_exists_for_every_run_status() {
        for status in ["running", "completed", "failed", "killed", "queued"] {
            assert_eq!(run_glyph(status).chars().count(), 1, "{status}");
        }
    }

    #[test]
    fn cutting_a_long_string_says_it_was_cut() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("a-very-long-name", 8), "a-very-…");
    }
}
