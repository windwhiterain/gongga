//! Rendering: header, scrollable log, footer, completion popup, input line.

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

/// How many events one `line` group shows under its header.
pub const LINE_ROWS: u16 = 3;

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
    let chunks = areas(
        area,
        input,
        s.panes.command().is_some(),
        s.panes.of_kind(crate::panes::Kind::Line).count(),
    );

    draw_header(f, s, chunks[0]);
    // The body is one horizontal split: the session list, then the panes.
    let (list, panes) = body(chunks[1], &s.panes);
    draw_list(f, s, list);
    // The panes borrow the screen mutably: here each scroll offset gets clamped.
    draw_panes(f, s, panes);
    draw_lines(f, s, chunks[2]);
    draw_stickys(f, s, chunks[3]);
    draw_command(f, s, chunks[4]);
    draw_input(f, s, chunks[5]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[5]);
    }
    // The input area this frame laid out: the caret asks here, not a second
    // computation of the box.
    chunks[5]
}

/// The six parts of SPEC's window, in the order they are drawn.
///
/// # Invariant
///
/// This is the only layout a frame has: the copy, the page height and the mouse
/// all ask here, so what an operator reads is what was on the screen. The body
/// is one horizontal split, and what is left of it after the line groups is the
/// panes' height.
pub fn areas(area: Rect, input_rows: u16, command: bool, line_groups: usize) -> Vec<Rect> {
    // A header and a few events each; never more than half the window, because
    // the panes have to keep some rows.
    let lines = (line_groups as u16 * (1 + LINE_ROWS)).min(area.height / 2);
    Layout::vertical([
        Constraint::Length(1),                           // header
        Constraint::Min(1),                              // sessions | panes
        Constraint::Length(lines),                       // one line per event
        Constraint::Length(1),                           // stickys
        Constraint::Length(if command { 1 } else { 0 }), // cmd and its result
        Constraint::Length(input_rows),                  // the prompt
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
        .listed()
        .iter()
        .map(|listed| UnicodeWidthStr::width(listed.name.as_str()))
        .max()
        .unwrap_or(0);
    // The mark on the left, the name, and one column so a cursor can sit after it.
    (longest as u16 + 2).max(LIST_MIN)
}

/// Where one pane is drawn: its header, if it has one, and the rows below it.
#[derive(Debug)]
pub struct Slot {
    pub name: String,
    pub header: Option<Rect>,
    pub body: Rect,
}

/// Rows the panes get, with the sticky session taking what an even share leaves.
///
/// # Invariant
///
/// A pane that cannot get a row is not drawn, and `hidden` counts them: a pane
/// nobody can read is worse than one that says it is not on screen.
pub fn pane_slots(area: Rect, panes: &Panes, sticky: Option<&str>) -> (Vec<Slot>, usize) {
    // Only the sessions drawn as panes: the line groups and the command line
    // have areas of their own.
    let events: Vec<&Pane> = panes.of_kind(crate::panes::Kind::Full).collect();
    if events.is_empty() || area.height == 0 {
        return (Vec::new(), 0);
    }
    // A header per named pane, but never at the price of a pane having no rows.
    let named = events.iter().filter(|pane| !pane.name.is_empty()).count() as u16;
    let labels = if area.height >= named + events.len() as u16 {
        named
    } else {
        0
    };
    let rows = area.height - labels;
    let per = (rows / events.len() as u16).max(1);
    let extra = rows.saturating_sub(per * events.len() as u16);

    let mut slots: Vec<Slot> = Vec::with_capacity(events.len());
    let mut y = area.y;
    let mut hidden = 0usize;
    for pane in &events {
        let head = if labels > 0 && !pane.name.is_empty() {
            1
        } else {
            0
        };
        let share = per + u16::from(Some(pane.name.as_str()) == sticky) * extra;
        let left = area.bottom().saturating_sub(y + head);
        let height = share.min(left);
        if height == 0 {
            hidden += 1;
            continue;
        }
        slots.push(Slot {
            name: pane.name.clone(),
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
        });
        y += head + height;
    }
    (slots, hidden)
}

/// The session whose row in the list covers `row`, for a click.
///
/// # Invariant
///
/// It reads the same rows `draw_list` writes, so a click lands on the name the
/// operator read. See `docs/notes/tui-render.md`.
pub fn list_at(area: Rect, panes: &Panes, sticky: Option<&str>, row: u16) -> Option<String> {
    if !covers(&area, row) {
        return None;
    }
    let names = panes.listed_names();
    let (start, end) = list_window(names.len(), area.height as usize, sticky, &names);
    let index = start + (row - area.y) as usize;
    panes
        .listed()
        .get(start..end)
        .and_then(|shown| shown.get(index - start))
        .map(|listed| listed.name.clone())
}

/// The session whose line group covers `row`, for a click.
pub fn line_at(area: Rect, panes: &Panes, row: u16) -> Option<String> {
    if !covers(&area, row) {
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
pub fn pane_at(slots: &[Slot], row: u16) -> Option<&str> {
    slots
        .iter()
        .find(|slot| covers(&slot.body, row) || slot.header.is_some_and(|head| covers(&head, row)))
        .map(|slot| slot.name.as_str())
}

/// Whether a `row` falls in this rectangle.
fn covers(area: &Rect, row: u16) -> bool {
    area.height > 0 && area.y <= row && row < area.bottom()
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

/// The session list: one row per session, the sticky one marked.
///
/// # Invariant
///
/// Every session the command layer announced is here, whatever its render mode:
/// the list is how a mode gets changed, so hiding one from it would hide the way
/// back. See `docs/notes/tui-render.md`.
fn draw_list(f: &mut Frame, s: &mut Screen, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let height = area.height as usize;
    let names = s.panes.listed_names();
    let sticky = s.sticky_session.clone().unwrap_or_default();
    let (start, end) = list_window(names.len(), height, s.sticky_session.as_deref(), &names);
    let rows: Vec<Line> = s
        .panes
        .listed()
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|listed| {
            let here = listed.name == sticky;
            // The mark is one column, so the names line up down the list.
            let mark = if here { "▸" } else { " " };
            let mode = s.panes.get(&listed.name).map(|pane| pane.kind);
            let style = match mode {
                Some(crate::panes::Kind::Full) => Style::default().fg(Color::White),
                Some(crate::panes::Kind::Line) => dim(),
                _ => Style::default().fg(Color::DarkGray),
            };
            let style = if here {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                style
            };
            Line::from(Span::styled(format!("{mark}{}", listed.name), style))
        })
        .collect();
    f.render_widget(Paragraph::new(rows), area);
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

/// The way back to the newest row, over the last row of something scrolled up.
///
/// # Invariant
///
/// SPEC asks for it exactly when the view is away from its bottom: a button that
/// is always there says nothing about where the view is. See
/// `docs/notes/tui-render.md`.
fn draw_bottom(f: &mut Frame, area: Rect) {
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
        return;
    }
    let rect = Rect {
        x: area.right() - width,
        y: area.bottom() - 1,
        width,
        height: 1,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ))),
        rect,
    );
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
    // Laid out like the panes: a header each, rows shared, leftover to sticky.
    let sticky = s.sticky_session.clone();
    let (slots, _) = group_slots(area, &names, sticky.as_deref());
    for slot in &slots {
        if let Some(header) = slot.header {
            draw_group_header(f, &slot.name, header, sticky.as_deref());
        }
        draw_headlines(f, s, &slot.name, slot.body);
    }
}

/// One group's rows: one line per event, cut rather than wrapped.
///
/// # Invariant
///
/// A row is an event, so a block that would wrap is cut and marked. Reading a
/// sentence's tail instead of its subject -- or a blank continuation row -- is what
/// this view did before, and it is why a ticker is drawn from blocks rather than
/// from wrapped rows. See `docs/notes/tui-render.md`.
fn draw_headlines(f: &mut Frame, s: &mut Screen, name: &str, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Some((name, scroll)) = s.pane_at(name) else {
        return;
    };
    let width = area.width.saturating_sub(1).max(1) as usize;
    let (rows, max_scroll) =
        s.panes
            .headlines(View::Window(&name), width, area.height as usize, scroll);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(kind, text)| Line::from(Span::styled(text.clone(), style_for(*kind))))
        .collect();
    if let Some(pane) = s.panes.get_mut(&name) {
        pane.max_scroll = max_scroll;
        pane.scroll = scroll.min(max_scroll);
    }
    f.render_widget(Paragraph::new(lines), area);
    if scroll > 0 {
        draw_bottom(f, area);
    }
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
        });
        y += head + height;
    }
    (slots, hidden)
}

/// A header row, styled the one way every header is.
///
/// # Invariant
///
/// One function for the panes and the line groups: a session drawn `full` and the
/// same session drawn `line` must not be told apart by how their names are painted.
fn draw_group_header(f: &mut Frame, name: &str, area: Rect, sticky: Option<&str>) {
    draw_group_header_note(f, name, area, sticky, None);
}

fn draw_group_header_note(
    f: &mut Frame,
    name: &str,
    area: Rect,
    sticky: Option<&str>,
    note: Option<&str>,
) {
    let mut title = format!(" {name} ");
    if let Some(note) = note {
        title.push_str(&format!("{note} "));
    }
    let style = if Some(name) == sticky {
        Style::default().fg(Color::Black).bg(Color::DarkGray)
    } else {
        dim()
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(title, style))), area);
}

/// The sticky arguments, and how each session is drawn beside them.
fn draw_stickys(f: &mut Frame, s: &Screen, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(s.footer.clone(), dim()))),
        area,
    );
}

/// Draws every pane: the header that names it, then the rows that fit in its share.
fn draw_panes(f: &mut Frame, s: &mut Screen, area: Rect) {
    let sticky = s.sticky_session.clone();
    let (slots, hidden) = pane_slots(area, &s.panes, sticky.as_deref());
    for (index, slot) in slots.iter().enumerate() {
        // Only the borrow of one pane is live at a time.
        let last = index + 1 == slots.len();
        if let Some(header) = slot.header {
            // The last drawn pane says when the division hid one: a session whose
            // window is missing has to be explained somewhere.
            let note = (last && hidden > 0).then(|| format!("· {hidden} not shown"));
            draw_group_header_note(f, &slot.name, header, sticky.as_deref(), note.as_deref());
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
    let Some((name, scroll)) = s.pane_at(name) else {
        return;
    };
    let view = View::Window(&name);
    // One column short: a full-width row would otherwise be wrapped again.
    let width = area.width.saturating_sub(1).max(1) as usize;
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
        s.panes.command().is_some(),
        s.panes.of_kind(crate::panes::Kind::Line).count(),
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
            assert_eq!(
                pane_text(&buf, &screen, top + last as u16 - 1),
                "L29",
                "height {height}, input {text:?}: the copy is not the window"
            );
            assert_eq!(
                pane_text(&buf, &screen, top),
                copied[0],
                "height {height}, input {text:?}: the copy starts elsewhere"
            );
            assert_eq!(copied.last().unwrap(), "L29");
        }
    }

    /// A session drawn `line` gets the header a pane gets, laid out the same way.
    #[test]
    fn a_line_group_is_drawn_like_a_pane() {
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed: vec![
                Listed {
                    name: "main".to_string(),
                    status: "idle".to_string(),
                },
                Listed {
                    name: "peer".to_string(),
                    status: "idle".to_string(),
                },
            ],
            full: vec!["main".to_string()],
            line: vec!["peer".to_string()],
            command: "cmd".to_string(),
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
        // Counted the way `draw` counts them, or the two would disagree.
        let parts = parts(width, height, 1, true, 1);
        let (slots, _) = group_slots(parts[2], &["peer".to_string()], None);
        assert_eq!(slots.len(), 1, "one group, one slot");
        let slot = &slots[0];
        let head = slot.header.expect("a line group has a header");
        assert_eq!(
            row_text(&buf, head.y),
            " peer",
            "the same header a pane gets, not `[peer]` in the text"
        );
        // And a pane's header reads the same way: read from the panes, since the
        // list owns the columns on the left.
        let pane_header = body(parts[1], &screen.panes).1;
        assert_eq!(pane_text(&buf, &screen, pane_header.y), " main");
        let drawn: Vec<String> = (slot.body.y..slot.body.bottom())
            .map(|row| row_text(&buf, row))
            .collect();
        // The newest two, and the spare row a group has for the next event.
        assert_eq!(&drawn[..2], &["one", "two"], "and its own rows under it");
        assert!(drawn[2..].iter().all(String::is_empty), "{drawn:?}");
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

    /// The list shows the rows around the sticky session -- the only position.
    #[test]
    fn the_list_window_follows_the_sticky_session() {
        let names: Vec<String> = (0..12).map(|i| format!("s{i}")).collect();
        let listed: Vec<Listed> = names
            .iter()
            .map(|name| Listed {
                name: name.clone(),
                status: "idle".to_string(),
            })
            .collect();
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout {
            listed,
            full: names.clone(),
            line: Vec::new(),
            command: "cmd".to_string(),
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
            .position(|row| row.contains("s9"))
            .expect("drawn");
        assert_eq!(
            list_at(
                list,
                &screen.panes,
                screen.sticky_session.as_deref(),
                list.y + at as u16
            ),
            Some("s9".to_string()),
            "a click on the drawn row names the drawn session"
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
        let mut out = String::new();
        for x in area.x..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.trim_end().to_string()
    }

    /// Where a frame puts the parts a test looks at, for this size.
    fn parts(width: u16, height: u16, input: u16, command: bool, lines: usize) -> Vec<Rect> {
        areas(Rect::new(0, 0, width, height), input, command, lines)
    }

    /// A screen whose one pane reads the unnamed window these lines go to.
    fn screen_with(lines: &[&str]) -> Screen {
        let mut panes = Panes::new();
        panes.layout(&Layout {
            listed: Vec::new(),
            full: vec![String::new()],
            line: Vec::new(),
            command: String::new(),
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
            footer: String::new(),
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
                },
                Listed {
                    name: "peer".to_string(),
                    status: "idle".to_string(),
                },
                Listed {
                    name: "third".to_string(),
                    status: "idle".to_string(),
                },
            ],
            full: vec!["main".to_string(), "peer".to_string()],
            line: vec!["third".to_string()],
            command: "cmd".to_string(),
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
        let top = parts(60, 10, 1, false, 0)[1].y;
        assert_eq!(pane_text(&buf, &screen, top), "[peer] onethree");
        assert_eq!(pane_text(&buf, &screen, top + 1), "[test1] two");
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
            let body = parts(40, 12, 1, false, 0)[1];
            (0..body.height)
                .map(|row| pane_text(&buf, &screen, body.y + row))
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

        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();

        // Events area: rows 1..8, of which 2 are headers. `main` is sticky, so it
        // gets the row the even share left over.
        let body = parts(40, 12, 1, true, 1)[1];
        let rows: Vec<String> = (0..body.height)
            .map(|offset| pane_text(&buf, &screen, body.y + offset))
            .collect();
        let at = |want: &str| {
            rows.iter()
                .position(|row| row.trim() == want)
                .unwrap_or_else(|| panic!("{want:?} is not on screen: {rows:?}"))
        };
        // Each session has its header, and its own rows below it.
        assert_eq!(rows[at("main") + 1], "the answer", "{rows:?}");
        assert_eq!(rows[at("peer") + 1], "another answer", "{rows:?}");
        assert!(at("main") < at("peer"), "the panes keep the list's order");
        // The command line sits above the input, under the stickys.
        let parts = parts(40, 12, 1, true, 1);
        assert_eq!(row_text(&buf, parts[4].y), "$ get_messages");

        // Scrolling the sticky session moves only that pane: the other keeps its newest.
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
            rows.iter().any(|row| row == "another answer"),
            "the peer pane moved with the sticky session: {rows:?}"
        );
        assert_eq!(
            screen.panes.get("cmd").unwrap().max_scroll,
            0,
            "one row of a command line cannot scroll"
        );
    }

    /// A row names the pane drawn there: that is how the wheel and a press know
    /// what to point at.
    #[test]
    fn a_row_names_the_pane_it_falls_in() {
        let screen = split_screen();
        // The panes are found by the rows they were drawn in, whatever else moved.
        let body = parts(40, 12, 1, true, 1)[1];
        assert_eq!(
            screen.pane_at_row(40, 12, body.y + 1).as_deref(),
            Some("main")
        );
        let half = body.height / 2;
        assert_eq!(
            screen.pane_at_row(40, 12, body.y + half + 1).as_deref(),
            Some("peer")
        );
        // The command line is not a pane, and neither is the chrome above it.
        assert_eq!(screen.pane_at_row(40, 12, 0), None);
    }

    /// Layout, window and the rows the bottom of the log shows, without a terminal.
    #[test]
    fn the_log_window_shows_the_newest_rows_above_the_footer() {
        let lines: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut screen = screen_with(&refs);
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

        // The body keeps its rows: header, the panes, the stickys, the input.
        let buf = term.backend().buffer().clone();
        let body = parts(40, 10, 1, false, 0)[1];
        let shown: Vec<String> = (0..body.height)
            .map(|offset| pane_text(&buf, &screen, body.y + offset))
            .collect();
        assert_eq!(shown.last().map(String::as_str), Some("L9"), "{shown:?}");
        assert_eq!(
            screen.panes.get("").unwrap().max_scroll,
            10 - body.height as usize,
            "10 lines into the rows the body has"
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
