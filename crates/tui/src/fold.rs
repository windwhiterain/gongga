//! A styled row fitted into the rows a frame can draw.
//!
//! # Invariant
//!
//! This is draw-path code, so it may not panic on any input: every character is
//! either drawn or is the one space a break landed on, and a run keeps its style on
//! both sides of a break. A break falls where a word ends when one is in reach, so a
//! narrow column still shows whole words. See `docs/notes/tui-fold.md`.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// `line` as the rows `width` columns allow: one row, or as many as it needs.
pub fn rows(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = vec![Line::default()];
    let mut used = 0usize;
    for span in line.spans {
        let mut rest: &str = span.content.as_ref();
        while !rest.is_empty() {
            if used >= width {
                rows.push(Line::default());
                used = 0;
            }
            // A row after a break starts with a word, not with the space it broke at.
            if used == 0 && rows.len() > 1 {
                rest = rest.trim_start_matches(' ');
                if rest.is_empty() {
                    break;
                }
            }
            let fit = match room(rest, width - used) {
                Room { bytes: 0, .. } if used > 0 => {
                    // Not even one character fits in what is left of this row.
                    rows.push(Line::default());
                    used = 0;
                    continue;
                }
                // A character wider than the whole row is still text: it is drawn,
                // and it is the last thing this row can hold.
                Room { bytes: 0, .. } => match first(rest) {
                    Some(wide) => wide,
                    None => break,
                },
                fit => fit,
            };
            match rows.last_mut() {
                Some(row) => push(row, span.style, &rest[..fit.bytes]),
                None => break,
            }
            rest = &rest[fit.bytes..];
            used += fit.wide;
            if fit.done {
                rows.push(Line::default());
                used = 0;
            }
        }
    }
    rows
}

/// What one row takes from the text it is folding.
struct Room {
    /// Display columns the row gains.
    wide: usize,
    /// Bytes of the text that go on the row, starting at its first character.
    bytes: usize,
    /// Whether the row ends here although there is room left: a word ended.
    done: bool,
}

/// The columns and bytes of `text` that fit `room`, ending at a word when it can.
///
/// # Invariant
///
/// The answer always fits, and a cut inside a word backs up to the last space that
/// fits: that space ends the row, so the next row starts with a whole word.
fn room(text: &str, room: usize) -> Room {
    let mut wide = 0usize;
    let mut bytes = 0usize;
    // Where the last space that fits would end the row, if one is in reach.
    let mut space: Option<(usize, usize)> = None;
    for ch in text.chars() {
        let advance = ch.width().unwrap_or(0).max(1);
        if wide + advance > room {
            break;
        }
        wide += advance;
        bytes += ch.len_utf8();
        if ch == ' ' {
            space = Some((wide, bytes));
        }
    }
    // A cut inside a word: end the row at the space instead, and break there.
    if bytes < text.len() && !text[bytes..].starts_with(' ') {
        if let Some((space_wide, space_bytes)) = space {
            return Room {
                wide: space_wide,
                bytes: space_bytes,
                done: true,
            };
        }
    }
    Room {
        wide,
        bytes,
        done: false,
    }
}

/// The first character of `text`, however wide it is.
fn first(text: &str) -> Option<Room> {
    let ch = text.chars().next()?;
    Some(Room {
        wide: ch.width().unwrap_or(0).max(1),
        bytes: ch.len_utf8(),
        done: false,
    })
}

/// Adds `text` to `line` in `style`, continuing a run of the same ink.
pub fn push(line: &mut Line<'static>, style: Style, text: &str) {
    if text.is_empty() {
        return;
    }
    match line.spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => line.spans.push(Span::styled(text.to_string(), style)),
    }
}
