//! The request body, written as it is read. See `docs/notes/server-body.md`.

use std::collections::{HashMap, HashSet, VecDeque};
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

/// One item, as it will be written, with the two fields the ordering needs.
struct Entry {
    bytes: Vec<u8>,
    kind: String,
    call_id: Option<String>,
}

impl Entry {
    /// `None` when the row cannot be sent: its payload file is gone.
    fn of(row: Row) -> Option<Self> {
        let Row {
            item,
            kind,
            call_id,
            images,
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
            });
        }
        let bytes = if images.is_empty() {
            item.into_bytes()
        } else {
            // A result with pictures is the one item whose stored text becomes a
            // content array; every other row goes out untouched.
            match serde_json::from_str::<Value>(&item) {
                Ok(mut value) => {
                    let text = value
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let mut parts = vec![json!({"type": "input_text", "text": text})];
                    parts.extend(images.iter().map(content_part));
                    value["output"] = Value::Array(parts);
                    value.to_string().into_bytes()
                }
                Err(_) => item.into_bytes(),
            }
        };
        Some(Self {
            bytes,
            kind,
            call_id,
        })
    }
}

/// The rows of one model step, held until its results have all landed.
///
/// # Invariant
///
/// A provider reads a `function_call` as answered by the item directly after it, while
/// the rows are stored in completion order, so a step's items cannot be written until
/// the step is over. A step is over when its last open call is answered -- the model
/// cannot ask for the next one before it hears about these -- which bounds what this
/// holds by one step rather than by the transcript.
#[derive(Default)]
struct Step {
    entries: Vec<Entry>,
    /// The calls written so far whose result has not arrived.
    open: Vec<String>,
}

impl Step {
    /// Takes one row and returns what the request may now send.
    fn push(&mut self, entry: Entry) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        match entry.kind.as_str() {
            "tool_call" => {
                if let Some(id) = &entry.call_id {
                    self.open.push(id.clone());
                }
                self.entries.push(entry);
            }
            "tool_result" => {
                // A call and its result are the same step's: a result whose call is
                // not waiting here is an orphan.
                let waiting = entry
                    .call_id
                    .as_ref()
                    .is_some_and(|id| self.open.contains(id));
                if !waiting {
                    tracing::debug!(
                        call_id = entry.call_id.as_deref().unwrap_or("?"),
                        "a tool result lost its call: not sent"
                    );
                    return out;
                }
                let id = entry.call_id.clone().unwrap_or_default();
                self.open.retain(|open| open != &id);
                self.entries.push(entry);
                if self.open.is_empty() {
                    out = self.flush();
                }
            }
            // Not a tool: nothing left to pair, so the buffered step goes first.
            _ => {
                out = self.flush();
                out.push(entry.bytes);
            }
        }
        out
    }

    /// Ends the transcript: a call with no result gets one saying it was interrupted.
    fn close(&mut self, placeholder: &str) -> Vec<Vec<u8>> {
        for id in self.open.clone() {
            let answer = json!({
                "type": "function_call_output",
                "call_id": id,
                "output": placeholder,
            });
            self.entries.push(Entry {
                bytes: answer.to_string().into_bytes(),
                kind: "tool_result".to_string(),
                call_id: Some(id),
            });
        }
        self.flush()
    }

    /// Writes the buffered step with each call followed by its own result.
    fn flush(&mut self) -> Vec<Vec<u8>> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        // Taken by slot rather than borrowed: a call reaches past the item it stands
        // on to pull its own result forward.
        let answers: HashMap<String, usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind == "tool_result")
            .filter_map(|(index, entry)| Some((entry.call_id.clone()?, index)))
            .collect();
        let mut paired: HashSet<String> = HashSet::new();
        let mut slots: Vec<Option<Entry>> = self.entries.drain(..).map(Some).collect();
        let mut out = Vec::with_capacity(slots.len());
        for index in 0..slots.len() {
            let Some(entry) = slots[index].take() else {
                continue;
            };
            let Entry {
                bytes,
                kind,
                call_id,
            } = entry;
            if kind == "tool_result" {
                // Already written beside the call it answers.
                let paired = call_id.as_ref().is_some_and(|id| paired.contains(id));
                if !paired {
                    out.push(bytes);
                }
                continue;
            }
            out.push(bytes);
            if kind == "tool_call" {
                if let Some(id) = &call_id {
                    if let Some(&at) = answers.get(id) {
                        paired.insert(id.clone());
                        if let Some(answer) = slots[at].take() {
                            out.push(answer.bytes);
                        }
                    }
                }
            }
        }
        self.open.clear();
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
        let next = match self.rows.as_mut() {
            None => None,
            Some(rows) => rows.next().map(|row| row.map(Entry::of)),
        };
        match next {
            Some(Err(e)) => return Err(IoError::other(e)),
            Some(Ok(Some(entry))) => {
                for bytes in self.step.push(entry) {
                    self.emit(bytes);
                }
            }
            // Read and set aside: the loop asks for the row after it.
            Some(Ok(None)) | None => {}
        }
        if self.rows.is_some() {
            return Ok(true);
        }
        if self.rows.take().is_some() {
            for bytes in self.step.close(&self.placeholder) {
                self.emit(bytes);
            }
            return Ok(true);
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
