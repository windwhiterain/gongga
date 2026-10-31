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

/// One item, as it will be written, with the fields the ordering needs.
struct Entry {
    bytes: Vec<u8>,
    kind: String,
    call_id: Option<String>,
    /// A text row's speaker: the one kind that can end a step from the customer side.
    role: Option<String>,
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
}

impl Step {
    /// Takes one row and returns what the request may now send.
    fn push(&mut self, entry: Entry, placeholder: &str) -> Vec<Vec<u8>> {
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
                    out = self.flush(placeholder);
                }
            }
            // Buffered, not written where it arrived: `flush` puts it before each call.
            "reasoning" => self.entries.push(entry),
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

    /// The step's reasoning without its id, for the calls after the first.
    fn reasoning_for_each_call(&self) -> Option<Vec<u8>> {
        let calls = self
            .entries
            .iter()
            .filter(|entry| entry.kind == "tool_call")
            .count();
        let reasoning = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry.kind == "reasoning")?;
        if calls < 2 {
            return None;
        }
        let mut value: Value = serde_json::from_slice(&reasoning.bytes).ok()?;
        // A copy, not the same item twice: an id names one item.
        value.as_object_mut()?.remove("id");
        Some(value.to_string().into_bytes())
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
            });
        }
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
        let repeated = self.reasoning_for_each_call();
        let mut paired: HashSet<String> = HashSet::new();
        let mut slots: Vec<Option<Entry>> = self.entries.drain(..).map(Some).collect();
        let mut out = Vec::with_capacity(slots.len());
        let mut written_a_call = false;
        for index in 0..slots.len() {
            let Some(entry) = slots[index].take() else {
                continue;
            };
            let Entry {
                bytes,
                kind,
                call_id,
                ..
            } = entry;
            if kind == "tool_result" {
                // Already written beside the call it answers.
                let paired = call_id.as_ref().is_some_and(|id| paired.contains(id));
                if !paired {
                    out.push(bytes);
                }
                continue;
            }
            if kind == "tool_call" {
                // A second call with no reasoning before it is refused: repeat it.
                if written_a_call {
                    if let Some(repeated) = &repeated {
                        out.push(repeated.clone());
                    }
                }
                written_a_call = true;
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

    fn entry(kind: &str, value: Value) -> Entry {
        Entry {
            bytes: value.to_string().into_bytes(),
            kind: kind.to_string(),
            call_id: value
                .get("call_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            role: None,
        }
    }

    fn written(chunks: Vec<Vec<u8>>) -> Vec<String> {
        chunks
            .into_iter()
            .map(|b| String::from_utf8(b).unwrap())
            .collect()
    }

    /// A second call with no reasoning before it is refused by DeepSeek.
    #[test]
    fn a_step_with_two_calls_repeats_its_reasoning_without_an_id() {
        let reasoning = entry(
            "reasoning",
            json!({"type": "reasoning", "id": "rs_1", "summary": [],
                   "encrypted_content": "blob",
                   "content": [{"type": "reasoning_text", "text": "think"}]}),
        );
        let mut step = Step::default();
        assert!(step.push(reasoning, "gone").is_empty());
        // Both calls land before either result does: one step asked for two tools.
        let mut out = Vec::new();
        for id in ["call_a", "call_b"] {
            out.extend(step.push(
                entry(
                    "tool_call",
                    json!({"type": "function_call", "call_id": id, "name": "bash",
                           "arguments": "{}"}),
                ),
                "gone",
            ));
        }
        // And the answers arrive in their own order, not the asking order.
        for id in ["call_b", "call_a"] {
            out.extend(step.push(
                entry(
                    "tool_result",
                    json!({"type": "function_call_output", "call_id": id, "output": "ok"}),
                ),
                "gone",
            ));
        }
        let items = written(out);
        assert_eq!(items.len(), 6, "{}", items.join("\n"));
        assert!(items[0].contains("\"id\":\"rs_1\""), "{}", items[0]);
        assert!(items[3].contains("reasoning"), "{}", items[3]);
        assert!(
            !items[3].contains("rs_1"),
            "a second item cannot borrow the first one's id: {}",
            items[3]
        );
        assert!(items[3].contains("think"), "{}", items[3]);
    }

    /// One call is answered by the reasoning already before it; nothing is repeated.
    #[test]
    fn a_step_with_one_call_sends_its_reasoning_once() {
        let mut step = Step::default();
        let mut out = step.push(
            entry(
                "reasoning",
                json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "blob"}),
            ),
            "gone",
        );
        out.extend(step.push(
            entry(
                "tool_call",
                json!({"type": "function_call", "call_id": "a", "name": "bash", "arguments": "{}"}),
            ),
            "gone",
        ));
        out.extend(step.push(
            entry(
                "tool_result",
                json!({"type": "function_call_output", "call_id": "a", "output": "ok"}),
            ),
            "gone",
        ));
        assert_eq!(written(out).len(), 3);
    }
}
