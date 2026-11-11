//! The connection lifecycle a window shares with the client process.
//!
//! See `docs/notes/client-runtime.md`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use crate::render_wire::UiIntent;
use crate::window_command as command;
use crate::window_refill::WindowFacts;
use crate::window_state::State;
use crate::window_sync::WindowView;
use crate::{Connection, DialFailure, Greeting, LineKind, Link, Listed, Sink};

/// First redial delay, doubling up to the cap; a cadence, not a budget.
///
/// # Invariant
///
/// Backing off never costs the window: history, input and scroll stay put, and
/// Alt+Q still leaves. See `docs/notes/client-runtime.md`.
pub const RECONNECT_MIN: Duration = Duration::from_millis(250);
pub const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How much of a burst is decoded before the keyboard is looked at again.
///
/// # Invariant
///
/// Bounded on purpose: one paint per burst is the goal, but a stream that never
/// pauses would keep the drain inside one step, and no step means no `select!`,
/// no key read and no frame drawn -- which is what a fast answer looked like
/// while somebody was typing into it. See `docs/notes/tui-run.md`.
pub const BURST: usize = 64;
/// And the same bound in bytes, for a burst of a few very long messages.
pub const BURST_BYTES: usize = 256 * 1024;

/// The wait between dials while a server the UI started is coming up.
pub const SPAWN_POLL: Duration = Duration::from_millis(50);
/// How many of those are fast: past this it is not a start but a failure.
pub const SPAWN_ATTEMPTS: u32 = 120;

/// Whose server the window is talking to, which the header says beside the status.
///
/// # Invariant
///
/// A server's lifetime belongs to whoever *started* it, never to whoever
/// connects: dialling is reading. Two facts make one ours -- the pid `spawn`
/// answered, and the pid the server follows (`--parent`) being the UI's own,
/// which is how a handover keeps the lifetime it had. Anything else outlives
/// the window. See `docs/notes/tui-run.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// The UI started it, so it goes when the UI does.
    Ours { pid: u32 },
    /// A handover of the UI's server: a newer build in its place, carrying the
    /// same `--parent`, so it goes too.
    Handover { pid: u32 },
    /// Another window started it: this one reads it and cannot stop it.
    Another { pid: u32 },
    /// Nobody started it for a window: it outlives every window that reads it.
    Unowned { pid: u32 },
}

impl Server {
    /// Whether closing the UI stops this server.
    ///
    /// # Invariant
    ///
    /// One answer for two readers: the header draws it and the way out acts on
    /// it, so the bar cannot promise a lifetime the window does not honour.
    pub fn is_ours(self) -> bool {
        matches!(self, Server::Ours { .. } | Server::Handover { .. })
    }

    /// The words the header shows: whether it is own, why, and then its pid.
    pub fn label(&self) -> String {
        match self {
            Server::Ours { pid } => format!("own #{pid}"),
            Server::Handover { pid } => format!("own #{pid} (handover)"),
            Server::Another { pid } => format!("not own #{pid} (another window)"),
            Server::Unowned { pid } => format!("not own #{pid} (no owner)"),
        }
    }
}

/// Whether this window starts a server of its own.
///
/// # Invariant
///
/// Once, at the start, before any dial: asking first costs the first seconds of
/// every launch. `--client_only` is the whole refusal, and nothing is restarted
/// later -- a server that goes away is the operator's doing.
/// See `docs/notes/client-runtime.md`.
pub fn starts_a_server(client_only: bool) -> bool {
    !client_only
}

/// Whose server answered, from what the window knows about it.
///
/// # Invariant
///
/// `owner_pid` is the pid the server's `--parent` names -- the UI, not this
/// client process. Comparing against our own id would call a handover somebody
/// else's server once the client is a child of the UI. Being told is not owning:
/// only the pid `spawn` answered and the pid the server follows count.
/// See `docs/notes/client-runtime.md`.
pub fn whose_server(spawned: Option<u32>, greeting: &Greeting, owner_pid: u32) -> Server {
    let pid = greeting.pid();
    if spawned.is_some() && (spawned == Some(pid) || spawned == greeting.supervisor()) {
        return Server::Ours { pid };
    }
    match greeting.owner() {
        Some(owner) if owner == owner_pid => Server::Handover { pid },
        Some(_) => Server::Another { pid },
        None => Server::Unowned { pid },
    }
}

/// How long to wait before the next dial.
///
/// # Invariant
///
/// A server the UI started is coming up, so its first seconds are worth
/// [`SPAWN_POLL`] rather than the reconnect ladder, which exists for a server
/// that went away. Only our own: somebody else's is not ours to hammer.
pub fn wait_for(spawned: Option<u32>, attempts: u32, backoff: Duration) -> Duration {
    if spawned.is_some() && attempts <= SPAWN_ATTEMPTS {
        SPAWN_POLL
    } else {
        backoff
    }
}

/// One attempt, resolving afresh: `connect_server` moves the address.
///
/// # Invariant
///
/// Nothing of this window goes on the wire as a claim: a server's lifetime is
/// decided by whoever started it, so a window that merely dials -- into a home
/// somebody serves by hand, or onto a `connect_server` -- reads it and leaves it
/// running when it closes. See `docs/notes/client-runtime.md`.
pub async fn dial_now(conn: Connection) -> Result<(Link, Greeting), DialFailure> {
    let endpoint = conn.endpoint().map_err(DialFailure::Temporary)?;
    crate::dial(endpoint).await
}

/// What the loop hears from its one front-end, in words a pipe can carry.
///
/// # Invariant
///
/// `Recomplete` is not an intent: the completion needs the loop to reach the
/// connection, so it never crosses the wire. `Closed` covers both a terminal
/// that went away and a console that asked to close; the loop leaves for both.
pub enum FrontendEvent {
    Intent(UiIntent),
    Recomplete,
    Closed,
}

/// The one front-end this client serves, without naming a terminal.
///
/// # Invariant
///
/// `sink` hands out a short-lived view of the front-end's own renderer. A
/// borrowed wrapper cannot be returned by reference, so the box owns it for
/// the length of one call; the loop never holds two mutable borrows of the
/// front-end at once. See `docs/notes/client-frontend.md`.
pub trait Frontend: WindowFacts + WindowView {
    /// The next gesture, or that the front-end is gone.
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = FrontendEvent> + Send + '_>>;
    /// The console is going away. The future has no `self`, so `select!` can
    /// hold it beside `next_event`.
    fn closing(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    /// Lines the client draws through, for one call.
    fn sink(&mut self) -> Box<dyn Sink + '_>;
    /// The sessions this window already knew before a greeting.
    fn known_list(&self) -> Vec<Listed>;
    /// Candidates for the token under the cursor, computed by the client.
    fn show_completion(&mut self, completion: &crate::Completion);
    fn set_server(&mut self, server: Option<Server>);
    fn repoint(&mut self, client_home: &Path, endpoint: &str);
    fn restore_input(&mut self, text: &str);
    fn has_draft(&self) -> bool;
    fn server_is_ours(&self) -> bool;
    fn restore(&mut self) -> anyhow::Result<()>;
}

/// Gives the operator back the lines no server answered.
///
/// # Invariant
///
/// A line leaves the input when it is sent and comes back when the link ends
/// first: the words are the operator's until the server says it has them. A draft
/// in the input is not replaced -- the command row names what was lost, and the
/// history keeps it. See `docs/notes/tui-run.md`.
pub fn give_back(front: &mut dyn Frontend, lines: Vec<String>) {
    let Some(newest) = lines.last() else {
        return;
    };
    if !front.has_draft() {
        front.restore_input(newest);
    }
}

/// What the loop is waiting for this time round.
enum Step {
    /// What the front-end said; `Closed` covers a terminal that went away.
    Input(FrontendEvent),
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

/// The loop that outlives the link, driven through `Frontend`.
///
/// # Invariant
///
/// `--parent` names the UI, so its pid comes from the caller, not this process.
/// See `docs/notes/client-frontend.md`.
pub async fn run(
    args: &crate::Args,
    owner_pid: u32,
    make: impl FnOnce(crate::SharedCompleter, &Path, &str) -> anyhow::Result<Box<dyn Frontend>>,
) -> anyhow::Result<()> {
    // The command an agent's shell types, installed before any shell exists.
    let home = crate::home_of(args);
    // Both are this process's own: where a row lives never depends on a greeting.
    let client_home = crate::client_home_of(args);
    // One client per home: two would write the same roster and journal.
    let _held = home_lock::HomeLock::acquire(&client_home, "client.lock", "gongga client")?;
    // The window's own state, in its own home: two windows share no file.
    crate::window_state::adopt_legacy(&home, &client_home);
    let mut state = State::load(&home, &client_home);
    // The address in force: what the window remembers first, then the flags.
    let mut conn = args.conn.clone();
    if let Some(addr) = state.connection() {
        conn.server = Some(addr.to_string());
    }
    let mut endpoint = crate::endpoint_name(&conn);

    let mut client = crate::build_client(args);
    let mut window = command::Window::new(client.completer());
    let mut ui: Box<dyn Frontend> = make(window.shared(), &client_home, &endpoint)?;
    // What this window drew last: a greeting may never come.
    // See `docs/notes/tui-roster.md`.
    let known = ui.known_list();
    client.seed_listed(known);
    // Nothing has been sent yet: a fresh client owes nobody a line.
    let _ = client.set_offline(0, ui.sink().as_mut());
    crate::window_sync::sync(&mut client, &state, &window, &mut *ui);

    let mut link: Option<crate::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;
    // Started once, before any dial; what it spawned is its to stop, so the
    // server on the address is stopped first.
    let spawned: Option<u32> = match starts_a_server(args.client_only) {
        true => match crate::server_of(&conn) {
            Ok(addr) => match crate::stop(&conn, &home, addr).await {
                Ok(old) => {
                    if let Some(old) = old {
                        ui.sink().note(
                            LineKind::System,
                            &format!("stopped the server that was serving this home (pid {old})"),
                        );
                    }
                    // The key this window dials with, so the child does not mint
                    // another one and refuse the window that started it.
                    let key = conn.key.clone();
                    // `--parent` names the UI: a crashed client must not take the
                    // server from a window that is still there.
                    crate::spawn(&home, addr, Some(owner_pid), key.as_deref())
                        .ok()
                        .flatten()
                }
                // A server that would not leave still holds the lock, so ours
                // would refuse: say so instead of starting one.
                Err(error) => {
                    ui.sink().note(LineKind::System, &format!("{error}"));
                    None
                }
            },
            Err(_) => None,
        },
        false => None,
    };
    // The version of the listings already drawn, so a landed answer repaints once.
    let mut drawn_listings: u64 = 0;
    // What the window keeps for commands the server has not answered yet.
    let mut pending: Vec<(u64, crate::window_state::Follow)> = Vec::new();
    // The pages of the past windows asked to read and the loop has not sent yet.
    let mut owed: Vec<String> = Vec::new();
    let result = loop {
        // Every arm keeps the keyboard branch, so no state stops it answering keys.
        let step = if let Some(open) = link.as_mut() {
            let closing = ui.closing();
            tokio::select! {
                // The keyboard first, every time: a stream that never pauses would
                // otherwise win the race, and a window that cannot read a key
                // cannot show one either.
                biased;
                event = ui.next_event() => Step::Input(event),
                bytes = open.recv() => Step::Wire(bytes),
                _ = closing => Step::Closing,
            }
        } else if let Some(pending) = dialing.as_mut() {
            let closing = ui.closing();
            tokio::select! {
                // The keyboard first, every time: a stream that never pauses would
                // otherwise win the race, and a window that cannot read a key
                // cannot show one either.
                biased;
                event = ui.next_event() => Step::Input(event),
                outcome = pending.as_mut() => Step::Dialed(outcome),
                _ = closing => Step::Closing,
            }
        } else {
            let closing = ui.closing();
            tokio::select! {
                // The keyboard first, every time: a stream that never pauses would
                // otherwise win the race, and a window that cannot read a key
                // cannot show one either.
                biased;
                event = ui.next_event() => Step::Input(event),
                _ = tokio::time::sleep_until(next_try) => Step::Dial,
                _ = closing => Step::Closing,
            }
        };

        match step {
            Step::Input(FrontendEvent::Closed) => break Ok(()),
            // Not input: the key changed nothing to submit. The drain below is
            // what the completion was waiting for.
            Step::Input(FrontendEvent::Recomplete) => {}
            // The catalog stays here: a UI sends its line and gets words back.
            Step::Input(FrontendEvent::Intent(UiIntent::Complete { line, cursor })) => {
                // A byte cursor from another language may land inside a character.
                let mut at = cursor.min(line.len());
                while at > 0 && !line.is_char_boundary(at) {
                    at -= 1;
                }
                let offered = window.shared().complete(&line, at);
                let (start, end, _) = crate::token_bounds(&line, at);
                ui.show_completion(&crate::Completion {
                    start: offered.start.unwrap_or(start),
                    end,
                    subject: offered.subject,
                    candidates: offered.candidates,
                    selected: 0,
                });
            }
            // A scroll past the oldest row a window holds: a page above it.
            Step::Input(FrontendEvent::Intent(UiIntent::Older { windows })) => owed.extend(windows),
            // How tall each pane is drawn: what one page of the past is sized by.
            Step::Input(FrontendEvent::Intent(UiIntent::Room { windows })) => {
                for (name, rows) in windows {
                    WindowFacts::note_room(&mut *ui, &name, rows);
                }
            }
            Step::Input(FrontendEvent::Intent(UiIntent::Quit)) => break Ok(()),
            // The console is going away: it asked, and the way out is the same
            // one Alt-Q takes. See `docs/notes/tui-closing.md`.
            Step::Closing => break Ok(()),
            // A gesture of the window's own: an operation, not a line.
            Step::Input(FrontendEvent::Intent(UiIntent::Operate { operation })) => {
                let outcome =
                    command::operate(&mut client, &mut state, operation, ui.sink().as_mut());
                // Whatever the gesture changed of the window's own state reaches
                // the drawing and the completion before the next event.
                crate::window_sync::sync(&mut client, &state, &window, &mut *ui);
                if let crate::Submission::Send(msg) = outcome
                    && let Some(open) = link.as_ref()
                    && let Err(e) = open.send(msg).await
                {
                    ui.sink().note(
                        LineKind::Error,
                        &format!("the operation could not be sent: {e}"),
                    );
                }
            }
            Step::Input(FrontendEvent::Intent(UiIntent::Submit { line })) => {
                let mut moved = None;
                let outcome = crate::window_input::submission(
                    &mut client,
                    &mut state,
                    &line,
                    ui.sink().as_mut(),
                    &mut moved,
                );
                // Whatever the line changed of the window's own state reaches
                // the screen and the completion before the next keystroke.
                crate::window_sync::sync(&mut client, &state, &window, &mut *ui);
                // A `connect_server` moved the address: drop the link and let
                // the next dial find the new one.
                if let Some(addr) = moved {
                    conn.server = Some(addr.to_string());
                    // The rows on screen are the old endpoint's: they go, and the
                    // next frame reads this one's records instead.
                    endpoint = crate::endpoint_name(&conn);
                    let unanswered = client.set_offline(0, ui.sink().as_mut());
                    give_back(&mut *ui, unanswered);
                    ui.repoint(&client_home, &endpoint);
                    client = crate::build_client(args);
                    window = command::Window::new(client.completer());
                    client.seed_listed(ui.known_list());
                    dialing = None;
                    pending.clear();
                    owed.clear();
                    link = None;
                    ui.set_server(None);
                    backoff = RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    let unanswered = client.set_offline(0, ui.sink().as_mut());
                    give_back(&mut *ui, unanswered);
                }
                match outcome {
                    crate::Submission::Local | crate::Submission::Rejected => {}
                    // Nothing left this process, so the text is still the operator's:
                    // put it back rather than log it away.
                    crate::Submission::Offline => ui.restore_input(&line),
                    crate::Submission::Send(msg) => match link.as_ref() {
                        Some(open) => {
                            // The command names a session: that session's window
                            // is drawn, and what the server accepts is kept.
                            crate::window_input::follow(
                                &client,
                                &mut state,
                                &msg,
                                &line,
                                &mut pending,
                            );
                            crate::window_sync::sync(&mut client, &state, &window, &mut *ui);
                            if open.send(msg).await.is_err() {
                                // Its end went unnoticed: nothing was sent, and the
                                // line goes back like any other unanswered one.
                                link = None;
                                ui.set_server(None);
                                backoff = RECONNECT_MIN;
                                attempts = 0;
                                next_try = tokio::time::Instant::now();
                                let unanswered = client.set_offline(0, ui.sink().as_mut());
                                give_back(&mut *ui, unanswered);
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
                let decoded = client.handle_bytes(&bytes, ui.sink().as_mut());
                if let Err(e) = decoded {
                    ui.sink()
                        .note(LineKind::Error, &format!("bad message: {e}"));
                }
                // Everything waiting goes in before the next frame: one paint
                // per burst, not per delta.
                if let Some(open) = link.as_mut() {
                    let mut taken = 0usize;
                    let mut read = 0usize;
                    while taken < BURST && read < BURST_BYTES {
                        let Some(more) = open.try_recv() else {
                            break;
                        };
                        taken += 1;
                        read += more.len();
                        let decoded = client.handle_bytes(&more, ui.sink().as_mut());
                        if let Err(e) = decoded {
                            ui.sink()
                                .note(LineKind::Error, &format!("bad message: {e}"));
                            break;
                        }
                    }
                }
                crate::window_sync::settle(&client, &mut state, &mut pending, &mut *ui);
                crate::window_sync::sync(&mut client, &state, &window, &mut *ui);
                // A hole: only a greeting carries the whole list, so dial again.
                // See `docs/notes/tui-run.md`.
                if client.take_hole() && link.is_some() {
                    link = None;
                    ui.set_server(None);
                    backoff = RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    let unanswered = client.set_offline(0, ui.sink().as_mut());
                    ui.sink().note(
                        LineKind::System,
                        "the stream skipped events — reconnecting to read the list again",
                    );
                    give_back(&mut *ui, unanswered);
                }
            }
            Step::Wire(None) => {
                link = None;
                ui.set_server(None);
                backoff = RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                let unanswered = client.set_offline(0, ui.sink().as_mut());
                ui.sink()
                    .note(LineKind::System, "server connection lost — reconnecting");
                give_back(&mut *ui, unanswered);
            }
            Step::Dial => {
                attempts += 1;
                dialing = Some(Box::pin(dial_now(conn.clone())));
            }
            Step::Dialed(outcome) => {
                // Finished either way: clear it, or a later poll of a dead future panics.
                drop(dialing.take());
                match outcome {
                    Ok((open, greeting)) => {
                        ui.set_server(Some(whose_server(spawned, &greeting, owner_pid)));
                        // Applying the greeting is what the server says; what
                        // this window kept is the other half of the comparison.
                        greeting.apply(&mut client, ui.sink().as_mut());
                        crate::window_refill::declare(&mut client, &mut *ui);
                        link = Some(open);
                        backoff = RECONNECT_MIN;
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        let unanswered = client.set_offline(attempts, ui.sink().as_mut());
                        give_back(&mut *ui, unanswered);
                        // A spawned server is coming up: a fresh home has no key
                        // until it writes one, so that is not news yet.
                        if spawned.is_none() {
                            // Said every time: the refresh above wipes a note
                            // said once, and a window that says only
                            // "reconnecting" looks hung.
                            ui.sink().note(
                                LineKind::Error,
                                &format!(
                                    "{} (attempt {attempts}) — still trying; Alt+Q leaves",
                                    failure.message()
                                ),
                            );
                        }
                        backoff = (backoff * 2).min(RECONNECT_MAX);
                        next_try =
                            tokio::time::Instant::now() + wait_for(spawned, attempts, backoff);
                    }
                }
            }
        }
        // After the step, never during it: a peer's directory is not a
        // synchronous answer.
        crate::window_refill::answer_completion(
            &mut client,
            link.as_ref(),
            &mut *ui,
            &mut drawn_listings,
        )
        .await;
        // Same place, same reason: what a frame drew decides what is fetched.
        crate::window_refill::answer_transcripts(&mut client, link.as_ref(), &mut *ui).await;
        // And what the operator asked to read above: a page of the past.
        crate::window_refill::answer_older(&mut client, link.as_ref(), &mut *ui, &mut owed).await;
    };

    // SPEC's stop, for the server the bar drew as own, and nothing for one that is
    // somebody else's: one answer decides both.
    if ui.server_is_ours()
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
