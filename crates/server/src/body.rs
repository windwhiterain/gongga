//! The request body: the stored items, read in order. See `docs/notes/server-body.md`.
//!
//! # Invariant
//!
//! A row is copied as stored, with two exceptions and both stamped: a picture's provider
//! file id is resolved while writing, and the session's card is put in front of the first
//! user message's text. Everything else was decided when the row was appended, so a
//! replay sends the same bytes. See `docs/notes/server-body.md`.

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
    /// What the session's first user message carries in front of its text, until the row
    /// that carries it has been written.
    card: Option<String>,
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
    pub fn new(
        head: Vec<u8>,
        rows: Transcript,
        files: Files,
        counter: Arc<AtomicUsize>,
        card: Option<String>,
    ) -> Self {
        Self {
            head: Some(head),
            rows: Some(rows),
            card,
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
                Some(Ok(stored)) => {
                    let Stored {
                        mut item,
                        pictures,
                        carrier,
                    } = stored;
                    if carrier {
                        // Before the pictures: the card joins the row's own text.
                        if let Some(card) = self.card.take() {
                            match carry(&item, &card) {
                                Some(merged) => item = merged,
                                None => self.emit(
                                    serde_json::json!({
                                        "role": "user",
                                        "content": [{"type": "input_text", "text": card}],
                                    })
                                    .to_string(),
                                ),
                            }
                        }
                    }
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

/// One stored row: its item, its pictures, and whether it is the first user message.
pub struct Stored {
    pub item: String,
    pub pictures: Vec<ImageRef>,
    pub carrier: bool,
}

/// One row with the card in front of its text, as the single message the model reads.
///
/// # Invariant
///
/// Only the text field is rewritten, and only on the row the walk named as the carrier.
/// The card is stamped in the prompt row, so every request that replays the row produces
/// these bytes again. `None` when the row keeps its text somewhere this does not know:
/// the caller then sends the card as its own item, which is the old shape and still
/// whole. See `docs/notes/server-body.md`.
fn carry(item: &str, card: &str) -> Option<String> {
    let mut value: Value = serde_json::from_str(item).ok()?;
    let content = value.get_mut("content")?;
    match content {
        Value::String(text) => *text = format!("{card}\n\n{text}"),
        Value::Array(parts) => {
            let part = parts
                .iter_mut()
                .find(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))?;
            let text = part.get("text").and_then(Value::as_str)?.to_string();
            part["text"] = Value::String(format!("{card}\n\n{text}"));
        }
        _ => return None,
    }
    Some(value.to_string())
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
