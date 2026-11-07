//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `link`, never the reverse and never on `cli`: the command
//! surface and the wire stay usable from a script, and a new command never
//! changes this crate.

mod cache;
mod clipboard;
mod command;
mod cover;
mod fold;
mod input;
mod log;
mod markdown;
mod notice;
mod panes;
pub mod program;
mod render;
mod roster;
pub mod run;
mod state;
mod table;
mod tree;

use std::io::{self, Stdout, Write, stdout};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
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
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};

pub use crate::input::{InputLine, apply_completion};
pub use crate::log::Log;
pub use crate::panes::Panes;
pub use crate::run::interactive;

/// The modifiers a terminal forwards with a mouse event, as a gesture's own.
///
/// # Invariant
///
/// All three, because no one of them is enough: Shift is what a person reaches
/// for and what xterm, and so VS Code's terminal, takes for text selection --
/// that press never reaches the application -- while Alt and Ctrl are what it
/// does forward. One predicate, so the click and the wheel cannot disagree about
/// which modifiers exist. See `docs/notes/tui-lib.md`.
fn gesture_modifiers() -> KeyModifiers {
    KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL
}

/// Runs the window: its own runtime, because the loop owns every task in it.
pub fn main(args: link::Args) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(interactive(&args))
}

// The vocabulary is the command layer's, re-exported so this crate can name it.
pub use link::{
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
    /// Alt+C: put the selected text on the clipboard.
    CopySelection,
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
    /// A gesture of the window's own: an operation, never a command line.
    ///
    /// # Invariant
    ///
    /// The window asks for what it means -- "make this session sticky" -- and
    /// the command layer answers it. Spelling it as a command line would put
    /// the command syntax in the window, where it does not belong.
    Operate(link::protocol::Operation),
    /// Alt+Q: leave.
    Quit,
    /// The operator asked to read above what these windows hold.
    ///
    /// # Invariant
    ///
    /// Named, never searched for: the request is what the key or the wheel said at
    /// the moment it was pressed, so the loop that answers it holds the pages owed
    /// and the next frame cannot add to them behind its back.
    Older(Vec<String>),
    /// Nothing to submit: the completion asked for something only the connection
    /// can answer.
    ///
    /// # Invariant
    ///
    /// The loop has to go round to send it, and a keystroke whose ask is never
    /// returned leaves it unsent: the loop owns the connection and runs only when
    /// `next_event` returns.
    Recomplete,
}

/// Which part of the window a pointer landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The session list, which scrolls its own rows.
    List,
    /// The two columns that fold one row of the list away.
    Fold,
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
/// A server's lifetime belongs to whoever *started* it, never to whoever
/// connects: dialling is reading. Two facts make one ours -- the pid `spawn`
/// answered, and the pid the server follows (`--parent`) being this window's,
/// which is how a handover keeps the lifetime it had. Anything else outlives the
/// window. See `docs/notes/tui-run.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// This window started it, so it goes when the window does.
    Ours { pid: u32 },
    /// A handover of this window's server: a newer build in its place, carrying
    /// the same `--parent`, so it goes too.
    Handover { pid: u32 },
    /// Another window started it: this one reads it and cannot stop it.
    Another { pid: u32 },
    /// Nobody started it for a window: it outlives every window that reads it.
    Unowned { pid: u32 },
}

impl Server {
    /// Whether closing this window stops this server.
    ///
    /// # Invariant
    ///
    /// One answer for two readers: the header draws it and the way out acts on
    /// it, so the bar cannot promise a lifetime the window does not honour.
    pub fn is_ours(self) -> bool {
        matches!(self, Server::Ours { .. } | Server::Handover { .. })
    }

    /// The words the header shows: whether it is own, why, and then its pid.
    pub fn label(&self) -> String {
        match self {
            Server::Ours { pid } => format!("own #{pid}"),
            Server::Handover { pid } => format!("own #{pid} (handover)"),
            Server::Another { pid } => format!("not own #{pid} (another window)"),
            Server::Unowned { pid } => format!("not own #{pid} (no owner)"),
        }
    }
}

/// Everything the renderer reads, separate from the terminal it draws to.
pub struct Screen {
    /// Present until the first input; taking it releases the decoded pixels.
    cover: Option<cover::Cover>,
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
    /// Ends the cover and says whether it was showing.
    fn dismiss_cover(&mut self) -> bool {
        self.cover.take().is_some()
    }

    /// A note about this front-end, on the status line.
    ///
    /// # Invariant
    ///
    /// The status line shows the newest and replaces the one before it; the journal
    /// keeps them all, which is what `Tui::say` writes. Neither is a session's
    /// lines: "nothing is listening" is not something a session said.
    /// See `docs/notes/tui-lib.md`.
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

    /// The windows whose operator asked to read above what they had.
    pub fn take_wants(&mut self) -> Vec<String> {
        self.panes.take_wants()
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
        if let Some(hit) = render::list_at(list, &self.panes, column, row) {
            // The fold target folds; the name beside it does what a name does.
            let part = match hit.fold {
                true => Part::Fold,
                false => Part::List,
            };
            return Some(Hit {
                name: hit.name,
                part,
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
            Part::List | Part::Fold => self.panes.scroll_list_by(rows),
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
    /// What the backend was last told, so only the cover transition changes it.
    cursor_hidden: bool,
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
    clicked: Option<link::protocol::Operation>,
    /// The input box as the last frame drew it: a click has to find it.
    input_area: Rect,
    /// Where a drag began, while the button is still down.
    dragging: Option<usize>,
    /// Answers the loop has not read yet: one pair per `cmd_result`.
    ///
    /// # Invariant
    ///
    /// A queue the sink fills and the loop drains, because the outcome arrives
    /// inside a frame the command layer is already parsing: what the window keeps
    /// for an accepted command is the window's, and this is how it hears.
    answers: Vec<(u64, bool)>,
    /// The session list this window drew last, and the file it lives in.
    ///
    /// # Invariant
    ///
    /// Written by the frames that drew a list and read once, before the first dial:
    /// a window renders its own state, so a list it had is a list it can draw. The
    /// failure to record one is said once and then left alone -- a window that
    /// cannot write still draws. See `docs/notes/tui-roster.md`.
    roster: roster::Roster,
    roster_failed: bool,
    /// The window a note about this front-end is written down in.
    ///
    /// # Invariant
    ///
    /// The command window's own name, off the layout, so a note and the row it
    /// explains share one journal: the server decides that name (a session may take
    /// `cmd`), so the link's answer is the only one that can be right.
    /// See `docs/notes/tui-cache.md`.
    note_window: String,
}

/// The terminal's own answer about the pictures it can be drawn with.
///
/// # Invariant
///
/// Sixel and kitty are answers to a query and are taken. iTerm2 is a variable the
/// crate read, and its list includes `vscode`, which paints no picture: an iTerm2
/// escape there costs the whole picture. The cell geometry is checked the same way --
/// `Cover::rect` counts the picture in cells from it. See `docs/notes/tui-cover.md`.
fn picker() -> Picker {
    let queried = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if !paints_what_it_named(&queried) {
        return Picker::halfblocks();
    }
    if cell_shaped(queried.font_size()) {
        return queried;
    }
    let mut fallback = Picker::halfblocks();
    fallback.set_protocol_type(queried.protocol_type());
    fallback
}

/// Whether the protocol this picker named is one the terminal really paints.
///
/// # Invariant
///
/// Measured twice: sixel and kitty are the terminal's answers to a question, so they
/// are taken. iTerm2 is only a variable the crate read, and the terminals it reads it
/// off do not all paint one -- so this asks the variable again, for the name that
/// protocol belongs to.
fn paints_what_it_named(picker: &Picker) -> bool {
    match picker.protocol_type() {
        ProtocolType::Iterm2 => names_iterm2(),
        ProtocolType::Halfblocks | ProtocolType::Sixel | ProtocolType::Kitty => true,
    }
}

/// Whether this is a terminal whose own picture protocol is iTerm2's.
///
/// # Invariant
///
/// The two that paint one: iTerm2 itself, and WezTerm, which speaks the protocol on
/// purpose. The crate's list also counts `vscode`, `mintty`, `Tabby`, `Hyper`, `rio`,
/// `Bobcat` and `WarpTerminal`, and those are Electron shells and Windows consoles
/// that carry cells and nothing else. See `docs/notes/tui-cover.md`.
fn names_iterm2() -> bool {
    let paints = |value: &str| value.contains("iTerm") || value.contains("WezTerm");
    std::env::var("TERM_PROGRAM").is_ok_and(|program| paints(&program))
        || std::env::var("LC_TERMINAL").is_ok_and(|terminal| paints(&terminal))
}

/// Whether this measures a character cell: a few pixels across, about twice as tall.
fn cell_shaped(size: FontSize) -> bool {
    (1..=32).contains(&size.width) && (1..=64).contains(&size.height) && size.height >= size.width
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
            // Asked once, before the event stream takes the stdin the answer arrives on.
            let cover = cover::Cover::new(picker()).context("decoding the embedded cover")?;
            anyhow::Ok((terminal, cover))
        })();
        let (terminal, cover) = match initialized {
            Ok(initialized) => initialized,
            Err(error) => {
                let _ = restore_terminal(&mut stdout());
                return Err(error);
            }
        };
        let mut window = Self {
            terminal,
            cursor_hidden: false,
            screen: Screen {
                cover: Some(cover),
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
            input_area: Rect::default(),
            dragging: None,
            answers: Vec::new(),
            roster: roster::Roster::new(client_home, endpoint),
            roster_failed: false,
            note_window: link::COMMAND_WINDOW.to_string(),
            events: EventStream::new(),
            mouse_captured: true,
            restored: false,
            // The first frame has to happen even though nothing has changed.
            dirty: true,
            pending: Vec::new(),
            settled: None,
            stashed: None,
        };
        // Dialing can win the first select, so the cover cannot wait for next_event.
        window.draw()?;
        Ok(window)
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Sets the status text, asking for a frame only when it actually changed.
    ///
    /// # Invariant
    ///
    /// A change is a record as well as a line: what the window said it is doing is
    /// what a reader compares a later silence with. Unchanged text writes nothing,
    /// so a refresh that says the same thing costs no record.
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        if status != self.screen.status {
            self.screen
                .panes
                .note(&self.note_window, LineKind::System, &status);
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
        self.roster.repoint(client_home, endpoint);
        self.dirty = true;
    }

    /// The session list this window drew last, for a start that has no link yet.
    pub fn known_list(&self) -> Vec<link::Listed> {
        self.roster.load()
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
        if !layout.command.is_empty() {
            self.note_window = layout.command.clone();
        }
        self.screen.panes.layout(layout);
        // One file, written by the frames that drew a list.
        if let Err(e) = self.roster.know(&layout.listed)
            && !self.roster_failed
        {
            self.roster_failed = true;
            let note = format!("this window's session list could not be recorded: {e}");
            self.say(LineKind::Error, note);
        }
        self.dirty = true;
    }

    /// Files one command's outcome for the loop.
    pub fn settled(&mut self, id: u64, ok: bool) {
        self.answers.push((id, ok));
    }

    /// Takes every outcome filed since the last call.
    pub fn take_settled(&mut self) -> Vec<(u64, bool)> {
        std::mem::take(&mut self.answers)
    }

    /// A session stopped: its turn ended, and the window says so.
    ///
    /// # Invariant
    ///
    /// The toast is outside the window, and the mark is inside it: a session that
    /// stops while it is not drawn is the one the operator would not notice. A turn
    /// somebody cancelled never reaches here -- the command layer does not announce
    /// a stop that was asked for. See `docs/notes/tui-lib.md`.
    pub fn stopped(&mut self, session: &str, failed: bool) {
        if !self.screen.panes.is_shown(session) {
            self.screen.panes.mark_unseen(session);
        }
        let body = if failed {
            "the turn failed"
        } else {
            "the turn ended"
        };
        if let Err(e) = crate::notice::toast(session, body) {
            self.say(
                LineKind::Error,
                format!("the toast could not be shown: {e}"),
            );
        }
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
        let text = text.into();
        // A record whether or not a row draws it: the line keeps the newest.
        self.screen.panes.note(&self.note_window, kind, &text);
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

    /// Writes down that this window has drawn every row up to `seq` of the
    /// session whose `created_at` is `created`.
    ///
    /// # Invariant
    ///
    /// The front-end's half of the sync stamp, and the only thing this process
    /// knows that the server does not. See `docs/notes/tui-cache.md`.
    pub fn stamp(&mut self, name: &str, created: i64, seq: i64) {
        self.screen.panes.stamp(name, created, seq);
    }

    /// What this window's journal says it has drawn, and of which session.
    pub fn stamp_of(&self, name: &str) -> Option<(i64, i64)> {
        self.screen.panes.stamp_of(name)
    }

    /// Throws away what no stored row accounts for: the server is about to send
    /// this window's tail again.
    pub fn rewind(&mut self, name: &str) {
        self.screen.panes.rewind(name);
        self.dirty = true;
    }

    /// The oldest transcript row this window's journal says it holds.
    pub fn floor_of(&self, name: &str) -> Option<i64> {
        self.screen.panes.floor_of(name)
    }

    /// The rows arriving from here on are older than the ones this window holds.
    pub fn older(&mut self, name: &str) {
        self.screen.panes.older(name);
        self.dirty = true;
    }

    /// A page of older rows landed: this window holds every row from `floor` up.
    pub fn landed(&mut self, name: &str, created: i64, drawn: i64, floor: i64) {
        self.screen.panes.landed(name, created, drawn, floor);
        self.dirty = true;
    }

    /// The windows whose operator asked to read above what they had.
    ///
    /// # Invariant
    ///
    /// Taken, not read: each request is one page, so the one who answers it holds
    /// it from here on and the window does not keep asking on its behalf.
    pub fn take_wants(&mut self) -> Vec<String> {
        self.screen.take_wants()
    }

    /// The rows a window's pane could draw last frame: what one page is.
    pub fn room_of(&self, name: &str) -> usize {
        self.screen.panes.room_of(name)
    }

    /// The windows a frame drew last: a window nobody draws costs nothing.
    pub fn drawn(&self) -> &[String] {
        self.screen.panes.drawn()
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
    pub fn describe(&mut self, name: &str, key: &str, head: &link::protocol::Head) {
        self.screen.panes.describe(name, key, head);
        self.dirty = true;
    }

    /// The label of a block that learned its outcome after it opened.
    pub fn relabel(&mut self, _name: &str, key: &str, tag: &str, kind: LineKind) {
        self.screen.panes.relabel(key, tag, kind);
        self.dirty = true;
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        // Nothing changed: nothing is written. The cover is the one exception.
        // See `docs/notes/tui-cover.md`.
        if !self.dirty && !self.cover_awaiting_size() {
            return Ok(());
        }
        let seen = (self.screen.input.text().len(), self.screen.input.cursor());
        let typed = self.input_seen != Some(seen);
        self.input_seen = Some(seen);
        let frame = guarded(|| {
            present(
                &mut self.terminal,
                &mut self.screen,
                &mut self.cursor_hidden,
                typed,
            )
        });
        match frame {
            Ok(Ok(area)) => self.input_area = area,
            Ok(Err(error)) => return Err(error),
            // Abandoned, not fatal: the screen is cleared, and the row says why.
            Err(payload) => {
                let _ = self.terminal.clear();
                self.say(LineKind::Error, frame_failed(&payload));
            }
        }
        // Cleared only after the frame landed, so a failed paint is retried.
        self.dirty = false;
        Ok(())
    }

    /// Whether the cover is up and has not been drawn for this terminal size.
    ///
    /// # Invariant
    ///
    /// The first frame is drawn before any event, when the terminal may still be
    /// reporting the size it had a moment ago: a cover painted into an empty area
    /// draws nothing, and nothing else asks for a second one. See
    /// `docs/notes/tui-cover.md`.
    fn cover_awaiting_size(&self) -> bool {
        let Some(cover) = &self.screen.cover else {
            return false;
        };
        let Ok(size) = self.terminal.size() else {
            return false;
        };
        cover.painted() != Some((size.width, size.height))
    }

    /// Waits for the operator, painting a frame before every wait.
    pub async fn next_event(&mut self) -> Option<UiEvent> {
        loop {
            // A click is a command, and commands go through the loop that already
            // knows how to send one.
            if let Some(operation) = self.clicked.take() {
                return Some(UiEvent::Operate(operation));
            }
            // Taken, not read: a request is answered once, never on every frame.
            let owed = self.screen.take_wants();
            if !owed.is_empty() {
                return Some(UiEvent::Older(owed));
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
                    // The cover consumes one gesture, except the window's quit shortcut.
                    if self.screen.dismiss_cover() {
                        self.dirty = true;
                        if let Some(action) = Self::shortcut(&key) {
                            return Some(action);
                        }
                        continue;
                    }
                    // Held until the pause that says whether this was a paste.
                    self.pending.push(key);
                }
                Ok(TermEvent::Mouse(mouse)) => {
                    // A click must not operate the window hidden under the cover.
                    if self.screen.dismiss_cover() {
                        self.dirty = true;
                        continue;
                    }
                    self.handle_mouse(mouse);
                }
                // One string, so a pasted block stays a block: see `paste`.
                Ok(TermEvent::Paste(text)) => {
                    self.screen.dismiss_cover();
                    self.paste(&text);
                }
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
        // which owns the connection.
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
            KeyCode::Char('c' | 'C') => Some(ConsoleAction::CopySelection),
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
    /// never be the same keystroke. No other modifier reaches here: an arrow
    /// means the same thing whatever the terminal sent with it.
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
                } else {
                    self.move_caret_row(up);
                }
            }
        }
    }

    /// Moves the caret one row of the prompt as the frame draws it.
    ///
    /// # Invariant
    ///
    /// The prompt is drawn in the rows `render::layout_input` cuts, and the
    /// caret moves through those rows: a long line wrapped over three of them is
    /// three rows to the arrows, not one. At the top or the bottom row there is
    /// nowhere to go, and the arrow does nothing.
    fn move_caret_row(&mut self, up: bool) {
        if let Some(at) = render::caret_row(&self.screen, self.input_area, up) {
            self.screen.input.set_cursor(at);
        }
    }

    /// One arrow key: the caret's, whatever else the terminal attached to it.
    ///
    /// # Invariant
    ///
    /// Only Shift changes what an arrow does, and only where SPEC says so. A
    /// terminal that sends Alt or Ctrl along with the arrows must not take the
    /// caret away from the operator. See `docs/notes/tui-lib.md`.
    fn handle_arrow(&mut self, key: KeyEvent) -> Option<UiEvent> {
        let word = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                if let Some(action) = Self::arrow_key(&key) {
                    self.arrow(action);
                }
            }
            KeyCode::Left => {
                // The caret moves, so the popup's token is stale: it goes.
                self.close_completion();
                if word {
                    self.screen.input.word_left();
                } else {
                    self.screen.input.left();
                }
            }
            KeyCode::Right => {
                self.close_completion();
                if word {
                    self.screen.input.word_right();
                } else {
                    self.screen.input.right();
                }
            }
            _ => {}
        }
        None
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

        // An arrow is the caret's: a modifier the terminal sends must not take it away.
        if matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
        ) {
            return self.handle_arrow(key);
        }

        // The other modifier bindings use Alt too: a terminal that eats Ctrl+K is common.
        if key.modifiers.contains(KeyModifiers::ALT) {
            if matches!(key.code, KeyCode::Char('k' | 'K')) {
                self.close_completion();
                self.screen.input.kill_to_end();
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
            return Some(UiEvent::Operate(link::protocol::Operation::DeleteSession {
                session: target,
            }));
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

    /// The mouse points at a pane: a notch scrolls it three rows, a held modifier
    /// one. See `docs/notes/tui-lib.md`.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // The input box has a mouse of its own: the caret, and what a drag selects.
        if self.input_mouse(&mouse) {
            return;
        }
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
            Part::Header => {
                self.clicked = Some(link::protocol::Operation::StickySession {
                    session: hit.name.clone(),
                })
            }
            // A row that names no session is only a group, and the only thing that
            // can happen to it is folding.
            Part::Fold => self.screen.panes.toggle_row(&hit.name),
            Part::List => {
                if !self.is_session_row(&hit.name) {
                    self.screen.panes.toggle_row(&hit.name);
                    return;
                }
                let shown = self.screen.panes.is_shown(&hit.name);
                // SPEC's click, the way a list selects: a plain one makes this
                // the only window; a modifier adds or removes it alone.
                let (show, solo) = match mouse.modifiers.intersects(gesture_modifiers()) {
                    true => (!shown, false),
                    false => (true, true),
                };
                self.clicked = Some(link::protocol::Operation::RenderSession {
                    session: hit.name.clone(),
                    show,
                    solo,
                });
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

    /// What the pointer did to the input box, if that is where it was.
    ///
    /// # Invariant
    ///
    /// The box answers for its own rows only: a click that misses it belongs to
    /// the window, and the session under the pointer keeps it.
    fn input_mouse(&mut self, mouse: &MouseEvent) -> bool {
        let area = self.input_area;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A press ends the drag that was before it, wherever that was.
                self.dragging = None;
                let Some(offset) =
                    render::input_offset(&self.screen, area, mouse.column, mouse.row)
                else {
                    return false;
                };
                // A click puts the caret where it landed, and ends the selection.
                self.screen.input.set_cursor(offset);
                self.close_completion();
                self.dragging = Some(offset);
                self.dirty = true;
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(anchor) = self.dragging else {
                    return false;
                };
                let Some(offset) = self.clamped_offset(area, mouse.column, mouse.row) else {
                    return false;
                };
                self.screen.input.select(anchor, offset);
                self.dirty = true;
                true
            }
            MouseEventKind::Up(_) => self.dragging.take().is_some(),
            // Any other button ends the drag as well: the button that was down
            // is not the one that is.
            MouseEventKind::Down(_) => {
                self.dragging = None;
                false
            }
            _ => false,
        }
    }

    /// Where a drag lands, with the pointer pulled back into the box.
    fn clamped_offset(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let row = row.clamp(area.y, area.bottom().saturating_sub(1));
        let column = column.clamp(area.x, area.right().saturating_sub(1));
        render::input_offset(&self.screen, area, column, row)
    }

    /// Whether this row of the list names a session, and not only the ones below it.
    fn is_session_row(&self, name: &str) -> bool {
        self.screen
            .panes
            .row_of(name)
            .is_some_and(|row| row.session)
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
        let rows: i64 = if mouse.modifiers.intersects(gesture_modifiers()) {
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
                    self.say(LineKind::Error, format!("paste: {why}"));
                }
            }
            clipboard::FromClipboard::NoTool => self.say(
                LineKind::Error,
                "paste: no clipboard tool here; the terminal's own paste still works",
            ),
            clipboard::FromClipboard::Failed(why) => {
                self.say(LineKind::Error, format!("paste: {why}"))
            }
        }
        self.dirty = true;
    }

    /// Alt+Y copies what is on screen, Alt+C the selection; Alt+M hands the
    /// mouse back to the terminal.
    fn console(&mut self, action: ConsoleAction) {
        match action {
            ConsoleAction::Copy => self.copy_visible(),
            ConsoleAction::CopySelection => self.copy_selection(),
            ConsoleAction::Mouse => self.toggle_mouse(),
        }
        self.dirty = true;
    }

    /// Copies the text the operator selected in the input box.
    fn copy_selection(&mut self) {
        let Some(chosen) = self.screen.input.selected_text().map(str::to_string) else {
            self.say(LineKind::System, "copy: nothing is selected");
            return;
        };
        match clipboard::copy(&chosen) {
            clipboard::Copied::Ok { chars, tool } => self.say(
                LineKind::System,
                format!("copied {chars} characters ({tool})"),
            ),
            clipboard::Copied::Empty => self.say(LineKind::System, "copy: the selection is empty"),
            clipboard::Copied::NoTool => self.say(
                LineKind::Error,
                "copy: no clipboard tool here; Alt+M frees the mouse so the terminal can select",
            ),
            clipboard::Copied::Failed(why) => {
                self.say(LineKind::Error, format!("copy failed: {why}"))
            }
        }
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
        let fresh = next_completion(&self.screen.input, &self.completer);
        // The row the operator moved to survives: an event is not a keystroke.
        self.screen.completion = match (self.screen.completion.take(), fresh) {
            (Some(old), Some(mut new)) if same_offering(&old, &new) => {
                new.selected = old.selected.min(new.candidates.len() - 1);
                Some(new)
            }
            (_, fresh) => fresh,
        };
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
    cursor_hidden: &mut bool,
    typed: bool,
) -> anyhow::Result<Rect>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal.autoresize()?;
    let showing_cover = screen.cover.is_some();
    let input = {
        // The renderer writes the scroll bound back, hence `&mut`.
        let mut frame = terminal.get_frame();
        if let Some(cover) = screen.cover.as_mut() {
            cover.draw(&mut frame);
            Rect::default()
        } else {
            render::draw(&mut frame, screen)
        }
    };
    terminal.flush()?;
    terminal.swap_buffers();
    if showing_cover {
        if !*cursor_hidden {
            terminal.hide_cursor()?;
            *cursor_hidden = true;
        }
    } else {
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
        if *cursor_hidden {
            terminal.show_cursor()?;
            *cursor_hidden = false;
        }
        terminal.set_cursor_position(render::caret(screen, input))?;
    }
    terminal.backend_mut().flush()?;
    Ok(input)
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
///
/// # Invariant
///
/// The arrows are not here: they move the caret too, but they answer to
/// `handle_arrow`, which owns the modifiers a terminal may attach to them.
fn edit_input(input: &mut InputLine, completion: &mut Option<Completion>, key: KeyCode) -> bool {
    if !matches!(key, KeyCode::Delete | KeyCode::Home | KeyCode::End) {
        return false;
    }
    *completion = None;
    match key {
        KeyCode::Delete => input.delete(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        _ => unreachable!(),
    }
    true
}

/// The completer's answer for this input as a popup; bounds share its split.
fn next_completion(input: &InputLine, completer: &SharedCompleter) -> Option<Completion> {
    let offered = completer.complete(input.text(), input.cursor());
    let candidates = offered.candidates;
    if candidates.is_empty() {
        return None;
    }
    let (start, end, _) = token_bounds(input.text(), input.cursor());
    Some(Completion {
        // A reference is replaced from its own `@`, which may sit inside the word.
        start: offered.start.unwrap_or(start),
        end,
        subject: offered.subject,
        candidates,
        selected: 0,
    })
}

/// Whether two offerings are the same question: the same token, the same answers.
///
/// # Invariant
///
/// The popup's own state is compared, not the line: a rebuild that offers the same
/// list in the same place keeps the row the operator chose, and one that offers
/// anything else starts over.
fn same_offering(old: &Completion, new: &Completion) -> bool {
    old.start == new.start
        && old.end == new.end
        && old.subject == new.subject
        && old.candidates.len() == new.candidates.len()
        && old
            .candidates
            .iter()
            .zip(&new.candidates)
            .all(|(a, b)| a.insert == b.insert && a.detail == b.detail)
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
    fn complete(&self, _line: &str, _cursor: usize) -> link::Offered {
        link::Offered::default()
    }
}

/// The reference the prompt shows for a pasted picture: `@image(<path>)`.
///
/// # Invariant
///
/// The path is inserted verbatim inside the group, and that is exactly the spelling
/// the server's Meta parser reads, so the server replaces the mention with a file id
/// when the message is sent. Nothing needs escaping: the parenthesis is what ends
/// the value, and a path holding a space is written like any other.
fn image_reference(path: &Path) -> String {
    format!(
        "@{}({})",
        link::protocol::Reference::Image.name(),
        path.display()
    )
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
