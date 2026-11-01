//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `cli`, never the reverse, so the command layer stays usable
//! from a script and a new command never changes this crate.

mod clipboard;
mod input;
mod log;
mod markdown;
mod panes;
pub mod program;
mod render;
pub mod run;

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
pub use crate::log::View;
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

/// Above this block size, or with no cap, an exhausted inner wheel continues outside.
const SCROLL_CHAIN_BLOCK_SIZE: usize = 6;

/// What Alt+<key> does to the terminal itself, rather than to the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleAction {
    /// Alt+Y: put the visible rows on the clipboard.
    Copy,
    /// Alt+M: toggle whether this app or the terminal owns the mouse.
    Mouse,
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
        if let Some(name) = render::list_at(
            list,
            &self.panes,
            self.sticky_session.as_deref(),
            column,
            row,
        ) {
            return Some(Hit {
                name,
                part: Part::List,
            });
        }
        let slots = self.slots(width, height);
        // The hint is drawn over the pane's last row, so it answers before it.
        if let Some(name) = render::bottom_at(&slots, &self.panes, column, row) {
            return Some(Hit {
                name,
                part: Part::Bottom,
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
        let slots = self.slots(width, height);
        let slot = slots
            .iter()
            .find(|slot| render::covers(&slot.body, column, row))
            .or_else(|| {
                // A header row answers with the first item: a press there is the
                // session's, which `session_at_row` already handles.
                Option::<&render::Slot>::None
            })?;
        let name = slot.name.clone();
        let body = slot.body;
        let inside = (row - body.y) as usize;
        // One column short, as the renderer measures.
        let log_width = body.width.saturating_sub(1).max(1) as usize;
        let scroll = self.panes.get(&name).map(|pane| pane.scroll).unwrap_or(0);
        self.panes.block_at(
            View::Window(&name),
            log_width,
            body.height.max(1) as usize,
            scroll,
            inside,
        )
    }

    /// Scrolls one pane: the wheel reads what the pointer is over, not the keys'.
    pub fn scroll_pane(&mut self, name: &str, rows: i64) {
        self.panes.scroll_by(name, rows);
    }

    /// Scrolls the item under a pointer, or its pane when there is no open item.
    /// Large or unbounded items can hand an exhausted wheel to the outer pane.
    pub fn scroll_at(&mut self, width: u16, height: u16, column: u16, row: u16, rows: i64) -> bool {
        let Some(hit) = self.session_at_row(width, height, column, row) else {
            return false;
        };
        if hit.part == Part::Pane
            && let Some(at) = self.item_at(width, height, column, row)
            && self.panes.expanded_at(at)
        {
            // Large and unbounded boxes hand an exhausted wheel to the pane.
            // Small boxes keep it, so reading a short result never moves the log.
            if self.panes.scroll_block(at, rows, SCROLL_CHAIN_BLOCK_SIZE) {
                return true;
            }
        }
        if hit.part != Part::List {
            self.scroll_pane(&hit.name, rows);
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
        let window = self
            .panes
            .window(View::Window(&name), width, height, scroll);
        window
            .rows
            .iter()
            .map(|(_, row, _)| row.to_string())
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

/// One answer being accumulated, and whether its block is open yet.
///
/// # Invariant
///
/// `opened` is what keeps two turns on one key apart: a sealed block is still in
/// the log under that key, so "a block exists" cannot answer "have I opened mine".
#[derive(Default)]
struct Answer {
    raw: String,
    opened: bool,
}

/// What the log should do with the answer as it now stands.
#[derive(Debug, PartialEq, Eq)]
enum Drawn {
    /// Nothing yet: a blank answer is not a row.
    Nothing,
    /// Open the block with this text.
    Open(String),
    /// Replace the text of the block already open.
    Rewrite(String),
}

impl Answer {
    /// Takes one delta and says what the block now looks like.
    ///
    /// # Invariant
    ///
    /// The text is re-rendered from the whole answer, because markdown is not
    /// append-safe: one more `**` can change a row above the one arriving. Only
    /// the first delta opens the block -- a later one would double the text.
    fn feed(&mut self, text: &str) -> Drawn {
        self.raw.push_str(text);
        let drawn = markdown::render(&self.raw);
        if self.opened {
            return Drawn::Rewrite(drawn);
        }
        if drawn.is_empty() {
            return Drawn::Nothing;
        }
        self.opened = true;
        Drawn::Open(drawn)
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
    /// The answer arriving under each key: markdown is rendered from its whole,
    /// so deltas are kept until the block ends.
    answers: std::collections::HashMap<String, Answer>,
    /// The input as the last frame saw it: typing may have put console ink on
    /// the row.
    input_seen: Option<(usize, usize)>,
    /// A click on a session, as the command line it asks for, waiting for the loop
    /// to take it. See `docs/notes/tui-lib.md`.
    clicked: Option<String>,
}

impl Tui {
    pub fn new(completer: SharedCompleter) -> anyhow::Result<Self> {
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
                panes: Panes::new(),
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
            answers: std::collections::HashMap::new(),
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
    pub fn stream(
        &mut self,
        name: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        // SPEC's `output (colored markdown)`: re-rendered, never appended.
        // Everything else is drawn as it arrives.
        if kind == LineKind::Assistant {
            let drawn = self.answers.entry(key.to_string()).or_default().feed(text);
            match drawn {
                Drawn::Nothing => {}
                Drawn::Open(drawn) => self
                    .screen
                    .panes
                    .stream(name, key, kind, prefix, &drawn, false),
                Drawn::Rewrite(drawn) => self.screen.panes.rewrite(key, &drawn),
            }
            if done {
                self.answers.remove(key);
                self.screen.panes.stream(name, key, kind, prefix, "", true);
            }
            self.dirty = true;
            return;
        }
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

    /// Appends a tool's result to its call, preserving one expandable item.
    pub fn resolve(
        &mut self,
        name: &str,
        key: &str,
        tag: &str,
        kind: LineKind,
        text: &str,
        done: bool,
    ) {
        self.screen.panes.resolve(name, key, tag, kind, text, done);
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
        present(&mut self.terminal, &mut self.screen, typed)?;
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
        action
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

    /// Whether this key asks for the clipboard: SPEC's Ctrl+V, case either way.
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
                // plain ↑↓ stay with history.
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

        // A terminal that pastes itself sends the text, not this key.
        if Self::clipboard_paste(&key) {
            self.paste_from_clipboard();
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
            KeyCode::Up => {
                // ↑↓ go popup, then text, then history; Shift moves the view instead.
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.screen.scroll_by(1);
                } else if let Some(c) = self.screen.completion.as_mut() {
                    c.move_selection(-1);
                } else if !self.screen.input.up() {
                    self.history_prev();
                }
            }
            KeyCode::Down => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Down is SPEC's "back to the newest line".
                    self.screen.scroll_to_bottom();
                } else if let Some(c) = self.screen.completion.as_mut() {
                    c.move_selection(1);
                } else if !self.screen.input.down() {
                    self.history_next();
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
            // The wheel writes no state. Over an opened item it scrolls that
            // item's box -- the one scroll SPEC gives a block.
            self.screen
                .scroll_at(size.width, size.height, mouse.column, mouse.row, rows);
            self.dirty = true;
            return;
        }
        self.dirty = true;
        // The way back to the newest row is not a click on the session under it.
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

    /// Ctrl+V: the clipboard, when the terminal did not paste it for us.
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

    fn refresh_completion(&mut self) {
        self.screen.completion = next_completion(&self.screen.input, &self.completer);
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

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        self.screen.input.clear();
        self.screen.input.insert_str(&self.history[pos]);
    }

    fn history_next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 >= self.history.len() {
            self.history_pos = None;
            self.screen.input.clear();
        } else {
            self.history_pos = Some(pos + 1);
            self.screen.input.clear();
            self.screen.input.insert_str(&self.history[pos + 1]);
        }
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

#[cfg(test)]
mod startup {
    /// A start clears the screen it is about to draw on.
    ///
    /// # Invariant
    ///
    /// A previous process that died without restoring -- killed, not quitted --
    /// leaves its last frame in the alternate screen, and the first draw will not
    /// overwrite the blank cells. Only an explicit clear does.
    #[test]
    fn the_screen_is_cleared_before_the_first_frame() {
        let source = include_str!("lib.rs");
        let new = source
            .split_once("pub fn new(completer: SharedCompleter)")
            .and_then(|(_, rest)| rest.split_once("Ok(Self {"))
            .map(|(body, _)| body)
            .expect("the constructor is where a start happens");
        assert!(new.contains(".clear()"), "{new}");
    }

    #[test]
    fn an_initialization_failure_restores_the_terminal() {
        let source = include_str!("lib.rs");
        let new = source
            .split_once("pub fn new(completer: SharedCompleter)")
            .and_then(|(_, rest)| rest.split_once("Ok(Self {"))
            .map(|(body, _)| body)
            .expect("the constructor is where a start happens");
        let failure = new
            .split_once("Err(error) =>")
            .map(|(_, body)| body)
            .expect("initialization has a failure branch");
        assert!(failure.contains("restore_terminal"), "{failure}");
    }
}

#[cfg(test)]
mod repaint {
    /// A key that reaches the prompt has to repaint the frame.
    ///
    /// # Invariant
    ///
    /// `flush` is the only way a held key becomes an edit, and `draw` skips a
    /// frame nothing changed for. Without this flag every keypress looks
    /// ignored -- invisible to a headless test, which is why it is read here.
    #[test]
    fn a_flushed_run_marks_the_frame_dirty() {
        let source = include_str!("lib.rs");
        // Up to the closing brace: this file is CRLF, so `\n    }` is the split.
        let body = source
            .split_once("fn flush(&mut self)")
            .and_then(|(_, rest)| rest.split_once("\n    }"))
            .map(|(body, _)| body)
            .expect("flush is where a held key becomes an edit");
        assert!(body.contains("self.dirty = true"), "{body}");
    }
}

#[cfg(test)]
mod terminal_modes {
    /// Guards the enable/disable pair nothing else would notice losing.
    ///
    /// # Invariant
    ///
    /// Each mode is named on the way in and on the way out.
    #[test]
    fn restore_attempts_every_step_and_returns_the_first_error() {
        let mut attempted = Vec::new();
        let result = super::restore_steps([
            {
                attempted.push("raw");
                Err(std::io::Error::other("raw failed"))
            },
            {
                attempted.push("screen");
                Ok(())
            },
            {
                attempted.push("paste");
                Err(std::io::Error::other("paste failed"))
            },
            {
                attempted.push("mouse");
                Ok(())
            },
            {
                attempted.push("cursor");
                Ok(())
            },
        ]);
        assert_eq!(attempted, ["raw", "screen", "paste", "mouse", "cursor"]);
        assert_eq!(result.unwrap_err().to_string(), "raw failed");
    }

    #[test]
    fn a_failed_restore_can_be_retried_by_drop() {
        let mut restored = false;
        let mut attempts = 0;
        let result = super::restore_once(&mut restored, || {
            attempts += 1;
            Err(std::io::Error::other("try again"))
        });
        assert!(result.is_err());
        assert!(!restored);

        super::restore_once(&mut restored, || {
            attempts += 1;
            Ok(())
        })
        .unwrap();
        assert!(restored);
        super::restore_once(&mut restored, || {
            panic!("a restored terminal is not touched")
        })
        .unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn the_terminal_modes_are_paired() {
        let source = include_str!("lib.rs");
        // Only the impl body: `use` names every mode once, so the whole file
        // would pass even with the calls deleted.
        let body = source
            .split_once("impl Tui {")
            .map(|(_, rest)| rest)
            // And not the tests: this test's own literals would satisfy it.
            .and_then(|rest| rest.split_once("#[cfg(test)]").map(|(body, _)| body))
            .unwrap_or(source);
        for mode in ["EnableMouseCapture", "EnableBracketedPaste"] {
            let disable = mode.replacen("Enable", "Disable", 1);
            assert!(body.contains(mode), "{mode} is never enabled");
            assert!(body.contains(&disable), "{mode} is never disabled");
        }
    }
}

#[cfg(test)]
mod layering {
    /// The arrow this crate exists to point: `tui` depends on `cli`, not the reverse.
    #[test]
    fn the_front_end_depends_on_the_command_layer() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("cli = { workspace = true }"),
            "{manifest}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The ink an IME puts on the console, which no frame of ours ever drew.
    ///
    /// # Invariant
    ///
    /// A repaint that follows typing takes it back: the columns it lands in are
    /// ones the frame drew blank, so erasing there cannot lose our own text. See
    /// `docs/notes/tui-lib.md`.
    #[test]
    fn a_repaint_after_typing_takes_back_console_ink() {
        use ratatui::backend::TestBackend;
        use ratatui::buffer::Cell;

        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut screen = Screen {
            panes: Panes::new(),
            completion: None,
            input: InputLine::default(),
            status: String::new(),
            status_kind: LineKind::System,
            server: None,
            placeholder: String::new(),
            sticky_session: None,
        };
        screen.input.insert_str("hi");
        present(&mut terminal, &mut screen, false).unwrap();

        let area = Rect::new(0, 0, 40, 12);
        let row = render::input_area(area, &mut screen).y;
        // The composition a console-level IME would leave behind, past our text.
        let ink = Cell::from('拼');
        ratatui::backend::Backend::draw(
            terminal.backend_mut(),
            std::iter::once((20u16, row, &ink)),
        )
        .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(20, row)].symbol(),
            "拼",
            "the ink was written"
        );

        // One keystroke later, the row past its text is said again.
        screen.input.insert('!');
        present(&mut terminal, &mut screen, true).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(20, row)].symbol(),
            " ",
            "the composition's tail was left on the prompt"
        );
        // And the text itself is untouched.
        assert_eq!(terminal.backend().buffer()[(2, row)].symbol(), "h");
    }

    /// Drives the key handler without a terminal: the only part with real logic.
    struct Headless {
        screen: Screen,
    }

    impl Headless {
        fn new() -> Self {
            let mut panes = Panes::new();
            // One session drawn as a pane, which is the shape these keys were
            // written for.
            panes.layout(&Layout {
                listed: vec![Listed {
                    name: "main".to_string(),
                    status: "idle".to_string(),
                    title: String::new(),
                }],
                shown: vec!["main".to_string()],
                command: "cmd".to_string(),
                quota: None,
            });
            Self {
                screen: Screen {
                    panes,
                    completion: None,
                    input: InputLine::default(),
                    status: String::new(),
                    status_kind: LineKind::System,
                    server: None,
                    placeholder: String::new(),
                    sticky_session: None,
                },
            }
        }

        /// The scroll a pane is at, which is where the keyboard put it.
        fn pane_scroll(&self, name: &str) -> usize {
            self.screen
                .panes
                .get(name)
                .expect("the pane is there")
                .scroll
        }

        /// What the renderer would have written back for this pane's window.
        fn measure(&mut self, name: &str, rows: usize) {
            let max_scroll = self
                .screen
                .panes
                .window(View::Window(name), 200, rows, 0)
                .max_scroll;
            self.screen
                .panes
                .get_mut(name)
                .expect("the pane is there")
                .max_scroll = max_scroll;
        }

        /// The same normalization `Tui::paste` does, on this stand-in's input.
        fn paste(&mut self, raw: &str) {
            self.screen.input.insert_str(&clipboard::paste_text(raw));
        }

        /// The same insertion the window does for a picture: the reference goes
        /// into the input line whole, and nothing is sent.
        fn paste_image(&mut self, path: &Path) {
            let image = clipboard::ImageClipboard::Saved(path.to_path_buf());
            let Some(reference) = image_reference_of(&image) else {
                panic!("a saved picture must be a reference");
            };
            self.screen.insert_paste(&reference);
        }

        /// The same call `Tui::stream` makes, so a test drives the real path.
        fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
            self.screen
                .panes
                .stream("main", key, kind, prefix, text, done);
        }

        /// Every row the merged view holds, unwrapped at a width these cases reach.
        fn rows(&mut self) -> Vec<String> {
            self.screen
                .panes
                .window(View::All, 200, 100, 0)
                .rows
                .iter()
                .map(|(_, text, _)| text.to_string())
                .collect()
        }
    }

    struct StaticCompleter(Vec<Candidate>);

    impl Completer for StaticCompleter {
        fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
            let (_, _, token) = token_bounds(line, cursor);
            // An empty token means "nothing to complete", which is how the
            // popup stays shut on a blank line.
            if token.is_empty() {
                return Vec::new();
            }
            self.0
                .iter()
                .filter(|c| c.insert.starts_with(&token))
                .cloned()
                .collect()
        }
    }

    /// A stand-in catalog: `--session` takes session names. Not borrowed from `cli`.
    struct FlagCompleter;

    impl Completer for FlagCompleter {
        fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
            let (_, _, token) = token_bounds(line, cursor);
            if token.starts_with("--") {
                return vec![Candidate::word("--session", "target")];
            }
            if token.is_empty() && line[..cursor].trim_end().ends_with("--session") {
                return vec![
                    Candidate::word("main", "session"),
                    Candidate::word("scratch", "session"),
                ];
            }
            Vec::new()
        }
    }

    #[test]
    fn editing_and_cursor_moves_close_completion_before_tab() {
        let completer: SharedCompleter = std::sync::Arc::new(NoCompleter);
        for key in [
            KeyCode::Delete,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
        ] {
            let mut input = InputLine::default();
            input.insert_str("send");
            input.left();
            input.left();
            let mut completion = Some(Completion {
                start: 0,
                end: 4,
                candidates: vec![Candidate::word("replacement", "")],
                selected: 0,
            });
            assert!(edit_input(&mut input, &mut completion, key));
            assert!(completion.is_none(), "{key:?} left a stale popup");
            let edited = input.clone();
            apply_and_continue(&mut input, &mut completion, &completer);
            assert_eq!(input.text(), edited.text());
            assert_eq!(input.cursor(), edited.cursor());
        }
    }

    /// Tab on a flag, then Tab again, fills the value: the list reopens on its own.
    #[test]
    fn completing_a_flag_asks_for_its_value() {
        let completer: SharedCompleter = std::sync::Arc::new(FlagCompleter);
        let mut input = InputLine::default();
        input.insert_str("/cmd --sess");
        let mut completion = next_completion(&input, &completer);
        assert_eq!(
            completion.as_ref().map(|c| c.candidates.len()),
            Some(1),
            "the flag itself is the first offer"
        );

        // First Tab: the flag, with the space that starts its value.
        apply_and_continue(&mut input, &mut completion, &completer);
        assert_eq!(input.text(), "/cmd --session ");
        let popup = completion.as_ref().expect("the value list reopened");
        assert_eq!(popup.candidates[0].insert, "main");

        // Second Tab: the value, and the line has nothing left to offer.
        apply_and_continue(&mut input, &mut completion, &completer);
        assert_eq!(input.text(), "/cmd --session main ");
        assert!(completion.is_none(), "nothing more to offer");
    }

    #[test]
    fn deltas_on_one_key_become_one_line() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "he", false);
        h.stream("r:1", LineKind::Assistant, "", "llo", false);
        assert_eq!(h.rows(), vec!["hello"]);
    }

    /// The prefix opens the line once, so a delta can append bare.
    #[test]
    fn a_prefix_is_written_once_per_stream() {
        let mut h = Headless::new();
        let key = "r:peer:1:Reasoning";
        h.stream(key, LineKind::Reasoning, "[peer] thinking: ", "he", false);
        h.stream(key, LineKind::Reasoning, "[peer] thinking: ", "llo", false);
        h.stream(key, LineKind::Reasoning, "[peer] thinking: ", "!", true);
        // A reasoning item is SPEC's `abstract`: its mark, its label, its text.
        assert_eq!(h.rows(), vec!["\u{25c7} [peer] thinking: hello!"]);
    }

    #[test]
    fn done_closes_the_stream() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "a", true);
        h.stream("r:1", LineKind::Assistant, "", "b", true);
        assert_eq!(h.rows(), vec!["a", "b"]);
    }

    /// Two sessions, keys shaped as the client builds them: no row loses its tag.
    #[test]
    fn interleaved_sessions_keep_their_own_tag() {
        let peer = "r:peer:1:Reasoning";
        let peer_tag = "[peer] thinking: ";
        let test1 = "r:test1:1:Reasoning";
        let test1_tag = "[test1] thinking: ";
        let answer = "r:test1:1:Output";
        let script: &[(&str, LineKind, &str, &str, bool)] = &[
            (peer, LineKind::Reasoning, peer_tag, "我先", false),
            (test1, LineKind::Reasoning, test1_tag, "确认", false),
            (peer, LineKind::Reasoning, peer_tag, "看一下", false),
            ("t:c1:Stdout", LineKind::ToolOutput, "[peer] ", "ok", true),
            (test1, LineKind::Reasoning, test1_tag, "完毕", false),
            (peer, LineKind::Reasoning, peer_tag, "。", true),
            (answer, LineKind::Assistant, "[test1] ", "这", false),
            (answer, LineKind::Assistant, "[test1] ", "是我的", true),
        ];
        let mut h = Headless::new();
        for (key, kind, prefix, text, done) in script {
            h.stream(key, *kind, prefix, text, *done);
        }
        assert_eq!(
            h.rows(),
            vec![
                "\u{25c7} [peer] thinking: 我先看一下。",
                "\u{25c7} [test1] thinking: 确认完毕",
                "\u{2713} [peer]",
                "[test1] 这是我的",
            ]
        );
    }

    /// A deleted session's pane and rows leave together, and the command line stays.
    ///
    /// # Invariant
    ///
    /// The window is emptied because the session is gone, not because a layout
    /// stopped drawing it: hiding a session must keep every line it carried.
    #[test]
    fn forgetting_a_window_empties_its_pane() {
        let mut panes = Panes::new();
        panes.layout(&Layout {
            listed: vec![Listed {
                name: "main".to_string(),
                status: "idle".to_string(),
                title: String::new(),
            }],
            shown: vec!["main".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        panes.line("main", LineKind::System, "hi");
        panes.line("cmd", LineKind::Command, "$ x");
        assert_eq!(panes.rows(View::Window("main"), 40), 1);

        panes.forget("main");
        assert!(panes.get("main").is_none(), "the pane outlived its session");
        assert_eq!(panes.rows(View::Window("main"), 40), 0);
        // A line that names no session is not a session's, so it stays.
        assert_eq!(panes.rows(View::Window("cmd"), 40), 1);
    }

    /// A pasted block lands in the prompt whole: no line break may press Enter.
    ///
    /// # Invariant
    ///
    /// This is the difference between pasting a programme and sending its first
    /// line while the rest sits in the box.
    /// A pasted newline is not a plain Enter: Windows sends `Enter` with CONTROL,
    /// and a rule that only takes the bare key loses every line break -- or, worse,
    /// sends the block one line at a time.
    #[test]
    fn a_pasted_newline_is_recognised_however_the_terminal_spells_it() {
        let plain = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let control = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        let shifted = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);

        for key in [&plain, &control] {
            assert!(is_paste_newline(key), "{key:?} is a paste's break");
        }
        assert!(!is_paste_newline(&shifted), "Shift+Enter is the operator's");

        // A block pasted as keys: each Enter in it is a line, not a send.
        let keys: Vec<KeyEvent> = "one\ntwo"
            .chars()
            .map(|c| match c {
                '\n' => control,
                other => KeyEvent::new(KeyCode::Char(other), KeyModifiers::NONE),
            })
            .collect();
        assert_eq!(pasted(&keys).as_deref(), Some("one\ntwo"));

        // And a lone pasted break after a machine-fast run is still a line.
        assert!(glue_break(&[control], Some(MACHINE_RUN)));
        assert!(Tui::paste_key(&control), "and it keeps a run going");
    }

    #[test]
    fn a_paste_is_inserted_not_submitted() {
        let mut h = Headless::new();
        h.paste("fn main() {\n    todo!()\n}\n");
        assert_eq!(h.screen.input.text(), "fn main() {\n    todo!()\n}\n");
        // Pasted text is not a keystroke replay: nothing was sent.
        assert!(h.screen.input.text().contains('\n'));
    }

    /// A Windows clipboard's CRLF must not leave carriage returns in the box.
    #[test]
    fn a_paste_normalizes_line_endings() {
        let mut h = Headless::new();
        h.paste("a\r\nb\rc");
        assert_eq!(h.screen.input.text(), "a\nb\nc");
    }

    /// What joins a run and what ends it: typing and breaks join, bindings do not.
    #[test]
    fn only_text_joins_a_run() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert!(Tui::paste_key(&key(KeyCode::Char('a'), KeyModifiers::NONE)));
        // Shift is part of the character, not a binding.
        assert!(Tui::paste_key(&key(
            KeyCode::Char('A'),
            KeyModifiers::SHIFT
        )));
        assert!(Tui::paste_key(&key(KeyCode::Enter, KeyModifiers::NONE)));

        assert!(!Tui::paste_key(&key(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert!(!Tui::paste_key(&key(KeyCode::Char('k'), KeyModifiers::ALT)));
        assert!(!Tui::paste_key(&key(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL
        )));
        assert!(!Tui::paste_key(&key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!Tui::paste_key(&key(KeyCode::Up, KeyModifiers::NONE)));
    }

    /// The reported failure: a pasted block whose breaks each arrive alone.
    #[test]
    fn a_break_after_a_machine_run_is_part_of_the_paste() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        // Three keys inside one window is delivery; the break belongs to it.
        assert!(glue_break(&[enter], Some(MACHINE_RUN)));
        assert!(glue_break(&[enter], Some(40)));

        // A human's Enter follows one or two keys, or arrives late.
        assert!(!glue_break(&[enter], None));
        assert!(!glue_break(&[enter], Some(MACHINE_RUN - 1)));

        // Only a bare Enter: the modified ones already break the line by hand.
        let shift = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(!glue_break(&[shift], Some(40)));
        // And only a lone one: a run carrying Enter is already handled by `pasted`.
        let more = [enter, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)];
        assert!(!glue_break(&more, Some(40)));
    }

    /// Windows reports a paste key by key, so it only shows as no pause.
    #[test]
    fn a_key_run_with_a_break_in_it_is_a_paste() {
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(
            pasted(&[ch('a'), ch('b'), enter, ch('c')]),
            Some("ab\nc".to_string())
        );
        // One key is what a keyboard sends, break or not.
        assert_eq!(pasted(&[enter]), None);
        assert_eq!(pasted(&[ch('a')]), None);
        // No break, no send at stake: this is ordinary fast typing.
        assert_eq!(pasted(&[ch('a'), ch('b')]), None);
        // Shift+Enter breaks a line by hand; it is never part of a paste.
        assert_eq!(
            pasted(&[ch('a'), KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)]),
            None
        );
    }

    /// The console keys are not editing keys: Alt+Y and Alt+M are recognised,
    /// plain letters still type.
    #[test]
    fn the_console_keys_are_recognised() {
        let alt = KeyModifiers::ALT;
        assert_eq!(
            Tui::console_key(&KeyEvent::new(KeyCode::Char('y'), alt)),
            Some(ConsoleAction::Copy)
        );
        assert_eq!(
            Tui::console_key(&KeyEvent::new(KeyCode::Char('M'), alt)),
            Some(ConsoleAction::Mouse)
        );
        assert_eq!(
            Tui::console_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            Tui::console_key(&KeyEvent::new(KeyCode::Char('q'), alt)),
            None,
            "Alt+Q is the quit shortcut, not a console key"
        );
    }

    /// Two sessions squeezed into a window too small for both: each keeps a
    /// row, and the cut one still says who it is.
    #[test]
    fn a_squeezed_window_still_names_both_sessions() {
        let mut h = Headless::new();
        h.stream(
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "aaa",
            false,
        );
        h.stream(
            "r:test1:1:Output",
            LineKind::Assistant,
            "[test1] ",
            "bbb",
            true,
        );
        h.stream(
            "r:peer:1:Reasoning",
            LineKind::Reasoning,
            "[peer] ",
            "ccc",
            false,
        );
        // Two rows: the peer block is cut to its tail and repeats its tag.
        let rows = h.screen.panes.window(View::All, 40, 2, 0).rows;
        let shown: Vec<String> = rows.iter().map(|(_, t, _)| t.to_string()).collect();
        assert_eq!(shown, vec!["\u{25c7} [peer] aaaccc", "[test1] bbb"]);
    }

    /// A realistic command leaves no blank row and no empty block.
    ///
    /// # Invariant
    ///
    /// The shell writes one empty line before every exit sentinel on both
    /// streams, so this is the traffic every single `bash` call produces.
    #[test]
    fn a_command_leaves_no_blank_block() {
        let mut h = Headless::new();
        // stdout: the command's own output, then the sentinel's blank line.
        h.stream("t:c1:Stdout", LineKind::ToolOutput, "", "hi\n", false);
        h.stream("t:c1:Stdout", LineKind::ToolOutput, "", "\n", true);
        // stderr: nothing at all but the sentinel's blank line.
        h.stream("t:c1:Stderr", LineKind::ToolError, "", "\n", false);
        h.stream("t:c1:Stderr", LineKind::ToolError, "", "", true);
        // A result folds to its verdict; with none yet, the mark alone.
        assert_eq!(h.rows(), vec!["\u{2713} "]);
    }

    /// Two streams open at once stay separate: a tool's output never joins a response.
    #[test]
    fn two_open_streams_do_not_mix() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "answer", false);
        h.stream("t:1", LineKind::ToolOutput, "$ ", "stdout", false);
        h.stream("r:1", LineKind::Assistant, "", " more", false);
        assert_eq!(h.rows(), vec!["answer more", "\u{2713} $ stdout"]);
    }

    #[test]
    fn key_release_is_not_actionable() {
        let mut press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(Tui::actionable(&press));
        press.kind = KeyEventKind::Release;
        assert!(!Tui::actionable(&press));
        // A held key still has to type.
        press.kind = KeyEventKind::Repeat;
        assert!(Tui::actionable(&press));
    }

    /// Leaving is the one thing the keyboard does on its own: Alt+Q, nothing else.
    #[test]
    fn alt_q_is_the_only_way_out() {
        let quit = Some(UiEvent::Quit);
        // The plain case, and the shifted one, which arrives as 'Q'.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)),
            quit
        );
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(
                KeyCode::Char('Q'),
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            quit
        );
        // A bare q is a letter to type, not an exit.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            None
        );
        // Ctrl+Q is taken by VS Code, so it is no longer the binding: leaving must work.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
        // The old bindings are gone: Ctrl+X and Ctrl+C mean nothing now.
        for code in [KeyCode::Char('x'), KeyCode::Char('X'), KeyCode::Char('c')] {
            assert_eq!(
                Tui::shortcut(&KeyEvent::new(code, KeyModifiers::CONTROL)),
                None,
                "{code:?} must not do anything"
            );
        }
    }

    /// Ctrl+V is the clipboard; every other Ctrl+letter still does nothing.
    ///
    /// # Invariant
    ///
    /// The key is read before the swallow below it, or a terminal that forwards
    /// Ctrl+V (instead of pasting the text itself) would look like a dead key.
    #[test]
    fn ctrl_v_is_the_clipboard() {
        for code in [KeyCode::Char('v'), KeyCode::Char('V')] {
            assert!(Tui::clipboard_paste(&KeyEvent::new(
                code,
                KeyModifiers::CONTROL
            )));
        }
        // Typing a `v` is typing a letter; the rest of the Ctrl row stays silent.
        for key in [
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        ] {
            assert!(!Tui::clipboard_paste(&key), "{key:?} is not a paste");
        }
    }

    /// Deleting the sticky session is Shift+Delete; a bare key never deletes it.
    ///
    /// # Invariant
    ///
    /// No unmodified key deletes a session: Delete and Backspace are the editor's,
    /// and a session is the one thing on screen a keystroke cannot take back.
    #[test]
    fn shift_delete_is_the_delete_key() {
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        assert!(Tui::delete_key(&shift(KeyCode::Delete)));
        // Every earlier binding of this action is gone: bare Delete, Backspace, Shift+D.
        for key in [
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::SHIFT),
            shift(KeyCode::Char('d')),
            shift(KeyCode::Char('D')),
        ] {
            assert!(
                !Tui::delete_key(&key),
                "{:?} + {:?} must not delete a session",
                key.code,
                key.modifiers
            );
        }
    }

    /// Enter sends, the modified Enter breaks the line; backwards sends half a message.
    #[test]
    fn enter_sends_and_the_modified_enter_breaks_the_line() {
        let enter = KeyCode::Enter;
        assert!(!Tui::line_break(&KeyEvent::new(enter, KeyModifiers::NONE)));
        assert!(Tui::line_break(&KeyEvent::new(enter, KeyModifiers::SHIFT)));
        assert!(Tui::line_break(&KeyEvent::new(enter, KeyModifiers::ALT)));
        // Anything else is not a break, however it is modified.
        assert!(!Tui::line_break(&KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::SHIFT
        )));
    }

    /// What one wheel notch means, pinned down: three rows, one with Shift.
    #[test]
    fn wheel_notches_become_rows() {
        let up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(Tui::wheel_rows(&up), Some(3));
        let fine = MouseEvent {
            modifiers: KeyModifiers::SHIFT,
            ..up
        };
        assert_eq!(Tui::wheel_rows(&fine), Some(1));
        let down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..up
        };
        assert_eq!(Tui::wheel_rows(&down), Some(-3));
        // Anything else the mouse reports is not an action here.
        let click = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            ..up
        };
        assert_eq!(Tui::wheel_rows(&click), None);
    }

    /// Both ends stop the scroll: page-up past the top, page-down past the bottom.
    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut h = Headless::new();
        for i in 0..30 {
            h.screen
                .panes
                .line("main", LineKind::System, format!("L{i}"));
        }
        // SPEC: the keyboard scrolls the sticky session, so that is what a page
        // acts on.
        h.screen.sticky_session = Some("main".to_string());
        // What the renderer works out and writes back for a pane of this size.
        h.measure("main", 10);
        h.screen.scroll_by(3);
        assert_eq!(h.pane_scroll("main"), 3);
        h.screen.scroll_by(100);
        assert_eq!(h.pane_scroll("main"), 20);
        h.screen.scroll_by(-5);
        assert_eq!(h.pane_scroll("main"), 15);
        // Shift+Down is "back to the newest line", whatever the offset was.
        h.screen.scroll_to_bottom();
        assert_eq!(h.pane_scroll("main"), 0);
    }

    /// The way back to the newest row answers the click drawn over it.
    ///
    /// # Invariant
    ///
    /// The hint sits on a pane's last row, so a click there pins that pane: a
    /// click that set the session under the hint is the bug this ordering exists
    /// to prevent. See `docs/notes/tui-lib.md`.
    #[test]
    fn a_click_on_the_way_back_to_the_bottom_pins_the_view() {
        let mut h = Headless::new();
        for i in 0..40 {
            h.screen
                .panes
                .line("main", LineKind::System, format!("L{i}"));
        }
        h.screen.sticky_session = Some("main".to_string());
        let (width, height) = (40u16, 12u16);
        // The bound the renderer works out for a pane of this size, then the scroll.
        h.measure("main", 6);
        h.screen.scroll_by(3);
        assert_eq!(h.pane_scroll("main"), 3);
        let slot = h
            .screen
            .slots(width, height)
            .into_iter()
            .find(|slot| slot.name == "main")
            .expect("a pane named main");
        let hint = crate::render::bottom_rect(slot.body).expect("a hint on a scrolled pane");
        assert_eq!(
            h.screen
                .session_at_row(width, height, hint.x, hint.y)
                .map(|hit| hit.part),
            Some(Part::Bottom),
            "the hint answers before the session drawn under it"
        );
        // One row above it is the pane itself, whichever session that is.
        assert_eq!(
            h.screen
                .session_at_row(width, height, hint.x, hint.y - 1)
                .map(|hit| hit.part),
            Some(Part::Pane)
        );
    }

    /// With no sticky session there is nothing for a page to scroll.
    #[test]
    fn a_page_with_no_sticky_session_scrolls_nothing() {
        let mut h = Headless::new();
        for i in 0..30 {
            h.screen
                .panes
                .line("main", LineKind::System, format!("L{i}"));
        }
        h.measure("main", 10);
        h.screen.scroll_by(3);
        assert_eq!(h.pane_scroll("main"), 0);
    }

    /// A note about this window goes on the status line, never on the command row.
    #[test]
    fn a_note_stays_on_the_status_line() {
        let mut h = Headless::new();
        h.screen.sticky_session = Some("main".to_string());
        h.screen.panes.line("main", LineKind::System, "first");
        let cmd_before = h.screen.panes.rows(View::Window("cmd"), 40);

        h.screen
            .note(LineKind::System, "nothing is listening — starting one");

        assert_eq!(
            h.screen.status, "nothing is listening — starting one",
            "the note is on the status line"
        );
        assert_eq!(h.screen.status_kind, LineKind::System);
        assert_eq!(
            h.screen.panes.rows(View::Window("main"), 40),
            1,
            "and not among the session's lines: a connection is not its event"
        );
        assert_eq!(
            h.screen.panes.rows(View::Window("cmd"), 40),
            cmd_before,
            "nor on the command row"
        );

        // It replaces the previous note, and an error keeps its kind for the bar.
        h.screen.note(LineKind::Error, "copy failed: no tool");
        assert_eq!(h.screen.status, "copy failed: no tool");
        assert_eq!(h.screen.status_kind, LineKind::Error);
    }

    /// One announcement entry: a name, idle, with nothing measured yet.
    fn listed(name: &str) -> Listed {
        Listed {
            name: name.to_string(),
            status: "idle".to_string(),
            title: String::new(),
        }
    }

    /// Two turns under one key are two blocks: the markdown path opens its own.
    ///
    /// # Invariant
    ///
    /// `opened` answers this, not "is there a block with this key" -- the
    /// previous turn's block is still in the log under it.
    /// An answer is re-rendered whole on every delta, and opened only once.
    ///
    /// # Invariant
    ///
    /// Markdown is not append-safe -- a later `**` changes an earlier row -- so a
    /// delta rewrites the block instead of appending. The first delta opens it;
    /// opening twice would draw the text twice.
    #[test]
    fn an_answer_opens_once_and_is_rewritten_after() {
        let mut answer = Answer::default();
        assert_eq!(answer.feed(""), Drawn::Nothing, "a blank answer is no row");
        // Unclosed emphasis is literal text: an unfinished marker is not a rule.
        assert_eq!(answer.feed("**bol"), Drawn::Open("**bol".into()));
        assert_eq!(
            answer.feed("d** text"),
            Drawn::Rewrite("**bold** text".into()),
            "the whole answer again, and the markers are still there"
        );

        // A sealed answer under the same key starts a new block, not a rewrite
        // of the one already in the log under that key.
        let mut second = Answer::default();
        assert_eq!(second.feed("next"), Drawn::Open("next".into()));
    }

    /// What the log draws for an answer is markdown's structure, not its markers.
    #[test]
    fn an_answer_is_drawn_as_the_structure_it_was_written_in() {
        let mut answer = Answer::default();
        let Drawn::Open(drawn) = answer.feed("# title\n\n- **one**\n- `two`") else {
            panic!("an answer opens a block");
        };
        // Structure is rows; the writer's markers are all still on screen.
        assert_eq!(drawn, "# title\n- **one**\n- `two`");
    }

    /// A press on a tool call opens it, so its arguments are not the whole of it.
    ///
    /// # Invariant
    ///
    /// The block is keyed by the call id and its body is empty until something
    /// streams into it, so this is also the case that says a fold is not about
    /// having text: a call opens to show what it was given.
    #[test]
    fn a_press_on_a_tool_call_opens_it() {
        let mut h = Headless::new();
        h.stream("c:c1", LineKind::ToolCall, "bash{ls}", "", true);
        h.measure("main", 8);
        let (width, height) = (40u16, 14u16);
        let slot = h
            .screen
            .slots(width, height)
            .into_iter()
            .find(|slot| slot.name == "main")
            .expect("the window");
        let key = h
            .screen
            .item_at(width, height, slot.body.x, slot.body.y)
            .expect("a press there names the item");
        assert!(h.screen.panes.toggle_at(key), "a tool call is an abstract");
    }

    /// A press in the events opens the item under it, and the row count follows.
    ///
    /// # Invariant
    ///
    /// The window draws what the log holds: the press finds the block the frame
    /// drew at that cell (`Screen::item_at`), and opening it grows the window by
    /// exactly the rows the frame will now draw.
    #[test]
    fn a_press_in_the_events_opens_the_item_under_it() {
        let mut h = Headless::new();
        h.stream("t:c1", LineKind::ToolOutput, "", "a\nb\nc", true);
        h.measure("main", 8);
        let (width, height) = (40u16, 14u16);
        let slot = h
            .screen
            .slots(width, height)
            .into_iter()
            .find(|slot| slot.name == "main")
            .expect("the window");
        let folded = h.screen.panes.rows(View::Window("main"), 39);
        assert_eq!(folded, 1, "a sealed result is one summary row");

        let at = h
            .screen
            .item_at(width, height, slot.body.x, slot.body.y)
            .expect("a press there names the item");
        assert!(h.screen.panes.toggle_at(at));
        assert_eq!(
            h.screen.panes.rows(View::Window("main"), 39),
            5,
            "opened, three body rows plus the item's two borders"
        );
    }

    /// An open item owns its wheel even when it cannot move any further.
    #[test]
    fn an_inner_box_at_its_end_does_not_scroll_the_outer_pane() {
        let mut h = Headless::new();
        for row in 0..40 {
            h.screen
                .panes
                .line("main", LineKind::System, format!("other {row}"));
        }
        h.stream("c:c1", LineKind::ToolCall, "bash ", "bash", true);
        for row in 0..8 {
            h.screen.panes.resolve(
                "main",
                "c:c1",
                "",
                LineKind::ToolCall,
                &format!("line {row}\n"),
                false,
            );
        }
        h.screen
            .panes
            .resolve("main", "c:c1", "1.0s ", LineKind::ToolOutput, "", true);
        h.screen.panes.set_block_size(2);
        let (width, height) = (40u16, 14u16);
        assert!(h.screen.panes.toggle("c:c1"));
        let slot = h
            .screen
            .slots(width, height)
            .into_iter()
            .find(|slot| slot.name == "main")
            .expect("main's pane");
        let row = (slot.body.y..slot.body.bottom())
            .find(|row| {
                h.screen
                    .item_at(width, height, slot.body.x, *row)
                    .is_some_and(|at| h.screen.panes.expanded_at(at))
            })
            .expect("the expanded item on screen");
        for rows in [-100, 100] {
            h.screen.scroll_at(width, height, slot.body.x, row, rows);
            assert_eq!(h.pane_scroll("main"), 0, "small box owns its wheel");
        }
    }

    /// Large and unbounded inner boxes pass an exhausted wheel to the outer pane.
    #[test]
    fn a_large_or_unbounded_inner_box_chains_scroll_at_its_end() {
        for size in [7, 0] {
            let mut h = Headless::new();
            for row in 0..40 {
                h.screen
                    .panes
                    .line("main", LineKind::System, format!("other {row}"));
            }
            h.stream("c:c1", LineKind::ToolCall, "bash ", "bash", true);
            h.screen.panes.set_block_size(size);
            assert!(h.screen.panes.toggle("c:c1"));
            let outer = 10;
            h.screen.panes.get_mut("main").expect("main's pane").scroll = outer;
            // The pointer lands on this exact open item, so there is no outer
            // fallback until its own scroll is exhausted in the wheel's direction.
            assert!(!h.screen.panes.scroll_block(41, -1, 6), "block size {size}");
            assert_eq!(h.pane_scroll("main"), outer, "block size {size}");
        }
    }

    /// The cell a click lands on decides which of SPEC's gestures it is: a row
    /// toggles a window, a header makes it sticky.
    ///
    /// # Invariant
    ///
    /// One press, one answer: the window asks for the command the gesture is, and
    /// never for both. The events under a header are read, not clicked.
    #[test]
    fn a_press_answers_with_the_gesture_it_landed_on() {
        let mut h = Headless::new();
        h.screen.panes.layout(&Layout {
            listed: vec![listed("main"), listed("peer")],
            shown: vec!["main".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        let (width, height) = (60u16, 16u16);
        let (list, _) = crate::render::body(
            crate::render::areas(
                Rect::new(0, 0, width, height),
                1,
                crate::render::Chrome {
                    command: true,
                    command_rows: 1,
                    quota: false,
                },
            )[1],
            &h.screen.panes,
        );
        // The row of a shown session: a click there hides it.
        assert_eq!(
            h.screen
                .session_at_row(width, height, list.x, list.y)
                .map(|hit| hit.part),
            Some(Part::List),
            "every name is a row, shown or not"
        );
        let shown = h
            .screen
            .panes
            .get("main")
            .is_some_and(|pane| pane.kind == crate::panes::Kind::Shown);
        assert_eq!(
            toggle_line("main", shown),
            "/set_render_session --session main --show false"
        );
        // The header of the window beside it: a click there makes it sticky.
        let slot = h
            .screen
            .slots(width, height)
            .into_iter()
            .find(|slot| slot.name == "main")
            .expect("a window named main");
        let head = slot.header.expect("a header on the window");
        assert_eq!(
            h.screen
                .session_at_row(width, height, head.x + 1, head.y)
                .map(|hit| hit.part),
            Some(Part::Header)
        );
        assert_eq!(
            sticky_line("main"),
            "/set_sticky --name session --value main"
        );
        // And the events under it are nobody's click.
        assert_eq!(
            h.screen
                .session_at_row(width, height, slot.body.x + 1, slot.body.y)
                .map(|hit| hit.part),
            Some(Part::Pane)
        );
    }

    /// A click on a name reads that name, and the list holds every session.
    #[test]
    fn a_click_names_the_session_it_landed_on() {
        let mut panes = Panes::new();
        panes.layout(&Layout {
            listed: vec![
                listed("main"),
                listed("peer"),
                listed("third"),
                listed("fourth"),
            ],
            shown: vec!["main".to_string(), "fourth".to_string()],
            command: "cmd".to_string(),
            quota: None,
        });
        // The list is the body's left column, one row per name from its top.
        let area = Rect::new(0, 0, 80, 24);
        let parts = crate::render::areas(
            area,
            1,
            crate::render::Chrome {
                command: true,
                command_rows: 1,
                quota: false,
            },
        );
        let (list, _) = crate::render::body(parts[1], &panes);
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.y + 1),
            Some("peer".to_string()),
            "a click on a name reads that name"
        );
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.y),
            Some("main".to_string()),
            "a shown session is still a row: the list is where it is hidden"
        );
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.y - 1),
            None,
            "a row outside the list names nobody"
        );
        // A row below the last name names nobody.
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.bottom() - 1),
            None
        );
    }

    /// SPEC's two clicks: a name toggles its window, a header points the reading
    /// at it.
    #[test]
    fn a_name_toggles_its_window_and_a_header_makes_it_sticky() {
        assert_eq!(
            toggle_line("peer", true),
            "/set_render_session --session peer --show false",
            "a shown session is clicked to hide it"
        );
        assert_eq!(
            toggle_line("peer", false),
            "/set_render_session --session peer --show true"
        );
        assert_eq!(
            sticky_line("peer"),
            "/set_sticky --name session --value peer"
        );
    }

    /// The two lines a click can ask for are commands, not prose.
    #[test]
    fn a_click_is_a_command_line() {
        for line in [toggle_line("peer", true), sticky_line("peer")] {
            assert!(
                matches!(line.strip_prefix('/'), Some(tokens) if tokens.starts_with("set_")),
                "{line} was read as prose"
            );
        }
    }

    /// The reference is exactly the spelling the server's Meta parser reads.
    ///
    /// # Invariant
    ///
    /// `@image: <one word>` and nothing else: the server replaces it with a file
    /// id when the message is sent, and it reads the path as one word. The path
    /// is printed as given -- this helper does not rewrite a separator.
    #[test]
    fn an_image_reference_is_the_meta_spelling() {
        assert_eq!(
            image_reference(Path::new(r"C:\Users\me\pic.png")),
            r"@image: C:\Users\me\pic.png"
        );
        assert_eq!(
            image_reference(Path::new("/tmp/gongga/pic.png")),
            "@image: /tmp/gongga/pic.png"
        );
    }

    /// A pasted picture lands in the prompt as text and is not sent.
    ///
    /// # Invariant
    ///
    /// The reference is inserted whole, the way a paste is: the server turns it
    /// into a file id when the message is sent, so nothing is submitted here.
    #[test]
    fn a_pasted_image_is_inserted_not_submitted() {
        let mut h = Headless::new();
        h.paste_image(Path::new("/tmp/gongga/pic.png"));
        assert_eq!(h.screen.input.text(), "@image: /tmp/gongga/pic.png");
        // The input line still holds it: nothing took it and sent it.
        assert!(!h.screen.input.is_empty());
    }

    /// A pasted picture closes the completion popup, like a pasted text does.
    ///
    /// # Invariant
    ///
    /// The popup was about the token before the paste, not about what the
    /// reference means now.
    #[test]
    fn a_pasted_image_closes_the_completion_popup() {
        let mut h = Headless::new();
        h.screen.completion = Some(Completion {
            start: 0,
            end: 4,
            candidates: vec![Candidate::word("replacement", "")],
            selected: 0,
        });
        h.paste_image(Path::new("/tmp/gongga/pic.png"));
        assert!(h.screen.completion.is_none());
    }

    /// Only a saved picture is a reference; the other three outcomes leave the
    /// text branch to run.
    ///
    /// # Invariant
    ///
    /// A clipboard that holds no picture, a machine with no tool to read one, and
    /// a tool that refused all reach the text branch, which is where the
    /// reporting lives.
    #[test]
    fn only_a_saved_picture_is_a_reference() {
        let saved = clipboard::ImageClipboard::Saved(PathBuf::from("/tmp/pic.png"));
        assert_eq!(
            image_reference_of(&saved),
            Some("@image: /tmp/pic.png".to_string())
        );
        for (name, image) in [
            ("no image", clipboard::ImageClipboard::NoImage),
            ("no tool", clipboard::ImageClipboard::NoTool),
            (
                "a tool that refused",
                clipboard::ImageClipboard::Failed("no picture here".to_string()),
            ),
        ] {
            assert!(
                image_reference_of(&image).is_none(),
                "{name} must leave the text branch alone"
            );
        }
    }

    /// A tool that refused is not swallowed: its own words reach the status line.
    ///
    /// # Invariant
    ///
    /// The image read is tried before the text read, and the text read reports
    /// for itself. But a refusal carries words only the image tool had, and when
    /// the text read has nothing to say, those words are the only account of what
    /// happened -- so they are shown.
    #[test]
    fn a_refused_image_read_is_reported() {
        let refused = clipboard::ImageClipboard::Failed("the clipboard said no".to_string());
        assert_eq!(image_failure_of(&refused).as_deref(), Some("the clipboard said no"));
        // The other three answers have nothing to report.
        assert!(image_failure_of(&clipboard::ImageClipboard::NoImage).is_none());
        assert!(image_failure_of(&clipboard::ImageClipboard::NoTool).is_none());
        assert!(
            image_failure_of(&clipboard::ImageClipboard::Saved(PathBuf::from("/tmp/pic.png")))
                .is_none()
        );
    }

    #[test]
    fn completer_drives_the_popup() {
        let c: SharedCompleter = std::sync::Arc::new(StaticCompleter(vec![Candidate::word(
            "send_message",
            "Send",
        )]));
        let mut h = Headless::new();
        h.screen.input.insert_str("send");
        let candidates = c.complete(h.screen.input.text(), 4);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].insert, "send_message");
        // An empty result keeps the popup shut.
        h.screen.input.clear();
        assert!(c.complete("", 0).is_empty());
    }
}
