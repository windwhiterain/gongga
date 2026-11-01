//! SPEC's `output (colored markdown)`, drawn without losing anything.
//!
//! # Invariant
//!
//! A row is styled by what the document said it is, never by what the row happens to
//! print: the parse decides, and the wrap keeps what it decided. Nothing is dropped
//! to make a row look tidier. See `docs/notes/tui-markdown.md`.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::{fold, table};

/// The columns one nesting level adds in front of a list item.
const STEP: &str = "  ";

/// `text` as the rows an answer is drawn in, at most `width` columns wide.
///
/// # Invariant
///
/// Block structure becomes row structure -- a paragraph is rows, a list item is a row,
/// a table is the grid its columns need -- and every source marker is printed in the
/// quiet ink, with the words it covers in theirs. Where a row ends is the window's
/// decision, so a break may fall inside a run: both halves keep its ink.
pub fn rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let options =
        Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut doc = Doc::default();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut code = false;
    let mut table: Option<Table> = None;

    for event in Parser::new_ext(text, options) {
        // A cell holds inline content and no rows of its own, so nothing inside a
        // table is covered by the quote around it.
        let cover = match table.is_some() {
            true => 0,
            false => quote,
        };
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                doc.close();
                doc.base = heading_style();
                doc.open(cover);
                doc.styled(&"#".repeat(level as usize), marker_style());
                doc.styled(" ", marker_style());
            }
            Event::End(TagEnd::Heading(_)) => {
                doc.close();
                doc.base = Style::default();
            }
            // A paragraph is its rows and carries no marker; ending one ends a row.
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => doc.close(),
            Event::Start(Tag::List(start)) => {
                doc.close();
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                if lists.pop().is_some() && lists.is_empty() {
                    doc.close();
                }
            }
            Event::Start(Tag::Item) => {
                doc.close();
                doc.open(cover);
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "- ".to_string(),
                };
                let indent = STEP.repeat(lists.len().saturating_sub(1));
                doc.styled(&format!("{indent}{marker}"), quote_style());
            }
            Event::End(TagEnd::Item) => doc.close(),
            Event::Start(Tag::BlockQuote(_)) => {
                doc.close();
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                doc.close();
                quote = quote.saturating_sub(1);
            }
            // The fence is part of what was written, language included.
            Event::Start(Tag::CodeBlock(kind)) => {
                doc.close();
                doc.open(cover);
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language.trim().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                doc.styled(&format!("```{language}"), fence_style());
                doc.close();
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                doc.open(cover);
                doc.styled("```", fence_style());
                doc.close();
                code = false;
            }
            Event::Start(Tag::Table(aligns)) => {
                doc.close();
                table = Some(Table {
                    aligns: aligns.iter().map(|align| align_of(*align)).collect(),
                    header: Vec::new(),
                    body: Vec::new(),
                    row: Vec::new(),
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    doc.lines.extend(table.rows(width, quote));
                }
            }
            Event::Start(Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = table.as_mut() {
                    table.row.push(doc.take());
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = table.as_mut() {
                    table.header = std::mem::take(&mut table.row);
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.body.push(row);
                }
            }
            Event::Text(text) => match code {
                // A code block's rows are its own, blank lines included.
                true => {
                    for line in code_lines(&text) {
                        doc.open(cover);
                        doc.styled(line.trim_end(), code_style());
                        doc.close();
                    }
                }
                false => {
                    doc.open(cover);
                    doc.text(&text);
                }
            },
            Event::Code(code) => {
                doc.open(cover);
                doc.styled("`", marker_style());
                doc.styled(code.trim(), code_style());
                doc.styled("`", marker_style());
            }
            // A mark prints at both ends, and gives its words the ink between them.
            Event::Start(Tag::Emphasis) => {
                doc.open(cover);
                doc.open_mark("*", emphasis_style());
            }
            Event::End(TagEnd::Emphasis) => doc.close_mark(),
            Event::Start(Tag::Strong) => {
                doc.open(cover);
                doc.open_mark("**", strong_style());
            }
            Event::End(TagEnd::Strong) => doc.close_mark(),
            Event::Start(Tag::Strikethrough) => {
                doc.open(cover);
                doc.open_mark("~~", strike_style());
            }
            Event::End(TagEnd::Strikethrough) => doc.close_mark(),
            // A link is its text and its target: the target is information, and this
            // window has no way to follow it instead.
            Event::Start(Tag::Link { dest_url, .. }) => {
                doc.open(cover);
                doc.open_mark_text("[", format!("]({dest_url})"), link_style());
            }
            Event::End(TagEnd::Link) => doc.close_mark(),
            Event::Start(Tag::Image { dest_url, .. }) => {
                doc.open(cover);
                doc.open_mark_text("![", format!("]({dest_url})"), link_style());
            }
            Event::End(TagEnd::Image) => doc.close_mark(),
            Event::SoftBreak => {
                // A soft break is a space in the document, not a row: the row breaks
                // are the window's, at the width it actually has.
                doc.open(cover);
                doc.text(" ");
            }
            // A hard break ends a row, except inside a cell: a cell is one row.
            Event::HardBreak => match table.is_some() {
                true => {
                    doc.open(cover);
                    doc.text(" ");
                }
                false => doc.close(),
            },
            Event::Rule => {
                doc.close();
                doc.open(cover);
                doc.styled("---", marker_style());
                doc.close();
            }
            // The item's own `- ` already said this is a list; the box says which
            // kind, and it is what the writer typed.
            Event::TaskListMarker(done) => {
                doc.open(cover);
                let style = match done {
                    true => done_style(),
                    false => todo_style(),
                };
                doc.styled(if done { "[x] " } else { "[ ] " }, style);
            }
            // Raw HTML has no rendering here; it is printed as itself, a row per line.
            Event::InlineHtml(html) => {
                doc.open(cover);
                doc.text(&html);
            }
            Event::Html(html) => {
                doc.close();
                for line in html.lines() {
                    doc.open(cover);
                    doc.text(line);
                    doc.close();
                }
            }
            // Anything a future parser adds still arrives as `Text`, which is where it
            // is drawn.
            _ => {}
        }
    }
    doc.close();
    // The frame draws the rows it is given; a trailing blank row is not one.
    let mut rows: Vec<Line<'static>> = doc
        .lines
        .into_iter()
        .flat_map(|line| fold::rows(line, width))
        .collect();
    while rows.last().is_some_and(|row| row.width() == 0) {
        rows.pop();
    }
    rows
}

/// An inline mark that is open: what it prints when it closes, and the ink it gives.
struct Mark {
    close: String,
    style: Style,
}

/// The rows of a document being read, and the row still being written.
#[derive(Default)]
struct Doc {
    /// The rows finished so far: styled, and not yet fitted to a width.
    lines: Vec<Line<'static>>,
    row: Line<'static>,
    /// Whether a row is being written. A row exists from its first character.
    open: bool,
    /// The ink of the block this row is in: a heading's, or the window's own.
    base: Style,
    /// The inline marks open on this row, outermost first.
    marks: Vec<Mark>,
}

impl Doc {
    /// Starts a row with the quote marks it sits inside.
    fn open(&mut self, quote: usize) {
        if self.open {
            return;
        }
        for _ in 0..quote {
            fold::push(&mut self.row, quote_style(), "> ");
        }
        self.open = true;
    }

    /// Adds `text` in the ink this row is being written in.
    fn text(&mut self, text: &str) {
        let ink = self.ink();
        self.open = true;
        fold::push(&mut self.row, ink, text);
    }

    /// Adds `text` in an ink of its own: a mark, a fence, a code block's body.
    fn styled(&mut self, text: &str, style: Style) {
        self.open = true;
        fold::push(&mut self.row, style, text);
    }

    /// Opens an inline mark, and gives everything until it closes the mark's ink.
    fn open_mark(&mut self, open: &str, style: Style) {
        self.open_mark_text(open, close_of(open), style);
    }

    fn open_mark_text(&mut self, open: &str, close: String, style: Style) {
        self.styled(open, marker_style());
        self.marks.push(Mark { close, style });
    }

    /// Closes the innermost mark, printing what it closes with.
    fn close_mark(&mut self) {
        let Some(mark) = self.marks.pop() else {
            return;
        };
        self.styled(&mark.close, marker_style());
    }

    /// The ink of the text being written: the block's, and every mark open over it.
    fn ink(&self) -> Style {
        self.marks
            .iter()
            .fold(self.base, |style, mark| style.patch(mark.style))
    }

    /// Ends the row, keeping it.
    fn close(&mut self) {
        if !self.open {
            return;
        }
        let row = std::mem::take(&mut self.row);
        self.lines.push(row);
        self.open = false;
    }

    /// The row's runs without keeping the row: a table cell, not a row.
    fn take(&mut self) -> Line<'static> {
        self.open = false;
        std::mem::take(&mut self.row)
    }
}

/// What a mark whose opening is `open` closes with.
fn close_of(open: &str) -> String {
    match open {
        "[" => "]".to_string(),
        "![" => "]".to_string(),
        other => other.to_string(),
    }
}

/// A code block's rows: its own lines, the newline that closes the block dropped.
fn code_lines(text: &str) -> std::str::Split<'_, char> {
    text.strip_suffix('\n').unwrap_or(text).split('\n')
}

/// A table being read: its columns, its header, and the body so far.
struct Table {
    aligns: Vec<table::Align>,
    header: Vec<Line<'static>>,
    body: Vec<Vec<Line<'static>>>,
    /// The cells of the row being read.
    row: Vec<Line<'static>>,
}

impl Table {
    /// The grid this table is, in the columns the frame has room for.
    ///
    /// # Invariant
    ///
    /// A table inside a quote keeps the quote's marks on every row, out of the columns
    /// the table may use: a row without them is a row the source did not have.
    fn rows(self, width: usize, quote: usize) -> Vec<Line<'static>> {
        let room = width.saturating_sub(quote * 2);
        table::rows(&self.header, &self.aligns, &self.body, room)
            .into_iter()
            .map(|row| {
                let mut out = Line::default();
                for _ in 0..quote {
                    fold::push(&mut out, quote_style(), "> ");
                }
                out.spans.extend(row.spans);
                out
            })
            .collect()
    }
}

/// Which of the table's four alignments a source marker names.
fn align_of(align: Alignment) -> table::Align {
    match align {
        Alignment::None => table::Align::None,
        Alignment::Left => table::Align::Left,
        Alignment::Center => table::Align::Center,
        Alignment::Right => table::Align::Right,
    }
}

/// The markers themselves: present, quiet, and never the words' own ink.
pub fn marker_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// What a heading says, once its `#`s have been passed.
fn heading_style() -> Style {
    Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD)
}

/// A code span and a code block's body: one ink, because both are code.
fn code_style() -> Style {
    Style::default().fg(Color::LightGreen)
}

/// The fence itself, so the block it opens is visible from its first row.
fn fence_style() -> Style {
    code_style().add_modifier(Modifier::BOLD)
}

/// `**strong**` stands out from the prose around it without becoming a heading.
fn strong_style() -> Style {
    Style::default()
        .fg(Color::LightYellow)
        .add_modifier(Modifier::BOLD)
}

fn emphasis_style() -> Style {
    Style::default().add_modifier(Modifier::ITALIC)
}

/// `~~gone~~` reads as taken back, not as a second kind of emphasis.
fn strike_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::CROSSED_OUT)
}

fn link_style() -> Style {
    Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::UNDERLINED)
}

/// The structural marks a row can carry -- quote, list, number.
fn quote_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// A task list's state, in the box rather than in the words after it.
fn done_style() -> Style {
    Style::default().fg(Color::Green)
}

fn todo_style() -> Style {
    Style::default().fg(Color::Yellow)
}
