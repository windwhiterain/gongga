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
    /// A session drawn as its own pane of events.
    Full,
    /// A session drawn as one line per event, below the panes.
    Line,
    /// The single line `cmd` and its result share.
    Command,
}

impl Kind {
    /// Whether this pane reads events at all: the list draws its own rows.
    pub fn is_events(self) -> bool {
        matches!(self, Kind::Full | Kind::Line | Kind::Command)
    }

    /// Whether the wheel and the keyboard may scroll it.
    pub fn scrolls(self) -> bool {
        matches!(self, Kind::Full | Kind::Line)
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
    /// The names that announcement puts in the list: SPEC's `render mode = none`.
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
    pub fn chrome(&self) -> crate::render::Chrome {
        crate::render::Chrome {
            command: self.command().is_some(),
            line_groups: self.of_kind(Kind::Line).count(),
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
    /// Only the sessions whose render mode is `none`: SPEC gives each mode one
    /// area, and a name in two areas is the same name said twice.
    pub fn list_names(&self) -> &[String] {
        &self.list_names
    }

    /// Sets the views to show; the lines they hold never change.
    ///
    /// # Invariant
    ///
    /// The order is the announcement's on both lists, and the command line is
    /// always last of the panes: it is drawn under everything else.
    pub fn layout(&mut self, layout: &Layout) {
        self.listed = layout.listed.clone();
        self.list_names = layout.none.clone();
        self.quota = layout.quota.clone();
        let mut panes = vec![Pane::of("", Kind::List)];
        panes.extend(layout.full.iter().map(|name| Pane::of(name, Kind::Full)));
        panes.extend(layout.line.iter().map(|name| Pane::of(name, Kind::Line)));
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
            // The command line is one row per block whatever the log-wide cap says.
            self.log
                .set_cap(&pane.name, if pane.kind == Kind::Command { 1 } else { 0 });
        }
        self.panes = panes;
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

    /// The newest event as one row, for a session drawn as a line.
    pub fn headline(&mut self, view: View<'_>, width: usize) -> Option<(LineKind, String)> {
        self.log.headline(view, width)
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
