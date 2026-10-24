//! The scrollback: row offsets cached per line, so a frame never walks the log.
//!
//! # Invariant
//!
//! A frame touches only the visible rows, and an append lands on exactly the
//! rows a reflow of the whole line would. See `docs/notes/tui-log.md`.

use std::collections::HashMap;

use unicode_width::UnicodeWidthChar;

use crate::LineKind;

/// One block: a line that may still be growing, plus its rows.
///
/// # Invariant
///
/// `tag` holds the label the block opened with and is *not* part of `text`, so a
/// block whose first row is above the window can repeat it. See
/// `docs/notes/tui-log.md`.
struct Entry {
    kind: LineKind,
    text: String,
    /// Byte offset of each wrapped row's first character; its length is the row count.
    starts: Vec<usize>,
    /// Display columns used by the last row: the whole wrap state an append resumes.
    col: usize,
    /// No more text can arrive, so the block is scrollback.
    sealed: bool,
}

impl Entry {
    fn new(kind: LineKind, text: String, width: usize) -> Self {
        let mut entry = Self {
            kind,
            text,
            starts: vec![0],
            col: 0,
            sealed: false,
        };
        // Width 0 means "not measured yet": the first draw reflows at its own width.
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
            text, starts, col, ..
        } = self;
        starts.clear();
        // A line always has at least one row, so an empty line is a blank row.
        starts.push(0);
        let mut wrap = Wrap { width, col: 0 };
        wrap.feed(text, 0, starts);
        *col = wrap.col;
    }

    /// Adds text to the end of this line, continuing the wrap from the last row.
    fn append(&mut self, text: &str, width: usize) {
        let base = self.text.len();
        let Self {
            text: owned,
            starts,
            col,
            ..
        } = self;
        if width > 0 {
            let mut wrap = Wrap { width, col: *col };
            wrap.feed(text, base, starts);
            *col = wrap.col;
        }
        owned.push_str(text);
    }

    /// The text of row `row`, with the newline that ended it removed.
    ///
    /// # Invariant
    ///
    /// A row runs to the next row's start, one byte past the newline that broke
    /// it, so a trailing newline yields a final empty row. See
    /// `docs/notes/tui-log.md`.
    fn row(&self, row: usize) -> &str {
        let start = self.starts[row];
        let end = self.starts.get(row + 1).copied().unwrap_or(self.text.len());
        let text = &self.text[start..end];
        text.strip_suffix('\n').unwrap_or(text)
    }
}

/// The greedy wrap state machine, movable so an append can pick it up mid-line.
///
/// # Invariant
///
/// Wrapping depends only on the current row's column count, which is why one
/// number suffices and why streaming a long line stays linear.
struct Wrap {
    width: usize,
    col: usize,
}

impl Wrap {
    /// Feeds `text` into the wrapper, recording each row start shifted by `base`.
    fn feed(&mut self, text: &str, base: usize, starts: &mut Vec<usize>) {
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                starts.push(base + i + 1);
                self.col = 0;
                continue;
            }
            // A control character has no width but still advances one column.
            let w = ch.width().unwrap_or(0).max(1);
            if self.col + w > self.width && self.col > 0 {
                starts.push(base + i);
                self.col = 0;
            }
            self.col += w;
        }
    }
}

/// Prefix sums over the per-line row counts that survive a change in the middle.
///
/// # Invariant
///
/// The totals are current after any update, including one in the middle of the
/// log, which is what a streamed delta does. See `docs/notes/tui-log.md`.
#[derive(Default)]
struct RowIndex {
    /// 1-based Fenwick tree.
    tree: Vec<usize>,
}

impl RowIndex {
    /// Adds one line's row count at the end, carrying its whole range. O(log lines).
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
        self.shift(line, delta as isize);
    }

    /// Adds or removes rows on a line: a trim is the only caller that shrinks.
    fn shift(&mut self, line: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let mut i = line + 1;
        while i <= self.tree.len() {
            let next = self.tree[i - 1] as isize + delta;
            self.tree[i - 1] = next.max(0) as usize;
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

    /// The line holding 0-based row `row`; the line count when row is past the end.
    fn line_at(&self, row: usize) -> usize {
        let mut line = 0;
        let mut acc = 0;
        // The largest power of two that fits, for the binary lifting walk.
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

/// What one frame needs: the rows that fit, borrowed from the log, and the bound.
pub struct Viewport<'a> {
    pub rows: Vec<(LineKind, &'a str)>,
    /// Rows hidden above the window; only the renderer can work this out.
    pub max_scroll: usize,
}

/// The scrollback: sealed blocks, plus the ones still growing.
///
/// # Invariant
///
/// A block's position in `entries` is fixed when it opens, so two blocks that
/// stream at once each keep growing where they are instead of trading places.
/// See `docs/notes/tui-log.md`.
#[derive(Default)]
pub struct Log {
    entries: Vec<Entry>,
    index: RowIndex,
    /// Open stream key -> its block. Its identity, not its position.
    open: HashMap<String, usize>,
    /// The width the cached row offsets were measured at; 0 until a frame is drawn.
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
        let mut entry = Entry::new(kind, text.into(), self.width);
        entry.sealed = true;
        self.index.push(entry.rows());
        self.entries.push(entry);
    }

    /// Adds to the block `key` names, opening it with `tag` on first use.
    ///
    /// # Invariant
    ///
    /// `done` seals the block and forgets the key, so a later chunk under the same
    /// key opens a new block rather than appending to the closed one. An empty
    /// first chunk still opens the block, so a slow token keeps its label.
    pub fn stream(&mut self, key: &str, kind: LineKind, tag: &str, text: &str, done: bool) {
        let width = self.width;
        let line = match self.open.get(key).copied() {
            Some(line) => {
                let before = self.entries[line].rows();
                self.entries[line].append(text, width);
                let after = self.entries[line].rows();
                self.index.add(line, after.saturating_sub(before));
                line
            }
            // A blank chunk must not open a block: the shell writes one
            // before every exit sentinel.
            None if text.trim().is_empty() => {
                if done {
                    self.open.remove(key);
                }
                return;
            }
            None => {
                let mut entry = Entry::new(kind, format!("{tag}{text}"), width);
                entry.sealed = false;
                self.index.push(entry.rows());
                self.entries.push(entry);
                let line = self.entries.len() - 1;
                self.open.insert(key.to_string(), line);
                line
            }
        };
        if done {
            self.open.remove(key);
            self.trim_tail(line);
            self.entries[line].sealed = true;
        }
    }

    /// Drops the blank tail a finished block ends on.
    ///
    /// # Invariant
    ///
    /// A sealed block never ends in a blank row, so the newline that closes a
    /// command's output cannot leave a gap before the next block. Trimming the
    /// text rather than the rows keeps this correct at a width nobody measured
    /// yet: `reflow` derives the rows from the trimmed text afterwards.
    fn trim_tail(&mut self, line: usize) {
        let width = self.width;
        let entry = &mut self.entries[line];
        let before = entry.rows();
        let kept = entry.text.trim_end().len();
        if kept == entry.text.len() {
            return;
        }
        entry.text.truncate(kept);
        if width > 0 {
            entry.reflow(width);
        } else {
            entry.starts = vec![0];
        }
        let after = entry.rows();
        self.index.shift(line, after as isize - before as isize);
    }

    /// Appends to line `line`; a missing line is ignored rather than panicked on.
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

    /// How many rows the log wraps into at `width`, reflowing first if it moved.
    pub fn rows(&mut self, width: usize) -> usize {
        self.reflow(width);
        self.index.total()
    }

    /// The rows for one frame: the last `height` rows of the log, in order.
    ///
    /// # Invariant
    ///
    /// A block is one line however long it grows, so the window is simply the
    /// tail of the log: no block is reserved space, and none is cut out.
    pub fn window(&mut self, width: usize, height: usize, scroll: usize) -> Viewport<'_> {
        self.reflow(width);
        let total = self.index.total();
        let max_scroll = total.saturating_sub(height);
        let scroll = scroll.min(max_scroll);
        let end = total - scroll;
        let start = end.saturating_sub(height);

        let mut rows: Vec<(LineKind, &str)> = Vec::with_capacity(end - start);
        if start < end {
            // Jump to the block holding the first visible row: O(log blocks).
            let mut line = self.index.line_at(start).min(self.entries.len() - 1);
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

    /// Recomputes every row offset when the width changed: the only whole-log walk.
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

    /// Opens a block and streams `chunks` into it, sealing it on the last one.
    fn block(log: &mut Log, key: &str, tag: &str, chunks: &[&str]) {
        for (i, chunk) in chunks.iter().enumerate() {
            log.stream(key, LineKind::Reasoning, tag, chunk, i + 1 == chunks.len());
        }
    }

    /// Adds one delta to a block that stays open, the way a token arrives.
    fn grow(log: &mut Log, key: &str, tag: &str, chunk: &str) {
        log.stream(key, LineKind::Reasoning, tag, chunk, false);
    }

    /// Two blocks streaming at once each keep growing, in the order they opened.
    #[test]
    fn two_open_blocks_grow_together() {
        let mut log = Log::new();
        grow(&mut log, "a", "[peer] ", "xx");
        grow(&mut log, "b", "[test1] ", "yy");
        assert_eq!(shown(&mut log, 12, 4, 0), vec!["[peer] xx", "[test1] yy"]);

        // The first block grows in place: the second one stays below it.
        grow(&mut log, "a", "[peer] ", "zzzzzz");
        assert_eq!(
            shown(&mut log, 12, 4, 0),
            vec!["[peer] xxzzz", "zzz", "[test1] yy"]
        );
    }

    /// The window is the tail of the log: a growing block pushes rows up, and
    /// scrolling back finds them again.
    #[test]
    fn a_growing_block_scrolls_the_window() {
        let mut log = log_of(&["old1", "old2", "old3"], 20);
        block(&mut log, "a", "[peer] ", &["live"]);
        assert_eq!(
            shown(&mut log, 20, 3, 0),
            vec!["old2", "old3", "[peer] live"]
        );
        // No rows were lost: they went above the window, not out of the log.
        assert_eq!(shown(&mut log, 20, 3, 1), vec!["old1", "old2", "old3"]);
    }

    /// A block taller than the window shows its newest rows, like any other tail.
    #[test]
    fn a_long_block_shows_its_tail() {
        let mut log = Log::new();
        block(
            &mut log,
            "a",
            "[peer] ",
            &["abcdefgh", "ijklmnop", "qrstuvwx"],
        );
        let all = shown(&mut log, 12, 100, 0);
        assert_eq!(all.len(), 3, "three rows at width 12: {}", all.join("|"));
        // Two rows: the two newest. The label scrolled off with the head.
        assert_eq!(shown(&mut log, 12, 2, 0), all[1..].to_vec());
    }

    /// A blank chunk must not open a block.
    ///
    /// # Invariant
    ///
    /// The shell writes one empty line before every exit sentinel, so without
    /// this every command leaves a blank row on screen.
    #[test]
    fn a_blank_chunk_opens_no_block() {
        let mut log = Log::new();
        // Exactly what a finished command's stderr stream looks like.
        log.stream("t:c1:Stderr", LineKind::ToolError, "", "\n", false);
        log.stream("t:c1:Stderr", LineKind::ToolError, "", "", true);
        assert_eq!(log.len(), 0, "a blank chunk made a row");
        assert!(log.open.is_empty(), "the key was left open");
    }

    /// A sealed block does not end in a blank row, even when its output did.
    #[test]
    fn a_sealed_block_has_no_trailing_blank_row() {
        let mut log = Log::new();
        log.stream("t:c1:Stdout", LineKind::ToolOutput, "", "out\n", false);
        log.stream("t:c1:Stdout", LineKind::ToolOutput, "", "\n", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["out"]);
        // A blank line in the middle is still part of the block.
        let mut log = Log::new();
        log.stream("t:c2:Stdout", LineKind::ToolOutput, "", "a\n\nb", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["a", "", "b"]);
    }

    /// A window can start mid-line, which a "count lines, not rows" renderer gets wrong.
    #[test]
    fn scrolling_moves_one_row_at_a_time() {
        // Five lines that each take two rows at width 4: "aaaa"/"aaa".
        let mut log = log_of(&["aaaaaaa", "bbbbbbb", "ccccccc", "ddddddd", "eeeeeee"], 4);
        assert_eq!(log.rows(4), 10);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["ddd", "eeee", "eee"]);
        assert_eq!(shown(&mut log, 4, 3, 1), vec!["dddd", "ddd", "eeee"]);
        assert_eq!(shown(&mut log, 4, 3, 2), vec!["ccc", "dddd", "ddd"]);
    }

    /// Both ends stop the scroll: past the top and past the bottom, never wrap around.
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

    /// A frame must not depend on how long the log is, or wrapping per frame returns.
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

    /// The binary search must agree with a plain walk, or scrolling lands wrong.
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
