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

fn draw_log(f: &mut Frame, s: &mut Screen, area: Rect) {
    let width = area.width.saturating_sub(1).max(1) as usize;
    let height = area.height as usize;

    // Wrapping by hand, rather than letting Paragraph do it, is what makes the
    // scroll offset computable: we need the wrapped line count to know where the
    // bottom actually is.
    let mut wrapped: Vec<(LineKind, String)> = Vec::new();
    for line in &s.lines {
        for segment in wrap(&line.text, width) {
            wrapped.push((line.kind, segment));
        }
    }

    let total = wrapped.len();
    // Where the newest line sits when the view is pinned to the bottom. It is
    // also the highest offset that still shows a full window, so it is the
    // bound every scroll key clamps against -- and it is published back to the
    // screen, because the keys run when no rendering is happening.
    let max_scroll = total.saturating_sub(height);
    s.max_scroll = max_scroll;
    // Clamping here as well as in the key handler covers the cases the handler
    // cannot see: lines deleted from under a scrolled view, or a window that
    // just grew.
    s.scroll = s.scroll.min(max_scroll);
    let offset = max_scroll.saturating_sub(s.scroll);
    let visible = &wrapped[offset..(offset + height).min(total)];

    let lines: Vec<Line> = visible
        .iter()
        .map(|(kind, text)| Line::from(Span::styled(text.clone(), style_for(*kind))))
        .collect();

    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::NONE)), area);
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

/// Greedy wrap at `width` display columns, honouring embedded newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for hard in text.split('\n') {
        if hard.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut column = 0usize;
        for ch in hard.chars() {
            let w = ch.to_string().width().max(1);
            if column + w > width && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                column = 0;
            }
            current.push(ch);
            column += w;
        }
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_the_column_limit() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn embedded_newlines_force_breaks() {
        assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(wrap("中文ab", 5), vec!["中文a", "b"]);
    }

    #[test]
    fn empty_text_still_yields_one_line() {
        assert_eq!(wrap("", 10), vec![""]);
    }
}
