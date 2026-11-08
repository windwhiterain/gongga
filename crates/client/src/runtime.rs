//! The connection lifecycle a window shares with the client process.
//!
//! See `docs/notes/client-runtime.md`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use crate::render_wire::UiIntent;
use crate::window_refill::WindowFacts;
use crate::window_sync::WindowView;
use crate::{Connection, DialFailure, Greeting, Link, Listed, Sink};

/// First redial delay, doubling up to the cap; a cadence, not a budget.
///
/// # Invariant
///
/// Backing off never costs the window: history, input and scroll stay put, and
/// Alt+Q still leaves. See `docs/notes/client-runtime.md`.
pub const RECONNECT_MIN: Duration = Duration::from_millis(250);
pub const RECONNECT_MAX: Duration = Duration::from_secs(5);

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
