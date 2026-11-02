//! The views the events area is divided into, over one scrollback.
//!
//! # Invariant
//!
//! A line names the window it belongs to, and every view reads the same blocks: the
//! layout says what a frame shows, never what the client holds. See
//! `docs/notes/tui-panes.md`.

use crate::log::{Log, Viewport};
use crate::{Layout, LineKind};

/// One view of the events area: what it shows and where it is scrolled.
pub struct Pane {
    pub name: String,
    /// What kind of view this is: SPEC's three parts of the window.
    pub kind: Kind,
    /// Rows scrolled up from this pane's newest row: zero is the bottom.
    pub scroll: usize,
    /// The scroll bound, written back by the renderer.
    pub max_scroll: usize,
    /// The height and the row count the last frame drew, so the next one can hold
    /// the rows that frame was reading.
    last_height: usize,
    last_rows: usize,
}

/// Which part of the window a pane is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The session list, which draws names rather than events.
    List,
    /// A session drawn as a header and its own events.
    Shown,
    /// The single line `cmd` and its result share.
    Command,
}

impl Kind {
    /// Whether this pane reads events at all: the list draws its own rows.
    pub fn is_events(self) -> bool {
        matches!(self, Kind::Shown | Kind::Command)
    }

    /// Whether the wheel may scroll it: SPEC gives the list and a shown session a
    /// vertical scroll, and the command line none.
    pub fn scrolls(self) -> bool {
        matches!(self, Kind::List | Kind::Shown)
    }
}

impl Pane {
    fn of(name: &str, kind: Kind) -> Self {
        Self {
            name: name.to_string(),
            kind,
            scroll: 0,
            max_scroll: 0,
            last_height: 0,
            last_rows: 0,
        }
    }

    /// Keeps the rows on screen where they are when the window or the log changes.
    ///
    /// # Invariant
    ///
    /// A view at its newest row stays there: growing it is what shows more history.
    /// A view scrolled away from the bottom keeps its *top* row, so a taller window
    /// or a line that arrived adds rows underneath what the operator was reading
    /// instead of sliding the paragraph away. See `docs/notes/tui-panes.md`.
    pub fn anchor(&mut self, height: usize, rows: usize) {
        if self.scroll > 0 && self.last_height != 0 {
            // start = rows - scroll - height, held across the frame.
            let start = self
                .last_rows
                .saturating_sub(self.scroll)
                .saturating_sub(self.last_height);
            self.scroll = rows.saturating_sub(height).saturating_sub(start);
        }
        self.last_height = height;
        self.last_rows = rows;
    }

    /// Moves this view `rows` rows, positive into its past, within its bound.
    ///
    /// # Invariant
    ///
    /// The bound is the renderer's: only a frame knows how tall this view is drawn,
    /// so a scroll between frames stops where the last one ended. See
    /// `docs/notes/tui-panes.md`.
    pub fn scroll_by(&mut self, rows: i64) {
        self.scroll = (self.scroll as i64 + rows).clamp(0, self.max_scroll as i64) as usize;
    }

    /// Writes the bound this view is drawn within, keeping the offset inside it.
    ///
    /// # Invariant
    ///
    /// Called by the frame that draws it, because only a frame knows how tall the
    /// view is: the wheel between frames stops at this bound.
    pub fn bound(&mut self, max_scroll: usize) {
        self.max_scroll = max_scroll;
        self.scroll = self.scroll.min(max_scroll);
    }
}

/// One scrollback, and the panes that show it.
#[derive(Default)]
pub struct Panes {
    log: Log,
    panes: Vec<Pane>,
    /// The session list, which is drawn from the command layer's announcement
    /// rather than from the log.
    listed: Vec<crate::Listed>,
    /// The names that announcement puts in the list: SPEC's session list.
    list_names: Vec<String>,
    /// SPEC's `active_quota` row, as the command layer spelled it.
    quota: Option<crate::QuotaRow>,
    /// The windows this frame drew, so the rest can let their bodies go.
    drawn: Vec<String>,
}

impl Panes {
    pub fn new() -> Self {
        Self {
            log: Log::new(),
            panes: Vec::new(),
            listed: Vec::new(),
            list_names: Vec::new(),
            quota: None,
            drawn: Vec::new(),
        }
    }

    /// The views over a log whose blocks are written down for one endpoint.
    pub fn with_cache(root: &std::path::Path, endpoint: &str) -> Self {
        Self {
            log: Log::with_cache(root, endpoint),
            panes: Vec::new(),
            listed: Vec::new(),
            list_names: Vec::new(),
            quota: None,
            drawn: Vec::new(),
        }
    }

    /// Points the scrollback at another client root or endpoint, keeping nothing.
    pub fn repoint(&mut self, root: &std::path::Path, endpoint: &str) {
        self.log.repoint(root, endpoint);
    }

    /// Fills an empty window from its journal, until it covers `rows`.
    pub fn seed(&mut self, window: &str, width: usize, rows: usize) {
        self.log.seed(window, width, rows);
    }

    /// Drops the bodies this pane is not showing.
    pub fn evict(&mut self, window: &str, keep: std::ops::Range<usize>) {
        self.log.evict(window, keep);
    }

    /// Says this window's rows were drawn this frame.
    pub fn mark_drawn(&mut self, window: &str) {
        if !self.drawn.iter().any(|name| name == window) {
            self.drawn.push(window.to_string());
        }
    }

    /// Starts a frame: nothing has been drawn yet.
    pub fn begin_frame(&mut self) {
        self.drawn.clear();
    }

    /// Ends a frame: a window nobody drew holds no bodies.
    ///
    /// # Invariant
    ///
    /// What a frame does not draw is what memory stops paying for -- a hidden
    /// session's blocks and the rows above and below every pane.
    /// See `docs/notes/tui-cache.md`.
    pub fn end_frame(&mut self) {
        for window in self.log.windows() {
            if !self.drawn.iter().any(|name| *name == window) {
                self.log.evict(&window, 0..0);
            }
        }
    }

    /// The session list, as the command layer announced it.
    pub fn listed(&self) -> &[crate::Listed] {
        &self.listed
    }

    /// What a frame's chrome is made of, as [`crate::render::areas`] needs it.
    ///
    /// # Invariant
    ///
    /// `command_rows` is how many rows the command area's content needs at `width`,
    /// asked of the log: SPEC gives that area no `(line)`, so its height is its
    /// content's, and only the log knows what that is. The layout clamps it.
    pub fn chrome(&mut self, width: usize) -> crate::render::Chrome {
        let command = self.command().map(|pane| pane.name.clone());
        let command_rows = match &command {
            // Read back before it is measured: this is the area's height.
            Some(name) => {
                self.log.seed(name, width, 1);
                self.log.rows(name, width).max(1) as u16
            }
            None => 0,
        };
        crate::render::Chrome {
            command: command.is_some(),
            command_rows,
            quota: self.quota.is_some(),
        }
    }

    /// SPEC's `active_quota` row, when the last step's reading arrived.
    pub fn quota(&self) -> Option<&crate::QuotaRow> {
        self.quota.as_ref()
    }

    /// The names the session list draws, in its order.
    ///
    /// # Invariant
    ///
    /// Every session the server knows, shown or not: the list is where a name is
    /// clicked, so a name missing from it is a session nobody can show again.
    pub fn list_names(&self) -> &[String] {
        &self.list_names
    }

    /// What the announcement says this session is doing, empty when it is not in it.
    pub fn status_of(&self, name: &str) -> &str {
        self.listed
            .iter()
            .find(|listed| listed.name == name)
            .map(|listed| listed.status.as_str())
            .unwrap_or("")
    }

    /// Sets the views to show; the lines they hold never change.
    ///
    /// # Invariant
    ///
    /// The order is the announcement's on both lists, and the command line is
    /// always last of the panes: it is drawn under everything else.
    pub fn layout(&mut self, layout: &Layout) {
        self.listed = layout.listed.clone();
        self.list_names = layout.listed.iter().map(|l| l.name.clone()).collect();
        self.quota = layout.quota.clone();
        let mut panes = vec![Pane::of("", Kind::List)];
        panes.extend(layout.shown.iter().map(|name| Pane::of(name, Kind::Shown)));
        // An empty name is no command line: a frame of events alone.
        if !layout.command.is_empty() {
            panes.push(Pane::of(&layout.command, Kind::Command));
        }
        // A relayout says what is drawn; where each view is scrolled it leaves.
        for pane in &mut panes {
            let kept = self
                .panes
                .iter()
                .find(|old| old.name == pane.name && old.kind == pane.kind);
            if let Some(old) = kept {
                pane.scroll = old.scroll;
                pane.max_scroll = old.max_scroll;
                // A split that gained a session resizes every pane: the next frame
                // must see a resize, not a new pane.
                pane.last_height = old.last_height;
                pane.last_rows = old.last_rows;
            }
        }
        for pane in &panes {
            // No view caps its blocks on its own: `block_size` is the only cap.
            self.log.set_cap(&pane.name, 0);
        }
        self.panes = panes;
    }

    /// Drops what a window held: the session that name belonged to is gone.
    ///
    /// # Invariant
    ///
    /// The pane goes with its blocks, so a session that takes the name again
    /// starts at its newest row rather than where the deleted one was scrolled to.
    /// See `docs/notes/tui-panes.md`.
    pub fn forget(&mut self, name: &str) {
        self.log.forget(name);
        self.panes.retain(|pane| pane.name != name);
    }

    /// The panes of one kind, in order.
    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(move |pane| pane.kind == kind)
    }

    /// The panes that read events: everything but the session list.
    pub fn events(&self) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(|pane| pane.kind.is_events())
    }

    /// The single line `cmd` and its result are drawn on.
    pub fn command(&self) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.kind == Kind::Command)
    }

    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Drops what one window holds, keeping the window.
    pub fn clear(&mut self, window: &str) {
        self.log.clear(window);
    }

    /// Replaces the whole text of one block: an answer re-rendered as markdown.
    pub fn rewrite(&mut self, key: &str, text: &str) {
        self.log.rewrite(key, text);
    }

    /// Whether one block already exists under this key.
    pub fn has_block(&self, key: &str) -> bool {
        self.log.has_block(key)
    }

    /// Opens or closes the `abstract` `key` names.
    pub fn toggle(&mut self, key: &str) -> bool {
        self.log.toggle(key)
    }

    /// The block whose body holds a cell: what a wheel there scrolls first.
    ///
    /// # Invariant
    ///
    /// A box's rules are not body, so a wheel on them is the pane's.
    /// See [`Log::body_at`].
    pub fn body_at(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<usize> {
        self.log.body_at(window, width, height, scroll, row)
    }

    /// Scrolls the box of an opened item, which SPEC makes scrollable.
    ///
    /// # Invariant
    ///
    /// Answers whether the box took the wheel: `false` is the pane's turn.
    /// See [`Log::scroll_block`].
    pub fn scroll_block(&mut self, at: usize, rows: i64) -> bool {
        self.log.scroll_block(at, rows)
    }

    /// Opens or closes the `abstract` at one block, which is what a press names.
    pub fn toggle_at(&mut self, at: usize) -> bool {
        self.log.toggle_at(at)
    }

    /// The block under one row of a window, as the key a press carries.
    pub fn block_at(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<usize> {
        self.log.block_at(window, width, height, scroll, row)
    }

    /// Replaces a call's provisional body with its structured head.
    pub fn describe(&mut self, name: &str, key: &str, head: &cli::protocol::Head) {
        self.log.describe(name, key, head);
    }

    /// Replaces the label of the block `key` names, and the kind it is drawn as.
    pub fn relabel(&mut self, key: &str, tag: &str, kind: LineKind) {
        self.log.relabel(key, tag, kind);
    }

    /// Caps every view's blocks, and the ones opened later too.
    pub fn set_block_size(&mut self, lines: usize) {
        self.log.set_block_size(lines);
    }

    pub fn line(&mut self, name: &str, kind: LineKind, text: impl Into<String>) {
        self.log.push(name, kind, text);
    }

    pub fn stream(
        &mut self,
        name: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        self.log.stream(name, key, kind, prefix, text, done);
    }

    /// The pane `name` shows, read-only.
    pub fn get(&self, name: &str) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.name == name)
    }

    /// The session list: the one view drawn from the announcement, not the log.
    ///
    /// # Invariant
    ///
    /// Its name is empty because it has no window of the log to read: the rows it
    /// draws are SPEC's session list, and `list_names` is where they come from.
    pub fn list(&self) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.kind == Kind::List)
    }

    pub fn list_mut(&mut self) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.kind == Kind::List)
    }

    /// Positive scrolls into the past, negative towards the newest line.
    pub fn scroll_by(&mut self, name: &str, rows: i64) {
        if let Some(pane) = self.get_mut(name) {
            pane.scroll_by(rows);
        }
    }

    /// Scrolls the session list by `rows`, by the rule every other view obeys.
    pub fn scroll_list_by(&mut self, rows: i64) {
        if let Some(pane) = self.list_mut() {
            pane.scroll_by(rows);
        }
    }

    /// Pins one pane to its newest line: what "back to the bottom" means.
    pub fn pin(&mut self, name: &str) {
        if let Some(pane) = self.get_mut(name) {
            pane.scroll = 0;
        }
    }

    /// Pins every view of the log to its newest line.
    ///
    /// # Invariant
    ///
    /// The list is not one of them: a submitted line appends to the sessions, never
    /// to the names, so pinning it would be the auto scroll SPEC forbids. See
    /// `docs/notes/tui-panes.md`.
    pub fn pin_newest(&mut self) {
        for pane in &mut self.panes {
            if pane.kind.is_events() {
                pane.scroll = 0;
            }
        }
    }

    /// Rows this window holds at `width`.
    pub fn rows(&mut self, window: &str, width: usize) -> usize {
        self.log.rows(window, width)
    }

    /// The rows one frame of one window shows.
    pub fn window(
        &mut self,
        window: &str,
        width: usize,
        height: usize,
        scroll: usize,
    ) -> Viewport<'_> {
        self.log.window(window, width, height, scroll)
    }
}
