//! A markdown table fitted to the columns the window has.
//!
//! # Invariant
//!
//! Every row drawn is the same display width and no wider than it was given, so a
//! frame draws a table as a table instead of wrapping it into a paragraph.
//! See `docs/notes/tui-table.md`.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How a column's cells sit inside it, as the source's `:` markers say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// No marker: the writer left the column to the reader, and it reads left.
    None,
    Left,
    Center,
    Right,
}

/// One row of the drawn table: its text, and whether it is the header's rule.
pub struct Row {
    pub text: String,
    pub rule: bool,
}

/// The narrowest a column is squeezed to: its rule row spends three dashes.
const MIN: usize = 3;

/// The columns one cell spends on its border and its air: `| a ` is three.
const PADDING: usize = 3;

/// The rows `header` and `body` are drawn in, at most `width` columns wide.
pub fn rows(header: &[String], aligns: &[Align], body: &[Vec<String>], width: usize) -> Vec<Row> {
    let columns = header
        .len()
        .max(body.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return Vec::new();
    }
    let natural: Vec<usize> = (0..columns)
        .map(|column| {
            let widest = lines(header, body)
                .map(|row| cell(row, column).map_or(0, UnicodeWidthStr::width))
                .max()
                .unwrap_or(0);
            widest.max(MIN)
        })
        .collect();
    let widths = fit(&natural, width);
    let mut out: Vec<Row> = Vec::new();
    for (index, row) in lines(header, body).enumerate() {
        // The rule sits between what the columns are said to hold and what they do.
        if index == 1 {
            out.push(Row {
                text: rule(&widths, aligns),
                rule: true,
            });
        }
        for text in compose(row, &widths, aligns) {
            out.push(Row { text, rule: false });
        }
    }
    out
}

/// The header first, then the body: the order the table is read in.
fn lines<'a>(header: &'a [String], body: &'a [Vec<String>]) -> impl Iterator<Item = &'a [String]> {
    std::iter::once(header).chain(body.iter().map(|row| row.as_slice()))
}

fn cell(row: &[String], column: usize) -> Option<&str> {
    row.get(column).map(String::as_str)
}

/// The column widths that fit `width`, the widest columns yielding first.
///
/// # Invariant
///
/// The sum never exceeds the room the table was given, unless there is not one `MIN`
/// per column -- then the rows overflow and the log wraps them, which is the only
/// honest thing left. See `docs/notes/tui-table.md`.
fn fit(natural: &[usize], width: usize) -> Vec<usize> {
    let room = width.saturating_sub(1 + natural.len() * PADDING);
    if natural.iter().sum::<usize>() <= room {
        return natural.to_vec();
    }
    let mut widths = vec![MIN; natural.len()];
    let mut left = room.saturating_sub(MIN * natural.len());
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&column| natural[column]);
    for (index, &column) in order.iter().enumerate() {
        let share = left / (order.len() - index);
        let take = natural[column].saturating_sub(MIN).min(share);
        widths[column] = MIN + take;
        left -= take;
    }
    widths
}

/// One logical row as the rows its cells wrap into, each the table's whole width.
fn compose(cells: &[String], widths: &[usize], aligns: &[Align]) -> Vec<String> {
    let wrapped: Vec<Vec<String>> = widths
        .iter()
        .enumerate()
        .map(|(column, width)| wrap_cell(cell(cells, column).unwrap_or_default(), *width))
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    (0..height)
        .map(|line| {
            let mut out = String::from("|");
            for (column, width) in widths.iter().enumerate() {
                let text = wrapped[column].get(line).map(String::as_str).unwrap_or("");
                out.push(' ');
                out.push_str(&pad(text, *width, align_of(aligns, column)));
                out.push_str(" |");
            }
            out
        })
        .collect()
}

/// A cell's words as lines that fit `width` columns.
///
/// # Invariant
///
/// No word is dropped: one wider than its column is cut where the column ends. A
/// table row may not rely on the log wrapping it afterwards -- that would break the
/// columns the row is drawn in.
fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut used = 0usize;
    for word in text.split_whitespace() {
        if used > 0 && used + 1 + UnicodeWidthStr::width(word) > width {
            lines.push(String::new());
            used = 0;
        }
        if used > 0 {
            lines.last_mut().expect("a line is open").push(' ');
            used += 1;
        }
        for ch in word.chars() {
            let advance = ch.width().unwrap_or(0).max(1);
            if used + advance > width && used > 0 {
                lines.push(String::new());
                used = 0;
            }
            lines.last_mut().expect("a line is open").push(ch);
            used += advance;
        }
    }
    lines
}

/// `text` in a column `width` wide, by the alignment the source asked for.
fn pad(text: &str, width: usize, align: Align) -> String {
    let spare = width.saturating_sub(UnicodeWidthStr::width(text));
    let (left, right) = match align {
        Align::Right => (spare, 0),
        Align::Center => (spare / 2, spare - spare / 2),
        Align::None | Align::Left => (0, spare),
    };
    format!("{}{text}{}", " ".repeat(left), " ".repeat(right))
}

/// The row between header and body: dashes, and the `:` markers that align.
fn rule(widths: &[usize], aligns: &[Align]) -> String {
    let mut out = String::from("|");
    for (column, width) in widths.iter().enumerate() {
        let align = align_of(aligns, column);
        let colons = match align {
            Align::None => 0,
            Align::Left | Align::Right => 1,
            Align::Center => 2,
        };
        let dashes = width + 2 - colons;
        let body = "-".repeat(dashes);
        out.push_str(&match align {
            Align::None => body,
            Align::Left => format!(":{body}"),
            Align::Right => format!("{body}:"),
            Align::Center => format!(":{body}:"),
        });
        out.push('|');
    }
    out
}

fn align_of(aligns: &[Align], column: usize) -> Align {
    aligns.get(column).copied().unwrap_or(Align::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(text: &str) -> Vec<String> {
        text.split('|').map(str::to_string).collect()
    }

    /// Every drawn row is the width the table was given, and the columns line up.
    #[test]
    fn a_table_is_drawn_as_a_grid_inside_the_width_it_is_given() {
        let header = cells("name|what");
        let body = vec![cells("a|the first one"), cells("bb|c")];
        let aligns = vec![Align::Left, Align::Right];
        for width in [80usize, 24, 16] {
            let rows = rows(&header, &aligns, &body, width);
            let wide: Vec<usize> = rows
                .iter()
                .map(|row| UnicodeWidthStr::width(row.text.as_str()))
                .collect();
            assert!(wide.iter().all(|w| *w <= width), "{width}: {wide:?}");
            assert!(
                wide.windows(2).all(|pair| pair[0] == pair[1]),
                "{width}: every row is the same width: {wide:?}"
            );
            assert_eq!(rows.iter().filter(|row| row.rule).count(), 1, "{width}");
        }
    }

    /// A cell wider than its column wraps, and its row grows to hold it.
    #[test]
    fn a_long_cell_wraps_inside_its_column() {
        let header = cells("h|i");
        let body = vec![cells("one two three|x")];
        let rows = rows(&header, &[Align::None, Align::None], &body, 18);
        let drawn: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(drawn.len() > 3, "the row grew: {drawn:?}");
        assert!(
            drawn.iter().all(|row| row.contains('|')),
            "every row is a table row: {drawn:?}"
        );
    }

    /// The rule row spells the alignment the source spelled.
    #[test]
    fn the_rule_row_keeps_the_alignment_markers() {
        let widths = [4usize, 4, 4, 4];
        let aligns = [Align::None, Align::Left, Align::Center, Align::Right];
        assert_eq!(rule(&widths, &aligns), "|------|:-----|:----:|-----:|");
        for width in [0usize, 1, 2] {
            let rows = rows(&cells("a|b"), &[Align::None, Align::None], &[], width);
            assert!(rows.iter().all(|row| row.text.starts_with('|')));
        }
    }
}
