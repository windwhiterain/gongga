//! The interactive client: a window, a link, and the loop between them.

use std::future::Future;
use std::pin::Pin;

use link::{DialFailure, Greeting, Layout, LineKind, Link, Sink};
use tokio::time::Duration;

use crate::command::{self, Done};
use crate::state::State;
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

    fn layout(&mut self, layout: &Layout) {
        self.0.set_layout(layout);
    }

    fn rewrite(&mut self, window: &str, key: &str, text: &str) {
        self.0.rewrite(window, key, text);
    }
    fn tool_call(&mut self, window: &str, key: &str, head: &link::protocol::Head, _streamed: bool) {
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
///
/// # Invariant
///
/// Sends what the completion asked for, and reports whether the popup is out of
/// date. It runs outside the key handler, because a completion is answered
/// synchronously and a peer's directory cannot be.
async fn answer_completion(
    client: &mut link::Client,
    link: Option<&link::Link>,
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

/// Tells the client what this window's own journals hold, once per greeting.
///
/// # Invariant
///
/// Only what is written down: a window the journal says nothing about is left
/// undeclared, and an undeclared window is never fetched -- a fresh client has
/// nothing to be behind on. See `docs/notes/tui-run.md`.
fn declare(client: &mut link::Client, ui: &Tui) {
    let names: Vec<String> = client.sessions().iter().map(|s| s.name.clone()).collect();
    for name in names {
        match ui.stamp_of(&name) {
            Some((created, drawn)) => client.declare_drawn(&name, created, drawn),
            // No journal at all: nothing drawn, so the whole transcript is owed.
            None => client.declare_empty(&name),
        }
    }
}

/// Asks the server for the rows of the windows a frame drew and the greeting
/// found behind.
///
/// # Invariant
///
/// The draw is the trigger: a session nobody is reading is never fetched, so a
/// reconnect costs one request per window the operator is looking at rather than
/// one per session on the machine. The answer arrives as the events that drew
/// those rows the first time, so a refilled window is drawn by the code that drew
/// it live.
async fn answer_transcripts(client: &mut link::Client, link: Option<&link::Link>, ui: &mut Tui) {
    let Some(open) = link else {
        return;
    };
    let behind: Vec<String> = client.behind().into_iter().map(|(name, _)| name).collect();
    for name in behind {
        if !ui.drawn().iter().any(|drawn| *drawn == name) {
            continue;
        }
        let Some(message) = client.ask_transcript(&name) else {
            continue;
        };
        if open.send(message).await.is_err() {
            break;
        }
    }
}

/// One input line, answered.
///
/// # Invariant
///
/// A line the window runs itself never reaches the command layer, and a line the
/// server runs carries every argument the operator meant: the sticky entries are
/// filled here, where they are kept, and a filled value nothing accepts is left
/// out rather than sent as a statement the operator never made.
fn submission(
    client: &mut link::Client,
    state: &mut State,
    line: &str,
    sink: &mut TuiSink,
    moved: &mut Option<std::net::SocketAddr>,
) -> link::Submission {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return link::Submission::Rejected;
    }
    match classified(line) {
        Some(Ok(tokens)) => {
            let head = tokens.first().cloned().unwrap_or_default();
            if let Some(spec) = command::spec(&head) {
                client.echo(&tokens, None, sink);
                return match link::commands::parse_args(&tokens[1..], Some(&spec)) {
                    Ok(args) => {
                        if let Done::Moved(addr) = command::run(client, state, &head, &args, sink) {
                            *moved = Some(addr);
                        }
                        link::Submission::Local
                    }
                    Err(e) => {
                        client.say(LineKind::Error, &e.to_string(), sink);
                        link::Submission::Rejected
                    }
                };
            }
            client.command(filled(client, state, tokens, sink), sink)
        }
        Some(Err(e)) => {
            sink.say(LineKind::Error, &e);
            link::Submission::Rejected
        }
        None => match state.sticky().get("session") {
            Some(session) => client.message(trimmed, session, sink),
            None => {
                client.say(
                    LineKind::Error,
                    "no session to send to: set one with `/set_sticky session <name>`, or start the line with `/` and name the command",
                    sink,
                );
                link::Submission::Rejected
            }
        },
    }
}

/// One command line with the sticky entries filled in.
///
/// # Invariant
///
/// A value lands on a slot that is still empty, and only through the argument
/// whose `kind` names it; what the caller typed wins. A filled value the
/// published `accepts` refuses is left out *and said*, because a default is not
/// something the operator stated: sending it would be refused for a value nobody
/// typed, and dropping it silently would hide that the entry is still in the file.
fn filled(
    client: &link::Client,
    state: &State,
    tokens: Vec<String>,
    sink: &mut dyn Sink,
) -> Vec<String> {
    let head = tokens.first().cloned().unwrap_or_default();
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == head) else {
        return tokens;
    };
    let Ok(args) = link::commands::parse_args(&tokens[1..], Some(&spec)) else {
        // The command layer's own diagnosis is better than none: let it speak.
        return tokens;
    };
    let mut merged = args.clone();
    state.sticky().fill(Some(&spec), &mut merged);
    let line = format!("/{}", tokens.join(" "));
    let mut filled = tokens;
    for arg in &spec.args {
        // Typed, or no entry names it: nothing to add.
        if args.contains_key(&arg.name) {
            continue;
        }
        let Some(value) = merged.get(&arg.name).cloned() else {
            continue;
        };
        if client
            .refused_values(&line, &merged)
            .iter()
            .any(|(name, _)| *name == arg.name)
        {
            sink.note(
                LineKind::System,
                &format!(
                    "sticky {} = {value} is not one this model takes; left to the model's own",
                    arg.kind.as_deref().unwrap_or(&arg.name)
                ),
            );
            continue;
        }
        // A flag, whatever its position: filled, not re-ordered, and one token.
        filled.push(format!("--{}", arg.name));
        filled.push(value);
    }
    filled
}

/// Tells the view and the completion what the window's own state is now.
///
/// # Invariant
///
/// One place sets all of it, after any change: the block size, the margin, the
/// sticky session, the sessions drawn beside the list, and the completion's own
/// vocabulary. A front-end that set half of it would draw one state and complete
/// against another.
fn sync(client: &mut link::Client, state: &State, window: &command::Window, ui: &mut Tui) {
    ui.set_block_size(state.block_size().0);
    ui.set_box_edge(state.box_edge().0);
    ui.set_sticky_session(state.sticky().get("session"));
    client.set_sticky_vocabulary(&state.sticky().names(), state.sticky().get("session"));
    client.set_shown(state.shown_names());
    window.refresh(client, state);
    ui.refresh_completion();
    client.refresh(&mut TuiSink(ui));
}

/// What the window keeps for a command it is about to send.
///
/// # Invariant
///
/// Two things, both SPEC's: a session the command names is drawn and becomes the
/// sticky one, and an accepted `set_session` writes back every mode it carried.
/// Neither is written before the answer -- a refused command writes nothing.
fn follow(
    client: &link::Client,
    state: &mut State,
    msg: &link::protocol::ClientMessage,
    line: &str,
    pending: &mut Vec<(u64, crate::state::Follow)>,
) {
    let link::protocol::ClientMessage::Command { id, name, args } = msg else {
        return;
    };
    if let Some(session) = client.session_named(name, args).map(str::to_string) {
        if !state.is_shown(&session) {
            let _ = state.set_shown(&session, true);
        }
        let _ = state.set_sticky("session", &session);
    }
    if name == "set_session"
        && let Some(spec) = client
            .commands()
            .into_iter()
            .find(|spec| spec.name == *name)
    {
        // Only what was typed: a filled value is already in the file.
        let typed = typed_names(client, line);
        let pairs: Vec<(String, String)> = spec
            .args
            .iter()
            .filter(|arg| typed.contains(&arg.name))
            .filter_map(|arg| {
                let kind = arg.kind.clone()?;
                let value = args.get(&arg.name)?.clone();
                Some((kind, value))
            })
            .collect();
        if !pairs.is_empty() {
            pending.push((*id, pairs));
        }
    }
}

/// The argument names a line spelled out, before any sticky filled one in.
fn typed_names(client: &link::Client, line: &str) -> Vec<String> {
    let Some(Ok(tokens)) = classified(line) else {
        return Vec::new();
    };
    let head = tokens.first().cloned().unwrap_or_default();
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == head) else {
        return Vec::new();
    };
    link::commands::parse_args(&tokens[1..], Some(&spec))
        .map(|args| args.keys().cloned().collect())
        .unwrap_or_default()
}

/// Applies what the window kept for the commands the server has now answered.
///
/// # Invariant
///
/// Only an *accepted* command writes its entries back: a refusal leaves the file
/// as it was, which is why nothing was written when the line was sent.
fn settle(
    client: &link::Client,
    state: &mut State,
    pending: &mut Vec<(u64, crate::state::Follow)>,
    ui: &mut Tui,
) {
    for (id, ok) in ui.take_settled() {
        let Some(at) = pending.iter().position(|(other, _)| *other == id) else {
            continue;
        };
        let (_, pairs) = pending.remove(at);
        if !ok {
            continue;
        }
        let written: Vec<String> = pairs.iter().map(|(kind, _)| kind.clone()).collect();
        let mut changed: Vec<String> = Vec::new();
        for (kind, value) in &pairs {
            if state.change_sticky(kind, value).unwrap_or(false) {
                changed.push(kind.clone());
            }
        }
        // SPEC's `sticky to xxx`: what follows a changed model is forgotten, unless
        // this command stated it too.
        for (kind, parent) in command::follows(client) {
            if changed.contains(&parent) && !written.contains(&kind) {
                let _ = state.del_sticky(&kind);
            }
        }
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

/// Whose server answered, from what this window knows about it.
///
/// # Invariant
///
/// Being told is not owning: only two facts count, and both were decided before
/// any dial -- the pid `spawn` answered (the server, or the watchdog above it),
/// and the pid the server follows being this window's own, which a handover
/// carries in `--parent`. See `docs/notes/tui-run.md`.
fn whose_server(spawned: Option<u32>, greeting: &Greeting) -> Server {
    let pid = greeting.pid();
    if spawned.is_some() && (spawned == Some(pid) || spawned == greeting.supervisor()) {
        return Server::Ours { pid };
    }
    match greeting.owner() {
        Some(owner) if owner == std::process::id() => Server::Handover { pid },
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
///
/// # Invariant
///
/// Nothing of this window goes on the wire as a claim: a server's lifetime is
/// decided by whoever started it, so a window that merely dials -- into a home
/// somebody serves by hand, or onto a `connect_server` -- reads it and leaves it
/// running when it closes. See `docs/notes/tui-run.md`.
async fn dial_now(conn: link::Connection) -> Result<(Link, Greeting), DialFailure> {
    let endpoint = conn.endpoint().map_err(DialFailure::Temporary)?;
    link::dial(endpoint).await
}

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
pub async fn interactive(args: &link::Args) -> anyhow::Result<()> {
    // The command an agent's shell types, installed before any shell exists.
    let home = link::home_of(args);
    // Both are this process's own: where a row lives never depends on a greeting.
    let client_home = link::client_home_of(args);
    // The window's own state: sticky entries, drawn sessions, settings, address.
    let mut state = State::load(&home);
    // The address in force: what the window remembers first, then the flags.
    let mut conn = args.conn.clone();
    if let Some(addr) = state.connection() {
        conn.server = Some(addr.to_string());
    }
    let mut endpoint = link::endpoint_name(&conn);

    let mut client = link::build_client(args);
    let window = command::Window::new(client.completer());
    let mut ui = Tui::new(window.shared(), &client_home, &endpoint)?;
    // Nothing has been sent yet: a fresh client owes nobody a line.
    let _ = client.set_offline(0, &mut TuiSink(&mut ui));
    sync(&mut client, &state, &window, &mut ui);

    let mut link: Option<link::Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;
    // Started once, before any dial; what it spawned is its to stop, so the
    // server on the address is stopped first.
    let spawned: Option<u32> = match starts_a_server(args.client_only) {
        true => match link::server_of(&conn) {
            Ok(addr) => {
                if let Some(old) = link::stop(&conn, &home, addr).await {
                    TuiSink(&mut ui).say(
                        LineKind::System,
                        &format!("stopped the server that was serving this home (pid {old})"),
                    );
                }
                // The key this window dials with, so the child does not mint
                // another one and refuse the window that started it.
                let key = conn.key.clone();
                link::spawn(&home, addr, Some(std::process::id()), key.as_deref())
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
    let mut pending: Vec<(u64, crate::state::Follow)> = Vec::new();
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
            // A gesture of the window's own: an operation, not a line.
            Step::Input(Some(UiEvent::Operate(operation))) => {
                let outcome =
                    command::operate(&mut client, &mut state, operation, &mut TuiSink(&mut ui));
                // Whatever the gesture changed of the window's own state reaches
                // the drawing and the completion before the next event.
                sync(&mut client, &state, &window, &mut ui);
                if let link::Submission::Send(msg) = outcome
                    && let Some(open) = link.as_ref()
                    && let Err(e) = open.send(msg).await
                {
                    TuiSink(&mut ui).say(
                        LineKind::Error,
                        &format!("the operation could not be sent: {e}"),
                    );
                }
            }
            Step::Input(Some(UiEvent::Submit(line))) => {
                let mut moved = None;
                let outcome = submission(
                    &mut client,
                    &mut state,
                    &line,
                    &mut TuiSink(&mut ui),
                    &mut moved,
                );
                // Whatever the line changed of the window's own state reaches
                // the screen and the completion before the next keystroke.
                sync(&mut client, &state, &window, &mut ui);
                // A `connect_server` moved the address: drop the link and let
                // the next dial find the new one.
                if let Some(addr) = moved {
                    conn.server = Some(addr.to_string());
                    // The rows on screen are the old endpoint's: they go, and the
                    // next frame reads this one's records instead.
                    endpoint = link::endpoint_name(&conn);
                    ui.repoint(&client_home, &endpoint);
                    link = None;
                    ui.set_server(None);
                    backoff = RECONNECT_MIN;
                    attempts = 0;
                    next_try = tokio::time::Instant::now();
                    let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                    give_back(&mut ui, unanswered);
                }
                match outcome {
                    link::Submission::Local | link::Submission::Rejected => {}
                    // Nothing left this process, so the text is still the operator's:
                    // put it back rather than log it away.
                    link::Submission::Offline => ui.restore_input(&line),
                    link::Submission::Send(msg) => match link.as_ref() {
                        Some(open) => {
                            // The command names a session: that session's window
                            // is drawn, and what the server accepts is kept.
                            follow(&client, &mut state, &msg, &line, &mut pending);
                            sync(&mut client, &state, &window, &mut ui);
                            if open.send(msg).await.is_err() {
                                // Its end went unnoticed: nothing was sent, and the
                                // line goes back like any other unanswered one.
                                link = None;
                                ui.set_server(None);
                                backoff = RECONNECT_MIN;
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
                settle(&client, &mut state, &mut pending, &mut ui);
                sync(&mut client, &state, &window, &mut ui);
            }
            Step::Wire(None) => {
                link = None;
                ui.set_server(None);
                backoff = RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                let unanswered = client.set_offline(0, &mut TuiSink(&mut ui));
                TuiSink(&mut ui).say(LineKind::System, "server connection lost — reconnecting");
                give_back(&mut ui, unanswered);
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
                        ui.set_server(Some(whose_server(spawned, &greeting)));
                        // Applying the greeting is what the server says; what
                        // this window kept is the other half of the comparison.
                        greeting.apply(&mut client, &mut TuiSink(&mut ui));
                        declare(&mut client, &ui);
                        link = Some(open);
                        backoff = RECONNECT_MIN;
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
                        backoff = (backoff * 2).min(RECONNECT_MAX);
                        next_try =
                            tokio::time::Instant::now() + wait_for(spawned, attempts, backoff);
                    }
                }
            }
        }
        // After the step, never during it: a peer's directory is not a
        // synchronous answer.
        answer_completion(&mut client, link.as_ref(), &mut ui, &mut drawn_listings).await;
        // Same place, same reason: what a frame drew decides what is fetched.
        answer_transcripts(&mut client, link.as_ref(), &mut ui).await;
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
