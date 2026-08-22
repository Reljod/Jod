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

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use jod_core::team::MemberStatus;
use jod_core::store::RoleField;
use jod_core::PermissionPolicy;

use super::app::{
    absolute, plural, short_duration, since, until, AgentLine, App, Dictation, Entry, JobState,
    Layer, Overlay, Step,
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
use super::roles;
use super::secret;
use super::text;
use super::workspace::Workspace;
use jod_core::cards::{Card, CardKind, Delivery, Importance, Sort, Status};
use jod_core::projects::How;

const USER: Color = Color::Cyan;
const AGENT: Color = Color::Reset;
const MUTED: Color = Color::DarkGray;
const BAD: Color = Color::Red;
const GOOD: Color = Color::Green;
const WARN: Color = Color::Yellow;
/// A path, an identifier, a command — the spans of an answer that name
/// something you could go and look at. It shares cyan with the user's own
/// colour, which the two never collide over: one is a prompt at the left edge
/// and the other is a word inside a sentence.
const CODE: Color = Color::Cyan;

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

/// What one frame put where, for the events that arrive as screen coordinates
/// rather than as keys.
///
/// A mouse click carries a column and a row and nothing else, so something has
/// to say which card was under it. That something is the frame that drew the
/// card, and not a second copy of the layout arithmetic in the event loop — the
/// rail moves between a left-hand column and a bottom panel depending on the
/// terminal's width, and two places computing where it went is how they come to
/// disagree.
#[derive(Debug, Clone, Default)]
pub struct Painted {
    /// How many transcript lines one page holds.
    pub viewport: usize,
    /// How many lines the transcript came to, drawn. Reported for the reason
    /// [`Preview::lines`] is: scrolling is clamped to what is on screen, and
    /// only the frame knows how tall an entry drew.
    ///
    /// The key handler used to clamp to the *entry* count instead, which is a
    /// different number and is smaller whenever an entry wraps — a chat holding
    /// one seven-thousand-character summary and one notice stopped dead at
    /// `scrolled up 2` with two hundred lines still above it.
    pub lines: usize,
    pub rail: RailHits,
    pub panel: PanelHits,
    /// The preview pane of the frame just drawn.
    pub preview: Preview,
}

impl Painted {
    /// The furthest the transcript can be scrolled before its first line is at
    /// the top of the pane.
    pub fn max_scroll(&self) -> usize {
        self.lines.saturating_sub(self.viewport)
    }
}

/// The shape of the preview pane the last frame drew.
///
/// Reported back for the same reason [`Painted::viewport`] is: scrolling has to
/// be clamped to what is on screen, and only the frame knows. Deriving it in the
/// key handler would mean a second copy of the layout arithmetic — the pane
/// disappears below 90 columns, and its content is a different length for a
/// project row, a run and the pinned chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preview {
    /// Rows of content the pane can show at once, inside its border. Zero when
    /// the terminal was too narrow to draw a preview at all, which is how `⇥`
    /// knows not to stop on it.
    pub rows: usize,
    /// Lines the content came to.
    pub lines: usize,
    /// Where the pane was drawn, border included, so the wheel can tell a
    /// pointer over it from one over the rows beside it. `None` when no pane
    /// was drawn at all.
    pub area: Option<Rect>,
}

impl Preview {
    /// The furthest the pane can be scrolled before its last line is at the top.
    pub fn max_scroll(self) -> u16 {
        self.lines.saturating_sub(self.rows).min(u16::MAX as usize) as u16
    }

    /// Whether a pointer at these coordinates is over the pane.
    ///
    /// The same test [`RailHits::holds`] and [`PanelHits::holds`] make, for the
    /// same reason: the wheel has to go to the box under the pointer, and the
    /// only thing that knows where that box is is the frame that drew it.
    pub fn holds(self, column: u16, row: u16) -> bool {
        self.area.is_some_and(|area| {
            column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
        })
    }
}

/// Where the side panel's clickable parts landed.
///
/// Only the catalog for now. The sessions box below it and the context bar below
/// that are read-outs — there is nothing a click on a percentage could mean —
/// whereas the catalog is a list of rows, and a list of rows on screen that
/// cannot be pointed at is the complaint this answers.
#[derive(Debug, Clone, Default)]
pub struct PanelHits {
    /// The catalog box, so a click can tell it from whatever it is drawn over.
    /// `None` when the catalog is not on screen at all — the panel shut, the
    /// catalog collapsed, or no room for a third box.
    pub catalog: Option<Rect>,
    /// One entry per project row the catalog drew, in drawn order.
    pub projects: Vec<ProjectHit>,
}

/// A catalog row, and the line it was printed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHit {
    pub id: String,
    pub row: u16,
}

impl PanelHits {
    /// Whether a pointer at these coordinates is over the catalog.
    pub fn holds(&self, column: u16, row: u16) -> bool {
        self.catalog.is_some_and(|area| {
            column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
        })
    }

    /// The project under the pointer.
    pub fn project_at(&self, column: u16, row: u16) -> Option<&str> {
        if !self.holds(column, row) {
            return None;
        }
        self.projects
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| hit.id.as_str())
    }
}

/// Where the rail's clickable parts landed.
#[derive(Debug, Clone, Default)]
pub struct RailHits {
    /// The whole rail, so a wheel event can tell it from the chat beside it.
    /// `None` when the rail is not on screen at all.
    pub area: Option<Rect>,
    /// One entry per collapsed card the stack drew.
    pub cards: Vec<CardHit>,
    /// The card shown in full, when one is.
    pub expanded: Option<i64>,
    /// The row carrying the expanded card's title, which is the way back to the
    /// stack for somebody with no keyboard.
    pub back: Option<u16>,
    /// One entry per numbered option of the expanded card.
    pub options: Vec<OptionHit>,
    /// How many lines of the expanded card fall past the bottom of its pane.
    pub past: u16,
}

/// A collapsed card and the rows it occupies, top inclusive, bottom exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardHit {
    pub id: i64,
    pub top: u16,
    pub bottom: u16,
}

/// One numbered option of the expanded card, and the row it was printed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionHit {
    pub card: i64,
    /// The index into `Card::options` — the digit on screen, less one.
    pub at: usize,
    pub row: u16,
}

impl RailHits {
    /// Whether a pointer at these coordinates is over the rail.
    pub fn holds(&self, column: u16, row: u16) -> bool {
        self.area.is_some_and(|area| {
            column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
        })
    }

    /// The collapsed card under the pointer.
    pub fn card_at(&self, column: u16, row: u16) -> Option<i64> {
        if !self.holds(column, row) {
            return None;
        }
        self.cards
            .iter()
            .find(|hit| row >= hit.top && row < hit.bottom)
            .map(|hit| hit.id)
    }

    /// The option under the pointer, as the card it belongs to and its index.
    pub fn option_at(&self, column: u16, row: u16) -> Option<(i64, usize)> {
        if !self.holds(column, row) {
            return None;
        }
        self.options
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| (hit.card, hit.at))
    }

    /// Whether the pointer is on the expanded card's way back to the stack.
    pub fn on_back(&self, column: u16, row: u16) -> bool {
        self.holds(column, row) && self.back == Some(row)
    }
}

pub fn draw(f: &mut Frame, app: &App) -> Painted {
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
    let mut preview = Preview::default();
    // How tall the transcript came to. Only the chat screen has one; a workspace
    // list pages by its own height and clamps its own cursor, so zero there
    // leaves the chat's scroll at the bottom, which is where it belongs.
    let mut lines = 0usize;
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
            let (height, drawn) = draw_transcript(f, app, parts[0]);
            lines = drawn;
            draw_input(f, app, input);
            height
        }
    } else {
        preview = draw_workspace(f, app, body);
        // A workspace list pages by its own height, not the transcript's.
        body.height.saturating_sub(4).max(1) as usize
    };

    let hits = match rail {
        Some(rail) => draw_rail(f, app, rail),
        None => RailHits::default(),
    };
    let mut panel_hits = PanelHits::default();
    if let Some(side) = side {
        panel_hits = draw_panel(f, app, side);
    }
    draw_keybar(f, app, rows[1]);
    draw_status(f, app, rows[2]);

    // Last, so they float over everything.
    if app.panel && side.is_none() {
        panel_hits = draw_floating_panel(f, app, body);
    }
    // `body` rather than the frame: the popup may use the whole chat column,
    // which is what is left once anything beside it has been taken out.
    draw_completions(f, app, input, body);
    draw_mention(f, app, input);
    draw_overlay(f, app);
    // After the overlay, and that ordering is load-bearing. An overlay can raise
    // a notice without closing itself, so a flash drawn underneath would be a
    // refusal nobody can see, which is the fault this whole thing exists to fix,
    // one layer further in.
    //
    // Only off the chat screen. There the same words went into the transcript,
    // which is on screen, and saying them twice is worse than saying them once.
    if app.workspace != Workspace::Chat {
        draw_flash(f, app, body);
    }
    Painted {
        viewport: height,
        lines,
        rail: hits,
        panel: panel_hits,
        preview,
    }
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

/// How the composer breaks what was typed into rows, and where the caret lands
/// once it has.
struct Wrapped {
    /// The character index each row begins at. Never empty: there is always a
    /// first row, even when nothing has been typed into it.
    starts: Vec<usize>,
    /// The row the caret sits on, and how many columns into that row it is.
    caret: (usize, usize),
}

impl Wrapped {
    /// The character range row `row` covers, empty when there is no such row.
    fn row(&self, row: usize, len: usize) -> std::ops::Range<usize> {
        let from = self.starts.get(row).copied().unwrap_or(len);
        let to = self.starts.get(row + 1).copied().unwrap_or(len);
        from..to.max(from)
    }
}

/// Breaks `typed` into rows that fit a field `field` columns wide, with the
/// caret sitting before character `cursor`.
///
/// Rows are filled by column, not by character. A Japanese ideograph or an
/// emoji paints two columns, so a row of thirty-two characters can paint
/// thirty-six columns; the box is only as wide as it is, and the paragraph
/// clips whatever runs past its border. That clipping is silent, which is how
/// a line ending in `FFFF` came back reading `F` at forty columns. Counting
/// the columns each character costs puts the break where the terminal is
/// going to put it anyway.
///
/// A character whose second cell would land past the edge starts the next row
/// rather than being half drawn. A combining accent costs no column of its
/// own, so it never starts a row and stays with the letter it belongs to.
///
/// The caret has its own term because it sits one past the last character:
/// type exactly to the end of a row and the caret belongs on the next one,
/// which has to exist before it can be drawn there.
fn wrap_composer(typed: &[char], field: usize, cursor: usize) -> Wrapped {
    let mut starts = vec![0usize];
    let mut caret = (0usize, 0usize);
    let mut used = 0usize;
    let mut one = [0u8; 4];
    for (at, c) in typed.iter().enumerate() {
        let cost = columns(c.encode_utf8(&mut one));
        // The second condition matters on a field one column wide. A wide
        // character does not fit there at all, and without it the break would
        // open an empty row for a character that the next row cannot hold
        // either, and then another, forever. It stays where it is instead.
        if used + cost > field && at > *starts.last().expect("a first row") {
            starts.push(at);
            used = 0;
        }
        if at == cursor {
            caret = (starts.len() - 1, used);
        }
        used += cost;
    }
    if cursor >= typed.len() {
        if used >= field && !typed.is_empty() {
            starts.push(typed.len());
            caret = (starts.len() - 1, 0);
        } else {
            caret = (starts.len() - 1, used);
        }
    }
    Wrapped { starts, caret }
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
    let field = composer_field(column.width);
    let typed: Vec<char> = app.input.chars().collect();
    let lines = wrap_composer(&typed, field, app.cursor_column()).starts.len();
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
/// Below it the rail stops being a column, because taking thirty columns off an
/// eighty-column terminal leaves neither a readable rail nor a readable chat.
/// It lies along the bottom instead — see [`rail_below`].
const RAIL_BESIDE: u16 = 84;

/// How many rows the rail gets when it lies along the bottom.
///
/// Twelve holds a header and two collapsed cards, or an expanded card with room
/// for its body and its options. Fewer would put the blocking card back where it
/// was: technically on screen, and cut off before it says anything.
const RAIL_BELOW: u16 = 12;

/// The shortest bottom panel worth drawing instead of the one-line summary.
///
/// Under this the panel and the chat are both too short to read, and one honest
/// sentence beats two unreadable halves.
const RAIL_BELOW_MIN: u16 = 6;

/// A collapsed card: one row.
///
/// It was four — a border, two lines, a border — and that is what made the rail
/// unreadable. Six cards cost twenty-four rows, so five were drawn and the
/// sixth read as never having been raised; the border said nothing the gap
/// between rows does not; and the second line spent a thirty-four column rail
/// on three hex ids while the title, the only thing anybody reads, truncated at
/// twenty-eight characters. The ids moved to the group heading, where they are
/// said once for the whole project instead of once a card, and the row they
/// used to occupy went back to the chat.
const CARD_HEIGHT: u16 = 1;

/// The columns a row spends before the title starts: cursor, digit, gap, glyph,
/// gap.
const ROW_CHROME: usize = 5;

/// How many rows the bottom rail gets on a terminal too narrow for a column.
///
/// A panel tall enough to read, except in the two cases where it would be a
/// worse answer than the one-line summary: nothing has been raised, so there is
/// no card to show; or the body is so short that halving it leaves neither part
/// legible.
fn rail_below(app: &App, area: Rect) -> u16 {
    if app.cards.is_empty() {
        return 1;
    }
    // Never past half the body, for the same reason the column never passes
    // half the width: a panel bigger than what it sits beside has stopped
    // being a panel.
    let room = area.height / 2;
    if room < RAIL_BELOW_MIN {
        return 1;
    }
    RAIL_BELOW.min(room)
}

/// Where the rail goes, and what is left for everything else.
///
/// Three outcomes: hidden, a column down the left, or a panel across the bottom
/// when the terminal is too narrow for a column.
///
/// The bottom panel is the mobile case, and it is a panel rather than the single
/// line it used to be because the line could not do the rail's one job. A phone
/// terminal is eighty columns at best, so it took the narrow path every time,
/// and a card that had stopped an agent was never drawn — you got the count of
/// blockers and the key that opens them, and no way to read the question without
/// a wider screen. Columns are what a narrow terminal is short of; rows it still
/// has, so the rail spends rows instead.
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
        // Below the chat rather than above it: it sits directly over the keybar
        // that names the keys for answering, and the composer stays where the
        // hands already are.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(rail_below(app, area))])
            .split(area);
        return (Some(rows[1]), rows[0]);
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

fn draw_rail(f: &mut Frame, app: &App, area: Rect) -> RailHits {
    if area.height <= 1 {
        draw_rail_summary(f, app, area);
        // Nothing clickable: one line saying a run is blocked is not a card,
        // and pretending it can be answered would answer the wrong one.
        return RailHits::default();
    }
    let mut hits = RailHits {
        area: Some(area),
        ..RailHits::default()
    };
    match app.selected_card().filter(|_| app.rail.expanded) {
        Some(card) => draw_card(f, app, card, area, &mut hits),
        None => draw_rail_stack(f, app, area, &mut hits),
    }
    hits
}

/// The rail with a single row to say it in.
///
/// What is left when even the bottom panel will not fit — an empty rail, or a
/// body too short to divide. It owes the reader exactly two things — that
/// something is blocked, and the key that opens the rail — and it must not
/// pretend to be the rail. Hence a bar glyph and a sentence rather than a
/// squeezed card.
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

/// One drawable line of the stack.
///
/// The stack is built whole — every group heading and every card row — and then
/// windowed, rather than windowed and then built. Headings make the two
/// different: how many cards fit depends on how many headings fall between
/// them, so counting cards first would either overshoot the pane or leave a row
/// of it blank.
enum Row {
    /// The gap above a heading. Never the first line, and dropped when it would
    /// be — a stack that opens on an empty row looks like a rendering fault.
    Gap,
    /// A project's heading, as an index into the groups.
    Head(usize),
    /// A card, as an index into `app.cards`, carrying the quick-answer digit it
    /// was given — or `None` past the ninth, which has no key to print.
    Card(usize, Option<usize>),
}

/// The stack: a header, then each project's cards under a heading of their own.
fn draw_rail_stack(f: &mut Frame, app: &App, area: Rect, hits: &mut RailHits) {
    let ids = app.card_ids();
    let selected = app.rail.index(&ids);
    let groups = app.rail_groups();

    // The sort and the two filters are printed whenever they are in force, and
    // whenever the rail has the keyboard. A stack silently showing a subset is
    // a stack whose missing card reads as a card that was never raised.
    let filter = rail_filter_line(app);
    let settled = rail_settings(app).is_some();
    let armed = rail_armed(app);
    let plain = 1 + settled as u16 + filter.iter().count() as u16 + armed.iter().count() as u16;

    // Every line the stack would draw if it had unlimited room.
    let mut rows: Vec<Row> = Vec::new();
    let mut digit = 0usize;
    for (at_group, group) in groups.iter().enumerate() {
        if !rows.is_empty() {
            rows.push(Row::Gap);
        }
        rows.push(Row::Head(at_group));
        for card in &group.cards {
            digit += 1;
            rows.push(Row::Card(
                *card,
                (digit <= rail::QUICK).then_some(digit),
            ));
        }
    }

    // Where the cursor's own row landed, so the window can be built around it
    // rather than around a card count that does not know about the headings.
    let here = ids.get(selected).copied();
    let at_cursor = rows
        .iter()
        .position(|row| match row {
            Row::Card(at, _) => app.cards.get(*at).map(|c| c.id) == here,
            _ => false,
        })
        .unwrap_or(0);

    // Measured twice, because the line saying which cards are on screen is only
    // drawn when some of them are not — and drawing it takes a row off the
    // stack, which is what decides whether some of them are not. The second
    // pass settles it: a stack that overflows the shorter list overflows the
    // taller one too, so this cannot oscillate.
    let room = |head: u16| area.height.saturating_sub(head.min(area.height)) as usize;
    let fits = |head: u16| {
        let first = window_start(at_cursor, room(head).max(1), rows.len());
        shown_cards(&rows, first, room(head))
    };
    let crowded = app.cards.len() > fits(plain + !settled as u16);
    let used = (plain + (crowded && !settled) as u16).min(area.height);

    let rest = Rect {
        y: area.y + used,
        height: area.height.saturating_sub(used),
        ..area
    };
    let height = room(used);
    let mut first = window_start(at_cursor, height.max(1), rows.len());
    // Never open the window on the gap above a heading: it reads as the rail
    // having lost its first row.
    if matches!(rows.get(first), Some(Row::Gap)) {
        first += 1;
    }
    // The window is sized in rows and capped in cards, so on a tall terminal
    // holding more than nine cards it can start at the top and stop before the
    // cursor — which is how a stack ends up scrolling nowhere. Push it down
    // until the cursor is inside it. Bounded by construction: every step moves
    // `first` one row closer to `at_cursor`.
    while first < at_cursor && at_cursor >= window_end(&rows, first, height) {
        first += 1;
    }
    let last = window_end(&rows, first, height);
    let shown = shown_cards(&rows, first, height);

    let mut head = vec![rail_header(app)];
    // Below the header rather than in it. The header already carries the count,
    // the scope and the blocker tally, and thirty-four columns truncates from
    // the right — a window note appended there pushes `2 blocked` off the end,
    // which trades one honest line for a less honest one.
    if let Some(line) = rail_window(app, shown) {
        head.push(line);
    }
    head.extend(armed);
    head.extend(filter);
    f.render_widget(
        Paragraph::new(head),
        Rect {
            height: used,
            ..area
        },
    );

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

    let width = rest.width as usize;
    // Measured over the rows actually on screen, not over every card the query
    // returned: an answer nobody can see must not take columns from the titles
    // of the ones they can.
    let on_screen: Vec<&Card> = rows[first..last]
        .iter()
        .filter_map(|row| match row {
            Row::Card(at, _) => app.cards.get(*at),
            _ => None,
        })
        .collect();
    let answer_col = answer_width(&on_screen, width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    // A window that opens partway down a project must still say which project.
    // Scrolling into a list of bare commands with no heading over them is the
    // failure grouping was added to fix, arriving one keypress later.
    if let Some(Row::Card(at, _)) = rows.get(first) {
        if let Some(group) = groups.iter().find(|g| g.cards.contains(at)) {
            if !matches!(rows.get(first.saturating_sub(1)), Some(Row::Head(_))) {
                lines.push(group_heading(group, width, true));
            }
        }
    }
    for row in &rows[first..last] {
        if lines.len() >= rest.height as usize {
            break;
        }
        match row {
            Row::Gap => lines.push(Line::from("")),
            Row::Head(at) => match groups.get(*at) {
                Some(group) => lines.push(group_heading(group, width, false)),
                None => lines.push(Line::from("")),
            },
            Row::Card(at, digit) => {
                let Some(card) = app.cards.get(*at) else {
                    continue;
                };
                let y = rest.y + lines.len() as u16;
                hits.cards.push(CardHit {
                    id: card.id,
                    top: y,
                    bottom: y + CARD_HEIGHT,
                });
                lines.push(card_row(card, *digit, width, answer_col, *at == selected));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), rest);
}

/// Where a window of `height` rows starting at `first` has to stop.
///
/// Two limits, and either can bite first: the rows the pane actually has, and
/// [`rail::VISIBLE`] cards. Walked rather than calculated, because headings and
/// gaps take rows the cards were going to have and how many depends on where
/// the group boundaries fell.
fn window_end(rows: &[Row], first: usize, height: usize) -> usize {
    let mut cards = 0;
    let mut at = first;
    while at < rows.len() && at - first < height {
        if matches!(rows[at], Row::Card(_, _)) {
            if cards == rail::VISIBLE {
                break;
            }
            cards += 1;
        }
        at += 1;
    }
    at
}

/// How many cards a window of `height` rows starting at `first` actually shows.
fn shown_cards(rows: &[Row], first: usize, height: usize) -> usize {
    rows[first..window_end(rows, first, height)]
        .iter()
        .filter(|row| matches!(row, Row::Card(_, _)))
        .count()
}

/// What a row's right-hand column says.
///
/// One question, and which question depends on whether the card is still
/// waiting. An open card's is *what would the digit do*; an answered one's is
/// *has the agent heard yet*, which is the fact decision D2 exists to keep on
/// screen — a card that reads as done while its answer is still queued is a lie
/// the reader acts on.
fn answer_text(card: &Card, cap: usize) -> String {
    if let Some(rec) = rail::recommended(card) {
        return rec.label;
    }
    // The whole sentence when the rail is wide enough to hold it beside a
    // readable title, and the one-word form when it is not. The bottom panel a
    // phone gets is seventy-eight columns and takes the sentence; the column
    // beside a chat is thirty-four and takes the word.
    if let Some(note) = rail::delivery_note(card) {
        if note.chars().count() <= cap {
            return note.to_string();
        }
    }
    match rail::delivery_short(card) {
        Some(note) => note.to_string(),
        // An em dash rather than a blank: the column is a promise about what the
        // digit does, and an empty cell reads as one nobody got round to filling
        // in rather than as "there is nothing to accept here".
        None => "—".to_string(),
    }
}

/// The most columns the answers may take before the titles start suffering.
///
/// Fourteen columns of title is the floor, which is roughly where a command
/// stops being recognisable. Everything to the right of that is the answers'
/// to use and no more.
fn answer_cap(width: usize) -> usize {
    width.saturating_sub(ROW_CHROME + 14).max(6)
}

/// How wide the answer column has to be for every row to fit in it.
///
/// The widest answer on screen, floored so a stack of nothing but em dashes
/// still leaves a column rather than a stripe, and capped by [`answer_cap`].
fn answer_width(cards: &[&Card], width: usize) -> usize {
    let cap = answer_cap(width);
    cards
        .iter()
        .map(|card| answer_text(card, cap).chars().count())
        .max()
        .unwrap_or(1)
        .clamp(1, cap)
}

/// A project's heading: what it is called, and how much of it is stopped.
///
/// The name comes from the work's own title, which is a paraphrase of what that
/// agent was asked to do — so the heading reads as the thing you delegated
/// rather than as the identifier it was filed under. `carried` marks a heading
/// re-drawn at the top of a scrolled window, which is a repeat rather than the
/// start of a group and is dimmed to say so.
fn group_heading(group: &rail::Group, width: usize, carried: bool) -> Line<'static> {
    let blocked = if group.blocked > 0 {
        format!("{} {}", group.blocked, rail::BLOCKED)
    } else {
        String::new()
    };
    let room = width.saturating_sub(blocked.chars().count() + 2);
    let label = cut(&group.label, room);
    let pad = width
        .saturating_sub(label.chars().count() + blocked.chars().count() + 2)
        .max(1);
    Line::from(vec![
        Span::styled(
            format!(" {label}"),
            if carried { fg(MUTED) } else { bold(AGENT) },
        ),
        Span::styled(" ".repeat(pad), fg(MUTED)),
        Span::styled(blocked, bold(BAD)),
    ])
}

/// One card, in one row: what it is, what it says, and what the digit would do.
///
/// The row is a sentence read left to right — *this one, number three, blocked,
/// `rm -rf node_modules`, allow* — and everything that is not one of those five
/// things has been taken off it. What went, and where it went instead:
///
/// - **The border.** It said nothing; the gap between rows says the same.
/// - **The session and run ids.** Now on the group heading, once for the
///   project rather than once a card, which is the whole point of grouping. The
///   expanded card still carries all three in full.
/// - **The words `question`, `high` and `blocked`.** The first two are the
///   glyph and the colour restating themselves. The third is the one that
///   mattered, and it is still said — on the heading, on the rail's own header,
///   and by [`rail::row_glyph`] putting `!` in the glyph column, which is the
///   non-colour channel that rule actually asks for.
///
/// What arrived is the right-hand column: the answer `Ctrl-R` and this row's
/// digit would send. It is what makes the rail answerable without opening
/// anything — a column of `allow` beside a column of commands is a queue you
/// can clear, and a `—` says plainly that this one needs reading.
fn card_row(
    card: &Card,
    digit: Option<usize>,
    width: usize,
    answer_col: usize,
    here: bool,
) -> Line<'static> {
    let recommended = rail::recommended(card);
    let answer = cut(&answer_text(card, answer_cap(width)), answer_col);
    // Every row gives the answer column the same width, so the titles start and
    // stop in the same place down the whole stack. Sizing each row to its own
    // answer was a column of ragged left edges, and the point of the redesign is
    // that the rail can be read in one downward glance.
    let room = width
        .saturating_sub(ROW_CHROME + answer_col + 2)
        .max(4);
    let title = cut(&card.title, room);
    let pad = width
        .saturating_sub(ROW_CHROME + title.chars().count() + answer.chars().count() + 1)
        .max(1);
    Line::from(vec![
        Span::styled(
            if here { "▸" } else { " " }.to_string(),
            bold(USER),
        ),
        Span::styled(
            digit.map(|d| d.to_string()).unwrap_or_else(|| " ".into()),
            if recommended.is_some() {
                bold(USER)
            } else {
                fg(MUTED)
            },
        ),
        Span::styled(
            format!(" {} ", rail::row_glyph(card)),
            if card.blocking {
                bold(BAD)
            } else {
                fg(card_colour(card))
            },
        ),
        Span::styled(title, if here { bold(AGENT) } else { fg(AGENT) }),
        Span::styled(" ".repeat(pad), fg(MUTED)),
        Span::styled(
            answer,
            if recommended.is_some() {
                fg(GOOD)
            } else {
                fg(MUTED)
            },
        ),
    ])
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
    // The count always; the stack's name only when it is not the resting one.
    // ` rail · 6 open · subtree · 4 blocked` is thirty-five columns in a
    // thirty-four column rail, and what fell off the end was `blocked` — the one
    // word on the line that had to survive. `open` is what the rail shows unless
    // somebody pressed `t`, so it is the cheapest of the four to drop.
    let stack = app.rail.stack_now();
    spans.push(Span::styled(
        if stack == Status::Open {
            format!(" · {}", app.cards.len())
        } else {
            format!(" · {} {}", app.cards.len(), stack.as_str())
        },
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

/// The line that says the digits are live, while they are.
///
/// A line of its own rather than a clause on the header, which has no columns
/// to spare and drops what it cannot fit off the right-hand end. An armed prefix
/// is a state the keyboard is in and nothing else on screen would show it —
/// without this, digits that answer cards look exactly like digits that do not.
fn rail_armed(app: &App) -> Option<Line<'static>> {
    if !app.rail.quick {
        return None;
    }
    let numbered = app.cards.len().min(rail::QUICK);
    Some(Line::from(vec![
        Span::styled(format!(" 1–{numbered}"), bold(USER)),
        Span::styled(" accepts · Esc cancels".to_string(), fg(MUTED)),
    ]))
}

/// How much of the stack is on screen, when it is not all of it.
///
/// Not decoration: five cards drawn out of twelve, with nothing saying so, is
/// seven cards that read as never having been raised — the same failure the
/// settings line exists to prevent for a filter. It shares that line when the
/// settings are already being printed, and takes one of its own when they are
/// not, because it is drawn from a cap the reader never chose and so cannot be
/// expected to remember.
///
/// A count rather than the `1–5 of 12` range it used to print. Grouping moved
/// cards past each other, so a card's position in the drawn list is no longer
/// its position in the query — and a range over one, read as a range over the
/// other, is a more confident wrong answer than no range at all.
fn rail_window(app: &App, shown: usize) -> Option<Line<'static>> {
    let settings = rail_settings(app);
    if shown == 0 || shown >= app.cards.len() {
        return settings;
    }
    let window = format!("{shown} of {} shown", app.cards.len());
    Some(match settings {
        Some(line) => {
            let mut spans = line.spans;
            spans.push(Span::styled(format!(" · {window}"), fg(MUTED)));
            Line::from(spans)
        }
        None => Line::from(Span::styled(format!(" {window}"), fg(MUTED))),
    })
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
fn draw_card(f: &mut Frame, app: &App, card: &Card, area: Rect, hits: &mut RailHits) {
    let width = area.width.saturating_sub(4) as usize;
    // Line indices, paired with the option each one is for. See the loop below.
    let mut chosen_rows: Vec<(usize, usize)> = Vec::new();
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
            // Which line of the card this option went on. Turned into a screen
            // row further down, once every line's wrapped height is known — a
            // long body above pushes the options down by more rows than it has
            // lines, and a pointer that trusted the line number would answer
            // whichever option happened to sit at that index.
            chosen_rows.push((lines.len(), at));
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

    // Where each line lands once the pane has wrapped everything above it.
    let inner = area.width.saturating_sub(2) as usize;
    let mut tops: Vec<usize> = Vec::with_capacity(lines.len());
    let mut deep = 0usize;
    for line in &lines {
        tops.push(deep);
        deep += rows_for(line, inner);
    }
    let pane = area.height.saturating_sub(2) as usize;
    // How much of the card is below the pane, which is what stops the wheel at
    // the last line rather than at a screenful of nothing.
    let past = deep.saturating_sub(pane);
    let scroll = (app.rail.scroll as usize).min(past);

    hits.expanded = Some(card.id);
    // The title row, which is the top border. Clicking it puts the card back in
    // the stack — the only way out of an expanded card that does not need a
    // keyboard, which is the whole point on a phone.
    hits.back = Some(area.y);
    hits.past = past as u16;
    for (line, at) in chosen_rows {
        let Some(top) = tops.get(line) else { continue };
        // Off the top or off the bottom because of the scroll: not on screen,
        // so not clickable. A hit recorded for a row the reader cannot see
        // would answer the card from a click meant for whatever is there now.
        let Some(offset) = top.checked_sub(scroll) else {
            continue;
        };
        if offset >= pane {
            continue;
        }
        hits.options.push(OptionHit {
            card: card.id,
            at,
            row: area.y + 1 + offset as u16,
        });
    }

    // The title carries the way back, because a gesture nobody can see is a
    // gesture nobody uses.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(card_border(card))
        .title(format!(" ◂ card #{} ", card.id))
        .title_bottom(fit_verbs(&keys::rail_footer(), area.width));
    // Wrapped, because the state sentence is the longest line here and it is
    // the one that must not be clipped: "answered, queued — the agent is told
    // at the end of the turn in flight" cut at fifty columns says "answered,
    // queued — the agent is told at the", which reads as a promise about now.
    // `trim: false` keeps the indentation the lines above were built with.
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        area,
    );
}

/// How many rows one built line takes once the pane wraps it to `width`.
///
/// The same greedy break the pane itself does, so the answer is the row the
/// reader's pointer is actually over. An empty line still costs a row.
fn rows_for(line: &Line, width: usize) -> usize {
    let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
    if text.trim().is_empty() {
        return 1;
    }
    wrap(&text, width, 0).len().max(1)
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

    // Two rows of headroom, not one. A popup ending at `input.y - 1` puts its
    // bottom border exactly on the transcript's, and `Clear` only covers the
    // popup's own width — so the two borders join into one line of doubled
    // corners, `└──└──popup──┘──┘`, which reads as a half-drawn box rather
    // than as a panel floating over the transcript.
    let h = ((rows.len() + 2) as u16)
        .min(input.y.saturating_sub(2))
        .max(3);
    // Anchored on the `@` itself, then pulled back inside the box: a popup that
    // hangs off the right edge of the terminal is drawn over nothing. The
    // column is the one the `@` is drawn in, not how far into the prompt it is
    // — those part company as soon as the line wraps onto a second row. It
    // comes off the same wrapper the box is drawn with, so the popup and the
    // `@` cannot disagree about which column that is.
    let typed: Vec<char> = app.input.chars().collect();
    let at = app.input[..popup.at.min(app.input.len())].chars().count();
    let col = wrap_composer(&typed, composer_field(input.width), at).caret.1 as u16;
    let x = (input.x + 1 + CARET.chars().count() as u16 + col).min(
        input
            .x
            .saturating_add(input.width)
            .saturating_sub(w)
            .max(input.x),
    );
    let panel = Rect {
        x,
        y: input.y.saturating_sub(h + 1),
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
/// command whose whole answer is notices (`/root`, `/config`, the delegation
/// confirmation, most errors): the splash kept the column and
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
    let anchored = !app.completions_dismissed
        && !crate::tui::command::completions(&app.input, app).is_empty();

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
/// the same fact — amber while a turn is running, quiet once it is not — then
/// in red what has stopped and is waiting on him, and then in cyan what has
/// been asked and not yet read.
///
/// The two marks are spans of their own rather than fragments of
/// [`App::activity`], for the two reasons that fragment never worked. They get
/// their own colours, so they stop reading like `ready`; and they are reserved
/// before the activity is `cut`, so a narrow band drops what he is *doing*
/// rather than what is waiting. Those are the right things to lose in that
/// order: a run in flight finishes on its own and a question does not.
///
/// The cards mark is the one this band was missing. A blocking card had three
/// ways to reach a reader who was not looking at the rail — this line, the
/// status row, and the rail opening itself — and an ordinary question had
/// none, so the only way to find one was to press `Ctrl-N` on the chance that
/// something was there.
fn header_doing(app: &App, room: usize) -> Vec<Span<'static>> {
    let colour = if app.busy { WARN } else { MUTED };
    // Beside the activity rather than out at the right margin: the lines of
    // this band are a left-aligned block, and a mark floating at the far edge
    // of line three reads as belonging to something else.
    //
    // Most important first, which is also the order they are dropped in from
    // the other end — the same policy the status row's badges follow.
    let mut marks: Vec<(String, Style)> = Vec::new();
    if let Some(waiting) = app.waiting_on_you() {
        marks.push((format!("  ▌ {waiting}"), bold(BAD)));
    }
    if let Some(cards) = app.cards_to_read() {
        marks.push((format!("  ◆ {cards}"), fg(USER)));
    }
    let width = |marks: &[(String, Style)]| -> usize {
        marks.iter().map(|(text, _)| columns(text)).sum()
    };
    if marks.is_empty() {
        return vec![Span::styled(cut(&app.activity(), room), fg(colour))];
    }
    while width(&marks) > room && marks.len() > 1 {
        marks.pop();
    }
    let Some(left) = room.checked_sub(width(&marks)) else {
        // Not even one mark fits. It is what stays — the status row below
        // carries the same facts, but this is the line that is beside the lion
        // on every screen.
        let (text, style) = marks.remove(0);
        return vec![Span::styled(cut(text.trim_start(), room), style)];
    };
    let mut spans = vec![Span::styled(cut(&app.activity(), left), fg(colour))];
    spans.extend(
        marks
            .into_iter()
            .map(|(text, style)| Span::styled(text, style)),
    );
    spans
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

/// How tall the settings box is: two borders, and one row each for the mode,
/// the harness and the spend.
const SESSION_HEIGHT: u16 = 5;

/// The projects at the top, this conversation's settings under them, and how
/// much of the window it has eaten at the bottom.
///
/// The list of runs used to sit in the middle of this and it is gone. Thirty
/// columns could hold an id, an age and a truncated name — three facts about a
/// run, none of them the ones anybody acts on — while the fleet shows the same
/// runs with the room to tell two of them apart. A panel that costs a third of
/// the screen has to answer questions nothing else answers, and *what else is
/// running* was not one of them.
fn draw_panel(f: &mut Frame, app: &App, area: Rect) -> PanelHits {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(projects_height(app, area.height)),
            // The slack, so a short catalog is drawn at the size of what is in
            // it rather than stretched down the panel with blank rows under the
            // last project.
            Constraint::Min(0),
            Constraint::Length(SESSION_HEIGHT),
            Constraint::Length(CONTEXT_HEIGHT),
        ])
        .split(area);
    let hits = draw_projects(f, app, parts[0]);
    draw_session(f, app, parts[2]);
    draw_context(f, app, parts[3]);
    hits
}

/// How many rows the catalog gets.
///
/// Collapsed it is one line plus its border, which still answers the question
/// the panel is there for — *which project am I in*. Expanded it grows with the
/// catalog and stops where the two fixed boxes under it begin, so a long
/// catalog is cut rather than pushing the settings and the context off the
/// bottom. It used to stop at a third of the panel instead, to leave room for
/// the runs list that sat between them; with that list gone the room is the
/// catalog's.
///
/// **An opened catalog is never nothing.** Below twelve rows of panel this used
/// to return zero whatever the state was, so on a short terminal `Ctrl-P`
/// changed a flag nothing rendered: the key did nothing, said nothing, and gave
/// no reason — which is the exact failure the old projects key had already been
/// fixed once for, one state further out. Collapsed it still yields the whole
/// box, because a collapsed catalog is worth three rows only while there is
/// something left underneath it to be worth them against.
fn projects_height(app: &App, available: u16) -> u16 {
    // Two borders and a line. Under that there is no box to draw and no
    // arithmetic that produces one.
    if available < 3 {
        return 0;
    }
    if !app.projects_open {
        // No honest room for a third box. The current project still reaches the
        // status bar, which is where it matters most.
        return if available < 12 { 0 } else { 3 };
    }
    let ceiling = available
        .saturating_sub(SESSION_HEIGHT + CONTEXT_HEIGHT)
        .max(4)
        .min(available);
    let wanted = app.projects.len().clamp(1, 32) as u16 + 2;
    wanted.clamp(3.min(ceiling), ceiling)
}

/// The way out of an empty catalog, in the thirty-odd columns the panel has.
///
/// It used to say “ask Jod to add one”, which is not a remedy: it names no
/// command at all. The catalog is filled from a shell, so the empty state
/// names that command — an empty box is exactly when you are looking for the
/// way to fill it, and “ask somebody” is not it.
pub(super) const CATALOG_REMEDY: &str = "jod project add";

/// The catalog, with the project this conversation is about marked.
///
/// The mark is the point of the box. Everything else here is a list of
/// directories, which nobody needs on screen; *which one a dictated sentence
/// will land in* is a fact worth a permanent corner of the panel, because the
/// alternative is finding out when an agent starts editing the wrong
/// repository.
fn draw_projects(f: &mut Frame, app: &App, area: Rect) -> PanelHits {
    if area.height == 0 {
        return PanelHits::default();
    }
    let inner = area.width.saturating_sub(2) as usize;
    let current = app.current_project.as_ref();
    let focused = app.panel_focused && app.projects_open;

    // Named, carried, or nothing — three states with three different claims on
    // his attention, so they do not share a colour.
    let (title, border) = match current.map(|c| c.how) {
        Some(How::Sticky) => (" projects · carried ", WARN),
        Some(_) => (" projects ", MUTED),
        None => (" projects · none set ", MUTED),
    };

    // The border says who has the keyboard, the way the rail's does. A box with
    // a highlighted row in it and no other change would look like a box that had
    // highlighted a row on its own.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(if focused { USER } else { border }))
        .title(title);

    if !app.projects_open {
        let line = match current {
            Some(c) => Line::from(vec![
                Span::styled(" ▸ ", fg(MUTED)),
                Span::styled(cut(&c.name, inner.saturating_sub(3)), bold(GOOD)),
            ]),
            // Two different emptinesses, and the box used to say the same
            // sentence for both. Collapsed, this line is about the *current*
            // project — but "nothing set — /project add" beside a fleet drawing
            // four repositories reads as "you have no repositories", and its
            // remedy tells you to add one you already have. Pressing Ctrl-P
            // twice was enough to produce it.
            // Both fitted, like the row above them. The count line was added
            // without a `cut` and lost its last three characters at *every*
            // width — `… catalogued — Ctr` — which dropped the keystroke the
            // sentence exists to name. The same pass added "ellipsise rather
            // than clip" for the empty states two panes over, and this was the
            // one line that missed its own rule.
            None if app.projects.is_empty() => Line::from(Span::styled(
                cut(&format!(" ▸ nothing set — {CATALOG_REMEDY}"), inner),
                fg(MUTED),
            )),
            // Short enough to survive a thirty-two column panel, which is what
            // this box is at every terminal width. "None set" is not repeated
            // here because the box's own title already says it — what the line
            // is for is the two facts the title cannot carry: that there *are*
            // projects, and the key that shows them.
            None => Line::from(Span::styled(
                cut(
                    &format!(" ▸ {} catalogued · Ctrl-P", app.projects.len()),
                    inner,
                ),
                fg(MUTED),
            )),
        };
        f.render_widget(Paragraph::new(line).block(block), area);
        return PanelHits::default();
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
        // Clickable even while empty: a tap on the box still hands it the
        // keyboard, and the remedy it prints is the next thing to read.
        return PanelHits {
            catalog: Some(area),
            projects: Vec::new(),
        };
    }

    // The current project first regardless of recency, and the same order the
    // cursor steps through and a click resolves against — see `App::catalog`.
    let rows = app.catalog();

    // The box shows what fits, and the cursor decides which end of the catalog
    // that is. It used to render every row into a paragraph the box then clipped
    // silently: a twelve-project catalog in a nine-row box lost three projects,
    // with nothing on screen saying they existed and no key that could reach
    // them.
    let room = area.height.saturating_sub(2) as usize;
    let first = window_start(app.project_index(), room, rows.len());
    let visible: Vec<&jod_core::projects::Project> =
        rows.iter().skip(first).take(room).copied().collect();

    let selected = focused.then(|| app.selected_project().map(|p| p.id.clone())).flatten();
    let mut hits = PanelHits {
        catalog: Some(area),
        projects: Vec::new(),
    };

    // Names that more than one checkout answers to. Two repositories whose
    // directories are both called `web` are catalogued under one name, and the
    // panel drew them as two identical rows — so the one screen whose job is
    // "which repository does the next sentence land in" could not tell you.
    // The parent directory is what differs, and it is the shortest thing that
    // does.
    let shared: std::collections::HashSet<&str> = {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut twice: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for p in &rows {
            if !seen.insert(p.name.as_str()) {
                twice.insert(p.name.as_str());
            }
        }
        twice
    };

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(at, p)| {
            hits.projects.push(ProjectHit {
                id: p.id.clone(),
                row: area.y + 1 + at as u16,
            });
            // By id, not by name: two checkouts called `api` are two rows, and
            // marking both as current says two different repositories are the
            // one the next sentence lands in.
            let is_current = current.is_some_and(|c| p.id == c.id);
            let is_cursor = selected.as_deref() == Some(p.id.as_str());
            // The cursor and the current project are two different facts and the
            // row has to carry both: `▸` is *this is the repository your next
            // sentence lands in*, and the highlight is *this is the row a key
            // would act on*. They are usually the same row and must not be
            // assumed to be.
            let marker = match (is_cursor, is_current) {
                (true, _) => " › ",
                (false, true) => " ▸ ",
                (false, false) => "   ",
            };
            let style = match (is_cursor, is_current) {
                (true, _) => bold(USER),
                (false, true) => bold(GOOD),
                (false, false) => fg(AGENT),
            };
            // A checkout that is not there any more. Said on the row because
            // this panel is where a project is chosen, and choosing this one
            // routes an instruction into a directory that cannot be entered —
            // which surfaces as the supervisor blaming the harness binary for
            // the operating system refusing the working directory. `jod project
            // ls` prints the whole sentence; thirty columns get the word.
            let missing = app.broken_projects.contains(&p.id);
            // The qualifier is drawn muted and after the name, so a catalog
            // with no clashes in it reads exactly as it did.
            let qualifier = if missing {
                " · missing".to_string()
            } else {
                shared
                    .contains(p.name.as_str())
                    .then(|| {
                        p.path
                            .parent()
                            .and_then(|parent| parent.file_name())
                            .map(|dir| format!(" in {}", dir.to_string_lossy()))
                    })
                    .flatten()
                    .unwrap_or_default()
            };
            let room = inner.saturating_sub(3);
            let name = cut(&p.name, room.saturating_sub(qualifier.chars().count()));
            Line::from(vec![
                Span::styled(marker, fg(if is_current { GOOD } else { MUTED })),
                Span::styled(name, style),
                Span::styled(
                    cut(&qualifier, room),
                    fg(if missing { BAD } else { MUTED }),
                ),
            ])
        })
        .collect();

    // What the window is leaving out, in both directions, because a list that
    // silently ends is a list you believe you have read.
    let hidden = rows.len() - visible.len();
    let block = if hidden > 0 {
        block.title_bottom(cut(&format!(" {hidden} more · ↑↓ "), inner))
    } else if focused {
        block.title_bottom(cut(" ⏎ manager · Esc back ", inner))
    } else {
        block
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
    hits
}

/// The panel when the terminal is too narrow to put it beside anything.
///
/// Still on the right and still inside the body, rather than centred over the
/// whole screen: it is *the right-hand panel* whichever way it is drawn, and a
/// float that covered the keybar would hide the keys while you were looking for
/// them.
fn draw_floating_panel(f: &mut Frame, app: &App, body: Rect) -> PanelHits {
    let width = PANEL.min(body.width);
    let area = Rect {
        x: body.x + body.width - width,
        width,
        ..body
    };
    f.render_widget(Clear, area);
    draw_panel(f, app, area)
}

/// How many rows of a flash are drawn before the rest becomes a count.
///
/// A flash floats over the screen that raised it, so it has to leave that screen
/// usable: a fleet you cannot see is a fleet you cannot act on. Two thirds of
/// the body at the most, and never more than twelve rows.
fn flash_room(body: Rect) -> usize {
    (((body.height as usize).saturating_sub(2) * 2) / 3).clamp(1, 12)
}

/// What a keypress on this screen had to say, floating over it.
///
/// The transcript is drawn on the chat screen and nowhere else, so this is the
/// only place a notice raised on a workspace can be read at the moment it is
/// about something. Anchored to the bottom, above the keybar, because that is
/// where the eye already is after pressing a key, and because the top of every
/// workspace is its header and its first rows.
///
/// It expires on its own — see [`app::Flash`] — so there is no key to dismiss it
/// and none is advertised.
fn draw_flash(f: &mut Frame, app: &App, body: Rect) {
    let Some(flash) = &app.flash else {
        return;
    };
    // Under this there is no room for a box *and* a screen underneath it, and a
    // notice covering the whole screen is a modal with no way out.
    if body.height < 6 || body.width < 24 {
        return;
    }
    let inner = body.width.saturating_sub(4) as usize;
    let room = flash_room(body);
    // Wrapped before it is counted. Counting the notices instead would let one
    // long sentence fill the box and report nothing as dropped.
    let mut rows: Vec<String> = Vec::new();
    let mut left = 0usize;
    for (i, line) in flash.lines.iter().enumerate() {
        if rows.len() >= room {
            left = flash.lines.len() - i;
            break;
        }
        rows.extend(wrap(line, inner, 0));
    }
    rows.truncate(room);
    let height = rows.len() as u16 + 2;
    let area = Rect {
        x: body.x,
        y: body.y + body.height - height,
        width: body.width,
        height,
    };
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::from(Span::styled(format!(" {row}"), fg(WARN))))
        .collect();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(WARN))
        .title(" notice ");
    // Said rather than silently cut. A list that stops at twelve without saying
    // so reads as a list of twelve.
    if left > 0 {
        block = block.title_bottom(format!(" … and {left} more "));
    }
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What this conversation is set to and what it has cost so far.
///
/// Three facts, one row each. They were the header of the runs list until that
/// list was taken off the panel, and they are the part of it worth keeping:
/// every one of them is about the turn you are about to send, and the mode is
/// the only one of the three the status bar also carries.
fn draw_session(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(vec![
            Span::styled(" mode    ", fg(MUTED)),
            mode_span(app.mode),
            Span::styled("   Tab cycles", fg(MUTED)),
        ]),
        Line::from(vec![
            Span::styled(" harness ", fg(MUTED)),
            Span::styled(app.harness.label().to_string(), fg(AGENT)),
        ]),
        // A dash rather than `$0.0000`: four decimal places of nothing is four
        // decimal places of noise on a thirty-column panel.
        Line::from(Span::styled(
            if app.cost_usd > 0.0 {
                format!(" spend   ${:.4}", app.cost_usd)
            } else {
                " spend   —".to_string()
            },
            fg(MUTED),
        )),
    ];

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(MUTED))
                .title(" session ")
                // Both keys, because they are both true and the panel's own
                // border is where the way out of it is printed. `Esc` is the
                // one a reader tries first and the one that used to do nothing
                // here.
                .title_bottom(" Esc or Shift-Tab closes "),
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
    // What happens, not what is recommended. This line used to read `⚠ compact
    // recommended` while nothing in the console could compact anything — advice
    // pointing at a command that did not exist. Past the threshold the console
    // now does it itself at the end of the turn, so the line says so.
    //
    // `⚠` as well as the colour, so it survives NO_COLOR — it is the one line in
    // this box that reports something about to happen.
    lines.push(match (app.should_compact(), app.auto_compact) {
        (false, _) => Line::from(Span::styled(" room to keep going", fg(MUTED))),
        (true, true) => Line::from(Span::styled(" ⚠ compacting when the turn ends", bold(WARN))),
        // Automatic compaction gave up earlier in this session, so the honest
        // line is the old one: it is yours to do, and now there is a command.
        (true, false) => Line::from(Span::styled(" ⚠ type /compact to compact", bold(WARN))),
    });

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
        Overlay::None if app.rail.focused && app.rail.shown => (
            keys::rail_keybar(area.width),
            keys::exit_beneath(Layer::Rail, app.beneath_focus()),
        ),
        // The catalog, on the same terms and in the same place in the order:
        // below the rail, because that is the order the router dispatches in,
        // and a bar that named a layer the router checks second would print
        // keys that are not the ones in force.
        Overlay::None if app.panel_focused && app.panel && app.projects_open => (
            keys::catalog_keybar(area.width),
            keys::exit_beneath(Layer::Catalog, app.beneath_focus()),
        ),
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

/// How many columns a list of badges takes, separators included.
///
/// Counted here rather than by building the string and measuring it, because
/// the caller drops badges one at a time and has to re-measure after each — and
/// a measurement that disagreed with the layout by three columns would put the
/// last badge over the right edge, which is the one place a status row is never
/// allowed to go.
fn width_of(badges: &[(String, Style)]) -> usize {
    let text: usize = badges.iter().map(|(t, _)| t.chars().count()).sum();
    text + 3 * badges.len().saturating_sub(1)
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
    // The badges, in the order they matter, each with its own colour. A list
    // rather than one string because the two properties that string could not
    // have are the ones this row needs: a badge that is *louder* than the rest,
    // and a narrow terminal that drops the least important badge instead of
    // dropping all of them. Anything appended below is lower priority than
    // everything above it — that ordering is the whole policy.
    let mut badges: Vec<(String, Style)> = Vec::new();
    // First, and unconditionally. A live microphone is the one state on this
    // row where not knowing has a cost outside the program, so it goes ahead
    // of every other badge and never competes for the space.
    if let Some(said) = dictation_badge(app) {
        badges.push((said, fg(WARN)));
    }
    // Second, and in red: something has stopped and will not start again on its
    // own. It carries the key as well as the count, because a reader who has
    // just learned they are the blocker should not then have to go and find out
    // how to answer. This is the fact the whole change is about — as a grey
    // fragment inside the left-hand run-on it was there, and nobody saw it.
    let blocked = app.waiting_on_you();
    if let Some(waiting) = &blocked {
        badges.push((format!("▌ {waiting} · Ctrl-N"), bold(BAD)));
    }
    // Third: cards that have been raised and not answered, and are not stopping
    // anything. Quieter than the blocker badge because nothing is standing
    // still over them, but present, which is the change — an agent that asks a
    // question it can carry on past used to raise a card that nothing outside
    // the rail counted, so it stayed unread until somebody opened the rail for
    // an unrelated reason.
    if let Some(cards) = app.cards_to_read() {
        // The key once per row, not once per badge. Both badges are answered by
        // the same chord and they sit side by side, so printing it twice spends
        // nine of the scarcest columns on the screen repeating itself — and the
        // badges are dropped from this end, so the one that survives is always
        // the one still carrying it.
        let key = if blocked.is_some() { "" } else { " · Ctrl-N" };
        badges.push((format!("◆ {cards}{key}"), fg(USER)));
    }
    // The panel holds the context bar, but the panel is shut most of the time
    // and something about to happen unannounced is worse than advice nobody can
    // see — so it rides the one row that is always on screen.
    //
    // It still says "(estimate)", and the word earns its place more now than it
    // did as advice. `CONTEXT_WINDOW` is one assumed figure for every model, so
    // on a model with a larger window this lights up long before the
    // conversation is really full — and what follows is no longer a suggestion
    // the user can ignore but a model call the console makes on its own. The
    // hedge is what keeps that honest. What makes it an acceptable trade is that
    // compacting deletes nothing: the earlier turns stay searchable and the
    // thread behind the summary is still on the fleet.
    if app.should_compact() {
        badges.push((
            if app.auto_compact {
                "⚠ compacting (estimate)".to_string()
            } else {
                "⚠ compact (estimate)".to_string()
            },
            fg(WARN),
        ));
    }
    // Endings that arrive while you are away have to survive until you look.
    if app.unread() > 0 {
        badges.push((format!("⚑ {} unread", app.unread()), fg(WARN)));
    }
    // A shell building in the background is invisible by construction — that
    // is what backgrounding it means — so the always-on row is the only place
    // it can be seen without being looked for.
    if app.running_jobs() > 0 {
        badges.push((
            format!("⚙ {} running (Ctrl-G j)", app.running_jobs()),
            fg(WARN),
        ));
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
    // the badges have to yield rather than collide with it. Running them
    // together produced `1 queuedCtrl-X stop`, which reads as neither.
    //
    // They yield one at a time, from the least important end. Dropping the lot
    // the moment the row got tight was what made the blocker badge worth
    // nothing on the screens it was most needed on: a phone terminal is eighty
    // columns, `⚙ 2 running (Ctrl-G j)` is twenty-two of them, and the fact
    // that an agent had stopped went out with it.
    let used = mode_width + 3 + left.chars().count() + 2;
    let room = (area.width as usize).saturating_sub(used);
    while width_of(&badges) + 2 > room && !badges.is_empty() {
        badges.pop();
    }
    if !badges.is_empty() {
        spans.push(Span::raw(" ".repeat(room - width_of(&badges))));
        for (i, (text, style)) in badges.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", fg(MUTED)));
            }
            spans.push(Span::styled(text, style));
        }
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
fn draw_completions(f: &mut Frame, app: &App, input: Rect, column: Rect) {
    // Escape put it away for this line. The popup is derived from the input, so
    // there is nothing to close — this is the closing.
    if app.completions_dismissed {
        return;
    }
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
    // The room to the right of the input box, **within the chat column**.
    //
    // It used to be measured off the whole frame, which is not the same thing
    // whenever something sits beside the chat: the palette grew to ninety-two
    // columns next to a seventy-one column composer and painted straight over
    // the context rail, leaving `8s`, `200k` and orphaned corners behind it.
    //
    // Bounding it by the *composer* instead would be wrong in the other
    // direction, and there is a test that says so: the composer is capped at a
    // comfortable reading width, so on a wide terminal the palette would be
    // re-cut to it and `no argument restores` would lose its last word again —
    // the very fault the fixed 72-column cap was removed to fix. The column is
    // the honest bound: it is the whole width when nothing is beside the chat,
    // and exactly the chat's share when something is.
    let edge = column.x.saturating_add(column.width);
    let room = edge.saturating_sub(input.x).max(1);
    let w = (want as u16).min(room).max(24.min(room));
    // Whatever is left for the hint once the mark, the name column and the
    // borders have been paid for. Below that it is cut *with* a marker, so a
    // sentence that stops never reads as a sentence that ended.
    let hint_room = (w as usize).saturating_sub(widest_name + 6);
    // Only as tall as it needs to be, and never taller than the space above.
    // The headroom `draw_mention` documents, for the same reason. This popup
    // is usually as wide as the transcript, so the seam is hidden — but it is
    // the same seam, and two placements that only differ by accident drift.
    let h = ((suggestions.len() + 2) as u16)
        .min(input.y.saturating_sub(2))
        .max(3);
    let panel = Rect {
        x: input.x,
        y: input.y.saturating_sub(h + 1),
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
            // Every kind the submenu answers to. It listed four of the five,
            // and `n m memory` was reachable, undocumented, from the one menu
            // whose job is to document it.
            "new…        n s sched · n g goal · n h hook · n m memory · n t task".into(),
        ));
        rows.push(("e".into(), "editor      the input in $EDITOR".into()));
        // The verbs that lost their chord to tmux. They are drawn rather than
        // left to the keymap overlay because this menu is the only place they
        // are now reachable at all — a route nothing prints is a route nobody
        // takes. See `on_which_key`.
        rows.push(("j".into(), "jobs        background shells".into()));
        rows.push(("u".into(), "unread      the oldest thing unread".into()));
        rows.push(("l".into(), "clear       empty the screen only".into()));
        rows.push(("d".into(), "add dir     a directory to work in".into()));
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
    // What a short terminal is leaving out. `centred` clamps to the screen and
    // the list draws from the top, so at ten rows this menu simply stopped
    // after `a activity` — ten entries gone, including the only routes to
    // jobs, resume, search and the keymap, with nothing saying they existed.
    // The `?` overlay beside it has always said so; these two now agree.
    let room = panel.height.saturating_sub(2) as usize;
    let hidden = rows
        .iter()
        .skip(room.min(rows.len()))
        .filter(|(letter, _)| !letter.is_empty())
        .count();
    let bottom = if hidden > 0 {
        format!(" Esc cancels · {hidden} more — widen the window ")
    } else {
        " Esc cancels · any other key is ignored ".to_string()
    };
    f.render_widget(Clear, panel);
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(USER))
                .title(title)
                .title_bottom(bottom),
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
    } else if app.panel_focused && app.panel && app.projects_open {
        keys::catalog_keymap()
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

/// Returns the shape of the preview pane, for the one screen that has a
/// focusable one. Every other screen reports nothing, which is what keeps `⇥`
/// from offering a stop that does not exist.
fn draw_workspace(f: &mut Frame, app: &App, area: Rect) -> Preview {
    match app.workspace {
        Workspace::Fleet => return draw_fleet(f, app, area),
        Workspace::Memory => draw_memory(f, app, area),
        Workspace::MemoryGraph => draw_graph(f, app, area),
        Workspace::Schedules => draw_schedules(f, app, area),
        Workspace::Goals => draw_goals(f, app, area),
        Workspace::Hooks => draw_hooks(f, app, area),
        Workspace::Tasks => draw_tasks(f, app, area),
        Workspace::Activity => draw_activity(f, app, area),
        Workspace::Team => draw_team(f, app, area),
        Workspace::Roles => draw_roles(f, app, area),
        Workspace::Chat => {}
    }
    Preview::default()
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
        // Count the rows the pane is actually drawing.
        //
        // On the fleet with a tree — which is every fleet now — the rows are
        // the forest's, and `row_ids` answers with the *flat agent list* plus a
        // sentinel. The two collections are unrelated, so the number was
        // arbitrary: filtering for a word plainly present on four rows reported
        // `0 match` beside them. `tree_rows` is what `draw_tree` walks, and the
        // filter has already been applied to it, so nothing has to be
        // subtracted back off.
        let matched = if app.workspace == Workspace::Fleet && app.has_tree() {
            app.tree_rows().len()
        } else {
            // The flat list keeps its correction: its pinned row is a sentinel
            // that is always present and never a match.
            let unfilterable = usize::from(app.workspace == Workspace::Fleet);
            app.row_ids(app.workspace)
                .len()
                .saturating_sub(unfilterable)
        };
        format!("   ▸ filter · {matched} match")
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

fn empty(what: &str, width: u16) -> Vec<ListItem<'static>> {
    // Fitted, not left to the widget to clip. A silently cut sentence is not a
    // shorter sentence, it is a different one: at a hundred columns the memory
    // screen read "nothing remembered yet — /remember writes on", which is
    // both wrong and, being a grammatical phrase, not obviously truncated. An
    // ellipsis at least says something is missing.
    let inner = width.saturating_sub(2) as usize;
    vec![ListItem::new(Span::styled(cut(what, inner), fg(MUTED)))]
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
    // `tree_rows` runs past the forest: the loose pane's rows are on the end of
    // it, because both panes share one cursor. So the highlight is only this
    // pane's while the cursor is still inside it, and when it is not, the tree
    // stays parked on its own last row rather than scrolling to a position it
    // does not have.
    let cursor = app.tree.index(&ids);
    // The sentinel pinned row, and whether this pane draws one at all. It is
    // the fallback for a store whose forest has no `Main` node, exactly as
    // `App::tree_rows` treats it — the two counts have to be derived from the
    // same question or the cursor and the highlight drift apart.
    let lead = usize::from(!app.forest_holds_main());
    let mine = rows.len() + lead;
    let in_tree = cursor < mine;
    let selected = if in_tree {
        cursor
    } else {
        mine.saturating_sub(1)
    };
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

    // Position 0 is the pinned chat *only when the forest has no row of its
    // own for it*, and everything below reads its node at `at - lead`. This
    // ordering and `tree_rows`' have to agree, or the cursor lands one row off
    // its own highlight — the same trap the flat list documents, and the same
    // reason it is said twice.
    //
    // `tree_rows` already drops the sentinel once `forest_of` emits a
    // `NodeKind::Main` row, and drawing it anyway is how the two fell out of
    // step: the pane drew one more row than the cursor had ids, so from the
    // second row down the highlight sat above the row every verb acted on.
    // Pressing `x` on what looked like Jod's own row untracked the project
    // under it.
    let (start, height) = window(area, selected, mine);
    let mut items: Vec<ListItem> = Vec::new();
    for at in start..(start + height).min(mine) {
        let here = in_tree && at == selected;
        if lead == 1 && at == 0 {
            items.push(ListItem::new(Line::from(main_line(
                app,
                here,
                width,
                show_id,
                show_harness,
            ))));
            continue;
        }
        let node = rows[at - lead];
        let expanded = app.tree.is_expanded(&node.id, &app.closed_works);
        let mut spans = vec![
            Span::styled(if here { "▸ " } else { "  " }.to_string(), bold(USER)),
            // The guides describe the forest, so they are indexed into it —
            // a sentinel pinned row sits above the tree rather than in it, and
            // an elbow measured past it would point one row off.
            Span::styled(fleet::guides(&rows, at - lead, ascii), fg(MUTED)),
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
        // Columns, not characters. A label written in Japanese is half as
        // many characters as it is columns wide, and a budget that believed
        // the character count would hand the summary room the row does not
        // have — which the terminal then takes back by clipping the end of
        // the line off at the border.
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let mut room = width.saturating_sub(used);

        // A spinner, so a running node reads as moving rather than stuck. A run
        // that has stopped gets the glyph the flat list already gives that
        // status instead, because "not spinning" is the same picture for a run
        // that finished, one that failed and one that was killed — and a person
        // scanning the fleet needs to see the failure. Works and sessions have
        // no status of their own and keep the spinner-or-nothing they had.
        //
        // A stalled run is checked first and takes the spinner away, because a
        // stalled run is still `running` — that is the whole problem. An
        // animation is the strongest "this is fine" signal on the screen, and a
        // spinner turning on a wedged agent is the exact picture that let the
        // fleet fill up with hung sessions nobody noticed. It says how long,
        // too: "stalled" alone does not distinguish a run that went quiet a
        // minute ago from one that has been dead since yesterday.
        let mark = match (node.stalled_for_ms, node.running, node.status.as_deref()) {
            (Some(silent_for), _, _) => Some((
                format!(" ⏸ stalled {}", jod_core::heartbeat::human_ms(silent_for)),
                BAD,
            )),
            // A group row whose subtree holds a stalled run says so instead of
            // spinning. The fleet is read collapsed, and the spinner is the
            // strongest "this is fine" signal on the screen: a project drawing
            // one while its only engineer had been wedged for half an hour was
            // the original bug, one level up from where it was fixed. It takes
            // the row's whole mark rather than sitting beside the spinner,
            // because "working, and also stalled" is not a state.
            (None, _, None) if node.stalled > 0 => Some((
                if node.stalled == 1 {
                    " ⏸ stalled".to_string()
                } else {
                    format!(" ⏸ {} stalled", node.stalled)
                },
                BAD,
            )),
            (None, true, _) => Some((format!(" {}", app.spinner()), WARN)),
            (None, false, Some(status)) => {
                Some((format!(" {}", run_glyph(status)), status_colour(status)))
            }
            (None, false, None) => None,
        };
        if let Some((glyph, colour)) = mark {
            if room >= columns(&glyph) {
                room -= columns(&glyph);
                spans.push(Span::styled(glyph, fg(colour)));
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
            if room >= columns(&badge) {
                room -= columns(&badge);
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
        items.extend(empty(
            if app.here().filtering() {
                "  nothing matches"
            } else {
                "  no works yet"
            },
            area.width,
        ));
    }
    // The flat list has always drawn this and the tree never did — so once the
    // fleet had a tree, which it now always does, a filter hid rows with
    // nothing anywhere saying one was on. `★ jod` and whole projects vanished
    // and the screen looked like a fleet that had lost them.
    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }

    // Summed over the top-level rows, each of which already holds the blocked
    // count of everything under it. Counting every row instead would count a
    // card once on the agent that raised it and again on the project above it,
    // and the title would say twice the number the tree can show.
    let blocked: usize = rows.iter().filter(|n| n.depth == 0).map(|n| n.blocked).sum();
    // Both facts, because they are about different things and the second one
    // changes how much the first is worth. Without a daemon nothing marks a
    // stall, so every wedged agent on this screen draws as healthy — and the
    // screen saying nothing about that is what let the fleet fill up with hung
    // sessions nobody noticed. The transcript says it once when the console
    // starts; this says it for as long as it is true, on the screen where it
    // matters.
    let title = match (blocked, app.nothing_is_sweeping) {
        (0, false) => " fleet ".to_string(),
        (0, true) => " fleet · no stall watch · jod daemon ".to_string(),
        (n, false) => format!(" fleet · {n} blocked "),
        (n, true) => format!(" fleet · {n} blocked · no stall watch · jod daemon "),
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
fn draw_tree_detail(f: &mut Frame, app: &App, area: Rect) -> Preview {
    // The pinned chat gets the pane the flat list gives it, title and all:
    // none of `kind`, `id` or `state` means anything to a conversation, and
    // `selected_node` answers `None` for it — which would draw "nothing
    // selected" beside a row that is plainly selected.
    if app.tree_main_selected() {
        return preview_pane(
            f,
            app,
            area,
            main_detail(app, area.width),
            " the chat ",
            " ⏎ enter · /new leaves ",
            USER,
        );
    }
    // A run from the pane below the tree gets the pane the flat list gives it,
    // footer and all. `selected_node` answers `None` for it — it is a sentinel,
    // not a node — so without this the detail pane read "nothing selected"
    // beside a row that is plainly highlighted, and offered none of the verbs
    // the row actually answers.
    if let Some(a) = app.loose_selected().and_then(|_| app.selected_agent()) {
        return preview_pane(
            f,
            app,
            area,
            agent_detail(app, a, area.width),
            " run ",
            " ⏎ watch · s stop · r resume · f fork ",
            MUTED,
        );
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
                    jod_core::tree::NodeKind::Main => "jod",
                    jod_core::tree::NodeKind::Assistant => "assistant",
                    jod_core::tree::NodeKind::Project => "project",
                    jod_core::tree::NodeKind::Manager => "manager",
                    jod_core::tree::NodeKind::Work => "work",
                    jod_core::tree::NodeKind::Session => "session",
                    jod_core::tree::NodeKind::Run => "run",
                },
            ));
            lines.push(detail("id", &short(&node.id.id)));
            // A run says which of the four things it is. `running` or `idle`
            // was two words for four statuses, so a failed run and a clean
            // finish both read as "idle" — the pane that is supposed to explain
            // the row was hiding the one thing worth knowing about it. A work
            // and a session have no status of their own, so they keep the pair.
            //
            // A stall overrides the status here for the same reason it
            // overrides the spinner on the row: `runs.status` still says
            // `running`, truthfully, and that is the least useful true thing to
            // tell someone looking at a wedged agent.
            match (node.stalled_for_ms, node.status.as_deref()) {
                (Some(silent_for), _) => lines.push(detail_in(
                    "state",
                    &format!(
                        "stalled — running, but silent for {}",
                        jod_core::heartbeat::human_ms(silent_for)
                    ),
                    BAD,
                )),
                (None, Some(status)) => {
                    lines.push(detail_in("state", status, status_colour(status)))
                }
                (None, None) => lines.push(detail(
                    "state",
                    if node.running { "running" } else { "idle" },
                )),
            }
            if node.cards > 0 {
                lines.push(detail(
                    "cards",
                    &format!("{} open · {} {}", node.cards, node.blocked, rail::BLOCKED),
                ));
            }
            // Where the work is, which is the first thing a person asks after
            // "is it done". A work session reads the checkout and writes to a
            // worktree it claimed, so an agent can truthfully report a file
            // changed while the checkout on screen is untouched — and until
            // this was drawn, nothing anywhere said which directory to look
            // in. The branch comes first because it is the shorter answer and
            // the one that survives being written down.
            if let Some(branch) = &node.branch {
                lines.push(detail("branch", branch));
            }
            if let Some(worktree) = &node.worktree {
                // Cut from the left, like every other path on this screen: the
                // end of a worktree path is the repository name and the slug,
                // and the front of it is `$JOD_HOME/worktrees` on every row.
                let shown = under_home(
                    Path::new(worktree),
                    std::env::var_os("HOME").map(PathBuf::from).as_deref(),
                );
                lines.push(detail(
                    "worktree",
                    &fit_path(&shown, area.width.saturating_sub(14) as usize),
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
    // The verbs the selected row actually answers. The pane used to print none
    // for anything, which was true when a node was only ever a heading — and
    // stopped being true the moment `⏎` on an agent's row started going into
    // its conversation. A row you can enter, stop and resume, advertising
    // nothing, is a row nobody finds.
    let verbs = match app.selected_node().map(|node| node.kind) {
        Some(jod_core::tree::NodeKind::Session) => " ⏎ enter · s stop · r resume · f fork ",
        Some(jod_core::tree::NodeKind::Manager) => " ⏎ enter ",
        // The same one verb a manager's row answers, because it is the same
        // kind of row: a standing conversation you go into to read what it has
        // been deciding.
        Some(jod_core::tree::NodeKind::Assistant) => " ⏎ enter ",
        _ => "",
    };
    preview_pane(f, app, area, lines, " node ", verbs, MUTED)
}

/// The fleet's preview pane, scrolled to where the keyboard left it.
///
/// One function for all three of the things that pane can hold — the pinned
/// chat, a node, a run — because the scroll, the clamp and the border that says
/// who has the keyboard are the same for all three, and three copies of that is
/// three places for them to disagree about which pane is focused.
///
/// The clamp is here rather than only in the key handler because the content
/// changes underneath a scroll that does not: a run that was forty lines long
/// when `End` was pressed is nine lines long after it is stopped, and a
/// paragraph scrolled past its end draws an empty box.
fn preview_pane(
    f: &mut Frame,
    app: &App,
    area: Rect,
    lines: Vec<Line<'static>>,
    title: &str,
    verbs: &str,
    resting: Color,
) -> Preview {
    let shape = Preview {
        rows: area.height.saturating_sub(2) as usize,
        lines: lines.len(),
        area: Some(area),
    };
    // `resting` is the colour this pane wears when the rows have the keyboard,
    // and it is the caller's because it is not about focus: the chat is the
    // anchor of the screen and is drawn in `USER` whatever is selected, while a
    // node and a run are `MUTED`. Focus overrides all three, because the border
    // is the whole of how this pane says it has the keys — the loose pane below
    // the tree already brightens for the same reason, and a preview that looked
    // focused whether or not `↑` scrolled it would make its one key
    // unpredictable.
    let (border, footer) = if app.preview_focused {
        (USER, " ↑↓ scroll · ⇥ next pane · Esc back to the rows ")
    } else {
        (resting, verbs)
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(border))
                    .title(title.to_string())
                    // An empty `verbs` draws nothing, which is what the node
                    // pane has always done — it has no verbs of its own.
                    .title_bottom(fit_verbs(footer, area.width)),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.preview_scroll.min(shape.max_scroll()), 0)),
        area,
    );
    shape
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
    // A trailing space of its own, because `completed` is exactly nine
    // characters and the padding then adds none: beside a seven-character age
    // the two columns ran together as `completed206h26m`, which is one word
    // that is not a word. Every other column here is followed by a space for
    // the same reason.
    spans.push(Span::styled(
        format!("{:<9} ", a.status),
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
    let marker = if watched {
        "  \u{2190} on screen".to_string()
    } else {
        scratch_marker(app, &a.id)
    };
    let (name, marked) = fit_row(used, &a.name, &marker, inner);
    spans.push(Span::styled(
        name,
        // An archived scratch row is drawn shut, the way a closed work is: it is
        // on the screen because `z` asked for it, not because anything is
        // happening on it.
        if chosen {
            bold(USER)
        } else if app.scratch.archived.contains(&a.id) {
            fg(MUTED)
        } else {
            fg(AGENT)
        },
    ));
    if marked {
        spans.push(Span::styled(marker, fg(USER)));
    }
    Line::from(spans)
}

/// What a scratch row says about itself beside its name, if anything.
///
/// Three states and they are ordered by how much they need explaining.
///
/// A held row whose run has **stopped** gets the whole sentence, because that is
/// the row that otherwise looks like a sweep that failed: everything else on the
/// screen is there because it is running, and this one is there because somebody
/// said so. Core writes that sentence — see [`jod_core::tree::ScratchLane`] — and
/// it is deliberately not written for a held row that is still working, where
/// `kept` alone says everything there is to say.
///
/// An archived row says so too. It is only drawn at all because `z` asked for
/// the archives, and a row that looks live but cannot be continued is worse than
/// one that says it has been put away.
fn scratch_marker(app: &App, run: &str) -> String {
    if let Some(why) = app.scratch.why_held.get(run) {
        return format!("  {why}");
    }
    if app.scratch.held.contains(run) {
        return "  kept".to_string();
    }
    if app.scratch.archived.contains(run) {
        return "  put away".to_string();
    }
    String::new()
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
///
/// Shares the tree's cursor rather than keeping one of its own: `App::tree_rows`
/// puts these rows after the forest's, so walking off the bottom of the tree
/// arrives here. `here` is where that cursor is within this pane, and `None`
/// means it is still up in the tree.
fn draw_loose(f: &mut Frame, app: &App, area: Rect, runs: &[&super::AgentLine]) {
    let inner = area.width.saturating_sub(2) as usize;
    let show_id = inner >= 35;
    let show_harness = inner >= 31;
    let room = area.height.saturating_sub(2) as usize;
    let here = app.loose_selected();
    // Scrolled to the cursor rather than always to the top. The pane is three
    // or four rows tall and there can be forty runs in it, so a fixed window
    // would let the selection walk off the bottom of a box that never moved —
    // which looks exactly like a cursor that has stopped responding.
    let first = window_start(here.unwrap_or(0), room.max(1), runs.len());
    let items: Vec<ListItem> = runs
        .iter()
        .enumerate()
        .skip(first)
        .take(room)
        .map(|(at, a)| {
            ListItem::new(fleet_row(
                app,
                a,
                here == Some(at),
                inner,
                show_id,
                show_harness,
            ))
        })
        .collect();
    // The count is in the title rather than on a row of its own, because the
    // pane is small enough that a row spent saying "3 more" is a row not
    // spent showing one of them.
    let title = if runs.len() > room {
        format!(" loose · {} of {} ", room, runs.len())
    } else {
        format!(" loose · {} ", runs.len())
    };
    // The border brightens when the cursor is in here, because two stacked
    // panes with one cursor between them need to say which of them has it —
    // the highlighted row alone is easy to miss in a three-row box.
    let border = if here.is_some() { USER } else { MUTED };
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(border))
                .title(title)
                // `k keep` is printed here rather than on the fleet's key bar,
                // and that is the honest place for it: `k` steps the cursor up
                // everywhere else on this screen and means *keep* only while
                // the cursor is in this box. A bar that advertised it across
                // the whole fleet would be teaching a key that does something
                // else two rows higher.
                .title_bottom(fit_verbs(" in no work · k keep ", area.width)),
        ),
        area,
    );
}

fn draw_fleet(f: &mut Frame, app: &App, area: Rect) -> Preview {
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
        return match right {
            Some(right) => draw_tree_detail(f, app, right),
            None => Preview::default(),
        };
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
        items.extend(empty("  nothing delegated yet — Ctrl-B starts one", left.width));
    }
    if let Some(line) = filter_line(app) {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(line));
    }
    f.render_widget(body(Workspace::Fleet, items, left.width), left);

    let Some(right) = right else {
        return Preview::default();
    };
    // Its own pane, and its own footer: none of `s stop · r resume · f fork`
    // means anything to a conversation, and offering keys that quietly do
    // nothing is how a list teaches people not to trust its footer.
    if app.main_selected() {
        return preview_pane(
            f,
            app,
            right,
            main_detail(app, right.width),
            " the chat ",
            " ⏎ enter · /new leaves ",
            USER,
        );
    }
    let lines = match app.selected_agent() {
        None => vec![Line::from(Span::styled(" nothing selected", fg(MUTED)))],
        Some(a) => agent_detail(app, a, right.width),
    };
    preview_pane(
        f,
        app,
        right,
        lines,
        " run ",
        " ⏎ watch · s stop · r resume · f fork ",
        MUTED,
    )
}

/// One run, as the detail pane beside the fleet describes it.
///
/// Shared rather than copied for the same reason [`fleet_row`] is: this pane is
/// drawn for a run picked off the flat list, and now for a run picked out of
/// the loose pane under the tree. A run that reads one way on one screen and
/// another way on the other is a run you have to look at twice.
fn agent_detail(app: &App, a: &super::AgentLine, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(format!(" {}", a.name), bold(AGENT))),
        Line::from(Span::styled(format!(" {}", a.id), fg(MUTED))),
        Line::from(""),
        field("harness", &a.harness),
        field(
            "status",
            // The master column is 48 cells at the design width, so the inline
            // `← on screen` marker is the first thing *dropped* — whole, by
            // `fit_row`, never clipped to `← on scr`. This pane is where it is
            // always said, which is why dropping it there costs nothing above
            // 90 columns.
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
        // Above the spend on purpose: the question this pane is most often
        // opened with is "did that do what I asked", and a run launched
        // somewhere other than where you meant answers it before the cost does.
        // Cut from the left rather than left to wrap. A path is read from its
        // end — the repository and the slug — and a field that wrapped would
        // also make the pane one row taller than the line count the scroll is
        // clamped against, which puts its own last row out of reach.
        field(
            "in",
            &if a.cwd.is_empty() {
                "not recorded".to_string()
            } else {
                fit_path(&a.cwd, width.saturating_sub(13) as usize)
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
            for chunk in wrap(text, width.saturating_sub(4) as usize, 2) {
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
        empty("nothing remembered yet — /remember writes one", left.width)
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

/// The chain of command, and what each layer of it is spawned on.
///
/// Drawn as a tree because the nesting *is* the information: `main` hands to
/// `assistant`, which hands to `scratch` and `manager`, which hands to
/// `engineer`. `Store::role_list` answers alphabetically and cannot say any of
/// that — see [`roles::rows`], which is where the shape lives.
///
/// `●` is a role somebody has configured and `○` is one inheriting everything,
/// which is every role until this screen is opened.
fn draw_roles(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.role_rows();
    let selected = app
        .list(Workspace::Roles)
        .index(&app.row_ids(Workspace::Roles));
    let width = area.width.saturating_sub(2) as usize;
    // Declared drop order: permission → thinking → model. The harness stays,
    // because a model name means nothing without knowing whose it is.
    let show_permission = width >= 74;
    let show_thinking = width >= 58;
    let show_model = width >= 44;

    let mut lines: Vec<Line> = Vec::new();
    let mut header = vec![format!("  {:<20}", "role"), format!("{:<12}", "harness")];
    if show_model {
        header.push(format!("{:<16}", "model"));
    }
    if show_thinking {
        header.push(format!("{:<8}", "think"));
    }
    if show_permission {
        header.push(format!("{:<8}", "permission"));
    }
    lines.push(Line::from(Span::styled(header.concat(), fg(MUTED))));

    for (i, row) in rows.iter().enumerate() {
        let chosen = i == selected;
        // The branch and the name share one column, so the tree keeps its shape
        // whatever a role is called.
        let name = format!("{}{}", row.branch, row.role.as_str());
        let mut spans = vec![
            Span::styled(if chosen { "▸" } else { " " }, fg(USER)),
            Span::styled(
                format!("{} ", if row.configured { "●" } else { "○" }),
                fg(if row.configured { USER } else { MUTED }),
            ),
            Span::styled(
                format!("{:<18}", cut(&name, 18)),
                if chosen { bold(USER) } else { fg(AGENT) },
            ),
            cell(row, RoleField::Harness, 12),
        ];
        if show_model {
            spans.push(cell(row, RoleField::Model, 16));
        }
        if show_thinking {
            spans.push(cell(row, RoleField::Thinking, 8));
        }
        if show_permission {
            spans.push(cell(row, RoleField::Permission, 8));
        }
        lines.push(Line::from(spans));
    }

    // Said on the screen rather than left to be found out. A settings panel
    // whose changes do not touch what you are looking at is one people assume
    // is broken.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  {}", roles::WHEN_IT_TAKES_EFFECT),
        fg(MUTED),
    )));

    if let Some(row) = app.selected_role() {
        if roles::variant_caveat_applies(&row) {
            lines.push(Line::from(Span::styled(
                format!("  {}", roles::OPENCODE_VARIANT_CAVEAT),
                fg(MUTED),
            )));
        }
    }

    if let Some(choosing) = &app.choosing {
        lines.push(Line::from(""));
        lines.push(rule(width));
        lines.extend(chooser_lines(app, choosing));
    }

    f.render_widget(page(Workspace::Roles, app, lines, area.width), area);
}

/// One column of a role's row: the value, or the dash that says it inherits.
///
/// Muted when it inherits, so a screen of defaults reads as a screen of
/// defaults at a glance rather than as a table you have to compare cell by
/// cell.
fn cell(row: &roles::Row, field: RoleField, width: usize) -> Span<'static> {
    let text = row.cell(field);
    let colour = if row.value(field).is_some() {
        AGENT
    } else {
        MUTED
    };
    Span::styled(format!("{:<width$}", cut(text, width.saturating_sub(1))), fg(colour))
}

/// The list of values open over the panel, drawn inside the same box.
///
/// Inside rather than floating over the table, because the table is what you
/// are choosing *for*: a box covering the row you are editing would hide the
/// value you are about to replace.
fn chooser_lines<'a>(app: &App, choosing: &roles::Choosing) -> Vec<Line<'a>> {
    let role = choosing.role();
    let mut lines = vec![Line::from(Span::styled(
        match choosing {
            roles::Choosing::Field { .. } => format!("  {} — which of these?", role.as_str()),
            roles::Choosing::Value { field, .. } => {
                format!("  {} — {}", role.as_str(), field.as_str())
            }
        },
        bold(AGENT),
    ))];
    let at = choosing.selected();
    match choosing {
        // The four columns, each showing what it holds now, so choosing one is
        // reading the row as well as picking from it.
        roles::Choosing::Field { .. } => {
            let row = app.role_rows().into_iter().find(|r| r.role == role);
            for (i, field) in roles::FIELDS.into_iter().enumerate() {
                let now = match &row {
                    Some(row) => row.cell(field),
                    None => roles::INHERIT,
                };
                lines.push(option_line(i == at, field.as_str(), now));
            }
        }
        roles::Choosing::Value { field, options, .. } => {
            for (i, choice) in options.iter().enumerate() {
                lines.push(option_line(i == at, &choice.label, &choice.what));
            }
            // Whose names these are, when that is not the obvious answer.
            //
            // The list is whatever the harness *this console is on* said it
            // accepts, because that is the one Jod has already asked and asking
            // another means running its binary and waiting up to fifteen
            // seconds on a keypress. A row that names a different harness is
            // therefore being offered the wrong vocabulary, and saying so beats
            // a list that looks authoritative and is not — the name still goes
            // through verbatim, so anything can be set here.
            if *field == RoleField::Model {
                let row = app.role_rows().into_iter().find(|r| r.role == role);
                let theirs = row.and_then(|r| r.harness_kind());
                if theirs.is_some_and(|kind| kind != app.harness) {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "    these are {}'s names, and this row runs on {} — \
                             a name typed here is passed through as it is",
                            app.harness.label(),
                            theirs.expect("just matched").label()
                        ),
                        fg(MUTED),
                    )));
                }
            }
        }
    }
    lines.push(Line::from(Span::styled(
        "  ↑↓ move  ⏎ choose  Esc leave it alone",
        fg(MUTED),
    )));
    lines
}

fn option_line<'a>(chosen: bool, label: &str, what: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(if chosen { "  ▸ " } else { "    " }, fg(USER)),
        Span::styled(
            format!("{:<20}", cut(label, 19)),
            if chosen { bold(USER) } else { fg(AGENT) },
        ),
        Span::styled(what.to_string(), fg(MUTED)),
    ])
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
            "  the board is empty — n adds a task",
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
            // A dash rather than `0s` when nothing recorded when this task was
            // put on the board. `0s` reads as "just now", which is a claim
            // about a moment that never happened.
            spans.push(Span::styled(
                match t.age_ms {
                    0 => "—".to_string(),
                    age => super::app::short_duration(age),
                },
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

/// A message as one line, because a row is one line and a message is prose.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
            "── board ── empty · n adds one",
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
                .title_bottom(" ↑↓ pick · ⏎ mark done · n adds · Esc back "),
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
    detail_in(name, value, AGENT)
}

/// The same row, with the value in a colour the caller picks — a run's state
/// is the one that carries meaning, and red is most of how a failure is seen.
fn detail_in(name: &str, value: &str, colour: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {name:<10}"), fg(MUTED)),
        Span::styled(value.to_string(), fg(colour)),
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

/// How many terminal columns a string paints.
///
/// A `char` is not a column. A CJK ideograph or an emoji paints two of them,
/// and a combining accent paints none, so counting characters answers a
/// different question from the one a box's width asks. Ratatui already knows
/// the answer — this is the same measure it uses when it lays a line into the
/// buffer — so asking it keeps the budget and the paint in agreement, and
/// costs no dependency the renderer does not already have.
fn columns(s: &str) -> usize {
    Span::raw(s).width()
}

/// Truncate to `width` columns, saying so.
fn cut(s: &str, width: usize) -> String {
    if columns(s) <= width {
        return s.to_string();
    }
    // The ellipsis wants a column of its own, so the text keeps whatever
    // still fits beside it. A character that would straddle the last column
    // is dropped rather than half-drawn, which is why this walks the string
    // instead of slicing it.
    let budget = width.saturating_sub(1);
    let mut kept = String::new();
    let mut used = 0;
    let mut one = [0u8; 4];
    for c in s.chars() {
        let cost = columns(c.encode_utf8(&mut one));
        if used + cost > budget {
            break;
        }
        kept.push(c);
        used += cost;
    }
    format!("{kept}…")
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

/// Draws the conversation, and reports back how tall the pane is and how tall
/// what went in it came to — the two numbers scrolling has to be clamped to.
fn draw_transcript(f: &mut Frame, app: &App, area: Rect) -> (usize, usize) {
    let width = area.width.saturating_sub(2).max(1);
    let lines = transcript_lines(app, width);

    let viewport = area.height.saturating_sub(2).max(1) as usize;
    // `scroll` counts up from the bottom, but Paragraph scrolls down from the
    // top, so convert — and clamp, because the transcript can be shorter than
    // the window.
    let total = lines.len();
    let max_offset = total.saturating_sub(viewport);
    let offset = max_offset.saturating_sub(app.scroll.min(max_offset));

    // Naming what is on screen matters once several agents exist: a transcript
    // that could belong to any of them is a transcript you cannot trust.
    // The run being watched names the transcript; failing that, the
    // conversation the composer is bound to does. A manager is entered and then
    // has no run of its own until it is given one, so the run-based name left
    // it titled plainly `jod` — the one screen where knowing which project you
    // are typing into matters most.
    //
    // The kind of chat leads the title, because the three kinds are read
    // differently and used to be indistinguishable. Watching a delegated run is
    // somebody else's conversation and typing into it is not how you reply to
    // it; a project manager is where a project's standing instructions live;
    // main is the one that is yours. All three said `jod · something`, so the
    // only way to know which you were in was to remember how you got there.
    let watching = app
        .watching
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .map(|a| format!(" watching · {} ", a.name))
        .or_else(|| app.where_you_are().map(|where_| format!(" chat · {where_} ")))
        .unwrap_or_else(|| " chat ".to_string());
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
    (viewport, total)
}

/// The whole transcript as styled lines, with a blank line between blocks.
///
/// Split out of [`draw_transcript`] so the grouping can be tested without a
/// terminal, and because the spacing rule is the point of the function: the old
/// loop laid every entry directly under the one before it, so a question, the
/// six calls that answered it and the answer itself arrived as one unbroken
/// column of text with nothing for the eye to land on.
fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let ctx = Ctx {
        width,
        expanded: app.expand_details,
        frame: app.spinner(),
        agents: &app.agents,
    };
    let mut lines: Vec<Line> = Vec::new();
    // What the last block drawn was, so the next one knows whether it needs a
    // line of air above it. `None` until something is drawn, because the top of
    // the transcript is a boundary the reader can already see.
    let mut prev: Option<Chunk> = None;
    let mut put = |lines: &mut Vec<Line<'static>>, block: Chunk, body: Vec<Line<'static>>| {
        if body.is_empty() {
            return;
        }
        if prev.is_some_and(|p| parted(p, block)) {
            lines.push(Line::from(""));
        }
        lines.extend(body);
        prev = Some(block);
    };

    // A run of folded steps is replaced by one line saying how many there were
    // and which key brings them back, so a transcript never quietly loses
    // something. The count is of entries rather than of drawn lines: what the
    // reader is choosing to open is a number of steps, not a number of rows.
    let mut folded = 0usize;
    for (i, entry) in app.transcript.iter().enumerate() {
        if app.hidden(i) {
            folded += 1;
            continue;
        }
        // The marker stands in for the steps it replaced, so it is spaced as
        // one — a run of steps, with air above it and none between it and the
        // calls it is drawn among.
        put(&mut lines, Chunk::Step, fold_marker(app, folded, width));
        folded = 0;
        put(&mut lines, Chunk::of(entry), render(entry, &ctx));
    }
    put(&mut lines, Chunk::Step, fold_marker(app, folded, width));
    lines
}

/// Which visual block an entry belongs to.
///
/// Grouping is by kind rather than by turn because a turn is not what anybody
/// scans a transcript for. The question a reader is asking as they scroll is
/// "where did it start talking again", and the answer to that is a change of
/// kind: the calls stopped and the prose began.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chunk {
    /// What the user typed.
    You,
    /// Assistant prose.
    Say,
    /// Reasoning, when it is shown at all.
    Think,
    /// One run of steps: the tool calls of a turn, and the collapsed edits
    /// among them.
    Step,
    /// A tool's output. Never parted from the call above it — a line between
    /// them would read as output belonging to nothing.
    Out,
    /// The plan and a delegation: blocks several lines tall that are their own
    /// unit and would be swallowed inside a run of calls.
    Card,
    /// Something Jod said on its own account.
    Note,
    /// Something Jod said that has to be acted on.
    ///
    /// Apart from [`Chunk::Note`] rather than folded into it, because being
    /// told apart from the ordinary notices around it is the entire reason
    /// `Entry::Alert` exists. It does not pack, so a line of air goes above and
    /// below it even in a run of notices — which is where a blocked run is
    /// always announced.
    Alarm,
    /// The line a run ends on.
    End,
    /// Harness output we could not classify.
    Raw,
}

impl Chunk {
    fn of(entry: &Entry) -> Chunk {
        match entry {
            Entry::You(_) => Chunk::You,
            Entry::Agent(_) => Chunk::Say,
            Entry::Thinking(_) => Chunk::Think,
            Entry::Tool { .. } | Entry::Diff { .. } | Entry::Routing(_) => Chunk::Step,
            Entry::ToolOut { .. } => Chunk::Out,
            Entry::Plan(_) | Entry::Delegated { .. } | Entry::Carried { .. } => Chunk::Card,
            Entry::Notice(_) | Entry::Hint(_) => Chunk::Note,
            Entry::Alert(_) => Chunk::Alarm,
            Entry::Done { .. } => Chunk::End,
            Entry::Raw(_) => Chunk::Raw,
        }
    }

    /// Whether two neighbours of this kind sit together with no line between.
    ///
    /// True for the kinds that arrive as a series describing one thing: the
    /// call list of a turn, an answer pushed to the transcript one line at a
    /// time, a stretch of raw harness output. False for the kinds where each
    /// entry is a separate utterance — everything a person typed or the agent
    /// said arrives whole, so two of them in a row are two things, not one.
    fn packs(self) -> bool {
        matches!(self, Chunk::Step | Chunk::Note | Chunk::Raw | Chunk::Think)
    }
}

/// Whether a line of air goes between a block of kind `prev` and one of `cur`.
fn parted(prev: Chunk, cur: Chunk) -> bool {
    if cur == Chunk::Out {
        return false;
    }
    if prev == cur {
        return !cur.packs();
    }
    true
}

/// The single line standing in for `folded` steps that were not drawn.
///
/// Nothing at all when details are off: that setting is the answer to "I do not
/// want to see the steps", and a row per turn saying how many steps there were
/// is still seeing them. With details on the steps were on screen a moment ago
/// and folding them without a word would read as the transcript losing them, so
/// the line stays and names the key that opens it.
fn fold_marker(app: &App, folded: usize, width: u16) -> Vec<Line<'static>> {
    if folded == 0 || !app.show_details || app.expand_details {
        return vec![];
    }
    let style = fg(MUTED);
    let body = cut(
        &format!("{} · Ctrl-O", plural(folded, "step")),
        width.saturating_sub(2).max(1) as usize,
    );
    vec![Line::from(vec![
        Span::styled("⋯ ", style),
        Span::styled(body, style),
    ])]
}

/// One transcript entry as styled lines, already wrapped to `width`.
/// What an entry needs to know about the screen around it.
///
/// Bundled rather than passed as four arguments because every one of them is
/// needed by exactly one arm, and threading them individually through `render`
/// made its signature longer than most of the arms it dispatches to.
pub struct Ctx<'a> {
    pub width: u16,
    /// Whether `Ctrl-O` is currently holding the steps open.
    pub expanded: bool,
    /// This tick's spinner frame, for anything still running.
    pub frame: &'a str,
    /// The fleet, so a delegation can say whether the agent it started is still
    /// going.
    pub agents: &'a [AgentLine],
}

fn render(entry: &Entry, ctx: &Ctx) -> Vec<Line<'static>> {
    let width = ctx.width;
    let frame = ctx.frame;
    let expanded = ctx.expanded;
    let (prefix, style, body) = match entry {
        // Returns from inside the match rather than before it: the other
        // entries share a prefix/style/body shape that makes one-line entries
        // uniform, and a diff is the one entry whose whole point is that it is
        // not one line. An arm keeps the match exhaustive, so a new `Entry`
        // still fails the build here rather than falling through to a default.
        Entry::Diff { edit, step } => return render_diff(edit, *step, width, expanded, frame),
        Entry::Plan(items) => return render_plan(items, width),
        Entry::Delegated { id, prompt, dir } => {
            return render_delegated(id, prompt, dir, ctx)
        }
        Entry::Carried { heading, body } => return render_carried(heading, body, width, expanded),
        Entry::You(t) => ("› ", bold(USER), t.clone()),
        Entry::Agent(t) => return render_prose(t, width),
        Entry::Thinking(t) => ("  ", fg(MUTED).add_modifier(Modifier::ITALIC), t.clone()),
        Entry::Tool { name, detail, step } => {
            return render_call(name, detail.as_deref(), *step, width, frame)
        }
        Entry::ToolOut { text, failed } => return render_output(text, *failed, width, expanded),
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
        // A bar rather than a bullet, and red rather than amber. This is the
        // one entry the reader has to do something about, and in a column of
        // amber bullets a third amber bullet is not a signal.
        Entry::Alert(t) => ("▌ ", bold(BAD), t.clone()),
        // Reads as a notice, because while it is on screen it is one. What
        // separates it is only that it folds away with the other steps.
        Entry::Routing(t) => ("• ", fg(WARN), t.clone()),
        Entry::Raw(t) => ("", fg(MUTED), t.clone()),
    };

    laid_out(prefix, style, &body, width)
}

/// One entry's text, wrapped and hung under its own marker.
///
/// Split out of [`render`] so an arm whose marker is built at runtime — a
/// spinner frame changes four times a second — can use the same layout as the
/// arms whose marker is a literal.
fn laid_out(prefix: &str, style: Style, body: &str, width: u16) -> Vec<Line<'static>> {
    wrap(body, width as usize, prefix.chars().count())
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

/// How far assistant prose and tool output are set in from the left edge.
///
/// Two columns, which is exactly the width of the markers that lead them, so
/// the marker sits in the margin and every row of a block starts in the same
/// column whether it is the first row or the fortieth.
const PROSE: usize = 2;

/// How many rows of one tool's output are shown while the steps are folded shut.
///
/// Enough to see what came back, few enough that a command printing two hundred
/// lines of test output cannot push the question that prompted it off the top of
/// the screen. `Ctrl-O` opens the rest, and a failure is never held back at all.
const OUTPUT_ROWS: usize = 5;

/// A tool call: a status mark, the tool's name, and its argument beside it.
///
/// One line, cut rather than wrapped. A tool's argument is a command line or a
/// blob of JSON, and four rows of reflowed JSON per call is precisely what turns
/// the steps of a turn into a wall — the reason to have a call line at all is to
/// be able to skim twenty of them.
///
/// The name and the argument are styled apart, and that is the difference this
/// makes at a glance. `Bash(cargo test)` reads as a name and the thing it was
/// given; `Bash · cargo test`, all in one colour, reads as one long string.
fn render_call(
    name: &str,
    detail: Option<&str>,
    step: Step,
    width: u16,
    frame: &str,
) -> Vec<Line<'static>> {
    // The spinner is the whole point of the running state: a command that takes
    // four minutes used to look exactly like one that had already finished, so
    // the only way to tell whether anything was happening was to watch the
    // clock in the header.
    let (mark, colour) = match step {
        Step::Running => (format!("{frame} "), WARN),
        Step::Failed => ("✗ ".to_string(), BAD),
        Step::Ok => ("● ".to_string(), GOOD),
    };
    let mut spans = vec![
        Span::styled(mark.clone(), fg(colour)),
        Span::styled(name.to_string(), bold(AGENT)),
    ];
    if let Some(detail) = detail {
        // The brackets are reserved along with the mark and the name rather
        // than appended and hoped for, so a long argument loses its own tail
        // instead of the closing bracket. A line ending `--fea…)` says it was
        // cut; one ending `--fea` says nothing.
        let used = columns(&mark) + columns(name) + 2;
        let room = (width as usize).saturating_sub(used);
        if room > 1 {
            let detail = cut(&one_line(detail), room);
            spans.push(Span::styled(format!("({detail})"), fg(MUTED)));
        }
    }
    vec![Line::from(spans)]
}

/// What a tool gave back, hung under the call it belongs to.
///
/// Held to [`OUTPUT_ROWS`] while the steps are folded, with a line saying how
/// many rows were kept back and which key opens them — the same key that opens
/// every other step. A failure is never held back: it is the reason the answer
/// is about to be wrong, and a truncated one is a bug report you have to go and
/// reconstruct.
fn render_output(text: &str, failed: bool, width: u16, expanded: bool) -> Vec<Line<'static>> {
    let style = fg(if failed { BAD } else { MUTED });
    let rows = wrap(text, width as usize, PROSE + 2);
    let held = if expanded || failed {
        0
    } else {
        rows.len().saturating_sub(OUTPUT_ROWS)
    };
    let mut lines: Vec<Line<'static>> = rows
        .into_iter()
        .take(if held > 0 { OUTPUT_ROWS } else { usize::MAX })
        .enumerate()
        .map(|(i, text)| {
            // The elbow on the first row only. Repeated down forty rows it
            // stops reading as "this belongs to the call above" and starts
            // reading as a column of furniture.
            let lead = if i == 0 { "  ⎿ " } else { "    " };
            Line::from(vec![
                Span::styled(lead, fg(MUTED)),
                Span::styled(text, style),
            ])
        })
        .collect();
    if held > 0 {
        lines.push(Line::from(Span::styled(
            format!("    … {} · Ctrl-O", plural(held, "more line")),
            fg(MUTED),
        )));
    }
    lines
}

/// A carried-context seed: one line saying what arrived, and the document it
/// carried behind `Ctrl-O`.
///
/// Collapsed by default, and that default is the point. The body is a handoff
/// summary — the whole of what the model is holding now that this thread has
/// replaced the one before it — so on a chat that has just compacted itself it
/// is the only entry in the transcript and it is several screens long. Drawn in
/// full it reads as somebody else's report pasted into your own chat, and it
/// pushes the composer's own history off the top of a pane it cannot be
/// scrolled out of.
///
/// Never dropped, only folded. What the model was handed has to stay something
/// the reader can check.
fn render_carried(heading: &str, body: &str, width: u16, expanded: bool) -> Vec<Line<'static>> {
    let held = wrap(body, width as usize, PROSE).len();
    let mut lines = vec![Line::from(vec![
        Span::styled("⟲ ", fg(MUTED)),
        Span::styled(heading.to_string(), bold(MUTED)),
        Span::styled(
            if expanded {
                String::new()
            } else {
                // Worded and spaced exactly as the held-back rows of a tool's
                // output are — see [`render_output`]. It is the same offer, and
                // two spellings of one key would read as two different keys.
                format!(" · {} · Ctrl-O", plural(held, "line"))
            },
            fg(MUTED),
        ),
    ])];
    if expanded {
        lines.extend(render_prose(body, width));
    }
    lines
}

/// Assistant prose, read as prose rather than as one long paragraph.
///
/// The harnesses all write markdown and Jod used to print it verbatim, so a
/// reply arrived as a single wrapped block with `###` and `**` in it. The three
/// things worth honouring are the three that carry the structure: a blank line
/// is a paragraph, a `#` line is a heading, and a `-` line is a bullet.
/// Everything else is left exactly as it was written, including the leading
/// spaces of a code block.
fn render_prose(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Emphasis is carried across rows, so a run that straddles a wrap boundary
    // keeps its styling on both of them.
    let mut state = Emphasis::default();
    for raw in text.split('\n') {
        let lead = raw.chars().take_while(|c| *c == ' ').count();
        let trimmed = &raw[lead..];
        if trimmed.is_empty() {
            // A blank line in the source is a paragraph break, which is the
            // cheapest readability there is. Never one at the top and never two
            // in a row: both read as the renderer having dropped something.
            if lines.last().is_some_and(|l| l.width() > 0) {
                lines.push(Line::from(""));
            }
            continue;
        }
        // A heading is never indented, so an indented `#` is a comment in a
        // code block and is left alone.
        let heading = lead == 0 && trimmed.starts_with('#');
        let bullet = ["- ", "* ", "+ ", "• "]
            .iter()
            .any(|m| trimmed.starts_with(m));
        let (body, indent, marker) = if bullet {
            (&trimmed[2..], PROSE + lead + 2, "• ")
        } else if heading {
            // The hashes go the same way the `**` does: they are how the
            // harness said "bold", not something it wanted printed.
            (trimmed.trim_start_matches('#').trim_start(), PROSE, "")
        } else {
            (raw, PROSE + lead, "")
        };
        let style = if heading { bold(AGENT) } else { fg(AGENT) };
        for (i, row) in wrap(body, width as usize, indent).into_iter().enumerate() {
            // The first row of the whole message wears the marker; every other
            // row is padded to the same column. A bullet's own glyph wins over
            // it, because a bullet is already a marker.
            let pad = " ".repeat(indent.saturating_sub(marker.chars().count()));
            let lead = match (lines.is_empty(), i == 0) {
                (true, true) => "● ".to_string(),
                (_, true) => format!("{pad}{marker}"),
                _ => " ".repeat(indent),
            };
            lines.push(Line::from(
                [Span::styled(lead, fg(MUTED))]
                    .into_iter()
                    .chain(emphasised(&row, style, &mut state))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    lines
}

/// Whether an emphasis run is still open at the end of a row.
#[derive(Default, Clone, Copy)]
struct Emphasis {
    bold: bool,
    code: bool,
}

/// One row of prose, split into spans on the little markdown the harnesses
/// actually emit: `**bold**` and `` `code` ``.
///
/// Applied after wrapping rather than before, so `state` can carry a run across
/// a wrap boundary. The cost is that the markers were counted when the row was
/// measured, so a row containing emphasis stops a column or two short of the
/// edge. That is the right way round — the alternative is a row that overflows
/// the pane.
fn emphasised(row: &str, base: Style, state: &mut Emphasis) -> Vec<Span<'static>> {
    let dressed = |state: &Emphasis| {
        let style = if state.code { fg(CODE) } else { base };
        if state.bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        // Which run this character opens or closes, if any. A lone `*` is a
        // multiplication sign far more often than it is emphasis, so only the
        // doubled marker counts.
        let toggle = match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                Some(true)
            }
            '`' => Some(false),
            _ => None,
        };
        match toggle {
            Some(is_bold) => {
                if !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), dressed(state)));
                }
                if is_bold {
                    state.bold = !state.bold;
                } else {
                    state.code = !state.code;
                }
            }
            None => run.push(c),
        }
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, dressed(state)));
    }
    // A row that was nothing but markers still has to occupy its row, or the
    // paragraph above it closes up over it.
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
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
fn render_delegated(id: &str, prompt: &str, dir: &str, ctx: &Ctx) -> Vec<Line<'static>> {
    let width = ctx.width;
    // Whether the agent this block started is still going. The block used to
    // say "in the background, Ctrl-F to watch" for the rest of the session, no
    // matter what became of the run — so the transcript's account of a
    // delegation was frozen at the moment it was made, and the only way to find
    // out whether the work had finished was to leave the conversation.
    let agent = ctx.agents.iter().find(|a| a.id == id);
    let (mark, note, colour) = match agent {
        Some(a) if a.is_running() => (
            format!("  {} ", ctx.frame),
            " · running in the background, Ctrl-F to watch".to_string(),
            WARN,
        ),
        Some(a) => match a.status.as_str() {
            "completed" => ("  ✓ ".to_string(), " · finished".to_string(), GOOD),
            "failed" => ("  ✗ ".to_string(), " · failed".to_string(), BAD),
            "killed" => ("  ✗ ".to_string(), " · stopped".to_string(), BAD),
            other => ("  ⇢ ".to_string(), format!(" · {other}"), MUTED),
        },
        // Not in the fleet: the list is trimmed to the recent runs, so an old
        // delegation says nothing rather than guessing it is still going.
        None => (
            "  ⇢ ".to_string(),
            " · in the background, Ctrl-F to watch".to_string(),
            GOOD,
        ),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(mark, fg(colour)),
        Span::styled("delegated ".to_string(), bold(AGENT)),
        Span::styled(short(id), bold(USER)),
        Span::styled(note, fg(MUTED)),
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
fn render_diff(
    edit: &diff::Edit,
    step: Step,
    width: u16,
    expanded: bool,
    frame: &str,
) -> Vec<Line<'static>> {
    let room = (width as usize).saturating_sub(6);
    // The counts are reserved before the path is laid out, not appended after
    // it and hoped for: `room` used to be computed and then applied only to the
    // body, so an absolute path — every path, in a worktree — ran to the right
    // edge and pushed both the filename and the `+6 -0` off the screen.
    //
    // The verb is what turns a list of paths into a report: `created` and
    // `edited` are different facts about the same `+12 -0`, and while the call
    // is still out it reads `creating`, so the summary is a live account of the
    // work rather than a record written afterwards.
    let (marker, verb, colour) = match step {
        Step::Running => (format!("  {frame} "), edit.verb.doing(), WARN),
        Step::Failed => ("  ✗ ".to_string(), edit.verb.done(), BAD),
        Step::Ok => ("  ± ".to_string(), edit.verb.done(), GOOD),
    };
    let counts = format!("  +{} -{}", edit.added, edit.removed);
    let label = format!("{verb} ");
    let mut lines = vec![Line::from(vec![
        Span::styled(marker.clone(), fg(colour)),
        Span::styled(label.clone(), fg(MUTED)),
        Span::styled(
            text::path_beside(
                &edit.path,
                room,
                marker.chars().count() + label.chars().count() + counts.chars().count(),
            ),
            bold(AGENT),
        ),
        Span::styled(counts, fg(MUTED)),
    ])];
    // Collapsed, the summary line *is* the entry. That is the level most
    // reading happens at — "which files changed, and how much" — and forty rows
    // of diff per edit buries the conversation the transcript exists to show.
    // `Ctrl-O` opens the body, the same key that opens every other step.
    if !expanded {
        return lines;
    }
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
    // Where this line is going, on the box it is typed into. Only for main and
    // for a manager: an ordinary session is already named by the transcript
    // above it, while those two look identical from the chair and differ in
    // what typing does — main routes across every project, a manager acts
    // inside exactly one. What is queued or in flight wins the title when there
    // is something to say about it, because that is the newer fact.
    let bound = app
        .where_you_are()
        .map(|where_| format!(" you → {where_} "))
        .unwrap_or_else(|| " you ".to_string());
    let title = match (app.busy, app.queued.len()) {
        (_, n) if n > 0 => format!(" you · {n} queued "),
        (true, _) => format!(
            " you · sends after this turn{} ",
            app.elapsed().map(|t| format!(" ({t})")).unwrap_or_default()
        ),
        _ => bound,
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
    let wrapped = wrap_composer(&typed, field, col);
    let lines_total = wrapped.starts.len();
    let (caret_row, caret_col) = wrapped.caret;
    // The box has already grown for what was typed, so this only bites past the
    // cap: then the rows scroll to keep the caret in view, the same rule the
    // field used to apply sideways.
    let rows = area.height.saturating_sub(2).max(1) as usize;
    let first = window_start(caret_row, rows, lines_total);

    // Muted while the field is empty so the caret and the hint read as one
    // piece of furniture; live the moment there is something to send.
    let lines: Vec<Line> = if app.input.is_empty() {
        vec![Line::from(vec![
            Span::styled(caret, fg(MUTED)),
            Span::styled(placeholder(field), fg(MUTED)),
        ])]
    } else {
        let style = fg(if app.busy { WARN } else { USER });
        (first..lines_total.min(first + rows))
            .map(|row| {
                // The caret marks where the line starts, so it goes on the first
                // row only; the rest are indented to it, and the wrapped text
                // keeps one left edge instead of two.
                let lead = if row == 0 {
                    Span::styled(caret, style)
                } else {
                    Span::raw(" ".repeat(gutter))
                };
                let text: String = typed[wrapped.row(row, typed.len())].iter().collect();
                Line::from(vec![lead, Span::raw(text)])
            })
            .collect()
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
    let row = caret_row.saturating_sub(first).min(rows - 1);
    f.set_cursor_position((
        area.x + 1 + (gutter + caret_col) as u16,
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

    // ---- the flash ----

    fn flashing(lines: &[&str]) -> App {
        let mut a = app();
        a.go(Workspace::Fleet);
        for line in lines {
            a.push(Entry::Notice((*line).to_string()));
        }
        a
    }

    /// The other half of the fix. Keeping a notice out of the conversation is
    /// only right if it is readable where it was raised — otherwise the key
    /// silently does nothing, which is worse than the clutter.
    #[test]
    fn a_notice_raised_on_the_fleet_is_drawn_over_the_fleet() {
        let a = flashing(&["untracking is a repository's, so `x` works on a project row"]);
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("works on a project row"), "{frame}");
        assert!(frame.contains("notice"), "and it is labelled: {frame}");
        assert!(
            frame.contains("fleet"),
            "with the screen it belongs to still under it: {frame}"
        );
    }

    /// Check 30, on the screen. The chain of command is the drawing, so the
    /// nesting has to survive the renderer: `assistant` is indented under
    /// `main`, `engineer` under `manager`, and `housekeeping` back at the root.
    #[test]
    fn the_roles_panel_draws_the_chain_of_command() {
        let mut a = app();
        a.go(Workspace::Roles);
        a.reconcile();
        let frame = rendered(&a, 120, 30);

        // Anchored on the row's own glyph, because the status bar and the
        // detail text also say words like `main` and the first matching line
        // would not be a row of this table.
        let indent = |name: &str| -> usize {
            let row = frame
                .lines()
                .find(|l| l.contains('○') && l.contains(name))
                .unwrap_or_else(|| panic!("`{name}` is not drawn:\n{frame}"));
            // Cells, not bytes. The border, the glyph and the branch are all
            // multi-byte, so a byte offset would say `main` is further right
            // than `housekeeping` purely because it has a `▸` in front of it.
            let at = row.find(name).expect("the row holds the name");
            row[..at].chars().count()
        };
        assert!(
            indent("assistant") > indent("main"),
            "assistant is not drawn under main:\n{frame}"
        );
        assert!(
            indent("engineer") > indent("manager"),
            "engineer is not drawn under manager:\n{frame}"
        );
        assert_eq!(
            indent("housekeeping"),
            indent("main"),
            "nothing delegates to housekeeping, so it belongs at the root:\n{frame}"
        );
    }

    /// A column nobody has set says *inherit* rather than sitting empty, which
    /// would read as a value that failed to load.
    #[test]
    fn an_unconfigured_role_draws_a_dash_in_every_column() {
        let mut a = app();
        a.go(Workspace::Roles);
        a.reconcile();
        let frame = rendered(&a, 120, 30);
        let row = frame
            .lines()
            .find(|l| l.contains('○') && l.contains("main"))
            .unwrap_or_else(|| panic!("{frame}"));
        assert_eq!(
            row.matches(roles::INHERIT).count(),
            4,
            "all four columns inherit on a machine nobody has configured:\n{frame}"
        );
        assert!(
            frame.contains("already going are untouched"),
            "the panel has to say when an edit takes effect:\n{frame}"
        );
    }

    /// The list a letter opens is drawn inside the panel rather than over the
    /// row being edited — the value you are replacing has to stay readable.
    #[test]
    fn the_chooser_is_drawn_under_the_table_it_is_editing() {
        let mut a = app();
        a.go(Workspace::Roles);
        a.reconcile();
        a.choosing = Some(roles::Choosing::Value {
            role: jod_core::harness::Role::Main,
            field: RoleField::Harness,
            options: roles::options(RoleField::Harness, None, &[]),
            selected: 0,
        });
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("main — harness"), "{frame}");
        assert!(frame.contains("claude_code"), "{frame}");
        assert!(frame.contains("Esc leave it alone"), "{frame}");
        assert!(
            frame.contains("housekeeping"),
            "the table is still readable under it:\n{frame}"
        );
    }

    /// The model list is whatever the harness this console is on said it
    /// accepts, and a row that names a different one has to be told so — a list
    /// that looks authoritative and is not is worse than no list.
    #[test]
    fn the_model_list_says_whose_names_it_is_offering() {
        let mut a = app();
        a.roles = vec![jod_core::store::RoleRow {
            role: "main".into(),
            harness: Some(HarnessKind::OpenCode.id().into()),
            ..Default::default()
        }];
        a.go(Workspace::Roles);
        a.reconcile();
        a.choosing = Some(roles::Choosing::Value {
            role: jod_core::harness::Role::Main,
            field: RoleField::Model,
            options: roles::options(RoleField::Model, Some(HarnessKind::OpenCode), &a.models),
            selected: 0,
        });

        let frame = rendered(&a, 120, 30);
        assert!(
            frame.contains("this row runs on OpenCode"),
            "the console is on Claude Code and the row is not:\n{frame}"
        );
    }

    /// It has to leave the screen usable. A notice covering the fleet is a
    /// modal with no key to dismiss it.
    #[test]
    fn a_long_answer_gives_most_of_the_screen_back_to_the_fleet() {
        let rows: Vec<String> = (0..60).map(|i| format!("conversation number {i}")).collect();
        let a = flashing(&rows.iter().map(String::as_str).collect::<Vec<_>>());
        let frame = rendered(&a, 120, 30);
        let over = frame.lines().filter(|l| l.contains("conversation number")).count();
        assert!(over <= 12, "the flash took {over} rows: {frame}");
        assert!(
            frame.contains("more"),
            "and a list cut short has to say so: {frame}"
        );
    }

    /// An overlay can raise a notice and stay open. Drawn under the overlay that
    /// raised it, the refusal is invisible, which is the original fault one
    /// layer further in.
    #[test]
    fn a_notice_raised_by_an_open_overlay_is_drawn_over_it() {
        let mut a = flashing(&["a fresh fork never reported a conversation"]);
        a.overlay = Overlay::Search {
            query: String::new(),
            selected: 0,
            hits: Vec::new(),
        };
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("never reported a conversation"), "{frame}");
        assert!(
            frame.contains("search every transcript"),
            "and the overlay that raised it is still readable above: {frame}"
        );
    }

    /// On chat the same words are in the transcript, which is on screen. Drawn
    /// again over it they would be the same sentence twice.
    #[test]
    fn chat_draws_no_flash() {
        let mut a = flashing(&["nothing to stop"]);
        a.workspace = Workspace::Chat;
        let frame = rendered(&a, 120, 30);
        assert!(!frame.contains("nothing to stop"), "{frame}");
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
            created_at_ms: 0,
            paths: Vec::new(),
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
    /// row said which was which. The second form is gone rather than relabelled,
    /// so the fix is now that there is exactly one row.
    ///
    /// Counted over the popup rather than the whole screen: `/main` is also in
    /// the input box being typed, and a screen-wide count would see two.
    #[test]
    fn main_is_offered_once() {
        let mut a = app();
        a.input = "/main".into();
        let screen = rendered(&a, 120, popup_height());
        let rows: Vec<&str> = screen
            .lines()
            .filter(|line| line.contains("go into the main chat"))
            .collect();
        assert_eq!(rows.len(), 1, "one /main row, not two:\n{screen}");
        let line = rows[0];
        let label = line[..line.find("go into the main chat").unwrap()]
            .trim_matches(|c: char| c.is_whitespace() || c == '│' || c == '▸')
            .to_string();
        assert_eq!(label, "/main", "{screen}");
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
        let last = crate::tui::command::HELP.last().unwrap().1;
        let starts: Vec<usize> = screen
            .lines()
            .filter_map(|l| column(l, "this list").or_else(|| column(l, last)))
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
    /// the `chat` box above it reads as a rendering slip. They line up.
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
            box_span(&screen, "┌ chat "),
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

    /// T1: a line with a double-width character in it lost text at the wrap.
    ///
    /// The composer used to fill each row with a fixed number of characters
    /// and assume that number of columns had been used. A Japanese ideograph
    /// or an emoji paints two columns, so a row of thirty-two characters could
    /// paint thirty-six columns into a field thirty-two wide. The paragraph
    /// clipped the overflow at the border and the characters that fell off
    /// were simply gone: at forty columns `FFFF` came back as `F`.
    ///
    /// Read off the painted cells rather than off any length the code
    /// computes, because a computed length is the thing that was wrong.
    ///
    /// Five cases. The first is the straddling one, the worst version, where
    /// the wide character's second cell falls past the edge and the character
    /// vanishes whole. The next two are the lines from the report. The last
    /// two are the twins that already wrapped correctly — the emoji line and
    /// the plain ASCII line — and they are here so that a fix cannot pass by
    /// breaking every line earlier than it needs to.
    ///
    /// Each case is checked twice over. Every character has to survive
    /// somewhere in the box, which is what the bug broke; and the box has to
    /// use the number of rows the text actually needs, which is what stops a
    /// fix wrapping harder to be safe. Spaces are dropped from the first
    /// comparison because a row is padded out to the right border with blanks,
    /// and a blank there cannot be told apart from a space someone typed.
    #[test]
    fn a_wide_character_at_the_wrap_keeps_every_character() {
        // At forty columns the field is thirty-two, so each of these needs two
        // rows: thirty-nine, thirty-six, thirty-eight, thirty-seven and
        // thirty-nine columns of text respectively.
        let straddle = format!("{}日{}", "A".repeat(31), "B".repeat(6));
        for (text, rows_wanted) in [
            (straddle.as_str(), 2),
            ("AAAA BBBB CCCC 日本語 DDDD EEEE FFFF", 2),
            ("tanong: anong ginagawa? 日本語 🚀 café", 2),
            ("AAAA BBBB CCCC 🚀 DDDD EEEE FFFF GGGG", 2),
            ("AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH", 2),
        ] {
            let mut a = app();
            a.input = text.to_string();
            a.cursor = a.input.len();
            let rows = painted_composer(&a, 40, 20);
            assert_eq!(
                rows.concat().replace(' ', ""),
                text.replace(' ', ""),
                "at forty columns {text:?} was painted as {rows:?}",
            );
            assert_eq!(
                rows.len(),
                rows_wanted,
                "{text:?} needs {rows_wanted} rows, not {}: {rows:?}",
                rows.len(),
            );
        }
    }

    /// The composer's own check, as the task states it: the wrapped rows
    /// rejoin to exactly what was typed, for a mix of ASCII, CJK and emoji, at
    /// every terminal width from twenty to a hundred and twenty.
    ///
    /// The string has no spaces in it, so the rejoin can be exact. A row is
    /// padded out to the right of the box with blanks, and a blank at the end
    /// of a row is indistinguishable from a space someone typed there, so a
    /// string with spaces could only be compared loosely.
    #[test]
    fn the_wrapped_rows_rejoin_to_what_was_typed_at_every_width() {
        let typed = "AAAA日本語BBBB🚀CCCCcaféDDDD漢字EEEEFFFF🌟GGGG中文HHHH";
        let mut a = app();
        a.input = typed.to_string();
        a.cursor = a.input.len();
        for width in 20..=120u16 {
            let rows = painted_composer(&a, width, 24);
            assert_eq!(
                rows.concat(),
                typed,
                "at {width} columns the rows were {rows:?}",
            );
        }
    }

    /// The composer's rows as the terminal paints them, one cell at a time.
    ///
    /// A wide character owns two cells and leaves the second one blank, so
    /// reading the cells as if each held one character would count that blank
    /// as a space. This walks by columns instead: it takes the symbol sitting
    /// in a cell, then steps over as many cells as that symbol paints. The
    /// blanks a row is padded out with on the right are dropped, since they
    /// are the box's fill rather than anything that was typed.
    fn painted_composer(a: &App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                draw(f, a);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let at = |x: u16, y: u16| buffer[(x, y)].symbol().to_string();
        let (top, left, right) = (0..buffer.area.height)
            .find_map(|y| {
                let row: String = (0..buffer.area.width).map(|x| at(x, y)).collect();
                if !row.contains("┌ you ") {
                    return None;
                }
                let start = (0..buffer.area.width).find(|x| at(*x, y) == "┌")?;
                let end = (start..buffer.area.width).find(|x| at(*x, y) == "┐")?;
                Some((y, start, end))
            })
            .expect("a composer box");

        let mut rows = Vec::new();
        let mut y = top + 1;
        while y < buffer.area.height && at(left, y) != "└" {
            let mut line = String::new();
            let mut x = left + 1 + CARET.chars().count() as u16;
            while x < right {
                let symbol = at(x, y);
                x += columns(&symbol).max(1) as u16;
                line.push_str(&symbol);
            }
            rows.push(line.trim_end().to_string());
            y += 1;
        }
        rows
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

    /// The summary a compaction leaves behind is a handoff document, and the
    /// thread it seeds contains it and nothing else. Drawn in full it is the
    /// whole screen, and it is text the reader never wrote and the agent never
    /// just said.
    #[test]
    fn a_carried_summary_arrives_folded_to_one_line() {
        let mut a = app();
        a.push(Entry::Carried {
            heading: "the conversation so far, compacted".into(),
            body: "# Handoff Summary\n\nThe manager was started and acknowledged, \
                   but had raised nothing by the end of this conversation."
                .into(),
        });
        let out = rendered(&a, 80, 20);
        assert!(
            out.contains("the conversation so far, compacted"),
            "the reader is told one arrived: {out}"
        );
        assert!(out.contains("Ctrl-O"), "and how to read it: {out}");
        assert!(
            !out.contains("Handoff Summary"),
            "but the document itself stays shut: {out}"
        );
    }

    /// Folded, never dropped. A summary the model is being handed has to stay
    /// something the reader can check.
    #[test]
    fn the_carried_summary_opens_with_the_rest_of_the_details() {
        let mut a = app();
        a.push(Entry::Carried {
            heading: "the conversation so far, compacted".into(),
            body: "# Handoff Summary\n\nThe manager was started.".into(),
        });
        a.expand_details = true;
        let out = rendered(&a, 60, 20);
        assert!(out.contains("Handoff Summary"), "{out}");
        assert!(out.contains("The manager was started"), "{out}");
    }

    /// Scrolling is clamped to the lines on screen, not to the number of
    /// entries that produced them. Clamping to the entry count left a chat
    /// holding one long message stuck two lines from the bottom with the rest
    /// of the message above it and no way to reach it — which is exactly the
    /// shape of a freshly compacted `main`.
    #[test]
    fn one_long_entry_can_be_scrolled_back_to_its_first_line() {
        let mut a = app();
        let body = (0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.push(Entry::Agent(body));
        assert_eq!(a.transcript.len(), 1, "one entry, sixty lines");

        let max = painted(&a, 60, 14).max_scroll();
        assert!(
            max > a.transcript.len(),
            "the frame counts lines, not entries: {max}"
        );
        assert!(!rendered(&a, 60, 14).contains("line 0 "), "starts at the end");

        a.scroll_up(max, max);
        let top = rendered(&a, 60, 14);
        assert!(top.contains("line 0"), "and scrolling reaches the top: {top}");
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

    // ---- how a transcript is laid out ----

    /// The transcript's rows, borders stripped, so a test can assert on the
    /// blank lines *between* blocks rather than on the text inside them.
    fn transcript_rows(a: &App, width: u16, height: u16) -> Vec<String> {
        let screen = rendered(a, width, height);
        let rows: Vec<&str> = screen.lines().collect();
        let top = rows
            .iter()
            .position(|r| r.contains("┌ chat") || r.contains("┌ watching"))
            .expect("a transcript box");
        let bottom = rows[top..]
            .iter()
            .position(|r| r.contains('└'))
            .expect("its bottom border")
            + top;
        rows[top + 1..bottom]
            .iter()
            .map(|r| {
                let inner: String = r.chars().filter(|c| *c != '│').collect();
                inner.trim_end().to_string()
            })
            .collect()
    }

    /// The rows that carry something, in order, with the blank ones marked —
    /// which is the shape a spacing test is actually about.
    fn shape(rows: &[String]) -> Vec<&str> {
        let end = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
        rows[..end]
            .iter()
            .map(|r| if r.is_empty() { "" } else { r.as_str() })
            .collect()
    }

    /// The complaint this answers: a question, the calls that answered it and
    /// the answer arrived as one unbroken column with nothing for the eye to
    /// land on.
    #[test]
    fn a_line_of_air_separates_the_question_from_the_answer() {
        let mut a = app();
        a.push(Entry::You("what is the progress of zuma?".into()));
        a.push(Entry::Agent("Zuma is on its third milestone.".into()));
        let rows = transcript_rows(&a, 60, 12);
        let shape = shape(&rows);
        assert_eq!(shape.len(), 3, "question, air, answer:\n{rows:#?}");
        assert!(shape[0].contains("what is the progress"), "{shape:#?}");
        assert_eq!(shape[1], "", "a line between them:\n{rows:#?}");
        assert!(shape[2].contains("Zuma is on its third"), "{shape:#?}");
    }

    /// The other half of the rule. Air between blocks is only worth anything if
    /// there is none *inside* one, and a turn's calls are one block: six tool
    /// lines double-spaced is the wall again with twice the scrolling.
    #[test]
    fn the_calls_of_one_turn_stay_packed_together() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        for name in ["Read", "Grep", "Bash"] {
            a.push(Entry::Tool {
                name: name.into(),
                detail: Some("something".into()),
                step: Step::Ok,
            });
        }
        let rows = transcript_rows(&a, 60, 12);
        let shape = shape(&rows);
        assert_eq!(shape.len(), 3, "three calls and no gaps:\n{rows:#?}");
        assert!(shape.iter().all(|r| !r.is_empty()), "{rows:#?}");
    }

    /// Output belongs to the call above it. A line between them would read as
    /// output belonging to nothing, which is what the elbow is there to deny.
    #[test]
    fn a_tools_output_hangs_directly_under_its_call() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.push(Entry::Tool {
            name: "Bash".into(),
            detail: Some("cargo test".into()),
            step: Step::Ok,
        });
        a.push(Entry::ToolOut {
            text: "1235 passed".into(),
            failed: false,
        });
        let rows = transcript_rows(&a, 60, 12);
        let shape = shape(&rows);
        assert_eq!(shape.len(), 2, "no gap between them:\n{rows:#?}");
        assert!(shape[1].contains('⎿'), "hung under the call:\n{rows:#?}");
        assert!(shape[1].contains("1235 passed"), "{rows:#?}");
    }

    /// A call is a name and the thing it was given, and telling them apart at a
    /// glance is the point of the bracket.
    #[test]
    fn a_call_names_the_tool_and_brackets_its_argument() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.push(Entry::Tool {
            name: "Bash".into(),
            detail: Some("cargo test --all".into()),
            step: Step::Ok,
        });
        let rows = transcript_rows(&a, 60, 12);
        assert!(rows[0].contains("Bash(cargo test --all)"), "{rows:#?}");
    }

    /// A long argument loses its own tail rather than the closing bracket: a
    /// line that stops mid-argument with no `)` does not say it was cut.
    #[test]
    fn a_long_argument_is_cut_inside_its_brackets() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.push(Entry::Tool {
            name: "Bash".into(),
            detail: Some("cargo test ".repeat(40)),
            step: Step::Ok,
        });
        let rows = transcript_rows(&a, 60, 12);
        assert_eq!(
            rows.iter().filter(|r| !r.is_empty()).count(),
            1,
            "{rows:#?}"
        );
        assert!(rows[0].ends_with("…)"), "cut inside the bracket:\n{rows:#?}");
        assert!(rows[0].chars().count() <= 60, "{rows:#?}");
    }

    /// Two hundred lines of test output used to push the question that prompted
    /// it off the top of the screen.
    #[test]
    fn a_long_tool_output_is_held_back_with_a_count_and_a_key() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.push(Entry::Tool {
            name: "Bash".into(),
            detail: Some("cargo test".into()),
            step: Step::Ok,
        });
        a.push(Entry::ToolOut {
            text: (0..40)
                .map(|i| format!("row {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            failed: false,
        });
        let rows = transcript_rows(&a, 60, 40);
        let shown = rows.iter().filter(|r| r.contains("row ")).count();
        assert_eq!(shown, OUTPUT_ROWS, "held to the cap:\n{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("35 more lines · Ctrl-O")),
            "and says what it kept back:\n{rows:#?}"
        );
    }

    /// A failure is the reason the answer is about to be wrong, and a truncated
    /// one is a bug report the reader has to go and reconstruct.
    #[test]
    fn a_failed_tools_output_is_never_held_back() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.push(Entry::ToolOut {
            text: (0..40)
                .map(|i| format!("row {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            failed: true,
        });
        let rows = transcript_rows(&a, 60, 60);
        assert_eq!(
            rows.iter().filter(|r| r.contains("row ")).count(),
            40,
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|r| r.contains("Ctrl-O")), "{rows:#?}");
    }

    /// The harnesses all write markdown. Printing it verbatim gave a reply with
    /// `###` and `**` in it and no paragraphs at all.
    #[test]
    fn prose_keeps_its_paragraphs_and_loses_its_markers() {
        let mut a = app();
        a.push(Entry::Agent(
            "## Repository Status\n\nThe tree is **clean** and `cargo test` passes.\n\n\
             - 91 tests\n- 29 files"
                .into(),
        ));
        let rows = transcript_rows(&a, 60, 20);
        let shape = shape(&rows);
        assert!(shape[0].contains("Repository Status"), "{rows:#?}");
        assert_eq!(shape[1], "", "a paragraph break survives:\n{rows:#?}");
        assert!(
            shape[2].contains("clean") && !shape[2].contains("**"),
            "the markers are style, not text:\n{rows:#?}"
        );
        assert!(
            shape[2].contains("cargo test") && !shape[2].contains('`'),
            "{rows:#?}"
        );
        assert!(
            shape.iter().any(|r| r.contains("• 91 tests")),
            "and a bullet is a bullet:\n{rows:#?}"
        );
    }

    /// `wrap` protects the leading spaces of a code block on purpose, and the
    /// prose pass must not undo it — a `def f():` and its body in the same
    /// column is the bug being avoided.
    #[test]
    fn an_indented_block_inside_prose_keeps_its_indentation() {
        let mut a = app();
        a.push(Entry::Agent("here:\n\n    def f():\n        return 1".into()));
        let rows = transcript_rows(&a, 60, 20);
        let body = rows
            .iter()
            .find(|r| r.contains("return 1"))
            .expect("the body survives");
        let def = rows.iter().find(|r| r.contains("def f")).expect("the head");
        let indent = |r: &str| r.chars().take_while(|c| *c == ' ').count();
        assert!(indent(body) > indent(def), "still nested:\n{rows:#?}");
    }

    /// The three kinds of chat are read differently and used to be
    /// indistinguishable: all three said `jod · something`.
    #[test]
    fn the_title_says_which_kind_of_chat_this_is() {
        let mut a = app();
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        a.watching = Some("aaa11111".into());
        assert!(
            rendered(&a, 80, 12).contains("watching · port the parser"),
            "somebody else's conversation says so"
        );
    }

    // ---- folding the steps away ----

    /// One finished turn: a question, the hand-off, a tool call and what it
    /// returned, the answer, and the ending.
    fn a_turn_with_steps() -> App {
        let mut a = app();
        a.push(Entry::You("what is the progress of zuma?".into()));
        a.push(Entry::Routing(
            "→ 16b9a192 · handed to the orchestrator".into(),
        ));
        a.push(Entry::Tool {
            name: "project_switch".into(),
            detail: Some("{\"project\":\"zuma\"}".into()),
            step: Step::Ok,
        });
        a.push(Entry::ToolOut {
            text: "switched to zuma".into(),
            failed: false,
        });
        a.push(Entry::Agent("Zuma is on its third milestone.".into()));
        a.push(Entry::Done {
            text: "12s".into(),
            failed: false,
        });
        a
    }

    /// What the change is for. Scrolling back through a day of work should read
    /// as the conversation, not as a log of the calls that produced it.
    #[test]
    fn the_steps_of_a_finished_turn_are_folded_into_one_line() {
        let a = a_turn_with_steps();
        let screen = rendered(&a, 100, 24);
        assert!(!screen.contains("project_switch"), "{screen}");
        assert!(!screen.contains("switched to zuma"), "{screen}");
        assert!(!screen.contains("handed to the orchestrator"), "{screen}");
        assert!(screen.contains("Zuma is on its third"), "the answer:\n{screen}");
        assert!(
            screen.contains("3 steps · Ctrl-O"),
            "and how to read them back:\n{screen}"
        );
    }

    /// The steps are still on screen while they are happening, which is most of
    /// why anyone sits in front of a harness at all.
    #[test]
    fn the_steps_of_the_turn_in_flight_stay_on_screen() {
        let mut a = a_turn_with_steps();
        a.transcript.pop();
        a.busy = true;
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("project_switch"), "{screen}");
        assert!(!screen.contains("Ctrl-O"), "nothing folded yet:\n{screen}");
    }

    #[test]
    fn ctrl_o_brings_the_folded_steps_back() {
        let mut a = a_turn_with_steps();
        a.expand_details = true;
        let screen = rendered(&a, 100, 24);
        assert!(screen.contains("project_switch"), "{screen}");
        assert!(screen.contains("handed to the orchestrator"), "{screen}");
        assert!(!screen.contains("steps · Ctrl-O"), "no marker:\n{screen}");
    }

    /// Details off is the stronger setting: not the steps, and not a line
    /// saying how many there were either.
    #[test]
    fn with_details_off_not_even_the_fold_marker_is_drawn() {
        let mut a = a_turn_with_steps();
        a.show_details = false;
        let screen = rendered(&a, 100, 24);
        assert!(!screen.contains("project_switch"), "{screen}");
        assert!(!screen.contains("Ctrl-O"), "{screen}");
        assert!(screen.contains("Zuma is on its third"), "{screen}");
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
        // Sized to the palette, as in `the_completion_hints_line_up_in_a_column`.
        // The last command in the list is the sentinel for "the far end is
        // reachable", so the screen has to be tall enough to hold the list it
        // is the end of. Taken from `HELP` rather than named, so retiring a
        // command moves the sentinel instead of breaking the test.
        let screen = rendered(&a, 100, popup_height());
        let last = crate::tui::command::HELP.last().unwrap().1;
        assert!(screen.contains("this list"), "/help:\n{screen}");
        assert!(
            screen.contains(last),
            "{last:?}, the far end of the list:\n{screen}"
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
        let top = screen.lines().find(|l| l.contains("┌ chat ")).unwrap();
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
            manager_conversation_id: None,
        }
    }

    fn with_catalog(names: &[&str], current: Option<(&str, How)>) -> App {
        let mut a = app();
        a.panel = true;
        a.projects = names.iter().map(|n| catalogued(n)).collect();
        a.current_project = current.map(|(n, how)| crate::tui::app::Current {
            // `catalogued` uses the name as the id, so a fixture that names a
            // project names its row.
            id: n.to_string(),
            name: n.to_string(),
            how,
        });
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

    /// A collapsed catalog must not claim the catalog is empty.
    ///
    /// Collapsed, the box shows the *current* project — and with none set it
    /// said "nothing set — /project add" beside a fleet drawing four
    /// repositories, with a remedy telling you to add one you already have.
    /// Two presses of `Ctrl-P` were enough to get there.
    #[test]
    fn a_collapsed_catalog_with_nothing_current_still_admits_it_has_projects() {
        let mut a = with_catalog(&["alpha", "beta"], None);
        a.projects_open = false;

        let screen = rendered(&a, 140, 30);
        assert!(
            screen.contains("2 catalogued"),
            "it says how many it has:\n{screen}"
        );
        assert!(
            !screen.contains("nothing set"),
            "and does not claim to have none:\n{screen}"
        );
        // And it fits. Written without a `cut`, this line lost its last three
        // characters at every width — `… catalogued · Ctr` — which dropped the
        // keystroke the sentence exists to name, on the one screen that tells
        // you how to reach your projects.
        assert!(
            screen.contains("Ctrl-P"),
            "the keystroke it names has to survive the width:\n{screen}"
        );

        // A genuinely empty catalog keeps the sentence that fits it, remedy and
        // all — that case is the one `/project add` is the answer to.
        let mut empty = with_catalog(&[], None);
        empty.projects_open = false;
        let screen = rendered(&empty, 140, 30);
        assert!(screen.contains("nothing set"), "{screen}");
        assert!(screen.contains(CATALOG_REMEDY), "{screen}");
    }

    /// A catalogued checkout that is not there any more says so.
    ///
    /// The panel is where a project is chosen, and choosing one whose directory
    /// has been deleted or renamed routes an instruction into a directory that
    /// cannot be entered. It does not fail politely: the supervisor reports the
    /// operating system refusing the working directory as
    /// `could not start ".../claude": No such file or directory`, which reads
    /// as the harness being missing from the machine. `jod project ls` has
    /// explained this for a while; this screen listed it like any other row.
    #[test]
    fn a_project_whose_directory_is_gone_is_marked_on_the_panel() {
        let mut a = with_catalog(&["alpha", "gone"], None);
        a.projects_open = true;
        a.broken_projects = ["gone".to_string()].into_iter().collect();

        let screen = rendered(&a, 140, 30);
        assert!(
            screen.contains("gone · missing"),
            "the row says the checkout is not there:\n{screen}"
        );
        assert!(
            screen.lines().any(|l| l.contains("alpha") && !l.contains("missing")),
            "and a healthy project is left exactly as it was:\n{screen}"
        );
    }

    /// Two checkouts whose directories share a name are two rows, and the
    /// panel has to say which is which.
    ///
    /// Found by running it: two repositories both called `web` drew as two
    /// identical rows, on the one box whose whole job is saying which
    /// repository the next sentence lands in. Pressing `⏎` on either entered "a
    /// manager" with no way to know whose.
    #[test]
    fn two_projects_with_one_name_are_told_apart_by_where_they_are() {
        let mut a = with_catalog(&["web", "gamma"], None);
        a.projects_open = true;
        let mut other = catalogued("web");
        other.id = "web-two".into();
        other.path = "/home/reljod/work/web".into();
        a.projects.push(other);

        let screen = rendered(&a, 140, 30);
        assert!(
            screen.contains("web in repo"),
            "the first says which directory it is in:\n{screen}"
        );
        assert!(
            screen.contains("web in work"),
            "and so does the second:\n{screen}"
        );
        // A name nothing else shares is left exactly as it was — the qualifier
        // is for the clash, not decoration on every row.
        assert!(
            screen.lines().any(|l| l.contains("gamma") && !l.contains(" in ")),
            "an unshared name is not qualified:\n{screen}"
        );
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

    /// Two checkouts can have the same directory name — the catalog knows it,
    /// and `/project untrack` already refuses to guess between them. The panel
    /// used to mark the current project by comparing names, so both rows got
    /// the `▸` and the box said two different repositories were the one the
    /// next sentence would land in. That is the one question it exists to
    /// answer, so it must name a row rather than a word.
    #[test]
    fn two_projects_of_the_same_name_do_not_both_read_as_current() {
        let mut a = app();
        a.panel = true;
        let mut work = catalogued("api");
        work.id = "work-api".into();
        work.path = std::path::PathBuf::from("/home/reljod/work/api");
        let mut side = catalogued("api");
        side.id = "side-api".into();
        side.path = std::path::PathBuf::from("/home/reljod/side/api");
        a.projects = vec![work, side];
        a.current_project = Some(crate::tui::app::Current {
            id: "side-api".into(),
            name: "api".into(),
            how: How::Inferred,
        });

        let screen = rendered(&a, 140, 30);
        let marked = screen.lines().filter(|l| l.contains('▸') && l.contains("api")).count();
        assert_eq!(marked, 1, "both rows read as the current project:\n{screen}");
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

    /// What the last frame drew, for the tests that need the catalog's own
    /// rects rather than the characters in them.
    fn panel_hits(a: &App, w: u16, h: u16) -> PanelHits {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut out = Painted::default();
        terminal.draw(|f| out = draw(f, a)).unwrap();
        out.panel
    }

    /// A catalog longer than its box used to be rendered whole into a paragraph
    /// the box then clipped: the rows past the bottom were on no screen, named
    /// by nothing, and reachable by no key. With a cursor in the box that is
    /// worse than invisible — the cursor could sit on a row nothing drew.
    ///
    /// So the box windows around the cursor, and says how many it is leaving
    /// out. A list that silently ends is a list you believe you have read.
    #[test]
    fn a_catalog_taller_than_its_box_follows_the_cursor_and_counts_the_rest() {
        let mut a = app();
        a.panel = true;
        a.projects = (0..20)
            .map(|i| catalogued(&format!("project-{i:02}")))
            .collect();
        a.focus_catalog();

        // The last row, which is the one the old renderer could never show.
        a.project_selected = Some("project-19".into());
        let screen = rendered(&a, 140, 30);
        assert!(
            screen.contains("project-19"),
            "the cursor is on a row the box did not draw:\n{screen}"
        );
        assert!(
            !screen.contains("project-00"),
            "the window did not move at all:\n{screen}"
        );
        assert!(
            screen.contains("more"),
            "the box dropped rows without saying so:\n{screen}"
        );
    }

    /// The cursor and the current project are two different facts, and a row
    /// that is both must still say both — otherwise pointing at another project
    /// looks like having switched to it.
    #[test]
    fn the_cursor_is_drawn_apart_from_the_current_project() {
        let mut a = with_catalog(&["tetris", "zephyr"], Some(("tetris", How::Inferred)));
        a.focus_catalog();
        a.project_selected = Some("zephyr".into());
        let screen = rendered(&a, 140, 30);
        let row = |name: &str| {
            screen
                .lines()
                .find(|l| l.contains(name))
                .unwrap_or_default()
                .to_string()
        };
        assert!(
            row("zephyr").contains('›'),
            "the cursor is not on the row it is on: {}",
            row("zephyr")
        );
        assert!(
            row("tetris").contains('▸'),
            "the current project lost its mark to the cursor: {}",
            row("tetris")
        );
    }

    /// A click carries a column and a row and nothing else, so the frame that
    /// drew the catalog is what says which project was under it.
    #[test]
    fn every_drawn_project_row_is_one_a_click_can_name() {
        let mut a = with_catalog(&["tetris", "zephyr"], Some(("tetris", How::Inferred)));
        a.focus_catalog();
        let hits = panel_hits(&a, 140, 30);
        let area = hits.catalog.expect("the catalog was drawn");
        assert_eq!(hits.projects.len(), 2, "{:?}", hits.projects);
        for hit in &hits.projects {
            assert_eq!(
                hits.project_at(area.x + 4, hit.row),
                Some(hit.id.as_str()),
                "the row it drew is not the row it resolves"
            );
        }
        // The border rows belong to no project, or a click on the box's edge
        // would move the cursor to whatever happened to be nearest.
        assert_eq!(hits.project_at(area.x + 4, area.y), None);
    }

    /// A catalog that is not on screen must resolve no clicks at all, or a tap
    /// on the chat would land on a project drawn by an earlier frame.
    #[test]
    fn a_collapsed_catalog_resolves_no_clicks() {
        let mut a = with_catalog(&["tetris"], None);
        a.projects_open = false;
        assert!(panel_hits(&a, 140, 30).catalog.is_none());
    }

    /// A long catalog stops where the two fixed boxes under it begin. It is cut
    /// rather than allowed to push the settings and the context off the bottom,
    /// and it now gets every row above them — the third-of-the-panel cap was
    /// there to leave space for the runs list that used to sit between them.
    #[test]
    fn a_long_catalog_stops_above_the_boxes_below_it() {
        let mut a = app();
        a.panel = true;
        a.projects = (0..40)
            .map(|i| catalogued(&format!("project-{i}")))
            .collect();
        let room = 30 - (SESSION_HEIGHT + CONTEXT_HEIGHT);
        let height = projects_height(&a, 30);
        assert_eq!(
            height, room,
            "a 40-project catalog should fill the {room} rows above the fixed boxes"
        );
    }

    /// Below a certain height there is no honest room for a third box, and
    /// squeezing one in costs the boxes below it their last row. That applies to
    /// the *collapsed* catalog, which is a one-line reminder nobody asked for.
    #[test]
    fn a_short_panel_drops_a_collapsed_catalog_rather_than_squeezing_it() {
        let mut a = with_catalog(&["tetris"], None);
        a.projects_open = false;
        assert_eq!(projects_height(&a, 10), 0);
    }

    /// An *opened* catalog is never nothing, however short the panel is. This
    /// used to return zero below twelve rows whatever the state was, so on a
    /// short terminal `Ctrl-P` flipped a flag nothing rendered: the key did
    /// nothing, said nothing, and gave no reason — the same failure the key had
    /// already been fixed for once, one state further out.
    #[test]
    fn an_opened_catalog_keeps_a_box_on_a_short_panel() {
        let a = with_catalog(&["tetris"], None);
        assert!(a.projects_open);
        assert!(
            projects_height(&a, 10) >= 3,
            "an opened catalog rendered as nothing on a ten-row panel"
        );
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
        assert!(
            !rendered(&a, 140, 24).contains("session"),
            "shut by default"
        );

        a.panel = true;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("session"), "{screen}");
        assert!(screen.contains("projects"), "{screen}");
        assert!(screen.contains("context"), "{screen}");
        assert!(
            screen.contains("Esc or Shift-Tab closes"),
            "the way out:\n{screen}"
        );
    }

    /// The runs are the fleet's to show, not the panel's.
    ///
    /// Thirty columns held an id, an age and a name cut to nothing, which is
    /// three facts about a run and none of the ones anybody acts on. Two
    /// screens listing the same runs also meant two places to look and two to
    /// keep right, so the panel gave the question up.
    #[test]
    fn the_panel_does_not_list_the_runs() {
        let mut a = app();
        a.panel = true;
        a.agents = vec![agent_line("aaa11111", "port the parser", "running")];
        let screen = rendered(&a, 140, 24);
        assert!(
            !screen.contains("port the parser"),
            "the runs list is the fleet's:\n{screen}"
        );
        assert!(!screen.contains("aaa11111"), "nor its id:\n{screen}");
        assert!(
            !screen.contains("no runs yet"),
            "and no empty state for a list that is gone:\n{screen}"
        );
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
    /// not, so it happens before the wall — and not a moment earlier, or it is
    /// spending a model call on nothing.
    #[test]
    fn the_context_box_says_it_will_compact_past_the_threshold_and_not_before() {
        use super::super::app::{COMPACT_AT, CONTEXT_WINDOW};
        let mut a = app();
        a.panel = true;

        a.context_tokens = (CONTEXT_WINDOW as f64 * (COMPACT_AT - 0.1)) as u64;
        let quiet = rendered(&a, 140, 24);
        assert!(!quiet.contains("compact"), "too early to say so:\n{quiet}");

        a.context_tokens = (CONTEXT_WINDOW as f64 * COMPACT_AT) as u64;
        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("⚠ compacting when the turn ends"), "{screen}");
    }

    /// The line reports what is about to happen, so when nothing is about to
    /// happen it must stop saying so. Automatic compaction gives up after a
    /// failure — see `App::auto_compact` — and from then on the honest reading
    /// is the old one: it is yours to do.
    #[test]
    fn the_context_box_asks_for_the_command_once_it_has_stopped_compacting_on_its_own() {
        use super::super::app::CONTEXT_WINDOW;
        let mut a = app();
        a.panel = true;
        a.context_tokens = CONTEXT_WINDOW;
        a.auto_compact = false;

        let screen = rendered(&a, 140, 24);
        assert!(screen.contains("/compact"), "{screen}");
        assert!(
            !screen.contains("compacting when the turn ends"),
            "it is not going to:\n{screen}"
        );
    }

    /// The panel is shut most of the time, and a model call nobody can see
    /// coming is worse than advice nobody can see — so it rides the row that is
    /// always on.
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

    /// The status bar's badge has to say it is guessing, because none of the
    /// panel's hedging travels with it.
    ///
    /// `CONTEXT_WINDOW` is 200,000 for every model, so on a model with a
    /// million-token window this badge lights up at about 15% of the real
    /// capacity — and what follows it is now a model call the console makes on
    /// its own rather than a suggestion the user can ignore. That makes the
    /// hedge matter more, not less: a bare `⚠ compacting` claims the
    /// conversation is full when five sixths of the room is left. Calling it an
    /// estimate is the condition `CONTEXT_WINDOW`'s own doc comment sets for
    /// keeping one fixed number.
    #[test]
    fn the_status_bar_calls_the_compaction_advice_an_estimate() {
        use super::super::app::CONTEXT_WINDOW;
        let mut a = app();
        a.context_tokens = CONTEXT_WINDOW;
        let screen = rendered(&a, 140, 24);
        let status = screen.lines().last().unwrap().to_string();
        assert!(status.contains("compact"), "{screen}");
        assert!(
            status.contains("estimate"),
            "a bare `⚠ compact` reads as a fact about this chat:\n{screen}"
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
        let screen = rendered(&a, 60, 20);
        assert!(
            screen.contains("session"),
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

    /// A short terminal must not drop menu entries in silence.
    ///
    /// At 100×10 the workspace menu stopped after `a activity` and lost the
    /// rows below it — including the only routes to jobs, search and the keymap
    /// — with nothing on screen saying they were there. The `?` overlay in the
    /// same situation has always said "N more — widen the window", so the two
    /// overlays disagreed about whether truncation is worth mentioning.
    #[test]
    fn the_workspace_menu_says_what_a_short_terminal_is_hiding() {
        let mut a = app();
        a.overlay = Overlay::WhichKey;

        let short = rendered(&a, 100, 10);
        assert!(
            short.contains("more — widen the window"),
            "ten rows cannot hold the menu, and it has to say so:\n{short}"
        );
        // The count is of entries, so it has to be a real number rather than
        // the word "some".
        assert!(
            short.contains("· 9 more"),
            "and say how many:\n{short}"
        );

        // Given room, it goes back to the ordinary footer and claims nothing.
        let tall = rendered(&a, 100, 40);
        assert!(
            !tall.contains("more — widen the window"),
            "nothing is hidden at forty rows:\n{tall}"
        );
        assert!(tall.contains("any other key is ignored"), "{tall}");
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
    /// included: `s stop · r resume · f fork` are meaningless here and
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
        assert!(rendered(&a, 100, 20).contains("n adds a task"));
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
        assert!(rendered(&a, 100, 20).contains("n adds one"));
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

    /// `cut` is asked for a number of terminal columns, and a column is not a
    /// character.
    ///
    /// Two different mistakes hide behind counting characters. Wide text —
    /// Japanese, Chinese, most emoji — paints two columns per character, so
    /// counting characters let twice as much text through as the caller asked
    /// for and the extra ran off the end of whatever box it was in. Combining
    /// accents are the other way round: two characters paint one column, so
    /// the old count trimmed text that would have fitted. Both are fixed by
    /// asking how wide the text is instead of how long it is.
    #[test]
    fn cutting_counts_columns_not_characters() {
        for (text, width) in [
            ("とても長い日本語の要約がここにあります", 20),
            ("超long mixed タイトル here", 12),
            ("🚀🚀🚀🚀🚀🚀🚀🚀", 7),
            ("a very long english summary", 10),
        ] {
            let out = cut(text, width);
            assert!(
                columns(&out) <= width,
                "`{out}` paints {} columns and only {width} were free",
                columns(&out),
            );
        }

        // Four accented letters, written as a letter and a separate accent
        // each: eight characters, four columns. All four fit in four columns
        // and none of them should be dropped.
        let accented = "e\u{301}e\u{301}e\u{301}e\u{301}";
        assert_eq!(accented.chars().count(), 8);
        assert_eq!(columns(accented), 4);
        assert_eq!(
            cut(accented, 4),
            accented,
            "an accent paints no column of its own, so nothing had to go",
        );
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
        // Both lists, exactly as `refresh_rail` fills them: the rail's own view
        // and the unfiltered open set the badges count. A fixture that filled
        // only the first would make every badge test pass for the wrong reason
        // — and disagree with the running program the moment a filter was on.
        a.open_cards = store
            .cards(&a.rail.open_query(a.conversation.clone()))
            .expect("the open cards");
        a.reconcile_rail();
        a
    }

    /// The claim `app::tests::a_blocked_agent_says_so_on_the_line_that_is_always_visible`
    /// makes but cannot check: that it reaches a *frame*.
    ///
    /// With the rail put away, which is its resting state. The whole point of
    /// the always-on row is that the news does not depend on the panel being
    /// open, and the panel is shut by default.
    #[test]
    fn a_blocked_run_is_the_loudest_thing_on_the_always_on_row() {
        let (store, conversation) = wordy_store();
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;

        let frame = rendered(&a, 120, 24);
        assert!(frame.contains("1 waiting on you"), "{frame}");
        assert!(
            frame.contains("Ctrl-N"),
            "a reader who has just learned they are the blocker must not then \
             have to go and find out how to answer: {frame}"
        );
        assert!(
            frame.contains('▌'),
            "colour is never the only channel here: {frame}"
        );
    }

    /// The complaint this change answers: the only way to learn a card had been
    /// raised was to open the rail and find it, so the cards you found were the
    /// ones you already suspected were there.
    ///
    /// With the rail shut, which is its resting state — a card that blocks
    /// nothing never triggers the auto-open, so this is the state it is
    /// actually read in.
    #[test]
    fn a_question_that_blocks_nothing_reaches_the_screen_with_the_rail_shut() {
        let (store, conversation) = crowded_store(3);
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;

        let frame = rendered(&a, 120, 24);
        assert!(
            frame.contains("3 cards"),
            "three questions were asked and nothing on screen said so: {frame}"
        );
        assert!(
            frame.contains("Ctrl-N"),
            "a reader who has just learned there is something to read must not \
             then have to go and find out how: {frame}"
        );
        assert!(
            frame.contains('◆'),
            "colour is never the only channel here: {frame}"
        );
    }

    /// The band beside the lion carries it too, not only the status row. The
    /// row is one line at the bottom of a screen whose middle is scrolling, and
    /// this is the line that stays put.
    #[test]
    fn the_header_band_says_how_many_cards_are_waiting_to_be_read() {
        let (store, conversation) = crowded_store(2);
        let a = rail_app(&store, &conversation, Default::default());

        let line: String = header_doing(&a, 60)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(line.contains("◆ 2 cards"), "{line}");
    }

    /// Both badges at once, and the numbers do not overlap: one card has
    /// stopped a run and two are merely waiting to be read, so the line says
    /// one and two rather than one and three.
    #[test]
    fn the_two_card_badges_never_count_the_same_card_twice() {
        let (store, conversation) = wordy_store();
        for at in 0..2 {
            store
                .raise_card(NewCard {
                    conversation_id: conversation.clone(),
                    kind: Some(CardKind::Question),
                    blocking: false,
                    title: format!("question number {at}"),
                    ..Default::default()
                })
                .expect("a card");
        }
        let a = rail_app(&store, &conversation, Default::default());

        let line: String = header_doing(&a, 80)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(line.contains("1 waiting on you"), "{line}");
        assert!(line.contains("◆ 2 cards"), "{line}");
    }

    /// Both badges are answered by the same chord, so the row prints it once.
    /// `▌ 1 waiting on you · Ctrl-N · ◆ 3 cards · Ctrl-N` spends nine of the
    /// scarcest columns on the screen saying the same thing twice.
    #[test]
    fn the_status_row_prints_the_rails_key_once_however_many_badges_it_has() {
        let (store, conversation) = wordy_store();
        store
            .raise_card(NewCard {
                conversation_id: conversation.clone(),
                kind: Some(CardKind::Question),
                blocking: false,
                title: "which linter?".into(),
                ..Default::default()
            })
            .expect("a card");
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;

        let both = rendered(&a, 120, 24);
        assert!(both.contains("waiting on you"), "{both}");
        assert!(both.contains("1 card"), "{both}");
        assert_eq!(both.matches("Ctrl-N").count(), 1, "{both}");
    }

    /// And the badge left standing is the one still carrying it. With nothing
    /// blocked there is no red badge to hold the key, so the cards badge takes
    /// it — otherwise the ordinary case, which is the whole reason this exists,
    /// is the one that never says how to answer.
    #[test]
    fn the_cards_badge_carries_the_key_when_there_is_no_blocker_to_hold_it() {
        let (store, conversation) = crowded_store(2);
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;

        let frame = rendered(&a, 120, 24);
        assert!(!frame.contains("waiting on you"), "nothing is blocked");
        assert!(frame.contains("◆ 2 cards · Ctrl-N"), "{frame}");
    }

    /// The badges count the open cards, not the rail's view of them. Searching
    /// the rail is not consent to be told nothing: before this, a filter that
    /// matched no cards emptied the list the header band and the status row
    /// were counting, so the one moment the reader was looking *through* the
    /// rail was the moment the rest of the screen stopped mentioning it.
    #[test]
    fn a_rail_filter_cannot_hide_a_blocker_from_the_rest_of_the_screen() {
        let (store, conversation) = wordy_store();
        let a = rail_app(
            &store,
            &conversation,
            RailState {
                filter: Some("nothingmatchesthis".into()),
                ..Default::default()
            },
        );
        assert!(a.cards.is_empty(), "the rail's own list is filtered empty");

        let frame = rendered(&a, 120, 24);
        assert!(frame.contains("1 waiting on you"), "{frame}");
    }

    /// The badges used to be one string, dropped whole the moment the row got
    /// tight — so on the terminal where a blocker was hardest to notice, the
    /// badge saying there was one is the thing that went.
    #[test]
    fn a_tight_status_row_drops_the_lesser_badges_before_the_blocker() {
        let (store, conversation) = wordy_store();
        let mut a = rail_app(&store, &conversation, Default::default());
        a.rail.shown = false;
        // A second badge to compete with, so there is something to drop.
        a.activity = vec![ActivityItem {
            id: "e1".into(),
            at_ms: 0,
            source: Source::Hook,
            text: "pr-opened fired".into(),
            unread: true,
            needs_you: false,
            jump_to: None,
        }];

        let wide = rendered(&a, 160, 24);
        assert!(wide.contains("unread"), "both fit at 160 columns: {wide}");
        assert!(wide.contains("waiting on you"), "{wide}");

        let tight = rendered(&a, 96, 24);
        assert!(
            tight.contains("waiting on you"),
            "the blocker is what survives: {tight}"
        );
        assert!(
            tight.lines().all(|l| l.chars().count() <= 96),
            "and nothing runs off the edge:\n{tight}"
        );
    }

    /// The transcript is laid out in blocks now, and an alert must not be one
    /// of the notices. Packed into `Chunk::Note` it would sit flush against the
    /// notice above it with no line between — the same failure the marker was
    /// added to fix, one layer along.
    #[test]
    fn an_alert_is_parted_from_the_notices_around_it() {
        let mut a = app();
        a.push(Entry::Notice("compacted — 17239 chars".into()));
        a.push(Entry::Alert("a run is blocked — Ctrl-N opens the rail".into()));
        a.push(Entry::Notice("context is 100% full".into()));

        let frame = rendered(&a, 120, 24);
        let lines: Vec<&str> = frame.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("a run is blocked"))
            .expect("the alert is on screen");
        // Empty of *text*, not of characters: the row still carries the chat
        // box's own borders on either side of it.
        let blank = |line: &str| !line.chars().any(|c| c.is_alphanumeric());
        assert!(blank(lines[at - 1]), "a line of air above it:\n{frame}");
        assert!(blank(lines[at + 1]), "and below it:\n{frame}");
    }

    /// A blocked run used to be pushed as a `Notice`, which put it in the same
    /// amber bullet as `compacted — 17239 chars…` directly above it.
    #[test]
    fn an_alert_does_not_come_out_looking_like_an_ordinary_notice() {
        let mut a = app();
        a.push(Entry::Notice("compacted — 17239 chars".into()));
        a.push(Entry::Alert("a run is blocked — Ctrl-N opens the rail".into()));

        let frame = rendered(&a, 120, 24);
        let notice = frame
            .lines()
            .find(|l| l.contains("compacted"))
            .expect("the notice is on screen");
        let alert = frame
            .lines()
            .find(|l| l.contains("a run is blocked"))
            .expect("and so is the alert");
        assert!(notice.contains('•'), "the notice keeps its bullet: {notice}");
        assert!(
            alert.contains('▌') && !alert.contains('•'),
            "and the alert is marked apart from it: {alert}"
        );
    }

    /// **E2's check:** a rendered frame showing three cards, one marked
    /// `blocked`, the answered one hidden until toggled.
    ///
    /// E2 asked for the blocked card to be *bordered*. It is marked instead:
    /// a border round a one-row card is three rows of box for one row of card,
    /// and the boxes are what made five of six cards the most the rail could
    /// draw. What E2 was actually protecting — that blocking is never carried by
    /// colour alone — is unchanged and asserted below, on the `!` in the glyph
    /// column and on the word itself in the header.
    #[test]
    fn the_rail_shows_three_cards_one_marked_blocked_and_the_answered_one_only_on_toggle() {
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
        // Three cards means three rows, each carrying one card and no other.
        for title in [
            "chat DB: chose SQLite",
            "which port for the API?",
            "GITHUB_TOKEN is missing",
        ] {
            assert_eq!(
                frame.lines().filter(|line| line.contains(title)).count(),
                1,
                "{title} gets a row of its own: {frame}"
            );
        }
        // And blocking is said in something other than colour, which is the
        // rule the border used to satisfy.
        let blocked = frame
            .lines()
            .find(|line| line.contains("which port for the API?"))
            .expect("the blocking card is drawn")
            .to_string();
        assert!(
            blocked.contains('!'),
            "the blocking card carries a mark as well as a colour: {blocked}"
        );

        // `t` cycles the stack, and the answered card comes back — saying both
        // what the human did and whether the agent has heard.
        let mut rail = RailState::default();
        rail.cycle_stack();
        let answered = rail_app(&store, &conversation, rail);
        let frame = rendered(&answered, 150, 40);
        assert!(frame.contains("retry the flaky test?"), "{frame}");
        // D2: an answer is asynchronous and the rail must not pretend
        // otherwise, so **both** facts are on screen — what the human did and
        // whether the agent has heard. In a thirty-four column column they are
        // said between the header and the row rather than joined on the row,
        // because `answered, queued` there leaves four columns for the card and
        // a state note beside an unreadable question is not the honest trade.
        // Joined on one row wherever there is room for it — see the phone panel
        // below, and the expanded card, which never abbreviates.
        assert!(
            frame.contains("answered"),
            "what the human did: {frame}"
        );
        assert!(
            frame.contains("queued"),
            "and whether the agent has heard: {frame}"
        );

        // The phone's bottom panel is wide enough for the sentence, so it gets
        // the sentence.
        let wide = rendered(&answered, 78, 30);
        assert!(
            wide.contains("answered, queued"),
            "a rail with room for both words joins them: {wide}"
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

    /// A narrow terminal is short of columns, not rows, so the rail lies along
    /// the bottom rather than standing beside the chat. What it must not do is
    /// what the one-line summary used to do here: count the blockers without
    /// ever showing one.
    #[test]
    fn a_narrow_terminal_gets_the_rail_along_the_bottom_rather_than_a_squeezed_column() {
        let (store, conversation) = rail_store();
        let a = rail_app(&store, &conversation, Default::default());

        let wide = rendered(&a, 150, 40);
        assert!(wide.contains("chat DB: chose SQLite"), "{wide}");

        // The blocking card, not merely some card: `Sort::Pressing` puts it at
        // the top of the stack, and it is the one the panel exists to show.
        let narrow = rendered(&a, 78, 30);
        assert!(
            narrow.contains("which port for the API?"),
            "the blocking question has to be readable, which is the whole point: {narrow}"
        );
        assert!(
            narrow.contains(rail::BLOCKED),
            "and it still says that it stopped a run: {narrow}"
        );
    }

    /// The panel goes *below* the chat, over the keybar that names the keys for
    /// answering. Above it would push the conversation down the screen every
    /// time an agent raised anything.
    #[test]
    fn the_narrow_rail_sits_under_the_chat_and_not_over_it() {
        let (store, conversation) = rail_store();
        let a = rail_app(&store, &conversation, Default::default());

        let narrow = rendered(&a, 78, 30);
        let lines: Vec<&str> = narrow.lines().collect();
        let card = lines
            .iter()
            .position(|l| l.contains("which port for the API?"))
            .expect("the card is drawn");
        let header = lines
            .iter()
            .position(|l| l.contains(" rail"))
            .expect("the rail header is drawn");
        let composer = lines
            .iter()
            .position(|l| l.contains(CARET))
            .expect("the composer is drawn");
        assert!(
            header < card,
            "the header tops the panel: header {header}, card {card}\n{narrow}"
        );
        assert!(
            composer < header,
            "and the whole panel sits under the chat: composer {composer}, panel {header}\n{narrow}"
        );
    }

    /// Expanding is what you press once the panel has told you a card is there,
    /// so the panel has to have the rows to answer that press — the body of the
    /// question and the sentence saying a run is waiting on it.
    #[test]
    fn the_narrow_rail_can_expand_the_blocking_card_in_place() {
        let (store, conversation) = rail_store();
        let mut rail = RailState::default();
        rail.expanded = true;
        let a = rail_app(&store, &conversation, rail);

        let narrow = rendered(&a, 78, 30);
        assert!(
            narrow.contains("which port for the API?"),
            "the blocking card is the one Pressing selected: {narrow}"
        );
        assert!(
            narrow.contains("The alternatives were weighed"),
            "and its body is readable, not just its title: {narrow}"
        );
        assert!(
            narrow.contains("the run is waiting on this"),
            "and it says what being blocked costs: {narrow}"
        );
    }



    /// A rail holding more cards than the stack draws at once.
    fn crowded_store(how_many: usize) -> (RealStore, String) {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        for at in 0..how_many {
            store
                .raise_card(NewCard {
                    conversation_id: conversation.clone(),
                    run_id: Some("3f2ab1c0".into()),
                    kind: Some(CardKind::Question),
                    blocking: false,
                    title: format!("question number {at}"),
                    body: "The alternatives were weighed and one was picked.".into(),
                    ..Default::default()
                })
                .expect("a card");
        }
        (store, conversation)
    }

    /// One blocking card with more to say than a phone-sized panel can hold.
    ///
    /// Which is the ordinary case, not a contrived one: an agent that has
    /// stopped explains why it stopped, and the explanation is a paragraph.
    fn wordy_store() -> (RealStore, String) {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .raise_card(NewCard {
                conversation_id: conversation.clone(),
                run_id: Some("3f2ab1c0".into()),
                kind: Some(CardKind::Question),
                importance: Some(Importance::High),
                blocking: true,
                title: "which port for the API?".into(),
                body: "The deploy script wants a port and the box already has \
                       something on 8080, which is the default the compose file \
                       carries. Nothing else in the repository names a port, so \
                       this is a choice rather than a lookup, and the answer \
                       goes into the unit file as well as the compose file."
                    .into(),
                options: vec!["8080".into(), "8081".into(), "3000".into()],
                ..Default::default()
            })
            .expect("a card");
        (store, conversation)
    }

    /// The frame, with the geometry a pointer is resolved against.
    fn painted(app: &App, w: u16, h: u16) -> Painted {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut out = Painted::default();
        terminal.draw(|f| out = draw(f, app)).unwrap();
        out
    }

    /// The rail as it now reads: a heading a project, a row a card, and a
    /// column down the right saying what the digit beside each one would do.
    #[test]
    fn the_stack_groups_by_project_and_prints_what_the_digit_would_answer() {
        let store = RealStore::in_memory().expect("an in-memory store");
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        let approval = |command: &str| NewCard {
            conversation_id: conversation.clone(),
            run_id: Some("3f2ab1c0".into()),
            blocking: true,
            importance: Some(Importance::High),
            title: format!("Bash: {command}"),
            options: vec![
                format!("{} `{command}*`", jod_core::approvals::ALWAYS),
                jod_core::approvals::ONCE.to_string(),
                jod_core::approvals::DENY.to_string(),
            ],
            dedupe_key: Some(format!("approval:Bash:{command}*")),
            ..Default::default()
        };
        store.raise_card(approval("cargo test ")).expect("a card");
        store.raise_card(approval("gh pr create ")).expect("a card");
        store
            .raise_card(NewCard {
                conversation_id: conversation.clone(),
                title: "which port for the API?".into(),
                ..Default::default()
            })
            .expect("a card");

        let a = rail_app(&store, &conversation, Default::default());
        let frame = rendered(&a, 150, 40);

        // Every permission card says what the digit sends, so the queue can be
        // cleared without opening any of them.
        for command in ["cargo test", "gh pr create"] {
            let row = frame
                .lines()
                .find(|line| line.contains(command))
                .unwrap_or_else(|| panic!("{command} is drawn: {frame}"))
                .to_string();
            assert!(
                row.contains("allow"),
                "the row says what accepting it does: {row}"
            );
        }
        // And the one that cannot be answered blind says so rather than
        // offering a word that would be a guess.
        // Matched on a prefix: beside a column of `allow` the title has
        // twenty-two columns, and this one is twenty-three.
        let open = frame
            .lines()
            .find(|line| line.contains("which port for the"))
            .expect("the question is drawn")
            .to_string();
        assert!(
            open.contains('—') && !open.contains("allow"),
            "an open question offers nothing to accept: {open}"
        );

        // One heading over the lot of them, naming where they came from.
        let session: String = conversation.chars().take(8).collect();
        assert!(
            frame.contains(&format!("session {session}")),
            "the group is headed by the session that raised them: {frame}"
        );
        // Three cards in five rows — a heading and three rows — where the
        // bordered version spent twelve.
        assert_eq!(
            painted(&a, 150, 40).rail.cards.len(),
            3,
            "all three drawn"
        );
    }

    /// A tall terminal used to fill the whole column with cards. Nine is the
    /// cap — the digits `Ctrl-R` arms — and the cards past it are reached by
    /// moving the cursor.
    #[test]
    fn the_stack_draws_nine_cards_at_most_however_tall_the_terminal_is() {
        let (store, conversation) = crowded_store(12);
        let a = rail_app(&store, &conversation, Default::default());
        assert_eq!(a.cards.len(), 12, "all twelve are in hand");

        let hits = painted(&a, 150, 60).rail;
        assert_eq!(
            hits.cards.len(),
            rail::VISIBLE,
            "a sixty-row terminal still draws nine: {:?}",
            hits.cards
        );

        let frame = rendered(&a, 150, 60);
        assert!(
            frame.contains("9 of 12 shown"),
            "and it says the other three exist: {frame}"
        );
    }

    /// The cap is on the cards, not on the cursor. Every card in the stack has
    /// to be reachable, or nine cards is nine cards and the rest were dropped.
    #[test]
    fn the_window_follows_the_cursor_to_the_bottom_of_a_crowded_stack() {
        let (store, conversation) = crowded_store(12);
        let mut a = rail_app(&store, &conversation, Default::default());
        let ids = a.card_ids();
        a.rail.look_at(*ids.last().expect("twelve cards"));

        let hits = painted(&a, 150, 60).rail;
        assert!(
            hits.cards.iter().any(|hit| Some(hit.id) == a.rail.selected),
            "the card under the cursor is on screen: {:?}",
            hits.cards
        );
        let frame = rendered(&a, 150, 60);
        assert!(
            frame.contains("9 of 12 shown"),
            "and the header says how many of the twelve those are: {frame}"
        );
    }

    /// The gesture the whole thing exists for: a tap on a card, on a terminal
    /// with no keyboard to press `Ctrl-N` on.
    #[test]
    fn every_drawn_card_answers_to_a_pointer_on_any_of_its_rows() {
        let (store, conversation) = rail_store();
        let a = rail_app(&store, &conversation, Default::default());
        let hits = painted(&a, 150, 40).rail;
        assert_eq!(hits.cards.len(), 3, "three cards drawn: {:?}", hits.cards);

        for hit in &hits.cards {
            for row in hit.top..hit.bottom {
                assert_eq!(
                    hits.card_at(hits.area.expect("a rail").x + 2, row),
                    Some(hit.id),
                    "row {row} belongs to card {}",
                    hit.id
                );
            }
        }
        // And nothing outside the rail does. A click in the transcript must not
        // answer whatever card happens to share its row.
        assert!(!hits.holds(0, 0), "the top-left corner is the chat's");

        // The phone, where the rail is a twelve-row panel. It used to have room
        // for two of the three cards, because a card was four rows; at one row
        // each all three fit, which is the point of the change — the panel that
        // used to hide a third of the fleet's questions now hides none of them.
        let narrow = painted(&a, 78, 30).rail;
        assert_eq!(
            narrow.cards.len(),
            3,
            "all three fit the phone panel now: {:?}",
            narrow.cards
        );
        for hit in &narrow.cards {
            assert_eq!(
                narrow.card_at(narrow.area.expect("a rail").x + 2, hit.top),
                Some(hit.id),
                "every card the panel drew is clickable"
            );
        }
    }

    /// An option is answered by tapping the option, which means the row it is
    /// recorded on has to be the row it was printed on.
    #[test]
    fn the_row_an_option_is_offered_on_is_the_row_it_is_printed_on() {
        let (store, conversation) = rail_store();
        let rail = RailState {
            expanded: true,
            ..Default::default()
        };
        let mut a = rail_app(&store, &conversation, rail);
        let decision = a
            .cards
            .iter()
            .find(|c| c.kind == CardKind::Decision)
            .expect("the decision card")
            .id;
        a.rail.look_at(decision);

        let hits = painted(&a, 78, 30).rail;
        assert_eq!(hits.expanded, Some(decision));
        assert_eq!(hits.options.len(), 2, "both options: {:?}", hits.options);

        let lines: Vec<String> = rendered(&a, 78, 30).lines().map(str::to_string).collect();
        for (hit, label) in hits.options.iter().zip(["SQLite", "Postgres"]) {
            assert_eq!(hit.card, decision);
            assert!(
                lines[hit.row as usize].contains(label),
                "row {} was recorded for {label} and reads {:?}",
                hit.row,
                lines[hit.row as usize]
            );
            let column = hits.area.expect("a rail").x + 4;
            assert_eq!(hits.option_at(column, hit.row), Some((decision, hit.at)));
        }
    }

    /// A card taller than its pane was unreadable below the fold on a phone.
    /// Scrolling brings the bottom of it up, and stops there.
    #[test]
    fn a_card_taller_than_its_pane_scrolls_to_its_last_line_and_no_further() {
        let (store, conversation) = wordy_store();
        let rail = RailState {
            expanded: true,
            ..Default::default()
        };
        let mut a = rail_app(&store, &conversation, rail);

        let hits = painted(&a, 78, 30).rail;
        assert!(
            hits.past > 0,
            "the bottom panel is twelve rows and the card is taller"
        );

        // The state sentence is the last line of the card, and the one the
        // reader is scrolling to reach.
        let folded = rendered(&a, 78, 30);
        assert!(
            !folded.contains("the run is waiting on this"),
            "it starts below the fold: {folded}"
        );
        a.rail.scroll_card(hits.past as i16, hits.past);
        let scrolled = rendered(&a, 78, 30);
        assert!(
            scrolled.contains("the run is waiting on this"),
            "and scrolling reaches it: {scrolled}"
        );

        // Past the end is not a place. A wheel that kept going would leave a
        // pane of blank rows where the card was.
        a.rail.scroll_card(50, hits.past);
        assert_eq!(a.rail.scroll, hits.past, "the wheel stops at the last line");
        let stopped = rendered(&a, 78, 30);
        assert!(stopped.contains("the run is waiting on this"), "{stopped}");
    }

    /// An option below the fold is not on screen, so a click on the row it will
    /// eventually occupy must not answer with it — and once it has been
    /// scrolled to, the row it is offered on has to be the row it is drawn on.
    #[test]
    fn an_option_below_the_fold_is_offered_only_once_it_is_on_screen() {
        let (store, conversation) = wordy_store();
        let rail = RailState {
            expanded: true,
            ..Default::default()
        };
        let mut a = rail_app(&store, &conversation, rail);

        let folded = painted(&a, 78, 30).rail;
        assert!(folded.past > 0, "the card is taller than the panel");
        assert!(
            folded.options.is_empty(),
            "the ports are below the fold, so nothing offers them: {:?}",
            folded.options
        );

        a.rail.scroll_card(folded.past as i16, folded.past);
        let scrolled = painted(&a, 78, 30).rail;
        assert_eq!(scrolled.options.len(), 3, "all three ports: {scrolled:?}");
        let lines: Vec<String> = rendered(&a, 78, 30).lines().map(str::to_string).collect();
        for hit in &scrolled.options {
            let label = &a.cards[0].options[hit.at];
            assert!(
                lines[hit.row as usize].contains(label.as_str()),
                "row {} is offered as {label} and reads {:?}",
                hit.row,
                lines[hit.row as usize]
            );
            let column = scrolled.area.expect("a rail").x + 4;
            assert_eq!(
                scrolled.option_at(column, hit.row),
                Some((a.cards[0].id, hit.at))
            );
        }
    }

    /// A click is resolved against the frame it was made on, so the option map
    /// has to travel with the text rather than staying where the card opened.
    #[test]
    fn the_option_map_moves_up_the_pane_with_the_scroll() {
        let (store, conversation) = wordy_store();
        let rail = RailState {
            expanded: true,
            ..Default::default()
        };
        let mut a = rail_app(&store, &conversation, rail);

        let past = painted(&a, 78, 30).rail.past;
        a.rail.scroll_card(past as i16 - 1, past);
        let before = painted(&a, 78, 30).rail;
        assert!(!before.options.is_empty(), "the ports are on screen");

        a.rail.scroll_card(1, past);
        let after = painted(&a, 78, 30).rail;
        for was in &before.options {
            let now = after
                .options
                .iter()
                .find(|hit| hit.at == was.at)
                .expect("the same option is still drawn");
            assert_eq!(now.row, was.row - 1, "one line up, with the text");
        }
    }

    /// The way out of an expanded card, for somebody with no `Esc` key.
    #[test]
    fn the_expanded_cards_title_is_the_way_back_to_the_stack() {
        let (store, conversation) = rail_store();
        let rail = RailState {
            expanded: true,
            ..Default::default()
        };
        let a = rail_app(&store, &conversation, rail);

        let hits = painted(&a, 78, 30).rail;
        let back = hits.back.expect("a way back");
        assert!(hits.on_back(hits.area.expect("a rail").x + 3, back));
        let frame = rendered(&a, 78, 30);
        assert!(
            frame.contains("◂ card #"),
            "and it is visible, because a gesture nobody can see is one nobody \
             uses: {frame}"
        );
    }

    /// A rail with nothing in it must not spend half a phone screen saying so.
    #[test]
    fn a_narrow_rail_with_no_cards_costs_one_line() {
        let (store, conversation) = rail_store();
        let mut a = rail_app(&store, &conversation, Default::default());
        a.cards.clear();
        a.reconcile_rail();

        let narrow = rendered(&a, 78, 30);
        assert!(
            narrow.contains("nothing waiting"),
            "the one-liner is the whole truth here: {narrow}"
        );
    }

    /// Halving a body that is already too short leaves two unreadable halves,
    /// so the single sentence is still the better answer down there.
    #[test]
    fn a_terminal_too_short_to_divide_falls_back_to_the_one_line_summary() {
        let (store, conversation) = rail_store();
        let a = rail_app(&store, &conversation, Default::default());

        let squat = rendered(&a, 78, 9);
        assert!(
            squat.contains("3 cards"),
            "the one-liner still says how many: {squat}"
        );
        assert!(
            squat.contains(&format!("1 {}", rail::BLOCKED)),
            "and that one of them stopped a run: {squat}"
        );
        assert!(
            squat.contains("Ctrl-R"),
            "and which key answers it: {squat}"
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
    /// diff rather than as a one-line summary — once it is asked to.
    ///
    /// Collapsed, the entry is the file-change summary: which file, what was
    /// done to it, and how big the change was. The body is what `Ctrl-O` opens,
    /// the same key that opens every other step, because forty rows of diff per
    /// edit buries the conversation the transcript exists to show.
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

        let collapsed = rendered(&a, 120, 30);
        assert!(
            collapsed.contains("cli/src/tui/ui.rs"),
            "the path is a header:\n{collapsed}"
        );
        assert!(
            collapsed.contains("+1 -1"),
            "and the shape of the change:\n{collapsed}"
        );
        assert!(
            !collapsed.contains("-let width = 80;"),
            "but not the body, which is what Ctrl-O is for:\n{collapsed}"
        );

        a.expand_details = true;
        let frame = rendered(&a, 120, 30);
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

    /// The delegation block used to read "in the background, Ctrl-F to watch"
    /// for the rest of the session however the run ended, so the transcript's
    /// account of a delegation was frozen at the moment it was made.
    #[test]
    fn a_delegation_says_whether_its_agent_is_still_running() {
        let mut a = app();
        a.push(Entry::Delegated {
            id: "run-7".into(),
            prompt: "build the engine".into(),
            dir: "/tmp/racing".into(),
        });

        a.agents = vec![agent_line("run-7", "engine", "running")];
        let live = rendered(&a, 120, 30);
        assert!(live.contains("running in the background"), "live:\n{live}");

        a.agents = vec![agent_line("run-7", "engine", "completed")];
        let done = rendered(&a, 120, 30);
        assert!(done.contains("finished"), "finished:\n{done}");
        assert!(
            !done.contains("running in the background"),
            "and it stops claiming to be running:\n{done}"
        );

        a.agents = vec![agent_line("run-7", "engine", "failed")];
        let failed = rendered(&a, 120, 30);
        assert!(failed.contains("failed"), "failed:\n{failed}");
    }

    /// A file being written says so while it is being written, and changes
    /// tense when it lands. The collapsed summary is the only account of a file
    /// most readers see, so "is this happening or did it happen" has to be
    /// legible without opening anything.
    #[test]
    fn a_file_change_reads_as_in_flight_until_its_call_comes_back() {
        let mut a = app();
        a.begin_turn("run-1", 0);
        a.apply(&jod_core::AgentEvent::ToolCall {
            name: "Write".into(),
            input: Some(serde_json::json!({
                "file_path": "src/car.js",
                "content": "export const car = 1;\n",
            })),
        });
        let during = rendered(&a, 120, 30);
        assert!(during.contains("creating"), "while it runs:\n{during}");

        a.apply(&jod_core::AgentEvent::ToolResult {
            name: "Write".into(),
            summary: None,
            is_error: false,
        });
        let after = rendered(&a, 120, 30);
        assert!(after.contains("created"), "once it lands:\n{after}");
        assert!(
            !after.contains("creating"),
            "and it stops claiming to still be working:\n{after}"
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
        // Settled, so the header carries its finished marker rather than a
        // spinner frame — this test is about how the path is laid out.
        a.apply(&jod_core::AgentEvent::ToolResult {
            name: "Write".into(),
            summary: None,
            is_error: false,
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
        // Mid-turn, which is when a call line is on screen at all: the steps of
        // a turn already over are folded away, and this test is about the shape
        // of the line rather than about when it is drawn.
        a.begin_turn("run-1", 0);
        a.apply(&jod_core::AgentEvent::ToolCall {
            name: "Bash".into(),
            input: Some(serde_json::json!({ "command": "cargo test" })),
        });
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("Bash(cargo test)"), "{frame}");
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

    // ---- the fleet's preview pane ----

    /// A fleet on the flat list with one run whose last message is long enough
    /// to overflow the pane beside it.
    fn a_talkative_run() -> App {
        let mut a = app();
        let mut run = agent_line("run-1", "port the parser", "completed");
        run.last = Some(
            (1..=40)
                .map(|n| format!("line {n} of what it said"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        a.agents = vec![run];
        a.go(Workspace::Fleet);
        a.reconcile();
        a
    }

    /// The frame reports the pane so the key that scrolls it can be clamped to
    /// what is on screen. Working it out again in the key handler would be a
    /// second copy of the arithmetic that decides the pane exists at all.
    #[test]
    fn the_frame_reports_the_preview_it_drew() {
        let a = a_talkative_run();

        let wide = painted(&a, 120, 30).preview;
        assert!(wide.rows > 0, "a preview was drawn: {wide:?}");
        assert!(
            wide.lines > wide.rows,
            "and it holds more than fits, so there is something to scroll: {wide:?}"
        );

        assert_eq!(
            painted(&a, 80, 30).preview,
            Preview::default(),
            "below 90 columns there is no preview, and `⇥` must not stop on one"
        );
    }

    /// Nothing scrolls until the pane is asked to. A screen that opened halfway
    /// down a run's output would look like output that had been lost.
    #[test]
    fn an_unscrolled_preview_starts_at_the_top() {
        let a = a_talkative_run();
        let frame = rendered(&a, 120, 30);
        assert!(frame.contains("line 1 of what it said"), "{frame}");
        assert!(!frame.contains("line 30 of what it said"), "{frame}");
    }

    #[test]
    fn a_scrolled_preview_draws_from_where_the_keyboard_left_it() {
        let mut a = a_talkative_run();
        a.preview_focused = true;
        a.preview_scroll = 12;

        let frame = rendered(&a, 120, 30);

        assert!(
            !frame.contains("line 1 of what it said"),
            "the top has scrolled off: {frame}"
        );
        assert!(frame.contains("line 20 of what it said"), "{frame}");
    }

    /// A scroll kept from a longer run must not leave the pane blank when the
    /// content shrinks under it — the state survives the frame, the content
    /// does not.
    #[test]
    fn a_preview_scrolled_past_its_end_is_pulled_back_to_it() {
        let mut a = a_talkative_run();
        a.preview_focused = true;
        a.preview_scroll = u16::MAX;

        let frame = rendered(&a, 120, 30);

        assert!(
            frame.contains("line 40 of what it said"),
            "the last line is still on screen rather than an empty box: {frame}"
        );
    }

    /// The pane has to say when it has the keyboard, because the two keys it
    /// takes are the two the rows would otherwise answer. The border colour
    /// says it in a terminal; the footer is what a test can read.
    #[test]
    fn the_preview_says_when_it_has_the_keyboard() {
        let mut a = a_talkative_run();
        let rows = rendered(&a, 120, 30);
        assert!(rows.contains("⏎ watch"), "the row verbs, unfocused: {rows}");

        a.preview_focused = true;
        let focused = rendered(&a, 120, 30);
        assert!(focused.contains("↑↓ scroll"), "{focused}");
        assert!(focused.contains("Esc back to the rows"), "{focused}");
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
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 0,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
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

    /// The screen the fleet is meant to open on: `main`, the repositories, and
    /// nothing else — then one keystroke and the repository's roster is a flat
    /// list of its manager and its agents.
    ///
    /// Built off a real store and folded the way a refresh folds it, because
    /// the claim is about the whole path from the query to the pane and a
    /// hand-made forest would only test the drawing.
    #[test]
    fn the_fleet_opens_on_the_projects_and_expands_to_their_agents() {
        use jod_core::works::Origin;

        let store = RealStore::in_memory().expect("an in-memory store");
        // A real directory, because cataloguing a repository checks that one is
        // there — a project is somewhere a session gets started.
        let checkout = std::env::temp_dir().join(format!("jod-fleet-{}", std::process::id()));
        std::fs::create_dir_all(&checkout).expect("a scratch checkout");
        let project = store
            .add_project(jod_core::projects::NewProject::at(&checkout).named("jod"))
            .expect("a catalogued repository");
        let (manager, _) = store
            .manager_conversation(&project.id, HarnessKind::ClaudeCode)
            .expect("a manager");
        store
            .set_conversation_title(&manager, "jod")
            .expect("a manager title");

        for (title, session) in [("the parser", "port the lexer"), ("the deploy", "fix the CI")] {
            let work = store
                .create_work_in(title, Some(&project.id))
                .expect("a work");
            store.set_work_title(&work.id, title).expect("a work title");
            let lead = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .expect("a conversation")
                .id;
            store
                .set_conversation_title(&lead, session)
                .expect("a session title");
            store
                .attach_conversation(&lead, &work.id, None, Origin::Agent)
                .expect("a session under the work");
        }

        let mut a = app();
        let folded = jod_core::tree::condense(
            &store.forest().expect("a forest"),
            &std::collections::HashSet::new(),
        );
        a.forest = folded.nodes;
        a.work_of = folded.works;
        a.run_of = folded.run_of;
        a.tree_runs = folded.runs;
        a.go(Workspace::Fleet);
        a.reconcile();

        let shut = rendered(&a, 150, 30);
        assert!(shut.contains("jod"), "the repository is on screen:\n{shut}");
        for hidden in ["the parser", "the deploy", "port the lexer", "fix the CI"] {
            assert!(
                !shut.contains(hidden),
                "`{hidden}` should be inside the shut project:\n{shut}"
            );
        }

        // `→` on the project, which is what a person presses.
        a.tree.selected = Some(jod_core::tree::NodeId::project(&project.id));
        let (forest, closed) = (a.forest.clone(), a.closed_works.clone());
        a.tree.expand_or_descend(&forest, &closed);
        let open = rendered(&a, 150, 30);

        // The agents are seats on this repository's roster rather than the
        // titles their conversations carry — see `tree::hired_as`.
        for agent in ["manager", "engineer#1", "engineer#2"] {
            assert!(open.contains(agent), "`{agent}` is missing:\n{open}");
        }
        for gone in ["the parser", "the deploy", "port the lexer", "fix the CI"] {
            assert!(
                !open.contains(gone),
                "neither a work nor an instruction is a row here, and `{gone}` is one:\n{open}"
            );
        }
        // The manager and the engineers are siblings, so their rows start at the
        // same column — which is the whole shape being asked for.
        let indent = |needle: &str| -> usize {
            let line = open
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle} is not on screen:\n{open}"));
            line.find(needle).expect("the needle is in the line")
        };
        assert_eq!(indent("manager"), indent("engineer#1"), "{open}");
        assert_eq!(indent("manager"), indent("engineer#2"), "{open}");
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

    /// Every row the cursor can be on must be the row the highlight is on.
    ///
    /// `forest_of` emits a `NodeKind::Main` row for the pinned chat, and
    /// `App::tree_rows` drops the older sentinel id when it does — but the pane
    /// drew the sentinel anyway. That is one more drawn row than there were
    /// ids, so from the second position down the `▸` sat one row above the node
    /// every verb acted on: `⏎` opened the wrong thing and `x` untracked the
    /// project under the row that looked like Jod's own.
    ///
    /// The assertion walks every cursor position rather than checking one,
    /// because the two lists agreeing at position 0 is exactly how the bug hid.
    #[test]
    fn the_highlighted_row_is_the_row_the_cursor_is_on() {
        use jod_core::projects::NewProject;

        let store = RealStore::in_memory().expect("an in-memory store");
        // The pinned chat, so the forest carries its own `Main` row — the case
        // the sentinel was supposed to stand in for and no longer must.
        store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .expect("a main chat");
        // A real directory, because the catalog refuses a path that is not
        // there — and rightly so: a project is somewhere a session is started.
        let dir = std::env::temp_dir().join(format!("jod-tree-cursor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory to catalog");
        let project = store.add_project(NewProject::at(&dir)).expect("a project");
        let work = store
            .create_work_in("port the parser", Some(&project.id))
            .expect("a work");
        store
            .set_work_title(&work.id, "the parser")
            .expect("a work title");

        let mut a = app();
        a.forest = store.forest().expect("a forest");
        a.go(Workspace::Fleet);
        a.reconcile();
        assert!(
            a.forest_holds_main(),
            "the forest carries the pinned chat, which is what this is about",
        );

        // Everything open, so the walk covers a project's children too rather
        // than stopping at the two top-level rows.
        a.tree.expand_all(&a.forest);
        let ids = a.tree_rows();
        assert!(
            ids.len() >= 3,
            "main, the project, its manager-less work: three rows to walk: {ids:?}",
        );
        a.tree.first(&ids);
        for (at, id) in ids.iter().enumerate() {
            if at > 0 {
                a.tree.step(1, &ids);
            }
            assert_eq!(a.tree.index(&ids), at, "the cursor walks one row at a time");
            let frame = rendered(&a, 150, 30);
            // The cursor is the first cell inside a pane's left border, which
            // is what tells it apart from the `▸` a collapsed node draws in
            // the marker column further along the same row.
            let cursor: Vec<&str> = frame
                .lines()
                .filter(|line| {
                    line.split('│')
                        .nth(1)
                        .is_some_and(|cell| cell.starts_with('▸'))
                })
                .collect();
            let label = a
                .forest
                .iter()
                .find(|n| &n.id == id)
                .map(|n| n.label.clone())
                .unwrap_or_default();
            assert!(
                cursor.iter().any(|line| line.contains(label.as_str())),
                "row {at} is `{label}`, and the ▸ must be on it, not above it:\n{frame}",
            );
        }
    }

    /// The fleet's caption describes the fleet, not only its processes.
    ///
    /// It counted runs, so a tree holding projects, managers and works whose
    /// agents had all finished was captioned "nothing delegated yet" — a line
    /// contradicting the rows directly above it.
    #[test]
    fn a_fleet_with_no_running_process_still_counts_what_is_on_it() {
        let mut a = two_works();
        a.agents = Vec::new();
        a.reconcile();

        let caption = a.count_for(Workspace::Fleet);
        assert!(
            !caption.contains("nothing delegated yet"),
            "there are works on the screen: {caption}"
        );
        assert!(caption.contains("work"), "it says what is there: {caption}");
        assert!(
            caption.contains("nothing running"),
            "and that none of it is moving: {caption}"
        );

        // A genuinely empty fleet keeps the sentence written for it.
        let mut empty = app();
        empty.go(Workspace::Fleet);
        empty.reconcile();
        assert_eq!(empty.count_for(Workspace::Fleet), "nothing delegated yet");
    }

    /// An empty state that does not fit says so, rather than becoming a
    /// different sentence.
    ///
    /// The line was handed to the widget whole and clipped in silence. At a
    /// hundred columns the memory screen read "nothing remembered yet —
    /// /remember writes on", which is wrong *and* grammatical, so nothing about
    /// it looks truncated — the reader has no reason to doubt it.
    #[test]
    fn an_empty_state_too_wide_for_its_pane_is_marked_as_cut() {
        let mut a = app();
        a.go(Workspace::Memory);
        a.reconcile();

        let narrow = rendered(&a, 100, 30);
        let line = narrow
            .lines()
            .find(|l| l.contains("nothing remembered yet"))
            .expect("the empty state is drawn");
        assert!(
            line.contains('…'),
            "a clipped sentence has to say it was clipped:\n{line}"
        );
        assert!(
            !line.contains("writes one"),
            "the premise: it does not fit at this width:\n{line}"
        );

        // Given the room, it is left exactly as written.
        let wide = rendered(&a, 200, 30);
        assert!(
            wide.contains("nothing remembered yet — /remember writes one"),
            "nothing is cut when nothing needs to be:\n{wide}"
        );
    }

    /// The board says how old a task is, and says nothing when it cannot.
    ///
    /// `age_ms` was hard-coded to zero in both converters, so every row read
    /// `0s` — "just now" — about tasks that had been sitting there for hours.
    /// The rule this repo already states for the pinned chat's row applies
    /// here: a row with no age has none, and `0s` is a claim that something
    /// just happened.
    #[test]
    fn the_board_ages_a_task_and_admits_when_it_cannot() {
        use jod_core::team::TeamTask;

        let mut a = app();
        a.now_ms = 10_000_000;
        a.tasks = vec![
            TeamTask {
                id: "t-old".into(),
                title: "port the parser".into(),
                owner: None,
                status: "open".into(),
                created_at_ms: a.now_ms - 3_600_000,
                paths: Vec::new(),
            },
            TeamTask {
                id: "t-undated".into(),
                title: "written by an older build".into(),
                owner: None,
                status: "open".into(),
                created_at_ms: 0,
                paths: Vec::new(),
            },
        ];
        a.go(Workspace::Tasks);
        a.reconcile();

        let frame = rendered(&a, 170, 30);
        let row = |needle: &str| {
            frame
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("expected a row for {needle}:\n{frame}"))
                .to_string()
        };
        let old_row = row("port the parser");
        assert!(old_row.contains("1h"), "an hour old reads as an hour:\n{old_row}");
        assert!(!old_row.contains("0s"), "and never as just now:\n{old_row}");
        let undated = row("written by an older build");
        assert!(
            undated.contains('\u{2014}'),
            "no timestamp is a dash, not a zero:\n{undated}"
        );
    }

    /// A popup stays inside the chat column and off the side panel.
    ///
    /// The width was the whole frame minus the box's left edge, which is not
    /// "the room to the right of the input box" whenever anything sits beside
    /// the chat. The palette grew past the composer and painted over the rail,
    /// leaving fragments of it — `8s`, `200k` — and orphaned corners. It
    /// happened at every width wide enough for the panel to exist and too
    /// narrow for the chat column to cover the palette on its own.
    #[test]
    fn a_popup_stays_inside_the_chat_column() {
        let mut a = app();
        for n in 0..6 {
            a.push(Entry::Notice(format!("something happened, number {n}")));
        }
        a.panel = true;
        a.input = "/".into();
        a.cursor = 1;

        let frame = rendered(&a, 110, 42);
        assert!(
            frame.lines().any(|l| l.contains("mode ")),
            "the side panel is on screen at all:\n{frame}"
        );

        // The bound is the **chat column**, not the composer.
        //
        // Asserting `palette <= composer` would be stricter than the truth and
        // would reject correct output: the composer is capped at a comfortable
        // reading width, and on a wide terminal the palette rightly runs past
        // it to the edge of the column. At this size the two happen to
        // coincide, which is exactly how such an assertion hides — so this
        // measures the thing that actually matters, which is whether the
        // palette reaches what sits beside the chat.
        let palette_end = frame
            .lines()
            .find(|l| l.contains("Tab completes"))
            .and_then(|l| l.chars().position(|c| c == '\u{2510}'))
            .unwrap_or_else(|| panic!("the palette is drawn:\n{frame}"));
        // The column where the *panel's* border begins, not the first border on
        // the row — the palette's own opening corner is further left.
        let panel_start = frame
            .lines()
            .find_map(|l| {
                let chars: Vec<char> = l.chars().collect();
                (0..chars.len()).find(|&at| {
                    chars[at..].starts_with(&['\u{250c}', ' ', 'p', 'r', 'o', 'j'])
                })
            })
            .unwrap_or_else(|| panic!("the side panel is drawn:\n{frame}"));
        assert!(
            palette_end < panel_start,
            "the palette closes at column {palette_end} and the panel opens at \
             {panel_start}, so it is drawn over what sits beside the chat:\n{frame}"
        );
    }

    /// A popup floats over the transcript; it does not merge with its border.
    ///
    /// Both popups were anchored at `input.y - h`, which puts their bottom
    /// border on exactly the row the transcript's bottom border occupies. The
    /// `Clear` covers only the popup's own width, so the two joined into one
    /// line of doubled corners —
    ///
    /// ```text
    /// └───────────└──────── @ · ⏎ inserts ────────┘─────────────┘
    /// ```
    ///
    /// — which reads as a half-drawn box. The `/` palette hid it by usually
    /// being as wide as the transcript; the `@` picker is 56 columns and showed
    /// it every time.
    #[test]
    fn a_popup_leaves_the_transcripts_border_whole() {
        let mut a = app();
        // A transcript with something in it, or the console draws the splash
        // instead of a transcript box — and the border this is about is the
        // transcript's. An empty console passes whatever the anchor does.
        for n in 0..6 {
            a.push(Entry::Notice(format!("something happened, number {n}")));
        }
        a.cwd = std::env::current_dir().expect("a working directory");
        a.roots = vec![jod_core::roots::Root {
            id: 1,
            conversation_id: "c".into(),
            path: a.cwd.clone(),
            writable: false,
            position: 0,
            origin: jod_core::roots::Origin::Human,
            added_at_ms: 0,
        }];
        a.input = "look at @".into();
        a.cursor = a.input.len();
        a.open_mention(a.input.len() - 1);

        let frame = rendered(&a, 150, 30);
        let lines: Vec<&str> = frame.lines().collect();
        let composer = lines
            .iter()
            .position(|l| l.contains("┌ you"))
            .expect("the composer box is drawn");
        let border = lines[composer - 1];

        assert!(
            border.matches('└').count() <= 1 && border.matches('┘').count() <= 1,
            "the row above the composer is one unbroken border, not two boxes \
             sharing a line:\n{border}\n\n{frame}"
        );
        assert!(
            !border.contains('┌') && !border.contains('┐'),
            "and nothing opens a box on it:\n{border}"
        );
        // Still on screen: the point is where it sits, not that it was hidden
        // in order to pass this.
        assert!(frame.contains("⏎ inserts"), "the picker is drawn:\n{frame}");
    }

    /// A filter on the fleet says so, on the tree as well as the flat list.
    ///
    /// The flat list has drawn this line since it had one; the tree never did.
    /// Once the fleet always had a tree, filtering hid rows — whole projects,
    /// and `★ jod` — with nothing anywhere saying a filter was on, so the
    /// screen read as a fleet that had lost them. The count has to be about
    /// those rows too: it was reading the flat *agent* list while the pane drew
    /// tree nodes, so it reported `0 match` beside rows plainly on screen.
    #[test]
    fn a_filtered_tree_says_it_is_filtered() {
        let mut a = two_works();
        a.reconcile();
        assert!(a.has_tree(), "the case is the tree, not the flat list");

        let unfiltered = rendered(&a, 150, 30);
        assert!(
            !unfiltered.contains("filter"),
            "nothing is claimed when nothing is filtered:\n{unfiltered}"
        );

        a.here_mut().filter = Some("parser".into());
        let frame = rendered(&a, 150, 30);
        assert!(
            frame.contains("/parser"),
            "the typed filter is on screen:\n{frame}"
        );
        assert!(
            frame.contains("match"),
            "and how much it is hiding:\n{frame}"
        );
        // The number has to be about the rows on screen. It counted
        // `row_ids(Fleet)` — the flat *agent* list plus a sentinel — while the
        // pane drew tree nodes, two unrelated collections. With no agents and a
        // filter matching several rows that arithmetic reports `0 match` beside
        // them, which is what a person actually saw.
        assert!(
            a.agents.is_empty(),
            "the premise: the flat list is empty while the tree is not",
        );
        assert!(
            !frame.contains("0 match"),
            "rows are plainly on screen, so the count cannot be zero:\n{frame}"
        );
        assert!(
            frame.contains(&format!("{} match", a.tree_rows().len())),
            "it counts the rows the pane draws:\n{frame}"
        );
    }

    /// The fleet says when nothing is marking stalls.
    ///
    /// Every session arms a heartbeat, and only `jod daemon` sweeps them. With
    /// no daemon no stall is ever marked, so this screen draws every wedged
    /// agent as healthy — the state the mark exists to end, quietly restored by
    /// a daemon nobody started. The console said so once into the transcript,
    /// which the person who opens the fleet an hour later never sees.
    #[test]
    fn the_fleet_says_when_no_daemon_is_watching_for_stalls() {
        let mut a = two_works();
        a.reconcile();

        let quiet = rendered(&a, 150, 30);
        assert!(
            !quiet.contains("no stall watch"),
            "nothing is claimed while a daemon is sweeping:\n{quiet}"
        );

        a.nothing_is_sweeping = true;
        let warned = rendered(&a, 150, 30);
        assert!(
            warned.contains("no stall watch"),
            "the fleet says the mark is not being written:\n{warned}"
        );
        assert!(
            warned.contains("jod daemon"),
            "and names the remedy, since the warning is useless without it:\n{warned}"
        );
    }

    /// A collapsed project whose engineer is wedged must not spin.
    ///
    /// The fleet is read collapsed. A spinner is the strongest "this is fine"
    /// signal the screen has, and a project drew one while its only agent had
    /// been silent for thirty-seven minutes — the exact picture the stall mark
    /// was added to prevent, one level above where it was fixed.
    #[test]
    fn a_collapsed_project_says_stalled_rather_than_spinning() {
        use jod_core::tree::{Node, NodeId, NodeKind};

        let mut a = app();
        a.forest = vec![Node {
            id: NodeId::project("p"),
            parent: None,
            kind: NodeKind::Project,
            depth: 0,
            label: "web".into(),
            summary: String::new(),
            // Truthfully running — its wedged engineer has not exited — which
            // is precisely why `running` alone must not decide the mark.
            running: true,
            status: None,
            stalled_for_ms: None,
            cards: 0,
            blocked: 0,
            stalled: 1,
            colour: "cyan".into(),
            branch: None,
            worktree: None,
            expanded: false,
            has_children: true,
        }];
        a.go(Workspace::Fleet);
        a.reconcile();

        let frame = rendered(&a, 150, 30);
        // The fleet pane's cell, not the detail pane beside it — both name the
        // project, and only one of them is the row being tested.
        let row = frame
            .lines()
            .filter_map(|l| l.split('│').nth(1))
            .find(|cell| cell.contains("web"))
            .expect("the project is drawn in the fleet pane")
            .to_string();
        assert!(row.contains("stalled"), "the row says so: {row}");
        assert!(
            !["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                .iter()
                .any(|frame| row.contains(frame)),
            "and does not also spin: {row}"
        );
    }

    /// The fleet is the agents somebody delegated, not Jod's own errands.
    ///
    /// A titler and a compaction write into no conversation, so the forest has
    /// no node for them and they fell into the pane for runs that belong to no
    /// work. On a machine with four projects on it, five of that pane's six
    /// rows were housekeeping and the delegated run it exists to show was the
    /// one scrolled out of sight.
    #[test]
    fn the_fleet_does_not_count_jods_own_errands_as_agents() {
        let mut a = two_works();
        let titler = jod_core::works::titler_run_name(&uuid::Uuid::new_v4().to_string());
        a.agents = vec![
            agent_line("de1e6a7e", "hello-agent", "running"),
            agent_line("7171e2ed", &titler, "completed"),
            agent_line(
                "c0m9ac70",
                jod_core::works::COMPACTION_RUN_NAME,
                "completed",
            ),
        ];
        a.reconcile();

        let loose: Vec<&str> = a.loose_rows().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            loose,
            vec!["hello-agent"],
            "only the delegated run belongs in the pane",
        );

        let frame = rendered(&a, 150, 30);
        assert!(
            frame.contains("hello-agent"),
            "the delegated run is still drawn:\n{frame}"
        );
        assert!(
            !frame.contains(&titler),
            "and the titler is not:\n{frame}"
        );
    }

    /// The pane was drawn and could not be reached: no row in it was ever
    /// highlighted, so the cursor keys stopped at the last node of the tree and
    /// the detail pane beside it said "nothing selected" whatever you pressed.
    #[test]
    fn the_cursor_reaches_the_loose_pane_and_the_detail_pane_follows_it() {
        let mut a = two_works();
        a.agents = vec![agent_line("de1e6a7e", "hello-agent", "running")];
        a.reconcile();
        assert!(a.has_tree());
        assert_eq!(a.loose_rows().len(), 1, "one run with no node");

        let before = rendered(&a, 150, 30);
        assert!(
            !before.contains("▸ ● de1e6a7e") && !before.contains("▸ ⠋ de1e6a7e"),
            "the cursor starts in the tree:\n{before}"
        );

        let rows = a.tree_rows();
        a.tree.last(&rows);
        let frame = rendered(&a, 150, 30);

        assert_eq!(a.loose_selected(), Some(0), "End lands in the lower pane");
        assert!(
            frame.contains("hello-agent"),
            "the run is named in the detail pane:\n{frame}"
        );
        assert!(
            frame.contains("⏎ watch"),
            "and the pane offers the verbs the row answers:\n{frame}"
        );
        assert!(
            !frame.contains("nothing selected"),
            "a highlighted row is not nothing:\n{frame}"
        );
    }

    /// A row written in Japanese has to stop at the same border an English
    /// one stops at.
    ///
    /// The tree budgets each row in columns, and a column is not a character:
    /// a CJK ideograph paints two of them. When the budget counted characters
    /// instead, a Japanese summary was handed twice the room the row actually
    /// had, the line ran past the pane's right border, and the terminal
    /// silently chopped the end off — taking the ellipsis with it, so nothing
    /// on the screen said the text had been cut.
    ///
    /// Read off the cells rather than off a string length, because the length
    /// of the string is the very thing that was wrong. A wide character owns
    /// two cells, so the test walks the row a cell at a time and asks what the
    /// last one holding anything is. If the row was trimmed to fit, that cell
    /// is the ellipsis. If the row overran and the border cut it short, it is
    /// whatever character happened to land there.
    ///
    /// An English row is seeded beside it, so the fix cannot pass by
    /// truncating every row harder than it needs to.
    #[test]
    fn a_japanese_row_stops_where_an_english_one_stops() {
        use jod_core::works::Origin;

        let store = RealStore::in_memory().expect("an in-memory store");
        for (title, summary) in [
            ("日本語の作業", "とても長い日本語の要約がここにあります"),
            (
                "ascii work",
                "a very long english summary that will not fit in the box at all",
            ),
        ] {
            let work = store.create_work(title).expect("a work");
            store.set_work_title(&work.id, title).expect("a title");
            store
                .set_work_summary(&work.id, summary)
                .expect("a summary");
            let lead = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .expect("a conversation")
                .id;
            store
                .attach_conversation(&lead, &work.id, None, Origin::Agent)
                .expect("a session under the work");
        }

        let mut a = app();
        a.forest = store.forest().expect("a forest");
        a.go(Workspace::Fleet);
        a.reconcile();

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| {
                draw(f, &a);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let cells = |y: u16| -> Vec<String> {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        };
        // A wide character's symbol sits in the first of its two cells and the
        // second is left blank, so reading a row back as text means stepping
        // over those blanks.
        let text = |row: &[String]| -> String {
            let mut out = String::new();
            let mut skip = false;
            for cell in row {
                if skip {
                    skip = false;
                    continue;
                }
                skip = columns(cell) == 2;
                out.push_str(cell);
            }
            out
        };
        let screen = (0..buffer.area.height)
            .map(|y| text(&cells(y)))
            .collect::<Vec<_>>()
            .join("\n");
        // Only the fleet pane, which is the box these rows have to fit in. The
        // detail pane to its right shows the same titles, so a search across
        // the whole screen would measure the wrong box. Its borders are read
        // off the row rather than assumed, so the box is whatever the layout
        // made it.
        let fleet = |y: u16| -> Option<Vec<String>> {
            let row = cells(y);
            let sides: Vec<usize> = (0..row.len()).filter(|x| row[*x] == "│").collect();
            match sides.as_slice() {
                [left, right, ..] => Some(row[left + 1..*right].to_vec()),
                _ => None,
            }
        };

        for (label, kept) in [
            ("日本語の作業", "とても長い日本語"),
            ("ascii work", "a very long english"),
        ] {
            let inside = (0..buffer.area.height)
                .filter_map(fleet)
                .find(|row| text(row).contains(label))
                .unwrap_or_else(|| panic!("no fleet row for {label}:\n{screen}"));

            let last = inside
                .iter()
                .rposition(|cell| !cell.trim().is_empty())
                .unwrap_or_else(|| panic!("the {label} row is empty:\n{screen}"));
            assert_eq!(
                inside[last], "…",
                "the {label} row runs to column {last} of {} and ends on `{}`, \
                 so the border cut it short instead of `cut` trimming it:\n{screen}",
                inside.len(),
                inside[last],
            );
            assert!(
                text(&inside).contains(kept),
                "the {label} row should still show `{kept}`, not be trimmed to \
                 nothing to make room:\n{screen}",
            );
        }
    }

    /// How a run ended has to be on the screen, not only in whatever the agent
    /// happened to say last.
    ///
    /// Three runs under one session, one that finished, one that failed and one
    /// that was killed. Built off a real store and read back through the real
    /// `Store::forest`, because the claim is about what survives the query as
    /// much as about what is drawn: the tree used to carry a single
    /// `running: bool`, so all three of these rows were the same row.
    #[test]
    fn a_finished_a_failed_and_a_killed_run_each_read_differently() {
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

        let runs = [
            ("run-fin0", "wrote-the-tests", "completed"),
            ("run-fail", "built-the-parser", "failed"),
            ("run-kill", "ran-the-suite", "killed"),
        ];
        for (id, name, status) in runs {
            store
                .save_run(&jod_core::store::StoredRun {
                    id: id.into(),
                    name: name.into(),
                    harness: "claude-code".into(),
                    status: status.into(),
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
                    &lead,
                    jod_core::conversation::NewMessage::new(
                        jod_core::conversation::Role::Assistant,
                        "done here",
                    )
                    .from_run(id),
                )
                .expect("a message");
        }

        let mut a = app();
        a.forest = store.forest().expect("a forest");
        a.go(Workspace::Fleet);
        a.reconcile();

        // The rows themselves. Each run wears the glyph the flat list already
        // uses for that status, so the two halves of the fleet screen agree.
        let frame = rendered(&a, 150, 30);
        for ((_, name, status), glyph) in runs.iter().zip(["✓", "✗", "■"]) {
            let row = frame
                .lines()
                .find(|line| line.contains(name))
                .unwrap_or_else(|| panic!("no row for {name}:\n{frame}"));
            assert!(
                row.contains(glyph),
                "a {status} run should wear {glyph}, and this row is `{row}`:\n{frame}",
            );
        }

        // And the detail pane, which used to have two words for four statuses.
        let mut states: Vec<String> = Vec::new();
        for (id, name, _) in runs {
            a.tree.selected = Some(NodeId::run(id));
            let frame = rendered(&a, 150, 30);
            let line = frame
                .lines()
                .find(|line| line.contains("state"))
                .unwrap_or_else(|| panic!("no state line beside {name}:\n{frame}"))
                .trim()
                .to_string();
            states.push(line);
        }
        for (state, word) in states.iter().zip(["completed", "failed", "killed"]) {
            assert!(state.contains(word), "the pane said `{state}`, not {word}");
        }
        let distinct: std::collections::HashSet<&String> = states.iter().collect();
        assert_eq!(
            distinct.len(),
            3,
            "three statuses have to read as three things, not as {states:?}",
        );
    }

    /// A run's row says what the run said, in the same words a person would
    /// read anywhere else on the screen.
    ///
    /// The row used to print `runs.summary` straight out of the column, and
    /// that column holds a serialised `AgentSummary` — so a run appeared as
    /// `{"created_at_ms":1…` under works and sessions that were showing prose.
    /// Seeded with a real `AgentSummary` rather than a hand-written blob,
    /// because the point is that whatever shape that struct has, none of it
    /// belongs on the screen.
    #[test]
    fn a_runs_row_reads_as_prose_rather_than_as_the_json_it_was_stored_with() {
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

        // Exactly what `service::stored_run` writes into the column.
        let recorded = jod_core::AgentSummary {
            id: "de1e6a7e".into(),
            name: "hello-agent".into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "Claude Code".into(),
            status: jod_core::AgentStatus::Completed,
            cwd: "/tmp".into(),
            model: Some("claude-sonnet-4".into()),
            permission: jod_core::PermissionPolicy::Bypass,
            pid: Some(4242),
            pgid: Some(4242),
            process_alive: false,
            watch_command: "jod watch de1e6a7e".into(),
            created_at_ms: 1,
            session_id: Some("sess-1".into()),
            usage: Default::default(),
            event_count: 3,
            last_message: Some("rewrote the tokeniser to stream".into()),
        };
        store
            .save_run(&jod_core::store::StoredRun {
                id: "de1e6a7e".into(),
                name: "hello-agent".into(),
                harness: "claude-code".into(),
                status: "completed".into(),
                cwd: "/tmp".into(),
                session_id: Some("sess-1".into()),
                pid: Some(4242),
                pgid: Some(4242),
                created_at_ms: 1,
                summary: serde_json::to_value(&recorded).expect("a serialised summary"),
            })
            .expect("a run");
        store
            .append_message(
                &lead,
                jod_core::conversation::NewMessage::new(
                    jod_core::conversation::Role::Assistant,
                    "rewrote the tokeniser to stream",
                )
                .from_run("de1e6a7e"),
            )
            .expect("a message");

        let mut a = app();
        a.forest = store.forest().expect("a forest");
        a.go(Workspace::Fleet);
        a.reconcile();

        let frame = rendered(&a, 150, 30);
        let row = frame
            .lines()
            .find(|line| line.contains("hello-agent"))
            .unwrap_or_else(|| panic!("no row for the run:\n{frame}"))
            .to_string();
        assert!(
            row.contains("rewrote the tokeniser to stream"),
            "the run's row should say what it said, and it reads `{row}`:\n{frame}",
        );
        for shard in ["{\"", "created_at_ms", "watch_command", "harness_label"] {
            assert!(
                !row.contains(shard),
                "`{shard}` is machine text and should never reach a row: `{row}`",
            );
        }
        // #130's status glyph shares this row and must survive the change.
        assert!(
            row.contains('✓'),
            "the completed run keeps its glyph: `{row}`",
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
    /// over the fleet and answering writes against one of them, so the reader
    /// has to be able to see whose question each one is.
    ///
    /// **Said once per group rather than once per card.** That is the whole
    /// point of grouping, and it is strictly more information than the row
    /// carried before: a heading has room for the work's own title, where a
    /// thirty-four column row had room for six characters of its id. What the
    /// row must still never do is spend its own columns repeating it.
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
        assert!(
            frame.contains("· subtree"),
            "the scope is on the header, always: {frame}"
        );

        // The heading carries it; the card's own row does not. This is the half
        // of E4.S5 that has to keep holding whichever way the provenance is
        // drawn — a row that repeats what the heading above it just said is the
        // crowding the redesign removed.
        let row = frame
            .lines()
            .find(|line| line.contains("which port for the API?"))
            .expect("the card is drawn")
            .to_string();
        assert!(
            !row.contains(&session),
            "the row spends its columns on the card, not on the id above it: {row}"
        );

        // Narrowed to one conversation, every card came from the same place —
        // and the header says which of the two states this is, because a
        // narrowed rail and a quiet fleet look identical otherwise.
        a.rail.cascade = false;
        let frame = rendered(&a, 150, 40);
        assert!(frame.contains("· here"), "{frame}");
        let row = frame
            .lines()
            .find(|line| line.contains("which port for the API?"))
            .expect("the card is drawn")
            .to_string();
        assert!(
            !row.contains(&session),
            "narrowed or not, the row is the card's: {row}"
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
    /// And it says so with the keystroke that fixes it *from here*. The popup
    /// is open and the cursor is in the chat box; a message naming a shell
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
        assert!(frame.contains("Ctrl-G d"), "{frame}");
    }

    /// The full-screen picker says which tree it is walking.
    ///
    /// It mattered less when the base was always the directory `jod` was
    /// launched in — you knew where you were. The base is a parameter now, and
    /// a list of bare relative paths with no header is a list you cannot tell
    /// apart from the last one.
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

    /// The panel holds the projects, the mode, the harness, the spend and the
    /// context left — a large fraction of the program's state — and
    /// `Shift-Tab` is the only way in. Until it had a row here the only
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
            40,
            Preview::default(),
        );
        let screen = rendered(&a, 120, 30);
        assert!(
            screen.contains("session"),
            "the overlay's row for the panel opened nothing:\n{screen}"
        );
    }

    /// Regression, from a **cold start** — the state every user is in and the
    /// one precondition the older test set away. The projects key has to open
    /// the panel it draws inside, or it flips a flag that renders nothing and
    /// says nothing about why.
    ///
    /// The key is `Ctrl-P` now, and it takes the keyboard as well as opening
    /// the box, so the assertion is both halves: the catalog is on screen, and
    /// the bar says whose keys are in force.
    #[test]
    fn the_projects_key_shows_the_catalog_from_a_cold_start() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        assert!(!a.panel, "a cold start has the panel shut");
        crate::tui::on_key(
            &mut a,
            &mut crate::tui::Thread::default(),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            20,
            40,
            Preview::default(),
        );
        assert!(a.panel && a.projects_open && a.panel_focused);
        let screen = rendered(&a, 120, 30);
        assert!(
            screen.contains("projects"),
            "the projects key drew nothing at all:\n{screen}"
        );
        assert!(
            screen.contains("manager"),
            "the catalog has the keyboard and the bar does not say so:\n{screen}"
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

