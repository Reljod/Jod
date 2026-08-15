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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use jod_core::team::MemberStatus;
use jod_core::PermissionPolicy;

use super::app::{
    absolute, short_duration, since, until, App, Dictation, Entry, JobState, Overlay,
};
use super::data::{Outcome, Source};
use super::diff;
use super::todo;
use super::graph::{self, Direction as EdgeDirection};
use super::keys;
use super::fleet;
use super::mention;
use super::picker;
use super::rail;
use super::secret;
use super::text;
use super::traffic;
use super::workspace::Workspace;
use jod_core::cards::{Card, CardKind, Delivery, Importance, Sort, Status};
use jod_core::projects::How;

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
    // The decision rail comes off the left of whatever is left, before the
    // chat/workspace branch below, because it is drawn beside both: a card can
    // arrive while you are reading the fleet, and a rail that only existed on
    // the chat screen would be a rail you have to navigate to.
    let (rail, body) = rail_beside(app, body);

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
            // The mascot goes on before the conversation is laid out, and takes
            // its rows off the top: the transcript pages by what is left, so a
            // band drawn over a viewport already measured would hide the last
            // lines of the conversation underneath itself.
            let column = draw_header(f, app, column);
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(composer(app, column))])
                .split(column);
            input = parts[1];
            let height = draw_transcript(f, app, parts[0]);
            draw_input(f, app, input);
            height
        }
    } else {
        draw_workspace(f, app, body);
        // A workspace list pages by its own height, not the transcript's.
        body.height.saturating_sub(4).max(1) as usize
    };

    if let Some(rail) = rail {
        draw_rail(f, app, rail);
    }
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
    draw_mention(f, app, input);
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

/// The tallest the composer gets: rows of text, before its two borders.
///
/// It grows instead of scrolling sideways, so the cap is only there to stop a
/// pasted essay from taking the whole screen and leaving nowhere to read the
/// reply. Six rows is around five hundred characters at the measure — longer
/// than any prompt anyone types by hand — and past it the box scrolls a line at
/// a time to keep the caret in view.
const COMPOSER_ROWS: u16 = 6;

/// The columns a composer `width` wide has left for text, once its borders and
/// its caret have been paid for.
fn composer_field(width: u16) -> usize {
    let inner = width.saturating_sub(2).max(1) as usize;
    let gutter = if inner >= CARET.chars().count() + 8 {
        CARET.chars().count()
    } else {
        0
    };
    inner.saturating_sub(gutter).max(1)
}

/// How many rows of text it takes to show `chars` characters with the caret at
/// `col`, wrapped into a field `field` columns wide.
///
/// The caret has its own term because it sits one past the last character: type
/// exactly to the end of a row and the caret belongs on the next one, which has
/// to exist before it can be drawn there.
fn composer_lines(chars: usize, col: usize, field: usize) -> usize {
    chars.div_ceil(field).max(1).max(col / field + 1)
}

/// How tall the composer's box is, borders included.
///
/// The box is the same span as the transcript above it — `MEASURE` caps
/// *reading*, and a `you` box wider than the `jod` box reads as a rendering
/// slip rather than a choice — so the room a long prompt needs is found
/// downwards rather than sideways. That is what keeps BUG-12 fixed: the field
/// no longer scrolls out from under what you typed, it wraps, so the whole of a
/// 200-character delegation prompt is on screen before Ctrl-B spends money on
/// it.
///
/// Never so tall that the conversation is squeezed out: the transcript keeps
/// its borders and three lines whatever is being typed.
fn composer(app: &App, column: Rect) -> u16 {
    let lines = composer_lines(
        app.input.chars().count(),
        app.cursor_column(),
        composer_field(column.width),
    );
    let room = column.height.saturating_sub(5).max(3);
    (lines as u16 + 2).clamp(3, COMPOSER_ROWS + 2).min(room)
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

// ---- the decision rail -------------------------------------------------

/// The rail's width while it is showing two-line cards.
///
/// Thirty-four rather than thirty: a card's second line carries an id, the word
/// `blocked` and `answered, queued`, and at thirty the last of those truncated
/// to `answered, queue…` — which is the one fact on the card the reader most
/// needs whole.
const RAIL: u16 = 34;

/// ...and while one card is expanded.
///
/// Wider because an expanded card carries a wrapped body, a numbered option
/// list and a provenance block, and thirty columns of prose is a column of
/// syllables.
const RAIL_WIDE: u16 = 58;

/// The narrowest body that can hold the rail *beside* the content.
///
/// Below it the rail becomes a single line rather than being squeezed, which is
/// what Reljod chose when the question was put: taking thirty columns off an
/// eighty-column terminal leaves neither a readable rail nor a readable chat,
/// and a rail you cannot read is worse than one sentence saying how many cards
/// are waiting and which key opens them.
const RAIL_BESIDE: u16 = 84;

/// A collapsed card: a border, two lines of card, a border.
const CARD_HEIGHT: u16 = 4;

/// Where the rail goes, and what is left for everything else.
///
/// Three outcomes, and the middle one is the interesting one: hidden, a column
/// down the left, or one line across the top when the terminal is too narrow
/// for a column.
fn rail_beside(app: &App, area: Rect) -> (Option<Rect>, Rect) {
    if !app.rail.shown {
        return (None, area);
    }
    let want = if app.rail.expanded { RAIL_WIDE } else { RAIL };
    // Never past half the body. A sidebar wider than what it sits beside has
    // stopped being a sidebar, and the expanded card is the case that would do
    // it — fifty-eight columns is most of a hundred-column terminal.
    let width = want.min(area.width / 2);
    if area.width < RAIL_BESIDE || width < RAIL || area.height < 4 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        return (Some(rows[0]), rows[1]);
    }
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(width), Constraint::Min(20)])
        .split(area);
    // One column of air between the rail and what it sits beside, so a card's
    // border does not touch the chat's first character.
    let body = Rect {
        x: halves[1].x + 1,
        width: halves[1].width.saturating_sub(1),
        ..halves[1]
    };
    (Some(halves[0]), body)
}

fn draw_rail(f: &mut Frame, app: &App, area: Rect) {
    if area.height <= 1 {
        return draw_rail_summary(f, app, area);
    }
    match app.selected_card().filter(|_| app.rail.expanded) {
        Some(card) => draw_card(f, app, card, area),
        None => draw_rail_stack(f, app, area),
    }
}

/// The rail on a terminal too narrow to hold it.
///
/// It owes the reader exactly two things — that something is blocked, and the
/// key that opens the rail — and it must not pretend to be the rail. Hence a
/// bar glyph and a sentence rather than a squeezed card.
fn draw_rail_summary(f: &mut Frame, app: &App, area: Rect) {
    let blocking = app.cards.iter().any(|c| c.blocking && c.is_open());
    let text = rail::summary(&app.cards);
    let line = if text.is_empty() {
        Line::from(Span::styled(" ▌ rail · nothing waiting".to_string(), fg(MUTED)))
    } else {
        Line::from(vec![
            Span::styled(" ▌ ".to_string(), fg(if blocking { BAD } else { USER })),
            Span::styled(
                cut(&text, area.width.saturating_sub(3) as usize),
                if blocking { bold(BAD) } else { fg(MUTED) },
            ),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The stack: a header, and one bordered two-line card per row.
fn draw_rail_stack(f: &mut Frame, app: &App, area: Rect) {
    let ids = app.card_ids();
    let selected = app.rail.index(&ids);

    let mut head = vec![rail_header(app)];
    // The sort and the two filters are printed whenever they are in force, and
    // whenever the rail has the keyboard. A stack silently showing a subset is
    // a stack whose missing card reads as a card that was never raised.
    if let Some(line) = rail_settings(app) {
        head.push(line);
    }
    if let Some(line) = rail_filter_line(app) {
        head.push(line);
    }
    let used = (head.len() as u16).min(area.height);
    f.render_widget(
        Paragraph::new(head),
        Rect {
            height: used,
            ..area
        },
    );

    let rest = Rect {
        y: area.y + used,
        height: area.height.saturating_sub(used),
        ..area
    };
    if rest.height == 0 {
        return;
    }
    if app.cards.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(empty_rail(app), fg(MUTED))))
                .wrap(Wrap { trim: true }),
            rest,
        );
        return;
    }

    let rows = (rest.height / CARD_HEIGHT).max(1) as usize;
    let first = window_start(selected, rows, app.cards.len());
    for (slot, (at, card)) in app.cards.iter().enumerate().skip(first).take(rows).enumerate() {
        let y = rest.y + slot as u16 * CARD_HEIGHT;
        if y + CARD_HEIGHT > rest.y + rest.height {
            break;
        }
        let box_ = Rect {
            y,
            height: CARD_HEIGHT,
            ..rest
        };
        let here = at == selected;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(card_border(card))
            // The cursor rides the top border rather than a column of its own:
            // thirty-two columns has none to spare, and a marker inside the box
            // would cost every card two cells of title to mark one card.
            .title(if here { " ▸ " } else { "" });
        f.render_widget(
            Paragraph::new(card_lines(
                card,
                box_.width.saturating_sub(2) as usize,
                here,
                app.rail.cascade,
            ))
            .block(block),
            box_,
        );
    }
}

/// The two lines of a collapsed card: what it says, and what state it is in.
///
/// `cascading` decides whether the second line has to name the session. With
/// the subtree scope on, the rail holds cards from agents all over the fleet
/// and answering writes against one specific one, so the provenance is not
/// optional — it is what stops "answer the top one" being a coin flip about
/// which agent gets unblocked. With the scope narrowed to one conversation
/// every card came from the same place, and printing it would spend a third of
/// a thirty-four column line saying so.
fn card_lines(card: &Card, width: usize, here: bool, cascading: bool) -> Vec<Line<'static>> {
    let title = Line::from(vec![
        Span::styled(
            format!("{} ", rail::kind_glyph(card.kind)),
            fg(card_colour(card)),
        ),
        Span::styled(
            cut(&card.title, width.saturating_sub(2)),
            if here { bold(AGENT) } else { fg(AGENT) },
        ),
    ]);

    // The session leads the line when cascading, ahead of the id: which agent
    // is asking outranks which card number it is.
    let stamp = if cascading {
        match rail::work_tag(card) {
            Some(work) => format!("{work}/{} ", rail::raised_by(card)),
            None => format!("{} ", rail::raised_by(card)),
        }
    } else {
        format!("#{} ", card.id)
    };
    let mut spans = vec![Span::styled(stamp.clone(), fg(MUTED))];
    let mut used = stamp.chars().count();
    // The literal word, beside the coloured border. Colour is never the only
    // channel in this program, and on the one card that stopped a run that
    // rule is not a nicety.
    if card.blocking {
        spans.push(Span::styled(format!("{} ", rail::BLOCKED), bold(BAD)));
        used += rail::BLOCKED.chars().count() + 1;
    }
    let mut tail: Vec<String> = Vec::new();
    // First, ahead of the kind, because it is the fact a reader of an answered
    // card actually needs — `status` is what the human did and `delivery` is
    // whether the agent has heard, and the second is the one that decides
    // whether there is anything left to wait for. Last in the line is where a
    // narrow rail truncates.
    if let Some(note) = rail::delivery_note(card) {
        tail.push(note.to_string());
    }
    tail.push(card.kind.as_str().to_string());
    if card.importance != Importance::Normal {
        tail.push(card.importance.as_str().to_string());
    }
    spans.push(Span::styled(
        cut(&tail.join(" · "), width.saturating_sub(used)),
        fg(MUTED),
    ));
    vec![title, Line::from(spans)]
}

fn rail_header(app: &App) -> Line<'static> {
    let blocking = app
        .cards
        .iter()
        .filter(|c| c.blocking && c.is_open())
        .count();
    let mut spans = vec![Span::styled(
        " rail".to_string(),
        if app.rail.focused {
            bold(USER)
        } else {
            fg(MUTED)
        },
    )];
    spans.push(Span::styled(
        format!(
            " · {} {}",
            app.cards.len(),
            app.rail.stack_now().as_str()
        ),
        fg(MUTED),
    ));
    // The scope rides the always-drawn header rather than the settings line,
    // which is only drawn when something is non-default. A rail narrowed to one
    // conversation and a fleet that has gone quiet look identical, and the
    // difference between them is the whole reason the orchestrator's rail
    // exists — so it is never something the reader has to infer.
    spans.push(Span::styled(
        if app.rail.cascade {
            " · subtree".to_string()
        } else {
            " · here".to_string()
        },
        fg(MUTED),
    ));
    if blocking > 0 {
        spans.push(Span::styled(
            format!(" · {blocking} {}", rail::BLOCKED),
            bold(BAD),
        ));
    }
    Line::from(spans)
}

/// The sort and the kind filter, when either is worth saying.
fn rail_settings(app: &App) -> Option<Line<'static>> {
    let sort = app.rail.sort_now();
    let kind = app.rail.kind_now();
    let plain = sort == Sort::default() && kind.is_none();
    if plain && !app.rail.focused {
        return None;
    }
    let kind = kind.map(|k| k.as_str()).unwrap_or("any");
    // The scope is on the header, which is always drawn — see `rail_header`.
    Some(Line::from(Span::styled(
        format!(" S {} · f {kind}", sort.as_str()),
        fg(MUTED),
    )))
}

fn rail_filter_line(app: &App) -> Option<Line<'static>> {
    let filter = app.rail.filter.as_ref()?;
    let cursor = if app.rail.editing_filter { "▏" } else { "" };
    let what = if app.rail.filtering() {
        format!("  {} match", app.cards.len())
    } else {
        "  type to search".to_string()
    };
    Some(Line::from(vec![
        Span::styled(format!(" /{filter}{cursor}"), fg(USER)),
        Span::styled(what, fg(MUTED)),
    ]))
}

/// What an empty rail says, which depends on *why* it is empty — a filter that
/// matched nothing and a fleet that has asked nothing are different answers.
fn empty_rail(app: &App) -> String {
    if app.rail.filtering() {
        let needle = app.rail.filter.clone().unwrap_or_default();
        return format!("  nothing matches “{}”", needle.trim());
    }
    match app.rail.stack_now() {
        Status::Open => "  nothing waiting — no agent has asked anything".to_string(),
        Status::Answered => "  nothing answered yet — t cycles back to the open ones".to_string(),
        Status::Dismissed => "  nothing dismissed".to_string(),
    }
}

/// One card in full: what it says, what it offers, and who raised it.
fn draw_card(f: &mut Frame, app: &App, card: &Card, area: Rect) {
    let width = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(
            format!(" {} ", rail::kind_glyph(card.kind)),
            fg(card_colour(card)),
        ),
        Span::styled(cut(&card.title, width), bold(AGENT)),
    ])];

    let mut facts = vec![card.kind.as_str().to_string(), card.importance.as_str().to_string()];
    if card.blocking {
        facts.insert(0, rail::BLOCKED.to_string());
    }
    lines.push(Line::from(Span::styled(
        format!("   {}", facts.join(" · ")),
        if card.blocking { bold(BAD) } else { fg(MUTED) },
    )));

    if !card.body.trim().is_empty() {
        lines.push(Line::from(""));
        for wrapped in wrap(&card.body, width, 3) {
            lines.push(Line::from(Span::styled(wrapped, fg(AGENT))));
        }
    }

    // A secret card explains where the value will live before `a` is ever
    // pressed. E3.S4 asks for this on the card and not only in the field,
    // because the card is what somebody reads while deciding whether to hand
    // over a production token at all.
    if card.kind == CardKind::Secret {
        let scope = card
            .secret_scope
            .as_deref()
            .map(jod_core::secrets::Scope::parse)
            .unwrap_or_default();
        let name = card.secret_name.as_deref().unwrap_or(&card.title);
        lines.push(Line::from(""));
        for said in secret::destination(name, scope) {
            lines.push(Line::from(Span::styled(format!("   {said}"), fg(MUTED))));
        }
        // Once it is stored the card carries a name and a scope and nothing
        // else — `card.answer` holds `secret::stored_summary`, never a value.
        if card.status == Status::Open {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "   a — type the value; it is masked and never echoed".to_string(),
                fg(WARN),
            )));
        }
    }

    // Numbered because the digits are the keys: the label on screen *is* the
    // keystroke, so nobody has to count rows to find out what `2` does.
    if !card.options.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "   options — press the digit".to_string(),
            fg(MUTED),
        )));
        for (at, option) in card.options.iter().take(9).enumerate() {
            let picked = card.chosen.as_deref() == Some(option.as_str());
            lines.push(Line::from(vec![
                Span::styled(format!("   {} ", at + 1), bold(USER)),
                Span::styled(
                    cut(option, width.saturating_sub(2)),
                    if picked { bold(GOOD) } else { fg(AGENT) },
                ),
                Span::styled(
                    if picked { "  ← chosen" } else { "" }.to_string(),
                    fg(GOOD),
                ),
            ]));
        }
    }

    if let Some(answer) = card.answer.as_ref().filter(|a| !a.trim().is_empty()) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("   answered".to_string(), fg(MUTED))));
        for wrapped in wrap(answer, width, 3) {
            lines.push(Line::from(Span::styled(wrapped, fg(GOOD))));
        }
    }

    lines.push(Line::from(""));
    lines.push(rule(area.width as usize));
    // Provenance. "Which agent asked me this" is the first thing a blocking
    // card provokes somebody to ask, and an answer landing on the wrong session
    // is the failure it prevents.
    lines.push(detail(
        "raised by",
        &card
            .run_id
            .as_deref()
            .map(short)
            .unwrap_or_else(|| "—".to_string()),
    ));
    lines.push(detail("session", &short(&card.conversation_id)));
    if let Some(work) = &card.work_id {
        lines.push(detail("work", &short(work)));
    }
    lines.push(detail("source", card.source.as_str()));
    lines.push(detail("state", &card_state(card)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(card_border(card))
        .title(format!(" card #{} ", card.id))
        .title_bottom(fit_verbs(&keys::rail_footer(), area.width));
    let _ = app;
    // Wrapped, because the state sentence is the longest line here and it is
    // the one that must not be clipped: "answered, queued — the agent is told
    // at the end of the turn in flight" cut at fifty columns says "answered,
    // queued — the agent is told at the", which reads as a promise about now.
    // `trim: false` keeps the indentation the lines above were built with.
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The card's state as a sentence, which is where D2 is either honoured or
/// broken.
///
/// An answered card is *queued*, and saying only "answered" would send the
/// reader back to the transcript to watch for a change that is not due yet.
/// They would conclude the key did not work and answer it again.
fn card_state(card: &Card) -> String {
    match (card.status, card.delivery) {
        (Status::Open, _) if card.blocking => {
            "blocked — the run is waiting on this".to_string()
        }
        (Status::Open, _) => "open".to_string(),
        (Status::Answered, Delivery::Queued) => {
            "answered, queued — the agent is told at the end of the turn in flight".to_string()
        }
        (Status::Answered, Delivery::Delivered) => {
            "answered, and the agent has been told".to_string()
        }
        (Status::Answered, Delivery::Undeliverable) => {
            "answered, but the session ended before it could be told".to_string()
        }
        (Status::Answered, Delivery::None) => "answered".to_string(),
        (Status::Dismissed, _) => "dismissed — the agent is told nothing".to_string(),
    }
}

/// A card's colour: blocking first, then kind.
///
/// Blocking wins over kind because the two answer different questions and only
/// one of them is urgent — a blocking secret request and a blocking question
/// are the same shade of "this run has stopped".
fn card_colour(card: &Card) -> Color {
    if card.blocking {
        return BAD;
    }
    match card.kind {
        CardKind::Decision => USER,
        CardKind::Question => WARN,
        CardKind::Secret => Color::Magenta,
    }
}

/// The border: kind for the colour, importance for the weight.
fn card_border(card: &Card) -> Style {
    if card.blocking {
        return bold(BAD);
    }
    match card.importance {
        Importance::High => bold(card_colour(card)),
        Importance::Normal => fg(card_colour(card)),
        // Dimmed rather than tinted. The glyph still says which kind it is, and
        // a low-importance card's whole job is to be there without competing
        // with the one above it.
        Importance::Low => fg(MUTED),
    }
}

// ---- the `@` picker ----------------------------------------------------

/// The mention popup, drawn under the `@` that opened it.
///
/// Under the cursor rather than in a corner, because the point of an inline
/// picker is that you never leave the sentence — which is also why Jod ranks
/// in-process instead of shelling out to `fzf`, a program that owns a whole
/// terminal and could not draw this at all. See decision D1.
fn draw_mention(f: &mut Frame, app: &App, input: Rect) {
    let Some(popup) = &app.mention else {
        return;
    };
    if app.workspace != Workspace::Chat || input.width < 8 {
        return;
    }

    let w = 56u16.min(input.width);
    // The borders eat a column either side, and what is left is what a row has
    // to fit into. Known before the rows are built, because each row is fitted
    // to it rather than clipped by the widget afterwards.
    let inner = w.saturating_sub(2) as usize;

    // Zero roots is a message, not an empty list: an empty list reads as "no
    // matches" and invites another keystroke, and there is no keystroke that
    // would help.
    let rows: Vec<ListItem> = if !popup.rooted {
        vec![ListItem::new(Line::from(Span::styled(
            cut(mention::NO_ROOTS, input.width.saturating_sub(4) as usize),
            fg(WARN),
        )))]
    } else if popup.rows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  no match".to_string(),
            fg(MUTED),
        )))]
    } else {
        popup
            .rows
            .iter()
            .enumerate()
            .map(|(at, row)| ListItem::new(mention_line(row, at == popup.selected, inner)))
            .collect()
    };

    let h = ((rows.len() + 2) as u16)
        .min(input.y.saturating_sub(1))
        .max(3);
    // Anchored on the `@` itself, then pulled back inside the box: a popup that
    // hangs off the right edge of the terminal is drawn over nothing. The
    // column is the one the `@` is drawn in, not how far into the prompt it is
    // — those part company as soon as the line wraps onto a second row.
    let col = (app.input[..popup.at.min(app.input.len())].chars().count()
        % composer_field(input.width)) as u16;
    let x = (input.x + 1 + CARET.chars().count() as u16 + col).min(
        input
            .x
            .saturating_add(input.width)
            .saturating_sub(w)
            .max(input.x),
    );
    let panel = Rect {
        x,
        y: input.y.saturating_sub(h),
        width: w,
        height: h,
    };

    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" @ · ⏎ inserts · ↑↓ choose · Esc keeps what you typed "),
        ),
        panel,
    );
}

/// One offered path, with the characters the query matched picked out, fitted
/// into `width` columns.
///
/// The highlight is the whole reason [`jod_core::rank::Match`] carries
/// positions rather than only a score: a fuzzy list you cannot read the match
/// in is a list you stop trusting.
///
/// `width` is the whole row, marker and root label included. Left to the
/// widget, the row was hard-clipped on the right — the end of a path is the
/// filename, so six different files came out as six identical rows. So it is
/// fitted here instead, by [`text::elide_left`], which drops the shared head
/// and keeps the part that tells them apart.
fn mention_line(row: &mention::Row, here: bool, width: usize) -> Line<'static> {
    let (mark, base) = if here {
        ("▸ ", fg(AGENT))
    } else {
        ("  ", fg(MUTED))
    };
    let mut spans = vec![Span::styled(mark.to_string(), bold(USER))];
    let mut spent = mark.chars().count();
    if let Some(label) = &row.label {
        let qualified = format!("{label}/");
        spent += qualified.chars().count();
        spans.push(Span::styled(qualified, fg(MUTED)));
    }
    // The label is not elided: it says which repository the row came from, and
    // a row that cannot say that is worse than a short one. It is short by
    // construction anyway — a root's own name.
    let fitted = text::elide_left(&row.path, width.saturating_sub(spent));
    if fitted.is_elided() {
        spans.push(Span::styled(text::ELLIPSIS.to_string(), fg(MUTED)));
    }
    // Byte offsets from `Match::positions`, moved into the fitted string —
    // dropping bytes off the front moved every one of them, and a position
    // that did not survive is dropped rather than guessed at.
    //
    // The offsets are ascending and always land on a character boundary —
    // `rank` takes a byte fast path only for an ASCII query and a char path
    // otherwise, precisely so this loop does not have to check. A guard here
    // would be a second, weaker copy of a guarantee core already makes, and it
    // would turn a bug in the matcher into a row that silently lost its
    // highlight instead of a panic naming the offset.
    let shown = &fitted.text[text::ELLIPSIS.len() * usize::from(fitted.is_elided())..];
    let mut at = 0usize;
    for hit in row.positions.iter().copied() {
        let Some(hit) = fitted
            .shift(hit)
            .map(|at| at - text::ELLIPSIS.len() * usize::from(fitted.is_elided()))
        else {
            continue;
        };
        if hit > at {
            spans.push(Span::styled(shown[at..hit].to_string(), base));
        }
        let end = hit + shown[hit..].chars().next().map_or(0, char::len_utf8);
        spans.push(Span::styled(shown[hit..end].to_string(), bold(USER)));
        at = end;
    }
    if at < shown.len() {
        spans.push(Span::styled(shown[at..].to_string(), base));
    }
    Line::from(spans)
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

// ---- the mascot ---------------------------------------------------------

/// The mascot's palette, and the one place in this file where colour is
/// decoration rather than state.
///
/// Palette indices rather than the sixteen named colours, because orange is not
/// one of the sixteen: `LightRed` is what a terminal calls a brighter red, and
/// an orange head with red highlights needs two warms that are actually
/// different hues. These are the xterm-256 slots, the one extension every
/// terminal anybody runs a TUI in has had for twenty years.
///
/// The head is *filled* rather than outlined, which is what lets the eyes and
/// teeth be white: they sit on a painted orange face rather than on the
/// terminal's own background, so they read on a light theme as well as a dark
/// one. The cost is paid under `NO_COLOR`, where the fill flattens to one
/// silhouette — so every feature is cut with a half-block as well as coloured.
/// The crown crenellates, the pupils, nose and fangs are notches out of an
/// otherwise solid head, and the chin is a row of points: with the colour off
/// the drawing is still a spiky head over a small body.
const MANE: Color = Color::Indexed(208);
const SPIKE: Color = Color::Indexed(196);
const FACE: Color = Color::Indexed(215);
const EYE: Color = Color::Indexed(231);
const PUPIL: Color = Color::Indexed(233);
const MAW: Color = Color::Indexed(88);
const FUR: Color = Color::Indexed(41);

/// Columns of air between the lion and the lettering it stands next to.
const LOCKUP_GAP: u16 = 2;

/// One pose of the mascot: a grid of glyphs, and a stencil that says what
/// colour each of them is.
///
/// Two parallel grids rather than a colour attached to each glyph, because on a
/// filled drawing almost every cell is `█` — the character cannot say whether
/// it is mane, cheek, eye or nose, only its position can. The stencil letters
/// are `f` for the mane, `r` for the red spike tips, `c` for the face, `n` for
/// the nose, `w` for the white of an eye or a tooth, `y` for a pupil, `m` for
/// the inside of an open mouth, `g` for the green body, and `.` for a cell that
/// takes no colour at all.
struct Pose {
    art: &'static [&'static str],
    ink: &'static [&'static str],
}

impl Pose {
    /// Every row is this wide — a test holds it — so row zero can answer for
    /// all of them.
    fn width(&self) -> u16 {
        self.art[0].chars().count() as u16
    }

    fn height(&self) -> u16 {
        self.art.len() as u16
    }
}

/// Jod's mascot: a small green lion, sitting, under a spiky mane several sizes
/// too big for it.
///
/// Drawn in the same block characters as the wordmark so the two read as one
/// lockup rather than as clip-art dropped beside a logo. Only block elements
/// appear in the art — box-drawing and geometric-shape codepoints are East
/// Asian Ambiguous, and a terminal that renders one of them double-width would
/// tear a hole in a picture whose rows all have to be the same width.
///
/// **Four rows, against the wordmark's five.** The lion is the companion mark
/// and the lettering is the name, so the lion is the smaller of the two: at any
/// larger it stops standing *beside* the word and starts standing *over* it. A
/// row of the terminal is worth two columns, so four rows is a drawing eight
/// units tall — enough for a crown, a pair of eyes, a muzzle and a body, and
/// nothing spare. Every feature below is therefore one row or half of one.
///
/// The mane alternates a tall red spike with a short orange one, because a
/// single row of even points reads as a saw blade and it is the *ragged* edge
/// that reads as fur. The eyes are three columns wide out of eleven — far too
/// big for a real lion, which is the entire point — and the body is a green nub
/// with a tail, because a body drawn to scale reads as a lion and a body drawn
/// far too small reads as a cub, which is the one of the two that is cute.
///
/// The twelfth column is blank in every pose: it is where the scratching paw
/// goes, and a grid that grew a column when the paw came out would shove the
/// wordmark sideways four times a second.
const SITTING: Pose = Pose {
    art: &[
        "█▄█▄█▄█▄█▄█ ",
        "███▀███▀███ ",
        "████▄▀▄████ ",
        "█▀█▀▟█▙▗▄▖█ ",
    ],
    ink: &[
        "rfrfrfrfrfr.",
        "ffwywcwywff.",
        "ffccwnwccff.",
        "rfrfggggggr.",
    ],
};

/// Eyes shut. At this size a closed eye has nowhere to put a lid, so the whites
/// and the pupils simply go and the face closes over them — which is what a
/// blink looks like once a head is four rows tall. Two ticks out of forty-eight
/// is half a second: long enough to see, short enough not to look like the
/// mascot has fallen asleep.
const BLINKING: Pose = Pose {
    art: &[
        "█▄█▄█▄█▄█▄█ ",
        "███████████ ",
        "████▄▀▄████ ",
        "█▀█▀▟█▙▗▄▖█ ",
    ],
    ink: &[
        "rfrfrfrfrfr.",
        "ffcccccccff.",
        "ffccwnwccff.",
        "rfrfggggggr.",
    ],
};

/// Mid-roar: the mane puffs — every notch in the crown fills in, so the head
/// gains half a row of height without gaining a row of grid — the eyes screw
/// shut, and the muzzle opens onto a dark throat with a fang at each side.
const ROARING: Pose = Pose {
    art: &[
        "███████████ ",
        "███████████ ",
        "███▄███▄███ ",
        "█▀█▀▟█▙▗▄▖█ ",
    ],
    ink: &[
        "rfrfrfrfrfr.",
        "ffcccccccff.",
        "ffcwmmmwcff.",
        "rfrfggggggr.",
    ],
};

/// Scratching an itch, paw down at the jaw, with the eye on that side screwed
/// shut — the squint is what says the paw belongs to this lion rather than
/// being a shape parked next to it.
const SCRATCH_LOW: Pose = Pose {
    art: &[
        "█▄█▄█▄█▄█▄█ ",
        "███▀███████ ",
        "████▄▀▄████▟",
        "█▀█▀▟█▙▗▄▖█ ",
    ],
    ink: &[
        "rfrfrfrfrfr.",
        "ffwywccccff.",
        "ffccwnwccffg",
        "rfrfggggggr.",
    ],
};

/// The same scratch, paw up at the cheek. Alternating the two is the whole
/// animation: one row of travel is all a scratch needs to read as motion.
const SCRATCH_HIGH: Pose = Pose {
    art: &[
        "█▄█▄█▄█▄█▄█ ",
        "███▀███████▟",
        "████▄▀▄████ ",
        "█▀█▀▟█▙▗▄▖█ ",
    ],
    ink: &[
        "rfrfrfrfrfr.",
        "ffwywccccffg",
        "ffccwnwccff.",
        "rfrfggggggr.",
    ],
};

/// What one stencil letter means.
fn ink(key: char) -> Style {
    match key {
        'f' => bold(MANE),
        'r' => bold(SPIKE),
        'c' => bold(FACE),
        // The nose is the mane's orange against the face's lighter one, which is
        // the only pair here that has to hold at one cell of separation.
        'n' => bold(MANE),
        'w' => bold(EYE),
        'y' => fg(PUPIL),
        'm' => fg(MAW),
        'g' => bold(FUR),
        _ => Style::default(),
    }
}

/// Colour one row of art through its stencil, one span per run of a colour.
///
/// The spans borrow out of the `'static` art rather than building strings, so
/// drawing a pose allocates one small vector per row and nothing else. Runs
/// rather than a span per cell for the same reason: a span costs a style write,
/// and thirteen of them per row times twelve rows times four frames a second is
/// a lot of escape sequences for a picture that changes colour eight times.
fn mascot_spans(art: &'static str, stencil: &str, into: &mut Vec<Span<'static>>) {
    // The ink key this run is drawn in, and the byte it started at.
    let mut run: Option<(char, usize)> = None;
    for ((at, _), key) in art.char_indices().zip(stencil.chars()) {
        match run {
            Some((was, _)) if was == key => {}
            _ => {
                if let Some((was, from)) = run {
                    into.push(Span::styled(&art[from..at], ink(was)));
                }
                run = Some((key, at));
            }
        }
    }
    if let Some((was, from)) = run {
        into.push(Span::styled(&art[from..], ink(was)));
    }
}

/// Which drawing of the mascot this tick gets.
///
/// The mascot is the only thing on a fresh screen that can say the fleet is
/// working, so it earns its place twice: while anything is running it scratches
/// at the itch, and otherwise it sits, blinks, and roars once every twelve
/// seconds. Ticks are quarter-seconds, so every number below is one.
///
/// Deliberately a pure function of the tick rather than a stored pose index.
/// The splash is not always on screen, and an index that advanced on render
/// would freeze mid-scratch the moment you opened a workspace and pick up from
/// there minutes later.
///
/// This costs nothing the TUI was not already paying: `event_loop` redraws on
/// every tick regardless, for the spinner and the elapsed clock, and ratatui
/// diffs the frame it built against the last one — so the ten seconds the
/// mascot spends sitting perfectly still write no bytes to the terminal at all.
fn mascot_pose(app: &App) -> &'static Pose {
    if app.busy || app.running() > 0 {
        return match app.tick % 8 {
            2 | 3 => &SCRATCH_HIGH,
            6 | 7 => &SITTING,
            _ => &SCRATCH_LOW,
        };
    }
    match app.tick % 48 {
        42 | 43 => &BLINKING,
        44..=47 => &ROARING,
        _ => &SITTING,
    }
}

/// The lion standing to the left of `letters`, or `None` if the two will not
/// fit side by side in `width`.
///
/// Beside rather than above, because stacking them costs the height of both and
/// this screen has an input box to seat underneath. Side by side the whole
/// lockup is only as tall as the taller half, which is the lettering.
///
/// The two are aligned on a shared ground line rather than centred on each
/// other: the lion has feet and the letters have a baseline, and a lion floated
/// half a row off the bottom reads as a sticker rather than as something
/// standing next to a word. The lettering takes the middle of whatever is left,
/// which matters only in the narrow fallback where it is a single line.
fn lockup(pose: &'static Pose, letters: &[String], width: u16) -> Option<Vec<Line<'static>>> {
    let letters_wide = letters.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    if width < pose.width() + LOCKUP_GAP + letters_wide {
        return None;
    }
    let rows = pose.height().max(letters.len() as u16);
    let lion_top = rows - pose.height();
    let letters_top = (rows - letters.len() as u16) / 2;
    Some(
        (0..rows)
            .map(|row| {
                let mut spans: Vec<Span<'static>> = Vec::with_capacity(10);
                match row
                    .checked_sub(lion_top)
                    .and_then(|i| pose.art.get(i as usize).zip(pose.ink.get(i as usize)))
                {
                    Some((art, stencil)) => mascot_spans(art, stencil, &mut spans),
                    // Above the crown of a lion shorter than the lettering.
                    None => spans.push(Span::raw(" ".repeat(pose.width() as usize))),
                }
                spans.push(Span::raw(" ".repeat(LOCKUP_GAP as usize)));
                // Padded out to the full width even where there is no letter,
                // because the caller centres the block one line at a time: rows
                // of different lengths would each be centred on their own and
                // the lion would come out with its crown offset from its chin.
                let text = row
                    .checked_sub(letters_top)
                    .and_then(|i| letters.get(i as usize))
                    .map_or("", String::as_str);
                spans.push(Span::styled(text.to_string(), bold(USER)));
                match letters_wide as usize - text.chars().count() {
                    0 => {}
                    pad => spans.push(Span::raw(" ".repeat(pad))),
                }
                Line::from(spans)
            })
            .collect(),
    )
}

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
        "jod · an orchestrator, not a chat window · Ctrl-G opens every screen",
        "jod · an orchestrator, not a chat window",
        "jod",
    ];
    LINES
        .into_iter()
        .find(|line| line.chars().count() <= width)
        .unwrap_or("jod")
}

/// The launch directory as the splash prints it, or `None` when there is none
/// to print or no room to print it.
///
/// Shares [`under_home`] and [`fit_path`] with the header band, so the two
/// screens cannot start disagreeing about how a path is written — the drift
/// that would otherwise show up as `~/Developer/Jod` on one and
/// `/Users/reljod/Developer/Jod` on the other.
fn splash_where(app: &App, width: usize) -> Option<String> {
    if app.cwd.as_os_str().is_empty() {
        return None;
    }
    let shown = under_home(
        &app.cwd,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    );
    // Nothing worth centring under a wordmark. The caption above it makes the
    // same call by having a shortest form and then giving up.
    (width >= 12).then(|| format!("▪ {}", fit_path(&shown, width.saturating_sub(2))))
}

/// Whether this counts as a new session for rendering.
///
/// It cannot be "the transcript is empty": `event_loop` pushes a hint at
/// startup and `/new` pushes "new conversation", so the transcript is never
/// literally empty and the splash would never appear at all. Nor can it be
/// "nothing but notices" — that was the first attempt, and it swallowed every
/// command whose whole answer is notices (`/root`, `/config`, `/sessions`, the
/// delegation confirmation, most errors): the splash kept the column and
/// painted over real output, so a cold session's first command rendered as
/// nothing at all.
///
/// So the test is "nothing but [`Entry::Hint`]" — the lines Jod prints on its
/// own account. That is true at startup, true again after `/new`, and false
/// the instant *anything* answers something the user did. Watching another run
/// is excluded outright: that transcript belongs to somebody else's
/// conversation, and its emptiness says the run has not spoken yet, not that
/// you are starting fresh.
fn fresh(app: &App) -> bool {
    app.watching.is_none()
        && !app
            .transcript
            .iter()
            .any(|entry| !matches!(entry, Entry::Hint(_)))
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

    // The mark, in the largest form the screen can seat: the lion beside the
    // block lettering, then the lettering on its own, then the lion beside a
    // plain "Jod AI" on a terminal too narrow for block letters at all, then
    // that name by itself. The mascot goes before the lettering does at every
    // rung, because the lettering is what says which program you launched.
    //
    // The six rows a lockup has to leave behind are the blank and the caption
    // under it, the row of air, and the three-row input box — a logo that has
    // pushed the box off the bottom has cost more than a mascot is worth.
    let pose = mascot_pose(app);
    let seats = |rows: &Vec<Line>| area.height >= rows.len() as u16 + 6;
    let lettering = || -> Vec<Line<'static>> {
        banner()
            .into_iter()
            .map(|row| Line::from(Span::styled(row, bold(USER))))
            .collect()
    };
    let mut head: Vec<Line> = lockup(pose, &banner(), area.width)
        .filter(seats)
        .or_else(|| art.then(lettering).filter(seats))
        .or_else(|| lockup(pose, &["Jod AI".to_string()], area.width).filter(seats))
        .unwrap_or_else(|| vec![Line::from(Span::styled("Jod AI", bold(USER)))]);

    if area.height >= head.len() as u16 + 5 {
        head.push(Line::from(""));
        head.push(Line::from(Span::styled(
            caption(area.width as usize),
            fg(MUTED),
        )));
    }

    // ...and where you are standing, under the caption.
    //
    // The header band says the same thing and is not on screen yet: the splash
    // is *by definition* the state where nothing has been said, which is
    // exactly when "which repository is this console pointed at" is worth
    // knowing — the answer stops being changeable the moment you type the first
    // instruction into it. Last, and dropped first on a short terminal, because
    // the wordmark is what says which program you launched.
    if area.height >= head.len() as u16 + 5 {
        if let Some(here) = splash_where(app, area.width as usize) {
            head.push(Line::from(Span::styled(here, fg(MUTED))));
        }
    }

    let head_height = head.len() as u16;

    // The box is the same shape here as it is under a conversation: the column's
    // width, and as many rows as what has been typed needs.
    let tall = composer(app, area);
    let (top, box_) = if anchored {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(tall)])
            .split(area);
        (parts[0], parts[1])
    } else {
        let block = (head_height + 1 + tall).min(area.height);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1), // air between the wordmark and the box
                Constraint::Length(tall),
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

    draw_input(f, app, box_);
    (top.height.max(1) as usize, box_)
}

// ---- the chat header ----------------------------------------------------

/// Rows the header band costs: the mascot's four, and one of air under it so
/// the transcript's top border does not land on the lion's paws.
const HEADER: u16 = 5;

/// The shortest chat column that seats the band and still leaves a
/// conversation worth reading: the band, the three-row input box, and eight
/// rows of transcript inside its own borders. Under that the conversation is
/// what the screen is for and the lion goes — the same order of sacrifice the
/// splash makes when it drops the mascot before the lettering.
const HEADER_SEATS: u16 = HEADER + 3 + 8;

/// ...and the narrowest. Fourteen of those columns are the lion and its gap,
/// and the identity line is thirty-odd before it starts eliding: below this the
/// band would cost four rows to print `Claude Code · …`.
const HEADER_FITS: u16 = 48;

/// The band over the conversation: the mascot, and four lines saying which
/// build you launched, who is answering, what he is doing about it, and where.
/// Returns what is left of `area` underneath it.
///
/// The mascot used to live on the splash alone, which put it on screen exactly
/// while nothing was happening and took it away the moment work started — the
/// one time a mascot has something to say. Over the transcript it stays for the
/// whole session and scratches through every turn, so the work on screen reads
/// as *his*: the lion is the one at the keyboard rather than a sticker on the
/// welcome screen. The activity line sits directly under his chin for the same
/// reason — a spinner belongs to a character, not to a chrome row.
///
/// The status bar states the same two facts on one row, and the overlap is
/// deliberate rather than an oversight. The band is the first thing a short
/// terminal drops; the bar is the row that is always there, on every workspace.
/// Chrome that can vanish must never be the only place a fact is stated.
fn draw_header(f: &mut Frame, app: &App, area: Rect) -> Rect {
    if area.height < HEADER_SEATS || area.width < HEADER_FITS {
        return area;
    }
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(HEADER), Constraint::Min(3)])
        .split(area);
    f.render_widget(Paragraph::new(header_lines(app, area.width)), parts[0]);
    parts[1]
}

/// The band's rows: the lion down the left, the text to the right of it.
///
/// Four rows of drawing, and up to four of text. A session that knows where it
/// is standing fills the last one with the directory; one that does not leaves
/// it blank, which is the shape this band had when it carried three lines — the
/// spare row *under* the last line rather than above the first, the opposite of
/// the splash's lockup and for the same reason. There the lion stood on the
/// lettering's baseline; here it stands on the transcript's border, which is
/// the ground line this block actually has, and it goes on standing there
/// whether or not there is a fourth line beside it.
fn header_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let pose = mascot_pose(app);
    let room = width.saturating_sub(pose.width() + LOCKUP_GAP) as usize;
    let mut text = vec![
        header_name(room),
        header_who(app, room),
        header_doing(app, room),
    ];
    // The fourth row is the lion's body, and until now nothing was written
    // beside it — the block was three lines against a four-row drawing. The
    // directory goes there rather than anywhere shorter because it is the one
    // fact on this band that a person cannot work out from the conversation:
    // which repository the next turn will run in.
    if let Some(where_) = header_where(app, room) {
        text.push(where_);
    }
    (0..pose.height() as usize)
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(10);
            mascot_spans(pose.art[row], pose.ink[row], &mut spans);
            spans.push(Span::raw(" ".repeat(LOCKUP_GAP as usize)));
            if let Some(line) = text.get(row) {
                spans.extend(line.iter().cloned());
            }
            Line::from(spans)
        })
        .collect()
}

/// Line one: the program, and the build of it that is running.
///
/// The version is here rather than anywhere else in the TUI because this is the
/// only line that survives a thousand rows of scrollback, and "which build am I
/// looking at" is the first question asked of a program that has just done
/// something surprising. It is dropped whole rather than elided on a narrow
/// band — half a version number is worse than none.
fn header_name(room: usize) -> Vec<Span<'static>> {
    const NAME: &str = "Jod AI";
    let version = concat!(" v", env!("CARGO_PKG_VERSION"));
    let mut spans = vec![Span::styled(cut(NAME, room), bold(USER))];
    if room >= NAME.chars().count() + version.chars().count() {
        spans.push(Span::styled(version, fg(MUTED)));
    }
    spans
}

/// Line two: who is answering — the harness and the model behind the replies.
fn header_who(app: &App, room: usize) -> Vec<Span<'static>> {
    vec![Span::styled(cut(&app.identity(), room), fg(MUTED))]
}

/// Line three: what he is doing about it, in the colour the status bar gives
/// the same fact — amber while a turn is running, quiet once it is not.
fn header_doing(app: &App, room: usize) -> Vec<Span<'static>> {
    let colour = if app.busy { WARN } else { MUTED };
    vec![Span::styled(cut(&app.activity(), room), fg(colour))]
}

/// Line four: where he is working — the directory `jod tui` was launched in,
/// which is also the root `@` searches and where every turn's harness process
/// starts. `None` in a fixture that was never given one, so the band stays
/// three lines rather than printing a blank row beside the lion.
///
/// The glyph rather than a word: `in ~/Developer/Jod` spends three of the
/// scarcest columns on the band saying what a folder mark says for one, and the
/// three lines above it are already bare facts with no labels.
fn header_where(app: &App, room: usize) -> Option<Vec<Span<'static>>> {
    if app.cwd.as_os_str().is_empty() {
        return None;
    }
    let shown = under_home(
        &app.cwd,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    );
    Some(vec![Span::styled(
        format!("▪ {}", fit_path(&shown, room.saturating_sub(2))),
        fg(MUTED),
    )])
}

/// A path under the home directory, written with a `~`.
///
/// `home` is passed in rather than read here so the rule can be tested without
/// the suite depending on whose machine it runs on — the same reason the picker
/// takes `$HOME` at its own edge. `None` means there is no home to be under.
fn under_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        // The home directory itself, rather than `~/`.
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A path in at most `room` columns, cut from the **left**.
///
/// The opposite end from [`cut`], and the difference is the whole reason this
/// exists: what identifies a directory is its last two components, so a path
/// truncated the ordinary way turns `~/Developer/Repositories/Projects/Jod`
/// into `~/Developer/Repositor…` — every column spent on the part that is the
/// same for every repository he owns.
fn fit_path(path: &str, room: usize) -> String {
    let len = path.chars().count();
    if len <= room {
        return path.to_string();
    }
    // Nothing legible fits. One glyph saying "there is a path here and it did
    // not fit" beats a single stray character of it.
    if room <= 1 {
        return "…".repeat(room);
    }
    format!("…{}", path.chars().skip(len - (room - 1)).collect::<String>())
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
        .constraints([
            Constraint::Length(projects_height(app, area.height)),
            Constraint::Min(3),
            Constraint::Length(CONTEXT_HEIGHT),
        ])
        .split(area);
    draw_projects(f, app, parts[0]);
    draw_sessions(f, app, parts[1]);
    draw_context(f, app, parts[2]);
}

/// How many rows the catalog gets.
///
/// Collapsed it is one line plus its border, which still answers the question
/// the panel is there for — *which project am I in* — while giving the rest of
/// the height back to the sessions list. Expanded it grows with the catalog but
/// never past a third of the panel: the sessions below it are what a running
/// fleet is watched through, and a twenty-project catalog must not push them
/// off the screen.
fn projects_height(app: &App, available: u16) -> u16 {
    if available < 12 {
        // No honest room for a third box. The current project still reaches the
        // status bar, which is where it matters most.
        return 0;
    }
    if !app.projects_open {
        return 3;
    }
    let ceiling = (available / 3).max(4);
    let wanted = app.projects.len().clamp(1, 32) as u16 + 2;
    wanted.min(ceiling)
}

/// The way out of an empty catalog, in the thirty-odd columns the panel has.
///
/// It used to say “ask Jod to add one”, which is not a remedy: it names no
/// command, and the console had none to name — the catalog could only be
/// filled from a second terminal. Now that `/project add` exists, the empty
/// state is the natural place to learn it, because an empty box is exactly
/// when you are looking for the way to fill it.
pub(super) const CATALOG_REMEDY: &str = "/project add";

/// The catalog, with the project this conversation is about marked.
///
/// The mark is the point of the box. Everything else here is a list of
/// directories, which nobody needs on screen; *which one a dictated sentence
/// will land in* is a fact worth a permanent corner of the panel, because the
/// alternative is finding out when an agent starts editing the wrong
/// repository.
fn draw_projects(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let inner = area.width.saturating_sub(2) as usize;
    let current = app.current_project.as_ref();

    // Named, carried, or nothing — three states with three different claims on
    // his attention, so they do not share a colour.
    let (title, border) = match current.map(|(_, how)| how) {
        Some(How::Sticky) => (" projects · carried ", WARN),
        Some(_) => (" projects ", MUTED),
        None => (" projects · none set ", MUTED),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(border))
        .title(title);

    if !app.projects_open {
        let line = match current {
            Some((name, _)) => Line::from(vec![
                Span::styled(" ▸ ", fg(MUTED)),
                Span::styled(cut(name, inner.saturating_sub(3)), bold(GOOD)),
            ]),
            None => Line::from(Span::styled(
                format!(" ▸ nothing set — {CATALOG_REMEDY}"),
                fg(MUTED),
            )),
        };
        f.render_widget(Paragraph::new(line).block(block), area);
        return;
    }

    if app.projects.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" none yet — {CATALOG_REMEDY} <path>"),
                fg(MUTED),
            )))
            .block(block),
            area,
        );
        return;
    }

    // The current project first regardless of recency, because the one fact
    // this box exists to show must not be scrolled out of it by a catalog
    // longer than the box is tall.
    let mut rows: Vec<&jod_core::projects::Project> = app.projects.iter().collect();
    rows.sort_by_key(|p| current.map(|(name, _)| &p.name != name).unwrap_or(true));

    let lines: Vec<Line> = rows
        .iter()
        .map(|p| {
            let is_current = current.is_some_and(|(name, _)| &p.name == name);
            let marker = if is_current { " ▸ " } else { "   " };
            Line::from(vec![
                Span::styled(marker, fg(if is_current { GOOD } else { MUTED })),
                Span::styled(
                    cut(&p.name, inner.saturating_sub(3)),
                    if is_current { bold(GOOD) } else { fg(AGENT) },
                ),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines).block(block), area);
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
        for chunk in wrap("no runs yet — Ctrl-B delegates one", inner, 1) {
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
        Overlay::Jobs => (
            "background shells — any key closes it".to_string(),
            "Esc closes",
        ),
        Overlay::ConfirmReload => (
            "y restarts into the new build · anything else stays".to_string(),
            "Esc stays",
        ),
        // Named for what they do rather than borrowing the prompt's wording:
        // "accepts" is the wrong verb for a credential, where the question a
        // reader has at that moment is what pressing enter is about to commit
        // them to and whether escape really throws it away.
        Overlay::Secret { .. } => (
            "typing is hidden · ⏎ stores it outside every repo".to_string(),
            "Esc discards it",
        ),
        Overlay::Picker(_) => (
            "type to narrow · ↑↓ choose · ⏎ adds it read-only".to_string(),
            "Esc cancels",
        ),
        Overlay::Search { .. } => (
            "searching every transcript · ⏎ opens the conversation".to_string(),
            "Esc closes",
        ),
        // The rail is checked before the screen's own filter and before the
        // screen's own verbs, because while it has the keyboard the screen's
        // verbs are *not* in force — printing `s stop` beside a rail where `x`
        // dismisses a card teaches a key that does something else.
        Overlay::None if app.rail.focused && app.rail.shown && app.rail.editing_filter => (
            "typing searches the rail".to_string(),
            "⏎ keeps it · Esc clears it",
        ),
        Overlay::None if app.rail.focused && app.rail.shown => {
            (keys::rail_keybar(area.width), keys::RAIL_EXIT)
        }
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
/// What the status row says about the microphone.
///
/// The microphone stays on for as long as it is wanted, which makes *forgetting
/// it is on* the failure this line exists to prevent — a live microphone in a
/// room having a different conversation. So it is unmissable, it says how long,
/// and it moves: the meter tracks what is being heard right now, which is the
/// only part that distinguishes a working microphone from a dead one.
fn dictation_badge(app: &App) -> Option<String> {
    let Dictation::Listening {
        since_ms,
        pending,
        speaking,
        level,
        ..
    } = &app.dictation
    else {
        return None;
    };

    let mut said = format!(
        "● listening {} {}",
        short_duration(app.now_ms.saturating_sub(*since_ms)),
        meter(*level, *speaking),
    );
    if *pending > 0 {
        // Sentences overlap — one is transcribed while the next is spoken — so
        // this is a count, and it is what explains a pause before words appear.
        said.push_str(&format!(" · {pending} transcribing"));
    }
    said.push_str(" · say \"go ahead\"");
    Some(said)
}

/// A five-cell level meter.
///
/// Blocks rather than a number: this is read out of the corner of an eye by
/// somebody whose hands are elsewhere, and "is it moving" is the whole
/// question. Silence still shows the empty meter rather than nothing, because
/// a meter that vanished would read as the microphone having stopped.
fn meter(level: f32, speaking: bool) -> String {
    const CELLS: usize = 5;
    // Speech sits well below full scale, so the meter is scaled to the range
    // dictation actually occupies. Against full scale it would never move.
    let filled = ((level / 0.15).clamp(0.0, 1.0) * CELLS as f32).round() as usize;
    let mut out = String::new();
    for i in 0..CELLS {
        out.push(if i < filled { '▮' } else { '▯' });
    }
    if speaking {
        out.push_str(" ▸");
    }
    out
}

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
    // First, and unconditionally. A live microphone is the one state on this
    // row where not knowing has a cost outside the program, so it goes ahead
    // of every other badge and never competes for the space.
    if let Some(said) = dictation_badge(app) {
        badge.push_str(&said);
    }
    // The panel holds the context bar, but the panel is shut most of the time
    // and advice nobody can see is not advice — so the recommendation itself
    // rides the one row that is always on screen.
    if app.should_compact() {
        if !badge.is_empty() {
            badge.push_str(" · ");
        }
        badge.push_str("⚠ compact");
    }
    // Endings that arrive while you are away have to survive until you look.
    if app.unread() > 0 {
        if !badge.is_empty() {
            badge.push_str(" · ");
        }
        badge.push_str(&format!("⚑ {} unread", app.unread()));
    }
    // A shell building in the background is invisible by construction — that
    // is what backgrounding it means — so the always-on row is the only place
    // it can be seen without being looked for.
    if app.running_jobs() > 0 {
        if !badge.is_empty() {
            badge.push_str(" · ");
        }
        badge.push_str(&format!("⚙ {} running (Ctrl-G j)", app.running_jobs()));
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
    // produced `1 queuedCtrl-X stop`, which reads as neither.
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
    // they can never run together — `1 queuedCtrl-X stop` reads as neither.
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
    const TAB_COMPLETES: &str = " Tab completes · ↑↓ choose ";
    let suggestions = crate::tui::command::completions(&app.input, app);
    if suggestions.is_empty() {
        return;
    }

    // Every row is the same shape — mark, padded name, hint — so the width is
    // the widest of each part rather than the widest whole row.
    let widest_name = suggestions
        .iter()
        .map(|c| c.label.chars().count())
        .max()
        .unwrap_or(0);
    let widest_hint = suggestions
        .iter()
        .map(|c| c.hint.chars().count())
        .max()
        .unwrap_or(0);
    // Sized to the rows *and* to the title that sits in the border, and bounded
    // by the room actually to the right of the input box rather than by a fixed
    // 72 columns — that cap is what stopped `no argument restores` one letter
    // short on a 200-column terminal.
    let want = text::panel_width([TAB_COMPLETES]).max(widest_name + widest_hint + 8);
    // The popup is anchored to the input box's left edge, so the room is what
    // lies to the right of it — never more, or it would be drawn off the
    // buffer.
    let room = f.area().width.saturating_sub(input.x).max(1);
    let w = (want as u16).min(room).max(24.min(room));
    // Whatever is left for the hint once the mark, the name column and the
    // borders have been paid for. Below that it is cut *with* a marker, so a
    // sentence that stops never reads as a sentence that ended.
    let hint_room = (w as usize).saturating_sub(widest_name + 6);
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
            // The label, not the line: the row has to read as the command it
            // stands for, which for `/main <instruction>` is not what
            // accepting it types.
            let name = &c.label;
            let pad = " ".repeat(widest_name.saturating_sub(name.chars().count()) + 2);
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(name.to_string(), style),
                Span::styled(format!("{pad}{}", cut(&c.hint, hint_room)), fg(MUTED)),
            ]))
        })
        .collect();

    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(cut(TAB_COMPLETES, (w as usize).saturating_sub(2))),
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
        Overlay::Jobs => draw_jobs(f, app),
        Overlay::ConfirmReload => draw_confirm_reload(f),
        Overlay::Secret {
            name, scope, value, ..
        } => draw_secret(f, name, *scope, value),
        Overlay::Picker(p) => draw_picker(f, p),
        Overlay::Search {
            query,
            selected,
            hits,
        } => draw_search(f, query, *selected, hits),
    }
}

/// The background shells this console started.
///
/// Every job, not only the running ones: the question "did that update
/// finish, and how did it go" is asked after the fact, and a list that emptied
/// itself on completion would answer it with a blank box.
fn draw_jobs(f: &mut Frame, app: &App) {
    let mut rows: Vec<Line> = vec![Line::from("")];
    if app.jobs.is_empty() {
        rows.push(Line::from(Span::styled(
            "  nothing running — /update starts one".to_string(),
            fg(MUTED),
        )));
    }
    for job in &app.jobs {
        let colour = match job.state {
            JobState::Running => AGENT,
            JobState::Ok => GOOD,
            JobState::Failed => BAD,
        };
        rows.push(Line::from(vec![
            Span::styled(format!("  {} ", job.mark()), fg(colour)),
            Span::styled(job.label.clone(), bold(colour)),
            Span::styled(
                format!("  {}", short_duration(job.elapsed_ms(app.now_ms))),
                fg(MUTED),
            ),
        ]));
        if let Some(last) = &job.last {
            // Truncated by chars, not bytes: the installer prints ✓ and →.
            let line: String = last.chars().take(64).collect();
            rows.push(Line::from(Span::styled(format!("      {line}"), fg(MUTED))));
        }
    }
    rows.push(Line::from(""));

    let width = 72.min(f.area().width.saturating_sub(4)).max(30);
    let panel = centred(
        f.area(),
        width,
        (rows.len() as u16 + 2).min(f.area().height),
    );
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" background shells ")
                .title_bottom(" output is in the transcript · any key closes "),
        ),
        panel,
    );
}

/// Full-text search over every transcript.
///
/// Each row says **which conversation** the turn is in, because the search is
/// across all of them and a line of prose with no home is not something you can
/// decide to open. `messages_fts` covers compacted messages too, so this
/// reaches turns that have already fallen out of every context window — which
/// is most of the reason to have it.
fn draw_search(f: &mut Frame, query: &str, selected: usize, hits: &[crate::tui::data::Hit]) {
    let screen = f.area();
    let width = screen.width.saturating_sub(8).min(110).max(40);
    let height = (hits.len() as u16 + 6).min(screen.height.saturating_sub(2)).max(6);
    let panel = centred(screen, width, height);
    let room = width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("  ▸ ".to_string(), fg(USER)),
            Span::styled(query.to_string(), fg(AGENT)),
            Span::styled("▏".to_string(), fg(USER)),
        ]),
        Line::from(""),
    ];
    if query.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "  type to search every conversation, compacted turns included".to_string(),
            fg(MUTED),
        )));
    } else if hits.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no turn matches".to_string(),
            fg(MUTED),
        )));
    }
    let rows = height.saturating_sub(5) as usize;
    let first = window_start(selected, rows.max(1), hits.len());
    for (at, hit) in hits.iter().enumerate().skip(first).take(rows.max(1)) {
        let here = at == selected;
        lines.push(Line::from(vec![
            Span::styled(if here { "▸ " } else { "  " }.to_string(), bold(USER)),
            Span::styled(
                format!("{:<10}", cut(&hit.title, 10)),
                if here { bold(AGENT) } else { fg(MUTED) },
            ),
            Span::styled(format!("{:<6}", cut(&hit.who, 6)), fg(MUTED)),
            Span::styled(
                cut(&hit.text, room.saturating_sub(20)),
                if here { fg(AGENT) } else { fg(MUTED) },
            ),
        ]));
    }

    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(" search every transcript ")
                .title_bottom(" ⏎ opens that conversation · ↑↓ choose · Esc closes "),
        ),
        panel,
    );
}

/// The big half of the one picker.
///
/// Rows are drawn by [`mention_line`] — the same function the inline popup
/// uses — so the matched characters are highlighted identically in both. That
/// shared call is what makes "one picker at two sizes" true in the rendering
/// as well as in the matcher.
fn draw_picker(f: &mut Frame, p: &picker::Picker) {
    const TITLE: &str = " a directory to work in ";
    const FOOTER: &str = " ⏎ adds it read-only · ↑↓ choose · Esc cancels ";
    const LABEL: &str = "  in ";
    let screen = f.area();
    let base = p.base.display().to_string();
    // Wide enough for the header and both border titles, and otherwise the
    // shape it always had. The old fixed `.min(96)` cap held on a 260-column
    // terminal too, so `…/tui-dogfood-tetris/tetris` came out as
    // `…/tui-dogfood-tetr` — a different directory, with nothing to say text
    // was missing.
    let want =
        text::panel_width([format!("{LABEL}{base}").as_str(), TITLE, FOOTER]).max(96) as u16;
    let width = want.clamp(40, screen.width.saturating_sub(8).max(40));
    let height = (picker::ROWS as u16 + 6).min(screen.height.saturating_sub(2));
    let panel = centred(screen, width, height);
    let inner = panel.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = vec![
        // Elided from the left when it still does not fit: the tail of a path
        // is the end that tells one directory from another.
        Line::from(Span::styled(
            format!(
                "{LABEL}{}",
                text::path_beside(&base, inner, LABEL.chars().count())
            ),
            fg(MUTED),
        )),
        Line::from(vec![
            Span::styled("  ▸ ".to_string(), fg(USER)),
            Span::styled(p.query.clone(), fg(AGENT)),
            Span::styled("▏".to_string(), fg(USER)),
        ]),
        Line::from(""),
    ];
    if p.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no directory matches".to_string(),
            fg(MUTED),
        )));
    }
    for (at, row) in p.rows.iter().enumerate() {
        lines.push(mention_line(row, at == p.selected, inner));
    }
    // A list that is quietly partial is one you trust and should not.
    if p.truncated {
        lines.push(Line::from(Span::styled(
            format!(
                "  … more than {} directories here; type to narrow",
                picker::MAX_DIRS
            ),
            fg(WARN),
        )));
    }

    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(cut(TITLE, inner))
                .title_bottom(cut(FOOTER, inner)),
        ),
        panel,
    );
}

/// The one question an update cannot answer for itself.
fn draw_confirm_reload(f: &mut Frame) {
    let panel = centred(f.area(), 62, 6);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  restart this console into the new build?".to_string(),
                bold(AGENT),
            )),
            Line::from(Span::styled(
                "  agents keep running; the conversation is on disk".to_string(),
                fg(MUTED),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(AGENT))
                .title(" update installed ")
                .title_bottom(" y restarts · anything else stays "),
        ),
        panel,
    );
}

/// The credential field.
///
/// Three things distinguish it from [`draw_prompt`], and each is a rule from
/// `secret.rs` made visible:
///
/// - the field shows `secret::masked`, never the characters — a shoulder, a
///   screen share and a recorded terminal are all ordinary, and this is the
///   one part of the flow a user cannot undo afterwards;
/// - the destination is printed *above* the field, because the moment to learn
///   where a production token is going is before pasting it, not after;
/// - the border is `WARN` rather than `USER`, so the one overlay in this
///   program that must not be typed into absent-mindedly does not look like
///   the one that asks for a schedule's name.
fn draw_secret(f: &mut Frame, name: &str, scope: jod_core::secrets::Scope, value: &secret::Typed) {
    let destination = secret::destination(name, scope);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for said in &destination {
        lines.push(Line::from(Span::styled(format!("  {said}"), fg(MUTED))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(format!("  {name} ▸ "), fg(WARN)),
        // The dots are the whole point: they prove the keystrokes are landing
        // without saying what they were.
        Span::styled(secret::masked(value), fg(AGENT)),
        Span::styled("▏", fg(USER)),
    ]));
    lines.push(Line::from(Span::styled(
        "  the value is not echoed, not stored in the transcript, and not shown to the agent"
            .to_string(),
        fg(MUTED),
    )));

    let width = destination
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(40)
        .max(64)
        + 6;
    let panel = centred(f.area(), width as u16, lines.len() as u16 + 2);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(WARN))
                .title(" a credential ")
                .title_bottom(" ⏎ stores it · Esc discards it "),
        ),
        panel,
    );
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
        // The verbs that lost their chord to tmux. They are drawn rather than
        // left to the keymap overlay because this menu is the only place they
        // are now reachable at all — a route nothing prints is a route nobody
        // takes. See `on_which_key`.
        rows.push(("j".into(), "jobs        background shells".into()));
        rows.push(("u".into(), "unread      the oldest thing unread".into()));
        rows.push(("l".into(), "clear       empty the transcript".into()));
        rows.push(("d".into(), "projects    show or hide the catalog".into()));
        rows.push(("/".into(), "search      every transcript".into()));
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
    // Whichever layer actually has the keyboard, for the reason the overlay is
    // screen-first at all: help that omits what is in force sends you to the
    // source, and help that lists what is *not* in force teaches a key that
    // does something else.
    let sections = if app.rail.focused && app.rail.shown {
        keys::rail_keymap()
    } else {
        keys::keymap(app.workspace)
    };
    let compose = |spaced: bool| {
        let mut lines: Vec<Line> = Vec::new();
        for (heading, bindings) in sections.clone() {
            if spaced && !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(heading, bold(USER))));
            for binding in bindings {
                // The trailing space is not padding, it is a separator. Twelve
                // columns fits every key but one — `Ctrl-A/E/Home/End` is
                // seventeen, and `{:<12}` pads rather than truncates, so that
                // row rendered as `Ctrl-A/E/Home/Endstart / end of the line`.
                //
                // Widening the field to the longest key would cost the panel a
                // whole column at 100 wide and hide twenty rows, which is a
                // worse bug than the one being fixed. One space costs one
                // column and only when the key overflows.
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<12} ", binding.key), fg(WARN)),
                    Span::styled(binding.what.to_string(), fg(AGENT)),
                ]));
            }
        }
        lines
    };
    let mut lines = compose(true);
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
    let affordable = ((screen.width.saturating_sub(2)) as usize / column.max(1)).max(1);
    // The blank line between sections is the cheapest thing on this panel — a
    // heading already separates them, and a separator teaches no key — so a map
    // that does not fit drops the separators before it drops a binding. Same
    // budget rule the keybar spends by, and for the same reason: what is
    // dropped should be the thing you can learn nowhere else, last.
    if lines.len() > affordable * rows {
        lines = compose(false);
    }
    let wanted = lines.len().div_ceil(rows);
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

/// A destructive verb on a bare letter is one fat-fingered `Ctrl-G h x` away
/// from losing a secret, so the confirmation **names the thing**.
fn draw_confirm(f: &mut Frame, verb: &str, what: &str) {
    const WARNING: &str = " this cannot be undone ";
    const WAYS_OUT: &str = " y confirms · anything else cancels ";
    let question = format!("  {verb} {what}?  ");
    // Sized to the widest of the question and the two border titles. Sizing it
    // from the question alone gave `forget x` a seventeen-column box, which
    // clipped the warning to "this canno" and never said what cancels — on the
    // one dialog in the program that destroys something.
    let panel = centred(
        f.area(),
        text::panel_width([question.as_str(), WARNING, WAYS_OUT]) as u16,
        5,
    );
    // `centred` clamps to the terminal, so a window narrower than the footer is
    // still possible. Fit the chrome to what there is rather than let the
    // border cut it: a sentence that stops mid-word reads as the whole
    // sentence.
    let inner = panel.width.saturating_sub(2) as usize;
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                cut(&format!("  {verb} {what}?"), inner),
                bold(BAD),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(BAD))
                .title(cut(WARNING, inner))
                .title_bottom(cut(WAYS_OUT, inner)),
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
        Workspace::Traffic => draw_traffic(f, app, area),
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
/// The forest, as rows.
///
/// **The column drop order is declared here and nowhere else**: summary first,
/// then the card count, then the spinner, and the label never. A label that
/// survives every width is the difference between a narrow tree and a broken
/// one — you can work out what a row is from its name and nothing else, and
/// from a truncated name you cannot.
fn draw_tree(f: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<&jod_core::tree::Node> = app
        .tree
        .visible(&app.forest, &app.closed_works, app.tree_filter())
        .into_iter()
        .map(|at| &app.forest[at])
        .collect();
    let ids = app.tree_rows();
    let selected = app.tree.index(&ids);
    let width = area.width.saturating_sub(2) as usize;
    // The guides go plain when the terminal says it cannot draw the alphabet.
    // `NO_COLOR` is the closest signal Jod has to "this terminal is minimal",
    // and it is the same one the rest of the renderer already honours.
    let ascii = *COLOURLESS;

    // The pinned chat's own columns drop in the flat list's declared order, at
    // the flat list's thresholds, because it is the same row drawn by the same
    // function — two sets of numbers for one line would drift apart.
    let show_id = width >= 35;
    let show_harness = width >= 31;

    // One longer than the forest: position 0 is the pinned chat and everything
    // below it reads its node at `at - 1`. This ordering and `tree_rows`' have
    // to agree, or the cursor lands one row off its own highlight — the same
    // trap the flat list documents, and the same reason it is said twice.
    let (start, height) = window(area, selected, rows.len() + 1);
    let mut items: Vec<ListItem> = Vec::new();
    for at in start..(start + height).min(rows.len() + 1) {
        let here = at == selected;
        if at == 0 {
            items.push(ListItem::new(Line::from(main_line(
                app,
                here,
                width,
                show_id,
                show_harness,
            ))));
            continue;
        }
        let node = rows[at - 1];
        let expanded = app.tree.is_expanded(&node.id, &app.closed_works);
        let mut spans = vec![
            Span::styled(if here { "▸ " } else { "  " }.to_string(), bold(USER)),
            // The guides describe the forest, so they are indexed into it —
            // the pinned row sits above the tree rather than in it, and an
            // elbow measured past it would point one row off.
            Span::styled(fleet::guides(&rows, at - 1, ascii), fg(MUTED)),
            Span::styled(fleet::marker(node, expanded).to_string(), fg(MUTED)),
            Span::styled(
                format!("{} ", fleet::kind_glyph(node.kind)),
                fg(work_colour(&node.colour)),
            ),
            Span::styled(
                node.label.clone(),
                if here { bold(AGENT) } else { fg(AGENT) },
            ),
        ];
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let mut room = width.saturating_sub(used);

        // A spinner, so a running node reads as moving rather than stuck.
        if node.running {
            let glyph = format!(" {}", app.spinner());
            if room >= glyph.chars().count() {
                room -= glyph.chars().count();
                spans.push(Span::styled(glyph, fg(WARN)));
            }
        }
        // The card count says *where the questions are* without expanding
        // anything, which is most of why the tree is worth looking at.
        if node.cards > 0 {
            let badge = if node.blocked > 0 {
                format!(" [{} {}]", node.blocked, rail::BLOCKED)
            } else {
                format!(" [{} cards]", node.cards)
            };
            if room >= badge.chars().count() {
                room -= badge.chars().count();
                spans.push(Span::styled(
                    badge,
                    if node.blocked > 0 { bold(BAD) } else { fg(MUTED) },
                ));
            }
        }
        // Last on, first off.
        if !node.summary.is_empty() && room > LEAST_TEXT {
            spans.push(Span::styled(
                format!("  {}", cut(&node.summary, room.saturating_sub(2))),
                fg(MUTED),
            ));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    // Said under the pinned row rather than instead of the tree, because the
    // tree is no longer empty — the chat is always its first row, and "no works
    // yet" as the only line would now be a claim the row above it contradicts.
    if rows.is_empty() {
        items.extend(empty(if app.here().filtering() {
            "  nothing matches"
        } else {
            "  no works yet"
        }));
    }

    let blocked: usize = rows
        .iter()
        .filter(|n| n.kind == jod_core::tree::NodeKind::Work)
        .map(|n| n.blocked)
        .sum();
    let title = if blocked > 0 {
        format!(" fleet · {blocked} blocked ")
    } else {
        " fleet ".to_string()
    };
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(title)
                .title_bottom(fit_verbs(
                    " ↑↓ pick · →← in/out · space toggle · ⏎ open · z closed ",
                    area.width,
                )),
        ),
        area,
    );
}

/// What the selected node is, in full.
fn draw_tree_detail(f: &mut Frame, app: &App, area: Rect) {
    // The pinned chat gets the pane the flat list gives it, title and all:
    // none of `kind`, `id` or `state` means anything to a conversation, and
    // `selected_node` answers `None` for it — which would draw "nothing
    // selected" beside a row that is plainly selected.
    if app.tree_main_selected() {
        f.render_widget(
            Paragraph::new(main_detail(app, area.width)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(USER))
                    .title(" the chat ")
                    .title_bottom(fit_verbs(" ⏎ enter · /new leaves ", area.width)),
            ),
            area,
        );
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    match app.selected_node() {
        Some(node) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", fleet::kind_glyph(node.kind)),
                    fg(work_colour(&node.colour)),
                ),
                Span::styled(node.label.clone(), bold(AGENT)),
            ]));
            lines.push(Line::from(""));
            lines.push(detail(
                "kind",
                match node.kind {
                    jod_core::tree::NodeKind::Work => "work",
                    jod_core::tree::NodeKind::Session => "session",
                    jod_core::tree::NodeKind::Run => "run",
                },
            ));
            lines.push(detail("id", &short(&node.id.id)));
            lines.push(detail(
                "state",
                if node.running { "running" } else { "idle" },
            ));
            if node.cards > 0 {
                lines.push(detail(
                    "cards",
                    &format!("{} open · {} {}", node.cards, node.blocked, rail::BLOCKED),
                ));
            }
            if !node.summary.is_empty() {
                lines.push(Line::from(""));
                for wrapped in wrap(&node.summary, area.width.saturating_sub(4) as usize, 2) {
                    lines.push(Line::from(Span::styled(wrapped, fg(MUTED))));
                }
            }
        }
        None => lines.push(Line::from(Span::styled(
            "  nothing selected".to_string(),
            fg(MUTED),
        ))),
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(MUTED))
                    .title(" node "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// A work's colour name as one of the eight the terminal's own theme controls.
///
/// Unknown names fall back to the ordinary foreground rather than to something
/// arbitrary: a work whose colour Jod does not recognise should look plain, not
/// look like a different work.
fn work_colour(name: &str) -> Color {
    match name {
        "red" => BAD,
        "green" => GOOD,
        "yellow" => WARN,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => USER,
        _ => AGENT,
    }
}

/// One agent, as the row both halves of the fleet screen draw it.
///
/// Shared rather than copied, because the loose pane below the tree and the
/// flat list are the same row in two places: a run that reads one way in the
/// list and another way under the tree is a run you have to look at twice.
fn fleet_row<'a>(
    app: &App,
    a: &'a super::AgentLine,
    chosen: bool,
    inner: usize,
    show_id: bool,
    show_harness: bool,
) -> Line<'a> {
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
    Line::from(spans)
}

/// How tall the loose pane may grow before the tree starts losing rows.
///
/// The tree is the reason the screen exists, so the runs hanging off nothing
/// get the smaller share: enough for a few of them plus the border, and a
/// count in the title once there are more than fit.
fn loose_height(area: Rect, runs: usize) -> u16 {
    let wanted = runs as u16 + 2;
    wanted.min(area.height / 3).max(3).min(area.height)
}

/// The runs that belong to no work, drawn under the tree that cannot hold them.
fn draw_loose(f: &mut Frame, app: &App, area: Rect, runs: &[&super::AgentLine]) {
    let inner = area.width.saturating_sub(2) as usize;
    let show_id = inner >= 35;
    let show_harness = inner >= 31;
    let room = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = runs
        .iter()
        .take(room)
        .map(|a| ListItem::new(fleet_row(app, a, false, inner, show_id, show_harness)))
        .collect();
    // The count is in the title rather than on a row of its own, because the
    // pane is small enough that a row spent saying "3 more" is a row not
    // spent showing one of them.
    let title = if runs.len() > room {
        format!(" loose · {} of {} ", room, runs.len())
    } else {
        format!(" loose · {} ", runs.len())
    };
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(title)
                .title_bottom(fit_verbs(" in no work — jod ls ", area.width)),
        ),
        area,
    );
}

fn draw_fleet(f: &mut Frame, app: &App, area: Rect) {
    // The tree the moment there is one. Not a replacement for the flat list
    // below but the other half of the same screen: a session belonging to no
    // work has no node in the forest, and the list is what shows it.
    if app.has_tree() {
        let (left, right) = split(area);
        // Both halves, not one instead of the other. A run started by
        // `delegate` belongs to no work, so `Store::forest_of` gives it no node
        // and the tree cannot draw it at any width. Returning here the moment a
        // single work existed made every such run invisible — the screen said
        // "1 running" in its status bar and showed nothing that was.
        let loose = app.loose_rows();
        let tree_area = if loose.is_empty() {
            left
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),
                    Constraint::Length(loose_height(left, loose.len())),
                ])
                .split(left);
            draw_loose(f, app, rows[1], &loose);
            rows[0]
        };
        draw_tree(f, app, tree_area);
        if let Some(right) = right {
            draw_tree_detail(f, app, right);
        }
        return;
    }
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
        items.push(ListItem::new(fleet_row(
            app,
            rows[i - 1],
            chosen,
            inner,
            show_id,
            show_harness,
        )));
    }
    // Said under the pinned row rather than instead of the list, because the
    // list is no longer empty — the chat is always in it, and "no agents yet"
    // as the only line would now be a claim the row above it contradicts.
    if rows.is_empty() {
        // Short enough to survive the master pane at the design width, which is
        // 44 cells inside its border — the longer form was clipped mid-word.
        items.extend(empty("  nothing delegated yet — Ctrl-B starts one"));
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
                    .title_bottom(fit_verbs(" ⏎ enter · /new leaves ", right.width)),
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
                // Above the spend on purpose: the question this pane is most
                // often opened with is "did that do what I asked", and a run
                // launched somewhere other than where you meant answers it
                // before the cost does.
                field(
                    "in",
                    if a.cwd.is_empty() {
                        "not recorded"
                    } else {
                        &a.cwd
                    },
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
    let marker = "  ⏎ enter";
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
            " the chat Jod keeps — pinned, and it never ends",
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
        "⏎ goes in, and what you type there is an instruction. It never does \
         the work itself — it delegates, continues an agent that already has \
         the context, arms a schedule, or sets a goal, and the agents below \
         are what came of that. /new leaves.",
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

/// One work's bus: who said what to whom, threaded, and what did not arrive.
///
/// **The column drop order is declared here and nowhere else**: the message
/// text is what gives way, and never the sender, the recipient or the reason a
/// message failed. Two of those three are the row's identity and the third is
/// the only thing on this screen that is *wrong* — a log that clipped
/// `` `nobody-here` is not a member of this work `` down to `undeliverable`
/// would have printed the state everybody already suspected and dropped the
/// half worth reading.
fn draw_traffic(f: &mut Frame, app: &App, area: Rect) {
    let (left, right) = split(area);
    draw_traffic_log(f, app, left);
    if let Some(right) = right {
        draw_traffic_detail(f, app, right);
    }
}

fn draw_traffic_log(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.traffic_rows();
    let selected = app
        .list(Workspace::Traffic)
        .index(&app.row_ids(Workspace::Traffic));
    let width = area.width.saturating_sub(2) as usize;

    let mut items: Vec<ListItem> = Vec::new();
    items.push(ListItem::new(traffic_header(app, width)));

    if app.traffic_of.is_none() {
        items.extend(empty("  no work chosen — T on a fleet row opens its bus"));
    } else if rows.is_empty() {
        items.extend(empty(if app.here().filtering() {
            "  nothing matches — Esc clears the filter"
        } else {
            "  nothing has been said on this bus yet"
        }));
    }

    // One row shorter than the other lists, because the header above is drawn
    // out of the same budget.
    let (first, height) = window(area, selected, rows.len());
    // Which threads a row is the first of, so the pause marker lands on the
    // thread rather than on every message in it.
    let mut thread_seen: HashSet<&str> = HashSet::new();
    for (i, envelope) in rows.iter().enumerate() {
        let opens_thread = thread_seen.insert(envelope.thread_id.as_str());
        if i < first || i >= first + height.saturating_sub(1) {
            continue;
        }
        let chosen = i == selected;
        let held = app.traffic.held.contains(&envelope.message.id);
        let trouble = traffic::trouble(envelope, held);
        let colour = if trouble.is_some() { BAD } else { MUTED };
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(format!("{} ", traffic::glyph(envelope, held)), fg(colour)),
            Span::styled(traffic::indent(envelope.depth), fg(MUTED)),
            Span::styled(traffic::depth_marker(envelope.depth), fg(MUTED)),
            Span::styled(
                format!("{} → {}  ", envelope.message.from, envelope.message.to),
                if chosen { bold(USER) } else { fg(USER) },
            ),
        ];
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        // A paused thread says so on its opening message: G4.S3 pauses a
        // thread and never the work, so the marker belongs to the thread.
        let marker = match app.traffic.paused.get(&envelope.thread_id) {
            Some(state) if state.is_paused() && opens_thread => "  ← paused",
            _ => "",
        };
        // The reason a message failed takes the text's place rather than
        // sitting after it. On a failure the text is what the sender *tried* to
        // say and the reason is what happened, and at eighty columns there is
        // room for exactly one of them.
        let said = trouble.unwrap_or_else(|| envelope.message.text.clone());
        let (said, marked) = fit_row(used, &one_line(&said), marker, width);
        spans.push(Span::styled(
            said,
            match (chosen, colour) {
                (_, BAD) => bold(BAD),
                (true, _) => bold(AGENT),
                _ => fg(AGENT),
            },
        ));
        if marked {
            spans.push(Span::styled(marker.to_string(), fg(WARN)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }

    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }
    f.render_widget(body(Workspace::Traffic, items, area.width), area);
}

/// The line above the log: whose bus this is, and what is left of its budget.
///
/// G4.S5 asks for the budget to be visible *before* it is spent, and this is
/// the row that does it — the escalation card is far too late to be the first
/// time anybody hears that two agents have been talking for two hundred turns.
/// The work's colour tints the glyph, so one work's traffic is distinguishable
/// from another's at a glance; the figures are the channel that survives
/// `NO_COLOR`.
fn traffic_header(app: &App, width: usize) -> Line<'static> {
    if app.traffic_of.is_none() {
        return Line::from(Span::styled("  nothing open", fg(MUTED)));
    }
    let left = app.traffic.budget_left();
    // Bold and warning-coloured once the allowance is nearly gone, because a
    // number that looks the same at 190 left and at 3 is a number nobody reads.
    let tight = left * 10 <= app.traffic.budget;
    let mut budget = format!("{left} of {} messages left", app.traffic.budget);
    // The state filter, on the header rather than only in the notice `f`
    // prints. A notice scrolls away and the narrowing does not, so a log that
    // said nothing here would look empty for no reason the next time it was
    // opened.
    if app.traffic_shown != traffic::Shown::Everything {
        budget.push_str(&format!(" · {}", app.traffic_shown.label()));
    }
    // Six cells of fixed furniture: the margin, the glyph and its space, and
    // the gap before the budget. The budget is what survives a narrow pane —
    // the title is repeated in the status bar and the allowance is not.
    let room = width.saturating_sub(budget.chars().count() + 6);
    let spans = vec![
        Span::styled("  ", fg(MUTED)),
        Span::styled("■ ", fg(work_colour(&app.traffic.colour))),
        Span::styled(cut(&app.traffic.title, room), bold(AGENT)),
        Span::styled("  ", fg(MUTED)),
        Span::styled(budget, if tight { bold(WARN) } else { fg(MUTED) }),
    ];
    Line::from(spans)
}

/// The selected message, whole — which is what the log's rows cannot be.
fn draw_traffic_detail(f: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = match app.selected_message() {
        None => vec![Line::from(Span::styled(" nothing selected", fg(MUTED)))],
        Some(envelope) => {
            let held = app.traffic.held.contains(&envelope.message.id);
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!(" {} → {}", envelope.message.from, envelope.message.to),
                        bold(USER),
                    ),
                    Span::styled(
                        format!("   #{}", envelope.message.id),
                        fg(MUTED),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        " {} · depth {} · {}",
                        traffic::state_word(envelope, held),
                        envelope.depth,
                        clock(envelope.message.at_ms)
                    ),
                    fg(MUTED),
                )),
            ];
            // The reason above the message rather than below it. What a refused
            // message *said* is the least useful thing about it — nobody read
            // it — and burying why under the body is how a reader concludes
            // the row was merely slow.
            if let Some(trouble) = traffic::trouble(envelope, held) {
                lines.push(Line::from(""));
                // Wrapped like the body, and for a sharper reason: this is the
                // sentence the whole screen exists to deliver, and a reason cut
                // off at `is not a member o` is a reason nobody can act on.
                for line in wrapped(&trouble, area.width.saturating_sub(3) as usize) {
                    lines.push(Line::from(Span::styled(format!(" {line}"), bold(BAD))));
                }
            }
            if let Some(state) = app.traffic.paused.get(&envelope.thread_id) {
                if state.is_paused() {
                    lines.push(Line::from(Span::styled(
                        format!(" this thread is paused · {}", state.as_str().replace('_', " ")),
                        fg(WARN),
                    )));
                }
            }
            lines.push(Line::from(""));
            for line in wrapped(&envelope.message.text, area.width.saturating_sub(3) as usize) {
                lines.push(Line::from(Span::styled(format!(" {line}"), fg(AGENT))));
            }
            lines
        }
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(" the message ")
                .title_bottom(fit_verbs(&keys::footer(Workspace::Traffic), area.width)),
        ),
        area,
    );
}

/// A message as one line, because a row is one line and a message is prose.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Wrap prose to a width, on whole words.
///
/// Hand-rolled rather than `Wrap`, because the detail pane mixes wrapped body
/// text with lines that must not wrap — the header, the reason, the thread
/// state — and `Paragraph::wrap` is all or nothing.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(LEAST_TEXT);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
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
        // Returns from inside the match rather than before it: the other
        // entries share a prefix/style/body shape that makes one-line entries
        // uniform, and a diff is the one entry whose whole point is that it is
        // not one line. An arm keeps the match exhaustive, so a new `Entry`
        // still fails the build here rather than falling through to a default.
        Entry::Diff(edit) => return render_diff(edit, width),
        Entry::Plan(items) => return render_plan(items, width),
        Entry::Delegated { id, prompt, dir } => return render_delegated(id, prompt, dir, width),
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
        // A hint reads exactly like a notice — the difference between them is
        // about which entries the splash may cover, not about how they look.
        Entry::Notice(t) | Entry::Hint(t) => ("• ", fg(WARN), t.clone()),
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

/// The agent's plan, as a block that updates in place.
///
/// A count in the header because the thing you want at a glance is *how far
/// through* it is; the items themselves answer "through what".
fn render_plan(items: &[todo::Item], width: u16) -> Vec<Line<'static>> {
    let (done, total) = todo::progress(items);
    let room = (width as usize).saturating_sub(8);
    let mut lines = vec![Line::from(vec![
        Span::styled("  ☰ ".to_string(), fg(USER)),
        Span::styled("plan".to_string(), bold(AGENT)),
        Span::styled(format!("  {done}/{total}"), fg(MUTED)),
    ])];
    for item in items {
        let (colour, style) = match item.state {
            // The one in flight is the only line worth finding at a glance, so
            // it is the only one in the foreground colour.
            todo::State::Doing => (WARN, bold(AGENT)),
            todo::State::Done => (GOOD, fg(MUTED)),
            todo::State::Pending => (MUTED, fg(MUTED)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("    {} ", item.state.glyph()), fg(colour)),
            Span::styled(cut(&item.text, room), style),
        ]));
    }
    lines
}

/// `Ctrl-B`, confirmed at the size of what it just did.
///
/// Three facts, because those are the three you would go and check afterwards
/// and only the first of them was ever on screen: which agent to look at, what
/// it was told to do, and — since a delegated agent edits files unattended —
/// which directory it was pointed at. Both the prompt and the path are wrapped
/// rather than cut: a confirmation you cannot read the end of is the thing
/// being fixed here, not a smaller version of it.
fn render_delegated(id: &str, prompt: &str, dir: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("  ⇢ ".to_string(), fg(GOOD)),
        Span::styled("delegated ".to_string(), bold(AGENT)),
        Span::styled(short(id), bold(USER)),
        Span::styled(
            " · in the background, Ctrl-F to watch".to_string(),
            fg(MUTED),
        ),
    ])];
    let indent = 6usize;
    let pad = " ".repeat(indent);
    for (label, text, style) in [("in ", dir, fg(MUTED)), ("", prompt, fg(AGENT))] {
        for (i, row) in wrap(text, width as usize, indent + label.chars().count())
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 {
                format!("{pad}{label}")
            } else {
                format!("{pad}{}", " ".repeat(label.chars().count()))
            };
            lines.push(Line::from(vec![
                Span::styled(lead, fg(MUTED)),
                Span::styled(row, style),
            ]));
        }
    }
    lines
}

/// A file edit, as a diff.
///
/// The path is a header rather than a prefix on every line: repeated down forty
/// rows it would cost the width the code needs, and it is the same file
/// throughout by construction.
fn render_diff(edit: &diff::Edit, width: u16) -> Vec<Line<'static>> {
    let room = (width as usize).saturating_sub(6);
    // The counts are reserved before the path is laid out, not appended after
    // it and hoped for: `room` used to be computed and then applied only to the
    // body, so an absolute path — every path, in a worktree — ran to the right
    // edge and pushed both the filename and the `+6 -0` off the screen.
    let marker = "  ± ";
    let counts = format!("  +{} -{}", edit.added(), edit.removed());
    let mut lines = vec![Line::from(vec![
        Span::styled(marker.to_string(), fg(WARN)),
        Span::styled(
            text::path_beside(
                &edit.path,
                room,
                marker.chars().count() + counts.chars().count(),
            ),
            bold(AGENT),
        ),
        Span::styled(counts, fg(MUTED)),
    ])];
    for line in &edit.lines {
        let colour = match line {
            diff::Line::Added(_) => GOOD,
            diff::Line::Removed(_) => BAD,
            diff::Line::Context(_) => MUTED,
        };
        lines.push(Line::from(Span::styled(
            // The sign is inside the styled text rather than a separate span:
            // it has to survive `NO_COLOR`, and a reader who cannot tell red
            // from green reads this column instead of the colour.
            format!("    {}{}", line.sign(), cut(line.text(), room)),
            fg(colour),
        )));
    }
    if edit.elided > 0 {
        lines.push(Line::from(Span::styled(
            format!("    … {} more lines", edit.elided),
            fg(MUTED),
        )));
    }
    lines
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
/// teach Ctrl-G, and a third copy is noise rather than help. Empty when even the
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
    // Both read off the one function, so the height the box was given and the
    // text drawn into it can never disagree about where the line breaks. The
    // caret costs two columns, which a box this narrow does not have to spare:
    // on it the text wins and the caret goes.
    let field = composer_field(area.width);
    let gutter = inner_width - field;
    let caret = if gutter > 0 { CARET } else { "" };

    let col = app.cursor_column();
    let typed: Vec<char> = app.input.chars().collect();
    let wrapped = composer_lines(typed.len(), col, field);
    // The box has already grown for what was typed, so this only bites past the
    // cap: then the rows scroll to keep the caret in view, the same rule the
    // field used to apply sideways.
    let rows = area.height.saturating_sub(2).max(1) as usize;
    let first = window_start(col / field, rows, wrapped);

    // Muted while the field is empty so the caret and the hint read as one
    // piece of furniture; live the moment there is something to send.
    let lines: Vec<Line> = if app.input.is_empty() {
        vec![Line::from(vec![
            Span::styled(caret, fg(MUTED)),
            Span::styled(placeholder(field), fg(MUTED)),
        ])]
    } else {
        let style = fg(if app.busy { WARN } else { USER });
        (first..wrapped.min(first + rows))
            .map(|row| {
                // The caret marks where the line starts, so it goes on the first
                // row only; the rest are indented to it, and the wrapped text
                // keeps one left edge instead of two.
                let lead = if row == 0 {
                    Span::styled(caret, style)
                } else {
                    Span::raw(" ".repeat(gutter))
                };
                let text: String = typed.iter().skip(row * field).take(field).collect();
                Line::from(vec![lead, Span::raw(text)])
            })
            .collect()
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
    let row = (col / field).saturating_sub(first).min(rows - 1);
    f.set_cursor_position((
        area.x + 1 + (gutter + col % field) as u16,
        area.y + 1 + row as u16,
    ));
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
    use crate::tui::rail::RailState;
    use crate::tui::PromptIntent;
    use jod_core::cards::NewCard;
    use jod_core::store::Store as RealStore;
    use jod_core::team::{Member, Post, Scope, Sent, TeamTask};
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
            cwd: "/srv/reljod/repo".into(),
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

    /// BUG-11: descriptions simply stopped — `no argument restore`, one letter
    /// short of `restores` — because the popup was capped at 72 columns and the
    /// row was clipped by the border. With no `…` a cut sentence reads as one
    /// the author forgot to finish.
    #[test]
    fn every_command_description_is_whole_or_marked_as_cut() {
        let mut a = app();
        a.input = "/".into();
        let screen = rendered(&a, 100, popup_height());
        for c in crate::tui::command::completions(&a.input, &a) {
            if screen.contains(&c.hint) {
                continue;
            }
            let marked = (1..c.hint.chars().count()).any(|n| {
                let head: String = c.hint.chars().take(n).collect();
                screen.contains(&format!("{head}…"))
            });
            assert!(
                marked,
                "{:?} is neither whole nor marked as cut:\n{screen}",
                c.hint
            );
        }
    }

    /// And on a terminal with room to spare there is nothing to cut: the cap
    /// was fixed at 72 columns whatever the terminal was.
    #[test]
    fn a_wide_terminal_shows_the_longest_description_whole() {
        let mut a = app();
        a.input = "/".into();
        let screen = rendered(&a, 200, popup_height());
        let longest = crate::tui::command::completions(&a.input, &a)
            .into_iter()
            .max_by_key(|c| c.hint.chars().count())
            .expect("the list is not empty")
            .hint;
        assert!(screen.contains(&longest), "{longest:?} whole:\n{screen}");
    }

    /// BUG-10: `/main` was listed twice, with opposite behaviours — go into the
    /// main chat, and send it one instruction from here — and nothing on either
    /// row said which was which. The difference is the argument, so the row has
    /// to show the argument.
    ///
    /// Pinned to the two rows rather than to the screen: `/main` is also in the
    /// input box being typed, and a screen-wide `contains` would pass on a
    /// palette that still shows the same token twice.
    #[test]
    fn the_two_main_rows_say_which_one_takes_an_instruction() {
        let mut a = app();
        a.input = "/main".into();
        let screen = rendered(&a, 120, popup_height());
        let row = |hint: &str| {
            screen
                .lines()
                .find(|line| line.contains(hint))
                .unwrap_or_else(|| panic!("expected a row hinting {hint:?}:\n{screen}"))
        };
        // The label is what is left of a row once its hint and the chrome come
        // off — the part a user reads to choose between the two.
        let label = |hint: &str| {
            let line = row(hint);
            line[..line.find(hint).unwrap()]
                .trim_matches(|c: char| c.is_whitespace() || c == '│' || c == '▸')
                .to_string()
        };
        let go = label("go into the main chat");
        let send = label("send it one instruction");
        assert_ne!(
            go, send,
            "the two /main rows must not read alike:\n{screen}"
        );
        assert_eq!(go, "/main", "{screen}");
        assert_eq!(send, "/main <instruction>", "{screen}");
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

    /// A terminal tall enough to show the whole command list at once.
    ///
    /// Derived rather than written down, because the two tests below assert
    /// things about rows that must all be *visible*, and a literal height is a
    /// fixture that silently expires the next time a command is added — which
    /// is exactly how it expired last time. The slack is the popup's borders,
    /// the input box and the frame around them.
    fn popup_height() -> u16 {
        crate::tui::command::HELP.len() as u16 + 12
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
    /// short terminal's popup can hold — and taller again each time a command
    /// is added, so the height is derived from the list rather than guessed.
    #[test]
    fn the_completion_hints_line_up_in_a_column() {
        let mut a = app();
        a.input = "/".into();
        // Tall enough to hold the whole palette. The two rows sampled below
        // are the first and the last, deliberately — alignment is only worth
        // checking across the full width of the list — so the viewport has to
        // fit every command, and it grows when the palette does. Nothing about
        // the assertion changes with it.
        let screen = rendered(&a, 100, popup_height());
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

    /// The two boxes are the whole screen, so a `you` box that runs wider than
    /// the `jod` box above it reads as a rendering slip. They line up.
    ///
    /// Measured off the boxes' own borders rather than by looking for text on
    /// the screen: the `you` title also appears in the keybar, and a
    /// screen-wide `contains` would pass on a box of any width at all.
    #[test]
    fn the_composer_is_the_same_span_as_the_transcript() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        let screen = rendered(&a, 260, 30);
        assert_eq!(
            box_span(&screen, "┌ jod "),
            box_span(&screen, "┌ you "),
            "the two boxes must line up:\n{screen}"
        );
    }

    /// BUG-12: on a 260-column terminal the composer was 72 columns wide, so a
    /// 200-character delegation prompt was about 68 characters visible and the
    /// rest scrolled off. Ctrl-B spends money and runs unattended; not being
    /// able to read your own prompt before sending it is a poor trade.
    ///
    /// The box is the measure wide, not the terminal, so the room comes from
    /// wrapping onto a second row instead: the assertion is that every
    /// character survives somewhere in the box, not that they share one line.
    #[test]
    fn a_long_delegation_prompt_is_readable_before_it_is_sent() {
        let mut a = app();
        a.input = "sweep every open PR, group them by which file they touch, \
                   and tell me which two would conflict if they both landed \
                   today — do not merge anything, just report what you find"
            .into();
        a.cursor = a.input.len();
        assert!(a.input.chars().count() >= 160, "a realistic prompt");
        for (w, h) in [(260, 30), (100, 24), (80, 24)] {
            let screen = rendered(&a, w, h);
            assert_eq!(
                composer_text(&screen),
                a.input,
                "the whole prompt must be readable at {w}×{h}:\n{screen}"
            );
        }
    }

    /// The box grows to hold it, rather than the transcript being squeezed out
    /// or the prompt being cut off at three rows.
    #[test]
    fn the_composer_grows_a_row_at_a_time_and_stops() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        let one = composer_height(&rendered(&a, 200, 30));
        a.input = "x".repeat(400);
        a.cursor = a.input.len();
        let many = composer_height(&rendered(&a, 200, 30));
        assert_eq!(one, 3, "one line of prompt, one row of box");
        assert!(many > one, "it grew: {many}");
        a.input = "x".repeat(10_000);
        a.cursor = a.input.len();
        assert_eq!(
            composer_height(&rendered(&a, 200, 30)),
            COMPOSER_ROWS as usize + 2,
            "and stopped at the cap"
        );
    }

    /// The columns a box titled `title` spans, as `(left, right)`.
    fn box_span(screen: &str, title: &str) -> (usize, usize) {
        let top = screen
            .lines()
            .find(|line| line.contains(title))
            .unwrap_or_else(|| panic!("expected a box titled {title:?}:\n{screen}"));
        let columns: Vec<char> = top.chars().collect();
        (
            columns.iter().position(|c| *c == '┌').unwrap(),
            columns.iter().position(|c| *c == '┐').unwrap(),
        )
    }

    /// The composer's rows, borders and caret gutter stripped, joined back into
    /// the one line they were wrapped from.
    fn composer_text(screen: &str) -> String {
        let (left, right) = box_span(screen, "┌ you ");
        let rows: Vec<&str> = screen.lines().collect();
        let top = rows.iter().position(|r| r.contains("┌ you ")).unwrap();
        rows.iter()
            .skip(top + 1)
            .take_while(|row| !row.chars().nth(left).is_some_and(|c| c == '└'))
            .map(|row| {
                row.chars()
                    .skip(left + 1 + CARET.chars().count())
                    .take(right - left - 1 - CARET.chars().count())
                    .collect::<String>()
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The composer's height in rows, borders included.
    fn composer_height(screen: &str) -> usize {
        let (left, _) = box_span(screen, "┌ you ");
        let rows: Vec<&str> = screen.lines().collect();
        let top = rows.iter().position(|r| r.contains("┌ you ")).unwrap();
        rows.iter()
            .skip(top)
            .position(|row| row.chars().nth(left).is_some_and(|c| c == '└'))
            .expect("the composer's bottom border")
            + 1
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

    /// The event loop pushes a hint at startup and `/new` pushes one of its
    /// own, so "the transcript is empty" would never be true and the splash
    /// would never appear. The first real turn is what ends it.
    ///
    /// This test used to push an `Entry::Notice` for the opening line and
    /// assert the wordmark survived it, which is how the splash came to
    /// swallow every notice-only command: a notice is what an *answer* is made
    /// of. The opening line is an `Entry::Hint`, and that is the only thing the
    /// splash now outlives — the second half of the test is unchanged.
    #[test]
    fn the_wordmark_survives_the_opening_line_and_goes_when_the_conversation_starts() {
        let mut a = app();
        a.push(Entry::Hint("Ctrl-G opens every screen".into()));
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

    /// Every drawing the mascot has, so a test can hold all of them to the
    /// same shape at once.
    const POSES: [(&str, &Pose); 5] = [
        ("sitting", &SITTING),
        ("blinking", &BLINKING),
        ("roaring", &ROARING),
        ("scratch-low", &SCRATCH_LOW),
        ("scratch-high", &SCRATCH_HIGH),
    ];

    /// The mascot is the brand, so it appears on the screen the brand is for —
    /// standing beside the lettering, not stacked over it. Stacked, the lockup
    /// costs the height of both and the input box pays for it.
    #[test]
    fn a_new_session_shows_the_mascot_beside_the_wordmark() {
        let screen = rendered(&app(), 100, 24);
        for row in SITTING.art {
            assert!(
                screen.contains(row.trim_end()),
                "mascot row missing {row:?}:\n{screen}"
            );
        }
        // Beside means one screen row holds both, in that order. The lion is
        // the shorter of the two and sits on the baseline, so its crown lands
        // on the wordmark's second row.
        let crown = SITTING.art[0].trim_end();
        let together = screen
            .lines()
            .find(|line| line.contains(crown))
            .expect("the crown");
        let lion = together.find(crown).expect("the mane");
        let letters = together
            .find(banner()[1].trim_end())
            .expect("the wordmark on the same row");
        assert!(
            lion < letters,
            "the lion stands to the left of the lettering:\n{together}"
        );
    }

    /// The lion is shorter than the lettering, so the two share a ground line
    /// rather than a centre: feet level with the baseline, and the blank row
    /// the lion does not fill is above its crown, not under its paws. Centred
    /// instead, it would float half a row off the bottom and read as a sticker
    /// stuck beside a word.
    #[test]
    fn the_lion_stands_on_the_lettering_s_baseline() {
        let rows = lockup(&SITTING, &banner(), 100).expect("a lockup at 100 columns");
        assert_eq!(
            rows.len(),
            banner().len(),
            "the lockup is as tall as the lettering and no taller"
        );
        let flat =
            |line: &Line| -> String { line.spans.iter().map(|s| s.content.as_ref()).collect() };
        assert!(
            flat(&rows[0]).starts_with(&" ".repeat(SITTING.width() as usize)),
            "the spare row is above the crown: {:?}",
            flat(&rows[0])
        );
        assert!(
            flat(rows.last().expect("a last row")).starts_with(SITTING.art[SITTING.art.len() - 1]),
            "and the paws are on the bottom row: {:?}",
            flat(rows.last().expect("a last row"))
        );
    }

    /// Twenty-four rows is the smallest terminal anybody calls standard, and
    /// eighty columns the narrowest. Both halves of the mark have to seat on
    /// one, with the caption and the input box under them.
    #[test]
    fn the_mascot_fits_the_smallest_terminal_worth_calling_standard() {
        let screen = rendered(&app(), 80, 24);
        assert!(screen.contains(SITTING.art[2]), "the muzzle:\n{screen}");
        assert!(screen.contains(&banner()[0]), "the wordmark:\n{screen}");
        assert!(screen.contains("you"), "and somewhere to type:\n{screen}");
    }

    /// The lion costs twelve columns and two of air, and on the band of widths
    /// that seats the block lettering but not both, the lettering is what
    /// survives: it is the thing that says what you launched.
    #[test]
    fn the_mascot_is_the_first_thing_a_narrow_terminal_drops() {
        let screen = rendered(&app(), 50, 20);
        assert!(
            !screen.contains(SITTING.art[2]),
            "no room for a muzzle:\n{screen}"
        );
        assert!(
            screen.contains(&banner()[0]),
            "the wordmark stays:\n{screen}"
        );
        assert!(screen.contains("you"), "and somewhere to type:\n{screen}");
    }

    /// Twelve columns is a far cheaper logo than the thirty-seven the block
    /// lettering needs, so a terminal too narrow for the wordmark still gets a
    /// mascot beside "Jod AI" — as long as it is tall enough to seat one.
    #[test]
    fn the_mascot_survives_a_terminal_too_narrow_for_the_wordmark() {
        let screen = rendered(&app(), 30, 20);
        assert!(screen.contains("Jod AI"), "{screen}");
        assert!(screen.contains(SITTING.art[2]), "the face:\n{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 30), "{screen}");
    }

    /// Every row of every frame the same width, and a stencil cell for every
    /// glyph. Miss either and the mane stops lining up with the chin the moment
    /// the thing is centred — or a row comes out in the wrong colour, which is
    /// the failure that renders fine in a test that only reads text.
    #[test]
    fn every_mascot_pose_is_a_rectangle_with_a_stencil_to_match() {
        let (_, first) = POSES[0];
        for (name, frame) in POSES {
            assert_eq!(
                (frame.width(), frame.height()),
                (first.width(), first.height()),
                "{name} is a different size, so the lockup would jump four times a second"
            );
            assert_eq!(
                frame.art.len(),
                frame.ink.len(),
                "{name} has a stencil of the wrong length"
            );
            for (art, stencil) in frame.art.iter().zip(frame.ink) {
                assert_eq!(
                    art.chars().count(),
                    frame.width() as usize,
                    "{name} row {art:?} is not {} columns",
                    frame.width()
                );
                assert_eq!(
                    stencil.chars().count(),
                    art.chars().count(),
                    "{name} stencil {stencil:?} does not cover {art:?}"
                );
            }
        }
    }

    /// The mascot is the only thing on a fresh screen that can say the fleet is
    /// busy, so a running agent has to change what it is doing. Two tells: a
    /// paw appears in the column the idle lion leaves blank, and the eye on
    /// that side screws shut.
    #[test]
    fn a_running_agent_sets_the_mascot_scratching() {
        // The whole muzzle row, not the `▟` alone: that glyph is also a haunch
        // of the body, so it is on screen in every pose.
        let paw = SCRATCH_LOW.art[2].trim_end();
        let both_eyes = SITTING.art[1].trim_end();
        let mut a = app();
        let idle = rendered(&a, 100, 24);
        assert!(!idle.contains(paw), "an idle lion scratches nothing");
        assert!(idle.contains(both_eyes), "and looks straight at you");

        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains(paw), "a paw up at the itch:\n{screen}");
        assert!(
            !screen.contains(both_eyes),
            "and a squint on the side it scratches:\n{screen}"
        );
    }

    /// A conversation with something in it, so the splash gives way to the
    /// transcript and the header is what carries the mascot.
    fn talking() -> App {
        let mut a = app();
        a.push(Entry::You("port the parser".into()));
        a.push(Entry::Agent("on it".into()));
        a
    }

    /// The mascot used to be on screen exactly while nothing was happening.
    /// Over the conversation it stays for the whole session, which is the only
    /// arrangement in which the work on screen can read as the lion's.
    #[test]
    fn the_mascot_stays_over_the_conversation_once_it_starts() {
        let screen = rendered(&talking(), 100, 30);
        for row in SITTING.art {
            assert!(
                screen.contains(row.trim_end()),
                "mascot row missing {row:?}:\n{screen}"
            );
        }
        assert!(screen.contains("port the parser"), "{screen}");
        // Above it, not beside it: the crown comes before the first line of the
        // conversation.
        let crown = screen
            .lines()
            .position(|l| l.contains(SITTING.art[0].trim_end()))
            .expect("the crown");
        let said = screen
            .lines()
            .position(|l| l.contains("port the parser"))
            .expect("the prompt");
        assert!(crown < said, "the header sits over the transcript:\n{screen}");
    }

    /// Three lines beside him, each answering a different question: which build
    /// is running, who is answering, and what he is doing about it.
    #[test]
    fn the_header_says_which_build_is_running_and_who_is_answering() {
        let screen = rendered(&talking(), 100, 30);
        let band = screen.lines().take(7).collect::<Vec<_>>().join("\n");
        assert!(band.contains("Jod AI"), "{band}");
        assert!(
            band.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "the build that is running:\n{band}"
        );
        assert!(band.contains("Claude Code"), "who is answering:\n{band}");
        assert!(band.contains("ready"), "and what he is doing:\n{band}");
    }

    /// The spinner belongs to a character rather than to a chrome row: while a
    /// turn runs the lion scratches, and the line under his chin says so.
    #[test]
    fn the_lion_over_the_conversation_works_while_the_turn_does() {
        let paw = SCRATCH_LOW.art[2].trim_end();
        let mut a = talking();
        assert!(
            !rendered(&a, 100, 30).contains(paw),
            "an idle lion scratches nothing"
        );

        a.busy = true;
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains(paw), "a paw up at the itch:\n{screen}");
        assert!(screen.contains("working"), "and a word for it:\n{screen}");
    }

    /// The band costs five rows, and a conversation squeezed into the few that
    /// would be left is not a conversation. Below the threshold the lion goes
    /// and the transcript keeps every row.
    #[test]
    fn a_short_terminal_drops_the_header_before_the_conversation() {
        let screen = rendered(&talking(), 100, 14);
        assert!(
            !screen.contains(SITTING.art[2]),
            "no room for a muzzle:\n{screen}"
        );
        assert!(screen.contains("port the parser"), "{screen}");
    }

    /// Fourteen columns of a narrow band are the lion, and what is left of the
    /// line naming the model is an ellipsis. The conversation gets them back.
    #[test]
    fn a_narrow_column_drops_the_header_rather_than_eliding_it_to_nothing() {
        let screen = rendered(&talking(), 44, 30);
        assert!(
            !screen.contains(SITTING.art[2]),
            "no room for a muzzle:\n{screen}"
        );
        assert!(screen.contains("port the parser"), "{screen}");
    }

    /// Four ticks in every forty-eight, which is a roar every twelve seconds.
    /// Driven by the tick rather than by a stored index, so the frame is a
    /// function of the clock and a test can simply wind it forward.
    #[test]
    fn the_mascot_roars_on_a_schedule_of_its_own() {
        let mut a = app();
        a.tick = 45;
        let screen = rendered(&a, 100, 24);
        assert!(
            screen.contains(ROARING.art[2].trim_end()),
            "an open mouth and two fangs:\n{screen}"
        );
    }

    /// The completion popup grows upwards out of the input box and the command
    /// list is thirty-odd rows, so a centred input would leave it half a screen
    /// and the list would come out cut in half.
    #[test]
    fn the_splash_yields_to_the_completion_popup_rather_than_clipping_it() {
        let mut a = app();
        a.input = "/".into();
        // Sized to the palette, as in `the_completion_hints_line_up_in_a_column`:
        // `/team` is the sentinel for "the far end of the list is reachable",
        // so the screen has to be tall enough to hold the list it is the end of.
        let screen = rendered(&a, 100, popup_height());
        assert!(screen.contains("this list"), "/help:\n{screen}");
        assert!(
            screen.contains("the team panel"),
            "/team, thirty rows further down:\n{screen}"
        );
    }

    // ---- where this console is standing ----

    /// The band has to name the directory, because it is the one fact on it a
    /// person cannot recover from the conversation: which repository the next
    /// turn runs in. A console that will edit `Jod` and a console that will
    /// edit `Jod-Apps` look identical without it.
    #[test]
    fn the_band_says_which_directory_the_next_turn_runs_in() {
        let mut a = app();
        a.cwd = PathBuf::from("/somewhere/Developer/Jod");
        a.push(Entry::Agent("here is the summary".into()));
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("/somewhere/Developer/Jod"), "{screen}");
    }

    /// ...and a fixture that was never given one stays three lines rather than
    /// printing a bare mark against a blank row.
    #[test]
    fn a_session_standing_nowhere_prints_no_fourth_line() {
        let mut a = app();
        a.push(Entry::Agent("here is the summary".into()));
        let screen = rendered(&a, 100, 24);
        assert!(
            !screen.contains('▪'),
            "no folder mark with no folder:\n{screen}"
        );
    }

    /// The splash is the screen up while you decide what to type, so it is the
    /// one that has to say which repository the first instruction will land in.
    /// The band says it too and is not on screen yet — the splash *is* the
    /// state where nothing has been said.
    #[test]
    fn the_new_session_screen_says_which_directory_you_opened_it_in() {
        let mut a = app();
        a.cwd = PathBuf::from("/somewhere/Developer/Jod");
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("an orchestrator"), "the splash:\n{screen}");
        assert!(screen.contains("/somewhere/Developer/Jod"), "{screen}");
    }

    /// Home is written `~`, which is how it is typed and how every other
    /// program on the machine prints it.
    #[test]
    fn a_path_under_home_is_written_with_a_tilde() {
        let home = Path::new("/Users/reljod");
        assert_eq!(
            under_home(Path::new("/Users/reljod/Developer/Jod"), Some(home)),
            "~/Developer/Jod"
        );
        assert_eq!(under_home(home, Some(home)), "~", "home itself is bare");
        assert_eq!(
            under_home(Path::new("/opt/jod"), Some(home)),
            "/opt/jod",
            "and a path that is not under it is left alone"
        );
        assert_eq!(
            under_home(Path::new("/Users/reljodoreta/x"), Some(home)),
            "/Users/reljodoreta/x",
            "a longer name that merely starts with home's is not under it"
        );
        assert_eq!(
            under_home(Path::new("/Users/reljod/x"), None),
            "/Users/reljod/x",
            "no home, nothing to shorten against"
        );
    }

    /// A path is identified by its *last* components, so the elision has to eat
    /// the front. Truncated the ordinary way, every repository he owns comes
    /// out reading `~/Developer/Repositor…`.
    #[test]
    fn a_path_too_long_for_the_band_loses_its_front_not_its_name() {
        assert_eq!(fit_path("~/Developer/Jod", 20), "~/Developer/Jod");
        assert_eq!(fit_path("~/Developer/Jod", 8), "…per/Jod");
        assert_eq!(
            fit_path("~/Developer/Jod", 8).chars().count(),
            8,
            "and it fills the room it was given, exactly"
        );
        assert_eq!(fit_path("~/Developer/Jod", 1), "…", "no room for a name");
        assert_eq!(fit_path("~/Developer/Jod", 0), "", "and none for anything");
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
        // The box's own top row, found rather than assumed: the header band
        // stands between the row of air and the transcript on any terminal tall
        // enough to seat one.
        let top = rows
            .iter()
            .find(|row| row.contains('┌'))
            .expect("the transcript's top border");
        assert!(top.starts_with("  ┌"), "a gutter to the left: {top:?}");
        assert!(top.ends_with("┐  "), "and to the right: {top:?}");
        // And the band above it keeps the same gutter, so the lion lines up
        // with the box rather than standing a column out from it.
        let crown = rows
            .iter()
            .find(|row| row.contains(SITTING.art[0].trim_end()))
            .expect("the header band");
        assert!(crown.starts_with("  █"), "the lion keeps it too: {crown:?}");
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

    // ---- the project catalog ----

    fn catalogued(name: &str) -> jod_core::projects::Project {
        jod_core::projects::Project {
            id: name.into(),
            name: name.into(),
            path: std::path::PathBuf::from(format!("/home/reljod/repo/{name}")),
            remote: None,
            aliases: Vec::new(),
            state: jod_core::projects::State::Active,
            colour: "cyan".into(),
            notes: String::new(),
            created_at_ms: 0,
            last_touched_ms: 0,
        }
    }

    fn with_catalog(names: &[&str], current: Option<(&str, How)>) -> App {
        let mut a = app();
        a.panel = true;
        a.projects = names.iter().map(|n| catalogued(n)).collect();
        a.current_project = current.map(|(n, how)| (n.to_string(), how));
        a
    }

    #[test]
    fn the_catalog_is_listed_in_the_panel() {
        let a = with_catalog(&["tetris", "jod"], Some(("tetris", How::Inferred)));
        let screen = rendered(&a, 140, 30);
        assert!(screen.contains("projects"), "{screen}");
        assert!(screen.contains("tetris"), "{screen}");
        assert!(screen.contains("jod"), "{screen}");
    }

    /// The one fact the box exists for.
    #[test]
    fn the_current_project_is_marked() {
        let a = with_catalog(&["tetris", "jod"], Some(("jod", How::Inferred)));
        let screen = rendered(&a, 140, 30);
        let marked = screen
            .lines()
            .find(|l| l.contains('▸') && l.contains("jod"))
            .is_some();
        assert!(marked, "the current project is not marked: {screen}");
    }

    /// A carried project is the one worth a glance before an agent starts, so
    /// the box has to say it was carried rather than named.
    #[test]
    fn a_carried_project_says_so_on_the_box() {
        let a = with_catalog(&["tetris"], Some(("tetris", How::Sticky)));
        assert!(rendered(&a, 140, 30).contains("carried"));
    }

    #[test]
    fn a_named_project_is_not_flagged_as_carried() {
        let a = with_catalog(&["tetris"], Some(("tetris", How::Inferred)));
        assert!(!rendered(&a, 140, 30).contains("carried"));
    }

    /// Collapsed still has to answer "which project am I in" — that is the
    /// difference between collapsing the box and closing it.
    /// Asserted against the catalog box rather than the whole screen: `jod` is
    /// the program's own name and appears in the banner, so a screen-wide
    /// search would be testing the wrong thing.
    #[test]
    fn a_collapsed_catalog_still_shows_the_current_project() {
        let mut a = with_catalog(&["tetris", "zephyr"], Some(("tetris", How::Inferred)));
        a.projects_open = false;
        let screen = rendered(&a, 140, 30);
        assert!(screen.contains("tetris"), "{screen}");
        assert!(
            !screen.contains("zephyr"),
            "collapsed still listed the rest: {screen}"
        );
    }

    /// It used to say "no projects yet — ask Jod to add one", which named no
    /// command, because until `/project` existed the console had none to name.
    /// The assertion is now on the remedy rather than on the complaint: an
    /// empty box whose text does not say how to fill it is the bug.
    #[test]
    fn an_empty_catalog_says_how_to_fill_it() {
        let a = with_catalog(&[], None);
        let screen = rendered(&a, 140, 30);
        assert!(screen.contains(CATALOG_REMEDY), "{screen}");
        // And the same is true with the box collapsed, which is one keypress
        // away and used to read only "nothing set".
        let mut shut = with_catalog(&[], None);
        shut.projects_open = false;
        let screen = rendered(&shut, 140, 30);
        assert!(screen.contains(CATALOG_REMEDY), "{screen}");
    }

    /// The sessions list is how a running fleet is watched. A long catalog must
    /// not push it off the panel.
    #[test]
    fn a_long_catalog_is_capped_at_a_third_of_the_panel() {
        let mut a = app();
        a.panel = true;
        a.projects = (0..40)
            .map(|i| catalogued(&format!("project-{i}")))
            .collect();
        let height = projects_height(&a, 30);
        assert!(height <= 10, "the catalog took {height} of 30 rows");
    }

    /// Below a certain height there is no honest room for a third box, and
    /// squeezing one in costs the sessions list its last row.
    #[test]
    fn a_short_panel_drops_the_catalog_rather_than_squeezing_it() {
        let a = with_catalog(&["tetris"], None);
        assert_eq!(projects_height(&a, 10), 0);
    }

    // ---- dictation ----

    #[test]
    fn a_live_microphone_is_on_the_always_visible_row() {
        let mut a = app();
        a.dictation = Dictation::Listening {
            since_ms: 0,
            backend: "arecord".into(),
            pending: 0,
            speaking: false,
            level: 0.0,
            heard: 0,
        };
        a.now_ms = 3_000;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("listening"), "{screen}");
        // The way out has to be sayable, not typeable: somebody using this has
        // their hands somewhere else, which is the whole reason it exists.
        assert!(
            screen.contains("go ahead"),
            "the hands-free way to act on it was not offered: {screen}"
        );
    }

    /// The failure a switch has that a button does not: a microphone left on
    /// in a room having a different conversation.
    #[test]
    fn the_listening_badge_counts_up_so_a_forgotten_microphone_is_obvious() {
        let mut a = app();
        a.dictation = Dictation::Listening {
            since_ms: 0,
            backend: "arecord".into(),
            pending: 0,
            speaking: false,
            level: 0.0,
            heard: 0,
        };
        a.now_ms = 75_000;
        assert!(
            dictation_badge(&a).is_some_and(|b| b.contains("1m15s")),
            "the elapsed time is not shown"
        );
    }

    #[test]
    fn nothing_is_said_about_the_microphone_when_it_is_off() {
        assert!(dictation_badge(&app()).is_none());
    }

    /// The meter is the only thing on screen that distinguishes a working
    /// microphone from a dead one, so it has to move with what is heard.
    #[test]
    fn the_meter_moves_with_the_level() {
        let quiet = meter(0.0, false);
        let loud = meter(0.15, true);
        assert_ne!(quiet, loud, "the meter does not respond to sound");
        assert!(
            loud.matches('▮').count() > quiet.matches('▮').count(),
            "louder did not read as fuller: {quiet:?} vs {loud:?}"
        );
    }

    /// Silence shows an empty meter, never nothing — a meter that vanished
    /// would read as the microphone having stopped.
    #[test]
    fn silence_still_draws_a_meter() {
        assert!(meter(0.0, false).contains('▯'));
    }

    /// Speech sits well below full scale, so a meter against full scale would
    /// never leave the first cell.
    #[test]
    fn ordinary_speech_moves_the_meter_off_the_floor() {
        assert!(
            meter(0.05, true).contains('▮'),
            "a normal speaking level did not register"
        );
    }

    /// Sentences overlap — one transcribes while the next is spoken — and the
    /// pause before words appear needs an explanation on screen.
    #[test]
    fn work_still_in_flight_is_shown() {
        let mut a = app();
        a.dictation = Dictation::Listening {
            since_ms: 0,
            backend: "arecord".into(),
            pending: 2,
            speaking: false,
            level: 0.0,
            heard: 3,
        };
        assert!(dictation_badge(&a).is_some_and(|b| b.contains("2 transcribing")));
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
                "Ctrl-B delegate · Ctrl-G menu",
                "Ctrl-X stop · Ctrl-C quit",
                width,
                MUTED,
            );
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().count() <= width as usize,
                "{width}: overflowed with {text:?}"
            );
            assert!(
                !text.contains("menuCtrl-X"),
                "{width}: they touched: {text:?}"
            );
            // The exit survives every width that can hold it at all.
            if "Ctrl-X stop · Ctrl-C quit".len() + 2 <= width as usize {
                assert!(
                    text.contains("Ctrl-X stop · Ctrl-C quit"),
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
    /// is exactly where the old spelling survives a move. `Ctrl-K` and
    /// `Ctrl-B` outlived the move *to* Alt in the splash caption and two
    /// empty-state sentences, which no table owns; `Alt-K` and `Alt-B` were
    /// sitting in those same three strings after the move back.
    ///
    /// So this one reads the finished screen instead of the source. Anything a
    /// pixel teaches is caught, whatever string it came from — and what must
    /// never be taught is Alt, because a stock macOS terminal cannot send it.
    ///
    /// Nothing is excluded now. The direction reversed with the keymap: the
    /// global table is Ctrl throughout, so `Overlay::Keymap` has no more
    /// licence to print an Alt chord than the splash does, and the exclusion
    /// that used to protect it would now only hide a stale row.
    ///
    /// Everything renders wide, at 150×40, because clipping makes a
    /// buffer-reading assertion lie in *both* directions: a token cut mid-word
    /// fails correct code, and a stale `Alt-B` truncated off the right edge
    /// passes broken code. The count at the end is what proves the width was
    /// actually enough — a scan that found nothing has not passed, it has
    /// failed to look.
    #[test]
    fn no_screen_teaches_an_alt_chord() {
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
                                kept += 1;
                                rest = &rest[at + "Ctrl-".len()..];
                            }
                            assert!(
                                !line.contains("Alt-"),
                                "{ws:?} row {i} teaches an Alt chord, which a stock macOS \
                                 terminal cannot send: {line}"
                            );
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
            "no Ctrl chord reached the buffer, so nothing was really scanned"
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
            (Workspace::Chat, "Ctrl-G menu"),
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
    /// terminal — `1 queuedCtrl-X stop`, which reads as neither. The hints now
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

        // With room for both, the keybar carries the exits again. `Ctrl-X`
        // because Alt cannot be typed on a stock macOS terminal, and `x` is
        // free of tmux; `Ctrl-C` stayed where every terminal already puts it.
        let wide = rendered(&a, 150, 12);
        let rows: Vec<&str> = wide.lines().collect();
        assert!(rows[rows.len() - 2].contains("Ctrl-X stop"), "{wide}");
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
        assert!(screen.contains("Ctrl-G"));
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

    /// `x` deleting a webhook silently is one fat-fingered `Ctrl-G h x` away
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
        // The whole warning and the whole footer, on the dialog's own border
        // rows. "cannot be undone" and "y confirms" both fitted the broken
        // 25-column box, which is why this test passed while the dialog on
        // screen read "this cannot be undo" / "y confirms · anythi" — and the
        // footer alone is no good either, because the keybar prints the same
        // sentence at the bottom of the screen whatever the dialog does.
        assert!(screen.contains("┌ this cannot be undone "), "{screen}");
        assert!(
            screen.contains("└ y confirms · anything else cancels "),
            "{screen}"
        );
    }

    /// BUG-20: the panel was sized from the question alone — `question + 8` —
    /// while its own border titles are 23 and 36 characters wide. The severity
    /// scales *inversely* with the name being destroyed, so the worst case is
    /// the shortest one: `forget x` gave a 17-column box and a warning reading
    /// "this canno".
    #[test]
    fn the_shortest_destructive_name_still_gets_the_whole_warning() {
        let mut a = app();
        a.overlay = Overlay::Confirm {
            verb: "forget".into(),
            what: "x".into(),
        };
        let screen = rendered(&a, 200, 24);
        assert!(screen.contains("forget x?"), "{screen}");
        assert!(screen.contains("┌ this cannot be undone "), "{screen}");
        assert!(
            screen.contains("└ y confirms · anything else cancels "),
            "the dialog itself says what cancels:\n{screen}"
        );
    }

    /// The same dialog on a terminal too narrow to seat the footer whole. It
    /// still may not stop mid-word: something has to say text was dropped.
    #[test]
    fn a_narrow_confirmation_marks_what_it_could_not_fit() {
        let mut a = app();
        a.overlay = Overlay::Confirm {
            verb: "forget".into(),
            what: "x".into(),
        };
        let screen = rendered(&a, 30, 12);
        let footer = screen
            .lines()
            .find(|line| line.contains("y confirms"))
            .expect("the dialog draws its footer");
        assert!(
            footer.contains('…'),
            "the drop is marked on the border row:\n{screen}"
        );
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

    /// Where a run *ran* is a fact about whether it did what you asked.
    ///
    /// A delegated run once wrote an entire project into the home directory,
    /// outside every declared root, and was recorded `✓ done` with the money
    /// spent — while the directory the user had actually pointed at stayed
    /// empty. Nothing on any screen would have told them: the store has always
    /// recorded the directory, and every pane dropped it on the way out.
    #[test]
    fn the_run_detail_says_which_directory_the_run_was_launched_in() {
        let mut a = app();
        a.agents = vec![super::super::AgentLine {
            cwd: "/srv/reljod/tetris".into(),
            ..agent_line("abcdef1234", "build a tetris game", "running")
        }];
        a.go(Workspace::Fleet);
        let out = rendered(&a, 120, 24);
        assert!(
            out.contains("/srv/reljod/tetris"),
            "the run detail has to say where it ran:\n{out}"
        );
    }

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
            cwd: "/srv/reljod/repo".into(),
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
        assert!(screen.contains("⏎ enter"), "{screen}");
        assert!(
            screen.contains("pinned, and it never ends"),
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

    /// A background shell is invisible by construction, so the two places it
    /// can be seen are the panel and the always-on status row. Both, or
    /// backgrounding an update means losing track of it.
    #[test]
    fn the_jobs_panel_shows_what_is_running_and_what_it_is_doing() {
        let mut a = populated();
        a.now_ms = 60_000;
        let job = a.job_start("update", 0);
        a.job_line(job, "→ Building v1.2.3 — this takes a few minutes the first time");

        let status = rendered(&a, 100, 30);
        assert!(
            status.contains("1 running"),
            "a running shell is on the status row: {status}"
        );

        a.overlay = Overlay::Jobs;
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("background shells"), "{screen}");
        assert!(screen.contains("update"), "the job is named: {screen}");
        assert!(screen.contains("1m00s"), "and aged: {screen}");
        assert!(
            screen.contains("Building v1.2.3"),
            "with what it is doing now: {screen}"
        );
    }

    /// A finished job stays listed. "Did that update work" is asked after it
    /// ended, and a panel that emptied itself on completion could not answer.
    #[test]
    fn a_finished_job_stays_in_the_panel_with_how_it_ended() {
        let mut a = populated();
        let job = a.job_start("update", 0);
        a.job_done(job, false, 2_000);
        a.now_ms = 90_000;
        a.overlay = Overlay::Jobs;
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("✗"), "a failure is marked as one: {screen}");
        assert!(
            screen.contains("2s"),
            "and its duration stopped when it did: {screen}"
        );
    }

    /// The reload question must not borrow the delete confirmation's "this
    /// cannot be undone" — restarting a console is neither.
    #[test]
    fn the_reload_question_says_what_it_costs_and_what_it_does_not() {
        let mut a = populated();
        a.overlay = Overlay::ConfirmReload;
        let screen = rendered(&a, 100, 30);
        assert!(screen.contains("restart this console"), "{screen}");
        assert!(screen.contains("agents keep running"), "{screen}");
        assert!(
            !screen.contains("cannot be undone"),
            "a reload is reversible and must not be dressed as a deletion: {screen}"
        );
    }
    // ---- the decision rail ----

    /// A conversation with four cards in it, one of them already answered.
    ///
    /// Built against a **real store** rather than by assigning to `app.cards`,
    /// because "the answered one is hidden until toggled" is a fact about the
    /// query the rail issues, not about the renderer. A fixture assigned by
    /// hand would assert that the renderer draws whatever it is given, which
    /// nobody doubted.
    fn rail_store() -> (RealStore, String) {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let raise = |title: &str, kind: CardKind, blocking: bool, options: Vec<String>| {
            store
                .raise_card(NewCard {
                    conversation_id: conversation.clone(),
                    run_id: Some("3f2ab1c0".into()),
                    kind: Some(kind),
                    importance: Some(if blocking {
                        Importance::High
                    } else {
                        Importance::Normal
                    }),
                    blocking,
                    title: title.into(),
                    body: "The alternatives were weighed and one was picked.".into(),
                    options,
                    ..Default::default()
                })
                .expect("a card")
        };
        raise(
            "chat DB: chose SQLite",
            CardKind::Decision,
            false,
            vec!["SQLite".into(), "Postgres".into()],
        );
        raise("which port for the API?", CardKind::Question, true, vec![]);
        raise("GITHUB_TOKEN is missing", CardKind::Secret, false, vec![]);
        let answered = raise("retry the flaky test?", CardKind::Question, false, vec![]);
        store
            .answer_card(answered.id, None, Some("yes, twice"))
            .expect("answering queues");
        (store, conversation)
    }

    /// Read the rail exactly as the running program does: one query, built from
    /// the rail's own state.
    fn rail_app(store: &RealStore, conversation: &str, rail: RailState) -> App {
        let mut a = app();
        a.conversation = Some(conversation.to_string());
        a.rail = rail;
        a.rail.shown = true;
        a.cards = store
            .cards(&a.rail.query(a.conversation.clone()))
            .expect("the rail's query");
        a.reconcile_rail();
        a
    }

    /// **E2's check, word for word:** a rendered frame showing three cards, one
    /// bordered `blocked`, the answered one hidden until toggled.
    #[test]
    fn the_rail_shows_three_cards_one_bordered_blocked_and_the_answered_one_only_on_toggle() {
        let (store, conversation) = rail_store();

        let open = rail_app(&store, &conversation, Default::default());
        assert_eq!(open.cards.len(), 3, "the answered card left the stack");
        let frame = rendered(&open, 150, 40);
        assert!(frame.contains("chat DB: chose SQLite"), "{frame}");
        assert!(frame.contains("which port for the API?"), "{frame}");
        assert!(frame.contains("GITHUB_TOKEN is missing"), "{frame}");
        assert!(
            frame.contains(rail::BLOCKED),
            "the blocking card must carry the word as well as the colour: {frame}"
        );
        assert!(
            !frame.contains("retry the flaky test?"),
            "an answered card is out of the stack until it is toggled back: {frame}"
        );
        // Bordered, not merely listed: three cards means three boxes.
        assert!(
            frame.matches('┌').count() >= 3,
            "each card gets a border of its own: {frame}"
        );

        // `t` cycles the stack, and the answered card comes back — saying both
        // what the human did and whether the agent has heard.
        let mut rail = RailState::default();
        rail.cycle_stack();
        let answered = rail_app(&store, &conversation, rail);
        let frame = rendered(&answered, 150, 40);
        assert!(frame.contains("retry the flaky test?"), "{frame}");
        assert!(
            frame.contains("answered, queued"),
            "an answer is asynchronous and the rail must not pretend otherwise: {frame}"
        );
    }

    /// The lie D2 exists to prevent. Answering while a turn is in flight queues
    /// the answer; a rail that printed "answered" alone would send the reader
    /// back to watch for a change that is not due yet.
    #[test]
    fn an_expanded_answered_card_says_when_the_agent_will_actually_hear() {
        let (store, conversation) = rail_store();
        let mut rail = RailState::default();
        rail.cycle_stack();
        rail.expanded = true;
        let a = rail_app(&store, &conversation, rail);
        let frame = rendered(&a, 150, 40);
        assert!(
            frame.contains("answered, queued"),
            "{frame}"
        );
        assert!(
            frame.contains("end of the turn"),
            "it has to say when, not only that it is waiting: {frame}"
        );
    }

    /// E2.S4: the full card, with the options numbered as the keys that pick
    /// them and the run that raised it named.
    #[test]
    fn an_expanded_card_numbers_its_options_and_names_who_raised_it() {
        let (store, conversation) = rail_store();
        let mut rail = RailState::default();
        rail.expanded = true;
        let mut a = rail_app(&store, &conversation, rail);
        // The decision, which is the one with options on it.
        a.rail.selected = a
            .cards
            .iter()
            .find(|c| c.kind == CardKind::Decision)
            .map(|c| c.id);
        let frame = rendered(&a, 150, 40);
        assert!(frame.contains("1 SQLite"), "{frame}");
        assert!(frame.contains("2 Postgres"), "{frame}");
        assert!(frame.contains("press the digit"), "{frame}");
        assert!(frame.contains("raised by"), "{frame}");
        assert!(
            frame.contains("3f2ab1c0"),
            "the run that raised it is the first thing anyone asks: {frame}"
        );
    }

    /// Reljod's own answer to "rail or third column": on a narrow terminal it
    /// is one line, not a squeezed rail.
    #[test]
    fn a_narrow_terminal_gets_one_line_instead_of_a_squeezed_rail() {
        let (store, conversation) = rail_store();
        let a = rail_app(&store, &conversation, Default::default());

        let wide = rendered(&a, 150, 40);
        assert!(wide.contains("chat DB: chose SQLite"), "{wide}");

        let narrow = rendered(&a, 78, 30);
        assert!(
            narrow.contains("3 cards"),
            "the one-liner still says how many: {narrow}"
        );
        assert!(
            narrow.contains("1 blocked"),
            "and that one of them stopped a run: {narrow}"
        );
        assert!(
            narrow.contains("Ctrl-N"),
            "and which key answers it: {narrow}"
        );
        assert!(
            !narrow.contains("chat DB: chose SQLite"),
            "a squeezed rail is what this replaces: {narrow}"
        );
    }

    /// A rail that is merely shown does not own the keyboard, and the bar has
    /// to say which of the two states it is in — the letters mean different
    /// things in each.
    #[test]
    fn the_keybar_carries_the_rails_verbs_only_while_the_rail_has_the_keyboard() {
        let (store, conversation) = rail_store();
        let mut a = rail_app(&store, &conversation, Default::default());

        let watching = rendered(&a, 150, 40);
        assert!(!watching.contains("x dismiss"), "{watching}");

        a.rail.focused = true;
        let holding = rendered(&a, 150, 40);
        assert!(holding.contains("x dismiss"), "{holding}");
        assert!(
            holding.contains("Esc back to the chat"),
            "the way out is always printed: {holding}"
        );
    }

    /// A hidden rail costs the chat nothing, which is what makes `Ctrl-R` worth
    /// pressing rather than something you turn off once and forget.
    #[test]
    fn a_hidden_rail_takes_no_columns_at_all() {
        let (store, conversation) = rail_store();
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;
        let frame = rendered(&a, 150, 40);
        assert!(!frame.contains("chat DB: chose SQLite"), "{frame}");
        assert!(!frame.contains(" rail ·"), "{frame}");
    }

    /// An empty rail says *why* it is empty. "No cards" after typing a filter
    /// reads as "the agents stopped asking".
    #[test]
    fn an_empty_rail_says_which_kind_of_empty_it_is() {
        let (store, conversation) = rail_store();
        let mut rail = RailState::default();
        rail.filter = Some("zzzz".into());
        let filtered = rail_app(&store, &conversation, rail);
        assert!(filtered.cards.is_empty());
        let frame = rendered(&filtered, 150, 40);
        assert!(frame.contains("nothing matches"), "{frame}");

        let mut bare = rail_app(&store, &conversation, Default::default());
        bare.cards.clear();
        bare.reconcile_rail();
        let frame = rendered(&bare, 150, 40);
        // A fragment rather than the whole sentence: thirty-four columns wrap
        // it, and asserting the wrapped shape would pin the rail's width.
        assert!(frame.contains("nothing waiting"), "{frame}");
    }

    /// Colour by kind, weight by importance, and `blocked` overriding both —
    /// asserted here because a flattened frame carries symbols and not styles.
    #[test]
    fn a_cards_border_takes_its_colour_from_the_kind_and_its_weight_from_the_importance() {
        let base = |kind: CardKind, importance: Importance, blocking: bool| Card {
            id: 1,
            conversation_id: "c".into(),
            work_id: None,
            run_id: None,
            kind,
            importance,
            blocking,
            status: Status::Open,
            delivery: jod_core::cards::Delivery::None,
            title: "t".into(),
            body: String::new(),
            options: vec![],
            chosen: None,
            answer: None,
            secret_name: None,
            secret_scope: None,
            source: jod_core::cards::Source::Mcp,
            created_at_ms: 0,
            updated_at_ms: 0,
            answered_at_ms: None,
            delivered_at_ms: None,
            dedupe_key: None,
        };

        assert_eq!(
            card_colour(&base(CardKind::Decision, Importance::Normal, false)),
            USER
        );
        assert_eq!(
            card_colour(&base(CardKind::Question, Importance::Normal, false)),
            WARN
        );
        // Blocking outranks kind: a blocking secret and a blocking question are
        // the same shade of "this run has stopped".
        assert_eq!(
            card_colour(&base(CardKind::Secret, Importance::Low, true)),
            BAD
        );
        assert!(card_border(&base(CardKind::Decision, Importance::High, false))
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(!card_border(&base(CardKind::Decision, Importance::Normal, false))
            .add_modifier
            .contains(Modifier::BOLD));
    }

    // ---- the plan, inline and in place ----

    fn todo_call(items: &[(&str, &str)]) -> jod_core::AgentEvent {
        jod_core::AgentEvent::ToolCall {
            name: "TodoWrite".into(),
            input: Some(serde_json::json!({
                "todos": items
                    .iter()
                    .map(|(text, status)| serde_json::json!({
                        "content": text, "status": status
                    }))
                    .collect::<Vec<_>>()
            })),
        }
    }

    /// **The slice's whole point**: a revision replaces the block rather than
    /// following it. A harness rewrites its list once per item finished, and
    /// appending would put a dozen near-identical lists between two sentences.
    #[test]
    fn a_revised_plan_replaces_the_block_rather_than_adding_one() {
        let mut a = app();
        a.apply(&todo_call(&[("port the lexer", "in_progress"), ("write the docs", "pending")]));
        a.apply(&todo_call(&[("port the lexer", "completed"), ("write the docs", "in_progress")]));

        let blocks = a
            .transcript
            .iter()
            .filter(|e| matches!(e, Entry::Plan(_)))
            .count();
        assert_eq!(blocks, 1, "one block, however many revisions");

        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("1/2"), "and it shows the newest state:\n{frame}");
    }

    /// The block stays where it first appeared. Its position says when the
    /// agent started planning, and a block that jumped to the bottom on every
    /// revision would be a second kind of noise in place of the first.
    #[test]
    fn the_plan_block_stays_where_it_first_appeared() {
        let mut a = app();
        a.apply(&todo_call(&[("port the lexer", "pending")]));
        a.push(Entry::Agent("starting on the lexer now".into()));
        a.apply(&todo_call(&[("port the lexer", "completed")]));

        assert!(
            matches!(a.transcript.first(), Some(Entry::Plan(_))),
            "still first: {:?}",
            a.transcript
        );
        assert!(matches!(a.transcript.last(), Some(Entry::Agent(_))));
    }

    #[test]
    fn the_plan_renders_inline_with_a_glyph_per_state() {
        let mut a = app();
        a.apply(&todo_call(&[
            ("port the lexer", "completed"),
            ("write the docs", "in_progress"),
            ("cut a release", "pending"),
        ]));
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("plan"), "{frame}");
        assert!(frame.contains("1/3"), "{frame}");
        assert!(frame.contains("port the lexer"), "{frame}");
        assert!(frame.contains("write the docs"), "{frame}");
        // A glyph per state, so the column reads without colour.
        assert!(frame.contains('●'), "done:\n{frame}");
        assert!(frame.contains('◐'), "in flight:\n{frame}");
        assert!(frame.contains('○'), "pending:\n{frame}");
    }

    // ---- diffs render as diffs ----

    /// **E7's check, the second half**: a rendered frame shows a file edit as a
    /// diff rather than as a one-line summary.
    #[test]
    fn a_file_edit_renders_as_a_diff_with_the_path_as_a_header() {
        let mut a = app();
        a.apply(&jod_core::AgentEvent::ToolCall {
            name: "Edit".into(),
            input: Some(serde_json::json!({
                "file_path": "cli/src/tui/ui.rs",
                "old_string": "let width = 80;\nlet height = 24;\n",
                "new_string": "let width = 100;\nlet height = 24;\n",
            })),
        });
        let frame = rendered(&a, 120, 30);

        assert!(frame.contains("cli/src/tui/ui.rs"), "the path is a header:\n{frame}");
        assert!(frame.contains("+1 -1"), "and the shape of the change:\n{frame}");
        assert!(
            frame.contains("-let width = 80;"),
            "the removed line, signed:\n{frame}"
        );
        assert!(
            frame.contains("+let width = 100;"),
            "the added line, signed:\n{frame}"
        );
        assert!(
            frame.contains(" let height = 24;"),
            "and unchanged context around it:\n{frame}"
        );
    }

    /// BUG-21: `docs/try-it.md` promises "the path as a header **and counts**".
    /// The header laid an absolute path out at full length and appended the
    /// counts after it, so both informative parts — the filename and the
    /// `+N -M` — ran off the right edge, and the path was cut mid-word with no
    /// marker. In a worktree that is every path.
    #[test]
    fn a_deep_absolute_path_keeps_its_filename_and_its_counts() {
        let deep = "/Users/reljodoreta/Developer/Repositories/Projects/Jod\
                    /.claude/worktrees/tui-dogfood-tetris/tetris/NOTES.md";
        let mut a = app();
        a.apply(&jod_core::AgentEvent::ToolCall {
            name: "Write".into(),
            input: Some(serde_json::json!({
                "file_path": deep,
                "content": "# Tetris\n",
            })),
        });
        let frame = rendered(&a, 100, 30);
        let header = frame
            .lines()
            .find(|line| line.contains("±"))
            .expect("the diff draws a header");
        assert!(
            header.contains("NOTES.md"),
            "the filename is the point of a path header:\n{frame}"
        );
        assert!(
            header.contains("+1 -0"),
            "and the counts the docs promise:\n{frame}"
        );
        assert!(header.contains('…'), "the head is marked dropped:\n{frame}");
    }

    /// Everything that is not an edit keeps its one-line summary, so the
    /// transcript does not turn into a wall of diffs.
    #[test]
    fn a_tool_that_is_not_an_edit_still_renders_as_one_line() {
        let mut a = app();
        a.apply(&jod_core::AgentEvent::ToolCall {
            name: "Bash".into(),
            input: Some(serde_json::json!({ "command": "cargo test" })),
        });
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("Bash · cargo test"), "{frame}");
        assert!(!frame.contains('±'), "no diff header:\n{frame}");
    }

    // ---- searching every transcript ----

    /// The search is across *every* conversation, so a hit that did not say
    /// where it came from would be a line of prose you cannot decide to open.
    #[test]
    fn the_search_names_the_conversation_each_hit_is_in() {
        let mut a = app();
        a.overlay = Overlay::Search {
            query: "lexer".into(),
            selected: 0,
            hits: vec![
                crate::tui::data::Hit {
                    conversation_id: "conv-a".into(),
                    title: "the parser".into(),
                    who: "agent".into(),
                    text: "porting the lexer now".into(),
                },
                crate::tui::data::Hit {
                    conversation_id: "conv-b".into(),
                    title: "the deploy".into(),
                    who: "you".into(),
                    text: "does the lexer matter here".into(),
                },
            ],
        };
        let frame = rendered(&a, 120, 24);
        assert!(frame.contains("the parser"), "{frame}");
        assert!(frame.contains("the deploy"), "{frame}");
        assert!(frame.contains("porting the lexer"), "{frame}");
        assert!(frame.contains("opens that conversation"), "{frame}");
    }

    /// An empty box says what it is for rather than listing every message ever.
    #[test]
    fn an_empty_search_box_says_what_it_searches() {
        let mut a = app();
        a.overlay = Overlay::Search {
            query: String::new(),
            selected: 0,
            hits: vec![],
        };
        let frame = rendered(&a, 120, 24);
        assert!(frame.contains("compacted turns included"), "{frame}");
    }

    // ---- the fleet tree ----

    fn tree_node(
        id: jod_core::tree::NodeId,
        parent: Option<jod_core::tree::NodeId>,
        kind: jod_core::tree::NodeKind,
        depth: usize,
        label: &str,
    ) -> jod_core::tree::Node {
        jod_core::tree::Node {
            id,
            parent,
            kind,
            depth,
            label: label.into(),
            summary: String::new(),
            running: false,
            cards: 0,
            blocked: 0,
            colour: "cyan".into(),
            expanded: true,
            has_children: false,
        }
    }

    /// **E5's check**: two works, four sessions, one expanded run, and a
    /// blocked count in the gutter.
    fn two_works() -> App {
        use jod_core::tree::{NodeId, NodeKind};
        let mut a = app();

        let mut parser = tree_node(NodeId::work("w1"), None, NodeKind::Work, 0, "the parser");
        parser.has_children = true;
        parser.cards = 3;
        parser.blocked = 1;
        parser.colour = "cyan".into();

        let mut lexer = tree_node(
            NodeId::session("s1"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            1,
            "port the lexer",
        );
        lexer.has_children = true;
        lexer.running = true;
        lexer.cards = 1;
        lexer.blocked = 1;
        lexer.summary = "editing tokens.rs".into();

        let mut run = tree_node(
            NodeId::run("r1"),
            Some(NodeId::session("s1")),
            NodeKind::Run,
            2,
            "cargo test",
        );
        run.running = true;

        let docs = tree_node(
            NodeId::session("s2"),
            Some(NodeId::work("w1")),
            NodeKind::Session,
            1,
            "write the docs",
        );

        let mut deploy = tree_node(NodeId::work("w2"), None, NodeKind::Work, 0, "the deploy");
        deploy.has_children = true;
        deploy.colour = "green".into();

        let ci = tree_node(
            NodeId::session("s3"),
            Some(NodeId::work("w2")),
            NodeKind::Session,
            1,
            "fix the CI",
        );
        let release = tree_node(
            NodeId::session("s4"),
            Some(NodeId::work("w2")),
            NodeKind::Session,
            1,
            "cut a release",
        );

        a.forest = vec![parser, lexer, run, docs, deploy, ci, release];
        a.go(Workspace::Fleet);
        let rows = a.tree_rows();
        a.tree.reconcile(&rows);
        a
    }

    #[test]
    fn the_fleet_tree_draws_two_works_four_sessions_and_an_expanded_run() {
        let a = two_works();
        let frame = rendered(&a, 150, 30);

        assert!(frame.contains("the parser"), "{frame}");
        assert!(frame.contains("the deploy"), "{frame}");
        for session in [
            "port the lexer",
            "write the docs",
            "fix the CI",
            "cut a release",
        ] {
            assert!(frame.contains(session), "{session} missing:\n{frame}");
        }
        assert!(
            frame.contains("cargo test"),
            "the expanded run:\n{frame}"
        );
        assert!(
            frame.contains("1 blocked"),
            "the blocked count in the gutter:\n{frame}"
        );
        // Guides, so the shape reads as a tree rather than as an indented list.
        assert!(frame.contains('├') || frame.contains('└'), "{frame}");
    }

    /// The pinned chat is the tree's first row, as it is the flat list's.
    ///
    /// The tree replaces that list whole, so losing the row with it left a fleet
    /// that had grown a single work with no way back into the chat at all. The
    /// star is what identifies it: `main_line` is the only thing that draws one.
    #[test]
    fn the_fleet_tree_pins_the_chat_above_the_works() {
        let mut a = two_works();
        a.tree.selected = Some(crate::tui::fleet::main_id());
        let frame = rendered(&a, 150, 30);

        let row = |needle: &str| {
            frame
                .lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle} is not on screen:\n{frame}"))
        };
        assert!(
            row("★") < row("the parser"),
            "the pinned chat is not above the works:\n{frame}"
        );
        // And its own detail pane: none of `kind`, `id` or `state` means
        // anything to a conversation.
        assert!(frame.contains("the chat"), "{frame}");
    }

    /// Collapsing a work takes its sessions off the screen and leaves the other
    /// work alone — the property the whole navigation hangs off.
    #[test]
    fn collapsing_a_work_hides_only_its_own_sessions() {
        let mut a = two_works();
        a.tree.selected = Some(jod_core::tree::NodeId::work("w1"));
        let closed = a.closed_works.clone();
        a.tree.toggle(&closed);

        let frame = rendered(&a, 150, 30);
        assert!(!frame.contains("port the lexer"), "{frame}");
        assert!(!frame.contains("cargo test"), "{frame}");
        assert!(frame.contains("the parser"), "the work itself stays:\n{frame}");
        assert!(frame.contains("fix the CI"), "the other work is untouched:\n{frame}");
    }

    /// A filter keeps the path to every hit, so a matching session never floats
    /// at a depth with nothing above it.
    #[test]
    fn a_filtered_tree_keeps_the_work_above_every_hit() {
        let mut a = two_works();
        // Through the screen's own `/`, which is what the key actually writes
        // to — and `reconcile` after it, because a cursor left on a
        // filtered-out node would keep the detail pane on a row the list no
        // longer shows. That was a real bug this test found.
        a.list_mut(Workspace::Fleet).filter = Some("release".into());
        a.reconcile();
        let frame = rendered(&a, 150, 30);
        assert!(frame.contains("cut a release"), "{frame}");
        assert!(
            frame.contains("the deploy"),
            "its work is kept as the path to it:\n{frame}"
        );
        assert!(!frame.contains("port the lexer"), "{frame}");
        assert!(!frame.contains("the parser"), "{frame}");
    }

    /// The declared drop order: the summary goes first, the label never.
    #[test]
    fn a_narrow_tree_drops_the_summary_before_the_label() {
        let a = two_works();
        let wide = rendered(&a, 150, 30);
        assert!(wide.contains("editing tokens.rs"), "{wide}");

        // Narrow enough that the drop actually bites: the row's fixed part —
        // guides, marker, glyph, label, spinner, card badge — is already near
        // forty columns, so the summary only goes when there is less than
        // `LEAST_TEXT` left after it.
        let narrow = rendered(&a, 50, 30);
        assert!(
            narrow.contains("port the lexer"),
            "the label survives every width:\n{narrow}"
        );
        assert!(
            !narrow.contains("editing tokens.rs"),
            "the summary is the first thing to go:\n{narrow}"
        );
    }

    /// With no works the screen is the older flat list, because a session that
    /// belongs to no work has no node in the forest.
    #[test]
    fn the_fleet_falls_back_to_its_list_when_there_are_no_works() {
        // Its own fixture rather than `tui::tests`'s: a test module cannot see
        // a sibling's helpers, and reaching for one is what left this file not
        // compiling.
        let mut a = app();
        a.agents = vec![
            agent_line("aaa11111", "port the parser", "running"),
            agent_line("bbb22222", "write the docs", "completed"),
        ];
        a.go(Workspace::Fleet);
        assert!(!a.has_tree());
        let frame = rendered(&a, 150, 30);
        assert!(frame.contains("aaa11111"), "the flat list still draws:\n{frame}");
    }

    /// A work and a loose run at the same time, which is the case the empty
    /// fleet above cannot cover.
    ///
    /// A run started by `delegate` belongs to no work, so its conversation has
    /// no `work_id` and `Store::forest_of` — which reads only conversations
    /// that have one — gives it no node. The tree therefore cannot show it, and
    /// the flat list is the half of the screen that can. Built off a real store
    /// rather than a hand-made forest, because the claim being made here is
    /// about what the query returns as much as about what is drawn.
    #[test]
    fn the_fleet_still_shows_a_run_that_belongs_to_no_work() {
        use jod_core::tree::NodeId;
        use jod_core::works::Origin;

        let store = RealStore::in_memory().expect("an in-memory store");
        let work = store.create_work("port the parser").expect("a work");
        store
            .set_work_title(&work.id, "the parser")
            .expect("a work title");
        let lead = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .set_conversation_title(&lead, "port the lexer")
            .expect("a session title");
        store
            .attach_conversation(&lead, &work.id, None, Origin::Agent)
            .expect("a session under the work");

        // The delegated run. `attach_conversation` is never called for it, so
        // its `work_id` stays null — which is exactly what `delegate` leaves
        // behind.
        let loose = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .set_conversation_title(&loose, "say potato")
            .expect("a title");
        store
            .save_run(&jod_core::store::StoredRun {
                id: "de1e6a7e".into(),
                name: "hello-agent".into(),
                harness: "claude-code".into(),
                status: "running".into(),
                cwd: "/tmp".into(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: 1,
                summary: serde_json::json!({}),
            })
            .expect("a run");
        store
            .append_message(
                &loose,
                jod_core::conversation::NewMessage::new(
                    jod_core::conversation::Role::Assistant,
                    "on it",
                )
                .from_run("de1e6a7e"),
            )
            .expect("a message");

        let mut a = app();
        a.forest = store.forest().expect("a forest");
        a.agents = vec![agent_line("de1e6a7e", "hello-agent", "running")];
        a.go(Workspace::Fleet);
        a.reconcile();

        assert!(a.has_tree(), "one work is enough to put a tree on screen");
        assert!(
            !a.forest.iter().any(|n| n.id == NodeId::run("de1e6a7e")),
            "the loose run has no node, which is the whole reason for the list",
        );

        let frame = rendered(&a, 150, 30);
        assert!(
            frame.contains("the parser"),
            "the tree is still drawn:\n{frame}"
        );
        assert!(
            frame.contains("de1e6a7e"),
            "the delegated run is on the screen too:\n{frame}"
        );
        assert!(
            frame.contains("hello-agent"),
            "and it is named, not just numbered:\n{frame}"
        );
    }

    // ---- the secret card ----

    /// The moment to learn where a production token is going is before pasting
    /// it, so the destination is on the card as well as in the field.
    #[test]
    fn an_expanded_secret_card_says_where_the_value_will_live() {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .raise_card(NewCard {
                conversation_id: conversation.clone(),
                kind: Some(CardKind::Secret),
                title: "GITHUB_TOKEN is missing".into(),
                secret_name: Some("GITHUB_TOKEN".into()),
                secret_scope: Some("work".into()),
                ..Default::default()
            })
            .expect("a card");

        let mut rail = RailState {
            expanded: true,
            ..Default::default()
        };
        rail.shown = true;
        let a = rail_app(&store, &conversation, rail);
        // Fragments short enough to survive the pane's wrap. The *wording* is
        // pinned in `secret::tests`, which reads the unwrapped source; what
        // this asserts is that the block reaches the screen at all.
        let frame = rendered(&a, 150, 40);
        assert!(frame.contains("stored outside"), "{frame}");
        assert!(frame.contains("0600"), "{frame}");
        assert!(
            frame.contains("only this work's sessions"),
            "the scope as who can use it: {frame}"
        );
        assert!(
            frame.contains("never echoed"),
            "and how the field will behave: {frame}"
        );
    }

    /// The one part of this flow a user cannot undo afterwards. A shoulder, a
    /// screen share and a recorded terminal are all ordinary.
    #[test]
    fn the_credential_field_shows_dots_and_never_the_value() {
        let mut a = app();
        a.transcript.push(Entry::Notice("hello".into()));
        let mut value = secret::Typed::new();
        for c in "sk-live-abcdef".chars() {
            value.push(c);
        }
        a.overlay = Overlay::Secret {
            card: 9,
            name: "GITHUB_TOKEN".into(),
            scope: jod_core::secrets::Scope::Work,
            value,
        };
        let frame = rendered(&a, 120, 34);
        assert!(!frame.contains("sk-live"), "the value is on screen: {frame}");
        assert!(frame.contains("••••••••••••••"), "{frame}");
        assert!(frame.contains("GITHUB_TOKEN"), "the name is not a secret: {frame}");
        assert!(frame.contains("Esc discards it"), "{frame}");
    }

    // ---- cascading cards ----

    /// E4.S5: with the subtree scope on, the rail holds cards from agents all
    /// over the fleet and answering writes against one of them, so every row
    /// has to say whose question it is.
    #[test]
    fn a_cascaded_rail_names_the_session_on_every_card() {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .raise_card(NewCard {
                conversation_id: conversation.clone(),
                run_id: Some("3f2ab1c0".into()),
                title: "which port for the API?".into(),
                ..Default::default()
            })
            .expect("a card");

        let mut a = rail_app(&store, &conversation, RailState::default());
        assert!(a.rail.cascade, "the orchestrator's rail is the default");
        let frame = rendered(&a, 150, 40);
        let session: String = conversation.chars().take(8).collect();
        assert!(frame.contains(&session), "the session that raised it: {frame}");
        assert!(frame.contains("3f2a"), "and which run: {frame}");
        assert!(
            frame.contains("· subtree"),
            "the scope is on the header, always: {frame}"
        );

        // Narrowed to one conversation, every card came from the same place,
        // so the line spends its columns on the card instead — and the header
        // says which of the two states this is, because a narrowed rail and a
        // quiet fleet look identical otherwise.
        a.rail.cascade = false;
        let frame = rendered(&a, 150, 40);
        assert!(frame.contains("· here"), "{frame}");
        assert!(
            !frame.contains(&session),
            "narrowed, every card came from here, so the columns go to the card: {frame}"
        );
    }

    // ---- the traffic log ----

    /// A work with three agents on its bus, one exchange threaded, and one
    /// message sent to somebody who is not a member of it.
    ///
    /// Built against a **real store** and read through `data::traffic_from` —
    /// the loader the tick actually calls — rather than by assigning to
    /// `app.traffic`. Threading, the depth, and the sentence explaining a
    /// refusal are all facts about what `Store::post` wrote; a fixture built by
    /// hand would assert that the renderer draws whatever it is handed, which
    /// nobody doubted.
    fn traffic_store() -> (RealStore, String) {
        let store = RealStore::in_memory().expect("an in-memory store");
        let work = store.create_work("port the parser").expect("a work");
        store
            .set_work_title(&work.id, "port the parser")
            .expect("a title");
        for name in ["asker", "answerer", "scribe"] {
            store
                .join_scope(
                    Scope::Work,
                    &work.id,
                    name,
                    HarnessKind::ClaudeCode,
                    "engineer",
                    None,
                )
                .expect("a member");
        }

        let post = |from: &str, to: &str, text: &str, replying_to: Option<i64>| -> Sent {
            let mut p = Post::new(Scope::Work, &work.id, from, text).to(to);
            if let Some(id) = replying_to {
                p = p.replying_to(id);
            }
            store.post(&p).expect("a post")
        };

        let question = match post("asker", "answerer", "where does the lexer live?", None) {
            Sent::Queued { ids, .. } => ids[0],
            other => panic!("the question did not go on the bus: {other:?}"),
        };
        post(
            "answerer",
            "asker",
            "in core, beside the tokeniser",
            Some(question),
        );
        post("scribe", "asker", "the docs are written", None);
        // The one that could not be delivered. `reljod` is the person's name on
        // this bus and *is* a member of the work, so the refusal a real fleet
        // produces needs a name that is not — exactly as `tests/e2e/jod/out/a2a.txt`
        // records it.
        let refused = post("asker", "nobody-here", "are you free?", None);
        assert!(
            matches!(refused, Sent::Undeliverable { .. }),
            "the fixture must actually produce a refusal: {refused:?}"
        );
        (store, work.id)
    }

    fn traffic_app(store: &RealStore, work: &str) -> App {
        let mut a = app();
        a.traffic_of = Some(traffic::Watching::work(work));
        a.traffic = crate::tui::data::traffic_from(store, a.traffic_of.as_ref().unwrap());
        a.go(Workspace::Traffic);
        a
    }

    /// **G5's check, word for word:** a rendered frame showing a threaded
    /// exchange between three members with one undelivered message marked.
    #[test]
    fn the_traffic_log_shows_a_threaded_exchange_between_three_members_with_one_undelivered() {
        let (store, work) = traffic_store();
        let a = traffic_app(&store, &work);
        let frame = rendered(&a, 150, 30);

        for member in ["asker", "answerer", "scribe"] {
            assert!(frame.contains(member), "{member} is not on the log:\n{frame}");
        }

        // Threaded: the reply is drawn *further in* than the question it
        // answers, which is the whole of "a reply should be visibly under what
        // it answers". Measured as a column rather than as a substring,
        // because an indent is a position and asserting on spaces in a
        // formatted string proves only that the string was formatted.
        let question = column_of(&frame, "asker → answerer");
        let answer = column_of(&frame, "answerer → asker");
        assert!(
            answer > question,
            "the reply is not indented under its question — {answer} vs {question}:\n{frame}"
        );

        // Marked, and marked with the reason rather than with the state word.
        // A8: mail that fails silently is worse than mail that fails.
        assert!(
            frame.contains("undeliverable"),
            "the refusal is not marked:\n{frame}"
        );
        assert!(
            frame.contains("`nobody-here` is not a member of this work"),
            "the row says what went wrong but not why:\n{frame}"
        );
    }

    /// Which column a phrase starts in, or a panic naming the frame — the
    /// indent test is worthless if a missing row reads as column zero.
    fn column_of(frame: &str, needle: &str) -> usize {
        frame
            .lines()
            .find_map(|line| line.find(needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on the frame:\n{frame}"))
    }

    /// G4.S5: the allowance is on screen before it is spent, because the
    /// escalation card is far too late to be the first time anybody hears that
    /// two agents have been talking for two hundred turns.
    #[test]
    fn the_traffic_log_says_what_is_left_of_the_works_message_budget() {
        let (store, work) = traffic_store();
        let a = traffic_app(&store, &work);
        let frame = rendered(&a, 150, 30);
        // Three delivered messages spend the budget; the refused one does not,
        // which is `MailState::counts_against_budget` and is core's rule rather
        // than this screen's.
        assert!(
            frame.contains("197 of 200 messages left"),
            "the budget is not on the screen:\n{frame}"
        );
    }

    /// The work's colour is what tells one work's traffic from another's at a
    /// glance, and it has to reach the header rather than staying in the store.
    #[test]
    fn the_traffic_header_is_tinted_with_the_works_own_colour() {
        let (store, work) = traffic_store();
        let colour = store
            .work(&work)
            .expect("the work")
            .expect("the work exists")
            .colour;
        let a = traffic_app(&store, &work);
        assert_eq!(a.traffic.colour, colour, "the loader dropped the colour");
        let line = traffic_header(&a, 60);
        let glyph = line
            .spans
            .iter()
            .find(|s| s.content.trim() == "■")
            .expect("the header carries a work glyph");
        assert_eq!(
            glyph.style.fg,
            Some(work_colour(&colour)),
            "the glyph is not the work's colour"
        );
        assert!(
            line.spans.iter().any(|s| s.content.contains("port the parser")),
            "and the header names the work"
        );
    }

    /// The screen has to be honest before anything has been opened on it —
    /// an empty box with no explanation reads as a broken screen.
    #[test]
    fn a_traffic_log_with_no_work_chosen_says_how_to_choose_one() {
        let mut a = app();
        a.go(Workspace::Traffic);
        let frame = rendered(&a, 120, 20);
        assert!(frame.contains("T on a fleet row"), "{frame}");
    }

    /// The state cycle narrows the log to what did not arrive, which is the
    /// question this screen is opened to answer.
    #[test]
    fn the_state_filter_narrows_the_log_to_what_never_arrived() {
        let (store, work) = traffic_store();
        let mut a = traffic_app(&store, &work);
        a.traffic_shown = traffic::Shown::Problems;
        a.reconcile();
        let frame = rendered(&a, 150, 30);
        assert!(
            frame.contains("`nobody-here` is not a member of this work"),
            "{frame}"
        );
        assert!(
            !frame.contains("the docs are written"),
            "a delivered message survived the problems filter:\n{frame}"
        );
        // And the screen says it is narrowed. A notice scrolls away; the
        // narrowing does not, so a log that only announced it once would look
        // empty for no reason the next time it was opened.
        assert!(
            frame.contains(traffic::Shown::Problems.label()),
            "the header does not say the log is narrowed:\n{frame}"
        );
    }

    // ---- the `@` picker ----

    /// D1's second requirement: the matched characters are picked out, so a row
    /// says *why* it matched.
    #[test]
    fn a_mention_row_highlights_exactly_the_characters_that_matched() {
        let row = crate::tui::mention::Row {
            label: None,
            path: "core/src/rank.rs".into(),
            positions: vec![9, 10, 11, 12],
        };
        let line = mention_line(&row, true, 56);
        let lit: String = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            // The cursor marker is bold too, and it is not part of the path.
            .filter(|s| s.content.trim() != "▸")
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(lit, "rank");
        let whole: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(whole.contains("core/src/rank.rs"), "{whole}");
    }

    /// Several roots means every row says which one it came from, because
    /// `src/main.rs` names two files when a session can see two repositories.
    #[test]
    fn a_mention_row_from_a_qualified_root_prints_the_root() {
        let row = crate::tui::mention::Row {
            label: Some("jod".into()),
            path: "src/main.rs".into(),
            positions: vec![],
        };
        let whole: String = mention_line(&row, false, 56)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(whole.contains("jod/src/main.rs"), "{whole}");
    }

    /// The spec's own words: with zero roots it says so. An empty list would
    /// read as "no matches" and invite another keystroke that cannot help.
    ///
    /// And it says so with the command that fixes it *from here*. The popup is
    /// open and the cursor is in the chat box; a message naming a shell
    /// command is a message you cannot act on without leaving.
    #[test]
    fn the_picker_with_no_roots_says_so_rather_than_showing_an_empty_list() {
        let mut a = app();
        a.transcript.push(Entry::Notice("hello".into()));
        a.input = "look at @".into();
        a.cursor = a.input.len();
        a.open_mention(8);
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("no folder to search"), "{frame}");
        assert!(frame.contains("/add-dir"), "{frame}");
    }

    /// The full-screen picker says which tree it is walking.
    ///
    /// It mattered less when the base was always the directory `jod` was
    /// launched in — you knew where you were. `/add-dir <path>` makes the base
    /// somewhere you named a moment ago, and a list of bare relative paths
    /// with no header is a list you cannot tell apart from the last one.
    #[test]
    fn the_full_screen_picker_names_the_tree_it_is_walking() {
        let mut a = app();
        a.overlay = Overlay::Picker(picker::Picker::new(
            std::path::PathBuf::from("/home/reljod/notes"),
            vec![".".into(), "daily".into(), "reference".into()],
            false,
        ));
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("/home/reljod/notes"), "{frame}");
        assert!(frame.contains("daily"), "{frame}");
        assert!(frame.contains("⏎ adds it read-only"), "{frame}");
    }

    /// BUG-3: the header was drawn into a panel capped at 96 columns however
    /// wide the terminal was, and clipped by the border with no marker. An
    /// eighteen-character fixture path proved the function, not the feature:
    /// a worktree path came out as `…/tui-dogfood-tetr`, which names a
    /// *different directory* from the real one.
    ///
    /// The tail is the informative end of a path, so it is the end that has to
    /// survive.
    #[test]
    fn the_picker_header_stays_readable_for_a_real_worktree_path() {
        let deep = "/Users/reljodoreta/Developer/Repositories/Projects/Jod\
                    /.claude/worktrees/tui-dogfood-tetris/tetris";
        let mut a = app();
        a.overlay = Overlay::Picker(picker::Picker::new(
            std::path::PathBuf::from(deep),
            vec![".".into()],
            false,
        ));

        // Wide enough for the whole thing: show the whole thing. At 120 too —
        // the old fixed 96-column cap clipped it on both of these.
        for width in [200, 120] {
            let wide = rendered(&a, width, 30);
            assert!(wide.contains(deep), "at {width} columns:\n{wide}");
        }

        // Genuinely too narrow for it: keep the end that tells directories
        // apart, and say that the head was dropped.
        let narrow = rendered(&a, 80, 30);
        assert!(
            narrow.contains("tui-dogfood-tetris/tetris"),
            "the distinguishing tail survives:\n{narrow}"
        );
        // The marker is `elide_left`'s, so it lands on the column rather than
        // on a separator — `…itories/Projects/…`. What matters is that it is
        // there: nothing else on the line says text was dropped.
        assert!(narrow.contains("  in …"), "the drop is marked:\n{narrow}");
        assert!(
            !narrow.contains("tui-dogfood-tetr\n") && !narrow.contains("tui-dogfood-tetr "),
            "never cut mid-word without a marker:\n{narrow}"
        );
    }

    /// With a root set, the popup ranks live under the cursor.
    #[test]
    fn the_picker_ranks_what_is_under_the_cursor() {
        let mut a = app();
        a.transcript.push(Entry::Notice("hello".into()));
        a.roots = vec![jod_core::roots::Root {
            id: 1,
            conversation_id: "c".into(),
            path: std::path::PathBuf::from("/home/reljod/repo/jod"),
            writable: false,
            position: 0,
            origin: jod_core::roots::Origin::Human,
            added_at_ms: 0,
        }];
        a.candidates = vec![std::sync::Arc::new(vec![
            "cli/src/tui/mod.rs".to_string(),
            "core/src/rank.rs".to_string(),
        ])];
        a.input = "@rank".into();
        a.cursor = a.input.len();
        a.open_mention(0);
        a.sync_mention();
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("core/src/rank.rs"), "{frame}");
        assert!(frame.contains("Esc keeps what you typed"), "{frame}");
    }

    // ---- the side panel: advertised, and reachable ----

    /// The panel holds the projects, the sessions, the mode, the harness, the
    /// spend and the context left — a large fraction of the program's state —
    /// and `Shift-Tab` is the only way in. Until it had a row here the only
    /// place the key was written down was the panel's own bottom border, which
    /// you can read only once you have already found it. An overlay that calls
    /// itself the whole keymap and omits the key to a sixth of the program
    /// sends the reader to the source, which is where this key was in fact
    /// found.
    #[test]
    fn the_keymap_names_the_key_that_opens_the_panel() {
        for ws in [Workspace::Chat, Workspace::Fleet] {
            let mut a = populated();
            a.go(ws);
            a.overlay = Overlay::Keymap;
            let screen = rendered(&a, 100, 30);
            assert!(
                screen.contains("Shift-Tab"),
                "{ws:?}: the overlay claims to be the whole keymap:\n{screen}"
            );
        }
    }

    /// And the key it names is the key that works. A row printed in the overlay
    /// is a promise; this is the half of it the drift net cannot check, because
    /// `Shift-Tab` arrives as `BackTab` and carries no Ctrl or Alt for
    /// `is_chord` to recognise.
    #[test]
    fn the_key_the_keymap_names_for_the_panel_is_the_one_that_opens_it() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        assert!(!a.panel, "a cold start has the panel shut");
        crate::tui::on_key(
            &mut a,
            &mut crate::tui::Thread::default(),
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            20,
        );
        let screen = rendered(&a, 120, 30);
        assert!(
            screen.contains("sessions"),
            "the overlay's row for the panel opened nothing:\n{screen}"
        );
    }

    /// Regression, from a **cold start** — the state every user is in and the
    /// one precondition the older test set away. `Ctrl-G d` is advertised in
    /// the workspace menu as `projects · show or hide the catalog`, and the
    /// catalog draws inside the side panel, so while the panel was shut the key
    /// flipped a flag that rendered nothing and said nothing about why.
    #[test]
    fn the_projects_key_shows_the_catalog_from_a_cold_start() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        assert!(!a.panel, "a cold start has the panel shut");
        let mut thread = crate::tui::Thread::default();
        crate::tui::on_key(
            &mut a,
            &mut thread,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            20,
        );
        crate::tui::on_key(
            &mut a,
            &mut thread,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            20,
        );
        let screen = rendered(&a, 120, 30);
        assert!(
            screen.contains("projects"),
            "the projects key drew nothing at all:\n{screen}"
        );
    }

    /// BUG-16: rows longer than the popup used to be clipped on the right,
    /// which is the end that tells one path from another. Six different files
    /// rendered as six identical lines, and choosing between them was
    /// impossible.
    #[test]
    fn two_long_paths_that_differ_only_at_the_end_render_differently() {
        let stem = "tetris/node_modules/.pnpm/tinyglobby@0.2.17/dist";
        let mut a = app();
        a.transcript.push(Entry::Notice("hello".into()));
        a.roots = vec![jod_core::roots::Root {
            id: 1,
            conversation_id: "c".into(),
            path: std::path::PathBuf::from("/home/reljod/tetris"),
            writable: false,
            position: 0,
            origin: jod_core::roots::Origin::Human,
            added_at_ms: 0,
        }];
        a.candidates = vec![std::sync::Arc::new(vec![
            format!("{stem}/index.js"),
            format!("{stem}/index.d.ts"),
        ])];
        a.input = "@index".into();
        a.cursor = a.input.len();
        a.open_mention(0);
        a.sync_mention();
        let frame = rendered(&a, 120, 30);

        // Both filenames are on screen, which is the whole of the fix: the
        // shared head is what gets dropped, not the part that distinguishes.
        assert!(frame.contains("index.d.ts"), "{frame}");
        assert!(frame.contains("index.js"), "{frame}");
        // And the two rows are not the same line of text.
        let rows: Vec<&str> = frame.lines().filter(|l| l.contains("tinyglobby")).collect();
        assert_eq!(rows.len(), 2, "expected both rows on screen: {frame}");
        assert_ne!(rows[0], rows[1], "two files, one rendering:\n{frame}");
    }

    /// The elision has to move the highlight with it, or a row says it matched
    /// characters it no longer shows.
    #[test]
    fn a_clipped_row_still_bolds_the_characters_it_matched() {
        let row = crate::tui::mention::Row {
            label: None,
            // `rank` at bytes 43..47 of a path far too long for the column.
            path: "a/very/long/prefix/nobody/needs/to/read/at/rank.rs".into(),
            positions: vec![43, 44, 45, 46],
        };
        let line = mention_line(&row, true, 24);
        let lit: String = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .filter(|s| s.content.trim() != "▸")
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(lit, "rank", "{line:?}");
        let whole: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(whole.ends_with("rank.rs"), "{whole}");
    }
}
