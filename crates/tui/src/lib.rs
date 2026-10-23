//! `gongga-tui`: the terminal front-end.
//!
//! This crate is deliberately ignorant. It renders lines, keeps an input line,
//! and shows a completion popup whose contents somebody else supplies. It has
//! no dependency on the wire protocol and no idea what a command is: the
//! command layer (`gongga-cli`) owns the catalog, the wire, and the meaning of
//! every line, and hands both completion candidates and typed lines over.
//!
//! That is why the dependency runs this way -- `tui` depends on `cli`, never
//! the reverse. A command layer that knew about terminals could not be used
//! from a script; a front-end that knew about commands would have to change
//! every time one is added.
//!
//! The `gongga` binary lives here because it is the composition root: the only
//! program that needs both halves, and linking them is what a binary is for.

mod input;
mod log;
mod render;
pub mod run;

use std::collections::HashMap;
use std::io::{Stdout, stdout};

use anyhow::Context;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
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

// The vocabulary belongs to the command layer: this crate paints whatever it is
// told to paint. Re-exported so `crate::LineKind` and friends keep reading the
// same from inside this crate -- the renderer and the log never have to know
// where the words came from.
pub use cli::{Candidate, Completer, Completion, LineKind, SharedCompleter, token_bounds};

/// Something the operator did.
///
/// There is deliberately no "cancel" here. Stopping work is a command like any
/// other (`interrupt_session`, `cancel`), so it reaches the server by the same
/// path as everything else and can be typed by a script that has no keyboard.
/// The only thing left to the keyboard is leaving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A finished input line.
    Submit(String),
    /// Ctrl+Q: leave.
    Quit,
}

/// Everything the renderer reads. Kept separate so drawing never borrows the
/// terminal while the caller is mutating state.
pub struct Screen {
    /// The scrollback. Its own type because a frame must not be allowed to walk
    /// it: see [`crate::log`] for why that is the difference between a usable
    /// window and a frozen one.
    pub log: Log,
    pub completion: Option<Completion>,
    pub input: InputLine,
    pub status: String,
    pub footer: String,
    pub placeholder: String,
    /// How far the log is scrolled: 0 is pinned to the newest line, `n` is `n`
    /// wrapped rows up into the past.
    pub scroll: usize,
    /// How far up the log *can* be scrolled right now, written back by the
    /// renderer, which is the only place that knows how many rows the lines
    /// wrap into and how tall the log window is. Scrolling keys clamp against
    /// it so a page-up cannot strand the view past the top.
    pub max_scroll: usize,
}

impl Screen {
    /// Positive scrolls into the past, negative back towards the newest line,
    /// and both stop at the ends. The upper bound is the renderer's, since
    /// only it knows how many rows the lines wrap into.
    pub fn scroll_by(&mut self, rows: i64) {
        let next = (self.scroll as i64 + rows).clamp(0, self.max_scroll as i64);
        self.scroll = next as usize;
    }
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    screen: Screen,
    completer: SharedCompleter,
    /// Open stream key -> index into `screen.log`, so deltas append in place.
    streams: HashMap<String, usize>,
    history: Vec<String>,
    history_pos: Option<usize>,
    events: EventStream,
    restored: bool,
    /// Whether anything on screen changed since the last frame.
    ///
    /// A redraw is a terminal write, and while a response streams the loop gets
    /// an event for every chunk. Painting a frame nothing has changed is pure
    /// cost, so the frame is skipped instead — the flag is what keeps that from
    /// freezing the display: every mutation and every key sets it.
    dirty: bool,
}

impl Tui {
    pub fn new(completer: SharedCompleter) -> anyhow::Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        let mut out = stdout();
        // Mouse capture is what makes the wheel a scroll key. Without it
        // crossterm never sees the wheel at all -- the terminal keeps the
        // events for its own scrollback, which an alternate screen has none of.
        execute!(out, EnterAlternateScreen, EnableMouseCapture).context("entering alternate screen")?;
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
            streams: HashMap::new(),
            history: Vec::new(),
            history_pos: None,
            events: EventStream::new(),
            restored: false,
            // The first frame has to happen even though nothing has changed.
            dirty: true,
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Both of these only ask for a frame when the text actually changed: the
    /// caller recomputes the status from the session list as events arrive, and
    /// most of those recomputations say the same thing as the last one.
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        if status != self.screen.status {
            self.screen.status = status;
            self.dirty = true;
        }
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

    /// Puts a line back into the prompt, cursor at the end.
    ///
    /// The counterpart of Enter: a line that could not be sent is not swallowed
    /// by a log entry. It goes back where it was typed, because the operator
    /// already knows how to press Enter again and does not want to retype it.
    pub fn restore_input(&mut self, text: &str) {
        self.screen.completion = None;
        self.screen.input.clear();
        self.screen.input.insert_str(text);
        self.screen.input.end();
        self.dirty = true;
    }

    /// Appends to an open stream, creating the line on first use.
    ///
    /// `key` identifies the stream (a response part, a tool's stdout) and
    /// `done` closes it, so the next chunk with the same key starts a new line.
    pub fn stream(
        &mut self,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        stream_into(
            &mut self.screen.log,
            &mut self.streams,
            key,
            kind,
            prefix,
            text,
            done,
        );
        self.dirty = true;
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        // Nothing changed, so nothing is written. This is what keeps a stream
        // that arrives faster than the terminal can paint from spending the
        // whole loop in the renderer.
        if !self.dirty {
            return Ok(());
        }
        // The renderer writes the scroll bound back into the screen, which is
        // why it takes it mutably even though it only reads the log.
        self.terminal.draw(|f| render::draw(f, &mut self.screen))?;
        // Cleared only after the frame landed: a failed paint must be retried
        // by the next call rather than counted as done.
        self.dirty = false;
        Ok(())
    }

    /// Waits for the operator. Paints a frame before every wait, so lines
    /// pushed by the caller show up even with nobody at the keyboard, and after
    /// any event that could have changed what is on screen.
    pub async fn next_event(&mut self) -> Option<UiEvent> {
        loop {
            if self.draw().is_err() {
                return None;
            }
            let next = self.events.next().await?;
            // Only key events used to be looked at, which is why the wheel did
            // nothing: it arrives as a mouse event and was dropped here. A
            // resize needs no handling of its own beyond the redraw it asks
            // for -- the layout is worked out from the area every frame, and
            // the log reflows to the new width when it is asked for rows.
            match next {
                Ok(TermEvent::Key(key)) => {
                    // Windows reports a Release in addition to every Press, and
                    // some rebuilds also emit Release on other platforms.
                    // Without this filter one keystroke moves the cursor twice
                    // or types twice.
                    if !Self::actionable(&key) {
                        continue;
                    }
                    if let Some(action) = self.handle_key(key) {
                        return Some(action);
                    }
                    self.dirty = true;
                }
                Ok(TermEvent::Mouse(mouse)) => self.handle_mouse(mouse),
                Ok(TermEvent::Resize(..)) => self.dirty = true,
                _ => {}
            }
        }
    }

    /// Whether a key event should drive the UI.
    ///
    /// Only presses matter. `Release` is dropped so that a single keystroke is
    /// handled once; `Repeat` (a held key) is kept, because holding Backspace
    /// or an arrow is expected to keep working.
    fn actionable(key: &KeyEvent) -> bool {
        !matches!(key.kind, KeyEventKind::Release)
    }

    /// The keys that are not editing at all: only leaving, which is the one
    /// action this window owns rather than asks the server for.
    ///
    /// Pure and separate from [`Self::handle_key`] because it is the part worth
    /// testing without a terminal: what a key means must not depend on whether
    /// a TTY exists. It also collects both spellings of Ctrl+Q -- a terminal
    /// that reports no modifiers sends `DC1` (`\u{11}`) instead of `'q'`+Ctrl,
    /// and the two have to mean the same thing.
    fn shortcut(key: &KeyEvent) -> Option<UiEvent> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // A terminal that reports no modifiers sends DC1 for Ctrl+Q, so
            // this spelling does not need the flag to be set.
            KeyCode::Char('\u{11}') => Some(UiEvent::Quit),
            // Case-insensitive: Shift+Ctrl+Q arrives as 'Q'.
            KeyCode::Char('q' | 'Q') if ctrl => Some(UiEvent::Quit),
            _ => None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<UiEvent> {
        if let Some(action) = Self::shortcut(&key) {
            return Some(action);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('k') => self.screen.input.kill_to_end(),
                KeyCode::Left => self.screen.input.word_left(),
                // Paging on the same arrows that already mean "history" and
                // "cursor": Ctrl+arrows, because PgUp/PgDn on a laptop hide
                // behind Fn, where some of them are not even wired to scroll.
                // Plain ↑↓ stay with history -- that is what a shell does with
                // them, and it is the one thing operators try first.
                KeyCode::Up => self.screen.scroll_by(self.page_height() as i64),
                KeyCode::Down => self.screen.scroll_by(-(self.page_height() as i64)),
                _ => {}
            }
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
                // Arrows belong to the completion popup and to history, which
                // is what a shell does with them. The log needs a key of its
                // own, so Shift is what says "move the view, not the line" --
                // it is the only way to scroll on keyboards where PgUp/PgDn
                // hide behind Fn.
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.screen.scroll_by(1);
                } else if let Some(c) = self.screen.completion.as_mut() {
                    c.move_selection(-1);
                } else {
                    self.history_prev();
                }
            }
            KeyCode::Down => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.screen.scroll_by(-1);
                } else if let Some(c) = self.screen.completion.as_mut() {
                    c.move_selection(1);
                } else {
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
            // A page is the log window, not a fixed ten rows: on a short
            // terminal ten rows scrolls past what is visible and the jump
            // looks arbitrary.
            KeyCode::PageUp => self.screen.scroll_by(self.page_height() as i64),
            KeyCode::PageDown => self.screen.scroll_by(-(self.page_height() as i64)),
            _ => {}
        }
        None
    }

    /// The wheel, which is the scroll control everybody tries first.
    ///
    /// A notch is three rows; Shift slows it to one, the way a terminal's own
    /// scrollback behaves. Nothing else the mouse does is wired to anything:
    /// clicks are not actions in a command line.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let Some(rows) = Self::wheel_rows(&mouse) {
            self.screen.scroll_by(rows);
            self.dirty = true;
        }
    }

    /// What a wheel event meant, in rows: positive is into the past. `None`
    /// for everything else the mouse can report, none of which is an action
    /// here.
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

    /// What one page-up should move: the height of the log window itself.
    fn page_height(&self) -> usize {
        let Ok(size) = self.terminal.size() else {
            return 1;
        };
        size.height.saturating_sub(render::NON_LOG_ROWS).max(1) as usize
    }

    fn refresh_completion(&mut self) {
        let candidates = self
            .completer
            .complete(self.screen.input.text(), self.screen.input.cursor());
        if candidates.is_empty() {
            self.screen.completion = None;
            return;
        }
        let (start, end, _) = token_bounds(self.screen.input.text(), self.screen.input.cursor());
        self.screen.completion = Some(Completion {
            start,
            end,
            candidates,
            selected: 0,
        });
    }

    fn close_completion(&mut self) {
        self.screen.completion = None;
    }

    fn apply_completion(&mut self) {
        if let Some(c) = self.screen.completion.take() {
            apply_completion(&mut self.screen.input, &c);
        }
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
        // Leaving the capture on would keep the wheel (and text selection)
        // away from the shell this process returns to.
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
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

/// A completer that always declines. Handy for tests and for headless modes.
pub struct NoCompleter;

impl Completer for NoCompleter {
    fn complete(&self, _line: &str, _cursor: usize) -> Vec<Candidate> {
        Vec::new()
    }
}

/// The stream bookkeeping, kept out of [`Tui`] so a test can drive it without a
/// terminal — and so the test drives *this* code rather than a copy of it.
///
/// A closed stream must not append to a line that has since moved: `done`
/// removes the key before the next chunk looks it up, which is what makes two
/// consecutive responses end up as two lines rather than one.
fn stream_into(
    log: &mut Log,
    streams: &mut HashMap<String, usize>,
    key: &str,
    kind: LineKind,
    prefix: &str,
    text: &str,
    done: bool,
) {
    match streams.get(key) {
        // In place, and only the new bytes are wrapped: a delta must cost what
        // the delta is worth, not what the line has grown to.
        Some(&idx) => log.append(idx, text),
        // An empty first chunk still opens the line when the stream is not
        // finished, so a slow first token does not lose its `prefix`.
        None if !text.is_empty() || !done => {
            log.push(kind, format!("{prefix}{text}"));
            streams.insert(key.to_string(), log.len() - 1);
        }
        None => {}
    }
    if done {
        streams.remove(key);
    }
}

#[cfg(test)]
mod layering {
    /// The arrow this crate exists to point: the front-end depends on the
    /// command layer, never the other way round. A comment cannot enforce that;
    /// a manifest can be read.
    #[test]
    fn the_front_end_depends_on_the_command_layer() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("cli = { workspace = true }"), "{manifest}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the key handler without a terminal, which is the only part of the
    /// UI that has logic worth testing.
    struct Headless {
        screen: Screen,
        streams: HashMap<String, usize>,
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
                streams: HashMap::new(),
            }
        }

        fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
            stream_into(
                &mut self.screen.log,
                &mut self.streams,
                key,
                kind,
                prefix,
                text,
                done,
            );
        }

        /// Every row the log holds, unwrapped at a width none of these cases
        /// reach — which is what the assertions are about.
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

    #[test]
    fn deltas_on_one_key_become_one_line() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "he", false);
        h.stream("r:1", LineKind::Assistant, "", "llo", false);
        assert_eq!(h.rows(), vec!["hello"]);
    }

    #[test]
    fn done_closes_the_stream() {
        let mut h = Headless::new();
        h.stream("r:1", LineKind::Assistant, "", "a", true);
        h.stream("r:1", LineKind::Assistant, "", "b", true);
        assert_eq!(h.rows(), vec!["a", "b"]);
    }

    /// Two streams open at once have to stay separate: the index map is what
    /// lets a tool's output and a response interleave without either one
    /// appending to the other's line.
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

    /// Leaving is the one thing the keyboard still does on its own, and Ctrl+Q
    /// is the only key that does it.
    ///
    /// Pinned down here rather than in prose because the TUI needs a real
    /// terminal to run: without a test like this, "Ctrl+Q quits" is only a
    /// claim, and the two bindings that were removed could come back unnoticed.
    #[test]
    fn ctrl_q_is_the_only_way_out() {
        let quit = Some(UiEvent::Quit);
        // The plain case, and the shifted one, which arrives as 'Q'.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            quit
        );
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(
                KeyCode::Char('Q'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            quit
        );
        // A terminal that sends the control code instead of 'q'+Ctrl.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('\u{11}'), KeyModifiers::NONE)),
            quit
        );
        // A bare q is a letter to type, not an exit.
        assert_eq!(
            Tui::shortcut(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
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

    /// The wheel is the scroll control everybody reaches for first, so what one
    /// notch means is worth pinning down.
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

    /// Pag Up pressed ten times must not leave the view past the top, and
    /// PgDn must not walk off the bottom: both ends stop the scroll.
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
