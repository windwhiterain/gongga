//! The terminal front-end: a Sink and the interface the client loop drives.
//! See `docs/notes/tui-run.md`.

use std::future::Future;
use std::pin::Pin;

use client::render_wire::{RenderMessage, UiIntent};
use client::runtime;
use client::{Layout, LineKind, Sink};

use crate::{Server, Tui, UiEvent, closing};
use client::window_state::State;

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

    fn settled(&mut self, id: u64, ok: bool) {
        self.0.settled(id, ok);
    }

    fn stopped(&mut self, session: &str, failed: bool) {
        self.0.stopped(session, failed);
    }

    fn transcript(&mut self, session: &str, created: i64, seq: i64) {
        self.0.stamp(session, created, seq);
    }

    fn rewind(&mut self, window: &str) {
        self.0.rewind(window);
    }

    fn older(&mut self, window: &str) {
        self.0.older(window);
    }

    fn landed(&mut self, window: &str, created: i64, drawn: i64, floor: i64) {
        self.0.landed(window, created, drawn, floor);
    }
}

/// Applies one frame to the terminal view, answering with an intent to send back.
///
/// # Invariant
///
/// The twelve drawing frames become `Sink` calls on the same `TuiSink` the
/// in-process loop used, so a pipe window draws a line exactly once. Control
/// frames are not drawings: they land on the window's own view state.
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
        RenderMessage::Stopped { session, failed } => sink.stopped(&session, failed),
        // The six control frames were answered by `apply_frame`.
        RenderMessage::Draft { .. }
        | RenderMessage::Server { .. }
        | RenderMessage::Repoint { .. }
        | RenderMessage::Vocabulary { .. }
        | RenderMessage::Completion { .. }
        | RenderMessage::StickySession { .. } => {}
    }
}

impl client::window_sync::WindowView for Tui {
    fn show_state(&mut self, state: &State) {
        self.set_block_size(state.block_size().0);
        self.set_box_edge(state.box_edge().0);
        self.set_sticky_session(state.sticky().get("session"));
    }
    fn refresh_completion(&mut self) {
        Tui::refresh_completion(self);
    }
    fn refresh_lines(&mut self, client: &client::Client) {
        client.refresh(&mut TuiSink(self));
    }
    fn take_settled(&mut self) -> Vec<(u64, bool)> {
        Tui::take_settled(self)
    }
}

impl runtime::Frontend for Tui {
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = runtime::FrontendEvent> + Send + '_>> {
        Box::pin(async move {
            match Tui::next_event(self).await {
                Some(UiEvent::Intent(intent)) => runtime::FrontendEvent::Intent(intent),
                Some(UiEvent::Recomplete) => runtime::FrontendEvent::Recomplete,
                None => runtime::FrontendEvent::Closed,
            }
        })
    }
    fn closing(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(closing::asked())
    }
    fn sink(&mut self) -> Box<dyn Sink + '_> {
        Box::new(TuiSink(self))
    }
    fn known_list(&self) -> Vec<client::Listed> {
        Tui::known_list(self)
    }
    // The terminal computes its own popup from its own input line.
    fn show_completion(&mut self, _completion: &client::Completion) {}
    fn set_server(&mut self, server: Option<Server>) {
        Tui::set_server(self, server);
    }
    fn repoint(&mut self, client_home: &std::path::Path, endpoint: &str) {
        Tui::repoint(self, client_home, endpoint);
    }
    fn restore_input(&mut self, text: &str) {
        Tui::restore_input(self, text);
    }
    fn has_draft(&self) -> bool {
        !self.screen().input.text().trim().is_empty()
    }
    fn server_is_ours(&self) -> bool {
        self.screen().server.is_some_and(Server::is_ours)
    }
    fn restore(&mut self) -> anyhow::Result<()> {
        Tui::restore(self)
    }
}

/// The window: the composition root that hands the client loop its one UI.
pub async fn interactive(args: &client::Args) -> anyhow::Result<()> {
    runtime::run(args, std::process::id(), |completer, home, endpoint| {
        Ok(Box::new(Tui::new(completer, home, endpoint)?) as Box<dyn runtime::Frontend>)
    })
    .await
}
