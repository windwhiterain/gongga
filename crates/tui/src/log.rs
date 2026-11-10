//! The scrollback: row offsets cached per line, so a frame never walks the log.
//!
//! # Invariant
//!
//! A frame touches only the visible rows, and an append lands on exactly the
//! rows a reflow of the whole line would. See `docs/notes/tui-log.md`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::LineKind;
use crate::markdown;
use client::protocol;
use client::window_index::{Ledger, RowIndex};

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
}

impl Entry {
    fn new(window: &str, kind: LineKind, text: String, width: usize) -> Self {
        Self::with_tag(window, kind, "", text, width)
    }

    /// A block that streams under `tag`: the label is not text, so no cut can take it.
    fn with_tag(window: &str, kind: LineKind, tag: &str, text: String, width: usize) -> Self {
        let mut entry = Self {
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
            call: None,
            sections: String::new(),
            styled: Vec::new(),
            at: 0,
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
                true if call.name == "exec" => "args",
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
}

impl Default for Log {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            views: HashMap::new(),
            open: HashMap::new(),
            block_size: 0,
            edge: protocol::DEFAULT_BOX_EDGE,
        }
    }
}

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
        self.drop_window(window);
    }

    /// Takes a window's blocks and its ledger out of memory.
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
        let width = self.width_of(name);
        let mut entry = Entry::new(name, kind, text.into(), width);
        // A whole row arrives at once: nothing will stream into it, so a folding
        // kind is an `abstract` from its first frame.
        entry.sealed = true;
        self.entries.push(entry);
        self.attach(self.entries.len() - 1);
    }

    /// Records block `at` in its own window's ledger.
    ///
    /// # Invariant
    ///
    /// The ledger's order is the order the pane draws: a live block joins at the
    /// end, and every block keeps the position it is summed at.
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
        let ledger = self.views.entry(window).or_default();
        let pos = ledger.blocks.len();
        ledger.blocks.push(at);
        ledger.index.push(rows);
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
    /// The view alone: SPEC's command row shows the last `cmd` or `cmd_result`, so
    /// the previous one leaves the screen.
    pub fn clear(&mut self, window: &str) {
        self.forget_blocks(window);
    }

    /// Drops every block this window holds, in memory only.
    ///
    /// # Invariant
    ///
    /// The indexes the removal invalidates are repaired here: an `open` entry whose
    /// block left with the window leaves too.
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
    pub fn scroll_block(&mut self, at: usize, rows: i64, body: usize) -> bool {
        // The row offsets of the frame the pointer landed on: reflowing before
        // the next draw would move the hit.
        let Some(entry) = self.entries.get(at) else {
            return false;
        };
        if !entry.boxed() {
            return false;
        }
        // The rows `box_at` measured for this box: the only bound this gesture
        // may move within, so a notch never walks past what the frame draws.
        let bound = entry.travel(body);
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
    ) -> Option<(usize, usize)> {
        let hit = self.hit(window, width, height, scroll, row, column)?;
        if !hit.boxed {
            return None;
        }
        let rule = hit.index == 0 || hit.index + 1 == hit.shown;
        if rule {
            return None;
        }
        // A body that already fits is no scroll target: the wheel goes to the
        // window under it, every time. What fits is the rows this frame drew for
        // it, rule included -- never the rows a block may keep. A block drawn in
        // full would otherwise take the wheel and scroll its own past under a body
        // the operator can already see whole, which is how a turn made hours ago
        // surfaces from a notch that should have moved one row.
        let entry = self.entries.get(hit.block)?;
        let body = hit.shown.saturating_sub(2).max(1);
        (entry.rows() > body).then_some((hit.block, body))
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
                let before = self.contributions(at);
                let width = self.width_of(&self.entries[at].window);
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
                let width = self.width_of(name);
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
        self.seal_quiet(at);
    }

    /// Seals a block's rows without touching a journal.
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
        let width = self.width_of(&self.entries[at].window);
        let before = self.contributions(at);
        self.entries[at].append(text, width);
        let after = self.contributions(at);
        self.shift(at, before, after);
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

        // Found first, drawn second: the rows borrow the same entries.
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
            self.entries[at].reflow(width);
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
