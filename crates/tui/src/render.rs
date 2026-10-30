//! Rendering: header, scrollable log, footer, completion popup, input line.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
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
pub const HELP: &str = "↑↓ hist · S+↑↓ row · A+↑↓ page · Sh+Enter newline · A+Y copy · click pane \
     · A+M mouse · A+Q quit";

/// Rows spent on header, footer and help; the input's height is asked separately.
pub const CHROME_ROWS: u16 = 3;

/// The one row `cmd` and its result share, outside the panes.
pub const COMMAND_ROWS: u16 = 1;

/// The prompt on the first row; continuation rows pad behind it.
const PREFIX: &str = "> ";
const PREFIX_WIDTH: u16 = 2;

/// The most rows the input box grows to before it scrolls: prose, not a file.
const MAX_INPUT_ROWS: u16 = 8;

pub fn draw(f: &mut Frame, s: &mut Screen) -> Rect {
    let area = f.area();
    let input = input_rows(s.input.text(), area.width, area.height);
    let chunks = areas(area, input, s.panes.command().is_some());

    draw_header(f, s, chunks[0]);
    // The panes borrow the screen mutably: here each scroll offset gets clamped.
    draw_panes(f, s, chunks[1]);
    draw_footer(f, s, chunks[2]);
    draw_command(f, s, chunks[3]);
    draw_input(f, s, chunks[4]);
    draw_help(f, chunks[5]);
    if s.completion.is_some() {
        draw_completion(f, s, chunks[4]);
    }
    // The input area this frame laid out: the caret asks here, not a second
    // computation of the box.
    chunks[4]
}

/// The six areas a frame is made of, in the order they are drawn.
///
/// # Invariant
///
/// This is the only layout a frame has: the copy, the page height and the mouse
/// all ask here, so what an operator reads is what was on the screen.
pub fn areas(area: Rect, input_rows: u16, command: bool) -> Vec<Rect> {
    Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(1),    // the panes
        Constraint::Length(1), // footer
        // The line `cmd` and its result share, only when there is one.
        Constraint::Length(if command { COMMAND_ROWS } else { 0 }),
        Constraint::Length(input_rows),
        Constraint::Length(1), // help
    ])
    .split(area)
    .to_vec()
}

/// Where one pane is drawn: its header, if it has one, and the rows below it.
pub struct Slot {
    pub name: String,
    pub header: Option<Rect>,
    pub body: Rect,
}

/// Rows the panes get, with `focus` taking what an even share leaves over.
///
/// # Invariant
///
/// A pane that cannot get a row is not drawn, and `hidden` counts them: a pane
/// nobody can read is worse than one that says it is not on screen.
pub fn pane_slots(area: Rect, panes: &Panes, focus: &str) -> (Vec<Slot>, usize) {
    let events: Vec<&Pane> = panes.events().collect();
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
        let share = per + u16::from(pane.name == focus) * extra;
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
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ))),
        area,
    );
}

/// Draws every pane: the header that names it, then the rows that fit in its share.
fn draw_panes(f: &mut Frame, s: &mut Screen, area: Rect) {
    let (slots, hidden) = pane_slots(area, &s.panes, &s.focus);
    for (index, slot) in slots.iter().enumerate() {
        // Only the borrow of one pane is live at a time.
        let last = index + 1 == slots.len();
        if let Some(header) = slot.header {
            let mut title = format!(" {} ", slot.name);
            if last && hidden > 0 {
                title.push_str(&format!("· {hidden} not shown "));
            }
            let style = if slot.name == s.focus {
                Style::default().fg(Color::Black).bg(Color::DarkGray)
            } else {
                dim()
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(title, style))),
                header,
            );
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
    let Some((name, merged, scroll)) = s.pane_at(name) else {
        return;
    };
    let view = if merged {
        View::All
    } else {
        View::Window(&name)
    };
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
}

fn draw_footer(f: &mut Frame, s: &Screen, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(s.footer.clone(), dim()))),
        area,
    );
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

/// The input area as `draw` lays it out, so a caller can find the row to re-anchor.
pub fn input_area(area: Rect, s: &Screen) -> Rect {
    areas(
        area,
        input_rows(s.input.text(), area.width, area.height),
        s.panes.command().is_some(),
    )[4]
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
    use crate::{Layout, Panes, Window};

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
            // The copy ends on the newest line, which is the last row of the area.
            let last = copied.len();
            assert_eq!(
                row_text(&buf, last as u16),
                "L29",
                "height {height}, input {text:?}: the copy is not the window"
            );
            assert_eq!(
                row_text(&buf, 1),
                copied[0],
                "height {height}, input {text:?}: the copy starts elsewhere"
            );
            assert_eq!(copied.last().unwrap(), "L29");
        }
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

    fn screen_with(lines: &[&str]) -> Screen {
        let mut panes = Panes::new();
        for line in lines {
            panes.line("", LineKind::System, *line);
        }
        Screen {
            panes,
            focus: String::new(),
            completion: None,
            input: crate::InputLine::default(),
            status: String::new(),
            footer: String::new(),
            placeholder: String::new(),
        }
    }

    /// A split area: two sessions the command layer named, plus the command line.
    fn split_screen() -> Screen {
        let mut screen = screen_with(&[]);
        screen.panes.layout(&Layout::Split(vec![
            Window {
                name: "main".to_string(),
                command: false,
            },
            Window {
                name: "peer".to_string(),
                command: false,
            },
            Window {
                name: "cmd".to_string(),
                command: true,
            },
        ]));
        screen.focus = "main".to_string();
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

        // 10 rows tall: 6 rows of log. Two blocks of one row each fit whole.
        let mut term = Terminal::new(TestBackend::new(24, 10)).unwrap();
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(row_text(&buf, 1), "[peer] onethree");
        assert_eq!(row_text(&buf, 2), "[test1] two");
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
            (1..=8).map(|row| row_text(&buf, row)).collect::<Vec<_>>()
        };

        // Uncapped: the thinking block wraps, and every row of it is on screen.
        let all = frame(0);
        assert!(all[0].starts_with("[main] thinking: abcdefghij"), "{all:?}");
        assert_eq!(
            all.iter().filter(|r| r.is_empty()).count(),
            8 - 5,
            "{all:?}"
        );

        // Capped at one row: the same block shows the row it is writing now, and
        // the label comes with it.
        let one = frame(1);
        assert!(one[0].starts_with("[main] thinking: uvwxyz"), "{one:?}");
        assert_eq!(one[1], "[main] tool: bash{ls}", "{one:?}");
        // The answer is the block no cap touches: both of its rows are still there.
        assert_eq!(one[2], "[main] a", "{one:?}");
        assert_eq!(one[3], "b", "{one:?}");
        assert_eq!(
            one.iter().filter(|r| r.is_empty()).count(),
            8 - 4,
            "{one:?}"
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

        // Events area: rows 1..8, of which 2 are headers. `main` is focused, so it
        // gets the row the even share left over.
        assert_eq!(row_text(&buf, 1), " main", "{buf:?}");
        assert_eq!(row_text(&buf, 2), "the answer");
        assert_eq!(row_text(&buf, 5), " peer");
        assert_eq!(row_text(&buf, 6), "another answer");
        // The command line sits between the footer and the input.
        assert_eq!(row_text(&buf, 9), "$ get_messages");

        // Scrolling the focus moves only that pane: the other keeps its newest.
        screen.scroll_by(1);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(
            row_text(&buf, 6),
            "another answer",
            "the peer pane moved with the focus"
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
        assert_eq!(screen.pane_at_row(40, 12, 1).as_deref(), Some("main"));
        assert_eq!(screen.pane_at_row(40, 12, 5).as_deref(), Some("peer"));
        // The command line is not a pane, and neither is the chrome above it.
        assert_eq!(screen.pane_at_row(40, 12, 9), None);
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

        // 10 rows tall: header, six rows of log, footer, input, help.
        let buf = term.backend().buffer().clone();
        for (offset, expected) in ["L4", "L5", "L6", "L7", "L8", "L9"].iter().enumerate() {
            assert_eq!(row_text(&buf, 1 + offset as u16), *expected);
        }
        assert_eq!(
            screen.panes.get("").unwrap().max_scroll,
            4,
            "10 lines into a 6-row window"
        );
    }

    /// One row up is one row, not one line: a window can start mid-line.
    #[test]
    fn scrolling_moves_the_window_by_rows() {
        let mut screen = screen_with(&["aaaaaaa", "bbbbbbb", "ccccccc"]);
        let mut term = Terminal::new(TestBackend::new(5, 8)).unwrap();
        // 8 rows tall: 4 rows of log, at width 4 (one column is spared), so six
        // rows of text scroll by two.
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
        assert_eq!(
            screen.panes.get("").unwrap().max_scroll,
            2,
            "six rows of text, four rows of glass"
        );

        screen.scroll_by(1);
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();
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
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

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
        term.draw(|f| {
            draw(f, &mut screen);
        })
        .unwrap();

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
