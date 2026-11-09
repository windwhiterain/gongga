//! Rendering: header, scrollable log, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::log::Shown;
use crate::panes::{Pane, Panes};
use crate::{LineKind, Screen};

/// SPEC's `hot-key` list, one binding per entry: the reminder row says these and
/// nothing else.
///
/// # Invariant
///
/// SPEC is the source of truth, so a binding SPEC does not name is not on the row
/// -- an operator reads a reminder as the whole keyboard. See
/// `docs/notes/tui-render.md`.
pub const HELP: &[&str] = &[
    "Shift+Enter newline",
    "PageUp/PageDown scroll",
    "Shift+\u{2193} bottom",
    "Shift+Delete delete session",
    "Alt+V paste",
    "Alt+C copy selection",
    "Shift+\u{2191} last cmd",
];

/// What separates two entries on one row of the reminder.
const HELP_SEP: &str = " \u{00b7} ";

/// Rows spent above the body: the header. The input's height is asked separately.
pub const CHROME_ROWS: u16 = 1;

/// The fewest rows `cmd` and its result are given when the client draws one.
///
/// # Invariant
///
/// A floor, not a height: SPEC dropped the `(line)` this row used to carry, so the
/// area is as tall as what the operator asked for -- `Chrome::command_rows` -- and
/// this is only what the prompt's own arithmetic keeps room for.
pub const COMMAND_ROWS: u16 = 1;

/// The most of the window the `cmd` area may take, so the panes keep a body.
///
/// # Invariant
///
/// A share rather than a count: `block_size = 0` caps nothing, and an area with no
/// ceiling would push the events out of a window whose whole job is to show them.
const COMMAND_SHARE: u16 = 3;

/// The prompt on the first row; continuation rows pad behind it.
const PREFIX: &str = "> ";
const PREFIX_WIDTH: u16 = 2;

/// The most rows the input box grows to before it scrolls: prose, not a file.
const MAX_INPUT_ROWS: u16 = 8;

/// The narrowest the list is drawn: a mark and a name still have to read.
pub const LIST_MIN: u16 = 6;

pub fn draw(f: &mut Frame, s: &mut Screen) -> Rect {
    s.panes.begin_frame();
    let area = f.area();
    let input = input_rows(s.input.text(), area.width, area.height);
    let chunks = areas(area, input, s.panes.chrome(command_width(area)));

    draw_header(f, s, chunks[0]);
    // The body is one split: the session list, then the windows beside it.
    let (list, panes) = body(chunks[1], &s.panes);
    draw_list(f, s, list);
    // The panes borrow the screen mutably: here each scroll offset gets clamped.
    draw_panes(f, s, panes);
    draw_quota(f, s, chunks[2]);
    draw_command(f, s, chunks[3]);
    draw_input(f, s, chunks[4]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[4]);
    }
    draw_help(f, chunks[5]);
    // A window nobody drew holds no bodies. See `docs/notes/tui-cache.md`.
    s.panes.end_frame();
    // The input area this frame laid out: the caret asks here, not a second
    // computation of the box.
    chunks[4]
}

/// The rows of chrome that exist only when something is drawn in them.
///
/// # Invariant
///
/// One value rather than four arguments: SPEC's rows are positional, and two
/// bare booleans in a row is a call that swaps them silently.
/// See `docs/notes/tui-render.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Chrome {
    /// SPEC's `[cmd/cmd_result]` area: the client's command window exists.
    pub command: bool,
    /// The rows that area wants, from the log; the layout decides what it gets.
    pub command_rows: u16,
    /// SPEC's `active_quota` row: a reading has been announced.
    pub quota: bool,
}

/// The six parts of SPEC's window, in the order they are drawn.
///
/// # Invariant
///
/// This is the only layout a frame has: the copy, the page height and the mouse
/// all ask here, so what an operator reads is what was on the screen. The help
/// row is reserved before anything else, so it is the last row of any window.
pub fn areas(area: Rect, input_rows: u16, chrome: Chrome) -> Vec<Rect> {
    // The reminder first: on a short window the rest shrinks, so the keys stay.
    let reminder = help_rows(area.width).len() as u16;
    let [rest, help] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(reminder)]).areas(area);
    // SPEC gives this area no `(line)`: a command and its answer take the rows
    // they need, up to a share of the window.
    let command = match chrome.command {
        false => 0,
        true => chrome.command_rows.clamp(
            COMMAND_ROWS,
            (area.height / COMMAND_SHARE).max(COMMAND_ROWS),
        ),
    };
    let mut chunks: Vec<Rect> = Layout::vertical([
        Constraint::Length(1),                                // header
        Constraint::Min(1),                                   // session list | windows
        Constraint::Length(if chrome.quota { 1 } else { 0 }), // active_quota
        Constraint::Length(command),                          // cmd and its result
        Constraint::Length(input_rows),                       // the prompt
    ])
    .split(rest)
    .to_vec();
    chunks.push(help);
    chunks
}

/// The columns the command area wraps at: one short of the window, as it is drawn.
fn command_width(area: Rect) -> usize {
    area.width.saturating_sub(1).max(1) as usize
}

/// The body split into the session list and the windows beside it.
///
/// # Invariant
///
/// The list is as wide as its own content -- the status mark, the brackets, the
/// longest name and one column of air -- so a short name is not padded out with empty
/// screen. It never takes more than half the window: the windows are where the work
/// is read.
pub fn body(area: Rect, panes: &Panes) -> (Rect, Rect) {
    let list = list_width(panes).min(area.width.saturating_sub(1) / 2);
    let list = Rect {
        width: list,
        ..area
    };
    let panes = Rect {
        x: area.x + list.width,
        width: area.width.saturating_sub(list.width),
        ..area
    };
    (list, panes)
}

/// Columns the session list needs: the mark, the indent, the marker, the name, air.
pub fn list_width(panes: &Panes) -> u16 {
    let longest = panes
        .list_rows()
        .iter()
        .map(|row| {
            // The status mark and its space are `marker_column`'s own first two, and
            // a folded row spends one more on its `|`.
            row.marker_column()
                + UnicodeWidthStr::width(row.marker())
                + UnicodeWidthStr::width(row.label.as_str())
                + if row.session { 2 } else { 0 }
        })
        .max()
        .unwrap_or(0);
    // One column of air after the longest row.
    ((longest + 1) as u16).max(LIST_MIN)
}

/// Where one pane is drawn: its header, if it has one, and the rows below it.
#[derive(Debug)]
pub struct Slot {
    pub name: String,
    /// The box a sticky pane is drawn in, and its rows start inside it.
    pub frame: Option<Rect>,
    pub header: Option<Rect>,
    pub body: Rect,
    /// What the header says beside the name, and how many rows it needed.
    pub note: Option<String>,
}

/// A session's name as every area draws it: bracketed, so it reads as a name.
pub fn bracketed(name: &str) -> String {
    format!("[{name}]")
}

/// The windows' columns, with the sticky session taking what an even share leaves.
///
/// # Invariant
///
/// A column that cannot get a cell is not drawn, and `hidden` counts them: a window
/// nobody can read is worse than one that says it is not on screen. A header takes
/// the rows its text wraps to, but never the column's last one.
pub fn pane_slots(area: Rect, panes: &Panes, sticky: Option<&str>) -> (Vec<Slot>, usize) {
    // Only the sessions drawn beside the list: the command line has its own row.
    let events: Vec<&Pane> = panes.of_kind(crate::panes::Kind::Shown).collect();
    if events.is_empty() || area.width == 0 || area.height == 0 {
        return (Vec::new(), 0);
    }
    let n = events.len() as u16;
    // SPEC: the windows are a horizontal split, so the width is what is shared.
    let per = (area.width / n).max(1);
    let extra = area.width.saturating_sub(per * n);

    let mut slots: Vec<Slot> = Vec::with_capacity(events.len());
    let mut x = area.x;
    let mut hidden = 0usize;
    for pane in &events {
        let share = per + u16::from(Some(pane.name.as_str()) == sticky) * extra;
        let left = area.right().saturating_sub(x);
        let width = share.min(left);
        if width == 0 {
            hidden += 1;
            continue;
        }
        let column = Rect {
            x,
            y: area.y,
            width,
            height: area.height,
        };
        x += width;
        slots.push(slot_of(panes, &pane.name, column, sticky));
    }
    // The last drawn window says when the division hid one: a session whose
    // window is missing has to be explained somewhere.
    if hidden > 0
        && let Some(last) = slots.last_mut()
    {
        last.note = Some(match last.note.take() {
            Some(note) => format!("{note} · {hidden} not shown"),
            None => format!("{hidden} not shown"),
        });
    }
    (slots, hidden)
}

/// One session's column: its frame when it is sticky, its header and its rows.
fn slot_of(panes: &Panes, name: &str, column: Rect, sticky: Option<&str>) -> Slot {
    let note = pane_note(panes, name);
    // Framed, not coloured: the box is the mark, and its rows keep their kind.
    let frame = (Some(name) == sticky && column.width > 0 && column.height > 2).then_some(column);
    let inner = match frame {
        Some(frame) => Rect {
            y: frame.y + 1,
            height: frame.height - 2,
            ..frame
        },
        None => column,
    };
    // A header's own height needs the width it will be drawn at, so it comes after.
    let rows = match inner.height {
        0 | 1 => 0,
        height => (pane_header_rows(name, note.as_deref(), inner.width).len() as u16)
            .min(height - 1)
            .max(1),
    };
    Slot {
        name: name.to_string(),
        frame,
        header: (rows > 0).then_some(Rect {
            height: rows,
            ..inner
        }),
        body: Rect {
            y: inner.y + rows,
            height: inner.height - rows,
            ..inner
        },
        note,
    }
}

/// What a pane's title says beside the name, as the command layer spelled it.
///
/// # Invariant
///
/// The measurements and the session's own tuning, read from the announcement: a
/// frame cannot say a session runs on something the client was not told.
pub fn pane_note(panes: &Panes, name: &str) -> Option<String> {
    panes
        .listed()
        .iter()
        .find(|listed| listed.name == name)
        .map(|listed| listed.title.clone())
        .filter(|title| !title.is_empty())
}

/// A pane's header, broken into the rows it is drawn on.
///
/// # Invariant
///
/// The one function that decides where a title breaks -- the layout reserves these
/// rows and the frame draws these rows -- and every row it returns fits `width`.
pub fn pane_header_rows(name: &str, note: Option<&str>, width: u16) -> Vec<String> {
    let text = match note {
        Some(note) if !note.is_empty() => format!("{} {note}", bracketed(name)),
        _ => bracketed(name),
    };
    wrap_words(&text, width.saturating_sub(1).max(1))
        .into_iter()
        .map(|row| format!(" {row}"))
        .collect()
}

/// `text` broken into rows: greedy on spaces, and a word wider than the column is
/// cut where it runs out.
fn wrap_words(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    for word in text.split_whitespace() {
        let size = UnicodeWidthStr::width(word);
        if used > 0 && used + 1 + size > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        if used > 0 {
            row.push(' ');
            used += 1;
        }
        // A word wider than a whole row is cut, because one row cannot hold it.
        let mut rest = word;
        while UnicodeWidthStr::width(rest) > width - used {
            let (head, tail) = split_at_width(rest, width - used);
            row.push_str(&head);
            rows.push(std::mem::take(&mut row));
            used = 0;
            rest = tail;
        }
        row.push_str(rest);
        used += UnicodeWidthStr::width(rest);
    }
    rows.push(row);
    rows
}

/// `text` split where `width` columns end, at a character boundary.
fn split_at_width(text: &str, width: usize) -> (String, &str) {
    let mut used = 0usize;
    for (index, ch) in text.char_indices() {
        let size = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + size > width {
            return (text[..index].to_string(), &text[index..]);
        }
        used += size;
    }
    (text.to_string(), "")
}

/// What a click on the list landed on: the row's name, and whether it asked to fold.
///
/// # Invariant
///
/// A row's fold is one gesture with two places to start it and its name is neither:
/// the `|` while folded, and the status mark -- the only column left that is not text
/// -- while expanded, since an expanded row holds no column for a fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListHit {
    pub name: String,
    pub fold: bool,
}

/// The session whose row in the list covers `row`, for a click.
///
/// # Invariant
///
/// It reads the same rows `draw_list` writes -- from the list's own offset -- so a
/// click lands on the name the operator read. See `docs/notes/tui-render.md`.
pub fn list_at(area: Rect, panes: &Panes, column: u16, row: u16) -> Option<ListHit> {
    if !covers(&area, column, row) {
        return None;
    }
    let rows = panes.list_rows();
    let scroll = panes.list().map(|pane| pane.scroll).unwrap_or(0);
    let (start, _end) = list_window(rows.len(), area.height as usize, scroll);
    let index = start + (row - area.y) as usize;
    let hit = rows.get(index)?;
    let cell = (column - area.x) as usize;
    Some(ListHit {
        name: hit.name.clone(),
        // The `|`, or the mark of a row that draws none. Both are the frame's own
        // columns. See `docs/notes/tui-render.md`.
        fold: hit.group && (cell == 0 || (hit.collapsed && cell == hit.marker_column())),
    })
}

/// The session whose header covers this cell, for a click.
///
/// # Invariant
///
/// Headers only: SPEC's click sets the sticky session from a session's own name
/// and figures, and the events under them are read, not clicked.
pub fn header_at(slots: &[Slot], column: u16, row: u16) -> Option<&str> {
    slots
        .iter()
        .find(|slot| slot.header.is_some_and(|head| covers(&head, column, row)))
        .map(|slot| slot.name.as_str())
}

/// The pane that covers screen row `row`, for the wheel and a click.
pub fn pane_at(slots: &[Slot], column: u16, row: u16) -> Option<&str> {
    slots
        .iter()
        .find(|slot| {
            slot.frame.is_some_and(|frame| covers(&frame, column, row))
                || covers(&slot.body, column, row)
                || slot.header.is_some_and(|head| covers(&head, column, row))
        })
        .map(|slot| slot.name.as_str())
}

/// Whether a cell falls in this rectangle.
///
/// # Invariant
///
/// Both coordinates: the panes are a horizontal split, so a row of the window holds
/// several of them and the column is what says which. Testing the row alone found
/// the leftmost pane for every click. See `docs/notes/tui-render.md`.
pub fn covers(area: &Rect, column: u16, row: u16) -> bool {
    area.height > 0
        && area.width > 0
        && area.y <= row
        && row < area.bottom()
        && area.x <= column
        && column < area.right()
}

/// Rows the input box needs for `text` here, capped so the log keeps three.
pub fn input_rows(text: &str, width: u16, height: u16) -> u16 {
    let rows = layout_input(text, text.len(), content_width(width))
        .rows
        .len() as u16;
    // The floor: three log rows, and a command line counted even when absent.
    let help = help_rows(width).len() as u16;
    let spare = height
        .saturating_sub(CHROME_ROWS + help + COMMAND_ROWS + 3)
        .max(1);
    rows.clamp(1, MAX_INPUT_ROWS.min(spare))
}

/// Columns for text: the prefix on the left, one spare on the right.
fn content_width(width: u16) -> usize {
    width.saturating_sub(PREFIX_WIDTH + 1).max(1) as usize
}

/// One row of the input as it will be drawn, plus where the cursor sits.
struct InputLayout {
    rows: Vec<String>,
    /// The bytes of the text each row holds: the rows are the text, cut.
    spans: Vec<(usize, usize)>,
    cursor_row: usize,
    cursor_col: u16,
}

/// Splits the input into rows: `'\n'` is a hard break, width wraps the rest.
///
/// # Invariant
///
/// The last row always exists, so an empty input, or one ending in a newline,
/// has somewhere for the cursor to sit. See `docs/notes/tui-render.md`.
fn layout_input(text: &str, cursor: usize, width: usize) -> InputLayout {
    let width = width.max(1);
    let cursor = cursor.min(text.len());
    let mut rows: Vec<String> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut row = String::new();
    let mut row_start = 0usize;
    let mut used = 0usize;
    let mut at_cursor: Option<(usize, u16)> = None;

    for (index, ch) in text.char_indices() {
        if at_cursor.is_none() && index == cursor {
            at_cursor = Some((rows.len(), used as u16));
        }
        if ch == '\n' {
            rows.push(std::mem::take(&mut row));
            spans.push((row_start, index));
            row_start = index + 1;
            used = 0;
            continue;
        }
        let advance = ch.width().unwrap_or(0);
        if used + advance > width && used > 0 {
            rows.push(std::mem::take(&mut row));
            spans.push((row_start, index));
            row_start = index;
            used = 0;
        }
        row.push(ch);
        used += advance;
    }
    rows.push(row);
    spans.push((row_start, text.len()));

    let (cursor_row, cursor_col) = at_cursor.unwrap_or((rows.len() - 1, used as u16));
    InputLayout {
        rows,
        spans,
        cursor_row,
        cursor_col,
    }
}

fn draw_header(f: &mut Frame, s: &Screen, area: Rect) {
    // The bar spans the row: a strip that stops where the text stops reads as
    // a selection. The server comes first.
    let label = s
        .server_text
        .clone()
        .or_else(|| s.server.as_ref().map(|server| server.label()));
    let mut title = match label {
        Some(label) => format!(" gongga · {label} · {} ", s.status),
        None => format!(" gongga · {} ", s.status),
    };
    let used = UnicodeWidthStr::width(title.as_str()).min(area.width as usize);
    title.push_str(&" ".repeat(area.width as usize - used));
    // Painted by the status's kind: a failure and a fact are not equally urgent.
    let bg = match s.status_kind {
        LineKind::Error => Color::Red,
        LineKind::Command | LineKind::Result => Color::Green,
        _ => Color::Cyan,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Black).bg(bg),
        ))),
        area,
    );
}

/// The session list: a status mark, then a name bright when it has a window.
///
/// # Invariant
///
/// Two facts, two looks, one column each: the mark is what the announcement says the
/// session is doing, and the name is bright exactly when that session is shown beside
/// the list. The sticky session is not marked here -- it is shown by definition, and
/// the frame around its window is its mark. See `docs/notes/tui-render.md`.
fn draw_list(f: &mut Frame, s: &mut Screen, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let height = area.height as usize;
    let total = s.panes.list_rows().len();
    let Some(scroll) = s.panes.list_mut().map(|pane| {
        pane.bound(total.saturating_sub(height));
        pane.scroll
    }) else {
        return;
    };
    let rows = s.panes.list_rows();
    let (start, end) = list_window(total, height, scroll);
    let drawn: Vec<Line> = rows
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            let shown = row.session
                && s.panes
                    .get(&row.name)
                    .is_some_and(|pane| pane.kind == crate::panes::Kind::Shown);
            let (mark, style) = status_mark(row.mark);
            // The indent and the marker are structure: cyan, as every rule is.
            let indent = " ".repeat(row.indent());
            let name = match row.session {
                true => bracketed(&row.label),
                false => row.label.clone(),
            };
            Line::from(vec![
                Span::styled(format!("{mark} "), style),
                Span::styled(indent, dim()),
                Span::styled(row.marker(), structure()),
                Span::styled(name, if shown { bright() } else { dim() }),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(drawn), area);
    if scroll > 0 {
        draw_bottom(f, area);
    }
}

/// A status as one column: what the session is doing, in the space of a letter.
///
/// # Invariant
///
/// One column whatever the word is: a word costs the width of the widest one
/// (`running` is seven). Shape and colour carry different facts -- the shape whether a
/// turn is running (`●` yes, `○` no), the colour how much attention the row asks for --
/// so a mark is legible without its colour. See `docs/notes/tui-render.md`.
fn status_mark(mark: crate::tree::Mark) -> (&'static str, Style) {
    match mark {
        crate::tree::Mark::Running => ("\u{25cf}", Style::default().fg(Color::Green)),
        crate::tree::Mark::Stopped => ("\u{25cb}", Style::default().fg(Color::Yellow)),
        crate::tree::Mark::Error => (
            "\u{2715}",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        crate::tree::Mark::Idle => ("\u{25cb}", Style::default().fg(Color::Green)),
    }
}

/// How the sticky session is painted where a row is filled for it.
///
/// # Invariant
///
/// One mark -- black on cyan -- on the way back to the newest row; `base` is the
/// area's own look for every other row. A window in the split is framed instead, so
/// its rows keep the look of the kind of event they hold, and the list says what is
/// shown rather than what is sticky. See `docs/notes/tui-render.md`.
fn name_style(sticky: bool, base: Style) -> Style {
    if sticky {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        base
    }
}

/// The rows of the list a frame shows: its tail, scrolled up by `scroll`.
///
/// # Invariant
///
/// The frame and the click read the same window, from the same offset, so a click
/// lands on the name the operator saw. See `docs/notes/tui-render.md`.
fn list_window(total: usize, height: usize, scroll: usize) -> (usize, usize) {
    let end = total - scroll.min(total.saturating_sub(height));
    (end.saturating_sub(height), end)
}

/// The way back to the newest row: its text and the one row it covers.
///
/// # Invariant
///
/// One rect for the frame and for the click, so a pointer that lands on the hint
/// gets the hint rather than the session drawn under it. A view too narrow for even
/// the arrow has no hint, and then nothing to click. See `docs/notes/tui-render.md`.
fn bottom_hint(area: Rect) -> Option<(&'static str, Rect)> {
    const FULL: &str = " \u{2193} bottom \u{00b7} Shift+\u{2193} ";
    // A narrow view gets the bare arrow: one column fits the list's spare one.
    const SHORT: &str = "\u{2193}";
    let hint = if area.width > UnicodeWidthStr::width(FULL) as u16 {
        FULL
    } else {
        SHORT
    };
    let width = UnicodeWidthStr::width(hint) as u16;
    if area.height == 0 || area.width < width {
        return None;
    }
    let rect = Rect {
        x: area.right() - width,
        y: area.bottom() - 1,
        width,
        height: 1,
    };
    Some((hint, rect))
}

/// The rect the hint covers in `area`, for the pointer.
pub fn bottom_rect(area: Rect) -> Option<Rect> {
    bottom_hint(area).map(|(_, rect)| rect)
}

/// The way back to the newest row, over the last row of something scrolled up.
///
/// # Invariant
///
/// SPEC asks for it exactly when the view is away from its bottom: a button that
/// is always there says nothing about where the view is. See
/// `docs/notes/tui-render.md`.
fn draw_bottom(f: &mut Frame, area: Rect) {
    let Some((hint, rect)) = bottom_hint(area) else {
        return;
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            name_style(true, Style::default()),
        ))),
        rect,
    );
}

/// The view whose hint covers this cell, for a click: the list's, or a window's.
///
/// # Invariant
///
/// Asked before the row it is drawn over: the way back to the newest row must not
/// become a click on the name or the session that holds it. See
/// `docs/notes/tui-render.md`.
pub fn bottom_at(
    slots: &[Slot],
    list: Rect,
    panes: &Panes,
    column: u16,
    row: u16,
) -> Option<String> {
    let scrolled = panes.list().is_some_and(|pane| pane.scroll > 0);
    if scrolled && bottom_rect(list).is_some_and(|rect| covers(&rect, column, row)) {
        return panes.list().map(|pane| pane.name.clone());
    }
    for slot in slots {
        let scrolled = panes.get(&slot.name).is_some_and(|pane| pane.scroll > 0);
        if !scrolled {
            continue;
        }
        if let Some(rect) = bottom_rect(slot.body) {
            if covers(&rect, column, row) {
                return Some(slot.name.clone());
            }
        }
    }
    None
}

/// A pane's header: the name, then whatever the title carries beside it.
///
/// # Invariant
///
/// The one place a pane's header is drawn, so a session reads the same whatever
/// else the frame holds; `selected` is [`name_style`]'s fill, which the caller
/// turns off for a pane its frame already marks. See `docs/notes/tui-render.md`.
fn draw_pane_header(f: &mut Frame, name: &str, area: Rect, selected: bool, note: Option<&str>) {
    let style = name_style(selected, dim());
    let rows: Vec<Line> = pane_header_rows(name, note, area.width)
        .into_iter()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect();
    f.render_widget(Paragraph::new(rows), area);
}

/// SPEC's `active_quota` row: the provider, then what it says is left.
///
/// # Invariant
///
/// One row under the command line, present only when a reading has arrived, and
/// painted by the row's own kind: a balance nobody could read is an error, not a
/// fact. See `docs/notes/tui-render.md`.
fn draw_quota(f: &mut Frame, s: &Screen, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let Some(row) = s.panes.quota() else {
        return;
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            row.text.clone(),
            style_for(row.kind),
        ))),
        area,
    );
}

/// One row of an item: the box's top rule with the item's label, its bottom rule, or
/// a row of its body.
///
/// # Invariant
///
/// The box is two rules and no sides, so every row of it is the full width of the
/// window and the label starts at the window's left edge, where the mark of every
/// folded row starts. The scroll a box keeps is its rows: nothing here moves one, and
/// `box_edge` is not drawn here at all -- it is a region of the wheel, not a column
/// the box gives up. See `docs/notes/tui-render.md`.
fn ruled_row(kind: LineKind, text: &str, rule: crate::log::Rule, width: u16) -> Line<'static> {
    use crate::log::Rule;
    let full = width as usize;
    Line::from(match rule {
        Rule::Free | Rule::Inside => item_row(kind, text).spans,
        Rule::Bottom => vec![Span::styled(
            "\u{2500}".repeat(full),
            Style::default().fg(Color::Cyan),
        )],
        Rule::Top => top_rule(kind, text, full),
    })
}

/// The box's top rule: the item's label, then air and dashes to the box's width.
///
/// # Invariant
///
/// At least one dash, whatever the label's length: a top rule with no dash on it
/// reads like a folded row at the same width.
fn top_rule(kind: LineKind, text: &str, inner: usize) -> Vec<Span<'static>> {
    let (title, _) = split_at_width(text, inner);
    let mut body = item_row(kind, &title);
    let used: usize = body
        .spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if used < inner {
        let (air, dash) = match inner - used {
            1 => (0, 1),
            fill => (1, fill - 1),
        };
        body.spans.push(Span::styled(
            format!("{}{}", " ".repeat(air), "\u{2500}".repeat(dash)),
            Style::default().fg(Color::Cyan),
        ));
    }
    body.spans
}

/// One row of an item: SPEC's coloured `symbol`, then the quiet gray beside it.
///
/// # Invariant
///
/// The kind's colour is the mark's, and the text after it is the one quiet gray --
/// so that what colour is left for is what the reader came for. An answer's rows are
/// never here: a document styled them, and `shown_row` draws them as they are.
/// See `docs/notes/tui-render.md`.
fn item_row(kind: LineKind, text: &str) -> Line<'static> {
    let mark = kind.mark();
    let rest = match mark.is_empty() {
        true => None,
        false => text.strip_prefix(mark.as_str()),
    };
    match rest {
        Some(rest) => Line::from(vec![
            Span::styled(mark, style_for(kind)),
            Span::styled(rest.to_string(), dim()),
        ]),
        None => Line::from(Span::styled(text.to_string(), dim())),
    }
}

/// One row of a pane, in the ink the log said it is drawn in.
///
/// # Invariant
///
/// A row a document styled is drawn as it is and nothing else: the runs are the
/// answer, so a break the log made cannot take a run's ink away. Every other row is
/// the kind's mark plus the quiet gray.
fn shown_row(
    kind: LineKind,
    shown: &Shown<'_>,
    rule: crate::log::Rule,
    width: u16,
) -> Line<'static> {
    match shown {
        // Cloned: the frame outlives the borrow of the log it read.
        Shown::Styled(spans) => Line::from(spans.to_vec()),
        Shown::Text(text) => ruled_row(kind, text.as_ref(), rule, width),
    }
}

/// Draws every pane: the header that names it, then the rows that fit in its share.
fn draw_panes(f: &mut Frame, s: &mut Screen, area: Rect) {
    let sticky = s.sticky_session.clone();
    let (slots, _hidden) = pane_slots(area, &s.panes, sticky.as_deref());
    for slot in &slots {
        if let Some(frame) = slot.frame {
            // Rules, not a border: a sticky window keeps every column it has.
            f.render_widget(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::Cyan)),
                frame,
            );
        }
        if let Some(header) = slot.header {
            let note = slot.note.as_deref();
            // A framed pane is marked by its box; the fill is the fallback.
            let filled = slot.frame.is_none() && Some(slot.name.as_str()) == sticky.as_deref();
            draw_pane_header(f, &slot.name, header, filled, note);
        }
        draw_rows(f, s, &slot.name, slot.body);
    }
}

/// Draws the single line `cmd` and its result share, when the area is split.
fn draw_command(f: &mut Frame, s: &mut Screen, area: Rect) {
    let Some(name) = s.panes.command().map(|pane| pane.name.clone()) else {
        return;
    };
    draw_rows(f, s, &name, area);
}

/// One pane's rows, and the scroll bound it writes back.
fn draw_rows(f: &mut Frame, s: &mut Screen, name: &str, area: Rect) {
    s.panes.mark_drawn(name);
    s.panes.note_room(name, area.height as usize);
    if area.height == 0 {
        return;
    }
    // One column short: a full-width row would otherwise be wrapped again.
    let width = area.width.saturating_sub(1).max(1) as usize;
    // A window this process has never drawn starts from its journal, and only as
    // far back as this pane reaches.
    s.panes.seed(name, width, area.height as usize);
    // The pane's row count: an offset cannot say a line arrived under it.
    let rows = s.panes.rows(name, width);
    let Some(pane) = s.panes.get_mut(name) else {
        return;
    };
    // Before the window is read: a taller pane keeps the rows it was showing.
    pane.anchor(area.height as usize, rows);
    let scroll = pane.scroll;
    let name = pane.name.clone();
    let window = s.panes.window(&name, width, area.height as usize, scroll);
    let max_scroll = window.max_scroll;
    // Cloned: the rows a frame draws cannot outlive the log they came from.
    let lines: Vec<Line> = window
        .rows
        .iter()
        .map(|(kind, shown, rule)| shown_row(*kind, shown, *rule, width as u16))
        .collect();
    drop(window);
    let scroll = scroll.min(max_scroll);
    let end = rows.saturating_sub(scroll);
    let start = end.saturating_sub(area.height as usize);
    // What this pane draws is what memory keeps; the rest goes back to the file.
    s.panes.evict(&name, start..end);
    if let Some(pane) = s.panes.get_mut(&name) {
        // Clamped here too, for a pane that just grew or a log that just shrank.
        pane.bound(max_scroll);
    }
    f.render_widget(Paragraph::new(lines), area);
    if scroll > 0 {
        draw_bottom(f, area);
    }
}

/// The rows of the input box a frame shows, from the first one it scrolls to.
fn input_rows_of<'a>(s: &'a Screen, layout: &'a InputLayout, area: Rect) -> Vec<Line<'a>> {
    let height = area.height.max(1) as usize;
    // The box stops at its cap and scrolls, so the cursor never leaves the window.
    let first = (layout.cursor_row + 1).saturating_sub(height);
    let selection = s.input.selection();
    layout
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, row)| {
            let lead = if index == 0 { PREFIX } else { "  " };
            // The placeholder is not text, so it only fills the first row.
            let body = if index == 0 && s.input.is_empty() {
                vec![Span::styled(s.placeholder.clone(), dim())]
            } else {
                input_body(row, layout, index, selection)
            };
            let mut spans = vec![Span::styled(lead, Style::default().fg(Color::Cyan))];
            spans.extend(body);
            Line::from(spans)
        })
        .collect()
}

/// The text of one input row, with the selected part of it marked.
///
/// # Invariant
///
/// What is marked is the selection's bytes and nothing else, so what the
/// window shows highlighted is exactly what `Alt+C` copies.
fn input_body(
    row: &str,
    layout: &InputLayout,
    index: usize,
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let plain = || vec![Span::raw(row.to_string())];
    let Some((start, end)) = selection else {
        return plain();
    };
    let Some(&(row_start, row_end)) = layout.spans.get(index) else {
        return plain();
    };
    let from = start.max(row_start);
    let to = end.min(row_end);
    if from >= to {
        return plain();
    }
    let mut spans = Vec::new();
    if from > row_start {
        spans.push(Span::raw(row[..from - row_start].to_string()));
    }
    spans.push(Span::styled(
        row[from - row_start..to - row_start].to_string(),
        Style::default().add_modifier(Modifier::REVERSED),
    ));
    if to < row_end {
        spans.push(Span::raw(row[to - row_start..].to_string()));
    }
    spans
}

fn draw_input(f: &mut Frame, s: &Screen, area: Rect) {
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let lines = input_rows_of(s, &layout, area);
    // The caller anchors the caret after applying the frame; a position set here
    // would be applied by nobody.
    f.render_widget(Paragraph::new(lines), area);
}

/// SPEC's hot-keys, packed into the rows a window `width` columns wide allows.
///
/// # Invariant
///
/// One binding is never split across two rows: a row that cannot hold the next
/// entry ends before it, and an entry wider than the window keeps a row of its
/// own. The layout and the frame both ask here, so they cannot disagree about how
/// many rows the reminder takes. See `docs/notes/tui-render.md`.
pub fn help_rows(width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for entry in HELP {
        if row.is_empty() {
            row.push_str(entry);
            continue;
        }
        let grown = UnicodeWidthStr::width(row.as_str())
            + UnicodeWidthStr::width(HELP_SEP)
            + UnicodeWidthStr::width(*entry);
        if grown <= width {
            row.push_str(HELP_SEP);
            row.push_str(entry);
        } else {
            rows.push(std::mem::take(&mut row));
            row.push_str(entry);
        }
    }
    rows.push(row);
    rows
}

/// The reminder, on the window's last rows: the one place a binding is read.
fn draw_help(f: &mut Frame, area: Rect) {
    let rows: Vec<Line> = help_rows(area.width)
        .into_iter()
        .map(|row| Line::from(Span::styled(row, dim())))
        .collect();
    f.render_widget(Paragraph::new(rows), area);
}

/// Where a click in the input box lands, as a byte offset into the text.
///
/// # Invariant
///
/// The offset is a character boundary: the text is cut at it, and a cut inside
/// a character panics.
pub fn input_offset(s: &Screen, area: Rect, column: u16, row: u16) -> Option<usize> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    if row < area.y || row >= area.bottom() || column < area.x || column >= area.right() {
        return None;
    }
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let height = area.height.max(1) as usize;
    let first = (layout.cursor_row + 1).saturating_sub(height);
    let index = (row - area.y) as usize + first;
    // The prefix takes the first columns; the rest of the row is the text.
    let target = column.saturating_sub(area.x + PREFIX_WIDTH) as usize;
    Some(offset_in(&layout, index, target)?)
}

/// Where the caret goes when an arrow moves it one drawn row.
///
/// # Invariant
///
/// The rows are the ones `layout_input` cuts and the frame paints, so a wrapped
/// line is several rows to the arrows exactly as it is to the eye; a `None` is
/// a caret already on the first or the last row. See `docs/notes/tui-render.md`.
pub fn caret_row(s: &Screen, area: Rect, up: bool) -> Option<usize> {
    if area.width == 0 {
        return None;
    }
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let index = if up {
        layout.cursor_row.checked_sub(1)?
    } else {
        let below = layout.cursor_row + 1;
        if below >= layout.rows.len() {
            return None;
        }
        below
    };
    offset_in(&layout, index, layout.cursor_col as usize)
}

/// The byte offset of column `target` of row `index`; the row's end past its text.
fn offset_in(layout: &InputLayout, index: usize, target: usize) -> Option<usize> {
    let (row_start, row_end) = *layout.spans.get(index)?;
    let text = layout.rows.get(index)?;
    let mut used = 0usize;
    for (offset, ch) in text.char_indices() {
        let advance = ch.width().unwrap_or(0);
        if used + advance > target {
            return Some(row_start + offset);
        }
        used += advance;
    }
    Some(row_end)
}

/// Where the caret goes for this input in this area.
///
/// # Invariant
///
/// The renderer and the window agree on this one function, so a frame that draws
/// the input and a frame that only re-anchors the caret cannot disagree about where
/// it is.
pub fn caret(s: &Screen, area: Rect) -> (u16, u16) {
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let height = area.height.max(1) as usize;
    let first = (layout.cursor_row + 1).saturating_sub(height);
    // A row that exactly fills the width puts the cursor past the area: clamp it.
    let x = (area.x + PREFIX_WIDTH + layout.cursor_col).min(area.x + area.width.saturating_sub(1));
    let y = area.y + (layout.cursor_row - first) as u16;
    (x, y)
}

/// Where each visible input row's text ends, as a column to erase from.
///
/// # Invariant
///
/// Every column named is one this frame drew as blank, so erasing there can only
/// take away ink the console wrote itself -- an IME composes into the console, on
/// this row, and a diff cannot see ink it did not put there. See
/// `docs/notes/tui-render.md`.
pub fn input_ends(s: &Screen, area: Rect) -> Vec<(u16, u16)> {
    let area = Rect {
        height: area.height.max(1),
        ..area
    };
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let lines = input_rows_of(s, &layout, area);
    lines
        .iter()
        .enumerate()
        .map(|(row, line)| {
            let used: usize = line
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            let used = used.min(area.width.saturating_sub(1) as usize);
            (area.y + row as u16, area.x + used as u16)
        })
        .collect()
}

fn draw_completion(f: &mut Frame, s: &Screen, input_area: Rect) {
    let Some(completion) = &s.completion else {
        return;
    };
    let shown = completion.candidates.len().min(8);
    // The window follows the selection, or a ninth candidate is highlighted off-screen.
    let start = if shown == 0 {
        0
    } else {
        completion.selected.saturating_sub(shown - 1)
    };
    let height = shown + 2; // borders
    let y = input_area.y.saturating_sub(height as u16);
    let width = 60.min(f.area().width);
    let area = Rect {
        x: input_area.x,
        y,
        width,
        height: height as u16,
    };
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = completion
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(|(i, c)| {
            let style = if i == completion.selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:<24}", c.insert), style),
                Span::styled(c.detail.clone(), dim()),
            ]))
        })
        .collect();

    // The title names the argument: a value list is never read as another list.
    let title = format!(
        " {} ",
        completion.subject.as_deref().unwrap_or("completions")
    );
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The ink of a structure rather than of a fact: a group's indent and its marker.
fn structure() -> Style {
    Style::default().fg(Color::Cyan)
}

/// A list row whose session has a window beside it: the list's own two looks.
fn bright() -> Style {
    Style::default().fg(Color::White)
}

/// The ink of each kind's mark: one glyph, and never the text beside it.
///
/// # Invariant
///
/// No two kinds look alike -- colour *and* modifier, because a colour that means
/// two things means neither. The one shared colour is deliberate: the harness's
/// own remarks and the model's thinking are both asides, and italic is what tells
/// the second from the first. See `docs/notes/tui-render.md`.
fn style_for(kind: LineKind) -> Style {
    match kind {
        // The harness about itself: the quietest thing on screen.
        LineKind::System => dim(),
        // What was asked for: magenta matches nothing else.
        LineKind::Command => Style::default().fg(Color::LightMagenta),
        // What the operator typed as prose: the other half of "asked for".
        LineKind::User => Style::default().fg(Color::LightCyan),
        // The answer.
        LineKind::Assistant => Style::default().fg(Color::White),
        // The model thinking: dark gray, italic. Not `Gray`: near-white here.
        LineKind::Reasoning => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        LineKind::ToolCall => Style::default().fg(Color::Yellow),
        LineKind::ToolOutput => Style::default().fg(Color::LightBlue),
        LineKind::ToolError => Style::default().fg(Color::LightRed),
        // A command's answer, and a failure, keep the two ends of the traffic
        // light that already mean "fine" and "look here".
        LineKind::Result => Style::default().fg(Color::Green),
        LineKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}
