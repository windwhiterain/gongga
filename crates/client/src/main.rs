//! `gongga-client`: the client process a UI starts, one pipe in and one out.
//!
//! See `docs/notes/client-pipe.md`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use gongga_client::render_wire::{
    CursorMessage, RenderMessage, UiIntent, WireSink, decode_intent, encode_frame,
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
    draft: bool,
}

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
            draft: false,
        }
    }

    /// Puts one frame on the pipe. No cap and no `try_send`: dropping a frame
    /// would lose a line the operator is waiting for.
    fn send(&mut self, message: RenderMessage) {
        if let RenderMessage::Layout { layout } = &message {
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
            self.journal.cache().remove(window);
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
    fn drawn(&self) -> &[String] {
        self.journal.drawn()
    }
    fn refresh_completion(&mut self) {}
}

impl WindowView for PipeFrontend {
    fn show_state(&mut self, state: &State) {
        let name = state.sticky().get("session").map(str::to_owned);
        if name != self.sticky {
            self.sticky = name.clone();
            self.send(RenderMessage::StickySession { name });
        }
    }
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
    fn set_server(&mut self, server: Option<Server>) {
        self.server = server;
    }
    fn repoint(&mut self, client_home: &Path, endpoint: &str) {
        self.journal.repoint(client_home, endpoint);
        self.roster.repoint(client_home, endpoint);
    }
    fn restore_input(&mut self, text: &str) {
        self.draft = true;
        self.send(RenderMessage::Note {
            kind: LineKind::System,
            text: format!("nothing was sent; keep this line: {text}"),
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
    }
    fn tool_call(&mut self, window: &str, key: &str, head: &protocol::Head, streamed: bool) {
        self.wire.tool_call(window, key, head, streamed);
    }
    fn relabel(&mut self, window: &str, key: &str, tag: &str, kind: LineKind) {
        self.wire.relabel(window, key, tag, kind);
    }
    fn forget(&mut self, window: &str) {
        self.wire.forget(window);
    }
    fn clear(&mut self, window: &str) {
        self.wire.clear(window);
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
    fn rewind(&mut self, window: &str) {
        self.wire.rewind(window);
    }
    fn older(&mut self, window: &str) {
        self.wire.older(window);
    }
    fn landed(&mut self, window: &str, created: i64, drawn: i64, floor: i64) {
        self.wire.landed(window, created, drawn, floor);
    }
}
