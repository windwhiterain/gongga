//! The interactive client: a window, a link, and the loop between them.

use std::future::Future;
use std::pin::Pin;

use cli::{DialFailure, Greeting, Layout, LineKind, Link, Sink};
use tokio::time::Duration;

use crate::{Tui, UiEvent};

/// First redial delay, doubling up to the cap; a cadence, not a budget.
///
/// # Invariant
///
/// Backing off never costs the window: history, input and scroll stay put, and
/// Alt+Q still leaves. See `docs/notes/tui-run.md`.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

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
        self.0.set_status(text);
    }

    fn footer(&mut self, text: &str) {
        self.0.set_footer(text);
    }

    fn block_size(&mut self, lines: usize) {
        self.0.set_block_size(lines);
    }

    fn layout(&mut self, layout: &Layout) {
        self.0.set_layout(layout);
    }
}

impl TuiSink<'_> {
    /// A line this front-end says about itself, in the window for lines that name
    /// no session.
    fn say(&mut self, kind: LineKind, text: &str) {
        self.0.say(kind, text);
    }
}

/// One input line: `/` starts a command, anything else is a message to the agent.
fn submission(client: &mut cli::Client, line: &str, sink: &mut TuiSink) -> cli::Submission {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return cli::Submission::Rejected;
    }
    match trimmed.strip_prefix('/') {
        Some(command) => match shell_words::split(command) {
            Ok(tokens) => client.command(tokens, sink),
            Err(e) => {
                sink.say(LineKind::Error, &format!("unparsable input: {e}"));
                cli::Submission::Rejected
            }
        },
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

/// One attempt, resolving afresh: `connect_server` moves the address.
async fn dial_now(conn: cli::Connection) -> Result<(Link, Greeting), DialFailure> {
    let endpoint = conn.endpoint().map_err(DialFailure::Temporary)?;
    cli::dial(endpoint).await
}

/// The interactive front-end: the window outlives the link. See `docs/notes/tui-run.md`.
pub async fn interactive(args: &cli::Args) -> anyhow::Result<()> {
    // The command an agent's shell types, installed before any shell exists.
    let home = cli::prepare(args);

    let mut client = cli::build_client(args);
    let mut ui = Tui::new(client.completer())?;
    client.set_offline(0, &mut TuiSink(&mut ui));

    let mut link: Option<cli::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;

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
            Step::Input(Some(UiEvent::Quit)) => break Ok(()),
            Step::Input(Some(UiEvent::Submit(line))) => {
                let outcome = submission(&mut client, &line, &mut TuiSink(&mut ui));
                // A `connect_server` moved the address: drop the link and let
                // the next dial find the new one.
                if client.take_server_change() {
                    link = None;
                    backoff = RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    client.set_offline(0, &mut TuiSink(&mut ui));
                }
                match outcome {
                    cli::Submission::Local | cli::Submission::Rejected => {}
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
            }
            Step::Wire(None) => {
                link = None;
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
                        // Applying the greeting is the resync: catalog, sessions, model.
                        greeting.apply(&mut client, &mut TuiSink(&mut ui));
                        link = Some(open);
                        backoff = RECONNECT_MIN;
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        client.set_offline(attempts, &mut TuiSink(&mut ui));
                        // Once per outage: start what is not there, and the
                        // loop keeps dialing while the task does it.
                        let starting = if attempts == 1 && !args.client_only {
                            cli::server_of(&args.conn)
                                .ok()
                                .filter(|addr| cli::starts_here(addr))
                        } else {
                            None
                        };
                        if let Some(addr) = starting {
                            let home = home.clone();
                            tokio::spawn(async move {
                                let _ = cli::ensure(&home, addr).await;
                            });
                        }
                        // Once per outage: the status line already carries "attempt N".
                        if attempts == 1 {
                            // A cold start is not a fault. Nothing is listening
                            // yet, and the server about to come up is the answer
                            // -- so it is said as news, not as an error.
                            let (kind, line) = match (failure.nothing_listening(), starting) {
                                (true, Some(_)) => (
                                    LineKind::System,
                                    format!("{} — starting one", failure.message()),
                                ),
                                _ => (
                                    LineKind::Error,
                                    format!(
                                        "{} — retrying in the background, Alt+Q still leaves",
                                        failure.message()
                                    ),
                                ),
                            };
                            TuiSink(&mut ui).say(kind, &line);
                        }
                        backoff = (backoff * 2).min(RECONNECT_MAX);
                        next_try = tokio::time::Instant::now() + backoff;
                    }
                }
            }
        }
    };

    // Best-effort goodbye: saves the server its idle timeout; failure is harmless.
    if let Some(open) = link.take() {
        open.goodbye().await;
    }

    ui.restore()?;
    result
}
