//! The scrollback: row offsets cached per line, so a frame never walks the log.
//!
//! # Invariant
//!
//! A frame touches only the visible rows, and an append lands on exactly the
//! rows a reflow of the whole line would. See `docs/notes/tui-log.md`.

use std::borrow::Cow;
use std::collections::HashMap;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::LineKind;
use crate::markdown;

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
    /// The markdown document this block was written as, when it is one.
    ///
    /// # Invariant
    ///
    /// An answer's rows are derived from it, never appended to: one more delta can
    /// change a row above the last one, and a table's columns depend on the width.
    source: Option<String>,
    /// The rows of that document, already styled and already fitted to `width`.
    ///
    /// # Invariant
    ///
    /// Non-empty exactly when `source` is: a row is a `Line` of runs, drawn as it is.
    styled: Vec<Line<'static>>,
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
            source: match kind {
                LineKind::Assistant => Some(text.clone()),
                _ => None,
            },
            text: match kind {
                LineKind::Assistant => String::new(),
                _ => text,
            },
            width,
            expanded: false,
            inner: 0,
            starts: vec![0],
            col: 0,
            sealed: false,
            call: None,
            sections: String::new(),
            styled: Vec::new(),
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
        match self.source {
            // Not measured yet: a block still owes the frame its one blank row.
            Some(_) => self.styled.len().max(1),
            None => self.starts.len(),
        }
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

    /// Recomputes every body row at `width`, independently of the title.
    ///
    /// # Invariant
    ///
    /// A markdown block is laid out at the width it is drawn at, so a table's columns
    /// are the window's and a run keeps its ink across a break. Every other block is
    /// wrapped from the text it already holds. See `docs/notes/tui-log.md`.
    fn reflow(&mut self, width: usize) {
        if width == 0 {
            // Not measured yet: the first draw reflows at its own width.
            self.starts = vec![0];
            self.styled = vec![Line::default()];
            return;
        }
        let width = self.content_width(width);
        self.sections = match self.expanded {
            true => self.section_text(width).unwrap_or_default(),
            false => String::new(),
        };
        let structured = self.expanded && self.call.is_some();
        self.width = width;
        if let Some(source) = &self.source {
            self.styled = markdown::rows(source, width);
            // The label is chrome on every block, and the first row carries it.
            if !self.tag.is_empty() {
                let room = width.saturating_sub(self.kind.mark().width());
                let label = format!("{}{}", self.kind.mark(), cut(&self.tag, room));
                if let Some(first) = self.styled.first_mut() {
                    first.spans.insert(0, Span::styled(label, Style::default()));
                }
            }
            if self.styled.is_empty() {
                self.styled.push(Line::default());
            }
            self.col = self.styled.last().map_or(0, |row| row.width().min(width));
            return;
        }
        self.starts.clear();
        // The top border owns the label. A structured body starts immediately
        // inside the left border, not behind another copy of the mark's padding.
        // Symbols and labels are title chrome, never a hanging indent on the body.
        let mut wrap = Wrap { width, col: 0 };
        // The first row of any block exists before a character is wrapped into it.
        self.starts.push(0);
        if structured {
            let text = self.sections.clone();
            wrap.feed(&text, 0, &mut self.starts);
            self.col = wrap.col;
            return;
        }
        let Self { text, starts, .. } = self;
        let text = text.as_str();
        wrap.feed(text, 0, starts);
        self.col = wrap.col;
    }

    /// Appends to the body the current view reads.
    ///
    /// # Invariant
    ///
    /// An answer appends to its *document*, and the rows are laid out again: markdown
    /// is not append-safe, so a delta can change a row above the one arriving.
    /// See `docs/notes/tui-log.md`.
    fn append(&mut self, text: &str, width: usize) {
        if let Some(source) = &mut self.source {
            source.push_str(text);
            self.reflow(width);
            return;
        }
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
        let start = match self.starts.get(row) {
            Some(start) => *start,
            None => return "",
        };
        let end = self.starts.get(row + 1).copied().unwrap_or(text.len());
        // A start the text no longer reaches is no slice to take: return nothing.
        let text = match text.get(start..end) {
            Some(text) => text,
            None => return "",
        };
        text.strip_suffix('\n').unwrap_or(text)
    }

    /// The item's own label: its mark and its tag, and none of its body.
    fn label(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{}{}", self.kind.mark(), self.summary()))
    }

    /// How row `row` is drawn: a document's runs, or the block's own text.
    ///
    /// # Invariant
    ///
    /// A markdown block yields its rows exactly as the document styled them: nothing
    /// about the drawing is decided again here -- not the ink, not the wrap.
    /// See `docs/notes/tui-log.md`.
    fn shown(&self, row: usize, labelled: bool) -> Shown<'_> {
        match self.source {
            Some(_) => match self.styled.get(row) {
                Some(line) => Shown::Styled(&line.spans),
                None => Shown::Text(Cow::Borrowed("")),
            },
            None => Shown::Text(self.view_row(row, labelled)),
        }
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

/// One row of a frame: what it is, how it is drawn, and the box it sits inside.
///
/// # Invariant
///
/// `edges` is empty for a row that is not part of a frame; a block the operator
/// opened is the only thing SPEC gives a scroll of its own, so it is the only one
/// that has a box, and the box's sides are what the renderer draws around it.
pub type Row<'a> = (LineKind, Shown<'a>, Edges);

/// How one row of a frame is drawn.
///
/// # Invariant
///
/// A `Styled` row is drawn exactly as it is: its runs were decided by the document
/// that wrote it, so a row a break split keeps its ink on both sides. `Text` is a row
/// no document wrote, and the frame draws it in its kind's ink.
pub enum Shown<'a> {
    Text(Cow<'a, str>),
    Styled(&'a [Span<'static>]),
}

impl Shown<'_> {
    /// The row's characters, whatever ink they are drawn in.
    pub fn text(&self) -> Cow<'_, str> {
        match self {
            Shown::Text(text) => Cow::Borrowed(text),
            Shown::Styled(spans) => Cow::Owned(
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
            ),
        }
    }
}

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

/// One cell of a view, as the walk that answers a press and a wheel finds it.
struct Hit {
    /// The block shown there.
    block: usize,
    /// Which of that block's shown rows the cell is, from the top.
    index: usize,
    /// How many rows the block occupies here, box included.
    shown: usize,
    /// Whether those rows are an opened item's box.
    boxed: bool,
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
        let before = self.contributions(at);
        let width = self.width;
        let entry = &mut self.entries[at];
        match &mut entry.source {
            // An answer's text is derived, so what changed is the document.
            Some(source) => {
                if source == text {
                    return;
                }
                source.clear();
                source.push_str(text);
            }
            None => {
                if entry.text == text {
                    return;
                }
                entry.text.clear();
                entry.text.push_str(text);
            }
        }
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
    /// Answers whether the box took the wheel: exactly when it moved. A box with
    /// nothing left in the wheel's direction -- at either end, or under a
    /// `block_size` of 0, which caps nothing and so leaves nothing inside to
    /// scroll -- hands the event to the pane, which is all SPEC's
    /// `scroll if block_size!=0` can mean. No other number is consulted, so one
    /// cell behaves the same way whatever the cap is.
    pub fn scroll_block(&mut self, at: usize, rows: i64) -> bool {
        // Use the row offsets of the frame the pointer actually landed on.
        // Reflowing a resized window before the next draw would move the hit.
        let Some(entry) = self.entries.get(at) else {
            return false;
        };
        if !entry.boxed() {
            return false;
        }
        let cap = self.room(entry.kind, 0);
        let bound = entry.rows() - entry.rows().min(cap);
        let current = entry.inner;
        let next = (current as i64 + rows).clamp(0, bound as i64) as usize;
        self.entries[at].inner = next;
        next != current
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
        self.hit(view, width, height, scroll, row)
            .map(|hit| hit.block)
    }

    /// The block whose body holds this row: the one thing a wheel there scrolls.
    ///
    /// # Invariant
    ///
    /// One walk answers both questions a cell asks -- which block a press names, and
    /// whether the wheel is a box's -- so a hit region cannot disagree with the
    /// frame. A box's first and last rows are its rules, which are not content:
    /// the wheel over them is the pane's. That is the edge an operator needs to be
    /// able to reach through.
    pub fn body_at(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<usize> {
        let hit = self.hit(view, width, height, scroll, row)?;
        let rule = hit.index == 0 || hit.index + 1 == hit.shown;
        match hit.boxed && rule {
            true => None,
            false => Some(hit.block),
        }
    }

    /// The block under one row, with where that row sits inside it.
    ///
    /// # Invariant
    ///
    /// `shown` is the rows the block occupies in this view, box included, so the
    /// rows that carry the box's rules are its first and its last.
    fn hit(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<Hit> {
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
        let block = *ledger.blocks.get(pos)?;
        let entry = self.entries.get(block)?;
        let boxed = entry.boxed();
        Some(Hit {
            block,
            index: want - ledger.index.prefix(pos),
            shown: entry.visible(self.room(entry.kind, ledger.cap)),
            boxed,
        })
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
        // An answer's text is derived from its document: the document is trimmed.
        let field = match &mut entry.source {
            Some(source) => source,
            None => &mut entry.text,
        };
        let kept = field.trim_end().len();
        if kept == field.len() {
            return;
        }
        field.truncate(kept);
        if width > 0 {
            entry.reflow(width);
        } else {
            entry.starts = vec![0];
            entry.styled = vec![Line::default()];
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
        if start < end && !ledger.blocks.is_empty() {
            // Jump to the block holding the first visible row: O(log blocks).
            let mut pos = ledger.index.line_at(start).min(ledger.blocks.len() - 1);
            let mut first = ledger.index.prefix(pos);
            while let Some(entry) = ledger.blocks.get(pos).and_then(|at| self.entries.get(*at)) {
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
                        rows.push((entry.kind, Shown::Text(entry.label()), Edges::TOP));
                        continue;
                    }
                    if boxed && index + 1 == shown {
                        rows.push((entry.kind, Shown::Text(Cow::Borrowed("")), Edges::BOTTOM));
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
                    rows.push((entry.kind, entry.shown(body_row, print), edges));
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
