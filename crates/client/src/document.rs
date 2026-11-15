//! SPEC's `output: colored markdown`, turned into rows once, here.
//!
//! # Invariant
//!
//! What a front-end draws is finished: a row already fits the width it was asked for
//! and each run already says how it looks, so no front-end parses markdown, counts
//! columns or decides what is strong. Two front-ends drawing one answer draw the same
//! rows. See `docs/notes/client-document.md`.
//!
//! Nothing here drops text: a marker the document does not close is printed as it
//! came, and a code row that is longer than the width is broken rather than cut.

use unicode_width::UnicodeWidthChar;

use crate::render_wire::{DocumentSpan, SpanClass};

/// The columns one nesting level adds in front of a list item.
const STEP: usize = 2;

/// `text` as the rows a body is drawn in, at most `width` columns wide.
pub fn rows(text: &str, width: usize) -> Vec<Vec<DocumentSpan>> {
    let width = width.max(1);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Vec<DocumentSpan>> = Vec::new();
    let mut code = false;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at].trim_end();
        let depth = (line.len() - line.trim_start().len()) / STEP;
        let head = line.trim_start();
        at += 1;
        // The fence is part of what was written, language included: a code block
        // whose boundary is not drawn is a paragraph that happens to be blue.
        if head.starts_with("```") {
            code = !code;
            out.push(vec![DocumentSpan::of(head.to_string(), SpanClass::Marker)]);
            continue;
        }
        if code {
            // Code keeps its own rows: a leading rule on every one of them, so the
            // block has an edge whatever the pane, and a break that never looks for a
            // space -- a line of code has no words to break between.
            for piece in hard_wrap(line, width) {
                let mut row: Vec<DocumentSpan> = Vec::new();
                if !piece.is_empty() {
                    row.push(DocumentSpan::of(piece, SpanClass::CodeBlock));
                }
                out.push(row);
            }
            continue;
        }
        // A horizontal rule is a line, not three hyphens: the row is the line.
        if is_rule(head) {
            out.push(vec![DocumentSpan::of("─".repeat(width), SpanClass::Marker)]);
            continue;
        }
        if head.is_empty() {
            // A blank source row is a blank row: a paragraph break has to stay one.
            out.push(Vec::new());
            continue;
        }
        // A pipe row with an alignment row under it is a table, and a table is laid
        // out here: columns are as wide as their widest cell, narrowed to the pane by
        // taking a column at a time from the widest one.
        if head.starts_with('|') {
            let aligns = lines.get(at).and_then(|next| alignment_row(next.trim()));
            if let Some(aligns) = aligns {
                if aligns.len() != cells(head).len() {
                    out.extend(wrap(&block(head, depth), width));
                    continue;
                }
                let mut body: Vec<Vec<String>> = Vec::new();
                while at < lines.len() {
                    let next = lines[at].trim();
                    if !next.starts_with('|') {
                        break;
                    }
                    body.push(cells(next));
                    at += 1;
                }
                out.extend(table(&cells(head), &aligns, &body, width));
                continue;
            }
        }
        out.extend(wrap(&block(head, depth), width));
    }
    out
}

/// The cells of one `| a | b |` row, trimmed.
fn cells(row: &str) -> Vec<String> {
    let inner = row.strip_prefix('|').unwrap_or(row);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// The alignments an `|:--|--:|` row states, or nothing when the row is not one.
fn alignment_row(row: &str) -> Option<Vec<Align>> {
    if !row.starts_with('|') {
        return None;
    }
    let mut aligns = Vec::new();
    for cell in cells(row) {
        let left = cell.starts_with(':');
        let right = cell.ends_with(':');
        let dashes = cell.trim_matches(':');
        if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            (true, true) => Align::Center,
            (false, false) => Align::None,
        });
    }
    Some(aligns)
}

/// Which way one column's cells lean.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    None,
    Left,
    Right,
    Center,
}

/// The narrowest a column is ever drawn.
const MIN_COLUMN: usize = 3;

/// `header`, its alignment row and `body` as rows that fit `width` columns.
///
/// # Invariant
///
/// A cell that does not fit its column is wrapped onto the row below, never cut:
/// text a reader cannot see is worse than a tall row. The rule row carries no
/// colons -- a colon is a column of its own, and one would shift every cell under
/// it by a character.
fn table(
    header: &[String],
    aligns: &[Align],
    body: &[Vec<String>],
    width: usize,
) -> Vec<Vec<DocumentSpan>> {
    let columns = header.len().max(1);
    let mut widths: Vec<usize> = (0..columns)
        .map(|column| {
            let mut widest = text_width(header.get(column).map(String::as_str).unwrap_or(""));
            for row in body {
                widest = widest.max(text_width(
                    row.get(column).map(String::as_str).unwrap_or(""),
                ));
            }
            widest.max(MIN_COLUMN)
        })
        .collect();
    // One pipe and one space at each side of every cell, plus the row's own edges.
    let used = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * columns + 1;
    while used(&widths) > width {
        let Some((widest, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, column)| **column > MIN_COLUMN)
            .max_by_key(|(_, column)| **column)
        else {
            break;
        };
        widths[widest] -= 1;
    }
    let mut out: Vec<Vec<DocumentSpan>> = Vec::new();
    out.extend(compose(header, &widths, aligns, SpanClass::Heading));
    for (at, row) in body.iter().enumerate() {
        out.extend(compose(row, &widths, aligns, SpanClass::Text));
        // A rule between the rows, so a cell that now takes two lines is still read
        // as one row: without it two wrapped cells look like four rows.
        if at + 1 != body.len() {
            out.push(vec![DocumentSpan::of(rule(&widths), SpanClass::Marker)]);
        }
    }
    out
}

/// One table row as the physical rows it takes.
///
/// # Invariant
///
/// A cell holds markdown like any other words: it is parsed by [`inline`] and wrapped
/// by [`wrap`], so `**bold**` and a backticked name inside a table look the way they
/// look outside one. A heading row promotes what it holds, so a header cell is a
/// heading rather than a bold word in the body's own ink.
fn compose(row: &[String], widths: &[usize], aligns: &[Align], class: SpanClass) -> Vec<Vec<DocumentSpan>> {
    let cells: Vec<Vec<Vec<DocumentSpan>>> = widths
        .iter()
        .enumerate()
        .map(|(column, width)| {
            let text = row.get(column).map(String::as_str).unwrap_or("");
            let mut spans = inline(text);
            if class == SpanClass::Heading {
                for span in &mut spans {
                    if span.class == SpanClass::Text {
                        span.class = SpanClass::Heading;
                    }
                }
            }
            wrap(&spans, *width)
        })
        .collect();
    let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut rows: Vec<Vec<DocumentSpan>> = Vec::new();
    for line in 0..height {
        let mut spans: Vec<DocumentSpan> = vec![DocumentSpan::of("|", SpanClass::Marker)];
        for (column, width) in widths.iter().enumerate() {
            let empty: Vec<DocumentSpan> = Vec::new();
            let cell = cells[column].get(line).unwrap_or(&empty);
            let used: usize = cell.iter().map(|span| text_width(&span.text)).sum();
            let spare = width.saturating_sub(used);
            let (before, after) = match aligns.get(column).copied().unwrap_or(Align::None) {
                Align::Right => (spare, 0),
                Align::Center => (spare / 2, spare - spare / 2),
                Align::None | Align::Left => (0, spare),
            };
            spans.push(DocumentSpan::of(" ".repeat(before + 1), class));
            spans.extend(cell.iter().cloned());
            spans.push(DocumentSpan::of(
                format!("{}{}", " ".repeat(after), " |"),
                SpanClass::Marker,
            ));
        }
        rows.push(spans);
    }
    rows
}

/// The row under the header: one dash run per column, nothing else.
fn rule(widths: &[usize]) -> String {
    let mut text = String::from("|");
    for width in widths {
        text.push_str(&"-".repeat(width + 2));
        text.push('|');
    }
    text
}

/// `text` cut to at most `width` columns.
fn clip(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let wide = character.width().unwrap_or(1);
        if used + wide > width {
            break;
        }
        used += wide;
        out.push(character);
    }
    out
}

/// How many columns `text` takes when drawn.
fn text_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(1)).sum()
}

/// Whether a line is a horizontal rule: three or more of one mark and nothing else.
fn is_rule(line: &str) -> bool {
    let mut marks = line.chars();
    let Some(first) = marks.next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    let count = 1 + marks.clone().count();
    count >= 3 && marks.all(|c| c == first || c == ' ')
}

/// What every row of a code block opens with: the rule that makes it one block.
/// `text` cut into pieces of at most `width` columns, breaking where it must.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut pieces = vec![String::new()];
    let mut used = 0;
    for character in text.chars() {
        let wide = character.width().unwrap_or(1);
        if used + wide > width {
            pieces.push(String::new());
            used = 0;
        }
        used += wide;
        if let Some(last) = pieces.last_mut() {
            last.push(character);
        }
    }
    pieces
}

/// One row of prose: its block structure as a marker run, then its words.
fn block(head: &str, depth: usize) -> Vec<DocumentSpan> {
    // A no-break space, because the wrapper collapses runs of spaces and an
    // indent that collapses is a list that lost its shape.
    let step = " ".repeat(STEP * depth);
    let mut spans: Vec<DocumentSpan> = Vec::new();
    let mut text = head;
    let mut level = 0;
    while text.starts_with('#') {
        level += 1;
        text = &text[1..];
    }
    if level != 0 {
        // A heading is one run, strong: its own size is the front-end's to choose.
        let body = inline(text.trim_start());
        spans.extend(body.into_iter().map(|span| span.heading()));
        return spans;
    }
    if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
        spans.push(DocumentSpan::of(format!("{}• ", step), SpanClass::Marker));
        spans.extend(inline(rest));
        return spans;
    }
    if let Some(rest) = text.strip_prefix("> ") {
        // A light vertical rule, not a left-eighth block: the block is missing from
        // the faces a body is drawn in, and a fallback glyph is an advance wider than
        // the column it claims -- one column of prose lost per quoted row.
        spans.push(DocumentSpan::of(format!("{}│ ", step), SpanClass::Marker));
        spans.extend(inline(rest));
        return spans;
    }
    if let Some((digits, rest)) = ordered(text) {
        spans.push(DocumentSpan::of(
            format!("{}{digits}. ", step),
            SpanClass::Marker,
        ));
        spans.extend(inline(rest));
        return spans;
    }
    inline(text)
}

/// The digits of a `1. ` marker and the words after it.
fn ordered(text: &str) -> Option<(&str, &str)> {
    let digits = text.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || !text[digits..].starts_with(". ") {
        return None;
    }
    Some((&text[..digits], &text[digits + 2..]))
}

/// Inline markers: `code`, **strong**, *emphasis*, ~~strike~~, [words](url).
fn inline(text: &str) -> Vec<DocumentSpan> {
    let mut spans: Vec<DocumentSpan> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some((inner, after)) = paired(rest, "**", "**") {
            flush(&mut plain, &mut spans);
            spans.push(DocumentSpan::of(inner, SpanClass::Strong));
            rest = after;
            continue;
        }
        if let Some((inner, after)) = paired(rest, "~~", "~~") {
            flush(&mut plain, &mut spans);
            spans.push(DocumentSpan::of(inner, SpanClass::Strike));
            rest = after;
            continue;
        }
        if let Some((inner, after)) = paired(rest, "`", "`") {
            flush(&mut plain, &mut spans);
            spans.push(DocumentSpan::of(inner, SpanClass::Code));
            rest = after;
            continue;
        }
        if let Some((inner, after)) = paired(rest, "*", "*") {
            flush(&mut plain, &mut spans);
            spans.push(DocumentSpan::of(inner, SpanClass::Emphasis));
            rest = after;
            continue;
        }
        if let Some((words, url, after)) = link(rest) {
            flush(&mut plain, &mut spans);
            spans.push(DocumentSpan::of(words, SpanClass::Text));
            // The address is what the reader needs, and the terminal prints it too.
            spans.push(DocumentSpan::of(format!(" ({url})"), SpanClass::Link));
            rest = after;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(first) = chars.next() {
            plain.push(first);
            rest = chars.as_str();
        }
    }
    flush(&mut plain, &mut spans);
    spans
}

/// The text between an opening and a closing marker, and what follows it.
///
/// # Invariant
///
/// A marker with no closing pair is text: an asterisk nobody finished is printed as
/// one, never swallowed.
fn paired<'a>(text: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
    let body = text.strip_prefix(open)?;
    let end = body.find(close)?;
    if end == 0 {
        return None;
    }
    Some((&body[..end], &body[end + close.len()..]))
}

fn link(text: &str) -> Option<(&str, &str, &str)> {
    let body = text.strip_prefix('[')?;
    let close = body.find("](")?;
    let url_end = body[close + 2..].find(')')?;
    Some((
        &body[..close],
        &body[close + 2..close + 2 + url_end],
        &body[close + 2 + url_end + 1..],
    ))
}

fn flush(plain: &mut String, spans: &mut Vec<DocumentSpan>) {
    if plain.is_empty() {
        return;
    }
    spans.push(DocumentSpan::of(std::mem::take(plain), SpanClass::Text));
}

/// `spans` fitted into rows of `width` columns.
///
/// # Invariant
///
/// A break falls where a space is when one is in reach, so a narrow pane still shows
/// whole words; a run keeps its class on both sides of a break, and no character is
/// lost -- a word longer than the row is broken rather than cut.
fn wrap(spans: &[DocumentSpan], width: usize) -> Vec<Vec<DocumentSpan>> {
    let mut rows: Vec<Vec<DocumentSpan>> = Vec::new();
    let mut row: Vec<DocumentSpan> = Vec::new();
    let mut used = 0usize;
    let mut pending_space = false;
    for span in spans {
        for character in span.text.chars() {
            if character == ' ' {
                pending_space = true;
                continue;
            }
            let wide = character.width().unwrap_or(1);
            let space = usize::from(pending_space);
            if used != 0 && used + space + wide > width {
                flush_row(&mut row, &mut rows);
                used = 0;
            // A leading space on a block's first row is its indent, and an indent
            // is structure: dropped, a nested list is a flat one.
            } else if pending_space && (used != 0 || rows.is_empty()) {
                push(&mut row, " ", span.class);
                used += 1;
            }
            pending_space = false;
            let mut buffer = [0u8; 4];
            push(&mut row, character.encode_utf8(&mut buffer), span.class);
            used += wide;
            if used > width {
                // Wider than the row itself: it ends here, and the next run starts one.
                flush_row(&mut row, &mut rows);
                used = 0;
            }
        }
    }
    flush_row(&mut row, &mut rows);
    rows
}

fn push(row: &mut Vec<DocumentSpan>, text: &str, class: SpanClass) {
    if let Some(last) = row.last_mut() {
        if last.class == class {
            last.text.push_str(text);
            return;
        }
    }
    row.push(DocumentSpan::of(text.to_string(), class));
}

fn flush_row(row: &mut Vec<DocumentSpan>, rows: &mut Vec<Vec<DocumentSpan>>) {
    if row.is_empty() {
        return;
    }
    rows.push(std::mem::take(row));
}
