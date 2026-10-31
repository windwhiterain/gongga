//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `cli`, never the reverse, so the command layer stays usable
//! from a script and a new command never changes this crate.

mod clipboard;
mod input;
mod log;
mod panes;
pub mod program;
mod render;
pub mod run;

use std::io::{self, Stdout, Write, stdout};
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
    Candidate, Completer, Completion, Layout, LineKind, Listed, QuotaRow, RenderMode,
    SharedCompleter, token_bounds,
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
    /// One session's pane of events.
    Pane,
    /// One session's line group.
    Line,
    /// The way back to the newest row, drawn over a pane's last row.
    Bottom,
}

/// What a pointer landed on: the session it names, and which part it hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub name: String,
    pub part: Part,
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
    fn slots(&self, width: u16, height: u16) -> Vec<render::Slot> {
        let area = Rect::new(0, 0, width, height);
        let input = render::input_rows(self.input.text(), width, height);
        let chunks = render::areas(area, input, self.panes.chrome());
        let (_, panes) = render::body(chunks[1], &self.panes);
        render::pane_slots(panes, &self.panes, self.sticky_session.as_deref()).0
    }

    /// Where a pane's rows come from and how far it is scrolled.
    fn pane_at(&self, name: &str) -> Option<(String, usize)> {
        let pane = self.panes.get(name)?;
        Some((pane.name.clone(), pane.scroll))
    }

    /// The pane covering this cell, which is where the wheel landed.
    pub fn pane_at_row(&self, width: u16, height: u16, column: u16, row: u16) -> Option<String> {
        render::pane_at(&self.slots(width, height), column, row).map(str::to_string)
    }

    /// The session a click on `row` names, from the list, a pane, or a group.
    pub fn session_at_row(&self, width: u16, height: u16, column: u16, row: u16) -> Option<Hit> {
        let area = Rect::new(0, 0, width, height);
        let input = render::input_rows(self.input.text(), width, height);
        let chunks = render::areas(area, input, self.panes.chrome());
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
        if let Some(name) = render::pane_at(&slots, column, row) {
            return Some(Hit {
                name: name.to_string(),
                part: Part::Pane,
            });
        }
        render::line_at(chunks[2], &self.panes, column, row).map(|name| Hit {
            name,
            part: Part::Line,
        })
    }

    /// Scrolls one pane: the wheel reads what the pointer is over, not the keys'.
    pub fn scroll_pane(&mut self, name: &str, rows: i64) {
        self.panes.scroll_by(name, rows);
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
        window.rows.iter().map(|(_, row)| row.to_string()).collect()
    }

    /// The rows the sticky session gets in a frame this size: what a page means.
    pub fn page_height(&self, width: u16, height: u16) -> usize {
        let Some(name) = self.sticky_session.as_deref() else {
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
        self.screen
            .panes
            .stream(name, key, kind, prefix, text, done);
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

    /// Whether this key is SPEC's delete: Shift+D, which arrives as an uppercase 'D'.
    fn delete_key(key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::SHIFT) && matches!(key.code, KeyCode::Char('d' | 'D'))
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
                KeyCode::Up => self.screen.scroll_by(self.page_height() as i64),
                KeyCode::Down => self.screen.scroll_by(-(self.page_height() as i64)),
                _ => {}
            }
            return None;
        }

        // Ctrl+<letter> must not insert its letter where a terminal delivers it.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        // With nothing typed to edit, Shift+D acts on the sticky session: SPEC's delete.
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
            KeyCode::PageUp => self.screen.scroll_by(self.page_height() as i64),
            KeyCode::PageDown => self.screen.scroll_by(-(self.page_height() as i64)),
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
            // The wheel writes no state; a list has no scroll of its own.
            if hit.part != Part::List {
                self.screen.scroll_pane(&hit.name, rows);
            }
            self.dirty = true;
            return;
        }
        self.dirty = true;
        // The way back to the newest row is not a click on the session under it.
        if hit.part == Part::Bottom {
            self.screen.panes.pin(&hit.name);
            return;
        }
        // Again on the sticky session: walk its mode.
        self.clicked = Some(
            if self.screen.sticky_session.as_deref() == Some(hit.name.as_str()) {
                let drawn = self.screen.panes.get(&hit.name).map(|pane| pane.kind);
                cycle_mode_line(&hit.name, drawn)
            } else {
                click_line(&hit.name)
            },
        );
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
    fn page_height(&self) -> usize {
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
        // The popup was about the token before the paste, not about what it means now.
        self.screen.completion = None;
        self.screen.input.insert_str(&text);
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
fn click_line(session: &str) -> String {
    format!("/set_sticky --name session --value {session}")
}

/// What a second click on the sticky session asks for: its next render mode.
///
/// # Invariant
///
/// The cycle is SPEC's own -- a session nobody set is `full`, `full` goes to
/// `line`, `line` to `none`, `none` back to `full` -- so clicking a name walks it
/// round rather than jumping. What the window knows is what it draws: a session
/// with a pane is `full`, one with a group is `line`, and one in neither is `none`.
/// See `docs/notes/tui-lib.md`.
fn cycle_mode_line(session: &str, drawn_as: Option<crate::panes::Kind>) -> String {
    let next = match drawn_as {
        Some(crate::panes::Kind::Full) => "line",
        Some(crate::panes::Kind::Line) => "none",
        _ => "full",
    };
    format!("/set_render_session --session {session} --mode {next}")
}

/// The command line Shift+D asks for: the sticky session's own deletion.
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
            placeholder: String::new(),
            sticky_session: None,
        };
        screen.input.insert_str("hi");
        present(&mut terminal, &mut screen, false).unwrap();

        let area = Rect::new(0, 0, 40, 12);
        let row = render::input_area(area, &screen).y;
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
                none: Vec::new(),
                full: vec!["main".to_string()],
                line: Vec::new(),
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
                .map(|(_, text)| text.to_string())
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
        assert_eq!(h.rows(), vec!["[peer] thinking: hello!"]);
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
                "[peer] thinking: 我先看一下。",
                "[test1] thinking: 确认完毕",
                "[peer] ok",
                "[test1] 这是我的",
            ]
        );
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
        let shown: Vec<String> = rows.iter().map(|(_, t)| t.to_string()).collect();
        assert_eq!(shown, vec!["[peer] aaaccc", "[test1] bbb"]);
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
        assert_eq!(h.rows(), vec!["hi"]);
    }

    /// Two streams open at once stay separate: a tool's output never joins a response.
    #[test]
    fn two_open_streams_do_not_mix() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "answer", false);
        h.stream("t:1", LineKind::ToolOutput, "$ ", "stdout", false);
        h.stream("r:1", LineKind::Assistant, "", " more", false);
        assert_eq!(h.rows(), vec!["answer more", "$ stdout"]);
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

    /// Deleting the sticky session is Shift+D; a bare d is a letter to type.
    ///
    /// # Invariant
    ///
    /// No unmodified key deletes a session: Delete and Backspace are the editor's,
    /// and a session is the one thing on screen a keystroke cannot take back.
    #[test]
    fn shift_d_is_the_delete_key() {
        // Shift+D arrives as an uppercase 'D'; a terminal that keeps the lowercase
        // letter still means the same binding.
        for code in [KeyCode::Char('D'), KeyCode::Char('d')] {
            assert!(Tui::delete_key(&KeyEvent::new(code, KeyModifiers::SHIFT)));
        }
        // Everything the old binding had: bare d, Delete and Backspace.
        for code in [KeyCode::Char('d'), KeyCode::Delete, KeyCode::Backspace] {
            assert!(
                !Tui::delete_key(&KeyEvent::new(code, KeyModifiers::NONE)),
                "{code:?} must not delete a session"
            );
        }
        // Shift with another modifier is that other key first.
        assert!(!Tui::delete_key(&KeyEvent::new(
            KeyCode::Char('D'),
            KeyModifiers::CONTROL
        )));
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

    /// A second click on the sticky session asks for its next render mode.
    #[test]
    fn a_second_click_on_the_sticky_session_asks_for_the_next_mode() {
        use crate::panes::Kind;
        // One click: set it sticky.
        assert_eq!(
            click_line("peer"),
            "/set_sticky --name session --value peer"
        );
        // Again: walk the cycle SPEC names, from what the window draws.
        assert_eq!(
            cycle_mode_line("peer", Some(Kind::Full)),
            "/set_render_session --session peer --mode line",
            "a pane goes to a line"
        );
        assert_eq!(
            cycle_mode_line("peer", Some(Kind::Line)),
            "/set_render_session --session peer --mode none",
            "a line goes to nothing"
        );
        assert_eq!(
            cycle_mode_line("peer", None),
            "/set_render_session --session peer --mode full",
            "and nothing comes back to a pane"
        );
    }

    /// One announcement entry: a name, idle, with nothing measured yet.
    fn listed(name: &str) -> Listed {
        Listed {
            name: name.to_string(),
            status: "idle".to_string(),
            title: String::new(),
        }
    }

    /// A click asks for the sticky session, as the command line that sets it.
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
            // SPEC's list is the `none` mode, so these are its rows and the
            // sessions drawn elsewhere are not among them.
            none: vec!["peer".to_string(), "third".to_string()],
            full: vec!["main".to_string()],
            line: vec!["fourth".to_string()],
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
                line_groups: 1,
                quota: false,
            },
        );
        let (list, _) = crate::render::body(parts[1], &panes);
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.y + 1),
            Some("third".to_string()),
            "a click on a name reads that name"
        );
        assert_eq!(
            crate::render::list_at(list, &panes, None, list.x, list.y),
            Some("peer".to_string())
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
        // The line group is its own area, and a click there names its session.
        assert_eq!(
            crate::render::line_at(parts[2], &panes, parts[2].x, parts[2].y),
            Some("fourth".to_string())
        );
        // A press on a name becomes the command that sets it.
        assert_eq!(
            format!("/set_sticky --name session --value {}", "peer"),
            "/set_sticky --name session --value peer",
            "the loop reads a `/` line as a command"
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
