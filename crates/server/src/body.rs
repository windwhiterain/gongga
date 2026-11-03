//! The request body: the stored items, read in order. See `docs/notes/server-body.md`.
//!
//! # Invariant
//!
//! This module only ever *copies* rows. What a row is sent as was decided when it
//! was appended (`store::append_message`), and nothing here re-derives it: a
//! request is the concatenation of the stored bytes in `seq` order, which is what
//! makes the outgoing body a strict extension of the previous one. Every re-derivation
//! that used to live here -- reordering a step, inventing an answer for a call that
//! never returned, substituting a picture's file id, skipping a row whose payload had
//! gone -- rewrote bytes the provider had already cached, and each one silently cost
//! the whole transcript.

use std::collections::VecDeque;
use std::io::Error as IoError;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::Stream;

use crate::transcript::Transcript;

/// A model request's body, produced as the network asks for it.
pub struct RequestBody {
    /// The object up to and including `"input":[`.
    head: Option<Vec<u8>>,
    rows: Option<Transcript>,
    /// Encoded items waiting to be read.
    ready: VecDeque<Vec<u8>>,
    tail: Option<Vec<u8>>,
    /// An item has been written, so the next needs a separator before it.
    wrote: bool,
    /// How many bytes the network has taken, counted as they go.
    counter: Arc<AtomicUsize>,
}

impl RequestBody {
    pub fn new(head: Vec<u8>, rows: Transcript, counter: Arc<AtomicUsize>) -> Self {
        Self {
            head: Some(head),
            rows: Some(rows),
            ready: VecDeque::new(),
            tail: Some(b"]}".to_vec()),
            wrote: false,
            counter,
        }
    }

    /// Queues one item, with the comma that separates it from the last.
    fn emit(&mut self, bytes: &str) {
        if self.wrote {
            self.ready.push_back(b",".to_vec());
        }
        self.wrote = true;
        self.ready.push_back(bytes.as_bytes().to_vec());
    }

    /// Produces the next chunk, or says the body is over.
    fn fill(&mut self) -> Result<bool, IoError> {
        if let Some(head) = self.head.take() {
            self.ready.push_back(head);
            return Ok(true);
        }
        if let Some(rows) = self.rows.as_mut() {
            match rows.next() {
                Some(Err(e)) => return Err(IoError::other(e)),
                Some(Ok(item)) => {
                    self.emit(&item);
                    return Ok(true);
                }
                None => {
                    self.rows = None;
                    return Ok(true);
                }
            }
        }
        if let Some(tail) = self.tail.take() {
            self.ready.push_back(tail);
            return Ok(true);
        }
        Ok(false)
    }
}

impl Stream for RequestBody {
    type Item = Result<Bytes, IoError>;

    /// One chunk per poll: reading a row is a seek, not a wait.
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(chunk) = this.ready.pop_front() {
                if chunk.is_empty() {
                    continue;
                }
                this.counter.fetch_add(chunk.len(), Ordering::Relaxed);
                return Poll::Ready(Some(Ok(Bytes::from(chunk))));
            }
            match this.fill() {
                Ok(true) => continue,
                Ok(false) => return Poll::Ready(None),
                Err(e) => return Poll::Ready(Some(Err(e))),
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // Unknown: the transcript is read as it is sent, and its length is not a
        // promise the network can hold to anyway.
        (0, None)
    }
}
