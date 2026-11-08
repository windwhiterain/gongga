//! The interactive client: a window, a link, and the loop between them.

use std::future::Future;
use std::pin::Pin;

use client::render_wire::UiIntent;
use client::runtime;
use client::window_input;
use client::window_refill;
use client::window_sync;
use client::{DialFailure, Greeting, Layout, LineKind, Link, Sink};

use crate::{Server, Tui, UiEvent, closing};
use client::window_command as command;
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

impl TuiSink<'_> {
    /// A line this front-end says about itself, in the window for lines that name
    /// no session.
    fn say(&mut self, kind: LineKind, text: &str) {
        self.0.say(kind, text);
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

/// What the loop is waiting for this time round.
enum Step {
    /// The operator did something (or the terminal went away: `None`).
    Input(Option<UiEvent>),
    /// The console holding this window is going away.
    Closing,
    /// A message arrived, or `None` when the link ended.
    Wire(Option<Vec<u8>>),
    /// The retry backoff expired: start dialling.
    Dial,
    /// A dial in flight finished.
    Dialed(Result<(Link, Greeting), DialFailure>),
}

/// A dial kept alive between loop iterations, boxed so the loop never blocks.
type Dialing = Pin<Box<dyn Future<Output = Result<(Link, Greeting), DialFailure>>>>;

/// Gives the operator back the lines no server answered.
///
/// # Invariant
///
/// A line leaves the input when it is sent and comes back when the link ends
/// first: the words are the operator's until the server says it has them. A draft
/// in the input is not replaced -- the command row names what was lost, and the
/// history keeps it. See `docs/notes/tui-run.md`.
fn give_back(ui: &mut Tui, lines: Vec<String>) {
    let Some(newest) = lines.last() else {
        return;
    };
    // A draft is the operator's next line, not the one that was lost.
    if ui.screen().input.text().trim().is_empty() {
        ui.restore_input(newest);
    }
}

/// The interactive front-end: the window outlives the link. See `docs/notes/tui-run.md`.
pub async fn interactive(args: &client::Args) -> anyhow::Result<()> {
    // `--parent` names the UI, which is this process for now.
    // See `docs/notes/client-runtime.md`.
    let owner_pid = std::process::id();
    // The command an agent's shell types, installed before any shell exists.
    let home = client::home_of(args);
    // Both are this process's own: where a row lives never depends on a greeting.
    let client_home = client::client_home_of(args);
    // The window's own state: sticky entries, drawn sessions, settings, address.
    let mut state = State::load(&home);
    // The address in force: what the window remembers first, then the flags.
    let mut conn = args.conn.clone();
    if let Some(addr) = state.connection() {
        conn.server = Some(addr.to_string());
    }
    let mut endpoint = client::endpoint_name(&conn);

    let mut client = client::build_client(args);
    let window = command::Window::new(client.completer());
    let mut ui = Tui::new(window.shared(), &client_home, &endpoint)?;
    // What this window drew last: a greeting may never come.
    // See `docs/notes/tui-roster.md`.
    let known = ui.known_list();
    client.seed_listed(known);
    // Nothing has been sent yet: a fresh client owes nobody a line.
    let _ = client.set_offline(0, &mut TuiSink(&mut ui));
    window_sync::sync(&mut client, &state, &window, &mut ui);

    let mut link: Option<client::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = runtime::RECONNECT_MIN;
    let mut attempts: u32 = 0;
    // Started once, before any dial; what it spawned is its to stop, so the
    // server on the address is stopped first.
    let spawned: Option<u32> = match runtime::starts_a_server(args.client_only) {
        true => match client::server_of(&conn) {
            Ok(addr) => {
                if let Some(old) = client::stop(&conn, &home, addr).await {
                    TuiSink(&mut ui).say(
                        LineKind::System,
                        &format!("stopped the server that was serving this home (pid {old})"),
                    );
                }
                // The key this window dials with, so the child does not mint
                // another one and refuse the window that started it.
                let key = conn.key.clone();
                client::spawn(&home, addr, Some(std::process::id()), key.as_deref())
                    .ok()
                    .flatten()
            }
            Err(_) => None,
        },
        false => None,
    };
    // The version of the listings already drawn, so a landed answer repaints once.
    let mut drawn_listings: u64 = 0;
    // What the window keeps for commands the server has not answered yet.
    let mut pending: Vec<(u64, client::window_state::Follow)> = Vec::new();
    // The pages of the past windows asked to read and the loop has not sent yet.
    let mut owed: Vec<String> = Vec::new();
    let result = loop {
        // Every arm keeps the keyboard branch, so no state stops it answering keys.
        let step = if let Some(open) = link.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                bytes = open.recv() => Step::Wire(bytes),
                _ = closing::asked() => Step::Closing,
            }
        } else if let Some(pending) = dialing.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                outcome = pending.as_mut() => Step::Dialed(outcome),
                _ = closing::asked() => Step::Closing,
            }
        } else {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                _ = tokio::time::sleep_until(next_try) => Step::Dial,
                _ = closing::asked() => Step::Closing,
            }
        };

        match step {
            Step::Input(None) => break Ok(()),
            // Not input: the key changed nothing to submit. The drain below is
            // what the completion was waiting for.
            Step::Input(Some(UiEvent::Recomplete)) => {}
            // A scroll past the oldest row a window holds: a page above it.
            Step::Input(Some(UiEvent::Intent(UiIntent::Older { windows }))) => owed.extend(windows),
            Step::Input(Some(UiEvent::Intent(UiIntent::Quit))) => break Ok(()),
            // The console is going away: it asked, and the way out is the same
            // one Alt-Q takes. See `docs/notes/tui-closing.md`.
            Step::Closing => break Ok(()),
            // A gesture of the window's own: an operation, not a line.
            Step::Input(Some(UiEvent::Intent(UiIntent::Operate { operation }))) => {
                let outcome =
                    command::operate(&mut client, &mut state, operation, &mut TuiSink(&mut ui));
                // Whatever the gesture changed of the window's own state reaches
                // the drawing and the completion before the next event.
                window_sync::sync(&mut client, &state, &window, &mut ui);
                if let client::Submission::Send(msg) = outcome
                    && let Some(open) = link.as_ref()
                    && let Err(e) = open.send(msg).await
                {
                    TuiSink(&mut ui).say(
                        LineKind::Error,
                        &format!("the operation could not be sent: {e}"),
                    );
                }
            }
            Step::Input(Some(UiEvent::Intent(UiIntent::Submit { line }))) => {
                let mut moved = None;
                let outcome = window_input::submission(
                    &mut client,
                    &mut state,
                    &line,
                    &mut TuiSink(&mut ui),
                    &mut moved,
                );
                // Whatever the line changed of the window's own state reaches
                // the screen and the completion before the next keystroke.
                window_sync::sync(&mut client, &state, &window, &mut ui);
                // A `connect_server` moved the address: drop the link and let
                // the next dial find the new one.
                if let Some(addr) = moved {
                    conn.server = Some(addr.to_string());
                    // The rows on screen are the old endpoint's: they go, and the
                    // next frame reads this one's records instead.
                    endpoint = client::endpoint_name(&conn);
                    ui.repoint(&client_home, &endpoint);
                    link = None;
                    ui.set_server(None);
                    backoff = runtime::RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                    give_back(&mut ui, unanswered);
                }
                match outcome {
                    client::Submission::Local | client::Submission::Rejected => {}
                    // Nothing left this process, so the text is still the operator's:
                    // put it back rather than log it away.
                    client::Submission::Offline => ui.restore_input(&line),
                    client::Submission::Send(msg) => match link.as_ref() {
                        Some(open) => {
                            // The command names a session: that session's window
                            // is drawn, and what the server accepts is kept.
                            window_input::follow(&client, &mut state, &msg, &line, &mut pending);
                            window_sync::sync(&mut client, &state, &window, &mut ui);
                            if open.send(msg).await.is_err() {
                                // Its end went unnoticed: nothing was sent, and the
                                // line goes back like any other unanswered one.
                                link = None;
                                ui.set_server(None);
                                backoff = runtime::RECONNECT_MIN;
                                attempts = 0;
                                next_try = tokio::time::Instant::now();
                                let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                                give_back(&mut ui, unanswered);
                            }
                        }
                        // Raced with the link going down: nothing was sent, so it goes
                        // back, and the client stops holding it against the server.
                        None => {
                            client.never_sent(&msg);
                            ui.restore_input(&line);
                        }
                    },
                }
            }
            Step::Wire(Some(bytes)) => {
                // Decoding stays in the command layer; this crate never names a protocol type.
                if let Err(e) = client.handle_bytes(&bytes, &mut TuiSink(&mut ui)) {
                    TuiSink(&mut ui).say(LineKind::Error, &format!("bad message: {e}"));
                }
                // Everything waiting goes in before the next frame: one paint
                // per burst, not per delta.
                if let Some(open) = link.as_mut() {
                    while let Some(more) = open.try_recv() {
                        if let Err(e) = client.handle_bytes(&more, &mut TuiSink(&mut ui)) {
                            TuiSink(&mut ui).say(LineKind::Error, &format!("bad message: {e}"));
                            break;
                        }
                    }
                }
                window_sync::settle(&client, &mut state, &mut pending, &mut ui);
                window_sync::sync(&mut client, &state, &window, &mut ui);
                // A hole: only a greeting carries the whole list, so dial again.
                // See `docs/notes/tui-run.md`.
                if client.take_hole() && link.is_some() {
                    link = None;
                    ui.set_server(None);
                    backoff = runtime::RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                    TuiSink(&mut ui).say(
                        LineKind::System,
                        "the stream skipped events — reconnecting to read the list again",
                    );
                    give_back(&mut ui, unanswered);
                }
            }
            Step::Wire(None) => {
                link = None;
                ui.set_server(None);
                backoff = runtime::RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                TuiSink(&mut ui).say(LineKind::System, "server connection lost — reconnecting");
                give_back(&mut ui, unanswered);
            }
            Step::Dial => {
                attempts += 1;
                dialing = Some(Box::pin(runtime::dial_now(conn.clone())));
            }
            Step::Dialed(outcome) => {
                // Finished either way: clear it, or a later poll of a dead future panics.
                drop(dialing.take());
                match outcome {
                    Ok((open, greeting)) => {
                        ui.set_server(Some(runtime::whose_server(spawned, &greeting, owner_pid)));
                        // Applying the greeting is what the server says; what
                        // this window kept is the other half of the comparison.
                        greeting.apply(&mut client, &mut TuiSink(&mut ui));
                        window_refill::declare(&mut client, &ui);
                        link = Some(open);
                        backoff = runtime::RECONNECT_MIN;
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        let unanswered = client.set_offline(attempts, &mut TuiSink(&mut ui));
                        give_back(&mut ui, unanswered);
                        // A spawned server is coming up: a fresh home has no key
                        // until it writes one, so that is not news yet.
                        if spawned.is_none() {
                            // Said every time: the refresh above wipes a note
                            // said once, and a window that says only
                            // "reconnecting" looks hung.
                            TuiSink(&mut ui).say(
                                LineKind::Error,
                                &format!(
                                    "{} (attempt {attempts}) — still trying; Alt+Q leaves",
                                    failure.message()
                                ),
                            );
                        }
                        backoff = (backoff * 2).min(runtime::RECONNECT_MAX);
                        next_try = tokio::time::Instant::now()
                            + runtime::wait_for(spawned, attempts, backoff);
                    }
                }
            }
        }
        // After the step, never during it: a peer's directory is not a
        // synchronous answer.
        window_refill::answer_completion(&mut client, link.as_ref(), &mut ui, &mut drawn_listings)
            .await;
        // Same place, same reason: what a frame drew decides what is fetched.
        window_refill::answer_transcripts(&mut client, link.as_ref(), &ui).await;
        // And what the operator asked to read above: a page of the past.
        window_refill::answer_older(&mut client, link.as_ref(), &ui, &mut owed).await;
    };

    // SPEC's stop, for the server the bar drew as own, and nothing for one that is
    // somebody else's: one answer decides both.
    if ui.screen().server.is_some_and(Server::is_ours)
        && let Some(open) = link.as_mut()
    {
        open.cancel().await;
    }
    // Best-effort goodbye: saves the server its idle timeout; failure is harmless.
    if let Some(open) = link.take() {
        open.goodbye().await;
    }

    ui.restore()?;
    result
}
