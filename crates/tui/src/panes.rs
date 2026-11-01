//! The views the events area is divided into, over one scrollback.
//!
//! # Invariant
//!
//! A line names the window it belongs to, and every view reads the same blocks: the
//! layout says what a frame shows, never what the client holds. See
//! `docs/notes/tui-panes.md`.

use crate::log::{Log, Viewport};
use crate::{Layout, LineKind, View};

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

    /// Whether the wheel and the keyboard may scroll it.
    pub fn scrolls(self) -> bool {
        self == Kind::Shown
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

    /// Which rows this pane reads.
    pub fn view(&self) -> View<'_> {
        View::Window(&self.name)
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
}

impl Panes {
    pub fn new() -> Self {
        Self {
            log: Log::new(),
            panes: Vec::new(),
            listed: Vec::new(),
            list_names: Vec::new(),
            quota: None,
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
            Some(name) => self.log.rows(View::Window(name), width).max(1) as u16,
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

    /// Scrolls the box of an opened item, which SPEC makes scrollable.
    pub fn scroll_block(&mut self, at: usize, rows: i64) -> bool {
        self.log.scroll_block(at, rows)
    }

    /// Whether the block `at` is an opened item with a box of its own.
    pub fn expanded_at(&self, at: usize) -> bool {
        self.log.expanded_at(at)
    }

    /// Opens or closes the `abstract` at one block, which is what a press names.
    pub fn toggle_at(&mut self, at: usize) -> bool {
        self.log.toggle_at(at)
    }

    /// The block under one row of a view, as the key a press carries.
    pub fn block_at(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
        row: usize,
    ) -> Option<usize> {
        self.log.block_at(view, width, height, scroll, row)
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

    /// Positive scrolls into the past, negative towards the newest line.
    pub fn scroll_by(&mut self, name: &str, rows: i64) {
        let Some(pane) = self.get_mut(name) else {
            return;
        };
        pane.scroll = (pane.scroll as i64 + rows).clamp(0, pane.max_scroll as i64) as usize;
    }

    /// Pins one pane to its newest line: what "back to the bottom" means.
    pub fn pin(&mut self, name: &str) {
        if let Some(pane) = self.get_mut(name) {
            pane.scroll = 0;
        }
    }

    /// Pins every pane to its newest line.
    pub fn pin_newest(&mut self) {
        for pane in &mut self.panes {
            pane.scroll = 0;
        }
    }

    /// Rows this view holds at `width`.
    pub fn rows(&mut self, view: View<'_>, width: usize) -> usize {
        self.log.rows(view, width)
    }

    /// The rows one frame of one view shows.
    pub fn window(
        &mut self,
        view: View<'_>,
        width: usize,
        height: usize,
        scroll: usize,
    ) -> Viewport<'_> {
        self.log.window(view, width, height, scroll)
    }
}
