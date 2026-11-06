//! The commands the window runs itself, against its own state.
//!
//! See `docs/notes/tui-command.md`.

use std::net::SocketAddr;

use link::protocol::{self, Args, CommandSpec};
use link::{LineKind, Sink};

use crate::state::{self, BlockSize, BoxEdge, State};

/// What one client command did.
pub enum Done {
    /// Answered here; nothing to dial.
    Local,
    /// `connect_server`: the address this window dials from now on.
    Moved(SocketAddr),
}

/// The spec of the command this line names, when the window runs it itself.
pub fn spec(name: &str) -> Option<CommandSpec> {
    protocol::client_commands()
        .into_iter()
        .find(|spec| spec.name == name)
}

/// Whether a line's head is one of the window's own commands.
pub fn handles(name: &str) -> bool {
    spec(name).is_some()
}

/// Runs one of the window's own commands, whose arguments are already bound.
///
/// # Invariant
///
/// Every answer replaces the command row, the way an answer to a line the server
/// ran does: SPEC's row is the last thing asked or told, not a log. Nothing here
/// dials, except the one command whose whole meaning is an address.
pub fn run(
    client: &link::Client,
    state: &mut State,
    name: &str,
    args: &Args,
    sink: &mut dyn Sink,
) -> Done {
    match name {
        "connect_server" => connect(client, state, args, sink),
        // The process is the server: a window cannot draw it, so say where to
        // type it.
        "serve" => {
            client.say(
                LineKind::Error,
                &format!(
                    "serve takes over the process; run it in a shell: `{} serve <port>`",
                    link::typed_name()
                ),
                sink,
            );
            Done::Local
        }
        "set_render" => render(client, state, args, sink),
        "set_render_session" => render_session(client, state, args, sink),
        "set_sticky" => {
            let kind = args.get("type").cloned().unwrap_or_default();
            let Some(value) = args.get("value").cloned() else {
                client.say(
                    LineKind::Error,
                    &format!(
                        "set_sticky is missing required argument(s): value; it takes {}",
                        link::commands::signature(spec(name).as_ref()),
                    ),
                    sink,
                );
                return Done::Local;
            };
            // An entry no argument declares can never take effect: refused, unless
            // no catalog has arrived to prove it wrong.
            let kinds: Vec<String> = client
                .commands()
                .iter()
                .flat_map(|command| command.args.iter())
                .filter_map(|arg| arg.kind.clone())
                .collect();
            if !kinds.is_empty() && !kinds.contains(&kind) {
                client.say(
                    LineKind::Error,
                    &format!(
                        "{kind:?} is not a type any published argument declares, so no \
                         sticky entry can use it; the types are: {}",
                        sorted(kinds).join(", ")
                    ),
                    sink,
                );
                return Done::Local;
            }
            write_sticky(client, state, &kind, &value, sink);
            Done::Local
        }
        "del_sticky" => {
            let kind = args.get("type").cloned().unwrap_or_default();
            match state.del_sticky(&kind) {
                Ok(true) => {
                    client.say(LineKind::Result, &format!("sticky {kind} cleared"), sink);
                    forget(client, state, &kind, sink);
                }
                Ok(false) => client.say(
                    LineKind::Error,
                    &format!("no sticky argument named {kind:?}"),
                    sink,
                ),
                Err(e) => client.say(LineKind::Error, &format!("could not save: {e}"), sink),
            }
            Done::Local
        }
        // Published as a client command this build cannot run: say so.
        other => {
            client.say(
                LineKind::Error,
                &format!(
                    "{other:?} is a client command, but this window has no handler for it: \
                     the two are from different builds"
                ),
                sink,
            );
            Done::Local
        }
    }
}

/// `connect_server`: the address this window dials from now on.
fn connect(client: &link::Client, state: &mut State, args: &Args, sink: &mut dyn Sink) -> Done {
    let port = args.get("port").map(String::as_str).unwrap_or_default();
    let ip = args.get("ip").map(String::as_str);
    let addr = match link::server_address(ip, port) {
        Ok(addr) => addr,
        Err(e) => {
            client.say(LineKind::Error, &format!("{e}"), sink);
            return Done::Local;
        }
    };
    // Saved before the window is told, so a redial at once reads the new
    // address rather than the old one.
    match state.remember(addr) {
        Ok(()) => {
            client.say(
                LineKind::Result,
                &format!("server is {addr}; reconnecting"),
                sink,
            );
            Done::Moved(addr)
        }
        Err(e) => {
            client.say(LineKind::Error, &format!("could not save: {e}"), sink);
            Done::Local
        }
    }
}

/// `set_render`: how tall a block may get, and the margin around it.
///
/// # Invariant
///
/// A value that does not parse saves nothing, and no argument at all is a read of
/// the setting rather than a reset of it.
fn render(client: &link::Client, state: &mut State, args: &Args, sink: &mut dyn Sink) -> Done {
    let size = args.get("block_size");
    let edge = args.get("box_edge");
    if size.is_none() && edge.is_none() {
        client.say(
            LineKind::Result,
            &format!(
                "render block_size = {}, box_edge = {}",
                state.block_size().0,
                state.box_edge().0
            ),
            sink,
        );
        return Done::Local;
    }
    // Both parsed before either is saved: half of a setting that does not parse
    // is a value nobody asked for.
    let size = match size.map(|raw| BlockSize::parse(raw)).transpose() {
        Ok(size) => size,
        Err(e) => {
            client.say(LineKind::Error, &e, sink);
            return Done::Local;
        }
    };
    let edge = match edge.map(|raw| BoxEdge::parse(raw)).transpose() {
        Ok(edge) => edge,
        Err(e) => {
            client.say(LineKind::Error, &e, sink);
            return Done::Local;
        }
    };
    if let Some(size) = size
        && let Err(e) = state.set_block_size(size)
    {
        client.say(LineKind::Error, &format!("could not save: {e}"), sink);
        return Done::Local;
    }
    if let Some(edge) = edge
        && let Err(e) = state.set_box_edge(edge)
    {
        client.say(LineKind::Error, &format!("could not save: {e}"), sink);
        return Done::Local;
    }
    client.say(
        LineKind::Result,
        &format!(
            "render block_size = {}, box_edge = {}",
            state.block_size().0,
            state.box_edge().0
        ),
        sink,
    );
    Done::Local
}

/// `set_render_session`: whether one session is drawn beside the list.
fn render_session(
    client: &link::Client,
    state: &mut State,
    args: &Args,
    sink: &mut dyn Sink,
) -> Done {
    let name = args.get("session").cloned().unwrap_or_default();
    let Some(raw) = args.get("show") else {
        let show = state.is_shown(&name);
        client.say(LineKind::Result, &format!("render {name} = {show}"), sink);
        return Done::Local;
    };
    match state::parse_show(raw) {
        Ok(show) => write_session(client, state, &name, show, false, sink),
        Err(e) => client.say(LineKind::Error, &e, sink),
    }
    Done::Local
}

/// Answers one of the window's own gestures.
///
/// # Invariant
///
/// A gesture ends where the command that says the same thing ends:
/// `StickySession` is `set_sticky session` and `RenderSession` is
/// `set_render_session`. `solo` is the part a typed line cannot ask for:
/// `write_session` applies it where the shown set is written, so a gesture costs
/// one file write and one line. `delete_session` is the command layer's.
/// See `docs/notes/tui-command.md`.
pub fn operate(
    client: &mut link::Client,
    state: &mut State,
    operation: protocol::Operation,
    sink: &mut dyn Sink,
) -> link::Submission {
    match operation {
        protocol::Operation::StickySession { session } => {
            write_sticky(client, state, "session", &session, sink);
            link::Submission::Local
        }
        protocol::Operation::RenderSession {
            session,
            show,
            solo,
        } => {
            write_session(client, state, &session, show, solo, sink);
            link::Submission::Local
        }
        operation => client.apply(operation, sink),
    }
}

/// Writes one sticky entry, says what it is, and forgets what followed it.
fn write_sticky(
    client: &link::Client,
    state: &mut State,
    kind: &str,
    value: &str,
    sink: &mut dyn Sink,
) {
    match state.change_sticky(kind, value) {
        Ok(changed) => {
            client.say(LineKind::Result, &format!("sticky {kind} = {value}"), sink);
            if changed {
                forget(client, state, kind, sink);
            }
        }
        Err(e) => client.say(LineKind::Error, &format!("could not save: {e}"), sink),
    }
}

/// Shows or hides one session's window.
///
/// # Invariant
///
/// It changes a view and never the state: the blocks a session already said are
/// still held, so `true` after `false` brings them back. It also moves the
/// sticky session -- shown makes it sticky, hiding the sticky one clears it --
/// so a click and a typed line leave the same window behind. `solo` closes the
/// other windows in the same write: a selection is one session.
fn write_session(
    client: &link::Client,
    state: &mut State,
    name: &str,
    show: bool,
    solo: bool,
    sink: &mut dyn Sink,
) {
    let written = if show && solo {
        // Only a session the window draws is a window: the list on screen is the
        // one a linkless start kept.
        let listed = client.listed();
        let live: Vec<&str> = listed.iter().map(|row| row.name.as_str()).collect();
        state.show_only(name, |held| live.contains(&held))
    } else {
        state.set_shown(name, show).map(|()| 0)
    };
    let closed = match written {
        Ok(closed) => closed,
        Err(e) => {
            client.say(LineKind::Error, &format!("could not save: {e}"), sink);
            return;
        }
    };
    let saved = if show {
        state.set_sticky("session", name)
    } else if state.sticky().get("session") == Some(name) {
        state.del_sticky("session").map(|_| ())
    } else {
        Ok(())
    };
    if let Err(e) = saved {
        client.say(LineKind::Error, &format!("could not save: {e}"), sink);
    }
    // One line: what the operator asked for, and what else it closed.
    let others = match closed {
        0 => String::new(),
        closed => format!(", {closed} other session(s) hidden"),
    };
    client.say(
        LineKind::Result,
        &format!("render {name} = {show}{others}"),
        sink,
    );
}

/// Forgets what followed a sticky entry that just changed, and says so.
///
/// # Invariant
///
/// SPEC's `sticky to xxx` is applied where the entries live: a model's level is
/// not carried to another model, and the reader is told rather than left to
/// notice that a line it never typed stopped applying.
pub fn forget(client: &link::Client, state: &mut State, changed: &str, sink: &mut dyn Sink) {
    let gone = state.forget_followers(changed, &follows(client));
    if gone.is_empty() {
        return;
    }
    // One row: SPEC's row is the last thing told, and a row each would leave one.
    let (what, verb) = if gone.len() == 1 {
        ("sticky entry".to_string(), "it follows")
    } else {
        ("sticky entries".to_string(), "they follow")
    };
    client.say(
        LineKind::System,
        &format!("{what} {} cleared: {verb} {changed}", gone.join(", ")),
        sink,
    );
}

/// Every published dependency: the entry a value keys on, and the one it follows.
pub fn follows(client: &link::Client) -> Vec<(String, String)> {
    client
        .commands()
        .iter()
        .flat_map(|command| command.args.iter())
        .filter_map(|arg| {
            let kind = arg.kind.clone()?;
            let parent = arg.sticky_to.clone()?;
            Some((kind, parent))
        })
        .collect()
}

/// The kinds once each, in the order a reader can scan them.
fn sorted(mut kinds: Vec<String>) -> Vec<String> {
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// The completion a window offers: its own commands, then the server's catalog.
///
/// # Invariant
///
/// One catalog per caller, and the window seeds its own with the commands it
/// runs: the command layer's catalog carries what the server publishes and
/// nothing of this one's, so a one-shot process cannot complete them and cannot
/// be handed one by accident.
pub struct Window {
    own: link::Catalog,
    server: link::SharedCompleter,
}

impl Window {
    pub fn new(server: link::SharedCompleter) -> Self {
        let own = link::Catalog::new();
        own.set_commands(protocol::client_commands());
        Self { own, server }
    }

    /// Hands the completion to a front-end that reads it.
    pub fn shared(&self) -> link::SharedCompleter {
        std::sync::Arc::new(Self {
            own: self.own.clone(),
            server: self.server.clone(),
        })
    }

    /// Tells the completion what the window's own state is now.
    pub fn refresh(&self, client: &link::Client, state: &State) {
        self.own.set_sticky_names(&state.sticky().names());
        self.own.set_message_session(state.sticky().get("session"));
        self.own.set_sessions(client.sessions().to_vec());
        self.own
            .set_roots(state.home(), &std::env::current_dir().unwrap_or_default());
    }
}

impl link::Completer for Window {
    fn complete(&self, line: &str, cursor: usize) -> link::Offered {
        match head(line) {
            Some(name) if handles(name) => self.own.complete(line, cursor),
            // The word names both sides: `/set` is `set_session` and `set_render`.
            // See `docs/notes/tui-command.md`.
            _ if line.trim_start().starts_with('/') && in_command_word(line, cursor) => {
                self.both(line, cursor)
            }
            _ => self.server.complete(line, cursor),
        }
    }

    fn wants(&self) -> bool {
        // A client command names no remote, so a listing is never wanted here.
        self.server.wants()
    }
}

impl Window {
    /// Both sides' command words: the ones this window runs and the ones the
    /// server does.
    ///
    /// # Invariant
    ///
    /// Names, not arguments: past its command word a line belongs to one command,
    /// and which one is the head's to say, so no argument is ever completed from
    /// the window's own catalog on a line the server will run.
    fn both(&self, line: &str, cursor: usize) -> link::Offered {
        let typed = head(line).unwrap_or_default();
        // Two catalogs answer the one word: the trait is what makes `Catalog`
        // callable beside the server's completer.
        let own = link::Completer::complete(&self.own, line, cursor);
        merged(self.server.complete(line, cursor), own, typed)
    }
}

/// Whether the cursor is still in the line's command word.
///
/// # Invariant
///
/// The word the slash starts, and nothing after the first space: past it the line
/// belongs to one command, whose own catalog answers for it.
fn in_command_word(line: &str, cursor: usize) -> bool {
    let start = line.len() - line.trim_start().len();
    let rest = &line[start..];
    let end = start + rest.find(char::is_whitespace).unwrap_or(rest.len());
    cursor.min(line.len()) <= end
}

/// One side's candidates, then the other's, under one rule about order.
///
/// # Invariant
///
/// A prefix match comes first across both lists, not within each: two catalogs
/// sorted apart would read as the server's answers followed by the window's, and
/// the operator would read that as one list in a strange order.
fn merged(mut server: link::Offered, own: link::Offered, typed: &str) -> link::Offered {
    for candidate in own.candidates {
        if !server
            .candidates
            .iter()
            .any(|held| held.insert == candidate.insert)
        {
            server.candidates.push(candidate);
        }
    }
    server
        .candidates
        .sort_by_key(|c| !c.insert.trim_start_matches('/').starts_with(typed));
    server
}

/// The command word of a line being typed, without its slash.
fn head(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix('/')?
        .split_whitespace()
        .next()
        .filter(|word| !word.is_empty())
}
