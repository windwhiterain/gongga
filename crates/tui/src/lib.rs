//! `gongga-tui`: the terminal front-end. See `docs/notes/tui-lib.md`.
//!
//! # Invariant
//!
//! `tui` depends on `cli`, never the reverse, so the command layer stays usable
//! from a script and a new command never changes this crate.

mod clipboard;
mod input;
mod log;
mod render;
pub mod run;

use std::io::{Stdout, stdout};
use std::time::Duration;

use anyhow::Context;
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
use ratatui::backend::CrosstermBackend;

pub use crate::input::{InputLine, apply_completion};
pub use crate::log::Log;
pub use crate::run::interactive;

// The vocabulary is the command layer's, re-exported so this crate can name it.
pub use cli::{Candidate, Completer, Completion, Detail, LineKind, SharedCompleter, token_bounds};

/// How long a keystroke waits to see whether the next one is part of a paste.
///
/// # Invariant
///
/// The wait only ever joins keys, never sends: one `Enter` alone still submits.
/// See `docs/notes/tui-paste.md`.
const PASTE_GAP: Duration = Duration::from_millis(10);

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

/// Everything the renderer reads, separate from the terminal it draws to.
pub struct Screen {
    /// The scrollback: a frame may not walk it. See [`crate::log`].
    pub log: Log,
    pub completion: Option<Completion>,
    pub input: InputLine,
    pub status: String,
    pub footer: String,
    pub placeholder: String,
    /// Scroll offset in wrapped rows: 0 is the newest line, `n` is `n` rows up.
    pub scroll: usize,
    /// Scroll bound, written back by the renderer; the keys clamp against it.
    pub max_scroll: usize,
}

impl Screen {
    /// Positive scrolls into the past, negative towards the newest line; clamped.
    pub fn scroll_by(&mut self, rows: i64) {
        let next = (self.scroll as i64 + rows).clamp(0, self.max_scroll as i64);
        self.scroll = next as usize;
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
    /// An event read while deciding whether a keystroke began a paste.
    stashed: Option<std::io::Result<TermEvent>>,
}

impl Tui {
    pub fn new(completer: SharedCompleter) -> anyhow::Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
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
        let terminal = Terminal::new(CrosstermBackend::new(out))?;
        Ok(Self {
            terminal,
            screen: Screen {
                log: Log::new(),
                completion: None,
                input: InputLine::default(),
                status: "connecting".to_string(),
                footer: String::new(),
                placeholder: "message the agent, or / for a command (Tab completes)".to_string(),
                scroll: 0,
                max_scroll: 0,
            },
            completer,
            history: Vec::new(),
            history_pos: None,
            events: EventStream::new(),
            mouse_captured: true,
            restored: false,
            // The first frame has to happen even though nothing has changed.
            dirty: true,
            pending: Vec::new(),
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

    /// How much of each event to show, as the command layer last said.
    pub fn set_detail(&mut self, mode: Detail) {
        self.screen.log.set_detail(mode);
        self.dirty = true;
    }

    pub fn set_footer(&mut self, footer: impl Into<String>) {
        let footer = footer.into();
        if footer != self.screen.footer {
            self.screen.footer = footer;
            self.dirty = true;
        }
    }

    /// Adds a finished line.
    pub fn push(&mut self, kind: LineKind, text: impl Into<String>) {
        self.screen.log.push(kind, text);
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

    /// Appends to the block `key` names, opening it on first use.
    ///
    /// Two blocks streaming at once both keep growing.
    pub fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
        self.screen.log.stream(key, kind, prefix, text, done);
        self.dirty = true;
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        // Nothing changed, so nothing is written: a fast stream cannot hog the loop.
        if !self.dirty {
            return Ok(());
        }
        // The renderer writes the scroll bound back, hence `&mut` on a read-only screen.
        self.terminal.draw(|f| render::draw(f, &mut self.screen))?;
        // Cleared only after the frame landed, so a failed paint is retried.
        self.dirty = false;
        Ok(())
    }

    /// Waits for the operator, painting a frame before every wait.
    pub async fn next_event(&mut self) -> Option<UiEvent> {
        loop {
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
        let typing = !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(_) => typing,
            KeyCode::Enter => key.modifiers.is_empty(),
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
        let action = match pasted(&keys) {
            Some(text) => {
                self.paste(&text);
                None
            }
            None => keys.into_iter().find_map(|key| self.handle_key(key)),
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
                KeyCode::Char('k' | 'K') => self.screen.input.kill_to_end(),
                KeyCode::Left => self.screen.input.word_left(),
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
                    self.screen.scroll_by(-1);
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
                self.screen.scroll = 0;
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
            KeyCode::Delete => self.screen.input.delete(),
            KeyCode::Left => self.screen.input.left(),
            KeyCode::Right => self.screen.input.right(),
            KeyCode::Home => self.screen.input.home(),
            KeyCode::End => self.screen.input.end(),
            // A page is the log window, not a fixed ten rows, so short terminals work too.
            KeyCode::PageUp => self.screen.scroll_by(self.page_height() as i64),
            KeyCode::PageDown => self.screen.scroll_by(-(self.page_height() as i64)),
            _ => {}
        }
        None
    }

    /// The wheel: a notch is three rows, Shift makes it one. See `docs/notes/tui-lib.md`.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let Some(rows) = Self::wheel_rows(&mouse) {
            self.screen.scroll_by(rows);
            self.dirty = true;
        }
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

    /// What one page-up should move: the rows of the log window itself.
    fn page_height(&self) -> usize {
        let Ok(size) = self.terminal.size() else {
            return 1;
        };
        // Measured against the rows left over, since the input's height varies.
        let input = render::input_rows(self.screen.input.text(), size.width, size.height);
        size.height
            .saturating_sub(render::CHROME_ROWS + input)
            .max(1) as usize
    }

    /// Breaks the line instead of sending it.
    /// Inserts a pasted block at the cursor, without sending it.
    ///
    /// # Invariant
    ///
    /// A paste is text, never a keystroke replay: a newline inside it must not
    /// press Enter, or pasting a programme would submit its first line and leave
    /// the rest in the prompt.
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
            self.push(LineKind::Error, "copy: the terminal has no size to read");
            return;
        };
        // One column short and minus the input box, as the renderer measures.
        let width = size.width.saturating_sub(1).max(1) as usize;
        let input = render::input_rows(self.screen.input.text(), size.width, size.height);
        let height = log_window_height(size.height, input);
        let view = self
            .screen
            .log
            .window(width, height.max(1), self.screen.scroll);
        let text = view
            .rows
            .iter()
            .map(|(_, row)| row.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        drop(view);
        match clipboard::copy(&text) {
            clipboard::Copied::Ok { chars, tool } => self.push(
                LineKind::System,
                format!("copied {chars} characters ({tool})"),
            ),
            clipboard::Copied::Empty => self.push(LineKind::System, "copy: the window is empty"),
            clipboard::Copied::NoTool => self.push(
                LineKind::Error,
                "copy: no clipboard tool here; Alt+M frees the mouse so the terminal can select",
            ),
            clipboard::Copied::Failed(why) => {
                self.push(LineKind::Error, format!("copy failed: {why}"))
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
            Ok(()) if self.mouse_captured => self.push(
                LineKind::System,
                "mouse: the app scrolls (Alt+M releases it for selecting)",
            ),
            Ok(()) => self.push(
                LineKind::System,
                "mouse: the terminal selects; drag to copy, Alt+M gives scrolling back",
            ),
            Err(e) => {
                // The flag must match reality, or the next toggle lies.
                self.mouse_captured = !self.mouse_captured;
                self.push(LineKind::Error, format!("mouse: {e}"));
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
        self.restored = true;
        disable_raw_mode()?;
        // Leaving capture on would keep the wheel from the shell we return to.
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // A panicking caller must not leave the terminal in raw mode.
        let _ = self.restore();
    }
}

/// Rows the log window covers, which is what a copy may take.
///
/// # Invariant
///
/// Must agree with the layout in `render::draw`: header, footer and help take one
/// row each, and the input box takes as many as its text needs. A copy that
/// assumed more would include rows the screen never drew.
fn log_window_height(terminal: u16, input_rows: u16) -> usize {
    terminal
        .saturating_sub(render::CHROME_ROWS + input_rows)
        .max(1) as usize
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

/// The text a run of keys spells, when the run is a paste rather than typing.
fn pasted(keys: &[KeyEvent]) -> Option<String> {
    // A lone key is what a keyboard sends; a paste is a break among other keys.
    if keys.len() < 2 || !keys.iter().any(|k| k.code == KeyCode::Enter) {
        return None;
    }
    let mut text = String::new();
    for key in keys {
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => text.push('\n'),
            KeyCode::Char(c) => text.push(c),
            // Anything else means this was not one pasted block after all.
            _ => return None,
        }
    }
    Some(text)
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

    /// Drives the key handler without a terminal: the only part with real logic.
    struct Headless {
        screen: Screen,
    }

    impl Headless {
        fn new() -> Self {
            Self {
                screen: Screen {
                    log: Log::new(),
                    completion: None,
                    input: InputLine::default(),
                    status: String::new(),
                    footer: String::new(),
                    placeholder: String::new(),
                    scroll: 0,
                    max_scroll: 0,
                },
            }
        }

        /// The same normalization `Tui::paste` does, on this stand-in's input.
        fn paste(&mut self, raw: &str) {
            self.screen.input.insert_str(&clipboard::paste_text(raw));
        }

        /// The same call `Tui::stream` makes, so a test drives the real path.
        fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
            self.screen.log.stream(key, kind, prefix, text, done);
        }

        /// Every row the log holds, unwrapped at a width these cases never reach.
        fn rows(&mut self) -> Vec<String> {
            self.screen
                .log
                .window(200, 100, 0)
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
        let rows = h.screen.log.window(40, 2, 0).rows;
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
        // What the renderer would have worked out for this content.
        h.screen.max_scroll = 20;
        h.screen.scroll_by(3);
        assert_eq!(h.screen.scroll, 3);
        h.screen.scroll_by(100);
        assert_eq!(h.screen.scroll, 20);
        h.screen.scroll_by(-5);
        assert_eq!(h.screen.scroll, 15);
        h.screen.scroll_by(-100);
        assert_eq!(h.screen.scroll, 0);
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
