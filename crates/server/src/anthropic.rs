//! The `anthropic-messages` face. See `docs/notes/server-anthropic.md`.
//!
//! # Invariant
//!
//! Nothing here is stored: a row keeps the shape it was appended in, and this module
//! reads it on the way out.

use std::collections::HashMap;

use serde_json::{Value, json};

use protocol::ResponsePart;

use crate::llm::{LlmEvent, Outbox, SseEvent, Usage};

/// The last word of the URL: what this face appends to a provider's base.
pub const PATH: &str = "/v1/messages";

/// The constant messages that keep the index off the first one: both are required, so
/// every request opens the same way.
#[derive(Debug, Clone, Default)]
pub struct Framing {
    pub opening: String,
    pub separator: String,
}

/// The request up to and including `"messages":[`.
///
/// # Invariant
///
/// Two breakpoints and no more: the system text, which every request of a directory
/// sends byte for byte, and the message before the newest one, which a turn extends.
///
/// The index is a message and the prose is the system prompt: an index inside the system
/// text would change the whole head with the first message.
pub fn head(
    model: &str,
    max_tokens: u32,
    instructions: &str,
    tools: &[Value],
    effort: Option<&str>,
    temperature: Option<f32>,
    indexing: (Framing, Option<&str>),
    card: Option<&str>,
) -> Vec<u8> {
    let (framing, index) = indexing;
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
    });
    if !instructions.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": instructions,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(effort) = effort {
        // The roster's per-model spelling for this face: `output_config.effort`.
        body["output_config"] = json!({ "effort": effort });
    }
    if let Some(temperature) = temperature {
        body["temperature"] = json!(temperature);
    }
    let mut head = body.to_string();
    head.pop();
    head.push_str(",\"messages\":[");
    if !framing.opening.is_empty() {
        head.push_str(
            &json!({"role": "user", "content": [{"type": "text", "text": framing.opening}]})
                .to_string(),
        );
        head.push(',');
    }
    if !framing.separator.is_empty() && index.is_some() {
        head.push_str(
            &json!({"role": "assistant", "content": [{"type": "text", "text": framing.separator}]})
                .to_string(),
        );
        head.push(',');
    }
    if let Some(index) = index.filter(|index| !index.is_empty()) {
        head.push_str(
            &json!({"role": "user", "content": [{"type": "text", "text": index}]}).to_string(),
        );
        head.push(',');
    }
    if let Some(card) = card.filter(|card| !card.is_empty()) {
        head.push_str(
            &json!({"role": "user", "content": [{"type": "text", "text": card}]}).to_string(),
        );
        head.push(',');
    }
    head.into_bytes()
}

/// One tool, in this face's spelling: `input_schema` where Responses says `parameters`.
pub fn tool(name: &str, description: &str, parameters: &Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": parameters,
    })
}

/// Marks the end of the cacheable prefix: what a request may reuse next time.
pub fn breakpoint(message: &mut Value) {
    let Some(blocks) = message["content"].as_array_mut() else {
        return;
    };
    if let Some(last) = blocks.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
}

/// A stored row, as this face sends it.
///
/// # Invariant
///
/// Every row reaches the request: SPEC's "everything must store and send" is not a
/// promise about the rows this face likes. A shape it has no block for -- one another
/// face produced -- is rendered as what it said, and a row that cannot be read at all
/// fails the request instead of vanishing from it.
pub fn item(stored: &Value) -> Result<Value, String> {
    match stored.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            let arguments = stored
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input = serde_json::from_str::<Value>(arguments)
                .unwrap_or_else(|_| json!({ "arguments": arguments }));
            Ok(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": stored.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                    "name": stored.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "input": input,
                }],
            }))
        }
        Some("function_call_output") => Ok(json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": stored.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                "content": stored.get("output").and_then(Value::as_str).unwrap_or_default(),
            }],
        })),
        // This face's own thought, replayed byte for byte: a tool loop is not accepted
        // without the block that led to the call.
        Some("thinking") | Some("redacted_thinking") => Ok(assistant(vec![stored.clone()])),
        // Another face's thought has no block here, so it travels as prose.
        Some("reasoning") => Ok(assistant(vec![json!({
            "type": "text",
            "text": summary_of(stored),
        })])),
        // A provider-side tool item has no block here either: it is what the provider
        // did, and this face is told it in prose.
        Some(other) => Ok(assistant(vec![json!({
            "type": "text",
            "text": format!(
                "used {}{}",
                crate::store::activity_name(other),
                crate::store::activity_detail(stored)
            ),
        })])),
        None => {
            let role = stored
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| "a row with neither a type nor a role".to_string())?;
            let Some(content) = stored.get("content") else {
                return Err(format!("a {role} row with no content"));
            };
            let blocks = blocks_of(content);
            if blocks.is_empty() {
                return Err(format!("a {role} row with no content"));
            }
            Ok(json!({ "role": role, "content": blocks }))
        }
    }
}

/// One assistant message holding these blocks.
fn assistant(blocks: Vec<Value>) -> Value {
    json!({ "role": "assistant", "content": blocks })
}

/// What a reasoning row says, in prose: the summary it carries, else its own text.
fn summary_of(stored: &Value) -> String {
    let parts = stored["summary"].as_array().cloned().unwrap_or_default();
    let text = parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        return text;
    }
    stored["text"]
        .as_str()
        .or_else(|| stored["thinking"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// This face's content blocks for a stored `content` field, which may be text or parts.
fn blocks_of(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("input_text") | Some("output_text") | Some("text") => {
                    Some(json!({"type": "text", "text": part.get("text").and_then(Value::as_str).unwrap_or_default()}))
                }
                Some("image") => Some(part.clone()),
                // A Responses picture names a file this face cannot use: the body
                // resolves pictures into this face's own block first.
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The usage block this face reports, in the shape the rest of the server reads.
///
/// # Invariant
///
/// `input_tokens` here is the whole input, cached part included: this face reports the
/// two halves separately, and a measurement that counted only one would price the
/// request as if the other had never been sent.
pub fn usage(input: u64, cached: u64, output: u64, created: u64) -> Usage {
    Usage {
        input_tokens: Some(input.saturating_add(cached).saturating_add(created)),
        output_tokens: Some(output),
        total_tokens: None,
        input_tokens_details: Some(crate::llm::InputTokens {
            cached_tokens: Some(cached),
            cache_write_tokens: Some(created),
        }),
    }
}

/// One block while it streams: what it is, and what it has said so far.
#[derive(Default)]
pub struct Block {
    id: String,
    name: String,
    text: String,
    json: String,
    signature: String,
    part: Option<ResponsePart>,
}

/// What a stream of this face adds up to, kept between events.
#[derive(Default)]
pub struct Stream {
    blocks: HashMap<u64, Block>,
    input: u64,
    cached: u64,
    created: u64,
    output: u64,
}

impl Stream {
    /// Feeds one event and hands back what the rest of the server should hear.
    pub async fn handle(
        &mut self,
        event: SseEvent,
        out: &mut Outbox<'_>,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<LlmEvent>> {
        let d = &event.data;
        match event.name.as_str() {
            "message_start" => {
                let usage = &d["message"]["usage"];
                self.input = usage["input_tokens"].as_u64().unwrap_or(0);
                self.cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                self.created = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                self.output = usage["output_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let index = d["index"].as_u64().unwrap_or(0);
                let block = &d["content_block"];
                let kind = block["type"].as_str().unwrap_or_default().to_string();
                let mut open = Block {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    ..Block::default()
                };
                let item_id = format!("block-{index}");
                match kind.as_str() {
                    "tool_use" => {
                        open.part = Some(ResponsePart::ToolCall);
                        out.send(LlmEvent::PartStart {
                            part: ResponsePart::ToolCall,
                            item_id: item_id.clone(),
                            call_id: Some(open.id.clone()),
                        })
                        .await?;
                        out.send(LlmEvent::Delta {
                            part: ResponsePart::ToolCall,
                            item_id,
                            text: format!("{}(", open.name),
                        })
                        .await?;
                    }
                    "thinking" | "redacted_thinking" => {
                        open.part = Some(ResponsePart::Reasoning);
                        out.send(LlmEvent::PartStart {
                            part: ResponsePart::Reasoning,
                            item_id,
                            call_id: None,
                        })
                        .await?;
                    }
                    _ => {
                        open.part = Some(ResponsePart::Output);
                        out.send(LlmEvent::PartStart {
                            part: ResponsePart::Output,
                            item_id,
                            call_id: None,
                        })
                        .await?;
                    }
                }
                self.blocks.insert(index, open);
            }
            "content_block_delta" => {
                let index = d["index"].as_u64().unwrap_or(0);
                let delta = &d["delta"];
                let item_id = format!("block-{index}");
                let Some(block) = self.blocks.get_mut(&index) else {
                    return Ok(());
                };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default().to_string();
                        block.text.push_str(&text);
                        out.send(LlmEvent::Delta {
                            part: ResponsePart::Output,
                            item_id,
                            text,
                        })
                        .await?;
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or_default().to_string();
                        block.text.push_str(&text);
                        out.send(LlmEvent::Delta {
                            part: ResponsePart::Reasoning,
                            item_id,
                            text,
                        })
                        .await?;
                    }
                    "signature_delta" => {
                        block
                            .signature
                            .push_str(delta["signature"].as_str().unwrap_or_default());
                    }
                    "input_json_delta" => {
                        block
                            .json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = d["index"].as_u64().unwrap_or(0);
                let Some(block) = self.blocks.remove(&index) else {
                    return Ok(());
                };
                let item_id = format!("block-{index}");
                match block.part {
                    Some(ResponsePart::ToolCall) => {
                        let arguments = if block.json.trim().is_empty() {
                            "{}".to_string()
                        } else {
                            crate::llm::repair_arguments(&block.json)
                        };
                        out.send(LlmEvent::ToolCall {
                            item_id: item_id.clone(),
                            call_id: block.id.clone(),
                            name: block.name.clone(),
                            arguments,
                        })
                        .await?;
                    }
                    Some(ResponsePart::Reasoning) => {
                        let item = json!({
                            "type": "thinking",
                            "thinking": block.text,
                            "signature": block.signature,
                        });
                        out.send(LlmEvent::Reasoning {
                            item_id: item_id.clone(),
                            item,
                            summary: block.text.clone(),
                        })
                        .await?;
                    }
                    Some(ResponsePart::Output) => {
                        out.send(LlmEvent::Output {
                            item_id: item_id.clone(),
                            text: block.text.clone(),
                        })
                        .await?;
                    }
                    _ => {}
                }
                if let Some(part) = block.part {
                    out.send(LlmEvent::PartDone { part, item_id }).await?;
                }
            }
            "message_delta" => {
                let usage = &d["usage"];
                if let Some(cached) = usage["cache_read_input_tokens"].as_u64() {
                    self.cached = cached;
                }
                if let Some(created) = usage["cache_creation_input_tokens"].as_u64() {
                    self.created = created;
                }
                if let Some(input) = usage["input_tokens"].as_u64() {
                    self.input = input;
                }
                if let Some(output) = usage["output_tokens"].as_u64() {
                    self.output = output;
                }
            }
            "message_stop" => {
                out.send(LlmEvent::Completed {
                    usage: Some(usage(self.input, self.cached, self.output, self.created)),
                })
                .await?;
            }
            "error" => {
                let message = d["error"]["message"]
                    .as_str()
                    .or_else(|| d["message"].as_str())
                    .unwrap_or("the provider reported an error");
                out.send(LlmEvent::Failed {
                    message: message.to_string(),
                    retryable: retryable(d),
                })
                .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Whether this event ends a response.
pub fn completes(name: &str) -> bool {
    name == "message_stop"
}

/// Whether this event's text is the provider naming its own refusal.
pub fn retryable(d: &Value) -> bool {
    matches!(
        d["error"]["type"].as_str().unwrap_or_default(),
        "overloaded_error" | "rate_limit_error" | "api_error"
    )
}
