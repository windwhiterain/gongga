//! The request body, written as it is read. See `docs/notes/server-body.md`.

use std::collections::VecDeque;
use std::io::Error as IoError;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{Value, json};

use crate::image::content_part;
use crate::transcript::{Row, Transcript};

/// One item, as it will be written, with the fields the ordering needs.
struct Entry {
    bytes: Vec<u8>,
    kind: String,
    call_id: Option<String>,
    /// The model step that wrote a call or a result, when the row says.
    step: Option<u64>,
}

impl Entry {
    /// `None` when the row cannot be sent: its payload file is gone.
    fn of(row: Row) -> Option<Self> {
        let Row {
            item,
            kind,
            call_id,
            images,
            step,
        } = row;
        if kind == "builtin_call" {
            let Ok(value) = serde_json::from_str::<Value>(&item) else {
                return None;
            };
            let replayed = crate::llm::sendable_activity(&value, true, crate::blobs::dir())?;
            return Some(Self {
                bytes: replayed.to_string().into_bytes(),
                kind,
                call_id,
                step,
            });
        }
        let bytes = with_pictures(item, &kind, &images).into_bytes();
        Some(Self {
            bytes,
            kind,
            call_id,
            step,
        })
    }
}

/// The one item whose text becomes a content array when the row carries pictures.
///
/// # Invariant
///
/// A picture rides in the field its kind keeps text in -- `content` for a message,
/// `output` for a result -- and every other row goes out as stored.
fn with_pictures(item: String, kind: &str, images: &[crate::image::ImageRef]) -> String {
    if images.is_empty() {
        return item;
    }
    let field = match kind {
        "tool_result" => "output",
        "text" => "content",
        _ => return item,
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&item) else {
        return item;
    };
    let Some(text) = value.get(field).and_then(|v| v.as_str()) else {
        return item;
    };
    let mut parts = vec![json!({"type": "input_text", "text": text})];
    parts.extend(images.iter().map(content_part));
    value[field] = Value::Array(parts);
    value.to_string()
}

/// The rows of one model step, held until its results have all landed.
///
/// # Invariant
///
/// A provider wants a `function_call` answered by the item after it, while results
/// land in completion order, so a step is held until its last call is answered or a
/// new prompt ends it -- unfinished calls then answered as interrupted. That bounds
/// what is held by one step, not by the transcript.
#[derive(Default)]
struct Step {
    entries: Vec<Entry>,
    /// The calls written so far whose result has not arrived.
    open: Vec<String>,
    /// Every call id this request has already answered, invented answers included:
    /// one output per call is what a provider accepts.
    answered: Vec<String>,
    /// The model step being buffered, once a row of it has been seen.
    step: Option<u64>,
}

impl Step {
    /// Takes one row and returns what the request may now send.
    fn push(&mut self, entry: Entry, placeholder: &str) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        // Another step's row ends the buffered one; a row that names none is its own.
        if let (Some(buffered), Some(incoming)) = (self.step, entry.step) {
            if buffered != incoming {
                out = self.flush(placeholder);
                self.step = None;
            }
        }
        if self.step.is_none() {
            self.step = entry.step;
        }
        match entry.kind.as_str() {
            "tool_call" => {
                if let Some(id) = &entry.call_id {
                    self.open.push(id.clone());
                }
                self.entries.push(entry);
            }
            "tool_result" => {
                let id = entry.call_id.clone().unwrap_or_default();
                // The waiting case: this step's own answer, written when the step is.
                if self.open.contains(&id) {
                    self.open.retain(|open| open != &id);
                    self.answered.push(id);
                    self.entries.push(entry);
                    return out;
                }
                // Pairing is by position: a result whose call is not in this step
                // cannot be placed beside it, and alone it is the item a provider
                // rejects. The row stays in the store; the request leaves it out.
                tracing::warn!(
                    call_id = entry.call_id.as_deref().unwrap_or("?"),
                    "a tool result whose call is not in its step: not sent"
                );
            }
            // A row that is not half of this step's pair ends the step. A provider
            // reads a call as answered by the item after it, so the buffer is written
            // as one block -- reasoning, calls, then one output each -- and nothing may
            // land inside that block: an assistant message between them is what the
            // provider reports as "no tool output found". Ending the step here is also
            // where an interrupted step's unanswered calls get their placeholder, so
            // the pair is complete by the time it is written.
            _ => {
                out = self.flush(placeholder);
                out.push(entry.bytes);
            }
        }
        out
    }

    /// Writes the buffered step, answering any call no result reached.
    fn flush(&mut self, placeholder: &str) -> Vec<Vec<u8>> {
        for id in self.open.drain(..) {
            self.entries.push(Entry {
                bytes: json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": placeholder,
                })
                .to_string()
                .into_bytes(),
                kind: "tool_result".to_string(),
                call_id: Some(id),
                step: None,
            });
        }
        if self.entries.is_empty() {
            return Vec::new();
        }
        // Reasoning, calls, results: the provider's own order (measurements in the
        // reference).
        let mut calls = Vec::new();
        let mut results = Vec::new();
        let mut reasoned = Vec::new();
        for entry in self.entries.drain(..) {
            match entry.kind.as_str() {
                "tool_result" => {
                    if let Some(id) = &entry.call_id {
                        self.answered.push(id.clone());
                    }
                    results.push(entry.bytes);
                }
                "reasoning" => reasoned.push(entry.bytes),
                _ => calls.push(entry.bytes),
            }
        }
        let mut out = Vec::with_capacity(reasoned.len() + calls.len() + results.len());
        out.append(&mut reasoned);
        out.append(&mut calls);
        out.append(&mut results);
        out
    }
}

/// A model request's body, produced as the network asks for it.
pub struct RequestBody {
    /// The object up to and including `"input":[`.
    head: Option<Vec<u8>>,
    rows: Option<Transcript>,
    step: Step,
    placeholder: String,
    /// Encoded items waiting to be read.
    ready: VecDeque<Vec<u8>>,
    tail: Option<Vec<u8>>,
    /// An item has been written, so the next needs a separator before it.
    wrote: bool,
    /// How many bytes the network has taken, counted as they go.
    counter: Arc<AtomicUsize>,
}

impl RequestBody {
    pub fn new(
        head: Vec<u8>,
        rows: Transcript,
        placeholder: String,
        counter: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            head: Some(head),
            rows: Some(rows),
            step: Step::default(),
            placeholder,
            ready: VecDeque::new(),
            tail: Some(b"]}".to_vec()),
            wrote: false,
            counter,
        }
    }

    /// Queues one item, with the comma that separates it from the last.
    fn emit(&mut self, bytes: Vec<u8>) {
        if self.wrote {
            self.ready.push_back(b",".to_vec());
        }
        self.wrote = true;
        self.ready.push_back(bytes);
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
                Some(Ok(row)) => {
                    if let Some(entry) = Entry::of(row) {
                        for bytes in self.step.push(entry, &self.placeholder) {
                            self.emit(bytes);
                        }
                    }
                    return Ok(true);
                }
                // The walk is over: whatever is still buffered is the last step.
                None => {
                    self.rows = None;
                    for bytes in self.step.flush(&self.placeholder) {
                        self.emit(bytes);
                    }
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
