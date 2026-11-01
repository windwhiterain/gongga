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
    /// The caller's name for this block, so a click can find it again.
    key: String,
    /// The label this block was opened with, printed on its top visible row.
    tag: String,
    text: String,
    /// The columns this block was last wrapped at: the fold is cut to them.
    width: usize,
    /// Whether the operator opened it: SPEC's click, and the only state a fold has.
    expanded: bool,
    /// Rows scrolled up inside an opened item's box: SPEC's `scroll if block_size!=0`.
    inner: usize,
    /// Byte offset of each wrapped row's first character; its length is the row count.
    starts: Vec<usize>,
    /// Display columns used by the last row: the whole wrap state an append resumes.
    col: usize,
    /// No more text can arrive, so the block is scrollback.
    sealed: bool,
    /// Structured call fields, once the call's head arrives.
    call: Option<cli::protocol::Head>,
    /// The expanded sections, cached at their measured width.
    sections: String,
    /// Where this block sits among the blocks of its own window.
    at: usize,
    /// Where it sits in the merged view, or `None` when a hidden window holds it.
    merged_at: Option<usize>,
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
            key: String::new(),
            tag: tag.to_string(),
            text,
            width,
            expanded: false,
            inner: 0,
            starts: vec![0],
            col: 0,
            sealed: false,
            call: None,
            sections: String::new(),
            at: 0,
            merged_at: None,
        };
        // Width 0 means "not measured yet": the first draw reflows at its own width.
        if width > 0 {
            entry.reflow(width);
        }
        entry
    }

    fn rows(&self) -> usize {
        if self.folded() {
            return self.folded_rows();
        }
        self.starts.len()
    }

    /// Every opened item uses the same frame, whether or not its body is capped.
    fn boxed(&self) -> bool {
        self.expanded && self.kind.folds()
    }

    /// Columns left for text after this block's own frame is accounted for.
    fn content_width(&self, width: usize) -> usize {
        match self.boxed() {
            true => width.saturating_sub(2).max(1),
            false => width.max(1),
        }
    }

    /// The rows this block occupies at `cap`, which is its body plus its box.
    fn visible(&self, cap: usize) -> usize {
        let body = self.rows().min(cap);
        match self.boxed() {
            true => body + 2,
            false => body,
        }
    }

    /// Rows scrolled up inside the box, clamped to what it holds.
    fn inside(&self, cap: usize) -> usize {
        let body = self.rows().min(cap);
        self.inner.min(self.rows().saturating_sub(body))
    }

    /// Whether this block is drawn as SPEC's `abstract`: one summary line.
    ///
    /// # Invariant
    ///
    /// Only a sealed item of a folding kind, and only until the operator opens it:
    /// a block still streaming is what "currently stream" means, and an answer,
    /// a command or an error is not an `abstract` at all.
    fn folded(&self) -> bool {
        self.sealed && self.kind.folds() && !self.expanded
    }

    /// The first and last lines the model wrote that have anything in them.
    ///
    /// # Invariant
    ///
    /// SPEC's two lines are lines of the *text*, never rows of its wrap: a row is
    /// a column of a line, so ending on one showed half a sentence. Blank lines
    /// are passed over at both ends: a row with nothing on it is not an end.
    /// See `docs/notes/tui-log.md`.
    fn ends(&self) -> Option<(&str, &str)> {
        let first = self
            .text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let last = self
            .text
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        Some((first, last))
    }

    /// The rows a folded block shows: a reasoning item keeps its beginning and end.
    ///
    /// # Invariant
    ///
    /// Two rows only when there are two ends: prose written as one line is one row,
    /// because the same line twice is a row that says nothing new.
    fn folded_rows(&self) -> usize {
        if self.kind == LineKind::Reasoning
            && let Some((first, last)) = self.ends()
            && first != last
        {
            return 2;
        }
        // An item with neither a label nor text is one blank row: the mark alone.
        1
    }

    /// One row of the folded block, cut to the columns it will be drawn in.
    fn folded_row(&self, row: usize) -> String {
        let width = self.width.max(1);
        let text = match (self.kind, row) {
            // SPEC's `reasoning{beginning (line), ending (line)}`: the first line
            // the model wrote, then the last, `…` between.
            (LineKind::Reasoning, 0) => match self.ends() {
                Some((first, _)) => format!("{}{}", self.tag, first),
                None => self.tag.clone(),
            },
            (LineKind::Reasoning, _) => match self.ends() {
                Some((_, last)) => format!("\u{2026} {last}"),
                None => String::new(),
            },
            // A result says how it went, never what it printed: SPEC's
            // `tool_result{ok, time (spent)}` is the whole of its summary.
            (LineKind::ToolOutput | LineKind::ToolError, _) => self.summary(),
            // A call's summary is its name and flags; with no label the body is
            // the call itself, which is what its first row says.
            (LineKind::ToolCall, _) if !self.tag.is_empty() => self.tag.trim_end().to_string(),
            // An item whose body is its own label: the folded row is its first row.
            _ => self.row(0).to_string(),
        };
        cut(&text, width.saturating_sub(self.kind.mark().width()))
    }

    /// Outcome first: long arguments cannot hide the elapsed time.
    fn summary(&self) -> String {
        self.tag.trim_end().to_string()
    }

    /// An opened call shows the arguments its summary left out, and nothing else.
    ///
    /// # Invariant
    ///
    /// The name and the fields marked `summary` are the folded row, and an opened
    /// block draws that row as its label on the box's top rule. The body repeats
    /// neither, so no argument is read twice. See `docs/notes/tui-log.md`.
    fn section_text(&self, width: usize) -> Option<String> {
        let call = self.call.as_ref()?;
        let mut rows: Vec<String> = Vec::new();
        for field in call.fields.iter().filter(|field| !field.summary) {
            let name = match field.label.is_empty() {
                true if call.name == "bash" => "command",
                true => "value",
                false => field.label.as_str(),
            };
            let mut lines = field.value.split('\n');
            let first = lines.next().unwrap_or_default();
            if lines.clone().next().is_some() {
                rows.push(format!("  {name}:"));
                rows.push(format!("    {first}"));
                for line in lines {
                    rows.push(format!("    {line}"));
                }
            } else {
                rows.push(format!("  {name}: {first}"));
            }
        }
        Some(hanging_indent(&rows.join("\n"), width))
    }

    /// Expanded calls read their structured fields; other entries read their text.
    fn body(&self) -> &str {
        match self.expanded && self.call.is_some() {
            true => self.sections.as_str(),
            false => self.text.as_str(),
        }
    }

    /// Recomputes every body-row offset at `width`, independently of the title.
    fn reflow(&mut self, width: usize) {
        let width = self.content_width(width);
        self.sections = match self.expanded {
            true => self.section_text(width).unwrap_or_default(),
            false => String::new(),
        };
        let structured = self.expanded && self.call.is_some();
        self.width = width;
        self.starts.clear();
        self.starts.push(0);
        // The top border owns the label. A structured body starts immediately
        // inside the left border, not behind another copy of the mark's padding.
        // Symbols and labels are title chrome, never a hanging indent on the body.
        let mut wrap = Wrap { width, col: 0 };
        if structured {
            let text = self.sections.clone();
            wrap.feed(&text, 0, &mut self.starts);
        } else {
            let Self { text, starts, .. } = self;
            let text = text.as_str();
            wrap.feed(text, 0, starts);
        }
        self.col = wrap.col;
    }

    /// Appends to the body the current view reads, continuing its wrap state.
    fn append(&mut self, text: &str, width: usize) {
        let width = self.content_width(width);
        let base = self.text.len();
        self.width = width;
        if width > 0 {
            let mut wrap = Wrap {
                width,
                col: self.col,
            };
            wrap.feed(text, base, &mut self.starts);
            self.col = wrap.col;
        }
        self.text.push_str(text);
    }

    /// The text of row `row`, with the newline that ended it removed.
    ///
    /// # Invariant
    ///
    /// A row runs to the next row's start, one byte past the newline that broke
    /// it, so a trailing newline yields a final empty row. See
    /// `docs/notes/tui-log.md`.
    fn row(&self, row: usize) -> &str {
        let text = self.body();
        let start = self.starts[row];
        let end = self.starts.get(row + 1).copied().unwrap_or(text.len());
        let text = &text[start..end];
        text.strip_suffix('\n').unwrap_or(text)
    }

    /// The item's own label: its mark and its tag, and none of its body.
    fn label(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{}{}", self.kind.mark(), self.summary()))
    }

    /// One row as a frame shows it, with the block's label on the row the window
    /// starts with.
    ///
    /// # Invariant
    ///
    /// Only the top row of a block carries its label, and a clipped tail loses
    /// nothing: those characters start the row below it.
    fn view_row(&self, row: usize, labelled: bool) -> Cow<'_, str> {
        if self.folded() {
            let summary = self.folded_row(row);
            // SPEC's `abstract (horizontal)`: the mark owns one column, so the
            // second row starts where the first one's text does.
            let label = match labelled {
                true => self.kind.mark(),
                false => " ".repeat(self.kind.mark().width()),
            };
            return Cow::Owned(format!("{label}{summary}"));
        }
        // The title is one horizontal unit: symbol, label, then body. The
        // title alone is clipped, never the body's wrapping or its continuation.
        let row = self.row(row);
        match labelled {
            false => Cow::Borrowed(row),
            true => Cow::Owned(format!(
                "{}{}{}",
                self.kind.mark(),
                cut(
                    &self.tag,
                    self.width.saturating_sub(self.kind.mark().width())
                ),
                row
            )),
        }
    }
}

/// `text` cut to `width` columns, marked when it was cut.
fn cut(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let advance = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if used + advance > width.saturating_sub(1) {
            out.push('\u{2026}');
            return out;
        }
        out.push(ch);
        used += advance;
    }
    out
}

/// Soft-wrap section rows without losing the indentation that names their level.
/// The inserted columns are real text, so row slicing and the right border use
/// exactly the same display width.
fn hanging_indent(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut column = 0usize;
    let mut indent = 0usize;
    let mut leading = true;
    for ch in text.chars() {
        if ch == '\n' {
            out.push(ch);
            column = 0;
            indent = 0;
            leading = true;
            continue;
        }
        if leading && ch == ' ' {
            indent += 1;
        } else {
            leading = false;
        }
        let advance = ch.width().unwrap_or(0).max(1);
        let continuation = indent.min(width.saturating_sub(1));
        if column + advance > width && column > continuation {
            out.push('\n');
            out.push_str(&" ".repeat(continuation));
            column = continuation;
        }
        out.push(ch);
        column += advance;
    }
    out
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

/// One row of a frame: what it is, its text, and the box it is drawn inside.
///
/// # Invariant
///
/// `edges` is empty for a row that is not part of a frame; a block the operator
/// opened is the only thing SPEC gives a scroll of its own, so it is the only one
/// that has a box, and the box's sides are what the renderer draws around it.
pub type Row<'a> = (LineKind, Cow<'a, str>, Edges);

/// Which sides of a frame a row carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    /// The frame's top border.
    pub top: bool,
    /// The frame's bottom border.
    pub bottom: bool,
    /// The frame's left and right borders.
    pub sides: bool,
}

impl Edges {
    /// A row inside a frame, with its sides only.
    pub const INSIDE: Self = Self {
        top: false,
        bottom: false,
        sides: true,
    };

    /// The frame's own top rule, which also carries the item's first row.
    pub const TOP: Self = Self {
        top: true,
        bottom: false,
        sides: true,
    };

    /// The frame's bottom rule.
    pub const BOTTOM: Self = Self {
        top: false,
        bottom: true,
        sides: true,
    };

    /// Whether this row is drawn inside a box at all.
    pub fn framed(self) -> bool {
        self.sides || self.top || self.bottom
    }
}

/// What one frame needs: the rows that fit, borrowed from the log, and the bound.
pub struct Viewport<'a> {
    pub rows: Vec<Row<'a>>,
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
    /// The merged view: every block whose window is not hidden, in arrival order.
    all: Ledger,
    /// One ledger per window name a line has carried.
    views: HashMap<String, Ledger>,
    /// Open stream key -> its block. Its identity, not its position.
    open: HashMap<String, usize>,
    /// The width the cached row offsets were measured at; 0 until a frame is drawn.
    width: usize,
    /// How many rows one block may fill. See [`Log::set_block_size`].
    block_size: usize,
    /// Windows the merged view does not read. See [`Log::set_hidden`].
    hidden: Vec<String>,
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

    /// Drops every block the window `window` holds: the session that wrote it is gone.
    ///
    /// # Invariant
    ///
    /// The name keeps nothing -- no block, no ledger, and no stream left open in
    /// it -- so a block arriving under a reused name is that session's first and
    /// starts at row zero. See `docs/notes/tui-log.md`.
    pub fn forget(&mut self, window: &str) {
        if self.views.remove(window).is_none() {
            return;
        }
        // Where each surviving block lands, so an open stream still names its own
        // block once the ones behind it are gone.
        let mut moved = Vec::with_capacity(self.entries.len());
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            moved.push((entry.window != window).then(|| kept.len()));
            if entry.window != window {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.open = self
            .open
            .drain()
            .filter_map(|(key, at)| moved[at].map(|at| (key, at)))
            .collect();
        self.rebuild();
    }

    /// Leaves these windows out of the merged view; an empty list shows every one.
    ///
    /// # Invariant
    ///
    /// Nothing is dropped: a hidden window's blocks stay in the log, so showing it
    /// again is a rebuild rather than a re-read of the server.
    pub fn set_hidden(&mut self, hidden: &[String]) {
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden.to_vec();
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
        // SPEC caps streamed abstracts, not commands, results, messages or errors.
        if !kind.folds() {
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
        // The box is two rows the block occupies, so both indexes count them.
        (
            entry.visible(self.room(entry.kind, self.all.cap)),
            entry.visible(self.room(entry.kind, self.view_cap(entry))),
        )
    }

    /// The cap one window puts on its own blocks, or 0 for the log's.
    fn view_cap(&self, entry: &Entry) -> usize {
        self.views
            .get(&entry.window)
            .map(|ledger| ledger.cap)
            .unwrap_or(0)
    }

    /// Moves both views' sums by what a change to one block did to them.
    fn shift(&mut self, at: usize, before: (usize, usize), after: (usize, usize)) {
        if let Some(pos) = self.entries[at].merged_at {
            self.all
                .index
                .shift(pos, after.0 as isize - before.0 as isize);
        }
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
        // A whole row arrives at once: nothing will stream into it, so a folding
        // kind is an `abstract` from its first frame.
        entry.sealed = true;
        self.entries.push(entry);
        self.attach(self.entries.len() - 1);
    }

    /// Records block `at` in the merged view and in its own window's view.
    fn attach(&mut self, at: usize) {
        let (merged, own) = self.contributions(at);
        let window = self.entries[at].window.clone();
        let shown = !self.hidden.contains(&window);
        let merged_at = if shown {
            let pos = self.all.blocks.len();
            self.all.blocks.push(at);
            self.all.index.push(merged);
            Some(pos)
        } else {
            None
        };
        self.entries[at].merged_at = merged_at;
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

    /// Where the block `key` currently names is: the open one, else the newest.
    ///
    /// # Invariant
    ///
    /// A key can belong to more than one block -- a stream that ended and started
    /// again leaves the sealed one behind -- so a key lookup is "the live block",
    /// never "the first block that ever used this key". See `docs/notes/tui-log.md`.
    fn entry_by_key(&self, key: &str) -> Option<usize> {
        if let Some(at) = self.open.get(key).copied() {
            return Some(at);
        }
        self.entries.iter().rposition(|entry| entry.key == key)
    }

    /// Drops every block of one window, keeping the window itself.
    ///
    /// # Invariant
    ///
    /// Only the blocks: SPEC's command row shows the last `cmd` or `cmd_result`, so
    /// the previous one is dropped rather than kept out of sight -- there is no way
    /// to scroll back to it, and holding it would be holding what cannot be read.
    pub fn clear(&mut self, window: &str) {
        if !self.entries.iter().any(|entry| entry.window == window) {
            return;
        }
        self.entries.retain(|entry| entry.window != window);
        let kept: Vec<&Entry> = self.entries.iter().collect();
        let _ = kept;
        // The open map points at indexes that have moved: re-derive it from the
        // blocks that are still streaming.
        let open: Vec<(String, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.sealed && !entry.key.is_empty())
            .map(|(at, entry)| (entry.key.clone(), at))
            .collect();
        self.open = open.into_iter().collect();
        self.rebuild();
    }

    /// Replaces the whole text of the block `key` names and re-wraps it.
    ///
    /// # Invariant
    ///
    /// For a block whose text is *derived* from what arrived -- an answer drawn as
    /// markdown, where one more delta can change a row above the last one -- so an
    /// append would be wrong and a rewrite is the only honest thing. Both indexes
    /// move by the difference in rows, as they do for a fold.
    pub fn rewrite(&mut self, key: &str, text: &str) {
        let Some(at) = self.entry_by_key(key) else {
            return;
        };
        if self.entries[at].text == text {
            return;
        }
        let before = self.contributions(at);
        let width = self.width;
        let entry = &mut self.entries[at];
        entry.text.clear();
        entry.text.push_str(text);
        entry.reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
    }

    /// Whether a block under this key exists already.
    pub fn has_block(&self, key: &str) -> bool {
        self.entries.iter().any(|entry| entry.key == key)
    }

    /// Scrolls the box of an opened item `at`, positive into its past.
    ///
    /// # Invariant
    ///
    /// Returns whether the inner box owns this wheel event. Above `chain_at`,
    /// or with no cap, exhaustion in the wheel's direction falls through to the
    /// outer pane. Smaller boxes consume the wheel at both ends.
    pub fn scroll_block(&mut self, at: usize, rows: i64, chain_at: usize) -> bool {
        // Use the row offsets of the frame the pointer actually landed on.
        // Reflowing a resized window before the next draw would move the hit.
        let Some(entry) = self.entries.get(at) else {
            return false;
        };
        let cap = self.room(entry.kind, 0);
        if !entry.boxed() {
            return false;
        }
        let bound = entry.rows() - entry.rows().min(cap);
        let next = (entry.inner as i64 + rows).clamp(0, bound as i64) as usize;
        let chains = cap == NO_CAP || cap > chain_at;
        let moved = next != entry.inner;
        self.entries[at].inner = next;
        chains || moved
    }

    /// The block `at` as a click names it: the box scrolls this instead of the pane.
    pub fn expanded_at(&self, at: usize) -> bool {
        self.entries
            .get(at)
            .is_some_and(|entry| entry.expanded && entry.kind.folds())
    }

    /// Opens or closes the `abstract` at one block, by the identity a click got.
    ///
    /// # Invariant
    ///
    /// The block, not the key: one key can name several blocks -- a turn's second
    /// reasoning is `r:<session>:<turn>:Reasoning` again -- so a press has to say
    /// which of them it landed on, or the wrong item opens.
    pub fn toggle_at(&mut self, at: usize) -> bool {
        if at >= self.entries.len() || !self.entries[at].kind.folds() {
            return false;
        }
        let before = self.contributions(at);
        let width = self.width;
        let entry = &mut self.entries[at];
        entry.expanded = !entry.expanded;
        entry.reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        true
    }

    /// Opens or closes the `abstract` `key` names.
    ///
    /// # Invariant
    ///
    /// The row count changes, so this is a reflow of one block and a shift of both
    /// indexes -- the arithmetic a seal does, for the same reason. A key that is not
    /// a folding block is nothing to toggle.
    pub fn toggle(&mut self, key: &str) -> bool {
        let Some(at) = self.entry_by_key(key) else {
            return false;
        };
        if !self.entries[at].kind.folds() {
            return false;
        }
        let before = self.contributions(at);
        let width = self.width;
        let entry = &mut self.entries[at];
        entry.expanded = !entry.expanded;
        entry.reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        true
    }

    /// The block under one row of a view, as the key a click carries.
    ///
    /// # Invariant
    ///
    /// It answers with the block *shown* at that row, counting the fold: a click on
    /// a summary line is a click on the item it summarises.
    pub fn block_at(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<usize> {
        self.reflow(width);
        let ledger = self.ledger(view)?;
        let total = ledger.index.total();
        let scroll = scroll.min(total.saturating_sub(height));
        let end = total - scroll;
        let start = end.saturating_sub(height);
        let want = start + row;
        if row >= end - start || want >= end {
            return None;
        }
        let pos = ledger
            .index
            .line_at(want)
            .min(ledger.blocks.len().checked_sub(1)?);
        let at = *ledger.blocks.get(pos)?;
        Some(at)
    }

    /// Replaces a call's provisional body with its structured name and fields.
    /// Output already received stays in the result section.
    pub fn describe(&mut self, name: &str, key: &str, head: &cli::protocol::Head) {
        let at = match self.entry_by_key(key) {
            Some(at) => at,
            None => {
                self.stream(
                    name,
                    key,
                    crate::LineKind::ToolCall,
                    &format!("{} ", head.summary()),
                    &head.detail(),
                    true,
                );
                let Some(at) = self.entry_by_key(key) else {
                    return;
                };
                at
            }
        };
        let width = self.width;
        let before = self.contributions(at);
        let entry = &mut self.entries[at];
        entry.tag = format!("{} ", head.summary());
        entry.text = head.detail();
        entry.kind = crate::LineKind::ToolCall;
        entry.call = Some(head.clone());
        // A call's structure is complete independently of its result stream.
        entry.sealed = true;
        entry.reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        self.open.remove(key);
    }

    /// Replaces the label of the block `key` names, once its outcome is known.
    ///
    /// # Invariant
    ///
    /// The row count can change: the label is not text, so a longer one eats the
    /// first row's columns and wraps it further. The block is reflowed and both
    /// indexes are corrected, which is what keeps a retro-set label from silently
    /// shifting every block after it. A key nobody opened is nothing to label.
    pub fn relabel(&mut self, key: &str, tag: &str, kind: LineKind) {
        // By key, not only through `open`: a verdict arrives on the chunk that
        // ends the call, and the block is sealed by then.
        let Some(at) = self.entry_by_key(key) else {
            return;
        };
        if self.entries[at].tag == tag && self.entries[at].kind == kind {
            return;
        }
        let before = self.contributions(at);
        self.entries[at].tag = tag.to_string();
        self.entries[at].kind = kind;
        let width = self.width;
        self.entries[at].reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
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
        if let Some(at) = self.open.get(key).copied()
            && self.entries[at].tag != tag
        {
            if done && !tag.is_empty() {
                self.relabel(key, tag, kind);
            } else {
                self.seal(key, at);
            }
        }
        let at = match self.open.get(key).copied() {
            // A block already open, ending: `done` and nothing in it closes a
            // part. Ahead of the arm whose `Some(at)` swallows this.
            Some(at) if text.is_empty() && done => {
                // A structured call's empty tail carries no new summary; a
                // result's tail carries the verdict on the same result block.
                if !tag.is_empty() {
                    self.relabel(key, tag, kind);
                }
                self.seal(key, at);
                return;
            }
            Some(at) => {
                let before = self.contributions(at);
                self.entries[at].append(text, width);
                let after = self.contributions(at);
                self.shift(at, before, after);
                at
            }
            // A blank chunk opens no block: one precedes every exit sentinel.
            // A labelled one does -- an item can have no body.
            None if text.trim().is_empty() && tag.is_empty() => {
                if done {
                    self.open.remove(key);
                }
                return;
            }
            None => {
                let mut entry = Entry::with_tag(name, kind, tag, text.to_string(), width);
                // A body-less item's body is its label: a call reads as its
                // arguments, and opening it must show them.
                entry.key = key.to_string();
                self.entries.push(entry);
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
        // Sealing can fold the block, and a fold is a row count: the indexes
        // hear about it, or the blocks behind this one shift.
        let before = self.contributions(at);
        self.entries[at].sealed = true;
        let after = self.contributions(at);
        self.shift(at, before, after);
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

        let mut rows: Vec<Row<'_>> = Vec::with_capacity(end - start);
        if start < end {
            // Jump to the block holding the first visible row: O(log blocks).
            let mut pos = ledger.index.line_at(start).min(ledger.blocks.len() - 1);
            let mut first = ledger.index.prefix(pos);
            loop {
                let at = ledger.blocks[pos];
                let entry = &self.entries[at];
                let cap = self.room(entry.kind, ledger.cap);
                let body = entry.rows().min(cap);
                let boxed = entry.boxed();
                // Rows above the cap are not in this view, so its rows are the
                // last `body` of them -- pulled further back by the box's scroll.
                let head = entry
                    .rows()
                    .saturating_sub(body)
                    .saturating_sub(entry.inside(cap));
                let shown = entry.visible(cap);
                let next = first + shown;
                let top = start.max(first);
                let bottom = end.min(next);
                // The label rides on the block's own first row, or on the first row
                // its cap left it -- never mid-paragraph.
                let labelled = head > 0 || top == first;
                for row in top..bottom {
                    let index = row - first;
                    // SPEC's opened item: a box whose top row carries the label.
                    if boxed && index == 0 {
                        rows.push((entry.kind, entry.label(), Edges::TOP));
                        continue;
                    }
                    if boxed && index + 1 == shown {
                        rows.push((entry.kind, Cow::Borrowed(""), Edges::BOTTOM));
                        continue;
                    }
                    let body_row = match boxed {
                        true => index - 1 + head,
                        false => index + head,
                    };
                    // The label is on the box's top row, so no body row repeats
                    // it: that column is the body's.
                    let print = labelled && !boxed && index == 0;
                    let edges = match boxed {
                        true => Edges::INSIDE,
                        false => Edges::default(),
                    };
                    rows.push((entry.kind, entry.view_row(body_row, print), edges));
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

    /// Call arguments and tool output stream independently, paired by their id.
    #[test]
    fn a_call_and_its_result_are_two_live_items() {
        let mut log = Log::new();
        log.stream("w", "c:c1", LineKind::ToolCall, "bash ", "cargo bu", false);
        log.stream("w", "c:c1", LineKind::ToolCall, "bash ", "ild", true);
        log.stream(
            "w",
            "t:c1",
            LineKind::ToolOutput,
            "",
            "Compiling one\n",
            false,
        );
        log.stream(
            "w",
            "t:c1",
            LineKind::ToolOutput,
            "",
            "Compiling two\n",
            false,
        );
        let rows = shown(&mut log, 60, 12, 0);
        assert!(rows.iter().any(|row| row.contains("bash")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("Compiling one")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("Compiling two")),
            "{rows:?}"
        );
        log.stream("w", "t:c1", LineKind::ToolOutput, "1.2s ", "", true);
        assert_eq!(
            log.len(),
            2,
            "one call and one result, never one mixed body"
        );
        let rows = shown(&mut log, 60, 12, 0);
        assert!(rows.iter().any(|row| row.contains("1.2s")), "{rows:?}");
        assert!(log.toggle("c:c1"), "the call has its own expanded state");
        assert!(log.toggle("t:c1"), "the result has its own expanded state");
    }

    /// An item with no body still opens: a call reads as its own arguments, and
    /// its bare label is not a blank sentinel line.
    #[test]
    fn a_labelled_blank_chunk_opens_the_item() {
        let mut log = Log::new();
        log.stream("w", "c1", LineKind::ToolCall, "bash{ls} ", "", true);
        assert_eq!(shown(&mut log, 40, 4, 0), vec!["\u{25b8} bash{ls}"]);
        // The shell's own sentinel still opens nothing.
        log.stream("w", "c2", LineKind::ToolOutput, "", "\n", true);
        assert_eq!(log.rows(View::All, 40), 1);
    }

    /// A folded result is its verdict, never its output however long: the `…` a
    /// reader saw was a cut-up body, not a summary.
    #[test]
    fn a_folded_result_never_shows_what_it_printed() {
        let mut log = Log::new();
        log.stream("w", "t1", LineKind::ToolOutput, "", &"x".repeat(200), true);
        log.relabel("t1", "1.2s ", LineKind::ToolOutput);
        assert_eq!(shown(&mut log, 20, 4, 0), vec!["\u{2713} 1.2s"]);
        // A result whose verdict never arrived is the mark alone -- still nothing
        // of the body.
        let mut log = Log::new();
        log.stream("w", "t2", LineKind::ToolError, "", "boom\nmore", true);
        assert_eq!(shown(&mut log, 20, 4, 0), vec!["\u{2715} "]);
    }

    /// An item's body forms a column under its first row, not under its mark.
    #[test]
    fn a_body_row_starts_where_the_first_rows_text_does() {
        let mut log = Log::new();
        log.stream("w", "r1", LineKind::Reasoning, "", "one\ntwo\nthree", true);
        log.toggle("r1");
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{25c7} ", "one", "two", "three", ""]
        );
    }

    /// An opened item taller than its cap gets a box, and the box scrolls.
    ///
    /// # Invariant
    ///
    /// SPEC: `abstract (…, block_size, scroll if block_size!=0)`. The box is two
    /// rows of its own, so the body still shows a whole `block_size` of rows, and
    /// scrolling moves that window into the past rather than off the end.
    #[test]
    fn an_opened_item_gets_a_box_that_scrolls() {
        let mut log = Log::new();
        log.set_block_size(2);
        log.stream("w", "r1", LineKind::Reasoning, "", "a\nb\nc\nd", true);
        log.toggle("r1");
        // Two body rows plus the box: the tail, as a cap shows.
        assert_eq!(log.rows(View::All, 40), 4, "two body rows and a box");
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{25c7} ", "c", "d", ""],
            "the frame's top row carries the label"
        );
        assert!(log.scroll_block(0, 2, 6), "the box scrolls");
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{25c7} ", "a", "b", ""],
            "and it moved into the past"
        );
        // Closing it takes the box away: a folded reasoning keeps its two ends.
        assert!(log.toggle("r1"));
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{25c7} a", "  \u{2026} d"],
            "the box is gone and the item is the `abstract` again"
        );
    }

    /// A part that ends with an empty chunk still folds.
    ///
    /// # Invariant
    ///
    /// `done` with no text and no label is how a streamed part closes, and sealing
    /// is what makes an item SPEC's `abstract`. Treating it as a blank sentinel --
    /// which is what a *shell's* exit line looks like -- left the item open, so it
    /// never folded.
    #[test]
    fn a_part_that_ends_with_an_empty_chunk_still_folds() {
        let mut log = Log::new();
        log.stream("w", "c1", LineKind::ToolCall, "", "bash(ls)", false);
        assert_eq!(
            log.rows(View::All, 40),
            1,
            "streaming: the rows it is writing"
        );
        log.relabel("c1", "name=build ", LineKind::ToolCall);
        log.stream("w", "c1", LineKind::ToolCall, "", "", true);
        assert_eq!(
            shown(&mut log, 40, 4, 0),
            vec!["\u{25b8} name=build"],
            "sealed, the row is the summary and not the body"
        );
    }

    /// SPEC's `abstract`: an item that is not the answer folds when it stops.
    ///
    /// # Invariant
    ///
    /// Reasoning, a call and its result fold; the answer, a command and an error
    /// do not -- SPEC gives those their own shape, not a summary.
    #[test]
    fn an_item_folds_when_it_stops_and_the_answer_never_does() {
        let cases = [
            (LineKind::Reasoning, true),
            (LineKind::ToolCall, true),
            (LineKind::ToolOutput, true),
            (LineKind::ToolError, true),
            (LineKind::Assistant, false),
            (LineKind::Command, false),
            (LineKind::Error, false),
        ];
        for (kind, folds) in cases {
            let mut log = Log::new();
            log.stream("w", "k", kind, "", "one\ntwo", true);
            let rows = log.rows(View::All, 40);
            // A reasoning summary keeps two lines, every other fold is one; what
            // is not a fold keeps the rows it wrote.
            let want = match (folds, kind) {
                (false, _) => 2,
                (true, LineKind::Reasoning) => 2,
                (true, _) => 1,
            };
            assert_eq!(
                rows,
                want,
                "{kind:?}: sealed, {}",
                if folds {
                    "an abstract"
                } else {
                    "not an abstract"
                }
            );
            if folds {
                // And the summary is not the text: nothing of `two` is one row.
                assert!(log.toggle("k"), "{kind:?} opens");
                assert_eq!(
                    log.rows(View::All, 40),
                    4,
                    "{kind:?}: body plus two borders"
                );
            }
        }
    }

    /// While it streams, an item is not folded: that is the text arriving.
    #[test]
    fn a_streaming_item_shows_what_it_is_writing() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::ToolOutput, "", "line one\n", false);
        log.stream("w", "k", LineKind::ToolOutput, "", "line two", false);
        assert_eq!(log.rows(View::All, 40), 2, "two rows so far");
        log.stream("w", "k", LineKind::ToolOutput, "", "", true);
        assert_eq!(log.rows(View::All, 40), 1, "sealed, it is one summary");
    }

    /// A folded result is the verdict, never its output: the output is the click.
    #[test]
    fn a_folded_result_says_how_it_went_and_not_what_it_printed() {
        let mut log = Log::new();
        log.stream(
            "w",
            "t:c1",
            LineKind::ToolOutput,
            "",
            "printed a lot\nof stuff",
            true,
        );
        log.relabel("t:c1", "1.2s ", LineKind::ToolOutput);
        assert_eq!(shown(&mut log, 40, 4, 0), vec!["\u{2713} 1.2s"]);
        assert!(log.toggle("t:c1"), "a result opens");
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{2713} 1.2s", "printed a lot", "of stuff", ""]
        );
    }

    /// A folded reasoning item keeps its beginning and its end, with `…` between.
    ///
    /// # Invariant
    ///
    /// SPEC's two lines are lines of the text, so a wrapped row is never one of
    /// them: the summary ends on the last thing the model wrote, complete, and not
    /// on whatever column of it the width happened to break.
    #[test]
    fn a_folded_reasoning_keeps_its_beginning_and_its_end() {
        let mut log = Log::new();
        log.stream(
            "w",
            "r1",
            LineKind::Reasoning,
            "",
            "first\nmiddle\nlast",
            true,
        );
        assert_eq!(
            shown(&mut log, 40, 4, 0),
            // SPEC's `abstract (horizontal)`: the second row starts where the
            // first one's text does; `…` says the middle was cut.
            vec!["\u{25c7} first", "  \u{2026} last"]
        );
        // One row has no second: the summary is one row, not a blank one.
        let mut log = Log::new();
        log.stream("w", "r2", LineKind::Reasoning, "", "only", true);
        assert_eq!(shown(&mut log, 40, 4, 0), vec!["\u{25c7} only"]);

        // One line with no newline in it is both ends at once: one whole row,
        // not two columns of the same sentence.
        let mut log = Log::new();
        log.stream(
            "w",
            "r3",
            LineKind::Reasoning,
            "",
            "one long paragraph that wraps across several rows of this width",
            true,
        );
        assert_eq!(
            shown(&mut log, 20, 4, 0),
            vec!["\u{25c7} one long paragrap…"],
            "one line of prose is one row"
        );
    }

    /// The two ends are non-empty lines, whatever the wrap and the blank lines say.
    ///
    /// # Invariant
    ///
    /// A trailing newline leaves a row with nothing on it, and a line longer than
    /// the window wraps into columns; neither is an end of what was written, so a
    /// folded reasoning says nothing about the break and skips the blank line.
    #[test]
    fn the_two_ends_are_the_lines_the_model_wrote() {
        let mut log = Log::new();
        log.stream(
            "w",
            "r1",
            LineKind::Reasoning,
            "",
            "a beginning far longer than this width, so it wraps\n\n   \n\na last line that is also far too long to fit",
            true,
        );
        assert_eq!(
            shown(&mut log, 24, 4, 0),
            vec![
                "\u{25c7} a beginning far longe…",
                "  \u{2026} a last line that is…"
            ],
            "the end is the last line, not the column it wrapped to"
        );
        // Trailing blank lines are not an end either.
        let mut log = Log::new();
        log.stream(
            "w",
            "r2",
            LineKind::Reasoning,
            "",
            "head\ntail\n\n  \n",
            true,
        );
        assert_eq!(
            shown(&mut log, 40, 4, 0),
            vec!["\u{25c7} head", "  \u{2026} tail"]
        );
    }

    /// Opening one item moves every block behind it by the rows it grew.
    ///
    /// # Invariant
    ///
    /// The indexes are the whole reason a frame does not walk the log, so a fold
    /// that changed the row count without telling them would draw the next block's
    /// text in the middle of this one.
    #[test]
    fn opening_an_item_grows_the_log_by_its_rows() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::ToolOutput, "", "a\nb\nc", true);
        log.push("w", LineKind::System, "after");
        assert_eq!(log.rows(View::All, 40), 2, "one summary and one row");
        // A result with no verdict yet: the mark alone, never its output.
        assert_eq!(shown(&mut log, 40, 4, 0), vec!["\u{2713} ", "after"]);
        assert!(log.toggle("k"));
        assert_eq!(
            log.rows(View::All, 40),
            6,
            "three body rows, two borders, and the row after"
        );
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["\u{2713} ", "a", "b", "c", "", "after"],
            "the row after stays after"
        );
        assert!(log.toggle("k"), "and it closes again");
        assert_eq!(log.rows(View::All, 40), 2);
    }

    /// A click names the block it landed on, folded rows included.
    #[test]
    fn a_row_names_the_block_drawn_there() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::ToolOutput, "", "a\nb", true);
        log.push("w", LineKind::System, "after");
        let at = |log: &mut Log, row: usize| log.block_at(View::All, 40, 8, 0, row);
        assert_eq!(at(&mut log, 0), Some(0), "the item");
        assert_eq!(at(&mut log, 1), Some(1), "the row after it");
        log.toggle_at(0);
        assert_eq!(at(&mut log, 1), Some(0), "its second row is still its own");
        assert_eq!(
            at(&mut log, 4),
            Some(1),
            "the block after the closing border"
        );
        assert_eq!(at(&mut log, 5), None, "and nothing past the end");
    }

    /// Two blocks under one key are two items, and a press opens the one it hit.
    ///
    /// # Invariant
    ///
    /// A turn's second reasoning is `r:<session>:<turn>:Reasoning` again, so keying
    /// a press by the string would open the wrong one -- which is what a reader
    /// saw: clicking the later item expanded the earlier one.
    #[test]
    fn a_press_opens_the_block_it_landed_on_not_the_key() {
        let mut log = Log::new();
        log.stream("w", "r1", LineKind::Reasoning, "", "one\ntwo", true);
        log.stream("w", "r1", LineKind::Reasoning, "", "three\nfour", true);
        // Folded, each is two rows: one at 0-1, the other at 2-3.
        let at = |log: &mut Log, row: usize| log.block_at(View::All, 40, 8, 0, row);
        assert_eq!(at(&mut log, 2), Some(1), "the second item");
        assert!(log.toggle_at(1), "opens that one");
        assert!(!log.entries[0].expanded, "and not the first");
        assert!(log.entries[1].expanded);
    }

    /// Only an `abstract` opens: an answer has nothing to fold.
    #[test]
    fn only_an_abstract_opens() {
        let mut log = Log::new();
        log.stream("w", "a", LineKind::Assistant, "", "the answer", true);
        assert!(!log.toggle("a"));
        assert!(!log.toggle("nobody"));
    }

    /// A key names the *live* block, not the first one that ever used it.
    ///
    /// # Invariant
    ///
    /// Two blocks can share a key: a stream that ends and is started again (a
    /// cancelled turn, a provider sending more text after `done`) leaves the sealed
    /// one behind and opens a new one under the same key. A rewrite must reach the
    /// new one, or the text lands in a block the frame has already drawn -- which
    /// on screen is a block frozen at its first character.
    #[test]
    fn a_rewrite_reaches_the_live_block_not_the_sealed_one() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::Assistant, "", "first turn", true);
        log.stream("w", "k", LineKind::Assistant, "", "second", false);
        log.rewrite("k", "second turn, whole");
        assert_eq!(
            shown(&mut log, 40, 6, 0),
            vec!["first turn", "second turn, whole"],
            "the live block grew, and the sealed one was left alone"
        );
    }

    /// A label that arrives after the block opened replaces the one it opened
    /// with, and the next frame draws the new one.
    ///
    /// # Invariant
    ///
    /// The rows a longer label pushes into are counted: a retro-set label that ate
    /// the first row's columns without reflowing would leave the block's own text
    /// wrapping as if the label were not there.
    #[test]
    fn a_label_set_after_the_block_opened_is_the_one_drawn() {
        let mut log = Log::new();
        log.stream("w", "k", LineKind::ToolOutput, "", "out", false);
        assert_eq!(shown(&mut log, 20, 4, 0), vec!["\u{2713} out"]);
        log.relabel("k", "1.2s ", LineKind::ToolOutput);
        assert_eq!(
            shown(&mut log, 20, 4, 0),
            vec!["\u{2713} 1.2s out"],
            "the label leads the block's first row"
        );

        // A label that eats the row: the text is re-wrapped, and what the frame
        // loses to the cut is still counted in the rows.
        let long = "x".repeat(19);
        log.relabel("k", &long, LineKind::ToolError);
        assert_eq!(
            log.rows(View::All, 20),
            1,
            "the label does not change body wrapping"
        );
        // The body column starts under the label, so the block's own text is the
        // last row and nothing of it is lost to the cut.
        let rows = shown(&mut log, 20, 8, 0);
        assert!(rows[0].starts_with("\u{2715} "), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.trim_end().ends_with("out")),
            "every column of the block is on screen: {rows:?}"
        );
    }

    /// A key nobody opened has no block to label, and a label in place is left
    /// alone: one set twice must not churn the log.
    #[test]
    fn relabelling_a_block_nobody_opened_is_nothing() {
        let mut log = Log::new();
        log.relabel("nobody", "ok ", LineKind::ToolOutput);
        assert_eq!(log.rows(View::All, 20), 0);

        log.stream("w", "k", LineKind::ToolOutput, "ok ", "out", true);
        log.relabel("k", "ok ", LineKind::ToolOutput);
        // Sealed, so SPEC's `abstract`: the label is the row, and the text is
        // what the click opens.
        assert_eq!(shown(&mut log, 20, 4, 0), vec!["\u{2713} ok"]);
    }

    fn shown(log: &mut Log, width: usize, height: usize, scroll: usize) -> Vec<String> {
        log.window(View::All, width, height, scroll)
            .rows
            .iter()
            .map(|(_, text, _)| text.to_string())
            .collect()
    }

    /// Opens a block and streams `chunks` into it, sealing it on the last one.
    /// Streams `chunks` into one block and seals it.
    ///
    /// # Invariant
    ///
    /// A kind with no mark and no fold, so these cases measure the wrap and the
    /// cap rather than SPEC's `abstract`. The fold has its own tests below.
    fn block(log: &mut Log, key: &str, tag: &str, chunks: &[&str]) {
        for (i, chunk) in chunks.iter().enumerate() {
            log.stream(
                "w",
                key,
                LineKind::System,
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

    /// A deleted session's name may come back, and it must open on nothing.
    ///
    /// # Invariant
    ///
    /// Forgetting a window takes its blocks, its ledger and any stream still open
    /// in it: a chunk the dead session left half-written must not append to the
    /// block the next session opens under the same key.
    #[test]
    fn forgetting_a_window_leaves_nothing_of_it() {
        let mut log = Log::new();
        log.push("peer", LineKind::System, "old");
        log.stream(
            "peer",
            "live",
            LineKind::Reasoning,
            "",
            "half a token",
            false,
        );
        log.push("main", LineKind::System, "kept");
        assert_eq!(log.rows(View::Window("peer"), 40), 2);

        log.forget("peer");
        assert_eq!(log.rows(View::Window("peer"), 40), 0, "the window's rows");
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["kept"], "the merged view");
        assert_eq!(log.len(), 1, "only the kept block is left");
        assert!(log.open.is_empty(), "the dead window's stream survived");

        // The name comes back: the next line is that session's first.
        log.push("peer", LineKind::System, "new");
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["kept", "new"]);
        // And its own stream, under the key the dead session used, opens a block
        // rather than appending to one that is gone.
        log.stream("peer", "live", LineKind::Reasoning, "", "fresh", false);
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["kept", "new", "\u{25c7} fresh"],
            "the half-written token came back"
        );
    }

    /// Two blocks streaming at once each keep growing, in the order they opened.
    #[test]
    fn two_open_blocks_grow_together() {
        let mut log = Log::new();
        grow(&mut log, "a", "[peer] ", "xx");
        grow(&mut log, "b", "[test1] ", "yy");
        assert_eq!(
            shown(&mut log, 12, 4, 0),
            vec!["\u{25c7} [peer] xx", "\u{25c7} [test1] yy"]
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
        assert_eq!(
            all.len(),
            2,
            "two rows at width 12, without a label indent: {}",
            all.join("|")
        );
        assert_eq!(all[0], "[peer] abcdefghijkl", "{all:?}");
        // A window that cuts a paragraph does not relabel it.
        assert_eq!(
            shown(&mut log, 12, 2, 0),
            vec!["[peer] abcdefghijkl", "mnopqrstuvwx"],
            "a continuation never inherits the label as indent"
        );
        // The same rows cut by a *cap* do carry it: row 0 is gone from the view.
        log.set_block_size(2);
        assert_eq!(
            shown(&mut log, 12, 4, 0),
            vec!["[peer] abcdefghijkl", "mnopqrstuvwx"]
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
    ///
    /// # Invariant
    ///
    /// Read with the item opened: a sealed tool result is SPEC's `abstract`, one
    /// summary row, and what it holds is what the click is for.
    #[test]
    fn a_sealed_block_has_no_trailing_blank_row() {
        let mut log = Log::new();
        log.stream("w", "t:c1:Stdout", LineKind::ToolOutput, "", "out\n", false);
        log.stream("w", "t:c1:Stdout", LineKind::ToolOutput, "", "\n", true);
        assert!(log.toggle("t:c1:Stdout"), "a tool result opens");
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["\u{2713} ", "out", ""]);
        // A blank line in the middle is still part of the block.
        let mut log = Log::new();
        log.stream("w", "t:c2:Stdout", LineKind::ToolOutput, "", "a\n\nb", true);
        assert!(log.toggle("t:c2:Stdout"));
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["\u{2713} ", "a", "", "b", ""],
            "a blank row keeps the body's alignment too"
        );
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
            LineKind::System,
            "[m] ",
            "aaaa\nb\ncccc\ndddd",
            false,
        );
        assert_eq!(
            log.rows(View::All, 10),
            4,
            "an ordinary block is not capped"
        );
        assert_eq!(
            shown(&mut log, 10, 4, 0),
            vec!["[m] aaaa", "b", "cccc", "dddd"]
        );
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
        assert_eq!(rows, vec!["◇ [m] amma"], "{rows:?}");
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
        assert!(rows[0].starts_with("◇ [peer] thinking: "), "{rows:?}");
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
            false,
        );
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["✓ [m] c", "d"]);
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
        log.stream("w", "k", LineKind::System, "[m] ", "a\nb", true);
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
        // Both are SPEC's `abstract`: a mark, the label, and -- folded -- no body.
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec![
                "\u{25c7} [main] thinking: thinking",
                "\u{25b8} [main] tool:"
            ]
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
                "\u{25c7} [main] thinking: thinking",
                "\u{25b8} [main] tool:",
                "\u{25c7} [main] thinking: again"
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
        log.stream("w", "k", LineKind::System, "[m] ", "one\ntwo\nthree", false);
        assert_eq!(
            log.rows(View::All, 40),
            4,
            "ordinary text keeps all three rows"
        );
        assert_eq!(log.window(View::All, 40, 2, 0).max_scroll, 2);
        assert_eq!(shown(&mut log, 40, 2, 0), vec!["two", "three"]);
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
                .map(|(_, t, _)| t.to_string())
                .collect::<Vec<_>>(),
            vec!["[main] one", "[peer] two"]
        );
        assert_eq!(
            log.window(View::Window("main"), 40, 10, 0)
                .rows
                .iter()
                .map(|(_, t, _)| t.to_string())
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

    /// Hiding is a reading, not a deletion: the blocks stay, and so does every
    /// line they carry.
    #[test]
    fn a_hidden_session_leaves_the_merged_view_and_comes_back_whole() {
        let mut log = Log::new();
        log.stream("main", "k1", LineKind::Assistant, "[main] ", "one", true);
        log.stream("peer", "k2", LineKind::Assistant, "[peer] ", "two", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[main] one", "[peer] two"]);

        log.set_hidden(&["peer".to_string()]);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[main] one"]);
        assert_eq!(
            log.rows(View::Window("peer"), 40),
            1,
            "the window's own view still holds it"
        );

        // What a hidden session says is kept and not drawn: no row moves.
        log.stream("peer", "k3", LineKind::Assistant, "[peer] ", "three", true);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[main] one"]);

        log.set_hidden(&[]);
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["[main] one", "[peer] two", "[peer] three"],
            "showing the session again is its whole history"
        );
    }

    /// A block growing while hidden keeps the rows around it in place.
    #[test]
    fn a_hidden_block_that_grows_does_not_move_the_rows_around_it() {
        let mut log = Log::new();
        log.set_hidden(&["peer".to_string()]);
        log.stream("main", "k1", LineKind::Assistant, "[main] ", "one", false);
        log.stream("peer", "k2", LineKind::Assistant, "[peer] ", "two", false);
        log.stream("peer", "k2", LineKind::Assistant, "[peer] ", " more", false);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[main] one"]);
        log.stream("main", "k1", LineKind::Assistant, "[main] ", "!", false);
        assert_eq!(shown(&mut log, 40, 10, 0), vec!["[main] one!"]);

        log.set_hidden(&[]);
        assert_eq!(
            shown(&mut log, 40, 10, 0),
            vec!["[main] one!", "[peer] two more"]
        );
    }

    /// A window can cap its blocks apart from the rest.
    #[test]
    fn a_window_can_cap_its_own_blocks() {
        let mut log = Log::new();
        log.set_block_size(4);
        log.stream(
            "cmd",
            "k1",
            LineKind::ToolOutput,
            "",
            "a\nb\nc\nd\ne",
            false,
        );
        log.stream(
            "main",
            "k2",
            LineKind::ToolOutput,
            "",
            "a\nb\nc\nd\ne",
            false,
        );
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
