//! Rendering: header, scrollable log, footer, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::{LineKind, Screen};
pub const HELP: &str =
    "↑↓ history · Shift+↑↓ row · Alt+↑↓ page · Tab complete · Shift+Enter newline · Alt+Q quit";

/// Rows the layout spends on everything that is not the log or the input:
/// header, footer and help. The input's own height varies with its text, so it
/// is asked for separately (see [`input_rows`]).
pub const CHROME_ROWS: u16 = 3;

/// The prompt on the first row, and the padding continuation rows line up
/// behind.
const PREFIX: &str = "> ";
const PREFIX_WIDTH: u16 = 2;

/// The most rows the input box grows to before it scrolls.
///
/// A message is prose, not a file: past a screenful of it, what the operator
/// needs to see is the answer they are replying to, so the box stops growing.
const MAX_INPUT_ROWS: u16 = 8;

pub fn draw(f: &mut Frame, s: &mut Screen) {
    let area = f.area();
    let input = input_rows(s.input.text(), area.width, area.height);
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(3),    // log
        Constraint::Length(1), // footer
        Constraint::Length(input),
        Constraint::Length(1), // help
    ])
    .split(area);

    draw_header(f, s, chunks[0]);
    // The log borrows the screen mutably: it is where the scroll offset gets
    // clamped against a height that only the renderer can compute.
    draw_log(f, s, chunks[1]);
    draw_footer(f, s, chunks[2]);
    draw_input(f, s, chunks[3]);
    draw_help(f, chunks[4]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[3]);
    }
}

/// How many rows the input box needs for `text` at this width, and how many the
/// window can spare.
///
/// Wrapping is part of the answer: the box is one row until the text outgrows
/// the line, then it is one row per wrapped row -- a long message must stay
/// visible, not scroll sideways off the edge where nothing can be edited.
pub fn input_rows(text: &str, width: u16, height: u16) -> u16 {
    let rows = layout_input(text, text.len(), content_width(width))
        .rows
        .len() as u16;
    // The log keeps at least three rows: a box that eats the window would hide
    // the very thing the message is about.
    let spare = height.saturating_sub(CHROME_ROWS + 3).max(1);
    rows.clamp(1, MAX_INPUT_ROWS.min(spare))
}

/// Columns available for text: the prefix on the left, and one spare on the
/// right so a full row is not wrapped a second time by the terminal itself.
fn content_width(width: u16) -> usize {
    width.saturating_sub(PREFIX_WIDTH + 1).max(1) as usize
}

/// One row of the input as it will be drawn, plus where the cursor sits.
struct InputLayout {
    rows: Vec<String>,
    cursor_row: usize,
    cursor_col: u16,
}

/// Breaks the input into rows the way the terminal will show it: a `'\n'` is a
/// hard break, and a row that reaches `width` columns continues on the next one.
///
/// The last row always exists, so an empty input -- or one that ends with a
/// newline -- has somewhere for the cursor to sit.
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
    let title = format!(" gongga · {} ", s.status);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ))),
        area,
    );
}

/// Draws the log window.
///
/// The log hands back exactly the rows that fit, already wrapped and borrowed
/// from its own buffers, so this walks a screenful and nothing else. The
/// scroll bound comes back with them because only here is the window height
/// known, and the key handler needs it between frames.
fn draw_log(f: &mut Frame, s: &mut Screen, area: Rect) {
    // One column short: a row that exactly fills the width leaves the terminal
    // free to wrap it again on its own, which double-spaces every full line.
    let width = area.width.saturating_sub(1).max(1) as usize;
    let height = area.height as usize;

    let view = s.log.window(width, height, s.scroll);
    s.max_scroll = view.max_scroll;
    // Clamping here as well as in the key handler covers the cases the handler
    // cannot see: a window that just grew, or a line set that just shrank.
    s.scroll = s.scroll.min(view.max_scroll);

    // Borrowed, not cloned: the text lives in the log and the spans can point
    // at it. Cloning every visible row was part of what made a long log slow.
    let lines: Vec<Line> = view
        .rows
        .iter()
        .map(|(kind, text)| Line::from(Span::styled(*text, style_for(*kind))))
        .collect();

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_footer(f: &mut Frame, s: &Screen, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(s.footer.clone(), dim()))),
        area,
    );
}

fn draw_input(f: &mut Frame, s: &Screen, area: Rect) {
    let layout = layout_input(s.input.text(), s.input.cursor(), content_width(area.width));
    let height = area.height.max(1) as usize;
    // The box stops at its cap rather than growing forever, so a long message
    // scrolls inside it: the window follows the cursor instead of the cursor
    // leaving the window.
    let first = (layout.cursor_row + 1).saturating_sub(height);

    let lines: Vec<Line> = layout
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, row)| {
            let lead = if index == 0 { PREFIX } else { "  " };
            // The placeholder explains the empty box; it is not text, so it is
            // only drawn where the text would have started.
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
        .collect();
    f.render_widget(Paragraph::new(lines), area);

    // A row that exactly fills the width puts the cursor one past its last
    // column, which is outside the area: keep it on the edge.
    let x = (area.x + PREFIX_WIDTH + layout.cursor_col).min(area.x + area.width.saturating_sub(1));
    let y = area.y + (layout.cursor_row - first) as u16;
    f.set_cursor_position((x, y));
}

fn draw_completion(f: &mut Frame, s: &Screen, input_area: Rect) {
    let Some(completion) = &s.completion else {
        return;
    };
    let shown = completion.candidates.len().min(8);
    // The window follows the selection. `move_selection` walks the whole list,
    // so drawing only the first screenful leaves the ninth candidate selected
    // with nothing highlighted -- and Tab still inserts it.
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

fn draw_help(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(HELP.to_string(), dim())))
            .alignment(Alignment::Left),
        area,
    );
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn style_for(kind: LineKind) -> Style {
    match kind {
        LineKind::System => dim(),
        LineKind::User => Style::default().fg(Color::LightCyan),
        LineKind::Assistant => Style::default().fg(Color::White),
        LineKind::Reasoning => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        LineKind::ToolCall => Style::default().fg(Color::Yellow),
        LineKind::ToolOutput => Style::default().fg(Color::LightBlue),
        LineKind::ToolError => Style::default().fg(Color::LightRed),
        LineKind::Result => Style::default().fg(Color::Green),
        LineKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::log::Log;

    /// The text drawn on row `y`, trailing blanks removed.
    fn row_text(buf: &Buffer, y: u16) -> String {
        let mut out = String::new();
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.trim_end().to_string()
    }

    fn screen_with(lines: &[&str]) -> Screen {
        let mut log = Log::new();
        for line in lines {
            log.push(LineKind::System, *line);
        }
        Screen {
            log,
            completion: None,
            input: crate::InputLine::default(),
            status: String::new(),
            footer: String::new(),
            placeholder: String::new(),
            scroll: 0,
            max_scroll: 0,
        }
    }

    /// Whole-path check without a terminal: layout, window, and the rows the
    /// bottom of the log actually shows. The TUI cannot be run in a test, but
    /// "the newest line is the one above the footer" can be.
    #[test]
    fn the_log_window_shows_the_newest_rows_above_the_footer() {
        let lines: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut screen = screen_with(&refs);
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| draw(f, &mut screen)).unwrap();

        // 10 rows tall: header, six rows of log, footer, input, help.
        let buf = term.backend().buffer().clone();
        for (offset, expected) in ["L4", "L5", "L6", "L7", "L8", "L9"].iter().enumerate() {
            assert_eq!(row_text(&buf, 1 + offset as u16), *expected);
        }
        assert_eq!(screen.max_scroll, 4, "10 lines into a 6-row window");
    }

    /// One row up is one row, not one line: the window can start halfway
    /// through a wrapped line, and the row above the bottom has to be the row
    /// that was there before.
    #[test]
    fn scrolling_moves_the_window_by_rows() {
        let mut screen = screen_with(&["aaaaaaa", "bbbbbbb", "ccccccc"]);
        let mut term = Terminal::new(TestBackend::new(5, 8)).unwrap();
        // 8 rows tall: 4 rows of log, at width 4 (one column is spared), so six
        // rows of text scroll by two.
        term.draw(|f| draw(f, &mut screen)).unwrap();
        assert_eq!(screen.max_scroll, 2, "six rows of text, four rows of glass");

        screen.scroll = 1;
        term.draw(|f| draw(f, &mut screen)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(row_text(&buf, 1), "aaa", "the row above the window");
        assert_eq!(row_text(&buf, 2), "bbbb");
    }

    /// A message longer than the line has to wrap inside the box instead of
    /// disappearing off the right edge: the input is where the text gets
    /// edited, and text that cannot be seen cannot be edited.
    #[test]
    fn a_long_input_wraps_into_more_rows_and_takes_them_from_the_log() {
        let mut screen = screen_with(&["L0", "L1", "L2", "L3", "L4"]);
        screen.input.insert_str("1234567890");
        // 10 wide: a two-column prompt and one column spared, so seven of text.
        assert_eq!(input_rows("1234567890", 10, 10), 2);
        let mut term = Terminal::new(TestBackend::new(10, 10)).unwrap();
        term.draw(|f| draw(f, &mut screen)).unwrap();

        let buf = term.backend().buffer().clone();
        // header, five rows of log, footer, two input rows, help.
        assert_eq!(row_text(&buf, 7), "> 1234567");
        assert_eq!(row_text(&buf, 8), "  890");
    }

    /// A typed newline is a hard break: the row after it starts a new line even
    /// where the width would have kept going.
    #[test]
    fn a_typed_newline_starts_a_new_row() {
        let mut screen = screen_with(&[]);
        screen.input.insert_str("one\ntwo");
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| draw(f, &mut screen)).unwrap();

        assert_eq!(input_rows("one\ntwo", 20, 10), 2);
        let buf = term.backend().buffer().clone();
        assert_eq!(row_text(&buf, 7), "> one");
        assert_eq!(row_text(&buf, 8), "  two");
    }

    /// The box stops growing: past its cap it scrolls, because an input that
    /// eats the window hides the answer it is replying to.
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
