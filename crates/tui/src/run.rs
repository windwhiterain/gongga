//! The terminal front-end: a Sink and the frames the client process sends.
//! See `docs/notes/tui-run.md`.

use client::render_wire::{RenderMessage, UiIntent};
use client::{Layout, LineKind, Sink};

use crate::Tui;

/// Renders the command layer's lines into the terminal.
struct TuiSink<'a>(&'a mut Tui);

impl Sink for TuiSink<'_> {
    fn line(&mut self, window: &str, kind: LineKind, text: &str) {
        self.0.push(window, kind, text);
    }

    fn stream(
        &mut self,
        window: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        self.0.stream(window, key, kind, prefix, text, done);
    }

    fn status(&mut self, text: &str) {
        // The server's own facts: a note this window made is over.
        self.0.set_status_kind(LineKind::System);
        self.0.set_status(text);
    }

    fn note(&mut self, kind: LineKind, text: &str) {
        self.0.say(kind, text);
    }

    fn layout(&mut self, layout: &Layout) {
        self.0.set_layout(layout);
    }

    fn rewrite(&mut self, window: &str, key: &str, text: &str) {
        self.0.rewrite(window, key, text);
    }
    fn tool_call(
        &mut self,
        window: &str,
        key: &str,
        head: &client::protocol::Head,
        _streamed: bool,
    ) {
        self.0.describe(window, key, head);
    }

    fn relabel(&mut self, window: &str, key: &str, tag: &str, kind: LineKind) {
        self.0.relabel(window, key, tag, kind);
    }

    fn clear(&mut self, window: &str) {
        self.0.clear(window);
    }

    fn forget(&mut self, window: &str) {
        self.0.forget(window);
    }

    fn stopped(&mut self, session: &str, failed: bool) {
        self.0.stopped(session, failed);
    }
}

/// Applies one frame, answering with an intent when the frame asks for one.
///
/// # Invariant
///
/// Drawing frames become `Sink` calls on the same `TuiSink`, so a line is drawn
/// exactly once. Control frames are not drawings: they land on view state.
pub(crate) fn apply_frame(tui: &mut Tui, frame: RenderMessage) -> Option<UiIntent> {
    match frame {
        RenderMessage::Draft { text } => {
            tui.restore_input(&text);
            None
        }
        RenderMessage::Server { label, .. } => {
            tui.set_server_text(label);
            None
        }
        RenderMessage::Repoint { .. } => {
            tui.drop_windows();
            None
        }
        // The words changed: ask again for the line the operator is on.
        RenderMessage::Vocabulary { .. } => Some(complete_intent(tui)),
        RenderMessage::Completion {
            start,
            end,
            subject,
            candidates,
        } => {
            let popup = (!candidates.is_empty()).then_some(client::Completion {
                start,
                end,
                subject,
                candidates,
                selected: 0,
            });
            tui.set_completion(popup);
            None
        }
        RenderMessage::StickySession { name } => {
            tui.set_sticky_session(name.as_deref());
            None
        }
        // A page of the past: newer than nothing, so it goes above what the
        // window holds instead of at its end.
        RenderMessage::Older { window, rows } => {
            tui.prepend_rows(&window, &rows);
            None
        }
        drawing => {
            let mut sink = TuiSink(tui);
            draw(&mut sink, drawing);
            None
        }
    }
}

/// The client's vocabulary is not this window's; it asks for its own line.
fn complete_intent(tui: &Tui) -> UiIntent {
    let (line, cursor) = tui.input_line();
    UiIntent::Complete { line, cursor }
}

/// Turns one drawing frame back into the `Sink` call that produced it.
fn draw(sink: &mut TuiSink<'_>, frame: RenderMessage) {
    match frame {
        RenderMessage::Line { window, kind, text } => sink.line(&window, kind, &text),
        RenderMessage::Stream {
            window,
            key,
            kind,
            prefix,
            text,
            done,
        } => sink.stream(&window, &key, kind, &prefix, &text, done),
        RenderMessage::Status { text } => sink.status(&text),
        RenderMessage::Note { kind, text } => sink.note(kind, &text),
        RenderMessage::Layout { layout } => sink.layout(&layout),
        RenderMessage::Render {
            block_size,
            box_edge,
        } => sink.0.set_render(block_size, box_edge),
        RenderMessage::Rewrite { window, key, text } => sink.rewrite(&window, &key, &text),
        RenderMessage::ToolCall {
            window,
            key,
            head,
            streamed,
        } => sink.tool_call(&window, &key, &head, streamed),
        RenderMessage::Relabel {
            window,
            key,
            tag,
            kind,
        } => sink.relabel(&window, &key, &tag, kind),
        RenderMessage::Forget { window } => sink.forget(&window),
        RenderMessage::Clear { window } => sink.clear(&window),
        RenderMessage::Rewind { window } => sink.0.rewind(&window),
        RenderMessage::Stopped { session, failed } => sink.stopped(&session, failed),
        // The control frames were answered by `apply_frame`.
        RenderMessage::Draft { .. }
        | RenderMessage::Server { .. }
        | RenderMessage::Repoint { .. }
        | RenderMessage::Vocabulary { .. }
        | RenderMessage::Completion { .. }
        | RenderMessage::Older { .. }
        | RenderMessage::StickySession { .. } => {}
    }
}
