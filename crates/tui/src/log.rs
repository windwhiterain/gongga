//! The scrollback, and the arithmetic that turns a scroll offset into rows.
//!
//! Why this is a structure rather than a `Vec<Line>`: a conversation grows
//! without bound, and everything the renderer does happens once per frame —
//! which, while a response streams, is many times a second. Wrapping the whole
//! log on every frame is what made a long conversation unusable: the cost of one
//! keystroke grew with the number of messages already on screen, because each
//! frame re-wrapped every line of the history and allocated a string per
//! *character* of it (the old `wrap` went through `char::to_string` to measure a
//! char). Five thousand lines meant millions of allocations per frame.
//!
//! So nothing on the path a frame takes may walk the log. Three caches make
//! that true:
//!
//! * A line caches the byte offset at which each of its wrapped rows starts
//!   ([`Entry::starts`]). Drawing a row is then a slice, not a re-wrap, so a
//!   frame costs what is *visible* and nothing else.
//! * Appending to a line — what streaming does, over and over — resumes that
//!   state machine from the last row instead of restarting it, so a delta costs
//!   what the delta is worth, not what the line is worth. Without this, a long
//!   streamed answer would be re-wrapped from the top on every token: quadratic
//!   in the answer's own length.
//! * A Fenwick tree over the per-line row counts answers "which line holds row
//!   N" in O(log lines) instead of walking. Scrolling to the top of a long log
//!   therefore costs the same as scrolling one notch at the bottom.
//!
//! The one operation that has to touch everything is a width change, because
//! every cached offset was measured for the old width. That happens when the
//! window changes shape and nowhere else.

use unicode_width::UnicodeWidthChar;

use crate::LineKind;

/// One line of the log, plus what it takes to draw a row of it without wrapping
/// the whole line again.
struct Entry {
    kind: LineKind,
    text: String,
    /// Byte offset of the first character of each wrapped row. The length of
    /// this list *is* the row count, which is what the row index sums over.
    starts: Vec<usize>,
    /// Display columns used by the last row: where an append resumes. Appending
    /// can only ever extend the last row or open new ones, so this one number
    /// plus the last start is the whole wrap state.
    open: usize,
}

impl Entry {
    fn new(kind: LineKind, text: String, width: usize) -> Self {
        let mut entry = Self {
            kind,
            text,
            starts: vec![0],
            open: 0,
        };
        // Width 0 means "not measured yet" — the first draw reflows everything
        // at the width it actually has.
        if width > 0 {
            entry.reflow(width);
        }
        entry
    }

    fn rows(&self) -> usize {
        self.starts.len()
    }

    /// Recomputes every row offset at `width`.
    fn reflow(&mut self, width: usize) {
        let Self {
            text, starts, open, ..
        } = self;
        starts.clear();
        // A line always has at least one row, even when it is empty: that is
        // what makes an empty line a blank row rather than nothing at all.
        starts.push(0);
        let mut wrap = Wrap { width, col: 0 };
        wrap.feed(text, 0, starts);
        *open = wrap.col;
    }

    /// Adds text to the end of this line, continuing the wrap from the last row.
    fn append(&mut self, text: &str, width: usize) {
        let base = self.text.len();
        let Self {
            text: owned,
            starts,
            open,
            ..
        } = self;
        if width > 0 {
            let mut wrap = Wrap { width, col: *open };
            wrap.feed(text, base, starts);
            *open = wrap.col;
        }
        owned.push_str(text);
    }

    /// The text of row `row`, with the newline that ended it removed.
    ///
    /// The row runs from its own start to the next row's start, which is one
    /// byte past the newline that forced the break — so trimming a trailing
    /// newline is exactly the difference between the two offsets. That is also
    /// why a trailing newline yields a final empty row: it starts at the end of
    /// the text and has nothing after it.
    fn row(&self, row: usize) -> &str {
        let start = self.starts[row];
        let end = self.starts.get(row + 1).copied().unwrap_or(self.text.len());
        let text = &self.text[start..end];
        text.strip_suffix('\n').unwrap_or(text)
    }
}

/// The greedy wrap state machine, movable so an append can pick it up mid-line.
///
/// Greedy wrapping only depends on the current row's column count, which is why
/// this is one number and why streaming a long line stays linear rather than
/// quadratic.
struct Wrap {
    width: usize,
    col: usize,
}

impl Wrap {
    /// Feeds `text` through the machine, recording the start of every row it
    /// opens in `starts`. `base` is where `text` begins inside the line, so the
    /// offsets land in line coordinates either way — a reflow passes 0, an
    /// append passes the current length.
    fn feed(&mut self, text: &str, base: usize, starts: &mut Vec<usize>) {
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                starts.push(base + i + 1);
                self.col = 0;
                continue;
            }
            // A control character has no width but still advances the row by
            // one, or a row of invisible bytes would never break.
            let w = ch.width().unwrap_or(0).max(1);
            if self.col + w > self.width && self.col > 0 {
                starts.push(base + i);
                self.col = 0;
            }
            self.col += w;
        }
    }
}

/// Prefix sums over the per-line row counts.
///
/// A plain `Vec` of running totals would work for appends and break on streams:
/// a streamed delta changes a line in the *middle* of the log whenever a tool
/// output and a response interleave, and every total after it would be wrong.
/// A tree pays O(log lines) for both the update and the lookup instead.
#[derive(Default)]
struct RowIndex {
    /// 1-based Fenwick tree.
    tree: Vec<usize>,
}

impl RowIndex {
    /// Adds one line's row count at the end. O(log lines).
    ///
    /// The new node has to carry its whole range, not just its own count: a
    /// Fenwick node covers `[i - lowbit(i) + 1, i]`, and the lines it covers
    /// were added before it existed. Filling that in from the prefix sums is
    /// what keeps "which line holds row N" honest after an append.
    fn push(&mut self, rows: usize) {
        let i = self.tree.len() + 1;
        let floor = i - (i & i.wrapping_neg());
        let below = self.prefix(i - 1) - self.prefix(floor);
        self.tree.push(below + rows);
    }

    /// Adds `delta` rows to line `line` (0-based). O(log lines).
    fn add(&mut self, line: usize, delta: usize) {
        if delta == 0 {
            return;
        }
        let mut i = line + 1;
        while i <= self.tree.len() {
            self.tree[i - 1] += delta;
            i += i & i.wrapping_neg();
        }
    }

    fn total(&self) -> usize {
        self.prefix(self.tree.len())
    }

    /// Rows held by the first `count` lines.
    fn prefix(&self, count: usize) -> usize {
        let mut i = count.min(self.tree.len());
        let mut sum = 0;
        while i > 0 {
            sum += self.tree[i - 1];
            i -= i & i.wrapping_neg();
        }
        sum
    }

    /// The line holding 0-based row `row`: the first line whose running total
    /// ends past it. Returns the line count when `row` is past the end, so the
    /// caller bounds it.
    fn line_at(&self, row: usize) -> usize {
        let mut line = 0;
        let mut acc = 0;
        // The largest power of two that fits: the classic binary lifting start.
        let mut step = if self.tree.is_empty() {
            0
        } else {
            1usize << (usize::BITS - 1 - self.tree.len().leading_zeros())
        };
        while step > 0 {
            let next = line + step;
            if next <= self.tree.len() && acc + self.tree[next - 1] <= row {
                line = next;
                acc += self.tree[next - 1];
            }
            step >>= 1;
        }
        line
    }
}

/// What one frame needs: the rows that fit, borrowed from the log, and how far
/// the log can be scrolled.
pub struct Viewport<'a> {
    pub rows: Vec<(LineKind, &'a str)>,
    /// Rows hidden above the window when it is pinned to the newest line. The
    /// key handler clamps against this, and only the renderer can work it out.
    pub max_scroll: usize,
}

/// The scrollback.
#[derive(Default)]
pub struct Log {
    entries: Vec<Entry>,
    index: RowIndex,
    /// The width the cached row offsets were measured at. 0 until a frame has
    /// been drawn, because until then nobody knows how wide the terminal is.
    width: usize,
}

impl Log {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds a finished line. Costs what the line is worth, once.
    pub fn push(&mut self, kind: LineKind, text: impl Into<String>) {
        let entry = Entry::new(kind, text.into(), self.width);
        self.index.push(entry.rows());
        self.entries.push(entry);
    }

    /// Appends to line `line`, which is what a streaming response and a
    /// streaming tool output do for every chunk.
    ///
    /// A missing line is ignored rather than panicked on: a stream key outlives
    /// its line only if the caller cleared the log underneath it, and losing a
    /// chunk is a better failure than taking the window down.
    pub fn append(&mut self, line: usize, text: &str) {
        let width = self.width;
        let Some(entry) = self.entries.get_mut(line) else {
            return;
        };
        let before = entry.rows();
        entry.append(text, width);
        let after = entry.rows();
        self.index.add(line, after.saturating_sub(before));
    }

    /// How many rows the log wraps into at `width`, reflowing first if the
    /// width moved since the offsets were measured.
    pub fn rows(&mut self, width: usize) -> usize {
        self.reflow(width);
        self.index.total()
    }

    /// The rows for one frame: everything visible, nothing more.
    ///
    /// `scroll` counts rows up from the newest line, already what the key
    /// handler meant; it is clamped here because lines can be added or the
    /// window can grow between the key and the frame.
    pub fn window(&mut self, width: usize, height: usize, scroll: usize) -> Viewport<'_> {
        self.reflow(width);
        let total = self.index.total();
        let max_scroll = total.saturating_sub(height);
        let scroll = scroll.min(max_scroll);
        let end = total - scroll;
        let start = end.saturating_sub(height);
        let mut rows = Vec::with_capacity(end - start);
        if start < end {
            let mut line = self.index.line_at(start).min(self.entries.len() - 1);
            // The row index of this line's first row; carried forward instead
            // of asked for again, so the walk is one pass over the window.
            let mut first = self.index.prefix(line);
            loop {
                let entry = &self.entries[line];
                let next = first + entry.rows();
                for row in start.max(first)..end.min(next) {
                    rows.push((entry.kind, entry.row(row - first)));
                }
                if next >= end || line + 1 >= self.entries.len() {
                    break;
                }
                first = next;
                line += 1;
            }
        }
        Viewport { rows, max_scroll }
    }

    /// Recomputes every row offset when the width changed. The only whole-log
    /// walk left: a resize is a shape change, not something a frame does.
    fn reflow(&mut self, width: usize) {
        let width = width.max(1);
        if width == self.width {
            return;
        }
        self.width = width;
        self.index = RowIndex::default();
        for entry in &mut self.entries {
            entry.reflow(width);
            self.index.push(entry.rows());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_of(lines: &[&str], width: usize) -> Log {
        let mut log = Log::new();
        for line in lines {
            log.push(LineKind::System, *line);
        }
        log.rows(width);
        log
    }

    fn shown(log: &mut Log, width: usize, height: usize, scroll: usize) -> Vec<String> {
        log.window(width, height, scroll)
            .rows
            .iter()
            .map(|(_, text)| text.to_string())
            .collect()
    }

    #[test]
    fn rows_break_at_the_column_limit() {
        let mut log = log_of(&["abcdefghij"], 4);
        assert_eq!(log.rows(4), 3);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        let mut log = log_of(&["中文ab"], 5);
        assert_eq!(shown(&mut log, 5, 2, 0), vec!["中文a", "b"]);
    }

    #[test]
    fn embedded_newlines_force_breaks() {
        assert_eq!(shown(&mut log_of(&["a\nb"], 10), 10, 2, 0), vec!["a", "b"]);
        // A trailing newline is an empty row, not a dropped one: that is how a
        // blank line between two paragraphs survives the round trip.
        assert_eq!(shown(&mut log_of(&["a\n"], 10), 10, 2, 0), vec!["a", ""]);
    }

    #[test]
    fn empty_text_is_one_blank_row() {
        let mut log = log_of(&[""], 10);
        assert_eq!(log.rows(10), 1);
        assert_eq!(shown(&mut log, 10, 1, 0), vec![""]);
    }

    /// The invariant the streaming path depends on: appending chunk by chunk
    /// must land on exactly the rows a single reflow of the whole line would.
    ///
    /// This is the bug that would otherwise come back as text jumping around
    /// mid-answer, and it is invisible in a screenshot because it only shows up
    /// on lines that wrap.
    #[test]
    fn appending_equals_rewrapping_from_the_top() {
        let cases: &[(&str, &[&str])] = &[
            ("abcdef", &["abc", "def"]),
            ("abcdefghij", &["abcde", "fghij"]),
            ("abcde", &["fgh", "ij"]),
            ("a\nbcdef", &["\nbc", "def"]),
            ("abcdef\n", &["\n", "x"]),
            ("中文中", &["文", "abcdef"]),
            ("", &["abcdefg"]),
            ("abc\ndef", &["", "\n", "ghi"]),
            ("abcd", &["e", "f", "ghijkl"]),
        ];
        for (first, chunks) in cases {
            for width in [1usize, 2, 3, 5, 7, 10] {
                let mut streamed = Log::new();
                streamed.push(LineKind::Assistant, *first);
                streamed.rows(width);
                let mut whole = String::from(*first);
                for chunk in *chunks {
                    streamed.append(0, chunk);
                    whole.push_str(chunk);
                }
                let expected = log_of(&[whole.as_str()], width).rows(width);
                assert_eq!(
                    streamed.rows(width),
                    expected,
                    "streamed {:?} at width {width} wrapped differently from the whole line",
                    whole
                );
                // Same row count is not enough: the text has to line up too.
                let fresh = log_of(&[whole.as_str()], width);
                for row in 0..expected {
                    let streamed_row = streamed.entries[0].row(row);
                    let whole_row = fresh.entries[0].row(row);
                    assert_eq!(
                        streamed_row, whole_row,
                        "row {row} of {:?} at width {width}",
                        whole
                    );
                }
            }
        }
    }

    /// A window can start in the middle of a line, which is the case a naive
    /// "count lines, not rows" renderer gets wrong.
    #[test]
    fn scrolling_moves_one_row_at_a_time() {
        // Five lines that each take two rows at width 4: "aaaa"/"aaa".
        let mut log = log_of(&["aaaaaaa", "bbbbbbb", "ccccccc", "ddddddd", "eeeeeee"], 4);
        assert_eq!(log.rows(4), 10);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["ddd", "eeee", "eee"]);
        assert_eq!(shown(&mut log, 4, 3, 1), vec!["dddd", "ddd", "eeee"]);
        assert_eq!(shown(&mut log, 4, 3, 2), vec!["ccc", "dddd", "ddd"]);
    }

    /// A page up from the bottom must show the rows just above the window, and
    /// scrolling past the top must stop at the first row — not wrap around.
    #[test]
    fn scroll_clamps_at_both_ends() {
        let mut log = log_of(&["aaaaaaa", "bbbbbbb"], 4);
        assert_eq!(log.rows(4), 4);
        // Height 2 sees the last two rows: "bbbb" then "bbb".
        assert_eq!(shown(&mut log, 4, 2, 0), vec!["bbbb", "bbb"]);
        assert_eq!(shown(&mut log, 4, 2, 1), vec!["aaa", "bbbb"]);
        assert_eq!(shown(&mut log, 4, 2, 99), vec!["aaaa", "aaa"]);
        assert_eq!(log.window(4, 2, 99).max_scroll, 2);
    }

    /// A frame must not depend on how long the log is: this is the assertion
    /// that catches a return to wrapping everything per frame.
    #[test]
    fn a_frame_touches_only_a_screenful() {
        let mut log = Log::new();
        for i in 0..50_000 {
            log.push(LineKind::System, format!("line {i} with some words on it"));
        }
        let height = 24;
        let view = log.window(80, height, 0);
        assert_eq!(view.rows.len(), height);
        // And from the bottom of a scrolled view too.
        let view = log.window(80, height, 30_000);
        assert_eq!(view.rows.len(), height);
    }

    #[test]
    fn a_width_change_reflows_every_line() {
        let mut log = log_of(&["abcdefghij"], 10);
        assert_eq!(log.rows(10), 1);
        assert_eq!(log.rows(4), 3);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["abcd", "efgh", "ij"]);
        // Back to the wide shape: the old offsets must not survive.
        assert_eq!(log.rows(10), 1);
        assert_eq!(shown(&mut log, 10, 1, 0), vec!["abcdefghij"]);
    }

    /// The row index's binary search has to agree with a plain walk, or
    /// scrolling lands on the wrong line while everything else looks fine.
    #[test]
    fn the_row_index_agrees_with_a_walk() {
        let mut log = Log::new();
        for i in 0..200usize {
            log.push(LineKind::System, "x".repeat(i % 17));
        }
        log.rows(8);
        let mut expected = Vec::new();
        for (line, entry) in log.entries.iter().enumerate() {
            for _ in 0..entry.rows() {
                expected.push(line);
            }
        }
        let mut row = 0;
        for (line, entry) in log.entries.iter().enumerate() {
            assert_eq!(log.index.prefix(line), row, "start of line {line}");
            for _ in 0..entry.rows() {
                assert_eq!(log.index.line_at(row), expected[row], "row {row}");
                row += 1;
            }
        }
        assert_eq!(log.index.total(), row);
    }
}
