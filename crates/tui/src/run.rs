//! The interactive client: a window, a link, and the loop between them.
//!
//! This is where the two halves meet. The command layer (`gongga-cli`) knows
//! what a line means and what to send; this crate knows what a terminal is. The
//! loop below is the only place that needs both, which is why it lives in the
//! crate that depends on the other one.
//!
//! `TuiSink` is the seam in the other direction: the command layer renders
//! through [`cli::Sink`], and this type is the implementation that paints a
//! terminal. Nothing above it decides colours.

use std::future::Future;
use std::pin::Pin;

use cli::{DialFailure, Greeting, LineKind, Link, Sink};
use tokio::time::Duration;

use crate::{Tui, UiEvent};

/// How long the interactive client waits before redialling, doubling up to the
/// cap. The first retry is fast enough to be invisible across a `dev.sh
/// restart`, and the cap keeps a server that is down for an hour from spinning.
///
/// This is a *redial* cadence, not a budget on anything the agent does: the
/// operator keeps the window, the history and the input line throughout, and
/// Ctrl+Q still works while it backs off.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Renders the command layer's lines into the terminal.
struct TuiSink<'a>(&'a mut Tui);

impl Sink for TuiSink<'_> {
    fn line(&mut self, kind: LineKind, text: &str) {
        self.0.push(kind, text);
    }

    fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
        self.0.stream(key, kind, prefix, text, done);
    }

    fn status(&mut self, text: &str) {
        self.0.set_status(text);
    }

    fn footer(&mut self, text: &str) {
        self.0.set_footer(text);
    }
}

/// What one input line asks for, which is the whole of the input syntax.
///
/// The grammar is decided by the first character -- the one piece of syntax an
/// operator has to know:
///
/// * `/` begins a command: `/send_message --session main --message hi`.
/// * Anything else is a message to the agent.
///
/// It lives here, not in the command layer, because it is a property of the one
/// input box: an input line has to mean *something* when the operator types
/// prose into it, so it needs a way to say "and now a command". A command line
/// has no such problem -- its argv is already tokens, and the process was started
/// with the command's name -- so `gongga send_message ...` takes no `/`. Carrying
/// the prefix over was not harmless: on Windows Git Bash rewrites a leading
/// `/word` into a `C:/.../` path before it is even an argument, and the whole line
/// then became a chat message to the agent.
fn submission(client: &mut cli::Client, line: &str, sink: &mut dyn Sink) -> cli::Submission {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return cli::Submission::Rejected;
    }
    match trimmed.strip_prefix('/') {
        Some(command) => match shell_words::split(command) {
            Ok(tokens) => client.command(tokens, sink),
            Err(e) => {
                sink.line(LineKind::Error, &format!("unparsable input: {e}"));
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

/// A dial kept alive between loop iterations.
///
/// Boxed rather than awaited inside the branch: an unreachable server takes the
/// whole handshake timeout to give up, and waiting for it inside the loop would
/// freeze the keyboard for five seconds at a stretch — the TUI would be
/// unresponsive exactly while the server is down, which is when the operator is
/// most likely to be pressing Ctrl+Q.
type Dialing = Pin<Box<dyn Future<Output = Result<(Link, Greeting), DialFailure>>>>;

/// The interactive front-end.
///
/// The window and the link are separate things. Nothing here makes the server's
/// presence a condition for the client to exist: the TUI opens first, dials
/// from inside the loop, and when the link dies it goes back to dialling
/// instead of taking the window down with it. A restart therefore looks like
/// two lines in the log — `connection lost`, then `connected` — with the input
/// line, the history and the scroll position exactly where they were.
pub async fn interactive(args: &cli::Args) -> anyhow::Result<()> {
    // Resolved before the terminal is taken over: a bad address or a missing
    // key is not fixed by retrying, and an error is far easier to read on the
    // normal screen than after `restore()` flashed the alternate screen away.
    let endpoint = args.conn.endpoint()?;

    let mut client = cli::build_client(args);
    let mut ui = Tui::new(client.completer())?;
    client.set_offline(0, &mut TuiSink(&mut ui));

    let mut link: Option<cli::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;

    let result = loop {
        // Every arm keeps the keyboard branch, so no state — connected, waiting,
        // or mid-handshake — can make the window stop answering keys.
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
                match submission(&mut client, &line, &mut TuiSink(&mut ui)) {
                    cli::Submission::Local | cli::Submission::Rejected => {}
                    // Nothing left this process, so the text is still the
                    // operator's. Put it back rather than log it away.
                    cli::Submission::Offline => ui.restore_input(&line),
                    cli::Submission::Send(msg) => match link.as_ref() {
                        Some(open) => {
                            if let Err(e) = open.send(msg).await {
                                    // The channel task is gone but its
                                    // half-open end had not been noticed yet.
                                    // This message may or may not have arrived,
                                    // so it is reported, not silently re-queued.
                                    TuiSink(&mut ui).line(
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
                        // Raced with the link going down between `submit` and
                        // here. Nothing was sent, so the line goes back.
                        None => ui.restore_input(&line),
                    },
                }
            }
            Step::Wire(Some(bytes)) => {
                // Decoding stays in the command layer, so the front-end never
                // names a protocol type: it hands over the bytes it read from
                // the link and reads the result off the screen.
                if let Err(e) = client.handle_bytes(&bytes, &mut TuiSink(&mut ui)) {
                    TuiSink(&mut ui).line(LineKind::Error, &format!("bad message: {e}"));
                }
            }
            Step::Wire(None) => {
                link = None;
                backoff = RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                client.set_offline(0, &mut TuiSink(&mut ui));
                TuiSink(&mut ui).line(LineKind::System, "server connection lost — reconnecting");
            }
            Step::Dial => {
                attempts += 1;
                dialing = Some(Box::pin(cli::dial(endpoint.clone())));
            }
            Step::Dialed(outcome) => {
                // Finished either way: clear it so a later outage cannot poll a
                // dead future, which would panic.
                drop(dialing.take());
                match outcome {
                    Ok((open, greeting)) => {
                        // Applying the greeting *is* the resync: from this
                        // moment the catalog, the session list and the model on
                        // screen are current, which is what makes a reconnect
                        // indistinguishable from a first start.
                        greeting.apply(&mut client, &mut TuiSink(&mut ui));
                        link = Some(open);
                        backoff = RECONNECT_MIN;
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        client.set_offline(attempts, &mut TuiSink(&mut ui));
                        // Once per outage: the status line carries "attempt N",
                        // so repeating this every few hundred milliseconds
                        // would bury the log without adding information.
                        if attempts == 1 {
                            TuiSink(&mut ui).line(
                                LineKind::Error,
                                &format!(
                                    "{} — retrying in the background, Ctrl+Q still leaves",
                                    failure.message()
                                ),
                            );
                        }
                        backoff = (backoff * 2).min(RECONNECT_MAX);
                        next_try = tokio::time::Instant::now() + backoff;
                    }
                }
            }
        }
    };

    // Best-effort goodbye: the server drops this connection now instead of
    // waiting out its idle timeout. Worth nothing if it fails — the window is
    // already closing.
    if let Some(open) = link.take() {
        open.goodbye().await;
    }

    ui.restore()?;
    result
}
