//! The views the events area is divided into, over one scrollback.
//!
//! # Invariant
//!
//! A line names the window it belongs to, and every view reads the same blocks: the
//! division says what a frame shows, never what the client holds. See
//! `docs/notes/tui-panes.md`.

use crate::log::{Log, Viewport};
use crate::{Layout, LineKind, View, Window};

/// The name the merged view is filed under, which no session can have.
const MERGED: &str = "";

/// One view of the events area: what it shows and where it is scrolled.
pub struct Pane {
    pub name: String,
    /// The single line `cmd` and its result share, drawn outside the other panes.
    pub command: bool,
    /// The merged view: every line, in arrival order.
    pub merged: bool,
    /// Rows scrolled up from this pane's newest line.
    pub scroll: usize,
    /// The scroll bound, written back by the renderer.
    pub max_scroll: usize,
}

impl Pane {
    fn merged() -> Self {
        Self {
            name: MERGED.to_string(),
            command: false,
            merged: true,
            scroll: 0,
            max_scroll: 0,
        }
    }

    /// Which rows this pane reads.
    pub fn view(&self) -> View<'_> {
        if self.merged {
            View::All
        } else {
            View::Window(&self.name)
        }
    }

    /// A pane worth scrolling to: the one under the wheel, or the keyboard's focus.
    pub fn is_event(&self) -> bool {
        !self.command
    }
}

/// One scrollback, and the panes that show it.
#[derive(Default)]
pub struct Panes {
    log: Log,
    panes: Vec<Pane>,
}

impl Panes {
    pub fn new() -> Self {
        Self {
            log: Log::new(),
            panes: vec![Pane::merged()],
        }
    }

    /// Sets the views to show; the lines they hold never change.
    pub fn layout(&mut self, layout: &Layout) {
        self.panes = match layout {
            Layout::Merged { hidden } => {
                // Every block but the hidden sessions': their lines stay in the
                // log, so showing one again costs no trip.
                self.log.set_hidden(hidden);
                vec![Pane::merged()]
            }
            Layout::Split(windows) => windows.iter().map(Pane::of).collect(),
        };
        for pane in &self.panes {
            // The command line is one row per block whatever the log-wide cap says.
            self.log
                .set_cap(&pane.name, if pane.command { 1 } else { 0 });
        }
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

    /// The panes that share the events area: everything but the command line.
    pub fn events(&self) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(|pane| !pane.command)
    }

    /// The single line `cmd` and its result are drawn on.
    pub fn command(&self) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.command)
    }

    /// Where a keyboard with no focus yet starts: the first view sharing the area.
    pub fn first_name(&self) -> String {
        self.events()
            .next()
            .map(|pane| pane.name.clone())
            .unwrap_or_default()
    }

    /// Positive scrolls into the past, negative towards the newest line.
    pub fn scroll_by(&mut self, name: &str, rows: i64) {
        let Some(pane) = self.get_mut(name) else {
            return;
        };
        pane.scroll = (pane.scroll as i64 + rows).clamp(0, pane.max_scroll as i64) as usize;
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

impl Pane {
    fn of(window: &Window) -> Self {
        Self {
            name: window.name.clone(),
            command: window.command,
            merged: false,
            scroll: 0,
            max_scroll: 0,
        }
    }
}
