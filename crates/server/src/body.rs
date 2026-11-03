//! The request body: the stored items, read in order. See `docs/notes/server-body.md`.
//!
//! # Invariant
//!
//! A row is copied as stored; the one value resolved while writing is a picture's provider
//! file id. Everything else was decided when the row was appended. See
//! `docs/notes/server-body.md`.

use std::collections::VecDeque;
use std::io::Error as IoError;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::Value;

use crate::image::{ImageRef, files::Files};
use crate::transcript::Transcript;

/// A model request's body, produced as the network asks for it.
pub struct RequestBody {
    /// The object up to and including `"input":[`.
    head: Option<Vec<u8>>,
    rows: Option<Transcript>,
    /// What this request knows about the files behind its pictures.
    files: Files,
    /// Encoded items waiting to be read.
    ready: VecDeque<Vec<u8>>,
    tail: Option<Vec<u8>>,
    /// An item has been written, so the next needs a separator before it.
    wrote: bool,
    /// How many bytes the network has taken, counted as they go.
    counter: Arc<AtomicUsize>,
    /// A row whose pictures were still being resolved when the last poll returned.
    pending: Option<Next>,
}

impl RequestBody {
    pub fn new(head: Vec<u8>, rows: Transcript, files: Files, counter: Arc<AtomicUsize>) -> Self {
        Self {
            head: Some(head),
            rows: Some(rows),
            files,
            ready: VecDeque::new(),
            tail: Some(b"]}".to_vec()),
            wrote: false,
            counter,
            pending: None,
        }
    }

    /// Queues one item, with the comma that separates it from the last.
    fn emit(&mut self, bytes: String) {
        if self.wrote {
            self.ready.push_back(b",".to_vec());
        }
        self.wrote = true;
        self.ready.push_back(bytes.into_bytes());
    }

    /// Queues the next chunk, or says the body is over.
    fn step(&mut self) -> Step {
        if let Some(head) = self.head.take() {
            self.ready.push_back(head);
            return Step::Ready(true);
        }
        if let Some(rows) = self.rows.as_mut() {
            match rows.next() {
                Some(Err(e)) => return Step::Failed(IoError::other(e)),
                Some(Ok(Stored { item, pictures })) => {
                    if pictures.is_empty() {
                        self.emit(item);
                        return Step::Ready(true);
                    }
                    // The one value resolved while writing: the provider's file per picture.
                    return Step::Resolving((item, pictures));
                }
                None => {
                    self.rows = None;
                    return Step::Ready(true);
                }
            }
        }
        if let Some(tail) = self.tail.take() {
            self.ready.push_back(tail);
            return Step::Ready(true);
        }
        Step::Ready(false)
    }
}

impl Stream for RequestBody {
    type Item = Result<Bytes, IoError>;

    /// One chunk per poll; a row with pictures is held as a future while it resolves.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(chunk) = this.ready.pop_front() {
                if chunk.is_empty() {
                    continue;
                }
                this.counter.fetch_add(chunk.len(), Ordering::Relaxed);
                return Poll::Ready(Some(Ok(Bytes::from(chunk))));
            }
            // A chunk queued by the last await is taken first; only when there is none
            // is the next one asked for.
            let mut future: Next = match this.pending.take() {
                Some(pending) => pending,
                None => match this.step() {
                    Step::Ready(true) => continue,
                    Step::Ready(false) => return Poll::Ready(None),
                    Step::Failed(e) => return Poll::Ready(Some(Err(e))),
                    Step::Resolving((item, pictures)) => {
                        let files = this.files.clone();
                        Box::pin(async move { resolved(files, item, pictures).await })
                    }
                },
            };
            match future.as_mut().poll(cx) {
                Poll::Ready(text) => {
                    this.emit(text);
                    continue;
                }
                Poll::Pending => {
                    this.pending = Some(future);
                    return Poll::Pending;
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // Unknown: the transcript is read as it is sent, and its length is not a
        // promise the network can hold to anyway.
        (0, None)
    }
}

/// What one turn of [`RequestBody::step`] produced.
enum Step {
    /// A chunk was queued, or the body is over.
    Ready(bool),
    /// This row carries pictures, and the files behind them have to be found first.
    Resolving((String, Vec<ImageRef>)),
    Failed(IoError),
}

/// A resolution in progress: owned, because a `Pending` poll cannot lend `self` out.
type Next = std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>;

/// One stored row as a request reads it: the item, and the pictures it named.
pub struct Stored {
    pub item: String,
    pub pictures: Vec<ImageRef>,
}

/// The stored item, with its pictures named as the files the provider holds.
///
/// # Invariant
///
/// The field a picture rides in is the one its kind keeps text in -- `content` for a
/// message, `output` for a result -- and the text is what the reader wrote. A row whose
/// stored item does not parse is sent as stored: a broken row is not this module's to
/// fix, and least of all on the way to a cache.
async fn resolved(mut files: Files, item: String, pictures: Vec<ImageRef>) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(&item) else {
        return item;
    };
    let field = match value.get("type").and_then(Value::as_str) {
        Some("function_call_output") => "output",
        _ => "content",
    };
    let Some(text) = value.get(field).and_then(Value::as_str).map(str::to_string) else {
        return item;
    };
    let mut parts = vec![serde_json::json!({"type": "input_text", "text": text})];
    for picture in &pictures {
        parts.push(files.part(picture).await);
    }
    value[field] = Value::Array(parts);
    value.to_string()
}
