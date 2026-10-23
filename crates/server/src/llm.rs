//! Streaming client for the OpenAI Responses API.
//!
//! The wire format is server-sent events over a POST to `{base_url}/responses`.
//! We parse it by hand rather than pulling in an SSE crate: the event set we
//! care about is small, and owning the parser means we can surface partial
//! tool-call arguments to the UI as they arrive.

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
        // The timeout lives on the client so every request is covered, the
        // compression call included. It is what turns "the provider went
        // quiet" from a turn that hangs forever into a retryable error.
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

    /// The model this client asks for.
    ///
    /// Logged at the start of every turn, so "which model answered this one"
    /// is a fact rather than something to infer from the transcript.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Streams one model request. `summary`, when present, stands in for the
    /// part of the transcript that has been compressed away; it is sent as the
    /// first item so the model reads it before anything else.
    ///
    /// A request that fails, times out, or completes with nothing in it is
    /// retried with a doubling delay, because all three look identical from
    /// here and all three are the provider's bad minute rather than ours. The
    /// caller sees one stream either way; a retry that is about to happen is
    /// announced as [`LlmEvent::Retrying`] so the operator can watch it happen.
    pub fn stream(
        &self,
        summary: Option<&str>,
        history: &[HistoryItem],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> mpsc::Receiver<LlmEvent> {
        let (tx, rx) = mpsc::channel(256);
        let body = self.build_body(summary, history, tools);
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
                // An empty completion is worth asking for again: the provider
                // acknowledged the request and then said nothing, which is what
                // an overloaded or truncated response looks like. So is a
                // stream that stopped without completing.
                if !retryable(attempt, max_retries, produced)
                    || cancel.is_cancelled()
                {
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
        if !self.store {
            // Without server-side storage, reasoning can only be replayed from
            // its encrypted blob.
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        body
    }

    /// Asks for a plain-text answer: no tools, no streaming. Used only to
    /// compress old context, which must never itself trigger a tool call.
    /// One non-streaming request, used for compression.
    ///
    /// Retried on the same terms as a stream: an empty summary is not a
    /// summary, and a provider that swallowed the request once often answers
    /// the second time. `max_retries == 0` is no ceiling, so the turn's
    /// `cancel` is what stops it -- compression must never outlive the turn
    /// that asked for it.
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
                    return Ok(empty)
                }
                Ok(_) => tracing::warn!(attempt, "compression came back empty; retrying"),
                Err(e) if self.max_retries != 0 && attempt >= self.max_retries => return Err(e),
                Err(e) => tracing::warn!(attempt, error = %e, "compression request failed; retrying"),
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

/// Budget for a compression request. The summary stands in for tens of
/// kilobytes, so it only has to be a page or so itself.
const SUMMARY_MAX_TOKENS: u32 = 2_000;

/// Pulls the assistant text out of a non-streaming Responses payload, falling
/// back to a slice of the body so a surprising shape is still diagnosable.
fn extract_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(2_000).collect();
    };
    if let Some(text) = value["output_text"].as_str() {
        return text.to_string();
    }
    let mut out = String::new();
    let items = value["output"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    for item in items {
        if item["type"] != "message" {
            continue;
        }
        let parts = item["content"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
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
/// `item_id` is carried on every variant because a single response can interleave
/// several output items; the loop does not need it today but the identity is the
/// thing that makes the events unambiguous if it ever does.
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
    /// The previous attempt produced nothing usable and another one is coming
    /// after `delay`. Announced rather than hidden so a stalled turn reads as
    /// "retrying" instead of "thinking".
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

/// Whether another attempt is worth making.
///
/// `produced` is the only thing that rules one out: once the model has said
/// anything, asking again would stand a second, different answer behind the
/// first. Everything else -- an error, a timeout, a completion with nothing in
/// it -- is the provider's bad minute and deserves another try.
///
/// `max_retries == 0` is *no ceiling*: a long-running turn is not allowed to
/// die on a transient outage at hour three, so the default retries until the
/// turn's cancel token fires. The cancel is the only honest stop -- nothing
/// about a bad provider's recovery time is knowable in advance.
fn retryable(attempt: u32, max_retries: u32, produced: bool) -> bool {
    (max_retries == 0 || attempt < max_retries) && !produced
}

/// One attempt at a streaming request, handed back as a channel.
///
/// Split out from [`LlmClient::stream`] so that a retry is just another call:
/// no attempt carries state into the next one, and the caller drains each the
/// same way.
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
                let _ = tx.send(LlmEvent::Failed { message: format!("{e}") }).await;
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
        // Silence counts as failure. A connection that is still open but has
        // stopped sending is exactly the state that used to park a turn
        // forever: no error, no EOF, nothing to wake up on.
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
        // A stream that ends without its trailing blank line still sent a final
        // event; dropping what is left is how the last `response.completed`
        // goes missing and the turn looks like the model said nothing.
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
    /// Text accumulated from deltas. Providers do not all return the finished
    /// text on `output_item.done` (MiniMax sends an empty `summary` array), so
    /// the deltas are the only place the readable text ever exists.
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
/// A block ends at a blank line, and providers do not agree on what a blank
/// line is: `\n\n`, `\r\n\r\n`, and even `\n\r\n` all occur. Knowing only one
/// of them looks exactly like silence -- no block ever completes, so nothing is
/// parsed and the turn dies on the idle timeout with "no response data", which
/// blames the provider for a separator nobody promised to send.
///
/// The block it returns has its trailing `\r` stripped, so the caller only ever
/// sees `data:` lines.
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
                    tx.send(LlmEvent::PartStart { part: ResponsePart::Reasoning, item_id: id }).await?;
                }
                ItemKind::Message => {
                    tx.send(LlmEvent::PartStart { part: ResponsePart::Output, item_id: id }).await?;
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
            // Not every provider emits `output_item.added` before the deltas,
            // so create the entry if it is missing: `output_item.done` sorts
            // out the kind from `type` regardless of how we got here.
            items.entry(item_id.clone()).or_default().text.push_str(&delta);
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
                    // The readable summary is the provider's if it sends one;
                    // otherwise the deltas we accumulated are all there is.
                    // MiniMax is in the second camp: its `summary` array on
                    // `output_item.done` is empty, so without this every stored
                    // reasoning entry would have a blank summary forever.
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
                    tx.send(LlmEvent::PartDone { part: ResponsePart::Reasoning, item_id: id }).await?;
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
                    tx.send(LlmEvent::Output { item_id: id.clone(), text }).await?;
                    tx.send(LlmEvent::PartDone { part: ResponsePart::Output, item_id: id }).await?;
                }
                ItemKind::FunctionCall => {
                    tx.send(LlmEvent::ToolCall {
                        item_id: id.clone(),
                        call_id: meta.call_id,
                        name: meta.name,
                        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
                    })
                    .await?;
                    tx.send(LlmEvent::PartDone { part: ResponsePart::ToolCall, item_id: id }).await?;
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
        HistoryItem::ToolCall { call_id, name, arguments } => {
            json!({"type":"function_call","call_id":call_id,"name":name,"arguments":arguments})
        }
        HistoryItem::ToolResult { call_id, output } => {
            json!({"type":"function_call_output","call_id":call_id,"output":output})
        }
        // The stored `summary` is for humans: the UI replays it when a client
        // reconnects. It is deliberately *not* sent back to the model, which
        // already has its reasoning state in the encrypted blob -- echoing the
        // summary too would put every thought into `input[]` a second time.
        HistoryItem::Reasoning {
            item_id,
            encrypted,
            summary: _summary,
        } => {
            // Replaying reasoning requires the encrypted blob; without it the
            // item is dropped rather than sent in a form the API would reject.
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

    /// Every separator a provider might use ends a block, and the `\r` does not
    /// travel into the parser; a block split across two reads is only complete
    /// once its separator lands.
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

    /// The tool schema is not part of the transcript, so nothing a compaction
    /// does can remove it -- but that also means nothing would notice if the
    /// body stopped carrying it. It is rebuilt from the registry every request,
    /// and this pins that.
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
            let body = client.build_body(summary, &[], &tools);
            let sent = &body["tools"][0];
            assert_eq!(sent["name"], "bash");
            assert_eq!(sent["parameters"]["required"][0], "cmd");
            assert!(sent["parameters"]["properties"].get("task").is_some());
        }

        // An empty registry means no `tools` key at all, rather than an empty
        // array that some providers reject.
        let body = client.build_body(None, &[], &[]);
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

    /// MiniMax streams the readable thinking as deltas and hands back an empty
    /// `summary` on `output_item.done`. Storing that would mean every reasoning
    /// entry in the transcript has a blank summary, so the deltas are the
    /// fallback.
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

    /// When the provider does send a summary, it wins: it is the same text the
    /// deltas carry, and it is the one the API considers canonical.
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

    /// A request that said nothing is worth asking again; one that said
    /// something is not, because the retry would answer a second time.
    #[test]
    fn a_silent_response_is_retried_but_a_spoken_one_is_not() {
        assert!(retryable(0, 3, false));
        assert!(retryable(2, 3, false));
        // Out of attempts under an explicit ceiling.
        assert!(!retryable(3, 3, false));
        // Already produced content: another answer would contradict it.
        assert!(!retryable(0, 3, true));
        // 0 is "no ceiling", not "no retries": a ten-day turn must not die on
        // a transient outage, and the cancel token is what stops it.
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
}
