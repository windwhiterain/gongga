//! The connection lifecycle a window shares with the client process.
//!
//! See `docs/notes/client-runtime.md`.

use std::time::Duration;

use crate::{Connection, DialFailure, Greeting, Link};

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
