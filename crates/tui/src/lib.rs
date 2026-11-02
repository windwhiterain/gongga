//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `cli`, never the reverse, so the command layer stays usable
//! from a script and a new command never changes this crate.

mod cache;
mod clipboard;
mod fold;
mod input;
mod log;
mod markdown;
mod panes;
pub mod program;
mod render;
pub mod run;
mod table;

use std::io::{self, Stdout, Write, stdout};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::ClearType;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

pub use crate::input::{InputLine, apply_completion};
pub use crate::log::Log;
pub use crate::panes::Panes;
pub use crate::run::interactive;

/// Runs the window: its own runtime, because the loop owns every task in it.
pub fn main(args: cli::Args) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(interactive(&args))
}

// The vocabulary is the command layer's, re-exported so this crate can name it.
pub use cli::{
    Candidate, Completer, Completion, Layout, LineKind, Listed, QuotaRow, SharedCompleter,
    token_bounds,
};

/// How long a keystroke waits to see whether the next one is part of a paste.
///
/// # Invariant
///
/// The wait only ever joins keys, never sends: one `Enter` alone still submits.
/// See `docs/notes/tui-paste.md`.
const PASTE_GAP: Duration = Duration::from_millis(10);

/// How soon after a machine-fast run an `Enter` still belongs to that paste.
///
/// # Invariant
///
/// A run of [`MACHINE_RUN`] text keys inside [`PASTE_GAP`] is delivery, not a
/// keyboard, so the break that follows it is the paste's, not a submit.
/// See `docs/notes/tui-paste.md`.
const GLUE_GAP: Duration = Duration::from_millis(150);

/// Text keys inside one [`PASTE_GAP`] that a keyboard cannot produce.
const MACHINE_RUN: usize = 3;

/// What Alt+<key> does to the terminal itself, rather than to the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleAction {
    /// Alt+Y: put the visible rows on the clipboard.
    Copy,
    /// Alt+M: toggle whether this app or the terminal owns the mouse.
    Mouse,
}

/// What a vertical arrow asks for: SPEC's three bindings on ↑/↓, told apart.
///
/// # Invariant
///
/// The recall is Shift's, never a bare key. A bare ↑ is the line above the
/// caret and nothing else, so on a one-line prompt it is not a key at all --
/// the last command line is one Shift+↑ away, and never a surprise. See
/// `docs/notes/tui-lib.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrow {
    /// Shift+↑: the last command line, then the one before it.
    LastCmd,
    /// Shift+↓: the sticky session's newest row.
    Bottom,
    /// A bare ↑/↓: the popup first, then the line above (true) or below.
    Line(bool),
}

/// Something the operator did: only what a keyboard can do that a command
/// cannot. See `docs/notes/tui-lib.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A finished input line. It may contain newlines: a message with
    /// paragraphs is still one message.
    Submit(String),
    /// Alt+Q: leave.
    Quit,
    /// Nothing to submit: the completion asked for something only the
    /// connection can answer, so the loop has to go round to send it.
    ///
    /// # Invariant
    ///
    /// A keystroke whose ask is never returned leaves it unsent: the loop owns the
    /// connection and runs only when `next_event` returns.
    Recomplete,
}

/// Which part of the window a pointer landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The session list, which scrolls its own rows.
    List,
    /// One session's header: its name and the figures beside it.
    Header,
    /// One session's events.
    Pane,
    /// The way back to the newest row, drawn over a pane's last row.
    Bottom,
}

/// What a pointer landed on: the session it names, and which part it hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub name: String,
    pub part: Part,
}

/// One cell of a window's rows, as the log counts them.
struct PaneCell {
    /// The session whose window holds the cell.
    name: String,
    /// The window's log width: one column short of the drawn one.
    width: usize,
    /// The rows the window shows.
    height: usize,
    /// How far the window is scrolled up from its newest row.
    scroll: usize,
    /// The cell's row inside the window.
    row: usize,
    /// The cell's column inside the window, for the margin a box leaves each side.
    column: usize,
}

/// Whose server the window is talking to, which the header says beside the status.
///
/// # Invariant
///
/// A window can only learn this from the greeting: `owner` is the pid the server
/// follows, and this process's own pid is what makes it *our* server. See
/// `docs/notes/tui-lib.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// This window started it, so it goes when the window does.
    Ours { pid: u32 },
    /// It was already running; this window took it as its own, and it goes too.
    Adopted { pid: u32 },
    /// Another window owns it: this one reads and cannot stop it.
    Another { pid: u32 },
    /// Nobody owns it: nothing will stop it when its windows close.
    Unowned { pid: u32 },
}

impl Server {
    /// The words the header shows: two, because the status shares the row.
    pub fn label(&self) -> String {
        match self {
            Server::Ours { pid } => format!("own #{pid}"),
            Server::Adopted { pid } => format!("adopted #{pid}"),
            Server::Another { pid } => format!("other #{pid}"),
            Server::Unowned { pid } => format!("unowned #{pid}"),
        }
    }
}

/// Everything the renderer reads, separate from the terminal it draws to.
pub struct Screen {
    /// The windows the events area is split into: a frame may not walk their logs.
    pub panes: Panes,
    pub completion: Option<Completion>,
    pub input: InputLine,
    pub status: String,
    /// What kind of thing the status line is saying, for its colour.
    pub status_kind: LineKind,
    /// Whose server this window is on, once a dial has answered.
    pub server: Option<Server>,
    pub placeholder: String,
    /// The session the sticky `session` argument names: what the keys scroll
    /// and what a click sets.
    pub sticky_session: Option<String>,
}

impl Screen {
    /// A note about this front-end, on the status line.
    ///
    /// # Invariant
    ///
    /// The status line, and nowhere else. Not the command row, which SPEC gives to
    /// `cmd` and `cmd_result`; and not a session's lines, which are that session's
    /// events -- "nothing is listening" is not something a session said. A note
    /// replaces the previous one, so the line always reads as the newest thing
    /// this window knows. See `docs/notes/tui-lib.md`.
    pub fn note(&mut self, kind: LineKind, text: impl Into<String>) {
        self.status_kind = kind;
        self.status = text.into();
    }

    /// Puts text into the input line, the way a paste does.
    ///
    /// # Invariant
    ///
    /// The popup is closed first: it was about the token before the paste, not
    /// about what the pasted text means now. The text goes in whole, never key by
    /// key, so a newline inside it cannot press Enter.
    pub fn insert_paste(&mut self, text: &str) {
        self.completion = None;
        self.input.insert_str(text);
    }

    /// Positive scrolls into the past, negative towards the newest line, on the
    /// sticky session -- which is SPEC's rule.
    pub fn scroll_by(&mut self, rows: i64) {
        let Some(target) = self.sticky_session.clone() else {
            return;
        };
        self.panes.scroll_by(&target, rows);
    }

    /// Shift+Down: back to the newest line, which is where a window starts.
    pub fn scroll_to_bottom(&mut self) {
        let Some(target) = self.sticky_session.clone() else {
            return;
        };
        self.panes.pin(&target);
    }

    /// The panes as a terminal this size lays them out, in its row numbers.
    fn slots(&mut self, width: u16, height: u16) -> Vec<render::Slot> {
        let area = Rect::new(0, 0, width, height);
        let input = render::input_rows(self.input.text(), width, height);
        let chrome = self.panes.chrome(width.saturating_sub(1).max(1) as usize);
        let chunks = render::areas(area, input, chrome);
        let (_, panes) = render::body(chunks[1], &self.panes);
        render::pane_slots(panes, &self.panes, self.sticky_session.as_deref()).0
    }

    /// Where a pane's rows come from and how far it is scrolled.
    fn pane_at(&self, name: &str) -> Option<(String, usize)> {
        let pane = self.panes.get(name)?;
        Some((pane.name.clone(), pane.scroll))
    }

    /// The pane covering this cell, which is where the wheel landed.
    pub fn pane_at_row(
        &mut self,
        width: u16,
        height: u16,
        column: u16,
        row: u16,
    ) -> Option<String> {
        render::pane_at(&self.slots(width, height), column, row).map(str::to_string)
    }

    /// The session a click on `row` names, from the list, a pane, or a group.
    pub fn session_at_row(
        &mut self,
        width: u16,
        height: u16,
        column: u16,
        row: u16,
    ) -> Option<Hit> {
        let area = Rect::new(0, 0, width, height);
        let input = render::input_rows(self.input.text(), width, height);
        let chrome = self.panes.chrome(width.saturating_sub(1).max(1) as usize);
        let chunks = render::areas(area, input, chrome);
        let (list, _) = render::body(chunks[1], &self.panes);
        let slots = self.slots(width, height);
        // The hint answers before the row it covers, the list's included.
        if let Some(name) = render::bottom_at(&slots, list, &self.panes, column, row) {
            return Some(Hit {
                name,
                part: Part::Bottom,
            });
        }
        if let Some(name) = render::list_at(list, &self.panes, column, row) {
            return Some(Hit {
                name,
                part: Part::List,
            });
        }
        if let Some(name) = render::header_at(&slots, column, row) {
            return Some(Hit {
                name: name.to_string(),
                part: Part::Header,
            });
        }
        render::pane_at(&slots, column, row).map(|name| Hit {
            name: name.to_string(),
            part: Part::Pane,
        })
    }

    /// The item under this cell of a window, as the key a press toggles.
    ///
    /// # Invariant
    ///
    /// It reads the rows the frame drew: the slot gives the cell's row inside the
    /// window, and the log answers which block that row belongs to -- counting the
    /// fold, so a press on a summary line toggles the item it summarises.
    pub fn item_at(&mut self, width: u16, height: u16, column: u16, row: u16) -> Option<usize> {
        let cell = self.cell_at(width, height, column, row)?;
        self.panes.block_at(
            &cell.name,
            cell.width,
            cell.height,
            cell.scroll,
            cell.row,
            cell.column,
        )
    }

    /// The opened block holding this cell, which is what a wheel there scrolls.
    ///
    /// # Invariant
    ///
    /// Only a box's own content answers. A box's rules and its margin, and every
    /// row of any other block, are the pane's. See [`Log::box_at`].
    fn box_at(&mut self, width: u16, height: u16, column: u16, row: u16) -> Option<usize> {
        let cell = self.cell_at(width, height, column, row)?;
        self.panes.box_at(
            &cell.name,
            cell.width,
            cell.height,
            cell.scroll,
            cell.row,
            cell.column,
        )
    }

    /// One cell of a window, in the terms the log answers in: what a press and a
    /// wheel ask about.
    ///
    /// # Invariant
    ///
    /// The width is one column short, as the renderer measures, and the row is
    /// counted from the window's first: the same numbers the frame was drawn with,
    /// so a hit cannot land on a row the frame drew something else on.
    fn cell_at(&mut self, width: u16, height: u16, column: u16, row: u16) -> Option<PaneCell> {
        let slots = self.slots(width, height);
        let slot = slots
            .iter()
            .find(|slot| render::covers(&slot.body, column, row))?;
        let body = slot.body;
        Some(PaneCell {
            name: slot.name.clone(),
            width: body.width.saturating_sub(1).max(1) as usize,
            height: body.height.max(1) as usize,
            scroll: self
                .panes
                .get(&slot.name)
                .map(|pane| pane.scroll)
                .unwrap_or(0),
            row: (row - body.y) as usize,
            column: (column - body.x) as usize,
        })
    }

    /// Scrolls one pane: the wheel reads what the pointer is over, not the keys'.
    pub fn scroll_pane(&mut self, name: &str, rows: i64) {
        self.panes.scroll_by(name, rows);
    }

    /// Scrolls the item under a pointer, or its pane when there is no open item.
    ///
    /// # Invariant
    ///
    /// What is under the pointer decides, never how far it can move: a box takes the
    /// wheel and keeps it, so opening an item cannot leave the reader scrolled
    /// somewhere they did not ask for; a box's margin and its rules are the pane's,
    /// so a pane is still reachable under a box that covers it. See
    /// `docs/notes/tui-log.md`.
    pub fn scroll_at(&mut self, width: u16, height: u16, column: u16, row: u16, rows: i64) -> bool {
        let Some(hit) = self.session_at_row(width, height, column, row) else {
            return false;
        };
        if hit.part == Part::Pane
            && let Some(at) = self.box_at(width, height, column, row)
        {
            // The box and only the box: no handover. Margin and rules are the pane's.
            self.panes.scroll_block(at, rows);
            return true;
        }
        // The list is its own scroll -- SPEC gives it one -- and the empty name
        // it is filed under is the pane, not a session.
        match hit.part {
            Part::List => self.panes.scroll_list_by(rows),
            _ => self.scroll_pane(&hit.name, rows),
        }
        true
    }

    /// Every row the panes show, top to bottom, in the order the frame draws them.
    ///
    /// # Invariant
    ///
    /// This is the layout `draw` uses, so a copy takes exactly what was on the
    /// screen and nothing that was not.
    pub fn visible_rows(&mut self, width: u16, height: u16) -> Vec<String> {
        let slots = self.slots(width, height);
        // Each pane's own columns: the window's width would copy a wrap the
        // frame never drew.
        let mut shapes: Vec<(String, u16, u16)> = slots
            .iter()
            .map(|slot| (slot.name.clone(), slot.body.width, slot.body.height))
            .collect();
        if let Some(pane) = self.panes.command() {
            shapes.push((pane.name.clone(), width, render::COMMAND_ROWS));
        }
        let mut rows = Vec::new();
        for (name, pane_width, pane_height) in shapes {
            // One column short, as the renderer measures.
            let log_width = pane_width.saturating_sub(1).max(1) as usize;
            rows.extend(self.rows_of(&name, log_width, (pane_height as usize).max(1)));
        }
        rows
    }

    /// The rows one pane shows, borrowed for no longer than the copy needs them.
    fn rows_of(&mut self, name: &str, width: usize, height: usize) -> Vec<String> {
        let Some((name, scroll)) = self.pane_at(name) else {
            return Vec::new();
        };
        let window = self.panes.window(&name, width, height, scroll);
        window
            .rows
            .iter()
            .map(|(_, row, _)| row.text().to_string())
            .collect()
    }

    /// The rows the sticky session gets in a frame this size: what a page means.
    pub fn page_height(&mut self, width: u16, height: u16) -> usize {
        let Some(name) = self.sticky_session.clone() else {
            return 1;
        };
        self.slots(width, height)
            .iter()
            .find(|slot| slot.name == name)
            .map(|slot| slot.body.height as usize)
            .unwrap_or(1)
    }
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    screen: Screen,
    completer: SharedCompleter,
    history: Vec<String>,
    history_pos: Option<usize>,
    events: EventStream,
    /// Whether mouse capture is on, so the toggle knows what to flip.
    mouse_captured: bool,
    restored: bool,
    /// Whether anything changed since the last frame. Every mutation sets it.
    dirty: bool,
    /// Keys seen so far that may be one paste. See `docs/notes/tui-paste.md`.
    pending: Vec<KeyEvent>,
    /// When the last run ended, and how many text keys it carried.
    settled: Option<(std::time::Instant, usize)>,
    /// An event read while deciding whether a keystroke began a paste.
    stashed: Option<std::io::Result<TermEvent>>,
    /// The input as the last frame saw it: typing may have put console ink on
    /// the row.
    input_seen: Option<(usize, usize)>,
    /// A click on a session, as the command line it asks for, waiting for the loop
    /// to take it. See `docs/notes/tui-lib.md`.
    clicked: Option<String>,
}

impl Tui {
    /// The window, whose rows are written down under this client's own root.
    pub fn new(
        completer: SharedCompleter,
        client_home: &Path,
        endpoint: &str,
    ) -> anyhow::Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        let initialized = (|| {
            let mut out = stdout();
            // Capture makes the wheel a scroll key; Alt+M gives it back. Bracketed
            // paste makes a paste arrive as one string.
            execute!(
                out,
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste
            )
            .context("entering alternate screen")?;
            let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
            // A blank cell is never written to, so a dead process's last frame stays.
            terminal.clear().context("clearing the alternate screen")?;
            anyhow::Ok(terminal)
        })();
        let terminal = match initialized {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_terminal(&mut stdout());
                return Err(error);
            }
        };
        Ok(Self {
            terminal,
            screen: Screen {
                panes: Panes::with_cache(client_home, endpoint),
                completion: None,
                input: InputLine::default(),
                status: "connecting".to_string(),
                status_kind: LineKind::System,
                server: None,
                placeholder: "message the agent, or / for a command (Tab completes)".to_string(),
                sticky_session: None,
            },
            completer,
            history: Vec::new(),
            history_pos: None,
            input_seen: None,
            clicked: None,
            events: EventStream::new(),
            mouse_captured: true,
            restored: false,
            // The first frame has to happen even though nothing has changed.
            dirty: true,
            pending: Vec::new(),
            settled: None,
            stashed: None,
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Sets the status text, asking for a frame only when it actually changed.
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        if status != self.screen.status {
            self.screen.status = status;
            self.dirty = true;
        }
    }

    /// Says whose server this window is on, or clears it when the link went away.
    pub fn set_server(&mut self, server: Option<Server>) {
        if self.screen.server != server {
            self.screen.server = server;
            self.dirty = true;
        }
    }

    /// A `connect_server` moved the address: the rows on screen are the old
    /// endpoint's, so they go.
    ///
    /// # Invariant
    ///
    /// Nothing is deleted. The other endpoint's records are still under this client
    /// root, and switching back finds them; what this drops is the blocks in memory,
    /// which the next frame refuses to draw as the new server's. See
    /// `docs/notes/tui-cache.md`.
    pub fn repoint(&mut self, client_home: &Path, endpoint: &str) {
        self.screen.panes.repoint(client_home, endpoint);
        self.dirty = true;
    }

    /// What kind of thing the status line says, for its colour.
    pub fn set_status_kind(&mut self, kind: LineKind) {
        if self.screen.status_kind != kind {
            self.screen.status_kind = kind;
            self.dirty = true;
        }
    }

    /// How tall one block may get, as the command layer last said.
    pub fn set_block_size(&mut self, lines: usize) {
        self.screen.panes.set_block_size(lines);
        self.dirty = true;
    }

    /// How much of a window each side of a box gives back to the pane.
    pub fn set_box_edge(&mut self, columns: usize) {
        self.screen.panes.set_box_edge(columns);
        self.dirty = true;
    }

    /// How the events area is divided, as the command layer last said.
    pub fn set_layout(&mut self, layout: &Layout) {
        self.screen.panes.layout(layout);
        self.dirty = true;
    }

    /// The session the sticky `session` argument names, as the command layer said.
    pub fn set_sticky_session(&mut self, name: Option<&str>) {
        let name = name.map(str::to_string);
        if self.screen.sticky_session == name {
            return;
        }
        // The target moved, so the view starts at its own newest line.
        self.screen.sticky_session = name;
        self.dirty = true;
    }

    /// Something this front-end says about itself: on the status line, where the
    /// harness's own state is read.
    ///
    /// # Invariant
    ///
    /// Never the command line. SPEC gives that row to `cmd` and `cmd_result`, and
    /// it keeps only the newest one, so a note about the window joins a ticker of
    /// commands the operator typed -- which is how "starting one" came to sit
    /// where an input line is expected. See `docs/notes/tui-lib.md`.
    pub fn say(&mut self, kind: LineKind, text: impl Into<String>) {
        self.screen.note(kind, text);
        self.dirty = true;
    }

    /// Adds a finished line to the window `name` names.
    pub fn push(&mut self, name: &str, kind: LineKind, text: impl Into<String>) {
        self.screen.panes.line(name, kind, text);
        self.dirty = true;
    }

    /// Empties the window a deleted session's lines were read in.
    ///
    /// # Invariant
    ///
    /// The name may come back, so nothing of the last session survives it: a
    /// session made again under the same name opens on its own first line. See
    /// `docs/notes/tui-lib.md`.
    pub fn forget(&mut self, name: &str) {
        self.screen.panes.forget(name);
        self.dirty = true;
    }

    /// Puts a line back into the prompt, cursor at the end. The counterpart of Enter.
    pub fn restore_input(&mut self, text: &str) {
        self.screen.completion = None;
        self.screen.input.clear();
        self.screen.input.insert_str(text);
        self.screen.input.to_document_end();
        self.dirty = true;
    }

    /// Appends to the block `key` names, in the window `name` names.
    ///
    /// # Invariant
    ///
    /// SPEC's `output (colored markdown)` is the block's own document: a delta joins
    /// it, and the rows are what it says at the width the block has. No other kind
    /// keeps a document, and none of them re-lays-out on a delta.
    pub fn stream(
        &mut self,
        name: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        self.screen
            .panes
            .stream(name, key, kind, prefix, text, done);
        self.dirty = true;
    }

    /// Drops what one window holds: SPEC's command row shows only the last one.
    pub fn clear(&mut self, name: &str) {
        self.screen.panes.clear(name);
        self.dirty = true;
    }

    /// Replaces what one block holds: a call's structure, once it is known.
    pub fn rewrite(&mut self, _name: &str, key: &str, text: &str) {
        self.screen.panes.rewrite(key, text);
        self.dirty = true;
    }

    /// Shows a call's name and fields as a structured section.
    pub fn describe(&mut self, name: &str, key: &str, head: &cli::protocol::Head) {
        self.screen.panes.describe(name, key, head);
        self.dirty = true;
    }

    /// The label of a block that learned its outcome after it opened.
    pub fn relabel(&mut self, _name: &str, key: &str, tag: &str, kind: LineKind) {
        self.screen.panes.relabel(key, tag, kind);
        self.dirty = true;
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        // Nothing changed, so nothing is written: a fast stream cannot hog the loop.
        if !self.dirty {
            return Ok(());
        }
        let seen = (self.screen.input.text().len(), self.screen.input.cursor());
        let typed = self.input_seen != Some(seen);
        self.input_seen = Some(seen);
        let frame = guarded(|| present(&mut self.terminal, &mut self.screen, typed));
        match frame {
            Ok(painted) => painted?,
            // Abandoned, not fatal: the screen is cleared, and the row says why.
            Err(payload) => {
                let _ = self.terminal.clear();
                self.screen.note(LineKind::Error, frame_failed(&payload));
            }
        }
        // Cleared only after the frame landed, so a failed paint is retried.
        self.dirty = false;
        Ok(())
    }

    /// Waits for the operator, painting a frame before every wait.
    pub async fn next_event(&mut self) -> Option<UiEvent> {
        loop {
            // A click is a command, and commands go through the loop that already
            // knows how to send one.
            if let Some(line) = self.clicked.take() {
                return Some(UiEvent::Submit(line));
            }
            if self.draw().is_err() {
                return None;
            }
            // A run in progress is finished first: it lives in `self`, so a
            // cancelled wait resumes it instead of losing the keys.
            if !self.pending.is_empty() {
                match tokio::time::timeout(PASTE_GAP, self.events.next()).await {
                    // Quiet: the run is over.
                    Err(_) => {}
                    Ok(None) => return self.flush(),
                    Ok(Some(Err(e))) => self.stashed = Some(Err(e)),
                    Ok(Some(Ok(TermEvent::Key(key))))
                        if Self::actionable(&key) && Self::paste_key(&key) =>
                    {
                        self.pending.push(key);
                        continue;
                    }
                    // Anything else is not part of a paste, and is kept for later.
                    Ok(Some(Ok(other))) => self.stashed = Some(Ok(other)),
                }
                if let Some(action) = self.flush() {
                    return Some(action);
                }
                continue;
            }
            let next = match self.stashed.take() {
                Some(held) => held,
                None => self.events.next().await?,
            };
            // Every event kind is handled; a resize only needs the redraw it asks for.
            match next {
                Ok(TermEvent::Key(key)) => {
                    // Windows reports Release too; without the filter one key acts twice.
                    if !Self::actionable(&key) {
                        continue;
                    }
                    // Held until the pause that says whether this was a paste.
                    self.pending.push(key);
                }
                Ok(TermEvent::Mouse(mouse)) => self.handle_mouse(mouse),
                // One string, so a pasted block stays a block: see `paste`.
                Ok(TermEvent::Paste(text)) => self.paste(&text),
                Ok(TermEvent::Resize(..)) => self.dirty = true,
                _ => {}
            }
        }
    }

    /// Whether a key event drives the UI: presses and repeats, never releases.
    fn actionable(key: &KeyEvent) -> bool {
        !matches!(key.kind, KeyEventKind::Release)
    }

    /// Whether a key is one a paste arrives as, when the terminal cannot say so.
    fn paste_key(key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(_) => !key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL),
            KeyCode::Enter => is_paste_newline(key),
            _ => false,
        }
    }

    /// Ends a run: a paste is inserted, typing is replayed key by key.
    ///
    /// # Invariant
    ///
    /// A run that changed the prompt marks the frame dirty. `draw` skips a frame
    /// nothing changed for, so a missed flag is a keypress that looks ignored.
    fn flush(&mut self) -> Option<UiEvent> {
        let keys = std::mem::take(&mut self.pending);
        // A break right after a machine-fast run is that paste's own newline.
        let run = self
            .settled
            .filter(|(at, _)| at.elapsed() <= GLUE_GAP)
            .map(|(_, chars)| chars);
        let action = match pasted(&keys) {
            Some(text) => {
                self.paste(&text);
                self.settled = None;
                None
            }
            None if glue_break(&keys, run) => {
                self.paste("\n");
                // The paste may continue after this break, so the run stays open.
                self.settled = Some((std::time::Instant::now(), MACHINE_RUN));
                None
            }
            None => {
                let run = keys.iter().filter(|k| k.code != KeyCode::Enter).count();
                if run >= MACHINE_RUN {
                    self.settled = Some((std::time::Instant::now(), run));
                } else {
                    self.settled = None;
                }
                keys.into_iter().find_map(|key| self.handle_key(key))
            }
        };
        self.dirty = true;
        // A completion that asked for a peer's directory has to reach the loop,
        // which owns the connection; the ask is taken there.
        action.or_else(|| self.completer.wants().then_some(UiEvent::Recomplete))
    }

    /// The one key that is not editing: leaving, on Alt+Q. See `docs/notes/tui-lib.md`.
    fn shortcut(key: &KeyEvent) -> Option<UiEvent> {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Case-insensitive: Shift+Alt+Q arrives as 'Q'.
            KeyCode::Char('q' | 'Q') if alt => Some(UiEvent::Quit),
            _ => None,
        }
    }

    /// The keys that act on the terminal, not on the conversation.
    fn console_key(key: &KeyEvent) -> Option<ConsoleAction> {
        if !key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
        match key.code {
            KeyCode::Char('y' | 'Y') => Some(ConsoleAction::Copy),
            KeyCode::Char('m' | 'M') => Some(ConsoleAction::Mouse),
            _ => None,
        }
    }

    /// Whether this key asks for the clipboard: SPEC's `Alt+V`, and `Ctrl+V`
    /// where a terminal forwards it, case either way.
    ///
    /// # Invariant
    ///
    /// Alt alone is the binding, not Alt as a tolerated extra. A terminal that
    /// keeps Ctrl+V for its own paste -- VS Code does -- never forwards the bare
    /// key, and the keystroke it sends for Ctrl+Alt+V reaches no console app at
    /// all, so a Ctrl spelling would leave an operator there with no key. See
    /// `docs/notes/tui-paste.md`.
    fn clipboard_paste(key: &KeyEvent) -> bool {
        key.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('v' | 'V'))
    }

    /// Whether this key is SPEC's delete: Shift+Delete, never a bare Delete.
    fn delete_key(key: &KeyEvent) -> bool {
        key.code == KeyCode::Delete && key.modifiers.contains(KeyModifiers::SHIFT)
    }

    /// Whether Enter breaks instead of sending: Shift or Alt (ESC CR), not plain Enter.
    fn line_break(key: &KeyEvent) -> bool {
        matches!(key.code, KeyCode::Enter)
            && key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
    }

    /// What a vertical arrow means, before any state is consulted.
    ///
    /// # Invariant
    ///
    /// Shift is the recall's, and a bare arrow is the prompt's, so the two can
    /// never be the same keystroke. Alt+arrows never arrive here: they page,
    /// above this point.
    fn arrow_key(key: &KeyEvent) -> Option<Arrow> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up if shift => Some(Arrow::LastCmd),
            KeyCode::Up => Some(Arrow::Line(true)),
            KeyCode::Down if shift => Some(Arrow::Bottom),
            KeyCode::Down => Some(Arrow::Line(false)),
            _ => None,
        }
    }

    /// Performs one vertical arrow on the window.
    ///
    /// # Invariant
    ///
    /// The popup answers before the text does: it is what the operator is
    /// looking at, and moving the line behind it moves the text they are
    /// choosing for. See `docs/notes/tui-lib.md`.
    fn arrow(&mut self, action: Arrow) {
        match action {
            Arrow::LastCmd => self.history_prev(),
            Arrow::Bottom => self.screen.scroll_to_bottom(),
            Arrow::Line(up) => {
                if let Some(c) = self.screen.completion.as_mut() {
                    c.move_selection(if up { -1 } else { 1 });
                } else if up {
                    self.screen.input.up();
                } else {
                    self.screen.input.down();
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<UiEvent> {
        if let Some(action) = Self::shortcut(&key) {
            return Some(action);
        }
        if let Some(action) = Self::console_key(&key) {
            self.console(action);
            return None;
        }

        if Self::line_break(&key) {
            self.newline();
            return None;
        }

        // Above the Alt branch: a terminal that keeps Ctrl+V for itself leaves
        // Alt+V as the spelling that reaches this window.
        if Self::clipboard_paste(&key) {
            self.paste_from_clipboard();
            return None;
        }

        // The other modifier bindings use Alt too: a terminal that eats Ctrl+K is common.
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('k' | 'K') => {
                    self.close_completion();
                    self.screen.input.kill_to_end();
                }
                KeyCode::Left => {
                    self.close_completion();
                    self.screen.input.word_left();
                }
                // Alt+arrows page, because PgUp/PgDn hide behind Fn on laptops;
                // a bare ↑↓ stays with the prompt's own lines.
                KeyCode::Up => {
                    let page = self.page_height() as i64;
                    self.screen.scroll_by(page);
                }
                KeyCode::Down => {
                    let page = self.page_height() as i64;
                    self.screen.scroll_by(-page);
                }
                _ => {}
            }
            return None;
        }

        // Ctrl+<letter> must not insert its letter where a terminal delivers it.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        // With nothing typed to edit, Shift+Delete acts on the sticky session.
        if self.screen.input.is_empty()
            && Self::delete_key(&key)
            && let Some(target) = self.screen.sticky_session.clone()
        {
            self.close_completion();
            return Some(UiEvent::Submit(delete_line(&target)));
        }

        if edit_input(
            &mut self.screen.input,
            &mut self.screen.completion,
            key.code,
        ) {
            return None;
        }

        match key.code {
            KeyCode::Esc => self.close_completion(),
            KeyCode::Tab => {
                if self.screen.completion.is_none() {
                    self.refresh_completion();
                }
                self.apply_completion();
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(action) = Self::arrow_key(&key) {
                    self.arrow(action);
                }
            }
            KeyCode::Enter => {
                let raw = self.screen.input.text().trim().to_string();
                if raw.is_empty() {
                    return None;
                }
                self.screen.input.clear();
                self.close_completion();
                self.screen.panes.pin_newest();
                if self.history.last().map(String::as_str) != Some(raw.as_str()) {
                    self.history.push(raw.clone());
                }
                self.history_pos = None;
                return Some(UiEvent::Submit(raw));
            }
            KeyCode::Char(c) => {
                self.screen.input.insert(c);
                self.refresh_completion();
            }
            KeyCode::Backspace => {
                self.screen.input.backspace();
                self.refresh_completion();
            }
            // A page is the log window, not a fixed ten rows, so short terminals work too.
            KeyCode::PageUp => {
                let page = self.page_height() as i64;
                self.screen.scroll_by(page);
            }
            KeyCode::PageDown => {
                let page = self.page_height() as i64;
                self.screen.scroll_by(-page);
            }
            _ => {}
        }
        None
    }

    /// The mouse points at a pane: a notch scrolls it three rows, Shift one. See
    /// `docs/notes/tui-lib.md`.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let rows = Self::wheel_rows(&mouse);
        let pressed = matches!(mouse.kind, MouseEventKind::Down(_));
        if rows.is_none() && !pressed {
            return;
        }
        let Ok(size) = self.terminal.size() else {
            return;
        };
        let Some(hit) =
            self.screen
                .session_at_row(size.width, size.height, mouse.column, mouse.row)
        else {
            return;
        };
        if let Some(rows) = rows {
            // The wheel moves what it is over -- a window, the list, an opened
            // item's box -- and writes no state any key reads.
            self.screen
                .scroll_at(size.width, size.height, mouse.column, mouse.row, rows);
            self.dirty = true;
            return;
        }
        self.dirty = true;
        // Not a click on the row under it: over the list it pins the list itself.
        if hit.part == Part::Bottom {
            self.screen.panes.pin(&hit.name);
            return;
        }
        // SPEC's two clicks: a name shows or hides its window, a header points
        // the reading at it.
        match hit.part {
            Part::Header => self.clicked = Some(sticky_line(&hit.name)),
            Part::List => {
                let shown = self
                    .screen
                    .panes
                    .get(&hit.name)
                    .is_some_and(|pane| pane.kind == crate::panes::Kind::Shown);
                self.clicked = Some(toggle_line(&hit.name, shown));
            }
            // A press in the events opens or closes SPEC's `abstract` there.
            Part::Pane => {
                if let Some(at) =
                    self.screen
                        .item_at(size.width, size.height, mouse.column, mouse.row)
                {
                    self.screen.panes.toggle_at(at);
                }
            }
            _ => {}
        }
    }

    /// The window a line of this front-end's own belongs to.
    pub fn command_window(&self) -> String {
        self.screen
            .panes
            .command()
            .map(|pane| pane.name.clone())
            .unwrap_or_default()
    }

    /// Rows a wheel event scrolls, positive into the past; `None` for anything else.
    fn wheel_rows(mouse: &MouseEvent) -> Option<i64> {
        let rows: i64 = if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            1
        } else {
            3
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => Some(rows),
            MouseEventKind::ScrollDown => Some(-rows),
            _ => None,
        }
    }

    /// What one page-up should move: the rows of the pane being scrolled.
    fn page_height(&mut self) -> usize {
        let Ok(size) = self.terminal.size() else {
            return 1;
        };
        self.screen.page_height(size.width, size.height)
    }

    /// A paste is text, never a keystroke replay: a newline inside it must not
    /// press Enter.
    fn paste(&mut self, raw: &str) {
        let text = clipboard::paste_text(raw);
        if text.is_empty() {
            return;
        }
        self.screen.insert_paste(&text);
        self.dirty = true;
    }

    /// Alt+V: the clipboard, when the terminal did not paste it for us.
    ///
    /// # Invariant
    ///
    /// The image is tried first and the text second: a screenshot copied from an
    /// image tool can carry both formats, and the picture is what the operator
    /// meant. A clipboard that holds only text reaches the text branch untouched,
    /// and a clipboard this machine cannot read says so on the status line --
    /// nothing else about the window moves.
    fn paste_from_clipboard(&mut self) {
        let image = clipboard::paste_image();
        // Only a saved picture is a reference; the rest falls through to text.
        if let Some(reference) = image_reference_of(&image) {
            self.screen.insert_paste(&reference);
            self.dirty = true;
            return;
        }
        // The image tool's own words, kept in case the text read has none.
        let image_failure = image_failure_of(&image);
        match clipboard::paste() {
            clipboard::FromClipboard::Ok(text) => self.paste(&text),
            clipboard::FromClipboard::Empty => {
                // An empty clipboard is silence, unless the image read refused:
                // then the refusal is the only thing there is to say.
                if let Some(why) = image_failure {
                    self.screen.note(LineKind::Error, format!("paste: {why}"));
                }
            }
            clipboard::FromClipboard::NoTool => self.screen.note(
                LineKind::Error,
                "paste: no clipboard tool here; the terminal's own paste still works",
            ),
            clipboard::FromClipboard::Failed(why) => {
                self.screen.note(LineKind::Error, format!("paste: {why}"))
            }
        }
        self.dirty = true;
    }

    /// Alt+Y copies what is on screen; Alt+M hands the mouse back to the terminal.
    fn console(&mut self, action: ConsoleAction) {
        match action {
            ConsoleAction::Copy => self.copy_visible(),
            ConsoleAction::Mouse => self.toggle_mouse(),
        }
        self.dirty = true;
    }

    /// Copies the rows the operator can see, which is what "copy that" means.
    fn copy_visible(&mut self) {
        let Ok(size) = self.terminal.size() else {
            self.say(LineKind::Error, "copy: the terminal has no size to read");
            return;
        };
        let text = self.screen.visible_rows(size.width, size.height).join("\n");
        match clipboard::copy(&text) {
            clipboard::Copied::Ok { chars, tool } => self.say(
                LineKind::System,
                format!("copied {chars} characters ({tool})"),
            ),
            clipboard::Copied::Empty => self.say(LineKind::System, "copy: the window is empty"),
            clipboard::Copied::NoTool => self.say(
                LineKind::Error,
                "copy: no clipboard tool here; Alt+M frees the mouse so the terminal can select",
            ),
            clipboard::Copied::Failed(why) => {
                self.say(LineKind::Error, format!("copy failed: {why}"))
            }
        }
    }

    /// Toggles terminal mouse capture, so selection and the wheel trade places.
    ///
    /// # Invariant
    ///
    /// Releasing capture is what makes native copy possible: while the app holds
    /// the mouse, the terminal never sees a drag and cannot select.
    fn toggle_mouse(&mut self) {
        self.mouse_captured = !self.mouse_captured;
        let result = if self.mouse_captured {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)
        };
        match result {
            Ok(()) if self.mouse_captured => self.say(
                LineKind::System,
                "mouse: the app scrolls (Alt+M releases it for selecting)",
            ),
            Ok(()) => self.say(
                LineKind::System,
                "mouse: the terminal selects; drag to copy, Alt+M gives scrolling back",
            ),
            Err(e) => {
                // The flag must match reality, or the next toggle lies.
                self.mouse_captured = !self.mouse_captured;
                self.say(LineKind::Error, format!("mouse: {e}"));
            }
        }
    }

    fn newline(&mut self) {
        self.screen.input.insert('\n');
        // The popup was about the token before the break, not the one after it.
        self.screen.completion = None;
    }

    /// Re-asks what the line wants, after something the answer depends on changed.
    ///
    /// # Invariant
    ///
    /// Marks the window dirty: an answer arriving while nothing is being typed is
    /// the only thing that changes the popup then, and nothing else would ask for
    /// the frame that shows it.
    pub fn refresh_completion(&mut self) {
        self.screen.completion = next_completion(&self.screen.input, &self.completer);
        self.dirty = true;
    }

    fn close_completion(&mut self) {
        self.screen.completion = None;
    }

    fn apply_completion(&mut self) {
        // Ask again right after applying, so Tab twice fills a flag and then its value.
        apply_and_continue(
            &mut self.screen.input,
            &mut self.screen.completion,
            &self.completer,
        );
    }

    /// Shift+↑: the last command line back into the prompt, and the one before
    /// it on the next press.
    ///
    /// # Invariant
    ///
    /// Never a bare ↑: the arrow belongs to the prompt's own lines, and a
    /// one-line prompt has no line above, so that key does nothing there.
    /// Recall walks one way and stops at the oldest line; nothing here goes
    /// forward again. See `docs/notes/tui-lib.md`.
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        let Some(line) = self.history.get(pos) else {
            return;
        };
        self.history_pos = Some(pos);
        self.screen.input.clear();
        self.screen.input.insert_str(line);
    }

    /// Puts the terminal back. Idempotent, and also run on drop.
    pub fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        let restored = &mut self.restored;
        let terminal = &mut self.terminal;
        Ok(restore_once(restored, || {
            restore_terminal(terminal.backend_mut())
        })?)
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // A panicking caller must not leave the terminal in raw mode.
        let _ = self.restore();
    }
}

/// The command line a click on a session asks for.
///
/// # Invariant
///
/// It starts with `/`: a line without one is prose, and prose goes to the agent.
/// A click that sent "set_sticky --name session --value peer" as a message was
/// exactly the bug this spelling exists to prevent. See `docs/notes/tui-lib.md`.
fn sticky_line(session: &str) -> String {
    format!("/set_sticky --name session --value {session}")
}

/// What a click on a name asks for: its window, the other way round.
///
/// # Invariant
///
/// "The other way round" is what a click means, and what the window knows is what
/// it draws: a session with a band beside the list is shown, one without it is
/// hidden. SPEC hangs the sticky session off the same click, and the client
/// command does that. See `docs/notes/tui-lib.md`.
fn toggle_line(session: &str, shown: bool) -> String {
    format!("/set_render_session --session {session} --show {}", !shown)
}

/// The command line Shift+Delete asks for: the sticky session's own deletion.
fn delete_line(session: &str) -> String {
    format!("/del_session --session {session}")
}

/// Paints `screen` and leaves the console caret on the prompt.
///
/// # Invariant
///
/// Every painted frame ends with the caret anchored and nothing is hidden: a
/// frame that writes the events area must not leave the console caret there, and
/// a caret that blinks on every repaint is what an IME follows. See
/// `docs/notes/tui-lib.md`.
fn present<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    screen: &mut Screen,
    typed: bool,
) -> anyhow::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal.autoresize()?;
    let input = {
        // The renderer writes the scroll bound back, hence `&mut`.
        let mut frame = terminal.get_frame();
        render::draw(&mut frame, screen)
    };
    terminal.flush()?;
    terminal.swap_buffers();
    // After typing, what the row drew blank is said again: an IME writes into
    // the console, which the diff cannot see.
    if typed {
        for (y, x) in render::input_ends(screen, input) {
            terminal.set_cursor_position((x, y))?;
            ratatui::backend::Backend::clear_region(
                terminal.backend_mut(),
                ClearType::UntilNewLine,
            )?;
        }
    }
    terminal.set_cursor_position(render::caret(screen, input))?;
    terminal.backend_mut().flush()?;
    Ok(())
}

/// Runs one frame, with a panic in it caught rather than thrown at the window.
///
/// # Invariant
///
/// Drawing happens on every event, so a bug in it may not end the session: the frame
/// is abandoned, the status row says what broke, and the next event paints again. The
/// state a frame touches is owned by this window and rewritten whole next time, which
/// is what makes it sound to carry on. See `docs/notes/tui-lib.md`.
fn guarded<T>(frame: impl FnOnce() -> T) -> std::thread::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(frame))
}

/// What a caught panic said, for a row an operator can read.
fn frame_failed(payload: &(dyn std::any::Any + Send)) -> String {
    let said = match payload.downcast_ref::<&str>() {
        Some(said) => said.to_string(),
        None => match payload.downcast_ref::<String>() {
            Some(said) => said.clone(),
            None => "the frame panicked".to_string(),
        },
    };
    format!("frame: {}", said.replace('\n', " "))
}

fn restore_terminal(out: &mut impl Write) -> io::Result<()> {
    // Evaluate every step even when an earlier one fails; each execute! flushes separately.
    restore_steps([
        disable_raw_mode(),
        execute!(out, LeaveAlternateScreen),
        execute!(out, DisableBracketedPaste),
        // Leaving capture on would keep the wheel from the shell we return to.
        execute!(out, DisableMouseCapture),
        execute!(out, Show),
    ])
}

fn restore_steps(results: impl IntoIterator<Item = io::Result<()>>) -> io::Result<()> {
    let mut first_error = None;
    for result in results {
        if let Err(error) = result {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn restore_once(restored: &mut bool, restore: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    if *restored {
        return Ok(());
    }
    restore()?;
    *restored = true;
    Ok(())
}

/// Editing or moving invalidates the popup's token range, even if the key is a no-op.
fn edit_input(input: &mut InputLine, completion: &mut Option<Completion>, key: KeyCode) -> bool {
    if !matches!(
        key,
        KeyCode::Delete | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
    ) {
        return false;
    }
    *completion = None;
    match key {
        KeyCode::Delete => input.delete(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        _ => unreachable!(),
    }
    true
}

/// The completer's answer for this input as a popup; bounds share its split.
fn next_completion(input: &InputLine, completer: &SharedCompleter) -> Option<Completion> {
    let candidates = completer.complete(input.text(), input.cursor());
    if candidates.is_empty() {
        return None;
    }
    let (start, end, _) = token_bounds(input.text(), input.cursor());
    Some(Completion {
        start,
        end,
        candidates,
        selected: 0,
    })
}

/// Applies the chosen candidate, then asks what the line wants next. See `docs/notes/tui-lib.md`.
fn apply_and_continue(
    input: &mut InputLine,
    completion: &mut Option<Completion>,
    completer: &SharedCompleter,
) {
    let Some(chosen) = completion.take() else {
        return;
    };
    apply_completion(input, &chosen);
    *completion = next_completion(input, completer);
}

/// A completer that always declines. Handy for tests and for headless modes.
pub struct NoCompleter;

impl Completer for NoCompleter {
    fn complete(&self, _line: &str, _cursor: usize) -> Vec<Candidate> {
        Vec::new()
    }
}

/// The reference the prompt shows for a pasted picture: `@image: <path>`.
///
/// # Invariant
///
/// The path is inserted verbatim and the reference is exactly the spelling the
/// server's Meta parser reads (`@image: <one word>`), so the server replaces it
/// with a file id when the message is sent. The generated file name has no
/// spaces, so the value is one word.
fn image_reference(path: &Path) -> String {
    format!("@image: {}", path.display())
}

/// The reference a clipboard's picture becomes, or `None` when the clipboard
/// holds no picture the prompt should show.
///
/// # Invariant
///
/// Only a saved picture is a reference. `NoImage` means the clipboard holds no
/// picture, `NoTool` means this machine has no way to read one, and `Failed`
/// means a tool refused -- all three leave the text branch to run, which is
/// where the reporting lives.
fn image_reference_of(image: &clipboard::ImageClipboard) -> Option<String> {
    match image {
        clipboard::ImageClipboard::Saved(path) => Some(image_reference(path)),
        clipboard::ImageClipboard::NoImage
        | clipboard::ImageClipboard::NoTool
        | clipboard::ImageClipboard::Failed(_) => None,
    }
}

/// What a refused image read said, or `None` when it had nothing to report.
///
/// # Invariant
///
/// Only a tool that ran and refused has words; the other three answers are not
/// failures and stay silent.
fn image_failure_of(image: &clipboard::ImageClipboard) -> Option<String> {
    match image {
        clipboard::ImageClipboard::Failed(why) => Some(why.clone()),
        _ => None,
    }
}

/// Whether this run is one bare `Enter` that a recent paste delivered.
fn glue_break(keys: &[KeyEvent], pasted_run: Option<usize>) -> bool {
    let lone_break = keys.len() == 1 && is_paste_newline(&keys[0]);
    // A modified Enter settles it alone: no hand sends with it, and the timing
    // rule cannot see a ConPTY paste.
    let modified = keys.len() == 1 && keys[0].modifiers.contains(KeyModifiers::CONTROL);
    lone_break && (modified || pasted_run.is_some_and(|chars| chars >= MACHINE_RUN))
}

/// Whether this Enter is a paste's line break rather than the operator sending.
///
/// # Invariant
///
/// A paste's newline is not a plain Enter. Windows delivers it as `Enter` with
/// `CONTROL` -- seen in a real terminal, not guessed -- so a rule that accepts only
/// the bare key loses every line break in a pasted block, and one that sends on it
/// posts the block a line at a time. Shift and Alt are the operator's own breaks
/// and are excluded. See `docs/notes/tui-paste.md`.
fn is_paste_newline(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
}

fn pasted(keys: &[KeyEvent]) -> Option<String> {
    // A lone key is what a keyboard sends; a paste is a break among other keys.
    if keys.len() < 2 || !keys.iter().any(|k| k.code == KeyCode::Enter) {
        return None;
    }
    let mut text = String::new();
    for key in keys {
        match key.code {
            KeyCode::Enter if is_paste_newline(key) => text.push('\n'),
            KeyCode::Char(c) => text.push(c),
            // Anything else means this was not one pasted block after all.
            _ => return None,
        }
    }
    Some(text)
}
