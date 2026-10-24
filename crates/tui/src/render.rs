//! Rendering: header, scrollable log, footer, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::{LineKind, Screen};
/// The whole keyboard, on one row: a key it does not name is a key nobody finds.
///
/// # Invariant
///
/// Stays under 100 columns, because a clipped help line loses its last entry --
/// which is the one that says how to leave.
pub const HELP: &str = "↑↓ hist · S+↑↓ row · A+↑↓ page · Tab complete · Sh+Enter newline · A+Y copy \
     · A+M mouse · A+Q quit";

/// Rows spent on header, footer and help; the input's height is asked separately.
pub const CHROME_ROWS: u16 = 3;

/// The prompt on the first row; continuation rows pad behind it.
const PREFIX: &str = "> ";
const PREFIX_WIDTH: u16 = 2;

/// The most rows the input box grows to before it scrolls: prose, not a file.
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
    // The log borrows the screen mutably: here the scroll offset gets clamped.
    draw_log(f, s, chunks[1]);
    draw_footer(f, s, chunks[2]);
    draw_input(f, s, chunks[3]);
    draw_help(f, chunks[4]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[3]);
    }
}

/// Rows the input box needs for `text` here, capped so the log keeps three.
pub fn input_rows(text: &str, width: u16, height: u16) -> u16 {
    let rows = layout_input(text, text.len(), content_width(width))
        .rows
        .len() as u16;
    // The log keeps at least three rows, so the box cannot hide the answer.
    let spare = height.saturating_sub(CHROME_ROWS + 3).max(1);
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
    let title = format!(" gongga · {} ", s.status);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ))),
        area,
    );
}

/// Draws the log window: the rows that fit, borrowed, and the scroll bound back.
fn draw_log(f: &mut Frame, s: &mut Screen, area: Rect) {
    // One column short: a full-width row would otherwise be wrapped again.
    let width = area.width.saturating_sub(1).max(1) as usize;
    let height = area.height as usize;

    let view = s.log.window(width, height, s.scroll);
    s.max_scroll = view.max_scroll;
    // Clamped here too, for a window that just grew or a log that just shrank.
    s.scroll = s.scroll.min(view.max_scroll);

    // Borrowed, not cloned: cloning every visible row made a long log slow.
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
    // The box stops at its cap and scrolls, so the cursor never leaves the window.
    let first = (layout.cursor_row + 1).saturating_sub(height);

    let lines: Vec<Line> = layout
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
        .collect();
    f.render_widget(Paragraph::new(lines), area);

    // A row that exactly fills the width puts the cursor past the area: clamp it.
    let x = (area.x + PREFIX_WIDTH + layout.cursor_col).min(area.x + area.width.saturating_sub(1));
    let y = area.y + (layout.cursor_row - first) as u16;
    f.set_cursor_position((x, y));
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

    /// A copy must take exactly the rows the frame drew, however tall the input is.
    ///
    /// # Invariant
    ///
    /// `log_window_height` is the copy's idea of the log area and `draw` is the
    /// renderer's. A mismatch copies rows that were never on screen -- the
    /// direction that silently hands the operator the wrong text.
    #[test]
    fn the_copied_height_matches_the_drawn_one() {
        // Thirty distinct lines, so where the window starts says how tall it is.
        let lines: Vec<String> = (0..30).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        for (height, text) in [(12u16, ""), (12, "a"), (12, "a\nb\nc"), (20, "a\nb")] {
            let mut screen = screen_with(&refs);
            screen.input.insert_str(text);
            let mut term = Terminal::new(TestBackend::new(30, height)).unwrap();
            term.draw(|f| draw(f, &mut screen)).unwrap();
            let buf = term.backend().buffer().clone();

            let input = input_rows(text, 30, height);
            let copied = crate::log_window_height(height, input);
            // The newest line sits on the last log row, so the copy's height
            // is the row it must be on.
            assert_eq!(
                row_text(&buf, copied as u16),
                "L29",
                "height {height}, input {text:?}: the newest line is not on the row the copy assumes"
            );
            let oldest_visible = 30 - copied;
            assert_eq!(
                row_text(&buf, 1),
                format!("L{oldest_visible}"),
                "height {height}, input {text:?}: the window starts on the wrong line"
            );
        }
    }

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

    /// Two sessions streaming at once, drawn: both blocks are on screen, in
    /// the order they opened.
    #[test]
    fn a_frame_shows_two_streaming_blocks_side_by_side() {
        let mut screen = screen_with(&[]);
        screen.log.stream(
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "one",
            false,
        );
        screen.log.stream(
            "r:test1:1:Output",
            LineKind::Assistant,
            "[test1] ",
            "two",
            true,
        );
        screen.log.stream(
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "three",
            false,
        );

        // 10 rows tall: 6 rows of log. Two blocks of one row each fit whole.
        let mut term = Terminal::new(TestBackend::new(24, 10)).unwrap();
        term.draw(|f| draw(f, &mut screen)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(row_text(&buf, 1), "[peer] onethree");
        assert_eq!(row_text(&buf, 2), "[test1] two");
    }

    /// Layout, window and the rows the bottom of the log shows, without a terminal.
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

    /// One row up is one row, not one line: a window can start mid-line.
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

    /// A long message wraps inside the box: text that cannot be seen cannot be edited.
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

    /// A typed newline is a hard break, even where the width would have kept going.
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
