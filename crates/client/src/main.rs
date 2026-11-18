//! `gongga-client`: the client process a UI starts, one pipe in and one out.
//!
//! See `docs/notes/client-pipe.md`.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use gongga_client::render_wire::{
    CursorMessage, PastRow, RenderMessage, UiIntent, WireSink, decode_intent, encode_frame,
};
use gongga_client::runtime::{self, Frontend, FrontendEvent, Server};
use gongga_client::window_journal::WindowJournal;
use gongga_client::window_refill::WindowFacts;
use gongga_client::window_roster::Roster;
use gongga_client::window_state::State;
use gongga_client::window_sync::WindowView;
use gongga_client::{Client, Layout, LineKind, Listed, Sink, protocol};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin};
use tokio::sync::mpsc;

/// The sentence this program answers `--help` with.
const ABOUT: &str = "Gongga client: JSON Lines from a UI, UDP to a server";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = gongga_client::parse_argv("gongga-client", ABOUT);
    if args.command.is_some() {
        anyhow::bail!("gongga-client speaks the pipe protocol; a command belongs to gongga-cli");
    }
    // The UI passes its own pid; the server follows that pid, not this one.
    let owner_pid = args.parent.unwrap_or_else(|| std::process::id());

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    // One writer owns stdout: Sink is synchronous and must never await a pipe.
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(frame) = rx.recv().await {
            if out.write_all(frame.as_bytes()).await.is_err() {
                break;
            }
            if out.flush().await.is_err() {
                break;
            }
        }
    });

    let lines = BufReader::new(tokio::io::stdin()).lines();
    let result = runtime::run(&args, owner_pid, |_, root, endpoint| {
        // A row's home never depends on a greeting; the client keeps both.
        let journal = WindowJournal::with_cache(root, endpoint);
        let roster = Roster::new(root, endpoint);
        Ok(Box::new(PipeFrontend::new(lines, tx, journal, roster)) as Box<dyn Frontend>)
    })
    .await;
    // The front-end drops its sender with the loop, so the writer drains and ends.
    let _ = writer.await;
    result
}

struct PipeFrontend {
    lines: Lines<BufReader<Stdin>>,
    out: mpsc::UnboundedSender<String>,
    journal: WindowJournal,
    roster: Roster,
    server: Option<Server>,
    sticky: Option<String>,
    /// The windows the last layout drew: a change on either side is a tree that
    /// has to be asked for again. See `docs/notes/client-pipe.md`.
    shown: Vec<String>,
    /// The cap and margin last put on the wire, so a repeat costs no frame.
    render: Option<(usize, usize, usize)>,
    draft: bool,
    layout: Option<Layout>,
    status: Option<String>,
    /// Every answer on screen, by window and key: the source a document is derived
    /// from. A body is rows, and rows are made from the whole text, so the text has to
    /// live here rather than in a front-end that may be replaced.
    answers: HashMap<String, HashMap<String, Answer>>,
    /// The columns each window's body is fitted to, as the pane last said.
    widths: HashMap<String, usize>,
}

/// One answer being written, and the last state of it a frame carried.
struct Answer {
    kind: LineKind,
    tag: String,
    text: String,
    done: bool,
}

/// What a window is fitted to before its pane has said: a comfortable line.
const DEFAULT_COLUMNS: usize = 80;

impl PipeFrontend {
    fn new(
        lines: Lines<BufReader<Stdin>>,
        out: mpsc::UnboundedSender<String>,
        journal: WindowJournal,
        roster: Roster,
    ) -> Self {
        Self {
            lines,
            out,
            journal,
            roster,
            server: None,
            sticky: None,
            shown: Vec::new(),
            render: None,
            draft: false,
            layout: None,
            status: None,
            answers: HashMap::new(),
            widths: HashMap::new(),
        }
    }

    /// Adds one delta of an answer, and re-states it as rows.
    ///
    /// # Invariant
    ///
    /// SPEC draws one kind as a document -- `output` -- and the rest as text. Only
    /// that kind is accumulated, and a key reused by another kind drops what it held:
    /// a stale body would be drawn where the new one belongs.
    fn answer(
        &mut self,
        window: &str,
        key: &str,
        kind: LineKind,
        tag: &str,
        delta: &str,
        done: bool,
    ) -> Option<RenderMessage> {
        if kind != LineKind::Assistant {
            self.drop_answer(window, key);
            return None;
        }
        let entry = self
            .answers
            .entry(window.to_string())
            .or_default()
            .entry(key.to_string())
            .or_insert_with(|| Answer {
                kind,
                tag: tag.to_string(),
                text: String::new(),
                done,
            });
        entry.tag = tag.to_string();
        entry.text.push_str(delta);
        entry.done = done;
        self.document_of(window, key)
    }

    /// Replaces an answer's whole text: what a rewrite of a derived body means.
    fn rewritten(&mut self, window: &str, key: &str, text: &str) -> Option<RenderMessage> {
        let entry = self
            .answers
            .get_mut(window)
            .and_then(|held| held.get_mut(key))?;
        entry.text.clear();
        entry.text.push_str(text);
        self.document_of(window, key)
    }

    fn drop_answer(&mut self, window: &str, key: &str) {
        if let Some(held) = self.answers.get_mut(window) {
            held.remove(key);
        }
    }

    fn drop_window(&mut self, window: &str) {
        self.answers.remove(window);
    }

    /// The columns a pane can draw: a width that changed re-states every body in it.
    fn refit(&mut self, window: &str, columns: usize) {
        if self.widths.insert(window.to_string(), columns.max(1)) == Some(columns.max(1)) {
            return;
        }
        let keys: Vec<String> = self
            .answers
            .get(window)
            .map(|held| held.keys().cloned().collect())
            .unwrap_or_default();
        for key in keys {
            self.emit_document(window, &key);
        }
    }

    /// Puts one body on the wire as the rows this window is fitted to.
    fn emit_document(&mut self, window: &str, key: &str) {
        if let Some(message) = self.document_of(window, key) {
            self.send(message);
        }
    }

    /// The frame that states one answer as rows, for the caller to queue.
    ///
    /// # Invariant
    ///
    /// Built here and sent by whoever got it: a document that overtook the row's
    /// own frames would be drawn into a window that is about to be cleared. See
    /// `docs/notes/client-pipe.md`.
    fn document_of(&mut self, window: &str, key: &str) -> Option<RenderMessage> {
        let width = self.widths.get(window).copied().unwrap_or(DEFAULT_COLUMNS);
        let entry = self.answers.get(window).and_then(|held| held.get(key))?;
        let rows = gongga_client::document::rows(&entry.text, width);
        Some(RenderMessage::Document {
            window: window.to_string(),
            key: key.to_string(),
            kind: entry.kind,
            tag: entry.tag.clone(),
            rows,
            done: entry.done,
        })
    }

    /// Puts one frame on the pipe. No cap and no `try_send`: dropping a frame
    /// would lose a line the operator is waiting for.
    fn send(&mut self, message: RenderMessage) {
        if let RenderMessage::Status { text } = &message {
            if self.status.as_ref() == Some(text) {
                return;
            }
            self.status = Some(text.clone());
        }
        if let RenderMessage::Layout { layout } = &message {
            if self.layout.as_ref() == Some(layout) {
                return;
            }
            self.layout = Some(layout.clone());
            // A window that leaves the frame holds nothing afterwards, and one
            // that joins it has no tree yet: both are declared empty, so the next
            // frame asks for the page instead of appending to a tree that is gone.
            for name in &self.shown {
                if !layout.shown.contains(name) {
                    self.journal.forget_held(name);
                }
            }
            for name in &layout.shown {
                if !self.shown.contains(name) {
                    self.journal.forget_held(name);
                }
            }
            self.shown = layout.shown.clone();
            self.journal.begin_frame();
            for name in &layout.shown {
                self.journal.mark_drawn(name);
            }
            // The list is a record of the past, written before any greeting.
            if let Err(e) = self.roster.know(&layout.listed) {
                eprintln!("client could not record the session list: {e}");
            }
        }
        if let RenderMessage::Forget { window } = &message {
            self.journal.forget(window);
        }
        match encode_frame(&message) {
            Ok(frame) => {
                let _ = self.out.send(frame);
            }
            Err(e) => eprintln!("client could not encode a frame: {e}"),
        }
    }

    /// Eats the cursor half: those answers are the client's, never the UI's.
    fn consume(&mut self, cursor: CursorMessage) {
        self.journal.consume(cursor);
    }

    fn note(&mut self, text: String) {
        self.send(RenderMessage::Note {
            kind: LineKind::Error,
            text,
        });
    }
}

impl WindowFacts for PipeFrontend {
    fn stamp_of(&self, window: &str) -> Option<(i64, i64)> {
        self.journal.stamp_of(window)
    }
    fn floor_of(&self, window: &str) -> Option<i64> {
        self.journal.floor_of(window)
    }
    fn room_of(&self, window: &str) -> usize {
        self.journal.room_of(window)
    }
    fn note_room(&mut self, window: &str, rows: usize) {
        self.journal.note_room(window, rows);
    }
    // A width is this daemon's own bookkeeping: the rows it already sent are refitted.
    fn note_columns(&mut self, window: &str, columns: usize) {
        self.refit(window, columns);
    }
    fn drawn(&self) -> &[String] {
        self.journal.drawn()
    }
    fn note_held(&mut self, window: &str, oldest: i64, newest: i64) {
        self.journal.note_held(window, oldest, newest);
    }
    fn events(&self, window: &str, before: u64, count: usize) -> Vec<(u64, protocol::Event)> {
        self.journal.events(window, before, count)
    }
    // Only a real vocabulary change is a frame; the UI asks for candidates itself.
    fn refresh_completion(&mut self, version: u64) {
        self.send(RenderMessage::Vocabulary { version });
    }
}

impl WindowView for PipeFrontend {
    fn show_state(&mut self, state: &State) {
        let name = state.sticky().get("session").map(str::to_owned);
        if name != self.sticky {
            self.sticky = name.clone();
            self.send(RenderMessage::StickySession { name });
        }
        let render = (
            state.block_size().0,
            state.box_edge().0,
            state.keep_rows().0,
        );
        if self.render != Some(render) {
            self.render = Some(render);
            self.send(RenderMessage::Render {
                block_size: render.0,
                box_edge: render.1,
                keep_rows: render.2,
            });
        }
    }
    // A popup is the UI's own; only a vocabulary change crosses the pipe.
    fn refresh_completion(&mut self) {}
    fn refresh_lines(&mut self, client: &Client) {
        let mut sink = PipeSink::new(self);
        client.refresh(&mut sink);
    }
    fn take_settled(&mut self) -> Vec<(u64, bool)> {
        self.journal.take_settled()
    }
}

impl Frontend for PipeFrontend {
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = FrontendEvent> + Send + '_>> {
        Box::pin(async move {
            loop {
                match self.lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => match decode_intent(&line) {
                        Ok(intent) => {
                            if matches!(&intent, UiIntent::Submit { .. }) {
                                self.draft = false;
                            }
                            return FrontendEvent::Intent(intent);
                        }
                        // A bad line is news, not a silently dropped frame.
                        Err(e) => self.note(format!("unreadable intent: {e}")),
                    },
                    Ok(None) => return FrontendEvent::Closed,
                    Err(e) => {
                        self.note(format!("the UI pipe ended: {e}"));
                        return FrontendEvent::Closed;
                    }
                }
            }
        })
    }

    // EOF on stdin is the UI going away; a Quit intent is the operator's.
    fn closing(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(std::future::pending())
    }
    fn sink(&mut self) -> Box<dyn Sink + '_> {
        Box::new(PipeSink::new(self))
    }
    // The roster is the client's own record of what a greeting replaced.
    fn known_list(&self) -> Vec<Listed> {
        self.roster.load()
    }
    fn show_completion(&mut self, completion: &gongga_client::Completion) {
        self.send(RenderMessage::Completion {
            line: completion.line.clone(),
            cursor: completion.cursor,
            start: completion.start,
            end: completion.end,
            subject: completion.subject.clone(),
            items: completion.items.clone(),
        });
    }
    fn set_server(&mut self, server: Option<Server>) {
        self.server = server;
        self.send(RenderMessage::Server {
            label: server.map(|s| s.label()),
            ours: server.is_some_and(Server::is_ours),
        });
    }
    fn repoint(&mut self, client_home: &Path, endpoint: &str) {
        self.journal.repoint(client_home, endpoint);
        self.roster.repoint(client_home, endpoint);
        self.layout = None;
        self.status = None;
        self.send(RenderMessage::Repoint {
            endpoint: endpoint.to_string(),
        });
    }
    fn restore_input(&mut self, text: &str) {
        self.draft = true;
        self.send(RenderMessage::Draft {
            text: text.to_string(),
        });
    }
    fn has_draft(&self) -> bool {
        self.draft
    }
    fn server_is_ours(&self) -> bool {
        self.server.is_some_and(Server::is_ours)
    }
    fn restore(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// One call's worth of `Sink`, drained onto the pipe when it goes away.
struct PipeSink<'a> {
    front: &'a mut PipeFrontend,
    wire: WireSink,
}

impl<'a> PipeSink<'a> {
    fn new(front: &'a mut PipeFrontend) -> Self {
        Self {
            front,
            wire: WireSink::default(),
        }
    }
}

impl Drop for PipeSink<'_> {
    fn drop(&mut self) {
        for message in self.wire.take_render() {
            self.front.send(message);
        }
        for cursor in self.wire.take_cursor() {
            self.front.consume(cursor);
        }
    }
}

impl Sink for PipeSink<'_> {
    fn line(&mut self, window: &str, kind: LineKind, text: &str) {
        self.wire.line(window, kind, text);
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
        self.wire.stream(window, key, kind, prefix, text, done);
        // The same words twice: the text every front-end drew before, and the rows the
        // one that reads them draws now. Queued after the stream, so a `Clear` this
        // row was preceded by still comes first. See `docs/notes/client-document.md`.
        if let Some(message) = self.front.answer(window, key, kind, prefix, text, done) {
            self.wire.push(message);
        }
    }
    fn status(&mut self, text: &str) {
        self.wire.status(text);
    }
    fn note(&mut self, kind: LineKind, text: &str) {
        self.wire.note(kind, text);
    }
    fn layout(&mut self, layout: &Layout) {
        self.wire.layout(layout);
    }
    fn rewrite(&mut self, window: &str, key: &str, text: &str) {
        self.wire.rewrite(window, key, text);
        if let Some(message) = self.front.rewritten(window, key, text) {
            self.wire.push(message);
        }
    }
    fn tool_call(&mut self, window: &str, key: &str, head: &protocol::Head, streamed: bool) {
        self.wire.tool_call(window, key, head, streamed);
    }
    fn relabel(&mut self, window: &str, key: &str, tag: &str, kind: LineKind) {
        self.wire.relabel(window, key, tag, kind);
    }
    fn forget(&mut self, window: &str) {
        self.wire.forget(window);
        self.front.drop_window(window);
    }
    fn clear(&mut self, window: &str) {
        self.wire.clear(window);
        self.front.drop_window(window);
    }
    fn sticky_session(&mut self, name: Option<&str>) {
        self.wire.sticky_session(name);
    }
    fn stopped(&mut self, session: &str, failed: bool) {
        self.wire.stopped(session, failed);
    }
    fn settled(&mut self, id: u64, ok: bool) {
        self.wire.settled(id, ok);
    }
    fn transcript(&mut self, session: &str, created: i64, seq: i64) {
        self.wire.transcript(session, created, seq);
    }
    fn row(&mut self, window: &str, seq: u64, event: &gongga_client::protocol::Event) {
        // The copy is the client's; the number is the front-end's only coordinate.
        self.front.journal.event(window, seq, window, event);
        self.wire.row(window, seq, event);
    }
    fn rewind(&mut self, window: &str) {
        self.wire.rewind(window);
    }
    fn older(&mut self, window: &str) {
        self.wire.older(window);
    }
    fn older_page(&mut self, window: &str, rows: &[PastRow]) {
        // What only the pipe can say: the rows go above what the window holds. See
        // `docs/notes/client-render-wire.md`.
        self.wire.prepend(window, rows);
    }
    fn landed(&mut self, window: &str, created: i64, drawn: i64, floor: i64) {
        self.wire.landed(window, created, drawn, floor);
    }
}
