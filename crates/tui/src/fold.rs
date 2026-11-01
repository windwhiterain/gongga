//! A styled row fitted into the rows a frame can draw.
//!
//! # Invariant
//!
//! No character is dropped, and a run keeps its style on both sides of a break: what
//! ink a word is drawn in never depends on where the window ends. A break falls where
//! a word ends when one is in reach, so a narrow column still shows whole words.
//! See `docs/notes/tui-fold.md`.

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
            // A row after a break starts with a word, not with the space that broke it.
            if used == 0 && rows.len() > 1 {
                rest = rest.trim_start_matches(' ');
            }
            let fit = match room(rest, width - used) {
                Room {
                    bytes: 0,
                    done: false,
                    ..
                } if used > 0 => {
                    // Not even one character fits in what is left of this row.
                    rows.push(Line::default());
                    used = 0;
                    continue;
                }
                // A character wider than the whole row is still text.
                Room { bytes: 0, .. } => first(rest),
                fit => fit,
            };
            push(
                rows.last_mut().expect("a row is open"),
                span.style,
                &rest[..fit.bytes],
            );
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
fn first(text: &str) -> Room {
    let ch = text.chars().next().expect("text is not empty");
    Room {
        wide: ch.width().unwrap_or(0).max(1),
        bytes: ch.len_utf8(),
        done: false,
    }
}

/// Adds `text` to `line` in `style`, continuing a run of the same ink.
pub fn push(line: &mut Line<'static>, style: Style, text: &str) {
    match line.spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => line.spans.push(Span::styled(text.to_string(), style)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn drawn(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// A break never changes a run's ink: the row after it is the same run.
    #[test]
    fn a_run_keeps_its_style_across_the_break() {
        let ink = Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::UNDERLINED);
        let line = Line::from(vec![
            Span::styled("plain ", Style::default()),
            Span::styled("a long underlined run", ink),
        ]);
        let folded = rows(line, 12);
        assert!(folded.len() > 2, "{folded:?}");
        // The space a break lands on is the break: every word is still there.
        let words: Vec<String> = folded
            .iter()
            .map(drawn)
            .map(|row| row.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        assert_eq!(
            words
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "plain a long underlined run",
            "{folded:?}"
        );
        for row in &folded[1..] {
            assert!(
                row.spans.iter().all(|span| span.style == ink),
                "the run after the break is the same run: {row:?}"
            );
        }
    }

    /// A narrow column still shows whole words.
    #[test]
    fn a_break_falls_where_a_word_ends() {
        let folded = rows(Line::from(Span::raw("the first one")), 5);
        let text: Vec<String> = folded.iter().map(drawn).collect();
        assert_eq!(text, vec!["the ", "first", "one"]);
    }

    /// A word too long for the row is cut, because it has to go somewhere.
    #[test]
    fn a_word_wider_than_the_row_is_cut() {
        let folded = rows(Line::from(Span::raw("abcdefgh")), 3);
        assert_eq!(
            folded.iter().map(drawn).collect::<Vec<_>>(),
            ["abc", "def", "gh"]
        );
    }

    /// A character wider than the row it lands on still gets drawn.
    #[test]
    fn a_character_wider_than_the_row_is_still_drawn() {
        let folded = rows(Line::from(Span::raw("日本語")), 2);
        let text: String = folded.iter().map(drawn).collect();
        assert_eq!(text, "日本語");
        assert_eq!(folded.len(), 3);
        // One column cannot hold a two-column character, and must not loop.
        assert_eq!(rows(Line::from(Span::raw("日")), 1).len(), 1);
    }
}
