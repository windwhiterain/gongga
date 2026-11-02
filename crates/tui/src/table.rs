//! A markdown table fitted to the columns the window has.
//!
//! # Invariant
//!
//! Every row drawn is the same display width and no wider than it was given, so a
//! frame draws a table as a table instead of wrapping it into a paragraph. A cell's
//! runs survive the column: what ink its words were written in is what they keep.
//! See `docs/notes/tui-table.md`.

use ratatui::style::Style;
use ratatui::text::Line;

use crate::fold;
use crate::markdown;

/// How a column's cells sit inside it, as the source's `:` markers say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// No marker: the writer left the column to the reader, and it reads left.
    None,
    Left,
    Center,
    Right,
}

/// The narrowest a column is squeezed to: its rule row spends three dashes.
const MIN: usize = 3;

/// The columns one cell spends on its border and its air: `| a ` is three.
const PADDING: usize = 3;

/// The rows a table is drawn in: its header, a rule, then every body row under one.
pub fn rows(
    header: &[Line<'static>],
    aligns: &[Align],
    body: &[Vec<Line<'static>>],
    width: usize,
) -> Vec<Line<'static>> {
    let columns = header
        .len()
        .max(body.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return Vec::new();
    }
    let natural: Vec<usize> = (0..columns)
        .map(|column| {
            lines(header, body)
                .map(|row| cell(row, column).map_or(0, Line::width))
                .max()
                .unwrap_or(0)
                .max(MIN)
        })
        .collect();
    let widths = fit(&natural, width);
    let mut out = compose(header, &widths, aligns);
    // The header's rule is the one boundary that states a column's alignment.
    out.push(rule(&widths, aligns));
    for (index, row) in body.iter().enumerate() {
        // Dashes alone between two body rows, so a wrapped cell cannot read as the
        // row after it. See `docs/notes/tui-table.md`.
        if index > 0 {
            out.push(rule(&widths, &[]));
        }
        out.extend(compose(row, &widths, aligns));
    }
    out
}

/// The header first, then the body: the order the table is read in.
fn lines<'a>(
    header: &'a [Line<'static>],
    body: &'a [Vec<Line<'static>>],
) -> impl Iterator<Item = &'a [Line<'static>]> {
    std::iter::once(header).chain(body.iter().map(|row| row.as_slice()))
}

fn cell<'a>(row: &'a [Line<'static>], column: usize) -> Option<&'a Line<'static>> {
    row.get(column)
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
fn compose(cells: &[Line<'static>], widths: &[usize], aligns: &[Align]) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = widths
        .iter()
        .enumerate()
        .map(|(column, width)| match cell(cells, column) {
            Some(line) => fold::rows(line.clone(), *width),
            None => vec![Line::default()],
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    (0..height)
        .map(|row| {
            let mut out = Line::default();
            let style = markdown::marker_style();
            fold::push(&mut out, style, "|");
            for (column, width) in widths.iter().enumerate() {
                let line = wrapped[column].get(row);
                let used = line.map_or(0, Line::width);
                let spare = width.saturating_sub(used);
                let (before, after) = match aligns.get(column).copied().unwrap_or(Align::None) {
                    Align::Right => (spare, 0),
                    Align::Center => (spare / 2, spare - spare / 2),
                    Align::None | Align::Left => (0, spare),
                };
                // Between two borders: air, the alignment's padding, and the cell.
                fold::push(
                    &mut out,
                    Style::default(),
                    &format!(" {}", " ".repeat(before)),
                );
                for span in line.map(|line| line.spans.as_slice()).unwrap_or_default() {
                    fold::push(&mut out, span.style, span.content.as_ref());
                }
                fold::push(
                    &mut out,
                    Style::default(),
                    &format!("{} ", " ".repeat(after)),
                );
                fold::push(&mut out, style, "|");
            }
            out
        })
        .collect()
}

/// The row on a boundary between two logical rows: dashes, and the `:` markers
/// `aligns` states.
///
/// # Invariant
///
/// A segment spends `width + 2` columns, the same as a cell's: the colons are taken
/// out of the dash count rather than added, so a rule is exactly as wide as the rows
/// around it. An empty `aligns` marks nothing. See `docs/notes/tui-table.md`.
fn rule(widths: &[usize], aligns: &[Align]) -> Line<'static> {
    let style = markdown::marker_style();
    let mut out = Line::default();
    fold::push(&mut out, style, "|");
    for (column, width) in widths.iter().enumerate() {
        let align = aligns.get(column).copied().unwrap_or(Align::None);
        let colons = match align {
            Align::None => 0,
            Align::Left | Align::Right => 1,
            Align::Center => 2,
        };
        let body = "-".repeat(width + 2 - colons);
        let text = match align {
            Align::None => format!("{body}|"),
            Align::Left => format!(":{body}|"),
            Align::Right => format!("{body}:|"),
            Align::Center => format!(":{body}:|"),
        };
        fold::push(&mut out, style, &text);
    }
    out
}
