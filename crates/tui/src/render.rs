//! Rendering: header, scrollable log, footer, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::{LineKind, Screen};
pub const HELP: &str = "↑↓ history · Ctrl+↑↓ or PgUp/PgDn page · Shift+↑↓ row · wheel scroll · Tab complete · Ctrl+Q quit";

/// Rows the layout spends on everything that is not the log: header, footer,
/// input and help. The log takes what is left, so scrolling needs this to know
/// how tall a page is. Keep it in step with the layout in [`draw`].
pub const NON_LOG_ROWS: u16 = 4;

pub fn draw(f: &mut Frame, s: &mut Screen) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(3),    // log
        Constraint::Length(1), // footer
        Constraint::Length(1), // input
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
    let prefix = "> ";
    let text = s.input.text();
    let before = &text[..s.input.cursor().min(text.len())];
    let cursor_x = area.x + (prefix.width() + before.width()) as u16;
    let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::Cyan))];
    if s.input.is_empty() {
        spans.push(Span::styled(s.placeholder.clone(), dim()));
    } else {
        spans.push(Span::raw(text));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    f.set_cursor_position((cursor_x, area.y));
}

fn draw_completion(f: &mut Frame, s: &Screen, input_area: Rect) {
    let Some(completion) = &s.completion else {
        return;
    };
    let shown = completion.candidates.len().min(8);
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
        Paragraph::new(Line::from(Span::styled(HELP.to_string(), dim()))).alignment(Alignment::Left),
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
}
