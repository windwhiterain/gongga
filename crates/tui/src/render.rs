//! Rendering: header, scrollable log, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::log::View;
use crate::panes::{Pane, Panes};
use crate::{LineKind, Screen};
/// The whole keyboard, on one row: a key it does not name is a key nobody finds.
///
/// # Invariant
///
/// Stays under 100 columns, because a clipped help line loses its last entry --
/// which is the one that says how to leave.
/// Rows spent on the header and the sticky line; the input's height is asked
/// separately, and the line groups get what their sessions earn.
pub const CHROME_ROWS: u16 = 2;

/// The one row `cmd` and its result share, outside the panes.
pub const COMMAND_ROWS: u16 = 1;

/// The prompt on the first row; continuation rows pad behind it.
const PREFIX: &str = "> ";
const PREFIX_WIDTH: u16 = 2;

/// The most rows the input box grows to before it scrolls: prose, not a file.
const MAX_INPUT_ROWS: u16 = 8;

/// The narrowest the list is drawn: a mark and a name still have to read.
pub const LIST_MIN: u16 = 6;

pub fn draw(f: &mut Frame, s: &mut Screen) -> Rect {
    let area = f.area();
    let input = input_rows(s.input.text(), area.width, area.height);
    let chunks = areas(area, input, s.panes.chrome());

    draw_header(f, s, chunks[0]);
    // The body is one horizontal split: the session list, then the panes.
    let (list, panes) = body(chunks[1], &s.panes);
    draw_list(f, s, list);
    // The panes borrow the screen mutably: here each scroll offset gets clamped.
    draw_panes(f, s, panes);
    draw_lines(f, s, chunks[2]);
    draw_quota(f, s, chunks[3]);
    draw_command(f, s, chunks[4]);
    draw_input(f, s, chunks[5]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[5]);
    }
    // The input area this frame laid out: the caret asks here, not a second
    // computation of the box.
    chunks[5]
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
    /// SPEC's `[{cmd, cmd_result}]` row: the client's command window exists.
    pub command: bool,
    /// How many sessions are drawn as one line each.
    pub line_groups: usize,
    /// SPEC's `active_quota` row: a reading has been announced.
    pub quota: bool,
}

/// The six parts of SPEC's window, in the order they are drawn.
///
/// # Invariant
///
/// This is the only layout a frame has: the copy, the page height and the mouse
/// all ask here, so what an operator reads is what was on the screen. The body
/// is one horizontal split, and what is left of it after the line groups is the
/// panes' height.
pub fn areas(area: Rect, input_rows: u16, chrome: Chrome) -> Vec<Rect> {
    // One row per group, never more than half the window: the panes keep some.
    let lines = (chrome.line_groups as u16).min(area.height / 2);
    Layout::vertical([
        Constraint::Length(1),                                  // header
        Constraint::Min(1),                                     // sessions | panes
        Constraint::Length(lines),                              // one line per event
        Constraint::Length(if chrome.quota { 1 } else { 0 }),   // active_quota
        Constraint::Length(if chrome.command { 1 } else { 0 }), // cmd and its result
        Constraint::Length(input_rows),                         // the prompt
    ])
    .split(area)
    .to_vec()
}

/// The body split into the session list and the panes beside it.
///
/// # Invariant
///
/// The list is as wide as its own content -- the mark, the longest name, one
/// column of air -- so a short name is not padded out with empty screen. It never
/// takes more than half the window: the panes are where the work is read.
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

/// Columns the session list needs for its longest name, mark and air included.
pub fn list_width(panes: &Panes) -> u16 {
    let longest = panes
        .list_names()
        .iter()
        .map(|name| UnicodeWidthStr::width(name.as_str()))
        .max()
        .unwrap_or(0);
    // The mark on the left, the brackets around the name, and one column of air.
    (longest as u16 + 4).max(LIST_MIN)
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

/// Rows the panes get, with the sticky session taking what an even share leaves.
///
/// # Invariant
///
/// A pane that cannot get a row is not drawn, and `hidden` counts them: a pane
/// nobody can read is worse than one that says it is not on screen. A header takes
/// the rows its text wraps to, but never the pane's last one.
pub fn pane_slots(area: Rect, panes: &Panes, sticky: Option<&str>) -> (Vec<Slot>, usize) {
    // Only the sessions drawn as panes: the line groups and the command line
    // have areas of their own.
    let events: Vec<&Pane> = panes.of_kind(crate::panes::Kind::Full).collect();
    if events.is_empty() || area.width == 0 || area.height == 0 {
        return (Vec::new(), 0);
    }
    let n = events.len() as u16;
    // SPEC: the panes are a horizontal split, so the width is what is shared.
    let per = (area.width / n).max(1);
    let extra = area.width.saturating_sub(per * n);

    // Columns first: a header's own height needs the width it will be drawn at.
    let mut columns: Vec<(String, Rect)> = Vec::with_capacity(events.len());
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
        columns.push((
            pane.name.clone(),
            Rect {
                x,
                y: area.y,
                width,
                height: area.height,
            },
        ));
        x += width;
    }

    let mut slots: Vec<Slot> = Vec::with_capacity(columns.len());
    let drawn = columns.len();
    for (index, (name, column)) in columns.into_iter().enumerate() {
        let mut note = pane_note(panes, &name);
        // The last drawn pane says when the division hid one: a session whose
        // window is missing has to be explained somewhere.
        if index + 1 == drawn && hidden > 0 {
            note = Some(match note {
                Some(note) => format!("{note} · {hidden} not shown"),
                None => format!("{hidden} not shown"),
            });
        }
        // Framed, not coloured: the box is the mark, and its rows keep their kind.
        let frame = (Some(name.as_str()) == sticky && column.width > 2 && column.height > 2)
            .then_some(column);
        let inner = match frame {
            Some(frame) => Rect {
                x: frame.x + 1,
                y: frame.y + 1,
                width: frame.width - 2,
                height: frame.height - 2,
            },
            None => column,
        };
        let rows = match inner.height {
            0 | 1 => 0,
            height => (pane_header_rows(&name, note.as_deref(), inner.width).len() as u16)
                .min(height - 1)
                .max(1),
        };
        slots.push(Slot {
            name,
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
        });
    }
    (slots, hidden)
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

/// The session whose row in the list covers `row`, for a click.
///
/// # Invariant
///
/// It reads the same rows `draw_list` writes, so a click lands on the name the
/// operator read. See `docs/notes/tui-render.md`.
pub fn list_at(
    area: Rect,
    panes: &Panes,
    sticky: Option<&str>,
    column: u16,
    row: u16,
) -> Option<String> {
    if !covers(&area, column, row) {
        return None;
    }
    let names = panes.list_names();
    let (start, _end) = list_window(names.len(), area.height as usize, sticky, names);
    let index = start + (row - area.y) as usize;
    names.get(index).cloned()
}

/// The session whose line group covers `row`, for a click.
pub fn line_at(area: Rect, panes: &Panes, column: u16, row: u16) -> Option<String> {
    if !covers(&area, column, row) {
        return None;
    }
    let names: Vec<String> = panes
        .of_kind(crate::panes::Kind::Line)
        .map(|pane| pane.name.clone())
        .collect();
    if names.is_empty() {
        return None;
    }
    // The same share `draw_lines` gives each group.
    let per = (area.height as usize / names.len()).max(1) as u16;
    let index = ((row - area.y) / per) as usize;
    names.get(index).cloned()
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
fn covers(area: &Rect, column: u16, row: u16) -> bool {
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
    let spare = height.saturating_sub(CHROME_ROWS + COMMAND_ROWS + 3).max(1);
    rows.clamp(1, MAX_INPUT_ROWS.min(spare))
}

/// Columns for text: the prefix on the left, one spare on the right.
fn content_width(width: u16) -> usize {
    width.saturating_sub(PREFIX_WIDTH + 1).max(1) as usize
}

/// One row of the input as it will be drawn, plus where the cursor sits.
struct InputLayout {
    rows: Vec<String>,
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
    let mut row = String::new();
    let mut used = 0usize;
    let mut at_cursor: Option<(usize, u16)> = None;

    for (index, ch) in text.char_indices() {
        if at_cursor.is_none() && index == cursor {
            at_cursor = Some((rows.len(), used as u16));
        }
        if ch == '\n' {
            rows.push(std::mem::take(&mut row));
            used = 0;
            continue;
        }
        let advance = ch.width().unwrap_or(0);
        if used + advance > width && used > 0 {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(ch);
        used += advance;
    }
    rows.push(row);

    let (cursor_row, cursor_col) = at_cursor.unwrap_or((rows.len() - 1, used as u16));
    InputLayout {
        rows,
        cursor_row,
        cursor_col,
    }
}

fn draw_header(f: &mut Frame, s: &Screen, area: Rect) {
    // The bar spans the row: a coloured strip that stops where the text stops reads
    // as a selection, not as a header.
    let mut title = format!(" gongga · {} ", s.status);
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

/// The session list: SPEC's `render mode = none`, one row per session.
///
/// # Invariant
///
/// A name is drawn here or in the pane header or in a line group's row, never in
/// two of them, and every row of the list is painted the same way whatever that
/// session will be drawn as once it is clicked. See `docs/notes/tui-render.md`.
fn draw_list(f: &mut Frame, s: &Screen, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let height = area.height as usize;
    let names = s.panes.list_names();
    let sticky = s.sticky_session.as_deref();
    let (start, end) = list_window(names.len(), height, sticky, names);
    let rows: Vec<Line> = names
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|name| {
            // The mark is one column, so the names line up down the list.
            let here = Some(name.as_str()) == sticky;
            let marker = if here { "\u{25b8}" } else { " " };
            let style = name_style(here, dim());
            let fill = here.then_some(style);
            fill_row(
                vec![Span::styled(format!("{marker}{}", bracketed(name)), style)],
                area.width,
                fill,
            )
        })
        .collect();
    f.render_widget(Paragraph::new(rows), area);
}

/// How the sticky session is painted in every row that is filled for it.
///
/// # Invariant
///
/// One mark -- black on cyan -- across the *whole* row, in the list, in a line
/// group and on the way back to the newest row; `base` is the area's own look for
/// every other row. A pane in the split is framed instead, so its rows keep the
/// look of the kind of event they hold. See `docs/notes/tui-render.md`.
fn name_style(sticky: bool, base: Style) -> Style {
    if sticky {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        base
    }
}

/// The rows of the list a frame shows: the ones around the sticky session.
///
/// # Invariant
///
/// The frame and the click read the same window, so a click lands on the name the
/// operator saw. See `docs/notes/tui-render.md`.
fn list_window(
    total: usize,
    height: usize,
    sticky: Option<&str>,
    names: &[String],
) -> (usize, usize) {
    if total <= height {
        return (0, total);
    }
    // The only position the list has is the sticky session, so the row that
    // says where a message goes is never off screen.
    let at = sticky
        .and_then(|name| names.iter().position(|known| known == name))
        .unwrap_or(0);
    let start = at.saturating_sub(height / 2).min(total - height);
    (start, start + height)
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

/// The pane whose hint covers this cell, for a click.
///
/// # Invariant
///
/// Asked before the pane it is drawn on: the way back to the newest row must not
/// become a click on the session that holds it.
pub fn bottom_at(slots: &[Slot], panes: &Panes, column: u16, row: u16) -> Option<String> {
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

/// The line groups: one line per event, for the sessions drawn that way.
///
/// # Invariant
///
/// A row is one event, whatever its length: this area is a ticker of what each
/// session is doing, not a place to read a transcript. See
/// `docs/notes/tui-render.md`.
fn draw_lines(f: &mut Frame, s: &mut Screen, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let names: Vec<String> = s
        .panes
        .of_kind(crate::panes::Kind::Line)
        .map(|pane| pane.name.clone())
        .collect();
    if names.is_empty() {
        return;
    }
    // One row per session, name then newest event: not a pane, so no header and
    // nothing to scroll.
    let sticky = s.sticky_session.clone();
    let (slots, _) = group_slots(area, &names, sticky.as_deref());
    for slot in &slots {
        draw_headline(f, s, &slot.name, slot.body, sticky.as_deref());
    }
}

/// One session's row: its name, then its newest event.
fn draw_headline(f: &mut Frame, s: &mut Screen, name: &str, area: Rect, sticky: Option<&str>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let selected = Some(name) == sticky;
    let style = name_style(selected, Style::default().fg(Color::White));
    let label = bracketed(name);
    let mut spans = vec![Span::styled(format!("{label} "), style)];
    // Cut to what is left beside the name, so a row is one line and the name never
    // leaves the screen.
    let room = (area.width as usize)
        .saturating_sub(UnicodeWidthStr::width(label.as_str()) + 1)
        .max(1);
    if let Some((kind, text)) = s.panes.headline(View::Window(name), room) {
        // A selected row is one mark across its width: the event's kind is not
        // what an operator is looking for in it.
        let event = if selected { style } else { style_for(kind) };
        spans.push(Span::styled(text, event));
    }
    let row = fill_row(spans, area.width, selected.then_some(style));
    f.render_widget(Paragraph::new(row), area);
}

/// `spans` as one row, with `fill` painting the columns left of `width`.
///
/// # Invariant
///
/// A filled row paints every column of its area: a strip that stops where the text
/// stops reads as a highlighted word, not as the row that is selected.
fn fill_row(mut spans: Vec<Span<'static>>, width: u16, fill: Option<Style>) -> Line<'static> {
    let Some(fill) = fill else {
        return Line::from(spans);
    };
    let used: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let left = width as usize - used.min(width as usize);
    if left > 0 {
        spans.push(Span::styled(" ".repeat(left), fill));
    }
    Line::from(spans)
}

/// Where each line group goes: its header, then its rows.
///
/// # Invariant
///
/// The same arithmetic as [`pane_slots`], so a `line` session and a `full` one are
/// the same picture at different heights. A flat `height / groups` left the later
/// groups with rows the earlier ones had already used, which is how a group came to
/// be drawn as blank lines.
pub fn group_slots(area: Rect, names: &[String], sticky: Option<&str>) -> (Vec<Slot>, usize) {
    let n = names.len() as u16;
    if names.is_empty() || area.height == 0 {
        return (Vec::new(), 0);
    }
    let labels = if area.height >= n + n { n } else { 0 };
    let rows = area.height - labels;
    let per = (rows / n).max(1);
    let extra = rows.saturating_sub(per * n);

    let mut slots: Vec<Slot> = Vec::with_capacity(names.len());
    let mut y = area.y;
    let mut hidden = 0usize;
    for name in names {
        let head = if labels > 0 { 1 } else { 0 };
        let share = per + u16::from(Some(name.as_str()) == sticky) * extra;
        let left = area.bottom().saturating_sub(y + head);
        let height = share.min(left);
        if height == 0 {
            hidden += 1;
            continue;
        }
        slots.push(Slot {
            name: name.clone(),
            frame: None,
            header: (head > 0).then_some(Rect {
                x: area.x,
                y,
                width: area.width,
                height: head,
            }),
            body: Rect {
                x: area.x,
                y: y + head,
                width: area.width,
                height,
            },
            note: None,
        });
        y += head + height;
    }
    (slots, hidden)
}

/// A pane's header: the name, then whatever the title carries beside it.
///
/// # Invariant
///
/// The one place a pane's header is drawn, so a `full` session reads the same
/// whatever else the frame holds; `selected` is [`name_style`]'s fill, which the
/// caller turns off for a pane its frame already marks. See `docs/notes/tui-render.md`.
fn draw_group_header_note(
    f: &mut Frame,
    name: &str,
    area: Rect,
    selected: bool,
    note: Option<&str>,
) {
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

/// Draws every pane: the header that names it, then the rows that fit in its share.
fn draw_panes(f: &mut Frame, s: &mut Screen, area: Rect) {
    let sticky = s.sticky_session.clone();
    let (slots, _hidden) = pane_slots(area, &s.panes, sticky.as_deref());
    for slot in &slots {
        if let Some(frame) = slot.frame {
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
                frame,
            );
        }
        if let Some(header) = slot.header {
            let note = slot.note.as_deref();
            // A framed pane is marked by its box; the fill is the fallback.
            let filled = slot.frame.is_none() && Some(slot.name.as_str()) == sticky.as_deref();
            draw_group_header_note(f, &slot.name, header, filled, note);
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
    if area.height == 0 {
        return;
    }
    // One column short: a full-width row would otherwise be wrapped again.
    let width = area.width.saturating_sub(1).max(1) as usize;
    // The pane's row count: an offset cannot say a line arrived under it.
    let rows = s.panes.rows(View::Window(name), width);
    let Some(pane) = s.panes.get_mut(name) else {
        return;
    };
    // Before the window is read: a taller pane keeps the rows it was showing.
    pane.anchor(area.height as usize, rows);
    let scroll = pane.scroll;
    let name = pane.name.clone();
    let view = View::Window(&name);
    let window = s.panes.window(view, width, area.height as usize, scroll);
    let max_scroll = window.max_scroll;
    // Cloned: the rows a frame draws cannot outlive the log they came from.
    let lines: Vec<Line> = window
        .rows
        .iter()
        .map(|(kind, text)| Line::from(Span::styled(text.to_string(), style_for(*kind))))
        .collect();
    drop(window);
    if let Some(pane) = s.panes.get_mut(&name) {
        pane.max_scroll = max_scroll;
        // Clamped here too, for a pane that just grew or a log that just shrank.
        pane.scroll = scroll.min(max_scroll);
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
                Span::styled(s.placeholder.clone(), dim())
            } else {
                Span::raw(row.clone())
            };
            Line::from(vec![
                Span::styled(lead, Style::default().fg(Color::Cyan)),
                body,
            ])
        })
        .collect()
}

fn draw_input(f: &mut Frame, s: &Screen, area: Rect) {
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let lines = input_rows_of(s, &layout, area);
    // The caller anchors the caret after applying the frame; a position set here
    // would be applied by nobody.
    f.render_widget(Paragraph::new(lines), area);
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

/// The input area as `draw` lays it out, for a test that asks.
#[cfg(test)]
pub fn input_area(area: Rect, s: &Screen) -> Rect {
    areas(
        area,
        input_rows(s.input.text(), area.width, area.height),
        s.panes.chrome(),
    )[5]
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

    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" completions ")
                .style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The look of each kind of line.
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

/// Every kind, so a test can prove the palette has no two alike.
#[cfg(test)]
pub const KINDS: &[LineKind] = &[
    LineKind::System,
    LineKind::Command,
    LineKind::User,
    LineKind::Assistant,
    LineKind::Reasoning,
    LineKind::ToolCall,
    LineKind::ToolOutput,
    LineKind::ToolError,
    LineKind::Result,
    LineKind::Error,
];
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::{Layout, Listed, Panes};

    /// A copy must take exactly the rows the frame drew, however tall the input is.
    ///
    /// # Invariant
    ///
    /// The copy reads the same layout the renderer draws. A mismatch copies rows
    /// that were never on screen -- the direction that hands the operator the wrong
    /// text without saying so.
    #[test]
    fn the_copied_rows_are_the_rows_the_frame_drew() {
        // Thirty distinct lines, so where the window starts says how tall it is.
        let lines: Vec<String> = (0..30).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        for (height, text) in [(12u16, ""), (12, "a"), (12, "a\nb\nc"), (20, "a\nb")] {
            let mut screen = screen_with(&refs);
            screen.input.insert_str(text);
            let mut term = Terminal::new(TestBackend::new(30, height)).unwrap();
            term.draw(|f| {
                draw(f, &mut screen);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();

            let copied = screen.visible_rows(30, height);
            // The copy ends on the newest line, which is the last row of the panes.
            let top = parts(
                30,
                height,
                input_rows(&text.to_string(), 30, height),
                false,
                0,
            )[1]
            .y;
            let last = copied.len();
            let first = top + 1; // under the pane's own header row
            assert_eq!(
                pane_text(&buf, &screen, first + last as u16 - 1),
                "L29",
                "height {height}, input {text:?}: the copy is not the window"
            );
            assert_eq!(
                pane_text(&buf, &screen, first),
                copied[0],
                "height {height}, input {text:?}: the copy starts elsewhere"
            );
            assert_eq!(copied.last().unwrap(), "L29");
        }
    }

    /// A session drawn `line` gets the header a pane gets, laid out the same way.
    #[test]
    fn a_line_group_is_a_name_and_its_newest_event() {
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed: vec![
                Listed {
                    name: "main".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
                Listed {
                    name: "peer".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
            ],
            none: Vec::new(),
            full: vec!["main".to_string()],
            line: vec!["peer".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        screen.sticky_session = None;
        screen.panes.line("peer", LineKind::Assistant, "one");
        screen.panes.line("peer", LineKind::Assistant, "two");

        let (width, height) = (40u16, 12u16);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let parts = parts(width, height, 1, true, 1);
        let (slots, _) = group_slots(parts[2], &["peer".to_string()], None);
        assert_eq!(slots.len(), 1, "one group, one slot");
        // One row: the name, then its newest event. No header, no rows below.
        assert!(slots[0].header.is_none(), "{slots:?}");
        assert_eq!(slots[0].body.height, 1);
        assert_eq!(
            row_text(&buf, slots[0].body.y),
            "[peer] two",
            "the newest event, and the name in front of it"
        );
    }

    /// Two groups split the rows: neither gets rows the other already used.
    #[test]
    fn two_line_groups_share_the_rows_without_gaps() {
        let names = vec!["a".to_string(), "b".to_string()];
        let area = Rect::new(0, 0, 40, 10);
        let (slots, hidden) = group_slots(area, &names, None);
        assert_eq!(hidden, 0);
        assert_eq!(slots.len(), 2);
        // Back to back: the second starts where the first ends, header included.
        assert_eq!(slots[0].body.bottom(), slots[1].header.unwrap().y);
        let used = slots
            .iter()
            .map(|slot| slot.header.map(|h| h.height).unwrap_or(0) + slot.body.height)
            .sum::<u16>();
        assert_eq!(used, area.height, "every row is somebody's");
        assert!(slots.iter().all(|slot| slot.body.height > 0), "{slots:?}");
    }

    /// SPEC's list is `render mode = none`: what no other area draws, one name
    /// per row, every row painted the same way.
    ///
    /// # Invariant
    ///
    /// A session drawn `full` or `line` is not in the list, so no name appears
    /// twice on screen, and the rows of the list do not encode a render mode.
    #[test]
    fn the_list_is_the_sessions_no_other_area_draws() {
        let mut screen = screen_with(&[]);
        let listed = |name: &str, usage: &str| Listed {
            name: name.to_string(),
            status: "idle".to_string(),
            title: usage.to_string(),
        };
        screen.panes.layout(&Layout {
            listed: vec![
                listed("main", "ctx 900 · tps 12.0"),
                listed("peer", ""),
                listed("third", ""),
                listed("fourth", ""),
            ],
            none: vec!["third".to_string(), "fourth".to_string()],
            full: vec!["main".to_string()],
            line: vec!["peer".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let body = parts(60, 12, 1, true, 1)[1];
        let (list, panes) = crate::render::body(body, &screen.panes);
        // Two names, in the announcement's order, and nothing else.
        let rows: Vec<String> = (0..list.height)
            .map(|offset| read(&buf, list.x, list.width, list.y + offset))
            .collect();
        assert_eq!(&rows[..2], [" [third]", " [fourth]"], "{rows:?}");
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("main") || row.contains("peer")),
            "the names the pane and the line group carry stay out of the list: {rows:?}"
        );
        // Every name in the list has one look: no colour says how a session is drawn.
        let paints: Vec<Style> = (0..2)
            .map(|offset| buf[(list.x, list.y + offset)].style())
            .collect();
        assert_eq!(paints[0], paints[1], "one pattern down the list");
        assert!(panes.width > 0);
    }

    /// The sticky session's mark: a filled row where there is no frame, a box where
    /// the session has a pane of its own.
    ///
    /// # Invariant
    ///
    /// A filled row paints every column of its area -- a strip that stops at the
    /// text reads as a highlighted word -- and a framed pane paints none of its
    /// rows: the two marks are never on one session at once.
    #[test]
    fn the_sticky_session_is_a_filled_row_or_a_frame_but_never_both() {
        // A line group's row: the name, its newest event, and the fill across it.
        let mut screen = split_screen();
        screen.panes.line("third", LineKind::Assistant, "an event");
        screen.sticky_session = Some("third".to_string());
        let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let groups = parts(60, 14, 1, true, 1)[2];
        for column in [groups.x, groups.x + 1, groups.right() - 1] {
            let cell = buf[(column, groups.y)].clone();
            assert_eq!(cell.fg, Color::Black, "{cell:?} at column {column}");
            assert_eq!(cell.bg, Color::Cyan, "{cell:?} at column {column}");
        }

        // A name in the list: one row, filled to the list's own edge.
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed: vec![
                Listed {
                    name: "third".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
                Listed {
                    name: "fourth".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
            ],
            none: vec!["third".to_string(), "fourth".to_string()],
            full: Vec::new(),
            line: Vec::new(),
            command: "cmd".to_string(),
            quota: None,
        });
        screen.sticky_session = Some("third".to_string());
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let list = body(parts(40, 8, 1, true, 0)[1], &screen.panes).0;
        assert_eq!(read(&buf, list.x, list.width, list.y), "\u{25b8}[third]");
        let filled = buf[(list.right() - 1, list.y)].clone();
        assert_eq!(filled.bg, Color::Cyan, "the fill stops short: {filled:?}");
        let quiet = buf[(list.right() - 1, list.y + 1)].clone();
        assert_ne!(quiet.bg, Color::Cyan, "the row below is not the mark");

        // A session drawn `full` is framed: the box is its mark, and its rows are
        // painted like any other pane's.
        let mut screen = split_screen();
        let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let body = parts(60, 14, 1, true, 1)[1];
        let (_, panes) = crate::render::body(body, &screen.panes);
        let (slots, _) = pane_slots(panes, &screen.panes, screen.sticky_session.as_deref());
        let frame = slots[0].frame.expect("a frame on the sticky column");
        for (x, y) in [
            (frame.x, frame.y),
            (frame.right() - 1, frame.y),
            (frame.x, frame.bottom() - 1),
            (frame.right() - 1, frame.bottom() - 1),
        ] {
            let cell = buf[(x, y)].clone();
            assert_eq!(cell.fg, Color::Cyan, "the box at {x},{y}: {cell:?}");
        }
        let head = slots[0].header.expect("a header inside the frame");
        let cell = buf[(head.x + 1, head.y)].clone();
        assert_ne!(
            cell.bg,
            Color::Cyan,
            "a framed pane paints no row: {cell:?}"
        );
        // And one session is sticky, not every session: the pane beside it is plain.
        let peer = slots[1].header.expect("the other header");
        let cell = buf[(peer.x + 1, peer.y)].clone();
        assert_ne!(cell.bg, Color::Cyan, "two marks on two sessions: {cell:?}");
    }

    /// The list shows the rows around the sticky session -- the only position.
    #[test]
    fn the_list_window_follows_the_sticky_session() {
        let names: Vec<String> = (0..12).map(|i| format!("s{i}")).collect();
        let listed: Vec<Listed> = names
            .iter()
            .map(|name| Listed {
                name: name.clone(),
                status: "idle".to_string(),
                title: String::new(),
            })
            .collect();
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed,
            none: names.clone(),
            full: Vec::new(),
            line: Vec::new(),
            command: "cmd".to_string(),
            quota: None,
        });
        let (width, height) = (40u16, 8u16);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        let list = {
            let parts = parts(width, height, 1, true, 0);
            body(parts[1], &screen.panes).0
        };
        let drawn = |screen: &mut Screen, term: &mut Terminal<TestBackend>| {
            term.draw(|f| {
                draw(f, screen);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            (0..list.height)
                .map(|offset| row_text(&buf, list.y + offset))
                .collect::<Vec<String>>()
        };

        // No sticky session: the window starts at the top.
        let rows = drawn(&mut screen, &mut term);
        assert!(rows[0].contains("s0"), "{rows:?}");

        // The sticky session moves the window, and a click reads the drawn row.
        screen.sticky_session = Some("s9".to_string());
        let rows = drawn(&mut screen, &mut term);
        assert!(
            rows.iter().any(|row| row.contains("s9")),
            "the sticky session is on screen: {rows:?}"
        );
        let at = rows
            .iter()
            .position(|row| row.trim().trim_start_matches('\u{25b8}').trim() == "[s9]")
            .unwrap_or_else(|| panic!("s9 is not drawn: {rows:?}"));
        assert_eq!(
            list_at(
                list,
                &screen.panes,
                screen.sticky_session.as_deref(),
                list.x,
                list.y + at as u16
            ),
            Some("s9".to_string()),
            "the drawn row and the click name the same session"
        );

        // And it never runs off either end.
        screen.sticky_session = Some("s11".to_string());
        let rows = drawn(&mut screen, &mut term);
        assert!(rows.iter().any(|row| row.contains("s11")), "{rows:?}");
        screen.sticky_session = Some("nobody".to_string());
        let rows = drawn(&mut screen, &mut term);
        assert!(
            rows[0].contains("s0"),
            "an unknown name pins the top: {rows:?}"
        );
    }

    /// The window arithmetic: it holds the row it is asked for, clamped.
    #[test]
    fn list_window_holds_the_row_it_is_asked_for() {
        let names: Vec<String> = (0..12).map(|i| format!("s{i}")).collect();
        let at = |want: Option<&str>| list_window(12, 8, want, &names);
        assert_eq!(at(None), (0, 8), "nothing named: from the top");
        assert_eq!(at(Some("s0")), (0, 8));
        assert_eq!(at(Some("s11")), (4, 12), "the end never runs off");
        assert_eq!(at(Some("nobody")), (0, 8), "an unknown name pins the top");
        assert_eq!(
            list_window(3, 8, Some("s1"), &names),
            (0, 3),
            "fewer names than rows: no window to move"
        );
    }

    /// No two kinds look alike: one that means two things means neither.
    #[test]
    fn no_two_kinds_of_line_look_alike() {
        let mut seen: Vec<(Option<Color>, Modifier)> = Vec::new();
        for kind in KINDS {
            let style = style_for(*kind);
            let fg = style.fg;
            assert!(fg.is_some(), "{kind:?} has no colour of its own");
            let pair = (fg, style.add_modifier);
            assert!(
                !seen.contains(&pair),
                "{kind:?} looks exactly like another kind: {pair:?}"
            );
            seen.push(pair);
        }
        assert_eq!(seen.len(), KINDS.len(), "every kind was checked");
    }

    /// The caret sits on the input row, and a message that wraps keeps it inside
    /// the box it is being typed in.
    #[test]
    fn the_caret_stays_on_the_input_row() {
        let mut screen = screen_with(&["a"]);
        let area = Rect::new(0, 0, 30, 12);
        let input = input_area(area, &screen);
        assert_eq!(input.height, 1);
        assert_eq!(caret(&screen, input), (2, input.y), "an empty prompt");

        screen.input.insert_str("hello");
        assert_eq!(caret(&screen, input), (7, input.y), "two columns of prefix");

        // A line longer than the box wraps, and the caret goes with it.
        screen.input.insert_str(&"x".repeat(3 * content_width(30)));
        let input = input_area(area, &screen);
        assert!(input.height > 1, "the box grew: {input:?}");
        let (x, y) = caret(&screen, input);
        assert!(y >= input.y && y < input.bottom(), "caret off the box: {y}");
        assert!(x < input.right(), "caret off the right edge: {x}");
    }

    /// The erase after typing starts where each row's text ends, so it can only
    /// take away ink this frame did not draw.
    #[test]
    fn the_erase_starts_after_the_input_text() {
        let mut screen = screen_with(&[]);
        // Tall enough for the box to grow a row, so both rows are in the frame.
        let area = Rect::new(0, 0, 40, 30);
        let input = input_area(area, &screen);
        assert_eq!(
            input_ends(&screen, input),
            vec![(input.y, 2)],
            "just the prompt"
        );

        screen.input.insert_str("hi");
        // Two columns of prompt, then the text: everything past it was drawn blank.
        assert_eq!(input_ends(&screen, input), vec![(input.y, 4)]);

        // A message with a break has a row of its own, each ending where its text does.
        screen.input.insert_str("\nbye");
        let input = input_area(area, &screen);
        assert_eq!(input.height, 2, "the box grew: {input:?}");
        let ends = input_ends(&screen, input);
        assert_eq!(ends.len(), 2, "{ends:?}");
        assert_eq!(ends[0], (input.y, 4));
        assert_eq!(
            ends[1],
            (input.y + 1, 5),
            "the continuation pad and its text"
        );
    }

    /// The text drawn on row `y`, trailing blanks removed.
    fn row_text(buf: &Buffer, y: u16) -> String {
        let mut out = String::new();
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.trim_end().to_string()
    }

    /// The same row, read from the panes: the session list takes the columns on
    /// the left, and the panes start where it ends.
    fn pane_text(buf: &Buffer, panes: &Screen, y: u16) -> String {
        let (_, area) = body(buf.area, &panes.panes);
        read(buf, area.x, area.width, y)
    }

    /// One pane's column of a row, found through the layout the frame drew.
    ///
    /// # Invariant
    ///
    /// The panes are a horizontal split, so a test that reads a session's text has
    /// to read its own columns -- a row of the window holds several sessions.
    fn slot_text(buf: &Buffer, screen: &Screen, name: &str, row: u16) -> String {
        let (_, panes) = body(buf.area, &screen.panes);
        let (slots, _) = pane_slots(panes, &screen.panes, screen.sticky_session.as_deref());
        let slot = slots
            .iter()
            .find(|slot| slot.name == name)
            .unwrap_or_else(|| panic!("no pane named {name:?}: {slots:?}"));
        read(buf, slot.body.x, slot.body.width, row)
    }

    /// `width` columns from `x`, trailing blanks removed.
    fn read(buf: &Buffer, x: u16, width: u16, y: u16) -> String {
        let mut out = String::new();
        for col in x..x + width {
            out.push_str(buf[(col, y)].symbol());
        }
        out.trim_end().to_string()
    }

    /// Where a frame puts the parts a test looks at, for this size.
    fn parts(width: u16, height: u16, input: u16, command: bool, lines: usize) -> Vec<Rect> {
        areas(
            Rect::new(0, 0, width, height),
            input,
            Chrome {
                command,
                line_groups: lines,
                quota: false,
            },
        )
    }

    /// The same, with SPEC's `active_quota` row present: one more row between the
    /// command line and the prompt.
    fn parts_with_quota(width: u16, height: u16, input: u16) -> Vec<Rect> {
        areas(
            Rect::new(0, 0, width, height),
            input,
            Chrome {
                command: true,
                line_groups: 0,
                quota: true,
            },
        )
    }

    /// A screen whose one pane reads the unnamed window these lines go to.
    fn screen_with(lines: &[&str]) -> Screen {
        let mut panes = Panes::new();
        panes.layout(&Layout {
            listed: Vec::new(),
            none: Vec::new(),
            full: vec![String::new()],
            line: Vec::new(),
            command: String::new(),
            quota: None,
        });
        for line in lines {
            panes.line("", LineKind::System, *line);
        }
        Screen {
            panes,
            completion: None,
            input: crate::InputLine::default(),
            status: String::new(),
            status_kind: LineKind::System,
            placeholder: String::new(),
            sticky_session: None,
        }
    }

    /// A window as SPEC lays it out: two sessions as panes, one as lines, the
    /// command line, and the list they all came from.
    fn split_screen() -> Screen {
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed: vec![
                Listed {
                    name: "main".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
                Listed {
                    name: "peer".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
                Listed {
                    name: "third".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                },
            ],
            none: Vec::new(),
            full: vec!["main".to_string(), "peer".to_string()],
            line: vec!["third".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        screen.sticky_session = Some("main".to_string());
        screen
    }

    /// Two sessions streaming at once, drawn: both blocks are on screen, in
    /// the order they opened.
    #[test]
    fn a_frame_shows_two_streaming_blocks_side_by_side() {
        let mut screen = screen_with(&[]);
        screen.panes.stream(
            "",
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "one",
            false,
        );
        screen.panes.stream(
            "",
            "r:test1:1:Output",
            LineKind::Assistant,
            "[test1] ",
            "two",
            true,
        );
        screen.panes.stream(
            "",
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "three",
            false,
        );

        // Wide enough that neither block wraps: each is one row.
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // Under the header the column draws for its session.
        let top = parts(60, 10, 1, false, 0)[1].y + 1;
        assert_eq!(pane_text(&buf, &screen, top), "[peer] onethree");
        assert_eq!(pane_text(&buf, &screen, top + 1), "[test1] two");
        // One column, one width: the two blocks share the rows they are drawn in.
    }

    /// The block size, drawn: it changes how many rows one turn takes.
    #[test]
    fn the_block_size_changes_what_a_frame_shows() {
        // Long enough to wrap at this width, so the cap has something to cut.
        let thinking = "abcdefghij klmnopqrst uvwxyz abcdefghi jklmnop";
        let frame = |lines: usize| {
            let mut screen = screen_with(&[]);
            screen.panes.set_block_size(lines);
            screen.panes.stream(
                "",
                "r:main:1:Reasoning",
                LineKind::Reasoning,
                "[main] thinking: ",
                thinking,
                true,
            );
            screen.panes.stream(
                "",
                "r:main:1:ToolCall",
                LineKind::ToolCall,
                "[main] tool: ",
                "bash{ls}",
                true,
            );
            screen.panes.stream(
                "",
                "r:main:1:Output",
                LineKind::Assistant,
                "[main] ",
                "a\nb",
                true,
            );
            let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
            term.draw(|f| {
                draw(f, &mut screen);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            // Under the header the column draws for its session.
            let body = parts(40, 12, 1, false, 0)[1];
            (0..body.height - 1)
                .map(|row| pane_text(&buf, &screen, body.y + 1 + row))
                .collect::<Vec<_>>()
        };

        // Uncapped: the thinking block wraps, and every row of it is on screen.
        let all = frame(0);
        assert!(all[0].contains("thinking: abc"), "{all:?}");
        // Every row the uncapped blocks need is drawn, and what is left is blank.
        assert!(all.len() >= 5, "{all:?}");
        assert!(
            all.iter().any(|r| r.contains("thinking: ")),
            "{all:?} the answer's first block is on screen"
        );

        // Capped at one row: the same block shows the row it is writing now, and
        // the label comes with it.
        let one = frame(1);
        assert!(one[0].contains("thinking:"), "{one:?}");
        assert!(
            one[0].contains("pqrst"),
            "{one:?} the tail is what a cap keeps"
        );
        // The answer is the block no cap touches: both of its rows are still there.
        assert!(
            one.iter().any(|row| row.ends_with("] a")),
            "{one:?} the answer keeps both rows"
        );
        assert!(one.contains(&"b".to_string()), "{one:?}");
        let shown = |frame: &Vec<String>| frame.iter().filter(|r| !r.is_empty()).count();
        assert!(
            shown(&one) < shown(&all),
            "the cap takes rows off the frame: {one:?} against {all:?}"
        );
    }

    /// A split area: each session its own header, rows and scroll.
    #[test]
    fn a_split_area_names_every_session_and_lets_each_scroll_alone() {
        let mut screen = split_screen();
        screen.panes.stream(
            "main",
            "r:main:1:Output",
            LineKind::Assistant,
            "",
            "the answer",
            true,
        );
        screen.panes.stream(
            "peer",
            "r:peer:1:Output",
            LineKind::Assistant,
            "",
            "another answer",
            true,
        );
        screen.panes.line("cmd", LineKind::System, "$ get_messages");

        // 41 columns: an odd width, so the split leaves a column for the sticky pane.
        let mut term = Terminal::new(TestBackend::new(41, 12)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();

        // Each session is its own column: a header row, then its own rows under it.
        let body = parts(41, 12, 1, true, 1)[1];
        let (_, panes) = crate::render::body(body, &screen.panes);
        let (slots, hidden) = pane_slots(panes, &screen.panes, screen.sticky_session.as_deref());
        assert_eq!(hidden, 0);
        assert_eq!(slots.len(), 2, "two sessions drawn full: {slots:?}");
        // Side by side, in the list's order, and the sticky one takes the leftover
        // columns because the operator is reading it.
        assert_eq!(slots[0].name, "main");
        assert_eq!(slots[1].name, "peer");
        // The framed pane is the sticky one; its rows start one column in.
        let frame = slots[0].frame.expect("a frame on the sticky column");
        assert_eq!(frame.x, panes.x);
        assert_eq!(slots[0].body.x, frame.x + 1, "{slots:?}");
        assert_eq!(slots[0].body.width, frame.width - 2, "{slots:?}");
        assert_eq!(slots[1].body.x, frame.right());
        assert!(slots[1].frame.is_none(), "one frame, on the sticky pane");
        assert!(
            frame.width > slots[1].body.width,
            "the sticky column is the wider one: {slots:?}"
        );
        let head = slots[0].header.expect("a header on the column");
        assert_eq!(read(&buf, head.x, head.width, head.y), " [main]");
        assert_eq!(
            slot_text(&buf, &screen, "main", slots[0].body.y),
            "the answer"
        );
        assert_eq!(
            slot_text(&buf, &screen, "peer", slots[1].body.y),
            "another answer"
        );

        // The command line sits above the input, which is the last row.
        let parts = parts(40, 12, 1, true, 1);
        assert_eq!(row_text(&buf, parts[4].y), "$ get_messages");

        // Scrolling the sticky session moves only that pane: the other keeps its newest.
        screen.scroll_by(1);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(
            slot_text(&buf, &screen, "peer", slots[1].body.y),
            "another answer",
            "the peer pane moved with the sticky session"
        );
        assert_eq!(
            screen.panes.get("cmd").unwrap().max_scroll,
            0,
            "one row of a command line cannot scroll"
        );
    }

    /// A row names the pane drawn there; side by side, the column decides.
    #[test]
    fn a_row_names_the_pane_it_falls_in() {
        let screen = split_screen();
        let body = parts(40, 12, 1, true, 1)[1];
        let (_, panes) = crate::render::body(body, &screen.panes);
        let (slots, _) = pane_slots(panes, &screen.panes, screen.sticky_session.as_deref());
        let main = slots.iter().find(|s| s.name == "main").expect("main");
        let peer = slots.iter().find(|s| s.name == "peer").expect("peer");
        // Same row, different columns: that is what picks a pane now.
        assert_eq!(
            screen
                .pane_at_row(40, 12, main.body.x, main.body.y)
                .as_deref(),
            Some("main")
        );
        assert_eq!(
            screen
                .pane_at_row(40, 12, peer.body.x, peer.body.y)
                .as_deref(),
            Some("peer"),
            "the same row, one column to the right"
        );
        // A click on the header row names the pane it is the header of.
        let head = peer.header.expect("a header per column");
        assert_eq!(
            screen.pane_at_row(40, 12, head.x, head.y).as_deref(),
            Some("peer")
        );
        // A column nobody draws is nobody's, and neither is the chrome above.
        assert_eq!(screen.pane_at_row(40, 12, 0, 0), None);
    }

    /// SPEC's `active_quota` row: under the command line, and absent until a
    /// reading arrives.
    ///
    /// # Invariant
    ///
    /// The row costs nothing when there is nothing to say, and once it exists it
    /// sits between the command line and the input, in the order SPEC names.
    #[test]
    fn the_quota_row_sits_above_the_command_line() {
        let mut screen = screen_with(&[]);
        screen
            .panes
            .line("cmd", LineKind::Command, "$ get_messages");
        let (width, height) = (40u16, 12u16);
        // No reading: no row, so the prompt sits right under the command line.
        let without = parts(width, height, 1, true, 0);
        assert_eq!(without.len(), 6);
        assert_eq!(without[3].height, 0, "no quota row");
        assert_eq!(without[5].y, without[4].y + 1, "the prompt follows it");

        screen.panes.layout(&Layout {
            listed: Vec::new(),
            none: Vec::new(),
            full: vec![String::new()],
            line: Vec::new(),
            command: "cmd".to_string(),
            quota: Some(crate::QuotaRow {
                text: "deepseek-official · money ¥215.52".to_string(),
                kind: LineKind::System,
            }),
        });
        // Which row is above which is SPEC's order: quota, then cmd, then input.
        let with = parts_with_quota(width, height, 1);
        assert_eq!(with[3].height, 1, "the quota row");
        assert_eq!(with[4].y, with[3].y + 1, "the command line follows it");
        assert_eq!(with[5].y, with[4].y + 1, "and the prompt follows that");

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(
            row_text(&buf, with[3].y),
            "deepseek-official · money ¥215.52"
        );
        assert_eq!(row_text(&buf, with[4].y), "$ get_messages");
        assert!(
            row_text(&buf, with[5].y).starts_with('>'),
            "the prompt is still the last row: {:?}",
            row_text(&buf, with[5].y)
        );
    }

    /// A reading that failed is painted as a failure, in the same row.
    #[test]
    fn an_unreadable_quota_row_is_drawn_as_an_error() {
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed: Vec::new(),
            none: Vec::new(),
            full: vec![String::new()],
            line: Vec::new(),
            command: "cmd".to_string(),
            quota: Some(crate::QuotaRow {
                text: "minimax-cn · no credential: set MINIMAX_CN_API_KEY".to_string(),
                kind: LineKind::Error,
            }),
        });
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let parts = parts_with_quota(60, 12, 1);
        let cell = buf[(parts[3].x, parts[3].y)].clone();
        assert_eq!(
            cell.fg,
            style_for(LineKind::Error).fg.unwrap_or(Color::Reset),
            "{cell:?}"
        );
        assert_eq!(
            row_text(&buf, parts[3].y),
            "minimax-cn · no credential: set MINIMAX_CN_API_KEY"
        );
    }

    /// SPEC's `full` pane title: the context, the rate and the first-token delay.
    ///
    /// # Invariant
    ///
    /// The three go in the title of a `full` pane and nowhere else, and a session
    /// that has measured nothing carries no trace of them.
    #[test]
    fn a_full_panes_title_carries_what_the_last_request_measured() {
        let mut screen = split_screen();
        let listed = |name: &str, usage: &str| Listed {
            name: name.to_string(),
            status: "idle".to_string(),
            title: usage.to_string(),
        };
        screen.panes.layout(&Layout {
            listed: vec![
                listed("main", "ctx 12345 · cached 89% · tps 41.6 · ttft 820ms"),
                listed("peer", ""),
                listed("third", "ctx 99 · tps 9.0"),
            ],
            none: Vec::new(),
            full: vec!["main".to_string(), "peer".to_string()],
            line: vec!["third".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        // Wide enough that a whole title fits its pane's column.
        let mut term = Terminal::new(TestBackend::new(130, 12)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let body = parts(130, 12, 1, true, 1)[1];
        let (_, panes) = crate::render::body(body, &screen.panes);
        let (slots, _) = pane_slots(panes, &screen.panes, screen.sticky_session.as_deref());
        let head = slots[0].header.expect("a header on the column");
        assert_eq!(
            read(&buf, head.x, head.width, head.y),
            " [main] ctx 12345 · cached 89% · tps 41.6 · ttft 820ms"
        );
        let head = slots[1].header.expect("a header on the column");
        assert_eq!(read(&buf, head.x, head.width, head.y), " [peer]");

        // SPEC gives them to a `full` pane: a `line` group's row is one event,
        // so the numbers stop at the pane.
        let groups = parts(130, 12, 1, true, 1)[2];
        assert_eq!(
            read(&buf, groups.x, groups.width, groups.y),
            "[third]",
            "one row per group: the name, and no measurement"
        );
    }

    /// A title too wide for its column takes the rows it needs, and never the
    /// pane's last one.
    ///
    /// # Invariant
    ///
    /// The header grows downwards, so the rows below it start lower; a pane keeps
    /// at least one row of its own whatever the title says.
    #[test]
    fn a_long_pane_title_wraps_instead_of_being_cut() {
        let mut screen = screen_with(&[]);
        let title = "ctx 123456 · cached 92% · tps 41.6 · ttft 820ms · cwd C:/resource/gongga · \
                     provider deepseek-official · thinking high · ctx_limit 70%";
        screen.panes.layout(&Layout {
            listed: vec![Listed {
                name: "main".to_string(),
                status: "idle".to_string(),
                title: title.to_string(),
            }],
            none: Vec::new(),
            full: vec!["main".to_string()],
            line: Vec::new(),
            command: String::new(),
            quota: None,
        });
        let (_, area) = crate::render::body(parts(40, 20, 1, false, 0)[1], &screen.panes);
        let (slots, _) = pane_slots(area, &screen.panes, None);
        let header = slots[0].header.expect("a header");
        assert!(
            header.height > 1,
            "a title this long needs more than one row: {header:?}"
        );
        assert_eq!(slots[0].body.y, header.bottom(), "the rows start under it");

        // Drawn: every word is on screen, so nothing was cut.
        let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let drawn: String = (0..header.height)
            .map(|row| read(&buf, header.x, header.width, header.y + row))
            .collect::<Vec<_>>()
            .join(" ");
        for word in ["[main]", "cached", "provider", "deepseek-official", "70%"] {
            assert!(drawn.contains(word), "{word:?} is on screen: {drawn:?}");
        }

        // A pane one row tall has nowhere for a title: it keeps its row and the
        // title is not drawn at all.
        let (slots, _) = pane_slots(Rect::new(0, 0, 40, 1), &screen.panes, None);
        assert!(slots[0].header.is_none(), "{slots:?}");
        assert_eq!(slots[0].body.height, 1);
    }

    /// A layout says what is drawn; where each view is scrolled is not its business.
    #[test]
    fn a_relayout_leaves_every_pane_where_it_was_scrolled() {
        let mut screen = split_screen();
        let pane = screen.panes.get_mut("main").expect("main");
        pane.max_scroll = 5;
        screen.panes.scroll_by("main", 3);
        // The next step's measurement arrives as a layout, as does a status change.
        screen.panes.layout(&Layout {
            listed: vec![Listed {
                name: "main".to_string(),
                status: "running".to_string(),
                title: "ctx 900 · tps 12.0".to_string(),
            }],
            none: Vec::new(),
            full: vec!["main".to_string(), "peer".to_string()],
            line: vec!["third".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        assert_eq!(
            screen.panes.get("main").expect("main").scroll,
            3,
            "a title that changes every step must not read as \"go to the bottom\""
        );
    }

    /// Layout, window and the rows the bottom of the log shows, without a terminal.
    #[test]
    fn the_log_window_shows_the_newest_rows_above_the_prompt() {
        let lines: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut screen = screen_with(&refs);
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

        // The body gives one row of its height to the header the column draws.
        let buf = term.backend().buffer().clone();
        let body = parts(40, 10, 1, false, 0)[1];
        let rows = body.height - 1;
        let shown: Vec<String> = (0..rows)
            .map(|offset| pane_text(&buf, &screen, body.y + 1 + offset))
            .collect();
        assert_eq!(shown.last().map(String::as_str), Some("L9"), "{shown:?}");
        assert_eq!(
            screen.panes.get("").unwrap().max_scroll,
            10 - rows as usize,
            "10 lines into the rows the pane has"
        );
    }

    /// One row up is one row, not one line: a window can start mid-line.
    #[test]
    fn scrolling_moves_the_window_by_rows() {
        let mut screen = screen_with(&["aaaaaaa", "bbbbbbb", "ccccccc"]);
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        // Three lines in a pane this wide: nothing scrolls until the text is
        // longer than the window.
        let body = parts(40, 8, 1, false, 0)[1];
        let rows = screen.panes.rows(View::Window(""), 8);
        assert_eq!(
            screen.panes.get("").unwrap().max_scroll,
            rows.saturating_sub(body.height as usize)
        );

        screen.scroll_by(1);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..body.height)
            .map(|offset| pane_text(&buf, &screen, body.y + offset))
            .collect();
        assert!(
            rows.iter().any(|row| row.starts_with("aaaaaaa")),
            "the row above the window: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.chars().count() <= 8),
            "the panes start where the list ends: {rows:?}"
        );
    }

    /// A taller window keeps the rows a scrolled view was reading.
    ///
    /// # Invariant
    ///
    /// Scroll is measured from the newest row, so a taller pane would otherwise show
    /// *older* rows above the same bottom: the paragraph the operator was reading
    /// slides down the screen. A view away from the bottom keeps its top row, and a
    /// view at the bottom stays at the bottom, where growing is what shows history.
    #[test]
    fn a_taller_window_keeps_what_a_scrolled_view_was_reading() {
        let lines: Vec<String> = (0..40).map(|i| format!("L{i:02}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut screen = screen_with(&refs);
        // The rows a frame shows, below the pane's own header row.
        fn shown(buf: &Buffer, area: Rect) -> Vec<String> {
            // The list column is on the left; a pane's text starts where it ends.
            (0..area.height - 1)
                .map(|offset| row_text(buf, area.y + 1 + offset).trim().to_string())
                .collect()
        }

        let mut term = Terminal::new(TestBackend::new(20, 8)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let short = parts(20, 8, 1, false, 0)[1];
        let bottom = shown(&term.backend().buffer().clone(), short);
        assert_eq!(bottom.last().map(String::as_str), Some("L39"));

        // Scrolled into the history: the top row is what is being read. This pane
        // has no sticky session, so scroll it by name.
        screen.panes.scroll_by("", 5);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let top = shown(&term.backend().buffer().clone(), short)[0].clone();
        assert_eq!(top, "L30", "five rows up from the bottom of 40 rows");

        // The same window, taller: the top row stays, the new rows land under it.
        let mut tall = Terminal::new(TestBackend::new(20, 12)).unwrap();
        tall.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let grown = parts(20, 12, 1, false, 0)[1];
        let after = shown(&tall.backend().buffer().clone(), grown);
        assert_eq!(after[0], top, "the row that was at the top is still there");
        assert_eq!(after.len(), bottom.len() + 4, "four more rows are drawn");

        // Back at the newest row, a taller window shows history above it instead.
        screen.panes.pin("");
        tall.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let after = shown(&tall.backend().buffer().clone(), grown);
        assert_eq!(
            after.last().map(String::as_str),
            Some("L39"),
            "the newest row is still the last row"
        );
    }

    /// A view scrolled off the bottom holds its top row when a line arrives.
    ///
    /// # Invariant
    ///
    /// SPEC: a view scrolled from the bottom does not follow the log. The offset is
    /// counted from the newest row, so a row that arrived under it has to move the
    /// window with it -- otherwise the paragraph slides up and out of sight.
    #[test]
    fn a_scrolled_view_holds_its_top_row_when_a_line_arrives() {
        let lines: Vec<String> = (0..40).map(|i| format!("L{i:02}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut screen = screen_with(&refs);
        let mut term = Terminal::new(TestBackend::new(20, 8)).unwrap();
        // One frame first: the scroll bound is what the renderer writes back.
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let area = parts(20, 8, 1, false, 0)[1];
        // The pane's first row is its header, so its text starts one below.
        fn top(buf: &Buffer, screen: &Screen, area: Rect) -> String {
            pane_text(buf, screen, area.y + 1)
        }

        screen.panes.scroll_by("", 5);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let reading = top(&term.backend().buffer().clone(), &screen, area);
        assert_eq!(reading, "L30", "five rows up from the bottom of 40 rows");

        // Two rows arrive under the window: it stays on what is being read.
        screen.panes.line("", LineKind::System, "L40");
        screen.panes.line("", LineKind::System, "L41");
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        assert_eq!(
            top(&term.backend().buffer().clone(), &screen, area),
            reading,
            "the view followed the log instead of holding its top row"
        );

        // Shift+Down is what brings it back, and then it follows again.
        screen.panes.pin("");
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let drawn = term.backend().buffer().clone();
        let rows = (0..area.height)
            .map(|offset| pane_text(&drawn, &screen, area.y + offset))
            .collect::<Vec<String>>();
        assert_eq!(rows.last().map(String::as_str), Some("L41"), "{rows:?}");
    }

    /// A long message wraps inside the box: text that cannot be seen cannot be edited.
    #[test]
    fn a_long_input_wraps_into_more_rows_and_takes_them_from_the_log() {
        let mut screen = screen_with(&["L0", "L1", "L2", "L3", "L4"]);
        screen.input.insert_str("1234567890");
        // 10 wide: a two-column prompt and one column spared, so seven of text.
        assert_eq!(input_rows("1234567890", 10, 10), 2);
        let mut term = Terminal::new(TestBackend::new(10, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

        let buf = term.backend().buffer().clone();
        let input = parts(10, 10, 2, false, 0)[5];
        assert_eq!(row_text(&buf, input.y), "> 1234567");
        assert_eq!(row_text(&buf, input.y + 1), "  890");
    }

    /// A typed newline is a hard break, even where the width would have kept going.
    #[test]
    fn a_typed_newline_starts_a_new_row() {
        let mut screen = screen_with(&[]);
        screen.input.insert_str("one\ntwo");
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

        assert_eq!(input_rows("one\ntwo", 20, 10), 2);
        let buf = term.backend().buffer().clone();
        let input = parts(20, 10, 2, false, 0)[5];
        assert_eq!(row_text(&buf, input.y), "> one");
        assert_eq!(row_text(&buf, input.y + 1), "  two");
    }

    /// Past its cap the box scrolls instead of growing, so the answer stays visible.
    #[test]
    fn the_input_box_stops_growing_and_scrolls_instead() {
        let many = "x\n".repeat(40);
        assert_eq!(input_rows(&many, 40, 30), MAX_INPUT_ROWS);
        // A three-row log is the floor the input may not take from.
        assert_eq!(input_rows(&many, 40, 6), 1);
        // `layout_input` follows the cursor, so the last row is the one shown.
        let layout = layout_input(&many, many.len(), 38);
        assert_eq!(layout.cursor_row, layout.rows.len() - 1);
        assert_eq!(layout.cursor_col, 0);
    }
}
