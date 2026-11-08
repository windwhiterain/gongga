//! The scrollback: row offsets cached per line, so a frame never walks the log.
//!
//! # Invariant
//!
//! A frame touches only the visible rows, and an append lands on exactly the
//! rows a reflow of the whole line would. See `docs/notes/tui-log.md`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::LineKind;
use crate::cache::{Cache, Record};
use crate::markdown;
use client::protocol;

/// One block: a line that may still be growing, plus its rows.
///
/// # Invariant
///
/// `tag` holds the label the block opened with and is *not* part of `text`, so a
/// block whose first row is above the window can repeat it. `window` is the name the
/// line carried, and `at` is where the block sits inside that window's own rows.
struct Entry {
    /// The journal's name for this block: a key can be used twice, an id cannot.
    id: u64,
    kind: LineKind,
    /// The window this block belongs to, as the line said.
    ///
    /// # Invariant
    ///
    /// Shared with its window's ledger, so a session holds its own name once
    /// however many blocks it writes. See `docs/notes/tui-log.md`.
    window: Arc<str>,
    /// The caller's name for this block, so a click can find it again.
    key: String,
    /// The label this block was opened with, printed on its top visible row.
    tag: String,
    text: String,
    /// The columns this block was last wrapped at: the fold is cut to them.
    width: usize,
    /// Whether the operator opened it: SPEC's click, and the only state a fold has.
    expanded: bool,
    /// The box's first visible row, counted from the block's own beginning.
    inner: usize,
    /// Byte offset of each wrapped row's first character; its length is the row count.
    starts: Vec<usize>,
    /// Display columns used by the last row: the whole wrap state an append resumes.
    col: usize,
    /// No more text can arrive, so the block is scrollback.
    sealed: bool,
    /// Text this block holds that its journal does not.
    ///
    /// # Invariant
    ///
    /// Set by an append and cleared by the seal that writes the block whole, so
    /// `Some(slot)` plus `pending` is a block whose record is older than what is on
    /// screen. Such a block may not be dropped: reading it back would lose the
    /// rows the operator is watching. See `docs/notes/tui-cache.md`.
    pending: bool,
    /// Structured call fields, once the call's head arrives.
    call: Option<client::protocol::Head>,
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
    /// Where the journal holds this block's whole current form, once it has one.
    ///
    /// # Invariant
    ///
    /// `Some` exactly when the block can be dropped and read back: a block that was
    /// never sealed has no record that stands for it alone.
    slot: Option<u64>,
    /// This block's rows at `width`, held instead of its body.
    ///
    /// # Invariant
    ///
    /// `Some` exactly when the body is not here, so every reader of `text`, `source`
    /// or `starts` either holds a body or has not been asked. See
    /// `docs/notes/tui-cache.md`.
    cold: Option<usize>,
}

impl Entry {
    fn new(id: u64, window: &str, kind: LineKind, text: String, width: usize) -> Self {
        Self::with_tag(id, window, kind, "", text, width)
    }

    /// A block that streams under `tag`: the label is not text, so no cut can take it.
    fn with_tag(
        id: u64,
        window: &str,
        kind: LineKind,
        tag: &str,
        text: String,
        width: usize,
    ) -> Self {
        let mut entry = Self {
            id,
            kind,
            window: Arc::from(window),
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
            pending: false,
            call: None,
            sections: String::new(),
            styled: Vec::new(),
            at: 0,
            slot: None,
            cold: None,
        };
        // Width 0 means "not measured yet": the first draw reflows at its own width.
        if width > 0 {
            entry.reflow(width);
        }
        entry
    }

    fn rows(&self) -> usize {
        if let Some(rows) = self.cold {
            return rows;
        }
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

    /// The rows this block occupies at `cap`, which is its body plus its box.
    fn visible(&self, cap: usize) -> usize {
        let body = self.rows().min(cap);
        match self.boxed() {
            true => body + 2,
            false => body,
        }
    }

    /// Rows this box holds above what its cap leaves it: how far it can scroll.
    fn travel(&self, cap: usize) -> usize {
        let body = self.rows().min(cap);
        self.rows().saturating_sub(body)
    }

    /// The box's first visible row, clamped to what its travel allows.
    fn inside(&self, cap: usize) -> usize {
        self.inner.min(self.travel(cap))
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
            // A call's label, or its summary composed from the fields: they are
            // the only copy kept.
            (LineKind::ToolCall, _) => match self.tag.trim_end() {
                "" => match &self.call {
                    Some(call) => call.summary(),
                    None => self.row(0).to_string(),
                },
                label => label.to_string(),
            },
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
                true if call.name == "bash" || call.name == "exec" => "command",
                true => "value",
                false => field.label.as_str(),
            };
            // A value starts at the left edge: an indent is columns it could have
            // used. See `docs/notes/tui-log.md`.
            let mut lines = field.value.split('\n');
            rows.push(match lines.next().unwrap_or_default() {
                "" => format!("{name}:"),
                first => format!("{name}: {first}"),
            });
            rows.extend(lines.map(str::to_string));
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
        if self.cold.is_some() {
            // Nothing to lay out: the rows are the ones the drop left behind.
            return;
        }
        if width == 0 {
            // Not measured yet: the first draw reflows at its own width.
            self.starts = vec![0];
            self.styled = vec![Line::default()];
            return;
        }
        let width = width.max(1);
        // A box spends two columns of its label on the top rule's air and dash.
        // See `docs/notes/tui-log.md`.
        if let Some(call) = &self.call {
            let rule = 2 * usize::from(self.boxed());
            self.tag = cut(
                &call.summary(),
                width.saturating_sub(self.kind.mark().width() + rule),
            );
        }
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
        // The top border owns the label; the body starts inside the left border.
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

    /// Appends to the body the block is drawn from.
    ///
    /// # Invariant
    ///
    /// An answer appends to its *document*, and the rows are laid out again: markdown
    /// is not append-safe, so a delta can change a row above the one arriving. At
    /// width 0 nothing is measured, and nothing may pretend to be. See
    /// `docs/notes/tui-log.md`.
    fn append(&mut self, text: &str, width: usize) {
        if let Some(source) = &mut self.source {
            source.push_str(text);
            self.reflow(width);
            return;
        }
        if width == 0 {
            // A window no pane has drawn yet: one placeholder row until it does.
            self.text.push_str(text);
            return;
        }
        let width = width.max(1);
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
        // The title is one horizontal unit: symbol, label, then body. Only the
        // title is clipped.
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

impl Entry {
    /// The record that stands for this block on its own.
    fn seal_record(&self) -> Record {
        Record::Seal {
            id: self.id,
            kind: self.kind,
            tag: self.tag.clone(),
            key: self.key.clone(),
            text: self.whole_text(),
            expanded: self.expanded,
            call: self.call.clone(),
        }
    }

    /// The text this block was written as: an answer's document, else its body.
    fn whole_text(&self) -> String {
        match &self.source {
            Some(source) => source.clone(),
            None => self.text.clone(),
        }
    }

    /// Whether the body is here rather than in the journal.
    fn hot(&self) -> bool {
        self.cold.is_none()
    }

    /// Drops the body and keeps the rows it had, so the index does not move.
    ///
    /// # Invariant
    ///
    /// Only a block the journal can stand for alone: a block still growing has no
    /// seal, and one holding text the journal has not been told yet would come back
    /// missing its newest rows. Dropping either would lose what is on screen.
    fn drop_body(&mut self) {
        if self.cold.is_some() || self.slot.is_none() || self.pending {
            return;
        }
        self.cold = Some(self.rows());
        self.text = String::new();
        self.source = None;
        self.styled = Vec::new();
        self.sections = String::new();
        self.starts = Vec::new();
        self.col = 0;
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

/// Soft-wraps rows keeping their indentation, in real spaces, so slicing and
/// drawing measure one width.
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

/// One row of a frame: what it is, how it is drawn, and which rule of a box it is.
///
/// # Invariant
///
/// A block the operator opened is the only thing SPEC gives a scroll of its own, so
/// it is the only one that has a box, and the box is one rule above its rows and one
/// below: a row is the top rule, the bottom rule, or neither. See
/// `docs/notes/tui-log.md`.
pub type Row<'a> = (LineKind, Shown<'a>, Rule);

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

/// Which rule of its box a row is drawn as.
///
/// # Invariant
///
/// A box is a rule above and a rule below and nothing on its sides: the sides cost
/// every row of the body two columns, and the pair of rules already says where the
/// box starts and ends. So a body row is as wide as the window, and starts at the
/// window's own left edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Rule {
    /// A row of the body of a block that has no box.
    #[default]
    Free,
    /// A row of the box's body, inside the margin the box leaves on each side.
    Inside,
    /// The box's top rule, which also carries the item's label.
    Top,
    /// The box's bottom rule.
    Bottom,
}

/// What one frame needs: the rows that fit, borrowed from the log, and the bound.
pub struct Viewport<'a> {
    pub rows: Vec<Row<'a>>,
    /// Rows hidden above the window; only the renderer can work this out.
    pub max_scroll: usize,
}

/// One window's row bookkeeping over the shared blocks.
///
/// # Invariant
///
/// One ledger, one width. A block belongs to exactly one window and is laid out
/// once, at the width of the pane that draws that window -- so a frame that asks
/// two panes for two widths pays for each block at most once, and only a change of
/// shape pays again. See `docs/notes/tui-log.md`.
#[derive(Default)]
struct Ledger {
    /// Global index of this window's k-th block, in arrival order.
    blocks: Vec<usize>,
    index: RowIndex,
    /// Rows one block may fill in this window; 0 defers to the log's `block_size`.
    cap: usize,
    /// The pages this window holds, in the order it draws them.
    ///
    /// # Invariant
    ///
    /// Each is a block id and the oldest row that page stands for: a page is what one
    /// fill landed, and this is the only place a row and a block are joined.
    pages: Vec<(u64, i64)>,
    /// The block a page now landing begins with, until its answer names the rows.
    page_first: Option<u64>,
    /// The width `blocks` were last laid out at; 0 until a frame asks.
    width: usize,
}

/// One cell of a window, as the walk that answers a press and a wheel finds it.
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
/// ledger per window over them.
///
/// # Invariant
///
/// A block's position in `entries` is fixed when it opens, so two blocks that
/// stream at once each keep growing where they are instead of trading places.
/// See `docs/notes/tui-log.md`.
pub struct Log {
    entries: Vec<Entry>,
    /// One ledger per window name a line has carried, each at its own width.
    views: HashMap<Arc<str>, Ledger>,
    /// Open stream key -> its block. Its identity, not its position.
    open: HashMap<String, usize>,
    /// How many rows one block may fill. See [`Log::set_block_size`].
    block_size: usize,
    /// Columns of a window a box leaves to the pane on each side. See
    /// [`Log::set_box_edge`].
    edge: usize,
    /// The journal this log is a view of. See `docs/notes/tui-cache.md`.
    cache: Cache,
    /// The next block id to hand out: monotonic within this window's journal.
    next_id: u64,
    /// The blocks whose bodies are here. Eviction walks this, not the log.
    hot: Vec<usize>,
    /// The windows a fill is landing in, and the floor of the page it opens when the
    /// journal named it.
    ///
    /// # Invariant
    ///
    /// One entry per fill, set when it starts and taken by the first block it lands:
    /// that block is where the page begins, which is the only place a `Floor` record
    /// may stand.
    filling: HashMap<String, Option<i64>>,
    /// The window whose next blocks are older than the ones it holds, and where in
    /// its ledger the next of them goes.
    ///
    /// # Invariant
    ///
    /// A page of the past is drawn above what the window already has, so its blocks
    /// take their places from the front rather than the end. Set by
    /// [`Log::older`], cleared by the seal the page's answer brings.
    prepend: Option<(String, usize)>,
    /// Windows this process has already decided the beginning of.
    ///
    /// # Invariant
    ///
    /// A window is here once, and only a name that arrives after this process
    /// started is not. A window that received one line before its first frame is
    /// still unseeded: the line is realtime, and the history behind it is the
    /// journal's. See `docs/notes/tui-cache.md`.
    seeded: HashSet<String>,
}

impl Default for Log {
    fn default() -> Self {
        Self::without_cache()
    }
}

/// How many records one walk of the journal reads at a time.
///
/// # Invariant
///
/// A read granularity, not a limit: a seed keeps walking until the window it is
/// filling has the rows a pane is about to draw.
const WALK: usize = 64;

/// The rows of a block that fills every row it has.
const NO_CAP: usize = usize::MAX;

/// How many of a box's own columns on each side belong to the wheel of the window.
///
/// # Invariant
///
/// A region, never a layout: the box is drawn at the window's full width either way,
/// and these columns are only where a wheel is the pane's. Never more than leaves the
/// box one column of its own. See `docs/notes/tui-log.md`.
pub(crate) fn box_margin(width: usize, edge: usize) -> usize {
    edge.min(width.saturating_sub(1) / 2)
}

/// The rows a block of `kind` may fill: the window's `cap`, else the log's.
fn room(kind: LineKind, cap: usize, block_size: usize) -> usize {
    // SPEC caps streamed abstracts, not commands, results, messages or errors.
    if !kind.folds() {
        return NO_CAP;
    }
    if cap > 0 {
        return cap;
    }
    if block_size > 0 { block_size } else { NO_CAP }
}

impl Log {
    pub fn new() -> Self {
        Self::default()
    }

    /// A scrollback that keeps nothing on disk.
    fn without_cache() -> Self {
        Self {
            entries: Vec::new(),
            views: HashMap::new(),
            open: HashMap::new(),
            block_size: 0,
            edge: protocol::DEFAULT_BOX_EDGE,
            // Nothing is ever written: a log built with no front-end in mind does
            // not invent a directory.
            cache: Cache::none(),
            filling: HashMap::new(),
            prepend: None,
            next_id: 0,
            hot: Vec::new(),
            seeded: HashSet::new(),
        }
    }

    /// A scrollback whose blocks are written down for one endpoint.
    ///
    /// # Invariant
    ///
    /// `root` is this client's own directory and `endpoint` is where it dials, both
    /// known before any server speaks. See `docs/notes/tui-cache.md`.
    pub fn with_cache(root: &std::path::Path, endpoint: &str) -> Self {
        Self {
            cache: Cache::new(root, endpoint),
            ..Self::without_cache()
        }
    }

    /// Points this scrollback at another client root or endpoint, keeping nothing.
    ///
    /// # Invariant
    ///
    /// The windows lose their blocks, so the next frame seeds each from the new
    /// place: what is on screen is the rows of the endpoint being dialled now, and
    /// a `connect_server` that moved the window must not leave the last server's
    /// lines behind. The old journals are not deleted -- they are that server's
    /// record, and switching back finds them again.
    pub fn repoint(&mut self, root: &std::path::Path, endpoint: &str) {
        self.cache = Cache::new(root, endpoint);
        let windows: Vec<String> = self.views.keys().map(|name| name.to_string()).collect();
        for window in windows {
            self.drop_window(&window);
        }
        // Another endpoint is another journal: every window starts over.
        self.seeded.clear();
    }

    /// Hands out the journal's next block id.
    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Reads a dropped body back, so a mutation has something to change.
    ///
    /// # Invariant
    ///
    /// Every mutation of a block calls this first: a record is dumped from the
    /// entry, and an entry holding only rows would write an empty block over a
    /// block the operator can see. See `docs/notes/tui-cache.md`.
    fn touch(&mut self, at: usize) {
        let Some(entry) = self.entries.get(at) else {
            return;
        };
        if entry.hot() {
            return;
        }
        let width = self.width_of(&entry.window);
        materialize(&mut self.entries, &self.cache, at, width);
    }

    /// Writes one record about block `at`.
    fn write(&mut self, at: usize, record: Record) -> Option<u64> {
        let window = self.entries.get(at)?.window.clone();
        self.cache.append(&window, &record)
    }

    /// Records a block's whole current form, and where it can be read back from.
    ///
    /// # Invariant
    ///
    /// This is the only writer a *growing* block has: a stream keeps its text in
    /// memory and lands here once, when it ends, so a journal never grows one chunk
    /// at a time. See `docs/notes/tui-cache.md`.
    fn record_seal(&mut self, at: usize) {
        if at >= self.entries.len() {
            return;
        }
        let record = self.entries[at].seal_record();
        if let Some(slot) = self.write(at, record) {
            self.entries[at].slot = Some(slot);
            // The file now holds what is on screen, so the body may be dropped.
            self.entries[at].pending = false;
        }
    }

    /// Writes down what a window said about itself: a record, never a row.
    ///
    /// # Invariant
    ///
    /// Nothing is added to the log, so what a note costs the frame is nothing, and
    /// replaying the window draws what it drew. See `docs/notes/tui-cache.md`.
    pub fn note(&mut self, window: &str, kind: LineKind, text: &str) {
        self.cache.note(window, kind, text);
    }

    /// Caps how tall a block may get: `0` caps nothing, and a block past its cap
    /// keeps its newest rows.
    pub fn set_block_size(&mut self, lines: usize) {
        if self.block_size == lines {
            return;
        }
        self.block_size = lines;
        self.reindex();
    }

    /// How many of a box's own columns on each side scroll the window instead.
    ///
    /// # Invariant
    ///
    /// Nothing to lay out: the box is drawn at the window's full width either way.
    pub fn set_box_edge(&mut self, columns: usize) {
        self.edge = columns;
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
        self.reindex_window(window);
    }

    /// Drops every block the window `window` holds: the session that wrote it is gone.
    ///
    /// # Invariant
    ///
    /// The name keeps nothing -- no block, no ledger, and no stream left open in
    /// it -- so a block arriving under a reused name is that session's first and
    /// starts at row zero. See `docs/notes/tui-log.md`.
    pub fn forget(&mut self, window: &str) {
        if self
            .prepend
            .as_ref()
            .is_some_and(|(name, _)| name == window)
        {
            self.prepend = None;
        }
        // A fill that empties a window opens a page too.
        self.begins(window);
        self.cache.remove(window);
        self.drop_window(window);
        // The name may be handed out again: its next beginning is the server's.
        self.seeded.remove(window);
    }

    /// Records that every row up to `seq` of the session is on screen, and which
    /// session that is.
    ///
    /// # Invariant
    ///
    /// The client's side of the sync stamp: the greeting says how far the server's
    /// transcript reaches, this says how far this window's drawing of it does, and
    /// the difference is what a refill fetches. Written to the journal so a
    /// restart answers the same question with the same number.
    pub fn stamp(&mut self, window: &str, created: i64, seq: i64) {
        self.cache.stamp(window, created, seq);
    }

    /// The oldest transcript row this window holds, among the pages in memory.
    ///
    /// # Invariant
    ///
    /// From memory, never from the journal: a page the journal holds and a seed did
    /// not read back is not a row this window can show, and answering with the
    /// journal's floor would have the next page land on rows already here. See
    /// `docs/notes/tui-cache.md`.
    pub fn floor_of(&self, window: &str) -> Option<i64> {
        if let Some(ledger) = self.ledger(window)
            && let Some((_, seq)) = ledger.pages.first()
        {
            return Some(*seq);
        }
        // Nothing drawn yet: the journal names the page the first frame reads back.
        self.cache
            .floor(window, self.cache.end(window))
            .map(|(_, seq)| seq)
    }

    /// Says a fill is landing in this window, so its first block opens a page.
    ///
    /// # Invariant
    ///
    /// Set by the two things a fill starts with -- [`Log::rewind`] for the tail and
    /// [`Log::older`] for the page above -- and taken by the first block that lands:
    /// the journal's page boundary may only stand where a page truly begins.
    fn begins(&mut self, window: &str) {
        self.filling.insert(window.to_string(), None);
    }

    /// The rows arriving from here on are older than the ones this window holds.
    ///
    /// # Invariant
    ///
    /// Nothing is dropped: a page of the past is added above the held rows, and the
    /// row the window ends at does not move -- which is why the answer to it is a
    /// [`Log::landed`] and not a stamp.
    pub fn older(&mut self, window: &str) {
        self.begins(window);
        self.prepend = Some((window.to_string(), 0));
    }

    /// A page has landed, so this window holds every row from `floor` up to `drawn`.
    ///
    /// # Invariant
    ///
    /// The journal is written in the order the pane draws, with the page boundary in
    /// front of the block the page began with: a page of older rows therefore lands
    /// above the held ones, and a restart replays from that boundary. Left alone when
    /// a block cannot stand for itself -- a journal missing a body is worse than an
    /// order a restart would get wrong. See `docs/notes/tui-cache.md`.
    pub fn landed(&mut self, window: &str, created: i64, drawn: i64, floor: i64) {
        let prepending = self
            .prepend
            .as_ref()
            .is_some_and(|(name, _)| name == window);
        if prepending {
            self.prepend = None;
        }
        self.filling.remove(window);
        let width = self.width_of(window);
        let Some(ledger) = self.views.get_mut(window) else {
            return;
        };
        let blocks = ledger.blocks.clone();
        let Some(first) = ledger.page_first.take() else {
            // No block opened a page: an answer with no rows in it names nothing.
            return;
        };
        // The list is the order the pane draws: a page of the past opens first.
        match prepending {
            true => ledger.pages.insert(0, (first, floor)),
            false => ledger.pages.push((first, floor)),
        }
        // A page whose blocks are gone names nothing: the boundary would stand in
        // front of a block this window no longer draws.
        let drawn_ids: Vec<u64> = blocks.iter().map(|&at| self.entries[at].id).collect();
        ledger.pages.retain(|(id, _)| drawn_ids.contains(id));
        let boundaries: Vec<(u64, i64)> = ledger.pages.clone();
        let mut records = Vec::with_capacity(blocks.len() + boundaries.len() + 1);
        let mut written: Vec<Option<usize>> = vec![None; blocks.len()];
        for (place, &at) in blocks.iter().enumerate() {
            materialize(&mut self.entries, &self.cache, at, width);
            let entry = &self.entries[at];
            if !entry.hot() {
                return;
            }
            if let Some((_, seq)) = boundaries.iter().find(|(id, _)| *id == entry.id) {
                records.push(Record::Floor { seq: *seq });
            }
            written[place] = Some(records.len());
            records.push(entry.seal_record());
        }
        records.push(Record::Stamp {
            created,
            seq: drawn,
        });
        let Some(offsets) = self.cache.rewrite(window, &records) else {
            return;
        };
        // Read off the list that was built: a boundary before a block shifts it.
        for (&at, place) in blocks.iter().zip(written) {
            let Some(place) = place else {
                continue;
            };
            self.entries[at].slot = Some(offsets[place]);
            self.entries[at].pending = false;
        }
    }

    /// What this window's journal says it has drawn, and of which session.
    pub fn stamp_of(&self, window: &str) -> Option<(i64, i64)> {
        let end = self.cache.end(window);
        self.cache
            .stamp_at(window, end)
            .map(|(_, created, seq)| (created, seq))
    }

    /// Throws away what no stored row accounts for: the server is about to send
    /// this window's tail again.
    ///
    /// # Invariant
    ///
    /// "Unaccounted" is a position in the journal, not a count of blocks: every
    /// block whose record was written above the newest `Stamp`, and every block
    /// this process drew and has not written down at all. A window read back only
    /// in part therefore cuts in the right place, which a count of blocks it had
    /// loaded would not.
    pub fn rewind(&mut self, window: &str) {
        self.begins(window);
        self.cache.rewind(window);
        let cut = self.stamp_offset(window);
        self.cut_tail(window, cut);
    }

    /// Where the newest stamp of a window sits in its journal, if it has one.
    fn stamp_offset(&self, window: &str) -> Option<u64> {
        let end = self.cache.end(window);
        self.cache.stamp_at(window, end).map(|(at, _, _)| at)
    }

    /// Drops a window's unaccounted tail, keeping the blocks below `cut`.
    fn cut_tail(&mut self, window: &str, cut: Option<u64>) {
        let doomed = |entry: &Entry, cut: Option<u64>| {
            if entry.window.as_ref() != window {
                return false;
            }
            match (entry.slot, cut) {
                // Never written down: drawn after the last row, by definition.
                (None, _) => true,
                // Written above the newest stamp: also after it.
                (Some(slot), Some(cut)) => slot > cut,
                // A journal with no stamp at all: nothing here is in question.
                (Some(_), None) => false,
            }
        };
        if !self.entries.iter().any(|entry| doomed(entry, cut)) {
            return;
        }
        let mut moved = Vec::with_capacity(self.entries.len());
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            let mine = doomed(&entry, cut);
            moved.push((!mine).then_some(kept.len()));
            if !mine {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.open = self
            .open
            .drain()
            .filter_map(|(key, at)| moved.get(at).copied().flatten().map(|at| (key, at)))
            .collect();
        self.reindex();
    }

    /// Takes a window's blocks out of memory, keeping its journal.
    fn drop_window(&mut self, window: &str) {
        if self.views.remove(window).is_none() {
            return;
        }
        // Where each surviving block lands, so an open stream still names its own
        // block once the ones behind it are gone.
        let mut moved = Vec::with_capacity(self.entries.len());
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            let mine = entry.window.as_ref() != window;
            moved.push(mine.then(|| kept.len()));
            if mine {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.open = self
            .open
            .drain()
            .filter_map(|(key, at)| moved[at].map(|at| (key, at)))
            .collect();
        self.reindex();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The rows one block of `kind` may fill under `cap`: the answer keeps its own.
    fn room(&self, kind: LineKind, cap: usize) -> usize {
        room(kind, cap, self.block_size)
    }

    /// The rows block `at` occupies in its own window's ledger.
    fn contributions(&self, at: usize) -> usize {
        let entry = &self.entries[at];
        // The box is two rows the block occupies, so the index counts them.
        entry.visible(self.room(entry.kind, self.cap_of(&entry.window)))
    }

    /// The cap a window puts on its own blocks, or 0 for the log's.
    fn cap_of(&self, window: &str) -> usize {
        self.views.get(window).map(|ledger| ledger.cap).unwrap_or(0)
    }

    /// The width this window's blocks are laid out at; 0 until a frame asks.
    fn width_of(&self, window: &str) -> usize {
        self.views
            .get(window)
            .map(|ledger| ledger.width)
            .unwrap_or(0)
    }

    /// Moves one window's row sum by what a change to one block did to it.
    fn shift(&mut self, at: usize, before: usize, after: usize) {
        let window = self.entries[at].window.clone();
        let pos = self.entries[at].at;
        if let Some(ledger) = self.views.get_mut(&window) {
            ledger.index.shift(pos, after as isize - before as isize);
        }
    }

    /// Adds a finished line to the window `name` names. Costs what the line is worth.
    pub fn push(&mut self, name: &str, kind: LineKind, text: impl Into<String>) {
        let text = text.into();
        let id = self.next_id();
        let slot = self.cache.append(
            name,
            &Record::Line {
                id,
                kind,
                text: text.clone(),
            },
        );
        let width = self.width_of(name);
        let mut entry = Entry::new(id, name, kind, text, width);
        // A whole row arrives at once: nothing will stream into it, so a folding
        // kind is an `abstract` from its first frame.
        entry.sealed = true;
        entry.slot = slot;
        self.entries.push(entry);
        self.attach(self.entries.len() - 1);
    }

    /// Records block `at` in its own window's ledger.
    ///
    /// # Invariant
    ///
    /// The ledger's order is the order the pane draws, so a block of a page of older
    /// rows lands where that page's own order puts it: in front of the rows the
    /// window already held. Every block it moved keeps the position it is summed at.
    fn attach(&mut self, at: usize) {
        let rows = self.contributions(at);
        // The name is the ledger's key: the block takes that copy, not its own.
        let shared = self
            .views
            .get_key_value(self.entries[at].window.as_ref())
            .map(|(name, _)| name.clone());
        if let Some(shared) = shared {
            self.entries[at].window = shared;
        }
        let window = self.entries[at].window.clone();
        // A fill's first block is where its page begins: taken here, once.
        let opens = self.filling.remove(window.as_ref());
        let above = match &self.prepend {
            Some((name, pos)) if window.as_ref() == name.as_str() => Some(*pos),
            _ => None,
        };
        let block_size = self.block_size;
        let Log {
            entries,
            views,
            hot,
            ..
        } = self;
        let ledger = views.entry(window.clone()).or_default();
        let pos = match above {
            None => {
                let pos = ledger.blocks.len();
                ledger.blocks.push(at);
                ledger.index.push(rows);
                pos
            }
            Some(pos) => {
                let pos = pos.min(ledger.blocks.len());
                ledger.blocks.insert(pos, at);
                // A sum inserted above the others moves every one of them.
                let cap = ledger.cap;
                let mut index = RowIndex::default();
                for &block in &ledger.blocks {
                    let entry = &entries[block];
                    index.push(entry.visible(room(entry.kind, cap, block_size)));
                }
                ledger.index = index;
                for (place, &block) in ledger.blocks.iter().enumerate().skip(pos + 1) {
                    entries[block].at = place;
                }
                pos
            }
        };
        entries[at].at = pos;
        hot.push(at);
        let id = entries[at].id;
        if let Some(floor) = opens {
            ledger.page_first = Some(id);
            // A journal that named the page's oldest row says so: a live fill learns
            // it from its answer instead.
            if let Some(seq) = floor {
                ledger.pages.push((id, seq));
            }
        }
        if let Some((name, next)) = self.prepend.as_mut()
            && window.as_ref() == name.as_str()
        {
            *next = pos + 1;
        }
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

    /// Drops every block of one window, keeping the window and its journal.
    ///
    /// # Invariant
    ///
    /// The view alone: SPEC's command row shows the last `cmd` or `cmd_result`, so
    /// the previous one leaves the screen, and it stays in the file -- a `Clear`
    /// record says where the view was cut. See `docs/notes/tui-cache.md`.
    pub fn clear(&mut self, window: &str) {
        // The view forgets it; the journal keeps it. See `docs/notes/tui-cache.md`.
        self.cache.clear(window);
        // The next beginning is whatever arrives from here, not what was read.
        self.seeded.remove(window);
        self.forget_blocks(window);
    }

    /// Drops every block this window holds, in memory only.
    ///
    /// # Invariant
    ///
    /// The journal is not touched: what the window drew is a record, and dropping a
    /// block from the view is not a reason to make it unreadable.
    fn forget_blocks(&mut self, window: &str) {
        if !self
            .entries
            .iter()
            .any(|entry| entry.window.as_ref() == window)
        {
            return;
        }
        let mut moved = Vec::with_capacity(self.entries.len());
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            let mine = entry.window.as_ref() != window;
            moved.push(mine.then_some(kept.len()));
            if mine {
                kept.push(entry);
            }
        }
        self.entries = kept;
        // The open map points at indexes that have moved: re-derive it from the
        // blocks that are still streaming.
        self.open = self
            .open
            .drain()
            .filter_map(|(key, at)| moved.get(at).copied().flatten().map(|at| (key, at)))
            .collect();
        self.reindex();
    }

    /// Replaces the whole text of the block `key` names and re-wraps it.
    ///
    /// # Invariant
    ///
    /// For a block whose text is *derived* from what arrived -- an answer drawn as
    /// markdown, where one more delta can change a row above the last one -- so an
    /// append would be wrong and a rewrite is the only honest thing. Its window's
    /// row sum moves by the difference in rows, as it does for a fold.
    pub fn rewrite(&mut self, key: &str, text: &str) {
        let Some(at) = self.entry_by_key(key) else {
            return;
        };
        self.touch(at);
        let before = self.contributions(at);
        let width = self.width_of(&self.entries[at].window);
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
        self.entries[at].reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        self.record_seal(at);
    }

    /// Whether a block under this key exists already.
    pub fn has_block(&self, key: &str) -> bool {
        self.entries.iter().any(|entry| entry.key == key)
    }

    /// Scrolls the box of an opened item `at`, positive into its past.
    ///
    /// # Invariant
    ///
    /// Answers whether the box moved. A box with nothing left in the wheel's
    /// direction -- at either end, or under a `block_size` of 0, which caps nothing
    /// and so leaves nothing inside to scroll -- moves nothing. No other number is
    /// consulted, so one cell behaves the same way whatever the cap is.
    pub fn scroll_block(&mut self, at: usize, rows: i64) -> bool {
        // The row offsets of the frame the pointer landed on: reflowing before
        // the next draw would move the hit.
        let Some(entry) = self.entries.get(at) else {
            return false;
        };
        if !entry.boxed() {
            return false;
        }
        let cap = self.room(entry.kind, 0);
        let bound = entry.travel(cap);
        // The stored row is the first one shown, so a notch into the past lowers it.
        let current = entry.inner;
        let next = (current as i64 - rows).clamp(0, bound as i64) as usize;
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
        self.fold(at)
    }

    /// Opens or closes the `abstract` the block under `key` is.
    ///
    /// # Invariant
    ///
    /// The block `entry_by_key` answers -- the live one, else the newest -- so a
    /// script addressing an item it never clicked flips the same block a lookup
    /// names. See `Log::fold`.
    pub fn toggle(&mut self, key: &str) -> bool {
        match self.entry_by_key(key) {
            Some(at) => self.fold(at),
            None => false,
        }
    }

    /// Flips the fold of block `at`, answering whether there was one to flip.
    ///
    /// # Invariant
    ///
    /// An item that opens is read from its first row, and one that closes forgets
    /// where it was, so opening it again is the same gesture however it was left.
    /// The row count changes, so this is a reflow of one block and a shift of its
    /// window's sum -- the arithmetic a seal does, for the same reason. A block that
    /// does not fold is nothing to toggle.
    fn fold(&mut self, at: usize) -> bool {
        if at >= self.entries.len() || !self.entries[at].kind.folds() {
            return false;
        }
        self.touch(at);
        let before = self.contributions(at);
        self.entries[at].expanded = !self.entries[at].expanded;
        // Read after the flip: whether it is a box is what its own width depends on.
        let width = self.width_of(&self.entries[at].window);
        let entry = &mut self.entries[at];
        entry.reflow(width);
        // The beginning is 0, so this is also what a restored block reads as.
        entry.inner = 0;
        let after = self.contributions(at);
        self.shift(at, before, after);
        self.record_seal(at);
        true
    }

    /// The block under one row of a window, as the key a click carries.
    ///
    /// # Invariant
    ///
    /// It answers with the block *shown* at that cell, counting the fold: a click
    /// on a summary line is a click on the item it summarises, and a click in a
    /// box's margin is the pane's. See `docs/notes/tui-log.md`.
    pub fn block_at(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
        column: usize,
    ) -> Option<usize> {
        self.hit(window, width, height, scroll, row, column)
            .map(|hit| hit.block)
    }

    /// The opened block whose own content holds this cell: what a wheel there scrolls.
    ///
    /// # Invariant
    ///
    /// `Some` exactly when the cell is inside a box, because that is the only thing
    /// a wheel can move. A box's rules are its edge and not content, a box's margin
    /// is the window's, and a row of any other block has no scroll of its own -- so
    /// all three answer `None`, and the wheel there is the pane's.
    pub fn box_at(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
        column: usize,
    ) -> Option<usize> {
        let hit = self.hit(window, width, height, scroll, row, column)?;
        if !hit.boxed {
            return None;
        }
        let rule = hit.index == 0 || hit.index + 1 == hit.shown;
        match rule {
            true => None,
            false => Some(hit.block),
        }
    }

    /// The block under one row, with where that row sits inside it.
    ///
    /// # Invariant
    ///
    /// `shown` is the rows the block occupies in this window, box included, so the
    /// rows that carry the box's rules are its first and its last. A cell in a box's
    /// margin is no block at all: that column is the pane's, which is how an operator
    /// reaches the window while a box covers it. See `docs/notes/tui-log.md`.
    fn hit(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
        column: usize,
    ) -> Option<Hit> {
        self.lay_out(window, width);
        let ledger = self.ledger(window)?;
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
        if boxed {
            let margin = box_margin(width, self.edge);
            if column < margin || column + margin >= width {
                return None;
            }
        }
        Some(Hit {
            block,
            index: want - ledger.index.prefix(pos),
            shown: entry.visible(self.room(entry.kind, ledger.cap)),
            boxed,
        })
    }

    /// Replaces a call's provisional body with its structured name and fields.
    ///
    /// # Invariant
    ///
    /// The fields are the body: what the block streamed before this -- the
    /// arguments spelled out -- is dropped rather than kept beside them, because a
    /// described call is drawn either folded (its label) or opened (its fields).
    /// Holding both was the same arguments twice. The buffer goes with the text:
    /// clearing a `String` keeps the allocation that text needed.
    /// See `docs/notes/tui-log.md`.
    pub fn describe(&mut self, name: &str, key: &str, head: &client::protocol::Head) {
        let at = match self.entry_by_key(key) {
            Some(at) => at,
            None => {
                self.stream(name, key, crate::LineKind::ToolCall, &head.name, "", true);
                let Some(at) = self.entry_by_key(key) else {
                    return;
                };
                at
            }
        };
        self.touch(at);
        let before = self.contributions(at);
        let width = {
            let entry = &mut self.entries[at];
            entry.text = String::new();
            entry.kind = crate::LineKind::ToolCall;
            entry.call = Some(head.clone());
            // A call's structure is complete independently of its result stream.
            entry.sealed = true;
            self.width_of(&self.entries[at].window)
        };
        self.entries[at].reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        self.open.remove(key);
        self.record_seal(at);
    }

    /// Replaces the label of the block `key` names, once its outcome is known.
    ///
    /// # Invariant
    ///
    /// The row count can change: the label is not text, so a longer one eats the
    /// first row's columns and wraps it further. The block is reflowed and its
    /// window's sum corrected, which is what keeps a retro-set label from silently
    /// shifting every block after it. A key nobody opened is nothing to label.
    pub fn relabel(&mut self, key: &str, tag: &str, kind: LineKind) {
        // By key, not only through `open`: a verdict arrives on the chunk that
        // ends the call, and the block is sealed by then.
        let Some(at) = self.entry_by_key(key) else {
            return;
        };
        self.touch(at);
        if self.entries[at].tag == tag && self.entries[at].kind == kind {
            return;
        }
        let before = self.contributions(at);
        self.entries[at].tag = tag.to_string();
        self.entries[at].kind = kind;
        let width = self.width_of(&self.entries[at].window);
        self.entries[at].reflow(width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        self.record_seal(at);
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
                self.touch(at);
                let before = self.contributions(at);
                let width = self.width_of(&self.entries[at].window);
                self.entries[at].append(text, width);
                let after = self.contributions(at);
                self.shift(at, before, after);
                // Held in memory until the seal writes it: one record per block,
                // not one per chunk. See `docs/notes/tui-cache.md`.
                self.entries[at].pending = true;
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
                let width = self.width_of(name);
                let id = self.next_id();
                let mut entry = Entry::with_tag(id, name, kind, tag, text.to_string(), width);
                // A body-less item's body is its label: a call reads as its
                // arguments, and opening it must show them.
                entry.key = key.to_string();
                self.entries.push(entry);
                let at = self.entries.len() - 1;
                self.attach(at);
                self.open.insert(key.to_string(), at);
                self.entries[at].pending = true;
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
        self.seal_quiet(at);
        self.record_seal(at);
    }

    /// Sealing without the journal: what a replay does, having read the record.
    fn seal_quiet(&mut self, at: usize) {
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
        let width = self.width_of(&self.entries[at].window);
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
        self.touch(at);
        let width = self.width_of(&self.entries[at].window);
        let before = self.contributions(at);
        self.entries[at].append(text, width);
        let after = self.contributions(at);
        self.shift(at, before, after);
        // The journal hears about this block when it ends, not once per chunk.
        self.entries[at].pending = true;
    }

    /// The ledger a window reads, or `None` for a name nobody has written to yet.
    fn ledger(&self, window: &str) -> Option<&Ledger> {
        self.views.get(window)
    }

    /// How many rows this window holds at `width`, laying it out first if needed.
    pub fn rows(&mut self, window: &str, width: usize) -> usize {
        self.lay_out(window, width);
        self.ledger(window)
            .map(|ledger| ledger.index.total())
            .unwrap_or(0)
    }

    /// The rows for one frame of one window: the last `height` rows, in order.
    ///
    /// # Invariant
    ///
    /// The window is the tail of its own ledger, and a block contributes only the
    /// rows its cap allows: no block is reserved space, and none is cut out mid-pass.
    pub fn window(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
    ) -> Viewport<'_> {
        self.lay_out(window, width);
        let Log {
            entries,
            views,
            cache,
            block_size,
            ..
        } = self;
        let Some(ledger) = views.get(window) else {
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

        // Found first, drawn second: reading a body back borrows mutably, and
        // the rows are borrowed from the same place.
        let mut shown_blocks: Vec<ShownBlock> = Vec::new();
        if start < end && !ledger.blocks.is_empty() {
            // Jump to the block holding the first visible row: O(log blocks).
            let mut pos = ledger.index.line_at(start).min(ledger.blocks.len() - 1);
            let mut first = ledger.index.prefix(pos);
            while let Some(&block) = ledger.blocks.get(pos) {
                let Some(entry) = entries.get(block) else {
                    break;
                };
                let cap = room(entry.kind, ledger.cap, *block_size);
                let body = entry.rows().min(cap);
                let boxed = entry.boxed();
                // A box keeps the row the operator left it on; any other capped
                // block keeps its newest rows.
                let head = match boxed {
                    true => entry.inside(cap),
                    false => entry.rows().saturating_sub(body),
                };
                let shown = entry.visible(cap);
                let next = first + shown;
                let top = start.max(first);
                let bottom = end.min(next);
                // The label rides on the block's own first row, or on the first row
                // its cap left it -- never mid-paragraph.
                let labelled = head > 0 || top == first;
                shown_blocks.push(ShownBlock {
                    block,
                    first,
                    top,
                    bottom,
                    head,
                    shown,
                    boxed,
                    labelled,
                });
                if bottom >= end || pos + 1 >= ledger.blocks.len() {
                    break;
                }
                first = next;
                pos += 1;
            }
        }
        // Rows on screen are the one thing a frame may pay a disk read for.
        for block in &shown_blocks {
            materialize(entries, cache, block.block, ledger.width);
        }
        let mut rows: Vec<Row<'_>> = Vec::with_capacity(end - start);
        for spot in &shown_blocks {
            let Some(entry) = entries.get(spot.block) else {
                continue;
            };
            for row in spot.top..spot.bottom {
                let index = row - spot.first;
                // SPEC's opened item: a box whose top row carries the label.
                if spot.boxed && index == 0 {
                    rows.push((entry.kind, Shown::Text(entry.label()), Rule::Top));
                    continue;
                }
                if spot.boxed && index + 1 == spot.shown {
                    rows.push((entry.kind, Shown::Text(Cow::Borrowed("")), Rule::Bottom));
                    continue;
                }
                let body_row = match spot.boxed {
                    true => index - 1 + spot.head,
                    false => index + spot.head,
                };
                // The label is on the box's top row, so no body row repeats it.
                let print = spot.labelled && !spot.boxed && index == 0;
                let rule = match spot.boxed {
                    true => Rule::Inside,
                    false => Rule::Free,
                };
                rows.push((entry.kind, entry.shown(body_row, print), rule));
            }
        }
        Viewport { rows, max_scroll }
    }

    /// Fills a window from its journal, from the newest page boundary.
    ///
    /// # Invariant
    ///
    /// The question is whether *this process* has decided where the window starts,
    /// not whether the window has blocks: a restart receives a realtime line before
    /// its first frame, and that line must not hide the journal behind it. A name
    /// decided once is never seeded again. See `docs/notes/tui-cache.md`.
    ///
    /// # Invariant
    ///
    /// One page, from the boundary that page begins at: the rows are the rows that
    /// boundary names, which is why [`Log::floor_of`] answers before the first frame.
    /// A page is as many rows as the pane that asked for it, so one is always enough.
    /// A journal written before boundaries has none, and claims no floor at all.
    pub fn seed(&mut self, window: &str, width: usize, rows: usize) {
        if !self.seeded.insert(window.to_string()) {
            return;
        }
        let end = self.cache.end(window);
        if end == 0 {
            return;
        }
        // The newest page boundary. A journal with none is read as far back as it can.
        let Some((boundary, _)) = self.cache.floor(window, end) else {
            let mut start = end;
            loop {
                start = self.cache.back(window, start, WALK);
                self.replay(window, start);
                self.lay_out(window, width);
                if start == 0 || self.rows(window, width) >= rows {
                    break;
                }
            }
            return;
        };
        self.replay(window, boundary);
        self.lay_out(window, width);
    }

    /// Rebuilds one window from its journal, from `start` to the end.
    ///
    /// # Invariant
    ///
    /// What this process already watched arrive is kept, and placed after what the
    /// journal holds: a window that showed a line before it was seeded still has
    /// that line, and it is newer than anything in the file.
    fn replay(&mut self, window: &str, start: u64) {
        let live = self.take_window(window);
        self.drop_window(window);
        let mut ids: HashMap<u64, usize> = HashMap::new();
        // Where a `Rewind` here cuts: the newest stamp at or before the first record.
        let mut cut = self.cache.stamp_at(window, start).map(|(at, _, _)| at);
        for (at, record) in self.cache.forward(window, start) {
            self.apply(window, at, &record, &mut ids, &mut cut);
        }
        // No stream survives a restart: the process writing it is gone.
        let open: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.window.as_ref() == window && !entry.sealed)
            .map(|(at, _)| at)
            .collect();
        for at in open {
            self.seal_quiet(at);
        }
        for entry in live {
            self.restore(entry, &mut ids);
        }
    }

    /// Takes a window's blocks out of memory and answers them, in arrival order.
    ///
    /// # Invariant
    ///
    /// The indexes the removal invalidates are repaired here: an `open` entry whose
    /// block left with the window leaves too, and the rest name their new places.
    /// `drop_window` may then run over a window that holds no blocks.
    fn take_window(&mut self, window: &str) -> Vec<Entry> {
        let mut moved = Vec::with_capacity(self.entries.len());
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut taken = Vec::new();
        for entry in std::mem::take(&mut self.entries) {
            let mine = entry.window.as_ref() == window;
            moved.push((!mine).then_some(kept.len()));
            if mine {
                taken.push(entry);
            } else {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.open = self
            .open
            .drain()
            .filter_map(|(key, at)| moved.get(at).copied().flatten().map(|at| (key, at)))
            .collect();
        self.reindex();
        taken
    }

    /// Puts a block this process watched arrive back, after a replay.
    ///
    /// # Invariant
    ///
    /// A block the journal also holds -- the same id -- is replaced by the live one:
    /// what arrived here is newer than what was written there. A stream still open
    /// takes its key back, or the next chunk would open a second block.
    fn restore(&mut self, entry: Entry, ids: &mut HashMap<u64, usize>) {
        let key = entry.key.clone();
        let streaming = !entry.sealed && !key.is_empty();
        let place = match ids.get(&entry.id).copied() {
            Some(place) => {
                let before = self.contributions(place);
                let at = self.entries[place].at;
                let mut entry = entry;
                entry.at = at;
                self.entries[place] = entry;
                let after = self.contributions(place);
                self.shift(place, before, after);
                place
            }
            None => {
                let id = entry.id;
                let place = self.adopt(entry);
                ids.insert(id, place);
                place
            }
        };
        if streaming {
            self.open.insert(key, place);
        }
    }

    /// Applies one journal record, as if the call that wrote it had just happened.
    fn apply(
        &mut self,
        window: &str,
        at: u64,
        record: &Record,
        ids: &mut HashMap<u64, usize>,
        cut: &mut Option<u64>,
    ) {
        let width = self.width_of(window);
        match record {
            Record::Line { id, kind, text } => {
                let mut entry = Entry::new(*id, window, *kind, text.clone(), width);
                entry.sealed = true;
                entry.slot = Some(at);
                let place = self.adopt(entry);
                ids.insert(*id, place);
            }
            Record::Delta {
                id,
                kind,
                tag,
                text,
                ..
            } => match ids.get(id).copied() {
                Some(place) => self.append_quiet(place, text),
                None => {
                    let entry = Entry::with_tag(*id, window, *kind, tag, text.clone(), width);
                    let place = self.adopt(entry);
                    ids.insert(*id, place);
                }
            },
            // Not a block: it says where a later `Rewind` cuts.
            Record::Stamp { .. } => *cut = Some(at),
            // Not a block either: what the window said about itself, which no frame
            // drew as a row, so reading it back draws nothing.
            Record::Note { .. } => {}
            // Where the view was cut: whatever this pass adopted is above the cut.
            Record::Clear => {
                self.forget_blocks(window);
                ids.clear();
            }
            // Not a block either: it opens a page, and the block after it is the one
            // the page begins with.
            Record::Floor { seq } => {
                self.filling.insert(window.to_string(), Some(*seq));
            }
            // Thrown away again on the way forward: what is above it is the refill.
            Record::Rewind => {
                let window = window.to_string();
                self.cut_tail(&window, *cut);
            }
            Record::Seal {
                id,
                kind,
                tag,
                key,
                text,
                expanded,
                call,
            } => {
                let mut entry = Entry::with_tag(*id, window, *kind, tag, text.clone(), width);
                entry.key = key.clone();
                entry.expanded = *expanded;
                entry.call = call.clone();
                entry.sealed = true;
                entry.slot = Some(at);
                entry.reflow(width);
                match ids.get(id).copied() {
                    Some(place) => {
                        let before = self.contributions(place);
                        entry.at = self.entries[place].at;
                        self.entries[place] = entry;
                        let after = self.contributions(place);
                        self.shift(place, before, after);
                    }
                    None => {
                        let place = self.adopt(entry);
                        ids.insert(*id, place);
                    }
                }
            }
        }
    }

    /// Adds a block read back from the journal, and says where it landed.
    fn adopt(&mut self, mut entry: Entry) -> usize {
        entry.at = 0;
        self.next_id = self.next_id.max(entry.id.saturating_add(1));
        self.entries.push(entry);
        let at = self.entries.len() - 1;
        self.attach(at);
        at
    }

    /// Appends to a block during a replay: no journal, because it came from one.
    fn append_quiet(&mut self, at: usize, text: &str) {
        let width = self.width_of(&self.entries[at].window);
        let before = self.contributions(at);
        self.entries[at].append(text, width);
        let after = self.contributions(at);
        self.shift(at, before, after);
    }

    /// Drops every body a pane is not showing, keeping their rows.
    ///
    /// # Invariant
    ///
    /// Only bodies the journal can stand for alone, and only outside the rows the
    /// pane draws: what is on screen is what memory is for. Nothing is lost -- the
    /// file holds it -- so this is why a session can run for days.
    pub fn evict(&mut self, window: &str, keep: std::ops::Range<usize>) {
        let block_size = self.block_size;
        let mut drops: Vec<usize> = Vec::new();
        if let Some(ledger) = self.views.get(window) {
            let cap = ledger.cap;
            for &at in &self.hot {
                let Some(entry) = self.entries.get(at) else {
                    continue;
                };
                if !entry.hot() || entry.window.as_ref() != window {
                    continue;
                }
                let first = ledger.index.prefix(entry.at);
                let last = first + entry.visible(room(entry.kind, cap, block_size));
                if last <= keep.start || first >= keep.end {
                    drops.push(at);
                }
            }
        }
        for at in drops {
            self.entries[at].drop_body();
        }
        let Log { hot, entries, .. } = self;
        hot.retain(|at| entries.get(*at).is_some_and(Entry::hot));
    }

    /// Every window this log holds blocks for.
    pub fn windows(&self) -> Vec<String> {
        self.views.keys().map(|name| name.to_string()).collect()
    }

    /// Re-numbers the blocks and re-derives every window's row sum.
    ///
    /// # Invariant
    ///
    /// A ledger names its blocks by their position in `entries`, so anything that
    /// takes a block away -- `clear`, `forget` -- makes every name stale and every
    /// block lands again. Rows, not layouts: a block keeps the rows it was laid out
    /// with, because nothing here moves a width. See `docs/notes/tui-log.md`.
    fn reindex(&mut self) {
        for ledger in self.views.values_mut() {
            ledger.blocks.clear();
            ledger.index = RowIndex::default();
        }
        self.hot.clear();
        for at in 0..self.entries.len() {
            self.attach(at);
        }
    }

    /// Re-derives one window's row sums, laying no block out again.
    fn reindex_window(&mut self, window: &str) {
        let block_size = self.block_size;
        let entries = &self.entries;
        let Some(ledger) = self.views.get_mut(window) else {
            return;
        };
        let cap = ledger.cap;
        let mut index = RowIndex::default();
        for &at in &ledger.blocks {
            let entry = &entries[at];
            index.push(entry.visible(room(entry.kind, cap, block_size)));
        }
        ledger.index = index;
    }

    /// Lays this window's blocks out at `width`, and no other window's.
    ///
    /// # Invariant
    ///
    /// A block belongs to exactly one window, so this is the only place a width
    /// costs a layout -- and a width is a shape change, not something a frame does.
    /// Two panes of different widths therefore never pay for each other's blocks:
    /// which is what a single width for the whole log could not do, at the price of
    /// laying the log out twice per frame. See `docs/notes/tui-log.md`.
    fn lay_out(&mut self, window: &str, width: usize) {
        let width = width.max(1);
        let blocks = {
            let Some(ledger) = self.views.get_mut(window) else {
                return;
            };
            if ledger.width == width {
                return;
            }
            ledger.width = width;
            // Taken out so the blocks can be laid out while the ledger is written.
            std::mem::take(&mut ledger.blocks)
        };
        for at in blocks.iter().copied() {
            if self.entries[at].hot() {
                self.entries[at].reflow(width);
                continue;
            }
            // Read back, counted at this width, and dropped again: the index
            // needs the rows, and only a resize asks for them.
            materialize(&mut self.entries, &self.cache, at, width);
            self.entries[at].drop_body();
        }
        if let Some(ledger) = self.views.get_mut(window) {
            ledger.blocks = blocks;
        }
        self.reindex_window(window);
    }
}

/// One block a window is about to draw, and which of its rows.
struct ShownBlock {
    block: usize,
    /// The block's first row in the view, and the rows of it the view shows.
    first: usize,
    top: usize,
    bottom: usize,
    /// Rows of the block the view does not show, and how many it does.
    head: usize,
    shown: usize,
    boxed: bool,
    labelled: bool,
}

/// Reads one block's body back from the journal, where it was left.
///
/// # Invariant
///
/// It answers with the block as the journal last saw it, at the width the pane is
/// drawing: a body is a function of the text and that width, so what comes back has
/// the rows the index was already counting. A record that cannot be read leaves the
/// block as it is rather than inventing an empty one. See `docs/notes/tui-cache.md`.
fn materialize(entries: &mut [Entry], cache: &Cache, at: usize, width: usize) {
    let Some(entry) = entries.get(at) else {
        return;
    };
    if entry.hot() {
        return;
    }
    let Some(slot) = entry.slot else {
        return;
    };
    let window = entry.window.clone();
    let place = entry.at;
    let Some(mut fresh) = cache
        .read(&window, slot)
        .and_then(|record| whole(record, &window, width))
    else {
        return;
    };
    fresh.slot = Some(slot);
    fresh.at = place;
    entries[at] = fresh;
}

/// The block a record stands for on its own, at the width it is drawn at.
///
/// # Invariant
///
/// A `Line` and a `Seal` both stand for a block alone -- a line was pushed whole,
/// and a seal is a block's whole current form -- so either one restores a body an
/// eviction dropped. A `Delta` is the tail of a block and stands for nothing: it
/// answers `None` rather than a block missing its head. See
/// `docs/notes/tui-cache.md`.
fn whole(record: Record, window: &str, width: usize) -> Option<Entry> {
    let (id, kind, tag, key, text, expanded, call) = match record {
        Record::Seal {
            id,
            kind,
            tag,
            key,
            text,
            expanded,
            call,
        } => (id, kind, tag, key, text, expanded, call),
        Record::Line { id, kind, text } => {
            (id, kind, String::new(), String::new(), text, false, None)
        }
        // Neither is a block: one says where the transcript stands, the other
        // says what to throw away.
        Record::Delta { .. }
        | Record::Stamp { .. }
        | Record::Floor { .. }
        | Record::Rewind
        | Record::Note { .. }
        | Record::Clear => {
            return None;
        }
    };
    let mut entry = Entry::with_tag(id, window, kind, &tag, text, width);
    entry.key = key;
    entry.expanded = expanded;
    entry.call = call;
    // A line arrives whole and a seal ends a stream: both are scrollback.
    entry.sealed = true;
    entry.reflow(width);
    Some(entry)
}
