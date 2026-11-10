//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `client`, never the reverse and never on the command-line
//! program: the command surface and the wire stay usable from a script, and a
//! new command never changes this crate.

mod clipboard;
mod closing;
mod cover;
mod fold;
mod input;
mod log;
mod markdown;
mod notice;
mod panes;
pub mod pipe;
pub mod program;
mod render;
pub mod run;
mod table;
mod tree;
mod vt_input;

use std::io::{self, Stdout, Write, stdout};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use client::render_wire::UiIntent;
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
pub use client::runtime::Server;

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
    KeyModifiers::SHIFT | KeyModifiers::CONTROL
}

/// Runs the window: its own runtime, because the loop owns every task in it.
pub fn main(args: client::Args) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(pipe::run(&args))
}

// The vocabulary is the command layer's, re-exported so this crate can name it.
pub use client::{
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

/// Text keys back to back a hand cannot produce, so the `Enter` after them is the
/// paste's. See `docs/notes/tui-paste.md`.
const PASTE_BURST: usize = 20;

/// The gap two keys of a burst may have and still be one. See `docs/notes/tui-paste.md`.
const BURST_GAP: Duration = Duration::from_millis(40);

/// What Ctrl+C does to the terminal rather than to the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleAction {
    /// Ctrl+C: put the selected text on the clipboard.
    CopySelection,
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

/// What the terminal produced: an intent, or its own ask to go round once more.
///
/// # Invariant
///
/// `Recomplete` is not an intent: the completion needs the loop to reach the
/// connection, so it belongs to this crate and never crosses the wire.
/// See `docs/notes/tui-lib.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    Intent(UiIntent),
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
    /// The header's own / not own words, spelled by a client over the pipe.
    pub server_text: Option<String>,
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
    /// The status line shows the newest and replaces the one before it. Neither is
    /// a session's lines: "nothing is listening" is not something a session said.
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
    /// The console-record reader, opened only when the byte reader is off.
    events: Option<EventStream>,
    /// The byte reader, when this window reads the console as a stream.
    vt: Option<tokio::sync::mpsc::UnboundedReceiver<TermEvent>>,
    /// Whether mouse capture is on, so the toggle knows what to flip.
    restored: bool,
    /// Whether anything changed since the last frame. Every mutation sets it.
    dirty: bool,
    /// Keys seen so far that may be one paste. See `docs/notes/tui-paste.md`.
    pending: Vec<KeyEvent>,
    /// When the last run ended, and how many text keys it carried.
    settled: Option<(std::time::Instant, usize)>,
    /// The console's own paste gesture, off for this window's life. Held, never
    /// read: dropping it is what puts the mode back.
    #[allow(dead_code)]
    quick_edit: QuickEdit,
    /// Every event the terminal sent, when `GONGGA_TUI_DEBUG_KEYS` names a file.
    ///
    /// # Invariant
    ///
    /// A diagnostic, not a feature: off unless the variable is set, and it writes what
    /// arrived rather than what this window made of it -- the only way to tell a
    /// terminal that sends bracketed paste from one replaying keystrokes.
    key_log: Option<std::fs::File>,
    /// Bracketed paste's markers, while a paste is open or a marker is half read.
    markers: PasteMarkers,
    /// Text keys that arrived back to back, and when the newest one did.
    fast: usize,
    fast_at: Option<std::time::Instant>,
    /// An event read while deciding whether a keystroke began a paste.
    stashed: Option<std::io::Result<TermEvent>>,
    /// The input as the last frame saw it: typing may have put console ink on
    /// the row.
    input_seen: Option<(usize, usize)>,
    /// A click on a session, as the command line it asks for, waiting for the loop
    /// to take it. See `docs/notes/tui-lib.md`.
    clicked: Option<client::protocol::Operation>,
    /// The input box as the last frame drew it: a click has to find it.
    input_area: Rect,
    /// Where a drag began, while the button is still down.
    dragging: Option<usize>,
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

/// A vocabulary with no words: a pipe window asks its client for candidates.
struct EmptyCompleter;

impl Completer for EmptyCompleter {
    fn complete(&self, _line: &str, _cursor: usize) -> client::Offered {
        client::Offered::default()
    }
}

/// The terminal's own answer about the pictures it can be drawn with.
///
/// # Invariant
///
/// The environment decides first: it is the only evidence that describes the terminal
/// and not the pty under it, and the query adds what the environment leaves unnamed.
/// The measured cell is kept whatever the protocol becomes -- `Cover::rect` counts the
/// picture in cells from it. See `docs/notes/tui-cover.md`.
fn picker() -> Picker {
    let queried = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let pictured = named_protocol()
        .or_else(|| answered_pictures(&queried))
        .unwrap_or(ProtocolType::Halfblocks);
    if cell_shaped(queried.font_size()) {
        let mut picker = queried;
        picker.set_protocol_type(pictured);
        return picker;
    }
    let mut fallback = Picker::halfblocks();
    fallback.set_protocol_type(pictured);
    fallback
}

/// The protocol this environment names, including the ones that name cells only.
///
/// # Invariant
///
/// A name is the only evidence that describes the terminal rather than the pty under
/// it, so a named protocol is taken before the query is even consulted. Every row and
/// the reason for it are in `docs/notes/tui-cover.md`.
fn named_protocol() -> Option<ProtocolType> {
    named_protocol_where(|name| std::env::var(name).ok())
}

/// The same table read through `get`, so a test can hand it its own environment.
///
/// # Invariant
///
/// Ported from `supports-terminal-graphics`, with its `vscode` row changed: that row
/// says sixel from 1.80, and this terminal answers sixel and paints none. Terms are
/// lowercase, as that library reads them.
fn named_protocol_where(get: impl Fn(&str) -> Option<String>) -> Option<ProtocolType> {
    let value = |name: &str| get(name).unwrap_or_default().to_lowercase();
    let term = value("TERM");
    let program = value("TERM_PROGRAM");
    // Shells that draw their terminal in a DOM: cells, and no picture of their own.
    if matches!(
        program.as_str(),
        "vscode" | "tabby" | "hyper" | "bobcat" | "apple_terminal"
    ) {
        return Some(ProtocolType::Halfblocks);
    }
    let wezterm = get("WEZTERM_PANE").is_some()
        || get("WEZTERM_UNIX_SOCKET").is_some()
        || program == "wezterm"
        || value("LC_TERMINAL") == "wezterm";
    if wezterm
        || get("KITTY_WINDOW_ID").is_some()
        || get("KITTY_PID").is_some()
        || get("GHOSTTY_RESOURCES_DIR").is_some()
        || matches!(
            program.as_str(),
            "kitty" | "ghostty" | "rio" | "warpterminal"
        )
        || term.contains("kitty")
        || term == "xterm-ghostty"
    {
        return Some(ProtocolType::Kitty);
    }
    if matches!(program.as_str(), "iterm.app" | "mintty") || value("LC_TERMINAL") == "iterm2" {
        return Some(ProtocolType::Iterm2);
    }
    if get("KONSOLE_VERSION").is_some_and(|version| !version.is_empty())
        || term.starts_with("mlterm")
    {
        return Some(ProtocolType::Sixel);
    }
    // Sixel reached Windows Terminal in 1.22; before that it paints none of it.
    if program == "windows_terminal" {
        let after = at_least(&value("TERM_PROGRAM_VERSION"), 1, 22);
        return Some(match after {
            true => ProtocolType::Sixel,
            false => ProtocolType::Halfblocks,
        });
    }
    None
}

/// Whether a `major.minor.patch` string names a version at least `major.minor`.
fn at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    match (parts.next(), parts.next()) {
        (Some(found_major), Some(found_minor)) => (found_major, found_minor) >= (major, minor),
        _ => false,
    }
}

/// The protocol the terminal's own answer named, where that answer can be believed.
///
/// # Invariant
///
/// Sixel and kitty are answers to a question, and the names that lie about them are
/// settled before this runs. iTerm2 is only a variable the crate read, so it is never
/// taken here: the table is the one place a name decides.
fn answered_pictures(queried: &Picker) -> Option<ProtocolType> {
    match queried.protocol_type() {
        ProtocolType::Halfblocks | ProtocolType::Iterm2 => None,
        other => Some(other),
    }
}

/// Whether this measures a character cell: a few pixels across, about twice as tall.
fn cell_shaped(size: FontSize) -> bool {
    (1..=32).contains(&size.width) && (1..=64).contains(&size.height) && size.height >= size.width
}

/// Bracketed paste's markers, recognised in the stream of ordinary keys.
///
/// # Invariant
///
/// `ESC[200~` is what makes a paste exact -- every break between the markers is text.
/// Windows never turns it into `Event::Paste` (crossterm's console parser has no such
/// arm), so the marker arrives as six ordinary keys. Recognising that sequence is not
/// a guess: a hand does not type it, and while the markers are open nothing is sent.
/// See `docs/notes/tui-paste.md`.
const MARKER_START: [char; 6] = ['\x1b', '[', '2', '0', '0', '~'];
const MARKER_END: [char; 6] = ['\x1b', '[', '2', '0', '1', '~'];

/// What one key did to the marker recogniser.
enum Marker {
    /// Not a marker: these keys are the operator's, in order. Empty when the key was
    /// part of a marker or was taken as paste text.
    Keys(Vec<KeyEvent>),
    /// The end marker arrived: this is the whole paste.
    Pasted(String),
}

#[derive(Default)]
struct PasteMarkers {
    /// The keys that may be the start marker so far.
    probe: Vec<KeyEvent>,
    /// The keys that may be the end marker so far.
    tail: Vec<KeyEvent>,
    inside: bool,
    text: String,
}

impl PasteMarkers {
    fn feed(&mut self, key: KeyEvent) -> Marker {
        self.feed_all(vec![key])
    }

    fn feed_all(&mut self, keys: Vec<KeyEvent>) -> Marker {
        let mut queue: Vec<KeyEvent> = keys.into_iter().rev().collect();
        while let Some(key) = queue.pop() {
            if !self.inside {
                if token(&key).is_none() {
                    let mut out = std::mem::take(&mut self.probe);
                    out.push(key);
                    out.extend(queue.into_iter().rev());
                    return Marker::Keys(out);
                }
                self.probe.push(key);
                let seen: Vec<char> = self.probe.iter().filter_map(token).collect();
                if !MARKER_START.starts_with(&seen[..]) {
                    return Marker::Keys(std::mem::take(&mut self.probe));
                }
                if seen.len() == MARKER_START.len() {
                    self.probe.clear();
                    self.inside = true;
                    self.text.clear();
                    self.tail.clear();
                }
                continue;
            }
            self.tail.push(key);
            let seen: Vec<char> = self.tail.iter().filter_map(token).collect();
            if MARKER_END.starts_with(&seen[..]) {
                if seen.len() == MARKER_END.len() {
                    self.tail.clear();
                    self.inside = false;
                    return Marker::Pasted(std::mem::take(&mut self.text));
                }
                continue;
            }
            // Not the end marker after all: its first key was text.
            let mut rest = std::mem::take(&mut self.tail);
            let first = rest.remove(0);
            self.take(first);
            for key in rest.into_iter().rev() {
                queue.push(key);
            }
        }
        Marker::Keys(Vec::new())
    }

    /// The keys held as a possible marker, for a stream that stopped mid-way.
    ///
    /// # Invariant
    ///
    /// A lone `Esc` is the operator's: nothing is held past the quiet gap that ends a
    /// run, so a key that turned out not to be a marker is never lost behind one.
    fn take_probe(&mut self) -> Vec<KeyEvent> {
        std::mem::take(&mut self.probe)
    }

    /// One key as paste text: a break is a break, never a submit.
    fn take(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => self.text.push(c),
            KeyCode::Enter => self.text.push('\n'),
            KeyCode::Tab => self.text.push('\t'),
            KeyCode::Backspace => {
                self.text.pop();
            }
            _ => {}
        }
    }
}

/// One key as the character it would be inside a marker.
fn token(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Esc => Some('\x1b'),
        KeyCode::Char(c) if key.modifiers.is_empty() => Some(c),
        _ => None,
    }
}

/// The console's own QuickEdit, off for as long as this window is up.
///
/// # Invariant
///
/// With QuickEdit on, a right-click pastes the clipboard into the prompt as
/// keystrokes, and a pasted line break arrives as the operator's `Enter` -- the one
/// thing a paste rule cannot tell apart. Turning the mode off is not a heuristic: the
/// console stops delivering that gesture at all. See `docs/notes/tui-paste.md`.
#[cfg(windows)]
struct QuickEdit(Option<u32>);

#[cfg(windows)]
impl QuickEdit {
    /// Clears the mode, keeping the word it replaced so it can be put back.
    fn off() -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle,
            STD_INPUT_HANDLE, SetConsoleMode,
        };
        // SAFETY: `GetStdHandle` takes a constant, and the two mode calls take the
        // handle it returned plus a word this function owns.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                return Self(None);
            }
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return Self(None);
            }
            let quiet = (mode & !ENABLE_QUICK_EDIT_MODE) | ENABLE_EXTENDED_FLAGS;
            if SetConsoleMode(handle, quiet) == 0 {
                return Self(None);
            }
            Self(Some(mode))
        }
    }
}

#[cfg(windows)]
impl Drop for QuickEdit {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode};
        let Some(mode) = self.0 else {
            return;
        };
        // SAFETY: the same handle `off` changed, put back to the word it held.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if !handle.is_null() {
                SetConsoleMode(handle, mode);
            }
        }
    }
}

/// QuickEdit is a Windows console mode; elsewhere there is nothing to turn off.
#[cfg(not(windows))]
struct QuickEdit;

#[cfg(not(windows))]
impl QuickEdit {
    fn off() -> Self {
        Self
    }
}

impl Tui {
    /// A window that only draws: the client keeps the records and the words.
    pub fn for_pipe() -> anyhow::Result<Self> {
        Self::assemble()
    }

    fn assemble() -> anyhow::Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        // Before the window draws a prompt the console could paste into.
        let quick_edit = QuickEdit::off();
        // Before the alternate screen: the platform's thread must find it here.
        let watched = closing::watch();
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
                panes: Panes::new(),
                completion: None,
                input: InputLine::default(),
                status: "connecting".to_string(),
                status_kind: LineKind::System,
                server_text: None,
                placeholder: "message the agent, or / for a command (Tab completes)".to_string(),
                sticky_session: None,
            },
            completer: std::sync::Arc::new(EmptyCompleter),
            history: Vec::new(),
            history_pos: None,
            input_seen: None,
            clicked: None,
            input_area: Rect::default(),
            dragging: None,
            note_window: client::COMMAND_WINDOW.to_string(),
            events: (!vt_input::wanted()).then(EventStream::new),
            vt: vt_input::wanted().then(vt_input::spawn),
            restored: false,
            // The first frame has to happen even though nothing has changed.
            dirty: true,
            pending: Vec::new(),
            settled: None,
            markers: PasteMarkers::default(),
            key_log: std::env::var("GONGGA_TUI_DEBUG_KEYS")
                .ok()
                .and_then(|path| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .ok()
                }),
            quick_edit,
            fast: 0,
            fast_at: None,
            stashed: None,
        };
        // Said once: a window that cannot hear the console closing leaves
        // it in raw mode when it goes.
        if !watched {
            window.say(
                LineKind::Error,
                "no console handler: a window closed from outside will not be seen",
            );
        }
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
    /// What the window said it is doing is what a reader compares a later silence
    /// with. Unchanged text draws nothing, so a refresh that says the same thing
    /// costs no frame.
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        if status != self.screen.status {
            self.screen.status = status;
            self.dirty = true;
        }
    }

    /// The header's words, spelled by the client rather than a local `Server`.
    pub fn set_server_text(&mut self, label: Option<String>) {
        if self.screen.server_text != label {
            self.screen.server_text = label;
            self.dirty = true;
        }
    }

    /// Candidates the client computed from its own catalog.
    pub fn set_completion(&mut self, completion: Option<Completion>) {
        self.screen.completion = completion;
        self.dirty = true;
    }

    /// The line the operator is on, for a `Complete` intent.
    pub fn input_line(&self) -> (String, usize) {
        (
            self.screen.input.text().to_string(),
            self.screen.input.cursor(),
        )
    }

    /// Drops the rows on screen when the client moved to another endpoint.
    pub fn drop_windows(&mut self) {
        for name in self.screen.panes.windows() {
            self.screen.panes.forget(&name);
        }
        self.dirty = true;
    }

    /// What kind of thing the status line says, for its colour.
    pub fn set_status_kind(&mut self, kind: LineKind) {
        if self.screen.status_kind != kind {
            self.screen.status_kind = kind;
            self.dirty = true;
        }
    }

    /// How the events area is divided, as the command layer last said.
    pub fn set_layout(&mut self, layout: &Layout) {
        if !layout.command.is_empty() {
            self.note_window = layout.command.clone();
        }
        self.screen.panes.layout(layout);
        self.dirty = true;
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

    /// The windows whose operator asked to read above what they had.
    ///
    /// # Invariant
    ///
    /// Taken, not read: each request is one page, so the one who answers it holds
    /// it from here on and the window does not keep asking on its behalf.
    pub fn take_wants(&mut self) -> Vec<String> {
        self.screen.take_wants()
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
    pub fn describe(&mut self, name: &str, key: &str, head: &client::protocol::Head) {
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
                return Some(UiEvent::Intent(UiIntent::Operate { operation }));
            }
            // Taken, not read: a request is answered once, never on every frame.
            let owed = self.screen.take_wants();
            if !owed.is_empty() {
                return Some(UiEvent::Intent(UiIntent::Older { windows: owed }));
            }
            if self.draw().is_err() {
                return None;
            }
            // A run in progress is finished first: it lives in `self`, so a
            // cancelled wait resumes it instead of losing the keys.
            if !self.pending.is_empty() {
                match tokio::time::timeout(PASTE_GAP, self.next_term()).await {
                    // Quiet: the run is over, and a key held as a marker was not one.
                    Err(_) => {
                        for key in self.markers.take_probe() {
                            self.pending.push(key);
                        }
                    }
                    Ok(None) => return self.flush(),
                    Ok(Some(Err(e))) => self.stashed = Some(Err(e)),
                    Ok(Some(Ok(TermEvent::Key(key))))
                        if Self::actionable(&key) && Self::paste_key(&key) =>
                    {
                        match self.markers.feed(key) {
                            Marker::Keys(keys) => {
                                for key in keys {
                                    self.hold(key);
                                }
                                continue;
                            }
                            Marker::Pasted(text) => {
                                self.paste(&text);
                                continue;
                            }
                        }
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
                None => self.next_term().await?,
            };
            if let (Some(log), Ok(event)) = (self.key_log.as_mut(), &next) {
                use std::io::Write as _;
                let _ = writeln!(log, "{:?}", event);
                let _ = log.flush();
            }
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
                    match self.markers.feed(key) {
                        Marker::Keys(keys) => {
                            for key in keys {
                                self.hold(key);
                            }
                        }
                        Marker::Pasted(text) => self.paste(&text),
                    }
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

    /// Holds one key the loop may find to be part of a paste, counting a burst.
    ///
    /// # Invariant
    ///
    /// [`Self::fast`] counts text keys that arrived within [`PASTE_GAP`] of the one
    /// before, and goes back to zero at the first gap a hand could have made. It is
    /// what [`Self::burst_break`] reads, and it survives a flush: a slow replay is
    /// many runs of one key, and only the count across them says "this was a paste".
    fn hold(&mut self, key: KeyEvent) {
        let now = std::time::Instant::now();
        let fast = self
            .fast_at
            .is_some_and(|at| now.duration_since(at) <= BURST_GAP);
        self.fast = if fast { self.fast + 1 } else { 0 };
        self.fast_at = Some(now);
        self.pending.push(key);
    }

    /// Whether this lone break ends a burst no hand typed.
    ///
    /// # Invariant
    ///
    /// The count is spent when it answers, so one paste's break cannot excuse the
    /// `Enter` a person presses after it. See `docs/notes/tui-paste.md`.
    fn burst_break(&mut self, keys: &[KeyEvent]) -> bool {
        let lone = keys.len() == 1 && is_paste_newline(&keys[0]);
        let recent = self.fast_at.is_some_and(|at| at.elapsed() <= GLUE_GAP);
        let pasted = lone && recent && self.fast >= PASTE_BURST;
        if pasted {
            self.fast = 0;
        }
        pasted
    }

    /// The next terminal event, from whichever reader this window opened.
    async fn next_term(&mut self) -> Option<std::io::Result<TermEvent>> {
        match &mut self.vt {
            Some(vt) => vt.recv().await.map(Ok),
            None => match &mut self.events {
                Some(events) => events.next().await,
                None => None,
            },
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
            None if glue_break(&keys, run) || self.burst_break(&keys) => {
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

    /// The one key that is not editing: leaving, on Ctrl+Q. See `docs/notes/tui-lib.md`.
    ///
    /// # Invariant
    ///
    /// Ctrl, not Alt: the console's own gestures are off ([`QuickEdit`]), so the
    /// plain key reaches this window, and Alt is left for the terminal's own menu on
    /// the platforms that have one.
    fn shortcut(key: &KeyEvent) -> Option<UiEvent> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Case-insensitive: Shift+Ctrl+Q arrives as 'Q'.
            KeyCode::Char('q' | 'Q') if control => Some(UiEvent::Intent(UiIntent::Quit)),
            _ => None,
        }
    }

    /// The keys that act on the terminal, not on the conversation.
    fn console_key(key: &KeyEvent) -> Option<ConsoleAction> {
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        match key.code {
            KeyCode::Char('c' | 'C') => Some(ConsoleAction::CopySelection),
            _ => None,
        }
    }

    /// Whether this key asks for the clipboard: `Ctrl+V`.
    ///
    /// # Invariant
    ///
    /// The console's own paste was turned off ([`QuickEdit`]), so the key this window
    /// reads is the key the operator pressed rather than a replay the terminal made.
    /// A terminal that still keeps Ctrl+V for itself pastes the text instead, which
    /// reaches this window as a whole `Paste`; either way the clipboard is read once.
    fn clipboard_paste(key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::CONTROL)
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

        // Alt is the terminal's: no menu of this window's is on it, so its keys are
        // swallowed here rather than typed into a prompt.
        if key.modifiers.contains(KeyModifiers::ALT) {
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
            return Some(UiEvent::Intent(UiIntent::Operate {
                operation: client::protocol::Operation::DeleteSession { session: target },
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
                return Some(UiEvent::Intent(UiIntent::Submit { line: raw }));
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
                self.clicked = Some(client::protocol::Operation::StickySession {
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
                self.clicked = Some(client::protocol::Operation::RenderSession {
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

    /// Ctrl+C copies the selection: the one clipboard write a console without
    /// QuickEdit no longer does by itself.
    fn console(&mut self, action: ConsoleAction) {
        match action {
            ConsoleAction::CopySelection => self.copy_selection(),
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
    fn complete(&self, _line: &str, _cursor: usize) -> client::Offered {
        client::Offered::default()
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
        client::protocol::Reference::Image.name(),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// One environment, as the table reads it.
    fn named(pairs: &[(&str, &str)]) -> Option<ProtocolType> {
        let environment: HashMap<&str, &str> = pairs.iter().copied().collect();
        named_protocol_where(|name| environment.get(name).map(|value| value.to_string()))
    }

    /// The terminals this window is actually opened in.
    #[test]
    fn what_the_environment_names() {
        let cells = Some(ProtocolType::Halfblocks);
        assert_eq!(named(&[("TERM_PROGRAM", "vscode")]), cells);
        assert_eq!(named(&[("TERM_PROGRAM", "Apple_Terminal")]), cells);
        assert_eq!(
            named(&[
                ("TERM_PROGRAM", "Windows_Terminal"),
                ("TERM_PROGRAM_VERSION", "1.24.12741.0"),
            ]),
            Some(ProtocolType::Sixel)
        );
        assert_eq!(
            named(&[
                ("TERM_PROGRAM", "Windows_Terminal"),
                ("TERM_PROGRAM_VERSION", "1.21.2911.0"),
            ]),
            cells
        );
        assert_eq!(
            named(&[("KONSOLE_VERSION", "220400")]),
            Some(ProtocolType::Sixel)
        );
        assert_eq!(
            named(&[("TERM_PROGRAM", "iTerm.app")]),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(named(&[("TERM", "xterm-256color")]), None);
    }

    /// The signals that still arrive through tmux and ssh, where no name is left.
    #[test]
    fn what_survives_a_wrapper() {
        let kitty = Some(ProtocolType::Kitty);
        assert_eq!(named(&[("WEZTERM_PANE", "3")]), kitty);
        assert_eq!(named(&[("KITTY_WINDOW_ID", "1")]), kitty);
        assert_eq!(named(&[("GHOSTTY_RESOURCES_DIR", "/opt/ghostty")]), kitty);
        assert_eq!(named(&[("TERM", "xterm-kitty")]), kitty);
        assert_eq!(
            named(&[("LC_TERMINAL", "iTerm2")]),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(
            named(&[("TERM", "mlterm-256color")]),
            Some(ProtocolType::Sixel)
        );
    }

    /// A version is compared by its numbers, not by its text.
    #[test]
    fn versions_are_numbers() {
        assert!(at_least("1.22.0.0", 1, 22));
        assert!(at_least("1.24.12741.0", 1, 22));
        assert!(at_least("2.0", 1, 22));
        assert!(!at_least("1.9.9999.0", 1, 22));
        assert!(!at_least("1.21", 1, 22));
        assert!(!at_least("", 1, 22));
    }
}
