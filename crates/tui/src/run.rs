//! The interactive client: a window, a link, and the loop between them.

use std::future::Future;
use std::pin::Pin;

use cli::{DialFailure, Greeting, Layout, LineKind, Link, Sink};
use tokio::time::Duration;

use crate::{Server, Tui, UiEvent};

/// First redial delay, doubling up to the cap; a cadence, not a budget.
///
/// # Invariant
///
/// Backing off never costs the window: history, input and scroll stay put, and
/// Alt+Q still leaves. See `docs/notes/tui-run.md`.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);
/// The wait between dials while a server this window started is coming up.
const SPAWN_POLL: Duration = Duration::from_millis(50);
/// How many of those are fast: past this it is not a start but a failure.
const SPAWN_ATTEMPTS: u32 = 120;

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

    fn block_size(&mut self, lines: usize) {
        self.0.set_block_size(lines);
    }

    fn layout(&mut self, layout: &Layout) {
        self.0.set_layout(layout);
    }

    fn rewrite(&mut self, window: &str, key: &str, text: &str) {
        self.0.rewrite(window, key, text);
    }
    fn tool_call(&mut self, window: &str, key: &str, head: &cli::protocol::Head, _streamed: bool) {
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

    fn sticky_session(&mut self, name: Option<&str>) {
        self.0.set_sticky_session(name);
    }
}

impl TuiSink<'_> {
    /// A line this front-end says about itself, in the window for lines that name
    /// no session.
    fn say(&mut self, kind: LineKind, text: &str) {
        self.0.say(kind, text);
    }
}

/// What an input line asks for: `/` starts a command, anything else is prose for
/// the agent.
///
/// # Invariant
///
/// A line this front-end generates for itself -- a click -- is written as a
/// command, because prose here is a message the operator never typed.
fn classified(line: &str) -> Option<Result<Vec<String>, String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .strip_prefix('/')
        .map(|command| shell_words::split(command).map_err(|e| format!("unparsable input: {e}")))
}

/// One input line, handed to the command layer or sent as a message.
/// Sends what the completion asked for, and reports whether the popup is now out
/// of date.
///
/// # Invariant
///
/// Outside the key handler, because a completion is answered synchronously and a
/// peer's directory cannot be: the popup shows what has arrived, and this is what
/// goes and gets the rest. See `docs/notes/tui-run.md`.
async fn answer_completion(
    client: &mut cli::Client,
    link: Option<&cli::Link>,
    ui: &mut Tui,
    seen: &mut u64,
) {
    if let Some(open) = link {
        for (target, directory) in client.take_wanted_paths() {
            let Some(message) = client.ask_paths(&target, &directory) else {
                continue;
            };
            if open.send(message).await.is_err() {
                break;
            }
        }
    }
    let version = client.paths_version();
    if version != *seen {
        *seen = version;
        ui.refresh_completion();
    }
}

fn submission(client: &mut cli::Client, line: &str, sink: &mut TuiSink) -> cli::Submission {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return cli::Submission::Rejected;
    }
    match classified(line) {
        Some(Ok(tokens)) => client.command(tokens, sink),
        Some(Err(e)) => {
            sink.say(LineKind::Error, &e);
            cli::Submission::Rejected
        }
        None => client.message(trimmed, sink),
    }
}

/// What the loop is waiting for this time round.
enum Step {
    /// The operator did something (or the terminal went away: `None`).
    Input(Option<UiEvent>),
    /// A message arrived, or `None` when the link ended.
    Wire(Option<Vec<u8>>),
    /// The retry backoff expired: start dialling.
    Dial,
    /// A dial in flight finished.
    Dialed(Result<(Link, Greeting), DialFailure>),
}

/// A dial kept alive between loop iterations, boxed so the loop never blocks.
type Dialing = Pin<Box<dyn Future<Output = Result<(Link, Greeting), DialFailure>>>>;

/// Whether this window starts a server of its own.
///
/// # Invariant
///
/// Once, at the start, before any dial: asking first costs the first seconds of
/// every launch. `--client_only` is the whole refusal, and nothing is restarted
/// later -- a server that goes away is the operator's doing. See
/// `docs/notes/tui-run.md`.
fn starts_a_server(client_only: bool) -> bool {
    !client_only
}

/// Whether the window on the way out stops the server it started.
///
/// # Invariant
///
/// Only the *process* this window spawned: a home somebody else already serves
/// refuses the lock, so this window's child exits at once and the pid answering
/// the dial is somebody else's. Comparing pids is what makes that answerable —
/// an address would say the wrong thing, because a `connect_server` can move the
/// window onto a server it did not start.
/// `spawned` is what `spawn` answered, which is the watchdog's pid when the
/// server runs under one, so either pid matching means this window started it.
/// See `docs/notes/tui-run.md`.
fn stops_a_server(spawned: Option<u32>, answering: Answering) -> bool {
    let Some(ours) = spawned else {
        return false;
    };
    ours != 0 && (Some(ours) == answering.pid || Some(ours) == answering.supervisor)
}

/// What the greeting said about the server that answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Answering {
    /// The pid in the greeting, which is the server process itself.
    pid: Option<u32>,
    /// The watchdog that server runs under, when it runs under one.
    supervisor: Option<u32>,
}

/// Whose server answered, from what this window knows about it.
///
/// # Invariant
///
/// Starting it is the only way to *be* its owner without asking: otherwise the
/// greeting's `owner` decides, and this process's pid is what makes it ours. A
/// server with no owner at all is nobody's, which is worth saying because nothing
/// will stop it. See `docs/notes/tui-run.md`.
fn whose_server(spawned: Option<u32>, answering: Answering, owner: Option<u32>, me: u32) -> Server {
    let pid = answering.pid.unwrap_or(0);
    if pid != 0 && (spawned == Some(pid) || answering.supervisor == spawned) {
        return Server::Ours { pid };
    }
    match owner {
        Some(pid) if pid == me => Server::Adopted {
            pid: answering.pid.unwrap_or(0),
        },
        Some(_) => Server::Another { pid },
        None => Server::Unowned { pid },
    }
}

/// How long to wait before the next dial.
///
/// # Invariant
///
/// A server this window started is coming up, so its first seconds are worth
/// [`SPAWN_POLL`] rather than the reconnect ladder, which exists for a server
/// that went away. Only our own: somebody else's is not ours to hammer.
fn wait_for(spawned: Option<u32>, attempts: u32, backoff: Duration) -> Duration {
    if spawned.is_some() && attempts <= SPAWN_ATTEMPTS {
        SPAWN_POLL
    } else {
        backoff
    }
}

/// One attempt, resolving afresh: `connect_server` moves the address.
async fn dial_now(conn: cli::Connection) -> Result<(Link, Greeting), DialFailure> {
    // The pid claims an unowned server, so it stops when this window does.
    let endpoint = conn
        .endpoint()
        .map_err(DialFailure::Temporary)?
        .owning(std::process::id());
    cli::dial(endpoint).await
}

/// The interactive front-end: the window outlives the link. See `docs/notes/tui-run.md`.
pub async fn interactive(args: &cli::Args) -> anyhow::Result<()> {
    // The command an agent's shell types, installed before any shell exists.
    let home = cli::home_of(args);

    let mut client = cli::build_client(args);
    let mut ui = Tui::new(client.completer(), &home)?;
    client.set_offline(0, &mut TuiSink(&mut ui));

    let mut link: Option<cli::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;
    // Started once, before any dial; what it spawned is its to stop. A window
    // that starts a server owns the address, so whatever serves it now is stopped
    // first -- otherwise this window would silently read another build's answers.
    let spawned: Option<u32> = match starts_a_server(args.client_only) {
        true => match cli::server_of(&args.conn) {
            Ok(addr) => {
                if let Some(old) = cli::stop(&args.conn, &home, addr).await {
                    TuiSink(&mut ui).say(
                        LineKind::System,
                        &format!("stopped the server that was serving this home (pid {old})"),
                    );
                }
                // The key this window dials with, so the child does not mint
                // another one and refuse the window that started it.
                let key = args.conn.key.clone();
                cli::spawn(&home, addr, Some(std::process::id()), key.as_deref())
                    .ok()
                    .flatten()
            }
            Err(_) => None,
        },
        false => None,
    };
    // The server that answers the dial, which is not `spawned` when the home was
    // already served by somebody else.
    let mut answering = Answering::default();

    // The version of the listings already drawn, so a landed answer repaints once.
    let mut drawn_listings: u64 = 0;
    let result = loop {
        // Every arm keeps the keyboard branch, so no state stops it answering keys.
        let step = if let Some(open) = link.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                bytes = open.recv() => Step::Wire(bytes),
            }
        } else if let Some(pending) = dialing.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                outcome = pending.as_mut() => Step::Dialed(outcome),
            }
        } else {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                _ = tokio::time::sleep_until(next_try) => Step::Dial,
            }
        };

        match step {
            Step::Input(None) => break Ok(()),
            // Not input: the key changed nothing to submit. The drain below is
            // what the completion was waiting for.
            Step::Input(Some(UiEvent::Recomplete)) => {}
            Step::Input(Some(UiEvent::Quit)) => break Ok(()),
            Step::Input(Some(UiEvent::Submit(line))) => {
                let outcome = submission(&mut client, &line, &mut TuiSink(&mut ui));
                // A `connect_server` moved the address: drop the link and let
                // the next dial find the new one.
                if client.take_server_change() {
                    link = None;
                    ui.set_server(None);
                    backoff = RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    client.set_offline(0, &mut TuiSink(&mut ui));
                }
                match outcome {
                    cli::Submission::Local | cli::Submission::Rejected => {}
                    // `wait` reads a stream, and this window is already drawing
                    // every session's events: there is nothing for it to wait on
                    // that the operator cannot already see.
                    cli::Submission::Wait(_) => TuiSink(&mut ui).say(
                        LineKind::System,
                        "wait is for a script: this window already draws every session as \
                         it happens",
                    ),
                    // Nothing left this process, so the text is still the operator's:
                    // put it back rather than log it away.
                    cli::Submission::Offline => ui.restore_input(&line),
                    cli::Submission::Send(msg) => match link.as_ref() {
                        Some(open) => {
                            if let Err(e) = open.send(msg).await {
                                // Its end went unnoticed, so the line may not have
                                // arrived: report it rather than re-queue it.
                                TuiSink(&mut ui).say(
                                    LineKind::Error,
                                    &format!(
                                        "connection dropped while sending ({e}); \
                                             that line may not have arrived"
                                    ),
                                );
                                link = None;
                                ui.set_server(None);
                                backoff = RECONNECT_MIN;
                                attempts = 0;
                                next_try = tokio::time::Instant::now();
                                client.set_offline(0, &mut TuiSink(&mut ui));
                            }
                        }
                        // Raced with the link going down: nothing was sent, so it goes back.
                        None => ui.restore_input(&line),
                    },
                }
            }
            Step::Wire(Some(bytes)) => {
                // Decoding stays in the command layer; this crate never names a protocol type.
                if let Err(e) = client.handle_bytes(&bytes, &mut TuiSink(&mut ui)) {
                    TuiSink(&mut ui).say(LineKind::Error, &format!("bad message: {e}"));
                }
                // Everything already waiting goes in before the next frame is drawn:
                // one paint per burst, not per delta. Painting per message is what
                // let a stream outrun the loop, and a reader that stops reading
                // collapses the link's window and stalls every command behind it.
                if let Some(open) = link.as_mut() {
                    while let Some(more) = open.try_recv() {
                        if let Err(e) = client.handle_bytes(&more, &mut TuiSink(&mut ui)) {
                            TuiSink(&mut ui).say(LineKind::Error, &format!("bad message: {e}"));
                            break;
                        }
                    }
                }
            }
            Step::Wire(None) => {
                link = None;
                ui.set_server(None);
                backoff = RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                client.set_offline(0, &mut TuiSink(&mut ui));
                TuiSink(&mut ui).say(LineKind::System, "server connection lost — reconnecting");
            }
            Step::Dial => {
                attempts += 1;
                dialing = Some(Box::pin(dial_now(args.conn.clone())));
            }
            Step::Dialed(outcome) => {
                // Finished either way: clear it, or a later poll of a dead future panics.
                drop(dialing.take());
                match outcome {
                    Ok((open, greeting)) => {
                        answering = Answering {
                            pid: Some(greeting.pid()),
                            supervisor: greeting.supervisor(),
                        };
                        ui.set_server(Some(whose_server(
                            spawned,
                            answering,
                            greeting.owner(),
                            std::process::id(),
                        )));
                        // Applying the greeting is the resync: catalog, sessions, model.
                        greeting.apply(&mut client, &mut TuiSink(&mut ui));
                        link = Some(open);
                        backoff = RECONNECT_MIN;
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        client.set_offline(attempts, &mut TuiSink(&mut ui));
                        // A spawned server is coming up: a fresh home has no key
                        // until it writes one, so that is not news yet.
                        if attempts == 1 && spawned.is_none() {
                            TuiSink(&mut ui).say(
                                LineKind::Error,
                                &format!(
                                    "{} — retrying in the background, Alt+Q still leaves",
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
        // After the step, never during it: a key handler answers completion
        // synchronously, and a peer's directory is not synchronous.
        answer_completion(&mut client, link.as_ref(), &mut ui, &mut drawn_listings).await;
    };

    // Nothing this window started is left behind: `cancel` is SPEC's stop, and
    // it stores what cannot be serialised.
    if stops_a_server(spawned, answering) {
        if let Some(open) = link.as_ref() {
            open.cancel().await;
        }
    }
    // Best-effort goodbye: saves the server its idle timeout; failure is harmless.
    if let Some(open) = link.take() {
        open.goodbye().await;
    }

    ui.restore()?;
    result
}
