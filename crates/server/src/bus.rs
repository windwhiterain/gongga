//! Fan-out of [`Event`]s to every connected client.

use protocol::Event;
use tokio::sync::broadcast;

/// Broadcast bus with a bounded buffer.
///
/// The buffer is generous because streaming produces bursts; a slow client that
/// falls behind gets `RecvError::Lagged` rather than silently dropping the
/// server's throughput, and the TUI shows that it skipped frames.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: Event) {
        // No receivers is the common case when the CLI runs headless; ignore it.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
