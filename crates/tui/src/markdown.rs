//! SPEC's `output (colored markdown)`, drawn without losing anything.
//!
//! # Invariant
//!
//! What is drawn is what the model wrote: a marker this window has no rendering
//! for is printed as itself. A reader must be able to reconstruct the output from
//! the screen, so nothing is silently dropped to make a row look tidier.
//! See `docs/notes/tui-markdown.md`.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::table;

/// The columns one nesting level adds in front of a list item.
const STEP: &str = "  ";

/// What a row of a document is, as far as the markdown that printed it says.
///
/// # Invariant
///
/// Only what a row's own text cannot say is here. A heading, a quote and a list item
/// all print their marker, so the frame reads them off the text; a code block's body
/// and a table's row print nothing that says what they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// Prose, and every marker a reader can see in it.
    Prose,
    /// A code block's body, where no marker of the row is markdown.
    Code,
    /// A fence line, which opens or closes the block around it.
    Fence,
    /// One row of a table.
    Table,
    /// The rule between a table's header and its body.
    TableRule,
}

/// One row of a document: the text a frame prints, and what it is.
pub struct Piece {
    pub text: String,
    pub ink: Ink,
}

/// `text` as the rows an answer is read in, at most `width` columns wide.
///
/// # Invariant
///
/// Block structure becomes row structure -- a paragraph is rows, a list item is a
/// row, a table is the grid its columns need -- and every source marker is printed.
/// Where a row ends is the window's decision, not the source's: it wraps.
/// See `docs/notes/tui-markdown.md`.
pub fn layout(text: &str, width: usize) -> Vec<Piece> {
    let options =
        Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut doc = Doc::default();
    // Where each open link's target is, until its `](…)` is due.
    let mut targets: Vec<String> = Vec::new();
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
                doc.open(cover);
                doc.push(&"#".repeat(level as usize));
                doc.push(" ");
            }
            Event::End(TagEnd::Heading(_)) => doc.close(Ink::Prose),
            // A paragraph is its rows and carries no marker; ending one ends a row.
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => doc.close(Ink::Prose),
            Event::Start(Tag::List(start)) => {
                doc.close(Ink::Prose);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                if lists.pop().is_some() && lists.is_empty() {
                    doc.close(Ink::Prose);
                }
            }
            Event::Start(Tag::Item) => {
                doc.close(Ink::Prose);
                doc.open(cover);
                for _ in 1..lists.len() {
                    doc.push(STEP);
                }
                match lists.last_mut() {
                    Some(Some(n)) => {
                        doc.push(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => doc.push("- "),
                }
            }
            Event::End(TagEnd::Item) => doc.close(Ink::Prose),
            Event::Start(Tag::BlockQuote(_)) => {
                doc.close(Ink::Prose);
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                doc.close(Ink::Prose);
                quote = quote.saturating_sub(1);
            }
            // The fence is part of what was written, language included.
            Event::Start(Tag::CodeBlock(kind)) => {
                doc.close(Ink::Prose);
                doc.open(cover);
                doc.push("```");
                if let CodeBlockKind::Fenced(language) = kind {
                    doc.push(language.trim());
                }
                doc.close(Ink::Fence);
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                doc.open(cover);
                doc.push("```");
                doc.close(Ink::Fence);
                code = false;
            }
            Event::Start(Tag::Table(aligns)) => {
                doc.close(Ink::Prose);
                table = Some(Table {
                    aligns: aligns.iter().map(|align| align_of(*align)).collect(),
                    header: Vec::new(),
                    body: Vec::new(),
                    row: Vec::new(),
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    doc.rows.extend(table.pieces(width, quote));
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
                // A code block's rows are its own, blank lines included: no mark
                // in front, so the indentation survives.
                true => {
                    for line in code_lines(&text) {
                        doc.open(cover);
                        doc.push(line.trim_end());
                        doc.close(Ink::Code);
                    }
                }
                false => {
                    doc.open(cover);
                    doc.push(&text);
                }
            },
            Event::Code(code) => {
                doc.open(cover);
                doc.push("`");
                doc.push(code.trim());
                doc.push("`");
            }
            // Emphasis is kept as it was written: the marker is what
            // [`colored_row`] reads to draw the words it covers.
            Event::Start(Tag::Emphasis) => {
                doc.open(cover);
                doc.push("*");
            }
            Event::End(TagEnd::Emphasis) => {
                doc.open(cover);
                doc.push("*");
            }
            Event::Start(Tag::Strong) => {
                doc.open(cover);
                doc.push("**");
            }
            Event::End(TagEnd::Strong) => {
                doc.open(cover);
                doc.push("**");
            }
            Event::Start(Tag::Strikethrough) => {
                doc.open(cover);
                doc.push("~~");
            }
            Event::End(TagEnd::Strikethrough) => {
                doc.open(cover);
                doc.push("~~");
            }
            // A link is its text and its target: the target is information, and
            // this window has no way to follow it instead.
            Event::Start(Tag::Link { dest_url, .. }) => {
                doc.open(cover);
                doc.push("[");
                targets.push(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                doc.open(cover);
                doc.push("](");
                doc.push(&targets.pop().unwrap_or_default());
                doc.push(")");
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                doc.open(cover);
                doc.push("![");
                targets.push(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                doc.open(cover);
                doc.push("](");
                doc.push(&targets.pop().unwrap_or_default());
                doc.push(")");
            }
            Event::SoftBreak => {
                // A soft break is a space in the document, not a row: the row
                // breaks are the log's, at the width it actually has.
                doc.open(cover);
                doc.push(" ");
            }
            // A hard break ends a row, except inside a cell: a cell is one row.
            Event::HardBreak => match table.is_some() {
                true => {
                    doc.open(cover);
                    doc.push(" ");
                }
                false => doc.close(Ink::Prose),
            },
            Event::Rule => {
                doc.close(Ink::Prose);
                doc.open(cover);
                doc.push("---");
                doc.close(Ink::Prose);
            }
            // The item's own `- ` already said this is a list; the box says which
            // kind, and it is what the writer typed.
            Event::TaskListMarker(done) => {
                doc.open(cover);
                doc.push(if done { "[x] " } else { "[ ] " });
            }
            // Raw HTML has no rendering here, and what it says is still what the
            // model wrote: it is printed as itself.
            Event::InlineHtml(html) => {
                doc.open(cover);
                doc.push(&html);
            }
            Event::Html(html) => {
                doc.close(Ink::Prose);
                for line in html.lines() {
                    doc.open(cover);
                    doc.push(line);
                    doc.close(Ink::Prose);
                }
            }
            // Anything a future parser adds still arrives as `Text`, which is
            // where it is drawn.
            _ => {}
        }
    }
    doc.close(Ink::Prose);
    // The log draws the rows it is given; a trailing blank row is not one.
    while doc.rows.last().is_some_and(|piece| piece.text.is_empty()) {
        doc.rows.pop();
    }
    doc.rows
}

/// `text` as one string of rows: the document, for a reader with no frame.
#[cfg(test)]
pub fn render(text: &str) -> String {
    layout(text, usize::MAX)
        .into_iter()
        .map(|piece| piece.text)
        .collect::<Vec<String>>()
        .join("\n")
}

/// The rows a document prints, and the row still being written.
#[derive(Default)]
struct Doc {
    rows: Vec<Piece>,
    row: String,
    /// Whether a row is being written. A row exists from its first character.
    open: bool,
}

impl Doc {
    /// Starts a row with the quote marks it sits inside.
    fn open(&mut self, quote: usize) {
        if self.open {
            return;
        }
        for _ in 0..quote {
            self.row.push_str("> ");
        }
        self.open = true;
    }

    fn push(&mut self, text: &str) {
        self.open = true;
        self.row.push_str(text);
    }

    /// Ends the row, keeping it as one of `ink`.
    fn close(&mut self, ink: Ink) {
        if !self.open {
            return;
        }
        let text = std::mem::take(&mut self.row);
        self.rows.push(Piece { text, ink });
        self.open = false;
    }

    /// The row's words without keeping the row: a table cell, not a row.
    fn take(&mut self) -> String {
        self.open = false;
        std::mem::take(&mut self.row)
    }
}

/// A code block's rows: its own lines, the newline that closes the block dropped.
fn code_lines(text: &str) -> std::str::Split<'_, char> {
    text.strip_suffix('\n').unwrap_or(text).split('\n')
}

/// A table being read: its columns, its header, and the body so far.
struct Table {
    aligns: Vec<table::Align>,
    header: Vec<String>,
    body: Vec<Vec<String>>,
    /// The cells of the row being read.
    row: Vec<String>,
}

impl Table {
    /// The grid this table is, in the columns the frame has room for.
    ///
    /// # Invariant
    ///
    /// A table inside a quote keeps the quote's marks on every row, out of the columns
    /// the table may use: a row without them is a row the source did not have.
    fn pieces(self, width: usize, quote: usize) -> Vec<Piece> {
        let mark = "> ".repeat(quote);
        let room = width.saturating_sub(mark.len());
        table::rows(&self.header, &self.aligns, &self.body, room)
            .into_iter()
            .map(|row| Piece {
                text: format!("{mark}{}", row.text),
                ink: match row.rule {
                    true => Ink::TableRule,
                    false => Ink::Table,
                },
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
/// One row of an answer, in the colours the markdown it prints asks for.
///
/// # Invariant
///
/// The row is read the way it was written: a marker keeps its place on screen and
/// the words it covers take the ink that says what they are. A marker with no
/// closing partner is text, and is drawn as itself in the row's own ink.
/// See `docs/notes/tui-markdown.md`.
pub fn colored_row(text: &str, ink: Ink) -> Line<'static> {
    match ink {
        // A code block's body carries no marker: all of it is what it is.
        Ink::Code => Line::from(Span::styled(text.to_string(), code_style())),
        Ink::Fence => {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let rest = quote_prefix(text, &mut spans);
            spans.push(Span::styled(rest.to_string(), fence_style()));
            Line::from(spans)
        }
        Ink::TableRule => Line::from(Span::styled(text.to_string(), marker_style())),
        Ink::Table => table_row(text),
        Ink::Prose => prose_row(text),
    }
}

/// A prose row: the quote it sits in, then the marker that says what the row is.
fn prose_row(text: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = quote_prefix(text, &mut spans);
    // A rule between two parts of a document.
    if rest == "---" {
        spans.push(Span::styled(rest.to_string(), marker_style()));
        return Line::from(spans);
    }
    // A heading: its `#`s are structure, its words are what the heading says.
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if hashes > 0 && rest[hashes..].starts_with(' ') {
        // The space after the `#`s is the marker's, and is printed with it.
        spans.push(Span::styled(rest[..hashes + 1].to_string(), marker_style()));
        inline(&rest[hashes + 1..], heading_style(), &mut spans);
        return Line::from(spans);
    }
    // A list item: the indentation, the marker, and then the item's own words.
    let indent = rest.len() - rest.trim_start_matches(' ').len();
    if let Some((marker, after)) = list_marker(&rest[indent..]) {
        spans.push(Span::styled(
            format!("{}{marker}", " ".repeat(indent)),
            quote_style(),
        ));
        rest = after;
        // A task list says whether it is done in the item's own marker.
        for (marker, state) in [("[x] ", done_style as fn() -> Style), ("[ ] ", todo_style)] {
            if let Some(after) = rest.strip_prefix(marker) {
                spans.push(Span::styled(marker.to_string(), state()));
                rest = after;
                break;
            }
        }
    }
    inline(rest, Style::default(), &mut spans);
    Line::from(spans)
}

/// The quote marks a row sits inside, pushed as chrome, and what follows them.
fn quote_prefix<'a>(text: &'a str, spans: &mut Vec<Span<'static>>) -> &'a str {
    let mut rest = text;
    while let Some(after) = rest.strip_prefix("> ") {
        spans.push(Span::styled("> ", quote_style()));
        rest = after;
    }
    rest
}

/// A table's row: its borders belong to the table, its cells are prose.
fn table_row(text: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cell = String::new();
    for ch in text.chars() {
        if ch != '|' {
            cell.push(ch);
            continue;
        }
        inline(&cell, Style::default(), &mut spans);
        cell.clear();
        spans.push(Span::styled("|", marker_style()));
    }
    inline(&cell, Style::default(), &mut spans);
    Line::from(spans)
}

/// `text` in `base`, with each inline marker drawn apart from what it covers.
///
/// # Invariant
///
/// The markers are the ones [`render`] prints, so a pair is read only in the form
/// that renderer wrote it: `*`, `**`, `~~`, a backtick, a `[text](url)`. A marker
/// with no closing partner is text, drawn as itself in `base`.
/// See `docs/notes/tui-markdown.md`.
fn inline(text: &str, base: Style, out: &mut Vec<Span<'static>>) {
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let marked = MARKS.iter().find_map(|mark| {
            let (body, after) = enclosed(rest, mark.marker)?;
            Some((mark, body, after))
        });
        if let Some((mark, body, after)) = marked {
            flush(&mut plain, base, out);
            out.push(Span::styled(mark.marker, marker_style()));
            inline(body, (mark.style)(), out);
            out.push(Span::styled(mark.marker, marker_style()));
            rest = after;
            continue;
        }
        if let Some(link) = link(rest) {
            flush(&mut plain, base, out);
            out.push(Span::styled(link.lead.to_string(), marker_style()));
            out.push(Span::styled(link.text.to_string(), link_style()));
            out.push(Span::styled("](", marker_style()));
            out.push(Span::styled(link.target.to_string(), marker_style()));
            out.push(Span::styled(")", marker_style()));
            rest = link.after;
            continue;
        }
        let ch = rest.chars().next().expect("rest is not empty");
        plain.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    flush(&mut plain, base, out);
}

/// The body between `marker` and its next partner, and what follows the pair.
///
/// # Invariant
///
/// An empty body is no pair, and neither is one with a space at either end: `a * b * c`
/// is arithmetic, and a marker read there would colour words the writer never marked.
/// The renderer only writes a marker around words, so this loses nothing it printed.
fn enclosed<'a>(text: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let after = text.strip_prefix(marker)?;
    let end = after.find(marker)?;
    if end == 0 {
        return None;
    }
    let body = &after[..end];
    if body.starts_with(char::is_whitespace) || body.ends_with(char::is_whitespace) {
        return None;
    }
    Some((body, &after[end + marker.len()..]))
}

/// One inline pair: the marker that is printed, and the ink it gives its words.
struct Mark {
    marker: &'static str,
    style: fn() -> Style,
}

/// The inline pairs an answer can print, longest first so `**` is never two `*`.
const MARKS: [Mark; 4] = [
    Mark {
        marker: "`",
        style: code_style,
    },
    Mark {
        marker: "**",
        style: strong_style,
    },
    Mark {
        marker: "~~",
        style: strike_style,
    },
    Mark {
        marker: "*",
        style: emphasis_style,
    },
];

/// A link as [`render`] prints it, split where the inks change.
struct Link<'a> {
    lead: &'a str,
    text: &'a str,
    target: &'a str,
    /// What follows the closing `)`.
    after: &'a str,
}

/// The link `text` starts with, if it starts with one.
///
/// # Invariant
///
/// `[` alone is text: a pair needs its `](...)`, so a row with a bracket in it is
/// not read as a link that was never made.
fn link(text: &str) -> Option<Link<'_>> {
    let (lead, after) = match text.strip_prefix("![") {
        Some(after) => ("![", after),
        None => ("[", text.strip_prefix('[')?),
    };
    let close = after.find("](")?;
    let tail = &after[close + 2..];
    let end = tail.find(')')?;
    Some(Link {
        lead,
        text: &after[..close],
        target: &tail[..end],
        after: &tail[end + 1..],
    })
}

/// A list item's marker -- `- `, `+ `, `* `, or `12. ` -- and what follows it.
fn list_marker(text: &str) -> Option<(&str, &str)> {
    for marker in ["- ", "+ ", "* "] {
        if let Some(after) = text.strip_prefix(marker) {
            return Some((marker, after));
        }
    }
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        if let Some(after) = text[digits..].strip_prefix(". ") {
            return Some((&text[..digits + ". ".len()], after));
        }
    }
    None
}

/// The plain text run before a marker, in the ink the run is read in.
fn flush(plain: &mut String, style: Style, out: &mut Vec<Span<'static>>) {
    if plain.is_empty() {
        return;
    }
    out.push(Span::styled(std::mem::take(plain), style));
}

/// The markers themselves: present, quiet, and never the words' own ink.
fn marker_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// What a heading says, once its `#`s have been passed.
fn heading_style() -> Style {
    Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD)
}

/// A code span and the row a fence is written on: one ink, because both are code.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing the model wrote is missing from the screen.
    ///
    /// # Invariant
    ///
    /// Walked through the markers one at a time, because each of them was dropped
    /// by an earlier version of this file for being untidy on screen.
    #[test]
    fn every_marker_the_writer_used_is_still_there() {
        for (source, want) in [
            ("**bold**", "**bold**"),
            ("*italic*", "*italic*"),
            ("~~gone~~", "~~gone~~"),
            ("`code`", "`code`"),
            ("[text](https://x.test/a)", "[text](https://x.test/a)"),
            ("![alt](img.png)", "![alt](img.png)"),
            ("> quoted", "> quoted"),
            ("- item", "- item"),
            ("1. first", "1. first"),
            ("# Head", "# Head"),
            ("### Deep", "### Deep"),
            ("---", "---"),
        ] {
            assert_eq!(render(source), want, "{source:?}");
        }
    }

    #[test]
    fn a_code_block_keeps_its_fence_and_language() {
        assert_eq!(
            render("```rust\nfn main() {}\n```"),
            "```rust\nfn main() {}\n```"
        );
        // Indentation inside the block is the document, so it survives.
        assert_eq!(render("```\n  kept\n```"), "```\n  kept\n```");
        assert_eq!(render("```\nno language\n```"), "```\nno language\n```");
    }

    #[test]
    fn a_paragraph_is_one_row_however_the_source_broke_it() {
        // A soft break is a space in the document; the log decides the rows.
        assert_eq!(render("one\ntwo"), "one two");
        assert_eq!(render("one\n\ntwo"), "one\ntwo");
    }

    #[test]
    fn a_nested_list_indents_by_two_and_keeps_its_marks() {
        assert_eq!(render("- a\n  - b"), "- a\n  - b");
        assert_eq!(render("1. a\n2. b"), "1. a\n2. b");
    }

    #[test]
    fn a_quote_marks_every_row_it_covers() {
        assert_eq!(render("> one\n>\n> two"), "> one\n> two");
        assert_eq!(render("> outer\n>\n> > inner"), "> outer\n> > inner");
    }

    #[test]
    fn a_task_list_says_whether_it_is_done() {
        assert_eq!(render("- [x] done\n- [ ] not"), "- [x] done\n- [ ] not");
    }

    #[test]
    fn a_link_inside_a_sentence_keeps_its_target() {
        assert_eq!(
            render("see [the docs](https://x.test/d) for more"),
            "see [the docs](https://x.test/d) for more"
        );
    }

    #[test]
    fn nothing_left_over_ends_in_a_blank_row() {
        for text in ["", "a", "- a\n", "# h\n", "```\nx\n```\n"] {
            assert!(!render(text).ends_with('\n'), "{text:?}");
        }
    }

    /// A table is drawn as a grid, in the columns the frame has room for.
    ///
    /// # Invariant
    ///
    /// The rows are the table's own: one width, no wider than the frame, and a rule
    /// between what the columns are said to hold and what they do.
    #[test]
    fn a_table_is_drawn_as_a_grid_of_the_width_it_is_given() {
        let source = "| name | what |\n|:--|--:|\n| a | the first one |\n| bb | c |";
        for width in [80usize, 32, 20, 16] {
            let pieces = layout(source, width);
            let drawn: Vec<&str> = pieces.iter().map(|piece| piece.text.as_str()).collect();
            assert!(drawn.len() >= 4, "{width}: {drawn:?}");
            let wide: Vec<usize> = drawn
                .iter()
                .map(|row| unicode_width::UnicodeWidthStr::width(*row))
                .collect();
            assert!(
                wide.iter().all(|w| *w == wide[0] && *w <= width),
                "{width}: {drawn:?}"
            );
            assert_eq!(pieces[1].ink, Ink::TableRule, "{width}");
            assert!(
                pieces
                    .iter()
                    .all(|piece| matches!(piece.ink, Ink::Table | Ink::TableRule)),
                "{width}"
            );
            // Nothing a cell says is lost, however narrow the column got.
            let all: String = drawn.join(" ");
            for word in ["name", "what", "first", "one"] {
                assert!(all.contains(word), "{width}: {word} is still on screen");
            }
        }
    }

    /// A table inside a quote keeps the marks the source put on every row.
    #[test]
    fn a_table_inside_a_quote_keeps_the_quotes_marks() {
        let pieces = layout("> | a | b |\n> |---|---|\n> | 1 | 2 |", 30);
        let drawn: Vec<&str> = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert!(drawn.iter().all(|row| row.starts_with("> |")), "{drawn:?}");
    }

    /// A code block's body is the one thing a row's text cannot say it is.
    #[test]
    fn a_code_block_says_which_rows_are_its_own() {
        let pieces = layout("```rust\nlet a = 1;\n\nlet b = 2;\n```", 80);
        let inks: Vec<Ink> = pieces.iter().map(|piece| piece.ink).collect();
        let drawn: Vec<&str> = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(
            drawn,
            vec!["```rust", "let a = 1;", "", "let b = 2;", "```"],
            "a blank line inside a block is a row"
        );
        assert_eq!(
            inks,
            vec![Ink::Fence, Ink::Code, Ink::Code, Ink::Code, Ink::Fence]
        );
    }

    /// The markers inside a code block are code, not markdown.
    #[test]
    fn a_code_block_draws_its_markers_as_the_words_they_are() {
        let pieces = layout("```\n# not a heading\n- not a list\n```", 80);
        let body: Vec<&Piece> = pieces
            .iter()
            .filter(|piece| piece.ink == Ink::Code)
            .collect();
        assert_eq!(body.len(), 2, "the two body rows");
        for piece in body {
            let row = colored_row(&piece.text, piece.ink);
            // One span for the whole row: no marker in it was read as markdown.
            assert_eq!(row.spans.len(), 1, "{:?}", piece.text);
            assert_eq!(row.spans[0].style, code_style());
            assert_eq!(row.spans[0].content, piece.text);
        }
    }

    /// Raw HTML has no rendering here and is still what the model wrote.
    #[test]
    fn raw_html_is_printed_as_itself() {
        assert_eq!(render("<div>\nhi\n</div>"), "<div>\nhi\n</div>");
    }

    /// An unfinished marker is text -- the parser's reading of a document still
    /// being written -- and it stays on screen.
    #[test]
    fn an_unfinished_marker_is_shown_as_itself() {
        assert_eq!(render("**bol"), "**bol");
        assert_eq!(render("```rust\nfn f("), "```rust\nfn f(\n```");
    }
}
