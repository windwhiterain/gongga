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
    let mut out: Vec<Vec<DocumentSpan>> = Vec::new();
    let mut code = false;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let line = line.trim_end();
        let head = line.trim_start();
        if head.starts_with("```") {
            code = !code;
            continue;
        }
        if code {
            let body = if line.is_empty() {
                " ".to_string()
            } else {
                line.to_string()
            };
            out.extend(wrap(&[DocumentSpan::of(body, SpanClass::Code)], width));
            continue;
        }
        if head.is_empty() {
            // A blank source row is a blank row: a paragraph break has to stay one.
            out.push(Vec::new());
            continue;
        }
        out.extend(wrap(&block(head), width));
    }
    out
}

/// One row of prose: its block structure as a marker run, then its words.
fn block(head: &str) -> Vec<DocumentSpan> {
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
        spans.extend(body.into_iter().map(|span| span.strong()));
        return spans;
    }
    if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
        spans.push(DocumentSpan::of(
            format!("{}• ", " ".repeat(STEP)),
            SpanClass::Marker,
        ));
        spans.extend(inline(rest));
        return spans;
    }
    if let Some(rest) = text.strip_prefix("> ") {
        // A light vertical rule, not a left-eighth block: the block is missing from
        // the faces a body is drawn in, and a fallback glyph is an advance wider than
        // the column it claims -- one column of prose lost per quoted row.
        spans.push(DocumentSpan::of(
            format!("{}│ ", " ".repeat(STEP)),
            SpanClass::Marker,
        ));
        spans.extend(inline(rest));
        return spans;
    }
    if let Some((digits, rest)) = ordered(text) {
        spans.push(DocumentSpan::of(
            format!("{}{digits}. ", " ".repeat(STEP)),
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
            } else if pending_space && used != 0 {
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
