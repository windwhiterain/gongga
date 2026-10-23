//! Streaming client for the OpenAI Responses API.
//!
//! See `docs/notes/server-llm.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use protocol::ResponsePart;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::store::HistoryItem;

#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    instructions: String,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    store: bool,
    /// Silence between chunks that counts as a broken stream.
    idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response.
    max_retries: u32,
    /// First retry delay; doubled per attempt up to [`MAX_RETRY_DELAY`].
    retry_base: Duration,
}

impl LlmClient {
    pub fn new(cfg: &crate::config::LlmConfig) -> anyhow::Result<Self> {
        if cfg.api_key.is_empty() {
            anyhow::bail!(
                "no API key: set {} (or GONGGA_API_KEY) before starting the server",
                cfg.api_key_env
            );
        }
        // One client-wide timeout covers every request, compression included.
        let http = reqwest::Client::builder()
            .timeout(cfg.request_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            instructions: cfg.instructions.clone(),
            temperature: cfg.temperature,
            max_output_tokens: cfg.max_output_tokens,
            store: cfg.store,
            idle_timeout: cfg.idle_timeout,
            max_retries: cfg.max_retries,
            retry_base: cfg.retry_base,
        })
    }

    /// The model this client asks for; logged at the start of every turn.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Streams one model request; `summary`, when present, is sent first.
    ///
    /// # Invariant
    ///
    /// A failed, timed-out, or empty attempt is retried with a doubling delay:
    /// the caller always sees one stream, and each retry is announced as
    /// [`LlmEvent::Retrying`]. See `docs/notes/server-llm.md`.
    pub fn stream(
        &self,
        summary: Option<&str>,
        history: &[HistoryItem],
        tools: &[ToolDefinition],
        effort: Option<&str>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<LlmEvent> {
        let (tx, rx) = mpsc::channel(256);
        let body = self.build_body(summary, history, tools, effort);
        let url = format!("{}/responses", self.base_url);
        let api_key = self.api_key.clone();
        let http = self.http.clone();
        let idle_timeout = self.idle_timeout;
        let max_retries = self.max_retries;
        let retry_base = self.retry_base;

        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            loop {
                let mut once =
                    stream_once(&http, &url, &api_key, &body, idle_timeout, cancel.clone()).await;
                let mut produced = false;
                let mut completed = false;
                let mut cancelled = false;
                let mut failure: Option<String> = None;

                while let Some(event) = once.recv().await {
                    match &event {
                        // Content of any kind: this attempt said something.
                        LlmEvent::Delta { .. }
                        | LlmEvent::Output { .. }
                        | LlmEvent::Reasoning { .. }
                        | LlmEvent::ToolCall { .. } => produced = true,
                        LlmEvent::Completed { .. } => completed = true,
                        LlmEvent::Cancelled => cancelled = true,
                        LlmEvent::Failed { message } => {
                            // Held back: if a retry follows, the first
                            // attempt's error is not the answer.
                            failure = Some(message.clone());
                            continue;
                        }
                        _ => {}
                    }
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
                if cancelled {
                    let _ = tx.send(LlmEvent::Cancelled).await;
                    return;
                }
                if completed && produced {
                    return;
                }
                // Empty completion or cut-short stream: both are retryable.
                if !retryable(attempt, max_retries, produced) || cancel.is_cancelled() {
                    if let Some(message) = failure {
                        let _ = tx.send(LlmEvent::Failed { message }).await;
                    }
                    return;
                }

                attempt += 1;
                let delay = backoff(retry_base, attempt);
                tracing::warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    empty = completed && !produced,
                    "model request gave nothing usable; retrying"
                );
                let _ = tx
                    .send(LlmEvent::Retrying {
                        attempt,
                        max: max_retries,
                        delay,
                    })
                    .await;
                // Waiting out the delay, but not past a cancel: an
                // `interrupt_session` has to stay responsive while a retry is
                // pending.
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = tx.send(LlmEvent::Cancelled).await;
                        return;
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        });

        rx
    }

    fn build_body(
        &self,
        summary: Option<&str>,
        history: &[HistoryItem],
        tools: &[ToolDefinition],
        effort: Option<&str>,
    ) -> Value {
        let mut input: Vec<Value> = Vec::with_capacity(history.len() + 1);
        if let Some(s) = summary {
            input.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": format!(
                    "Earlier in this session (compressed to keep the context manageable):\n\n{s}"
                )}],
            }));
        }
        input.extend(history.iter().filter_map(history_item_to_input));

        let mut body = json!({
            "model": self.model,
            "input": input,
            "stream": true,
            "store": self.store,
        });
        if !self.instructions.is_empty() {
            body["instructions"] = json!(self.instructions);
        }
        if !tools.is_empty() {
            body["tools"] = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect::<Vec<_>>()
                .into();
        }
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = self.max_output_tokens {
            body["max_output_tokens"] = json!(m);
        }
        if let Some(effort) = effort {
            // The roster's per-model name for this field is
            // `reasoning_field.responses`; `effort` is all this server speaks.
            body["reasoning"] = json!({ "effort": effort });
        }
        if !self.store {
            // Without server-side storage, reasoning can only be replayed from
            // its encrypted blob.
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        body
    }

    /// Asks for a plain-text answer: no tools, no streaming, compression only.
    ///
    /// # Invariant
    ///
    /// `max_retries == 0` is no ceiling, so the turn's `cancel` is the only stop:
    /// compression never outlives the turn that asked for it.
    ///
    /// See `docs/notes/server-llm.md`.
    pub async fn summarise(
        &self,
        prompt: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<String> {
        let mut attempt: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled while summarising");
            }
            match self.summarise_once(prompt).await {
                Ok(text) if !text.trim().is_empty() => return Ok(text),
                Ok(empty) if self.max_retries != 0 && attempt >= self.max_retries => {
                    return Ok(empty);
                }
                Ok(_) => tracing::warn!(attempt, "compression came back empty; retrying"),
                Err(e) if self.max_retries != 0 && attempt >= self.max_retries => return Err(e),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "compression request failed; retrying")
                }
            }
            attempt += 1;
            tokio::time::sleep(backoff(self.retry_base, attempt)).await;
        }
    }

    async fn summarise_once(&self, prompt: &str) -> anyhow::Result<String> {
        let started = Instant::now();
        let body = json!({
            "model": self.model,
            "input": [{"role": "user", "content": prompt}],
            "stream": false,
            "store": false,
            "max_output_tokens": SUMMARY_MAX_TOKENS,
        });
        let response = self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        tracing::info!(%status, chars = text.len(), elapsed = ?started.elapsed(), "compression request done");
        if !status.is_success() {
            anyhow::bail!("HTTP {status}: {}", summarise_error(&text));
        }
        Ok(extract_text(&text))
    }
}

/// Budget for a compression request: its summary replaces kilobytes of history.
const SUMMARY_MAX_TOKENS: u32 = 2_000;

/// Pulls assistant text out of a non-streaming payload, falling back to the body.
fn extract_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(2_000).collect();
    };
    if let Some(text) = value["output_text"].as_str() {
        return text.to_string();
    }
    let mut out = String::new();
    let items = value["output"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    for item in items {
        if item["type"] != "message" {
            continue;
        }
        let parts = item["content"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        for part in parts {
            if let Some(t) = part["text"].as_str() {
                out.push_str(t);
            }
        }
    }
    if out.is_empty() {
        body.chars().take(2_000).collect()
    } else {
        out
    }
}

/// A tool exposed to the model, in Responses-API shape.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// What the model produced, one chunk at a time.
///
/// # Invariant
///
/// Every variant carries `item_id`, which is the only thing that keeps
/// interleaved output items apart. See `docs/notes/server-llm.md`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum LlmEvent {
    /// Which part of the response the following deltas belong to.
    PartStart {
        part: ResponsePart,
        item_id: String,
    },
    Delta {
        part: ResponsePart,
        item_id: String,
        text: String,
    },
    PartDone {
        part: ResponsePart,
        item_id: String,
    },
    /// A complete function call, emitted once its arguments have finished.
    ToolCall {
        item_id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    /// A reasoning item to persist, for replay on the next request.
    Reasoning {
        item_id: String,
        summary: String,
        encrypted: Option<String>,
    },
    /// Assistant text, emitted once at the end of the message item.
    Output {
        item_id: String,
        text: String,
    },
    Completed {
        usage: Option<Usage>,
    },
    Failed {
        message: String,
    },
    Cancelled,
    /// The previous attempt gave nothing usable; another follows after `delay`.
    Retrying {
        attempt: u32,
        max: u32,
        delay: Duration,
    },
}

/// Cap on one retry delay. Doubling is meant to wait out a provider's bad
/// minute, not to add a silent one of our own.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Delay before attempt `attempt` (1-based): the base doubled each time, capped
/// at [`MAX_RETRY_DELAY`].
fn backoff(base: Duration, attempt: u32) -> Duration {
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    base.saturating_mul(factor).min(MAX_RETRY_DELAY)
}

/// Whether another attempt is worth making; only `produced` rules one out.
///
/// # Invariant
///
/// `max_retries == 0` is no ceiling, and a produced answer is never retried:
/// asking again would stand a second, different answer behind the first.
///
/// See `docs/notes/server-llm.md`.
fn retryable(attempt: u32, max_retries: u32, produced: bool) -> bool {
    (max_retries == 0 || attempt < max_retries) && !produced
}

/// One attempt at a streaming request, handed back as a channel.
///
/// See `docs/notes/server-llm.md`.
async fn stream_once(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &Value,
    idle_timeout: Duration,
    cancel: CancellationToken,
) -> mpsc::Receiver<LlmEvent> {
    let (tx, rx) = mpsc::channel(256);
    let body = body.clone();
    let url = url.to_string();
    let api_key = api_key.to_string();
    let http = http.clone();

    tokio::spawn(async move {
        let chars = body.to_string().len();
        let started = Instant::now();
        tracing::info!(%url, chars, "model request: sending");
        let request = http
            .post(&url)
            .header("Accept", "text/event-stream")
            .bearer_auth(api_key)
            .json(&body);

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, elapsed = ?started.elapsed(), "model request: send failed");
                let _ = tx
                    .send(LlmEvent::Failed {
                        message: format!("{e}"),
                    })
                    .await;
                return;
            }
        };
        tracing::info!(
            status = %response.status(),
            elapsed = ?started.elapsed(),
            "model request: headers"
        );
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(%status, body = %summarise_error(&text), "model request: rejected");
            let _ = tx
                .send(LlmEvent::Failed {
                    message: format!("HTTP {status}: {}", summarise_error(&text)),
                })
                .await;
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        // item_id -> output item metadata, so deltas can be attributed.
        let mut items: HashMap<String, ItemMeta> = HashMap::new();
        // Idle silence is failure: no error, no EOF, nothing else wakes the turn.
        let idle = tokio::time::sleep(idle_timeout);
        tokio::pin!(idle);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = tx.send(LlmEvent::Cancelled).await;
                    return;
                }
                _ = &mut idle => {
                    tracing::warn!(idle = ?idle_timeout, elapsed = ?started.elapsed(), "model request: no data");
                    let _ = tx.send(LlmEvent::Failed {
                        message: format!("no response data for {idle_timeout:?}"),
                    }).await;
                    return;
                }
                next = stream.next() => {
                    let Some(chunk) = next else { break };
                    idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                    match chunk {
                        Ok(bytes) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(block) = next_sse_block(&mut buf) {
                                if let Some(event) = parse_sse_block(&block) {
                                    if let Err(e) = handle(event, &mut items, &tx).await {
                                        tracing::debug!(error = %e, "llm stream closed early");
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(LlmEvent::Failed { message: format!("{e}") }).await;
                            return;
                        }
                    }
                }
            }
        }
        // Flush the tail: a stream may end without its blank line, still holding
        // a final event.
        let tail = buf.trim_end_matches('\r').trim();
        if !tail.is_empty() {
            if let Some(event) = parse_sse_block(tail) {
                let _ = handle(event, &mut items, &tx).await;
            }
        }
        tracing::info!(elapsed = ?started.elapsed(), "model request: stream ended");
    });

    rx
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct ItemMeta {
    kind: ItemKind,
    name: String,
    call_id: String,
    /// Text accumulated from deltas, the only place a readable summary exists.
    text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ItemKind {
    #[default]
    Unknown,
    Reasoning,
    Message,
    FunctionCall,
}

struct SseEvent {
    name: String,
    data: Value,
}

/// Takes one complete server-sent-event block off the front of `buf`.
///
/// # Invariant
///
/// Any blank-line separator ends a block -- `\n\n`, `\r\n\r\n`, `\n\r\n`; the
/// returned block never carries its trailing `\r`. Recognising only one of them
/// reads as silence. See `docs/notes/server-llm.md`.
fn next_sse_block(buf: &mut String) -> Option<String> {
    let (at, len) = ["\n\n", "\r\n\r\n", "\n\r\n"]
        .iter()
        .filter_map(|sep| buf.find(sep).map(|at| (at, sep.len())))
        .min_by_key(|(at, _)| *at)?;
    let block = buf[..at].trim_end_matches('\r').to_string();
    buf.drain(..at + len);
    Some(block)
}

fn parse_sse_block(block: &str) -> Option<SseEvent> {
    let mut name = String::new();
    let mut data = String::new();
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str(&data).unwrap_or(Value::Null);
    Some(SseEvent { name, data: parsed })
}

async fn handle(
    event: SseEvent,
    items: &mut HashMap<String, ItemMeta>,
    tx: &mpsc::Sender<LlmEvent>,
) -> Result<(), mpsc::error::SendError<LlmEvent>> {
    let d = &event.data;
    match event.name.as_str() {
        "response.output_item.added" => {
            let item = &d["item"];
            let id = item["id"].as_str().unwrap_or_default().to_string();
            let kind = match item["type"].as_str().unwrap_or_default() {
                "reasoning" => ItemKind::Reasoning,
                "message" => ItemKind::Message,
                "function_call" => ItemKind::FunctionCall,
                _ => ItemKind::Unknown,
            };
            items.insert(
                id.clone(),
                ItemMeta {
                    kind,
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                    call_id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    text: String::new(),
                },
            );
            match kind {
                ItemKind::Reasoning => {
                    tx.send(LlmEvent::PartStart {
                        part: ResponsePart::Reasoning,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::Message => {
                    tx.send(LlmEvent::PartStart {
                        part: ResponsePart::Output,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::FunctionCall => {
                    tx.send(LlmEvent::PartStart {
                        part: ResponsePart::ToolCall,
                        item_id: id.clone(),
                    })
                    .await?;
                    let mut shown = format!("{}(", item["name"].as_str().unwrap_or("?"));
                    if let Some(args) = item["arguments"].as_str() {
                        shown.push_str(args);
                    }
                    shown.push(')');
                    tx.send(LlmEvent::Delta {
                        part: ResponsePart::ToolCall,
                        item_id: id,
                        text: shown,
                    })
                    .await?;
                }
                ItemKind::Unknown => {}
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let delta = d["delta"].as_str().unwrap_or_default().to_string();
            // Deltas can precede `output_item.added`; `done` re-derives the kind.
            items
                .entry(item_id.clone())
                .or_default()
                .text
                .push_str(&delta);
            tx.send(LlmEvent::Delta {
                part: ResponsePart::Reasoning,
                item_id,
                text: delta,
            })
            .await?;
        }
        "response.output_text.delta" => {
            tx.send(LlmEvent::Delta {
                part: ResponsePart::Output,
                item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                text: d["delta"].as_str().unwrap_or_default().to_string(),
            })
            .await?;
        }
        "response.function_call_arguments.delta" => {
            tx.send(LlmEvent::Delta {
                part: ResponsePart::ToolCall,
                item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                text: d["delta"].as_str().unwrap_or_default().to_string(),
            })
            .await?;
        }
        "response.output_item.done" => {
            let item = &d["item"];
            let id = item["id"].as_str().unwrap_or_default().to_string();
            let meta = items.remove(&id).unwrap_or_default();
            let kind = if meta.kind == ItemKind::Unknown {
                match item["type"].as_str().unwrap_or_default() {
                    "reasoning" => ItemKind::Reasoning,
                    "message" => ItemKind::Message,
                    "function_call" => ItemKind::FunctionCall,
                    _ => ItemKind::Unknown,
                }
            } else {
                meta.kind
            };
            match kind {
                ItemKind::Reasoning => {
                    // The provider's summary wins when it sends one; the
                    // accumulated deltas are the fallback when it does not.
                    let mut summary = item["summary"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .or_else(|| item["summary"].as_str().map(str::to_string))
                        .unwrap_or_default();
                    if summary.is_empty() {
                        summary = meta.text.clone();
                    }
                    tx.send(LlmEvent::Reasoning {
                        item_id: id.clone(),
                        summary,
                        encrypted: item["encrypted_content"].as_str().map(str::to_string),
                    })
                    .await?;
                    tx.send(LlmEvent::PartDone {
                        part: ResponsePart::Reasoning,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::Message => {
                    let text = item["content"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    tx.send(LlmEvent::Output {
                        item_id: id.clone(),
                        text,
                    })
                    .await?;
                    tx.send(LlmEvent::PartDone {
                        part: ResponsePart::Output,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::FunctionCall => {
                    tx.send(LlmEvent::ToolCall {
                        item_id: id.clone(),
                        call_id: meta.call_id,
                        name: meta.name,
                        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
                    })
                    .await?;
                    tx.send(LlmEvent::PartDone {
                        part: ResponsePart::ToolCall,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::Unknown => {}
            }
        }
        "response.completed" => {
            let usage = serde_json::from_value(d["response"]["usage"].clone()).ok();
            tx.send(LlmEvent::Completed { usage }).await?;
        }
        "response.failed" | "error" => {
            let message = d["error"]["message"]
                .as_str()
                .or_else(|| d["message"].as_str())
                .unwrap_or("unknown error")
                .to_string();
            tx.send(LlmEvent::Failed { message }).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Renders a stored transcript entry as a Responses-API input item.
fn history_item_to_input(item: &HistoryItem) -> Option<Value> {
    Some(match item {
        HistoryItem::Text { role, text } => match role.as_str() {
            "assistant" => json!({
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}],
            }),
            _ => json!({"role": role, "content": text}),
        },
        HistoryItem::ToolCall {
            call_id,
            name,
            arguments,
        } => {
            json!({"type":"function_call","call_id":call_id,"name":name,"arguments":arguments})
        }
        HistoryItem::ToolResult { call_id, output } => {
            json!({"type":"function_call_output","call_id":call_id,"output":output})
        }
        // The summary is for the UI; the model gets only the encrypted blob.
        HistoryItem::Reasoning {
            item_id,
            encrypted,
            summary: _summary,
        } => {
            // Replay needs the blob; without it drop the item, never send a shape
            // the API would reject.
            let Some(enc) = encrypted else {
                return None;
            };
            let mut item = json!({"type":"reasoning","summary":[]});
            if let Some(id) = item_id {
                item["id"] = json!(id);
            }
            item["encrypted_content"] = json!(enc);
            item
        }
    })
}

/// Pulls a readable message out of an error body, tolerating plain text.
fn summarise_error(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) => v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                let t: String = body.chars().take(400).collect();
                t
            }),
        Err(_) => body.chars().take(400).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every separator ends a block and `\r` never reaches the parser; a split
    /// block waits for its separator.
    #[test]
    fn sse_blocks_end_at_any_blank_line() {
        for wire in [
            "data: a\n\ndata: b\n\n",
            "data: a\r\n\r\ndata: b\r\n\r\n",
            "data: a\n\r\ndata: b\n\r\n",
        ] {
            let mut buf = wire.to_string();
            let mut got = Vec::new();
            while let Some(block) = next_sse_block(&mut buf) {
                got.push(block);
            }
            assert_eq!(got, vec!["data: a", "data: b"], "wire: {wire:?}");
            assert!(buf.is_empty(), "leftover after {wire:?}: {buf:?}");
        }

        let mut buf = "data: partial\r\n\r".to_string();
        assert!(next_sse_block(&mut buf).is_none(), "no separator yet");
        buf.push('\n');
        assert_eq!(
            next_sse_block(&mut buf).as_deref(),
            Some("data: partial"),
            "the separator arrived with the next read"
        );
    }

    /// The tool schema is out of band, so compaction cannot drop it -- and this
    /// pins that every request still carries it.
    #[test]
    fn every_request_carries_the_tool_schema() {
        let cfg = crate::config::LlmConfig {
            provider: None,
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "m".to_string(),
            context: None,
            api_key_env: String::new(),
            api_key: "k".to_string(),
            instructions: String::new(),
            temperature: None,
            max_output_tokens: None,
            store: false,
            request_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(1),
            max_retries: 0,
            retry_base: Duration::from_millis(1),
        };
        let client = LlmClient::new(&cfg).unwrap();
        let params = serde_json::json!({"type": "object", "properties": {"task": {"type": "string"}}, "required": ["cmd"]});
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "run".into(),
            parameters: params,
        }];

        // A compacted prefix must not change what the model is offered: the
        // schema is out of band, not part of the history.
        for summary in [None, Some("earlier work, compressed")] {
            let body = client.build_body(summary, &[], &tools, None);
            let sent = &body["tools"][0];
            assert_eq!(sent["name"], "bash");
            assert_eq!(sent["parameters"]["required"][0], "cmd");
            assert!(sent["parameters"]["properties"].get("task").is_some());
        }

        // An empty registry means no `tools` key at all, rather than an empty
        // array that some providers reject.
        let body = client.build_body(None, &[], &[], None);
        assert!(body.get("tools").is_none(), "{body}");
    }

    #[test]
    fn parses_sse_blocks() {
        let block = "event: response.output_text.delta\ndata: {\"delta\":\"hi\"}";
        let ev = parse_sse_block(block).unwrap();
        assert_eq!(ev.name, "response.output_text.delta");
        assert_eq!(ev.data["delta"], "hi");
    }

    #[test]
    fn ignores_keepalive_comments() {
        assert!(parse_sse_block(": ping").is_none());
        assert!(parse_sse_block("event: ping\ndata: {}").is_some());
    }

    #[test]
    fn reasoning_without_encrypted_content_is_dropped() {
        let item = HistoryItem::Reasoning {
            item_id: Some("rs_1".into()),
            encrypted: None,
            summary: "thinking".into(),
        };
        assert!(history_item_to_input(&item).is_none());
    }

    /// Some providers send an empty `summary` on `output_item.done`, so the
    /// streamed deltas must be the fallback.
    #[tokio::test]
    async fn reasoning_summary_falls_back_to_accumulated_deltas() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut items = HashMap::new();
        let ev = |name: &str, data: Value| SseEvent {
            name: name.to_string(),
            data,
        };
        handle(
            ev(
                "response.output_item.added",
                json!({"item": {"id": "rs_1", "type": "reasoning"}}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();
        for delta in ["I should ", "check the port."] {
            handle(
                ev(
                    "response.reasoning_summary_text.delta",
                    json!({"item_id": "rs_1", "delta": delta}),
                ),
                &mut items,
                &tx,
            )
            .await
            .unwrap();
        }
        handle(
            ev(
                "response.output_item.done",
                json!({"item": {
                    "id": "rs_1",
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "blob",
                }}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();

        let mut summary = None;
        let mut encrypted = None;
        while let Ok(e) = rx.try_recv() {
            if let LlmEvent::Reasoning {
                summary: s,
                encrypted: c,
                ..
            } = e
            {
                summary = Some(s);
                encrypted = c;
            }
        }
        assert_eq!(summary.as_deref(), Some("I should check the port."));
        assert_eq!(encrypted.as_deref(), Some("blob"));
    }

    /// When the provider does send a summary, it wins: it is the canonical text.
    #[tokio::test]
    async fn a_supplied_reasoning_summary_is_kept() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut items = HashMap::new();
        let ev = |name: &str, data: Value| SseEvent {
            name: name.to_string(),
            data,
        };
        handle(
            ev(
                "response.output_item.added",
                json!({"item": {"id": "rs_1", "type": "reasoning"}}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.reasoning_summary_text.delta",
                json!({"item_id": "rs_1", "delta": "from deltas"}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.output_item.done",
                json!({"item": {
                    "id": "rs_1",
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "from provider"}],
                    "encrypted_content": "blob",
                }}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();

        let mut summary = None;
        while let Ok(e) = rx.try_recv() {
            if let LlmEvent::Reasoning { summary: s, .. } = e {
                summary = Some(s);
            }
        }
        assert_eq!(summary.as_deref(), Some("from provider"));
    }

    /// Some providers stream deltas without an `output_item.added` first. The
    /// summary must survive that too.
    #[tokio::test]
    async fn reasoning_deltas_are_kept_without_an_added_event() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut items = HashMap::new();
        let ev = |name: &str, data: Value| SseEvent {
            name: name.to_string(),
            data,
        };
        handle(
            ev(
                "response.reasoning_summary_text.delta",
                json!({"item_id": "rs_1", "delta": "thinking "}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.reasoning_summary_text.delta",
                json!({"item_id": "rs_1", "delta": "out loud"}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.output_item.done",
                json!({"item": {
                    "id": "rs_1",
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "blob",
                }}),
            ),
            &mut items,
            &tx,
        )
        .await
        .unwrap();

        let mut summary = None;
        while let Ok(e) = rx.try_recv() {
            if let LlmEvent::Reasoning { summary: s, .. } = e {
                summary = Some(s);
            }
        }
        assert_eq!(summary.as_deref(), Some("thinking out loud"));
    }

    #[test]
    fn reasoning_replays_the_blob_without_its_summary_text() {
        let item = HistoryItem::Reasoning {
            item_id: Some("rs_1".into()),
            encrypted: Some("blob".into()),
            summary: "a long thought".into(),
        };
        let v = history_item_to_input(&item).unwrap();
        assert_eq!(v["type"], "reasoning");
        assert_eq!(v["encrypted_content"], "blob");
        assert_eq!(v["summary"], json!([]));
    }

    #[test]
    fn retry_delays_double_and_then_stop_growing() {
        let base = Duration::from_millis(500);
        assert_eq!(backoff(base, 1), Duration::from_millis(500));
        assert_eq!(backoff(base, 2), Duration::from_millis(1_000));
        assert_eq!(backoff(base, 3), Duration::from_millis(2_000));
        // Past the cap the wait stays the same rather than becoming a silence
        // of its own.
        assert_eq!(backoff(base, 12), MAX_RETRY_DELAY);
        assert_eq!(backoff(base, 64), MAX_RETRY_DELAY);
    }

    /// A silent request is retried; a spoken one is not, since a retry would
    /// answer twice.
    #[test]
    fn a_silent_response_is_retried_but_a_spoken_one_is_not() {
        assert!(retryable(0, 3, false));
        assert!(retryable(2, 3, false));
        // Out of attempts under an explicit ceiling.
        assert!(!retryable(3, 3, false));
        // Already produced content: another answer would contradict it.
        assert!(!retryable(0, 3, true));
        // 0 is no ceiling, not no retries: the cancel token is the only stop.
        assert!(retryable(0, 0, false));
        assert!(retryable(10_000, 0, false));
        // Even unlimited retries never ask twice after the model has spoken.
        assert!(!retryable(0, 0, true));
    }

    #[test]
    fn tool_results_use_the_output_item_shape() {
        let item = HistoryItem::ToolResult {
            call_id: "c1".into(),
            output: "ok".into(),
        };
        let v = history_item_to_input(&item).unwrap();
        assert_eq!(v["type"], "function_call_output");
        assert_eq!(v["call_id"], "c1");
    }

    /// `--thinking-effort` rides on the request as the API spells it; unset sends
    /// no `reasoning` object at all.
    #[test]
    fn a_thinking_level_rides_on_the_request() {
        let cfg = crate::config::LlmConfig {
            provider: None,
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "m".to_string(),
            context: None,
            api_key_env: String::new(),
            api_key: "k".to_string(),
            instructions: String::new(),
            temperature: None,
            max_output_tokens: None,
            store: false,
            request_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(1),
            max_retries: 0,
            retry_base: Duration::from_millis(1),
        };
        let client = LlmClient::new(&cfg).unwrap();
        let body = client.build_body(None, &[], &[], Some("high"));
        assert_eq!(body["reasoning"]["effort"], "high");
        let body = client.build_body(None, &[], &[], None);
        assert!(
            body.get("reasoning").is_none(),
            "an unset level must not become an empty reasoning object"
        );
    }
}
