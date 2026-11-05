//! Fan-out of [`Event`]s to every connected client.
//!
//! See `docs/notes/server-net.md` and `docs/notes/protocol-message.md`.

use std::sync::{Arc, Mutex};

use protocol::{Delta, Event};
use tokio::sync::broadcast;

/// Broadcast bus with a bounded buffer; a lagging client is told it lagged.
///
/// # Invariant
///
/// A client that falls behind gets `RecvError::Lagged`: it never slows the server's
/// throughput. Every event carries the stream's cursor, handed out here and nowhere
/// else, which is what makes "what did a client miss" a comparison. See
/// [`EventBus::as_of`] and [`EventBus::step`].
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Delta>,
    /// The last number handed out. Also the gate a snapshot is read through.
    seq: Arc<Mutex<u64>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            seq: Arc::new(Mutex::new(0)),
        }
    }

    /// Publishes one event and answers the number it was given.
    pub fn publish(&self, event: Event) {
        self.step(|_| ((), Some(event)));
    }

    /// One step of the stream: `act` runs with the stream stopped and is told the
    /// number its event would get.
    ///
    /// # Invariant
    ///
    /// The only way to change what a snapshot can see *and* publish what explains it
    /// in one move: an answer being written and the chunk that carries it land
    /// together, so a snapshot afterwards has text and cursor together. `act` must not
    /// reach the bus, nor a lock the snapshot path takes, or it waits for itself.
    pub fn step<T>(&self, act: impl FnOnce(u64) -> (T, Option<Event>)) -> (u64, T) {
        let mut inner = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        let next = *inner + 1;
        let (value, event) = act(next);
        let Some(event) = event else {
            // A step that only reads: the number it was told is not consumed.
            return (*inner, value);
        };
        *inner = next;
        drop(inner);
        // No receivers is the common case when the CLI runs headless; ignore it.
        let _ = self.tx.send(Delta { seq: next, event });
        (next, value)
    }

    /// Reads a snapshot that everything published before it accounts for.
    ///
    /// # Invariant
    ///
    /// The number returned is the stream's position of the reading: every event
    /// published from now on has a number above it, so a client can tell which
    /// of the events it receives are already inside the snapshot and which are
    /// news. `read` runs under the publishing gate and must therefore not
    /// publish.
    pub fn as_of<T>(&self, read: impl FnOnce() -> T) -> (u64, T) {
        let inner = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        (*inner, read())
    }

    /// The stream's position right now: the number of the last event published.
    ///
    /// # Invariant
    ///
    /// A number, not a subscription: a client greeted here receives every event
    /// numbered above it, and one it has already drawn is never sent twice.
    pub fn position(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Delta> {
        self.tx.subscribe()
    }
}
