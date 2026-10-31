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
    /// A text row's speaker: the one kind that can end a step from the customer side.
    role: Option<String>,
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
            role,
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
                role,
                step,
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
            role,
            step,
        })
    }
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
                // Not yet: a step's calls are written together, so `push` waits for
                // a row of another step.
            }
            // A new prompt ends a step nobody finished; a step's own reasoning stays.
            _ if entry.kind == "text" && entry.role.as_deref() != Some("assistant") => {
                out = self.flush(placeholder);
                out.push(entry.bytes);
            }
            _ if self.open.is_empty() => {
                out = self.flush(placeholder);
                out.push(entry.bytes);
            }
            _ => self.entries.push(entry),
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
                role: None,
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
                "tool_result" => results.push(entry.bytes),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, value: Value, step: Option<u64>) -> Entry {
        Entry {
            bytes: value.to_string().into_bytes(),
            kind: kind.to_string(),
            call_id: value
                .get("call_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            role: None,
            step,
        }
    }

    fn call(id: &str, step: u64) -> Entry {
        entry(
            "tool_call",
            json!({"type": "function_call", "call_id": id, "name": "bash", "arguments": "{}"}),
            Some(step),
        )
    }

    fn result(id: &str, step: u64) -> Entry {
        entry(
            "tool_result",
            json!({"type": "function_call_output", "call_id": id, "output": "ok"}),
            Some(step),
        )
    }

    fn kinds(chunks: Vec<Vec<u8>>) -> Vec<String> {
        chunks
            .into_iter()
            .map(|bytes| {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                value["type"].as_str().unwrap_or_default().to_string()
            })
            .collect()
    }

    /// A step that asked for two tools writes both calls before either answer.
    #[test]
    fn two_calls_of_one_step_are_written_before_their_results() {
        let mut step = Step::default();
        let mut out = step.push(
            entry(
                "reasoning",
                json!({"type": "reasoning", "id": "rs_1", "summary": []}),
                None,
            ),
            "gone",
        );
        // Stored one call and one result at a time, because each runs as it lands.
        out.extend(step.push(call("a", 0), "gone"));
        out.extend(step.push(result("a", 0), "gone"));
        out.extend(step.push(call("b", 0), "gone"));
        out.extend(step.push(result("b", 0), "gone"));
        // The next step's row is what ends this one, as it is in the transcript.
        out.extend(step.push(call("c", 1), "gone"));
        // What the second step never answered is the walk's end to write.
        out.extend(step.flush("interrupted"));

        assert_eq!(
            kinds(out),
            [
                "reasoning",
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output",
                "function_call",
                "function_call_output"
            ]
        );
    }

    /// A call no result reached is answered when its step ends.
    #[test]
    fn an_open_call_is_answered_when_its_step_ends() {
        let mut step = Step::default();
        let mut out = step.push(call("a", 0), "gone");
        out.extend(step.push(call("b", 0), "gone"));
        out.extend(step.push(result("a", 0), "gone"));
        out.extend(step.push(
            entry("text", json!({"role": "user", "content": "next"}), None),
            "interrupted",
        ));

        let items = kinds(out);
        let stepped: Vec<&str> = items
            .iter()
            .map(String::as_str)
            .filter(|k| !k.is_empty())
            .collect();
        assert_eq!(
            stepped,
            [
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output"
            ],
            "{items:?}"
        );
    }
}
