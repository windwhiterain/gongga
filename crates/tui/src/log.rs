//! The scrollback: row offsets cached per line, so a frame never walks the log.
//!
//! # Invariant
//!
//! A frame touches only the visible rows, and an append lands on exactly the
//! rows a reflow of the whole line would. See `docs/notes/tui-log.md`.

use std::borrow::Cow;
use std::collections::HashMap;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::LineKind;

/// One block: a line that may still be growing, plus its rows.
///
/// # Invariant
///
/// `tag` holds the label the block opened with and is *not* part of `text`, so a
/// block whose first row is above the window can repeat it. `window` is the name the
/// line carried, and `at` is where the block sits inside that window's own rows.
struct Entry {
    kind: LineKind,
    /// The window this block belongs to, as the line said.
    window: String,
    /// The label this block was opened with, printed on its top visible row.
    tag: String,
    text: String,
    /// Byte offset of each wrapped row's first character; its length is the row count.
    starts: Vec<usize>,
    /// Display columns used by the last row: the whole wrap state an append resumes.
    col: usize,
    /// No more text can arrive, so the block is scrollback.
    sealed: bool,
    /// Where this block sits among the blocks of its own window.
    at: usize,
}

impl Entry {
    fn new(window: &str, kind: LineKind, text: String, width: usize) -> Self {
        Self::with_tag(window, kind, "", text, width)
    }

    /// A block that streams under `tag`: the label is not text, so no cut can take it.
    fn with_tag(window: &str, kind: LineKind, tag: &str, text: String, width: usize) -> Self {
        let mut entry = Self {
            kind,
            window: window.to_string(),
            tag: tag.to_string(),
            text,
            starts: vec![0],
            col: 0,
            sealed: false,
            at: 0,
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

    /// Recomputes every row offset at `width`, with the label's columns reserved on
    /// the row it is printed on.
    fn reflow(&mut self, width: usize) {
        let Self {
            tag,
            text,
            starts,
            col,
            ..
        } = self;
        starts.clear();
        // A line always has at least one row, so an empty line is a blank row.
        starts.push(0);
        let label = tag.width().min(width);
        let mut wrap = Wrap { width, col: label };
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

    /// One row as a frame shows it, with the block's label on the row the window
    /// starts with.
    ///
    /// # Invariant
    ///
    /// Only the top row of a block carries its label, and a clipped tail loses
    /// nothing: those characters start the row below it.
    fn view_row(&self, row: usize, labelled: bool) -> Cow<'_, str> {
        if !labelled || self.tag.is_empty() {
            return Cow::Borrowed(self.row(row));
        }
        Cow::Owned(format!("{}{}", self.tag, self.row(row)))
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
    pub rows: Vec<(LineKind, Cow<'a, str>)>,
    /// Rows hidden above the window; only the renderer can work this out.
    pub max_scroll: usize,
}

/// Which view of the log a frame is asking about.
///
/// # Invariant
///
/// A view is a way of reading the same blocks, never a second copy of them: a line
/// arrives once, and every view that can show it already does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View<'a> {
    /// Every block in arrival order: the events area undivided.
    All,
    /// One window's own blocks, in the order they arrived there.
    Window(&'a str),
}

/// One view's row bookkeeping over the shared blocks.
#[derive(Default)]
struct Ledger {
    /// Global index of this view's k-th block, in arrival order.
    blocks: Vec<usize>,
    index: RowIndex,
    /// Rows one block may fill in this view; 0 defers to the log's `block_size`.
    cap: usize,
}

/// The scrollback: blocks in arrival order, sealed and still-growing, plus one
/// ledger per view over them.
///
/// # Invariant
///
/// A block's position in `entries` is fixed when it opens, so two blocks that
/// stream at once each keep growing where they are instead of trading places.
/// See `docs/notes/tui-log.md`.
#[derive(Default)]
pub struct Log {
    entries: Vec<Entry>,
    /// The view over everything; its `blocks` is `0..entries.len()`.
    all: Ledger,
    /// One ledger per window name a line has carried.
    views: HashMap<String, Ledger>,
    /// Open stream key -> its block. Its identity, not its position.
    open: HashMap<String, usize>,
    /// The width the cached row offsets were measured at; 0 until a frame is drawn.
    width: usize,
    /// How many rows one block may fill. See [`Log::set_block_size`].
    block_size: usize,
}

/// The rows of a block that fills every row it has.
const NO_CAP: usize = usize::MAX;

impl Log {
    pub fn new() -> Self {
        Self::default()
    }

    /// Caps how tall a block may get: `0` caps nothing, and a block past its cap
    /// keeps its newest rows.
    pub fn set_block_size(&mut self, lines: usize) {
        if self.block_size == lines {
            return;
        }
        self.block_size = lines;
        self.rebuild();
    }

    /// Caps one window's blocks whatever the log-wide cap says: the command line's
    /// blocks are single lines by definition.
    pub fn set_cap(&mut self, window: &str, lines: usize) {
        let Some(ledger) = self.views.get_mut(window) else {
            return;
        };
        if ledger.cap == lines {
            return;
        }
        ledger.cap = lines;
        self.rebuild();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The rows one block of `kind` may fill under `cap`: the answer keeps its own.
    fn room(&self, kind: LineKind, cap: usize) -> usize {
        if kind == LineKind::Assistant {
            return NO_CAP;
        }
        if cap > 0 {
            return cap;
        }
        if self.block_size > 0 {
            self.block_size
        } else {
            NO_CAP
        }
    }

    /// The rows block `at` contributes to the two views that hold it: the merged one
    /// and its own window.
    fn contributions(&self, at: usize) -> (usize, usize) {
        let entry = &self.entries[at];
        let rows = entry.rows();
        let cap = self
            .views
            .get(&entry.window)
            .map(|ledger| ledger.cap)
            .unwrap_or(0);
        (
            rows.min(self.room(entry.kind, self.all.cap)),
            rows.min(self.room(entry.kind, cap)),
        )
    }

    /// Moves both views' sums by what a change to one block did to them.
    fn shift(&mut self, at: usize, before: (usize, usize), after: (usize, usize)) {
        self.all
            .index
            .shift(at, after.0 as isize - before.0 as isize);
        let window = self.entries[at].window.clone();
        let pos = self.entries[at].at;
        if let Some(ledger) = self.views.get_mut(&window) {
            ledger
                .index
                .shift(pos, after.1 as isize - before.1 as isize);
        }
    }

    /// Adds a finished line to the window `name` names. Costs what the line is worth.
    pub fn push(&mut self, name: &str, kind: LineKind, text: impl Into<String>) {
        let mut entry = Entry::new(name, kind, text.into(), self.width);
        entry.sealed = true;
        self.entries.push(entry);
        self.attach(self.entries.len() - 1);
    }

    /// Records block `at` in the merged view and in its own window's view.
    fn attach(&mut self, at: usize) {
        let (merged, own) = self.contributions(at);
        let window = self.entries[at].window.clone();
        self.all.blocks.push(at);
        self.all.index.push(merged);
        self.views.entry(window.clone()).or_default();
        let pos = {
            let ledger = self
                .views
                .get_mut(&window)
                .expect("the view was just made sure of");
            let pos = ledger.blocks.len();
            ledger.blocks.push(at);
            ledger.index.push(own);
            pos
        };
        self.entries[at].at = pos;
    }

    /// Adds to the block `key` names, opening it with `tag` in window `name` on
    /// first use.
    ///
    /// # Invariant
    ///
    /// A block is one label's text: a chunk wearing another label opens a new
    /// block, because gluing two events onto one row reads as a word that was never
    /// typed. `done` seals the block and forgets the key, so a later chunk under the
    /// same key opens a new block rather than appending to the closed one. An empty
    /// first chunk still opens the block, so a slow token keeps its label.
    pub fn stream(
        &mut self,
        name: &str,
        key: &str,
        kind: LineKind,
        tag: &str,
        text: &str,
        done: bool,
    ) {
        let width = self.width;
        if let Some(at) = self.open.get(key).copied() {
            if self.entries[at].tag != tag {
                self.seal(key, at);
            }
        }
        let at = match self.open.get(key).copied() {
            Some(at) => {
                let before = self.contributions(at);
                self.entries[at].append(text, width);
                let after = self.contributions(at);
                self.shift(at, before, after);
                at
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
                self.entries
                    .push(Entry::with_tag(name, kind, tag, text.to_string(), width));
                let at = self.entries.len() - 1;
                self.attach(at);
                self.open.insert(key.to_string(), at);
                at
            }
        };
        if done {
            self.seal(key, at);
        }
    }

    /// Ends a block: no later chunk may append to it.
    fn seal(&mut self, key: &str, at: usize) {
        self.open.remove(key);
        self.trim_tail(at);
        self.entries[at].sealed = true;
    }

    /// Drops the blank tail a finished block ends on.
    ///
    /// # Invariant
    ///
    /// A sealed block never ends in a blank row, so the newline that closes a
    /// command's output cannot leave a gap before the next block. Trimming the
    /// text rather than the rows keeps this correct at a width nobody measured
    /// yet: `reflow` derives the rows from the trimmed text afterwards.
    fn trim_tail(&mut self, at: usize) {
        let width = self.width;
        let before = self.contributions(at);
        let entry = &mut self.entries[at];
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
        let after = self.contributions(at);
        self.shift(at, before, after);
    }

    /// Appends to block `at`; a missing block is ignored rather than panicked on.
    pub fn append(&mut self, at: usize, text: &str) {
        if at >= self.entries.len() {
            return;
        }
        let width = self.width;
        let before = self.contributions(at);
        self.entries[at].append(text, width);
        let after = self.contributions(at);
        self.shift(at, before, after);
    }

    /// The ledger a view reads, or `None` for a window nobody has written to yet.
    fn ledger(&self, view: View<'_>) -> Option<&Ledger> {
        match view {
            View::All => Some(&self.all),
            View::Window(name) => self.views.get(name),
        }
    }

    /// How many rows this view holds at `width`, reflowing first if the width moved.
    pub fn rows(&mut self, view: View<'_>, width: usize) -> usize {
        self.reflow(width);
        self.ledger(view)
            .map(|ledger| ledger.index.total())
            .unwrap_or(0)
    }

    /// The rows for one frame of one view: the last `height` rows, in order.
    ///
    /// # Invariant
    ///
    /// The view is the tail of its own ledger, and a block contributes only the rows
    /// its cap allows: no block is reserved space, and none is cut out mid-pass.
    pub fn window(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
    ) -> Viewport<'_> {
        self.reflow(width);
        let Some(ledger) = self.ledger(view) else {
            return Viewport {
                rows: Vec::new(),
                max_scroll: 0,
            };
        };
        let total = ledger.index.total();
        let max_scroll = total.saturating_sub(height);
        let scroll = scroll.min(max_scroll);
        let end = total - scroll;
        let start = end.saturating_sub(height);

        let mut rows: Vec<(LineKind, Cow<'_, str>)> = Vec::with_capacity(end - start);
        if start < end {
            // Jump to the block holding the first visible row: O(log blocks).
            let mut pos = ledger.index.line_at(start).min(ledger.blocks.len() - 1);
            let mut first = ledger.index.prefix(pos);
            loop {
                let at = ledger.blocks[pos];
                let entry = &self.entries[at];
                let shown = entry.rows().min(self.room(entry.kind, ledger.cap));
                // Rows above the block's cap are not in this view at all, so the
                // view's rows of it are its last `shown` of them.
                let head = entry.rows() - shown;
                let next = first + shown;
                let top = start.max(first);
                let bottom = end.min(next);
                // The label rides on the block's own first row, or on the first row
                // its cap left it -- never mid-paragraph.
                let labelled = head > 0 || top == first;
                for row in top..bottom {
                    let print = labelled && row == top;
                    rows.push((entry.kind, entry.view_row(row - first + head, print)));
                }
                if bottom >= end || pos + 1 >= ledger.blocks.len() {
                    break;
                }
                first = next;
                pos += 1;
            }
        }
        Viewport { rows, max_scroll }
    }

    /// Re-derives every block's rows and every view's sums: the only whole-log walk.
    fn rebuild(&mut self) {
        let width = self.width;
        for entry in &mut self.entries {
            entry.reflow(width);
        }
        self.all = Ledger::default();
        for ledger in self.views.values_mut() {
            ledger.blocks = Vec::new();
            ledger.index = RowIndex::default();
        }
        for at in 0..self.entries.len() {
            self.attach(at);
        }
    }

    /// Recomputes everything when the width changed.
    fn reflow(&mut self, width: usize) {
        let width = width.max(1);
        if width == self.width {
            return;
        }
        self.width = width;
        self.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::View;

    fn log_of(lines: &[&str], width: usize) -> Log {
        let mut log = Log::new();
        for line in lines {
            log.push("", LineKind::System, *line);
        }
        log.rows(View::All, width);
        log
    }

    fn shown(log: &mut Log, width: usize, height: usize, scroll: usize) -> Vec<String> {
        log.window(View::All, width, height, scroll)
            .rows
            .iter()
            .map(|(_, text)| text.to_string())
            .collect()
    }

    /// Opens a block and streams `chunks` into it, sealing it on the last one.
    fn block(log: &mut Log, key: &str, tag: &str, chunks: &[&str]) {
        for (i, chunk) in chunks.iter().enumerate() {
            log.stream(
                "w",
                key,
                LineKind::Reasoning,
                tag,
                chunk,
                i + 1 == chunks.len(),
            );
        }
    }

    /// Adds one delta to a block that stays open, the way a token arrives.
    fn grow(log: &mut Log, key: &str, tag: &str, chunk: &str) {
        log.stream("w", key, LineKind::Reasoning, tag, chunk, false);
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

    /// A block taller than the window shows its newest rows, and says whose they
    /// are on the row shown first.
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
        assert_eq!(all[0], "[peer] abcde", "{all:?}");
        // A window that cuts a paragraph does not relabel it.
        assert_eq!(shown(&mut log, 12, 2, 0), vec!["fghijklmnopq", "rstuvwx"]);
        // The same rows cut by a *cap* do carry it: row 0 is gone from the view.
        log.set_block_size(2);
        assert_eq!(
            shown(&mut log, 12, 4, 0),
            vec!["[peer] fghijklmnopq", "rstuvwx"]
        );
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
        log.stream("w", "t:c1:Stderr", LineKind::ToolError, "", "\n", false);
        log.stream("w", "t:c1:Stderr", LineKind::ToolError, "", "", true);
        assert_eq!(log.len(), 0, "a blank chunk made a row");
        assert!(log.open.is_empty(), "the key was left open");
    }

    /// A sealed block does not end in a blank row, even when its output did.
    #[test]
    fn a_sealed_block_has_no_trailing_blank_row() {
        let mut log = Log::new();
        log.stream("w", "t:c1:Stdout", LineKind::ToolOutput, "", "out\n", false);
        log.stream("w", "t:c1:Stdout", LineKind::ToolOutput, "", "\n", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["out"]);
        // A blank line in the middle is still part of the block.
        let mut log = Log::new();
        log.stream("w", "t:c2:Stdout", LineKind::ToolOutput, "", "a\n\nb", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["a", "", "b"]);
    }

    /// A window can start mid-line, which a "count lines, not rows" renderer gets wrong.
    #[test]
    fn scrolling_moves_one_row_at_a_time() {
        // Five lines that each take two rows at width 4: "aaaa"/"aaa".
        let mut log = log_of(&["aaaaaaa", "bbbbbbb", "ccccccc", "ddddddd", "eeeeeee"], 4);
        assert_eq!(log.rows(View::All, 4), 10);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["ddd", "eeee", "eee"]);
        assert_eq!(shown(&mut log, 4, 3, 1), vec!["dddd", "ddd", "eeee"]);
        assert_eq!(shown(&mut log, 4, 3, 2), vec!["ccc", "dddd", "ddd"]);
    }

    /// Both ends stop the scroll: past the top and past the bottom, never wrap around.
    #[test]
    fn scroll_clamps_at_both_ends() {
        let mut log = log_of(&["aaaaaaa", "bbbbbbb"], 4);
        assert_eq!(log.rows(View::All, 4), 4);
        // Height 2 sees the last two rows: "bbbb" then "bbb".
        assert_eq!(shown(&mut log, 4, 2, 0), vec!["bbbb", "bbb"]);
        assert_eq!(shown(&mut log, 4, 2, 1), vec!["aaa", "bbbb"]);
        assert_eq!(shown(&mut log, 4, 2, 99), vec!["aaaa", "aaa"]);
        assert_eq!(log.window(View::All, 4, 2, 99).max_scroll, 2);
    }

    /// A cap is a height: the block keeps growing in place, and the rows past it
    /// leave the window.
    #[test]
    fn a_capped_block_shows_its_newest_rows() {
        let mut log = Log::new();
        log.set_block_size(2);
        // Four rows at this width, so two of them are past the cap.
        log.stream(
            "w",
            "k",
            LineKind::Reasoning,
            "[m] ",
            "aaaa\nb\ncccc\ndddd",
            false,
        );
        assert_eq!(
            log.rows(View::All, 10),
            2,
            "a capped block contributes only its cap"
        );
        assert_eq!(shown(&mut log, 10, 4, 0), vec!["[m] cccc", "dddd"]);
    }

    /// The tail follows: the newest text is what a capped row shows, and what came
    /// before it is off the top of the block.
    #[test]
    fn a_capped_block_follows_its_newest_text() {
        let mut log = Log::new();
        log.set_block_size(1);
        for chunk in ["alpha ", "beta ", "gamma"] {
            log.stream("w", "k", LineKind::Reasoning, "[m] ", chunk, false);
        }
        let rows = shown(&mut log, 12, 4, 0);
        assert_eq!(rows, vec!["[m] ta gamma"], "{rows:?}");
    }

    /// A block scrolled past its own first row still names itself, on the row that
    /// is shown first.
    #[test]
    fn a_capped_block_names_itself_on_its_top_row() {
        let mut log = Log::new();
        log.set_block_size(1);
        log.stream(
            "w",
            "k",
            LineKind::Reasoning,
            "[peer] thinking: ",
            &"x".repeat(500),
            false,
        );
        let rows = shown(&mut log, 30, 5, 0);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].starts_with("[peer] thinking: "), "{rows:?}");
    }

    /// A tool's own output is progress: capped like any other block, and the rows
    /// it shows are its newest.
    #[test]
    fn a_capped_tool_output_shows_its_newest_rows() {
        let mut log = Log::new();
        log.set_block_size(2);
        log.stream(
            "w",
            "t:c1:Stdout",
            LineKind::ToolOutput,
            "[m] ",
            "a\nb\nc\nd",
            true,
        );
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[m] c", "d"]);
    }

    /// The answer is the block the cap leaves alone: it is what the operator came
    /// to read.
    #[test]
    fn the_cap_leaves_the_answer_alone() {
        let mut log = Log::new();
        log.set_block_size(1);
        log.stream(
            "w",
            "o",
            LineKind::Assistant,
            "[main] ",
            "one\ntwo\nthree",
            true,
        );
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["[main] one", "two", "three"]
        );
    }

    /// `block_size = 0` is what `all` always was: every row a block grows.
    #[test]
    fn a_block_without_a_cap_keeps_every_row() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::Reasoning, "[m] ", "a\nb", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[m] a", "b"]);
    }

    /// One block is one label's text: another label opens a new block, so no row
    /// glues two events.
    #[test]
    fn a_new_label_opens_a_new_block() {
        let mut log = Log::new();
        log.stream(
            "w",
            "s:main",
            LineKind::Reasoning,
            "[main] thinking: ",
            "thinking",
            true,
        );
        log.stream(
            "w",
            "s:main",
            LineKind::ToolCall,
            "[main] tool: ",
            "bash{ls}",
            true,
        );
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["[main] thinking: thinking", "[main] tool: bash{ls}"]
        );
        // The sealed block took nothing more, even under the same key.
        log.stream(
            "w",
            "s:main",
            LineKind::Reasoning,
            "[main] thinking: ",
            " again",
            true,
        );
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec![
                "[main] thinking: thinking",
                "[main] tool: bash{ls}",
                "[main] thinking:  again"
            ]
        );
    }

    /// A cap counts rows of the width being measured, so a resize cannot leave a
    /// block taller than it.
    #[test]
    fn a_capped_block_stays_capped_at_any_width() {
        let mut log = Log::new();
        log.set_block_size(3);
        log.stream(
            "w",
            "k",
            LineKind::Reasoning,
            "[m] ",
            &"y ".repeat(300),
            false,
        );
        assert_eq!(log.rows(View::All, 10), 3);
        assert_eq!(log.rows(View::All, 200), 3);
        assert_eq!(log.rows(View::All, 4), 3);
    }

    /// The rows a cap took are not waiting above the window: scroll back and you
    /// find the block above.
    #[test]
    fn the_rows_a_cap_took_are_not_scrollable() {
        let mut log = log_of(&["older"], 40);
        log.set_block_size(1);
        log.stream(
            "w",
            "k",
            LineKind::Reasoning,
            "[m] ",
            "one\ntwo\nthree",
            false,
        );
        assert_eq!(log.rows(View::All, 40), 2, "two blocks of one row each");
        assert_eq!(log.window(View::All, 40, 2, 0).max_scroll, 0);
        assert_eq!(shown(&mut log, 40, 2, 0), vec!["older", "[m] three"]);
    }

    /// A frame must not depend on how long the log is, or wrapping per frame returns.
    #[test]
    fn a_frame_touches_only_a_screenful() {
        let mut log = Log::new();
        for i in 0..50_000 {
            log.push(
                "",
                LineKind::System,
                format!("line {i} with some words on it"),
            );
        }
        let height = 24;
        let view = log.window(View::All, 80, height, 0);
        assert_eq!(view.rows.len(), height);
        // And from the bottom of a scrolled view too.
        let view = log.window(View::All, 80, height, 30_000);
        assert_eq!(view.rows.len(), height);
    }

    #[test]
    fn a_width_change_reflows_every_line() {
        let mut log = log_of(&["abcdefghij"], 10);
        assert_eq!(log.rows(View::All, 10), 1);
        assert_eq!(log.rows(View::All, 4), 3);
        assert_eq!(shown(&mut log, 4, 3, 0), vec!["abcd", "efgh", "ij"]);
        // Back to the wide shape: the old offsets must not survive.
        assert_eq!(log.rows(View::All, 10), 1);
        assert_eq!(shown(&mut log, 10, 1, 0), vec!["abcdefghij"]);
    }

    /// The binary search must agree with a plain walk, or scrolling lands wrong.
    #[test]
    fn the_row_index_agrees_with_a_walk() {
        let mut log = Log::new();
        for i in 0..200usize {
            log.push("w", LineKind::System, "x".repeat(i % 17));
        }
        log.rows(View::All, 8);
        let mut expected = Vec::new();
        for at in 0..log.entries.len() {
            for _ in 0..log.contributions(at).0 {
                expected.push(at);
            }
        }
        let mut row = 0;
        for at in 0..log.entries.len() {
            assert_eq!(log.all.index.prefix(at), row, "start of block {at}");
            for _ in 0..log.contributions(at).0 {
                assert_eq!(log.all.index.line_at(row), expected[row], "row {row}");
                row += 1;
            }
        }
        assert_eq!(log.all.index.total(), row);
    }

    /// Views divide the reading, never the holding: a line said once is everywhere.
    #[test]
    fn the_views_divide_the_reading_not_the_holding() {
        let mut log = Log::new();
        log.stream("main", "k1", LineKind::Assistant, "[main] ", "one", true);
        log.stream("peer", "k2", LineKind::Assistant, "[peer] ", "two", true);
        assert_eq!(
            log.window(View::All, 40, 10, 0)
                .rows
                .iter()
                .map(|(_, t)| t.to_string())
                .collect::<Vec<_>>(),
            vec!["[main] one", "[peer] two"]
        );
        assert_eq!(
            log.window(View::Window("main"), 40, 10, 0)
                .rows
                .iter()
                .map(|(_, t)| t.to_string())
                .collect::<Vec<_>>(),
            vec!["[main] one"]
        );
        assert_eq!(
            log.rows(View::Window("later"), 40),
            0,
            "a window nobody wrote to"
        );
        assert_eq!(log.rows(View::All, 40), 2, "and the merged view holds both");
    }

    /// A window can cap its blocks apart from the rest.
    #[test]
    fn a_window_can_cap_its_own_blocks() {
        let mut log = Log::new();
        log.set_block_size(4);
        log.stream("cmd", "k1", LineKind::System, "", "a\nb\nc\nd\ne", true);
        log.stream("main", "k2", LineKind::Reasoning, "", "a\nb\nc\nd\ne", true);
        assert_eq!(log.rows(View::Window("main"), 40), 4, "the log's own cap");
        log.set_cap("cmd", 1);
        assert_eq!(log.rows(View::Window("cmd"), 40), 1, "the window's cap");
        assert_eq!(
            log.rows(View::Window("main"), 40),
            4,
            "and the other view is untouched"
        );
        // The merged view has its own cap: it is a view, not a union of the others.
        assert_eq!(
            log.rows(View::All, 40),
            8,
            "both blocks at the log's own cap"
        );
    }
}
