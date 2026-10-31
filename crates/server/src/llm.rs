//! Streaming client for the OpenAI Responses API.
//!
//! See `docs/notes/server-llm.md`.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use futures_util::{FutureExt, StreamExt};
use protocol::ResponsePart;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::store::{activity_detail, activity_name};

/// A connect that takes this long is a broken route, not a slow provider: the
/// answer itself is bounded by `headers_timeout` and `idle_timeout`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    store: bool,
    /// How long the provider may take to answer with response headers.
    ///
    /// Nothing about the body: an answer that is still arriving is never cut
    /// by a wall clock, only by [`Self::idle_timeout`].
    headers_timeout: Duration,
    /// Silence of *content* that counts as a broken stream.
    ///
    /// Chunks that carry no event -- keep-alives, comments -- do not move this
    /// clock, so a provider holding the socket open with nothing to say is as
    /// silent as one sending no bytes at all.
    idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response.
    max_retries: u32,
    /// First retry delay; doubled per attempt up to [`MAX_RETRY_DELAY`].
    retry_base: Duration,
    /// Provider-side tools, in the provider's own spelling; sent verbatim.
    builtin_tools: Vec<String>,
    /// What the roster says this model takes in; empty is "unstated".
    modalities: Vec<String>,
}

/// Where a request's input comes from: one session's transcript, up to a seq pinned
/// when the request started.
///
/// # Invariant
///
/// The source names the window rather than holding the body, because a body is read
/// once and a retry has to send the same bytes as the attempt it follows. The window
/// is closed before the request is sent, so a row landing while the network is still
/// writing belongs to the next request, not to a retry of this one.
#[derive(Clone)]
pub struct RequestSource {
    pub db_path: std::path::PathBuf,
    pub session_id: i64,
    pub upto_seq: i64,
    /// What a call that never got its result is said to have been.
    pub placeholder: String,
}

impl RequestSource {
    fn open(
        &self,
        head: &[u8],
    ) -> anyhow::Result<(
        crate::body::RequestBody,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    )> {
        let rows =
            crate::transcript::Transcript::open(&self.db_path, self.session_id, self.upto_seq)?;
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body = crate::body::RequestBody::new(
            head.to_vec(),
            rows,
            self.placeholder.clone(),
            counter.clone(),
        );
        Ok((body, counter))
    }
}

impl LlmClient {
    pub fn new(cfg: &crate::config::LlmConfig) -> anyhow::Result<Self> {
        if cfg.api_key.is_empty() {
            anyhow::bail!(
                "no API key: set {} (or GONGGA_API_KEY) before starting the server",
                cfg.api_key_env
            );
        }
        // No client-wide deadline on purpose: a wall clock covers the body
        // too, and a body that is still producing is the one thing an answer
        // of any length is entitled to do. What is left needs bounds that
        // know what they are bounding -- [`CONNECT_TIMEOUT`] for the socket,
        // `headers_timeout` for the answer, `idle_timeout` for the content.
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            temperature: cfg.temperature,
            max_output_tokens: cfg.max_output_tokens,
            store: cfg.store,
            headers_timeout: cfg.headers_timeout,
            idle_timeout: cfg.idle_timeout,
            max_retries: cfg.max_retries,
            retry_base: cfg.retry_base,
            builtin_tools: cfg.builtin_tools.clone(),
            modalities: cfg.modalities.clone(),
        })
    }

    /// The model this client asks for; logged at the start of every turn.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether the roster says this model can look at a picture.
    pub fn accepts_image(&self) -> bool {
        crate::config::accepts_image(&self.modalities)
    }

    /// Mints a provider-side id for one picture, so the request carries that
    /// instead of the bytes.
    ///
    /// # Invariant
    ///
    /// The id is kept on the [`crate::image::ImageRef`], so a replayed transcript
    /// reuses it instead of uploading again.
    pub async fn upload_image(&self, path: &str, media_type: &str) -> anyhow::Result<String> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name)
            .mime_str(media_type)?;
        let form = reqwest::multipart::Form::new()
            .text("purpose", "user_data")
            .part("file", part);
        let url = format!("{}/files", self.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("HTTP {status} from {url}: {}", shorten_body(&body));
        }
        let value: Value = serde_json::from_str(&body)?;
        value["id"].as_str().map(str::to_string).ok_or_else(|| {
            anyhow::anyhow!(
                "{url} returned no file id: {}",
                body.chars().take(200).collect::<String>()
            )
        })
    }

    /// What this turn's model can see, with the refusal rendered from `prompts`.
    pub fn vision(&self, prompts: &crate::prompts::Prompts) -> Vision {
        Vision {
            accepts_image: self.accepts_image(),
            refusal: crate::config::image_refusal(prompts, &self.model, &self.modalities),
        }
    }

    /// Streams one model request; `handover`, when present, is sent first.
    ///
    /// # Invariant
    ///
    /// A failed, timed-out, or empty attempt is retried with a doubling delay:
    /// the caller always sees one stream, and each retry is announced as
    /// [`LlmEvent::Retrying`]. A panic inside one attempt is that attempt's
    /// failure and enters the same loop, so it never ends this stream by
    /// unwinding past it. See `docs/notes/server-llm.md`.
    pub fn stream(
        &self,
        instructions: &str,
        handover: Option<&str>,
        source: &RequestSource,
        tools: &[ToolDefinition],
        effort: Option<&str>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<LlmEvent> {
        let (tx, rx) = mpsc::channel(256);
        let head = self.build_head(instructions, handover, tools, effort);
        let source = source.clone();
        let url = format!("{}/responses", self.base_url);
        let api_key = self.api_key.clone();
        let http = self.http.clone();
        let headers_timeout = self.headers_timeout;
        let idle_timeout = self.idle_timeout;
        let max_retries = self.max_retries;
        let retry_base = self.retry_base;

        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            loop {
                // Opened again for every attempt: a body is read once, and a retry
                // has to send what the attempt before it sent.
                let (body, bytes) = match source.open(&head) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = tx
                            .send(LlmEvent::Failed {
                                message: format!("could not read the transcript: {e}"),
                                retryable: false,
                            })
                            .await;
                        return;
                    }
                };
                let mut once = stream_once(
                    &http,
                    &url,
                    &api_key,
                    body,
                    bytes,
                    headers_timeout,
                    idle_timeout,
                    cancel.clone(),
                )
                .await;
                let mut produced = false;
                let mut completed = false;
                let mut cancelled = false;
                // Held back, not forwarded: if a retry follows, this error is
                // that retry's `reason` rather than the answer.
                let mut failure: Option<(String, bool)> = None;

                while let Some(event) = once.recv().await {
                    produced |= event_produced(&event);
                    match &event {
                        LlmEvent::Completed { .. } => completed = true,
                        LlmEvent::Cancelled => cancelled = true,
                        LlmEvent::Failed { message, retryable } => {
                            failure = Some((message.clone(), *retryable));
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
                // No completion, no failure: the read stopped short or died, and a
                // retry needs a reason to announce.
                if !completed && failure.is_none() {
                    failure = Some((retry_reason(&None, false, produced), true));
                }
                // Empty completion or cut-short stream: both are retryable, so
                // long as nothing said this attempt can never work.
                let refused = failure.as_ref().is_some_and(|(_, again)| !*again);
                if refused || !retryable(attempt, max_retries, produced) || cancel.is_cancelled() {
                    if let Some((message, again)) = failure {
                        let _ = tx
                            .send(LlmEvent::Failed {
                                message: with_size_hint(crate::prompts::shared(), message),
                                retryable: again,
                            })
                            .await;
                    }
                    return;
                }

                attempt += 1;
                let delay = backoff(retry_base, attempt);
                let reason = retry_reason(&failure, completed, produced);
                tracing::warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    empty = completed && !produced,
                    reason = %reason,
                    "model request gave nothing usable; retrying"
                );
                let _ = tx
                    .send(LlmEvent::Retrying {
                        attempt,
                        max: max_retries,
                        delay,
                        reason,
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

    fn build_head(
        &self,
        instructions: &str,
        note: Option<&str>,
        tools: &[ToolDefinition],
        effort: Option<&str>,
    ) -> Vec<u8> {
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "store": self.store,
        });
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        // One array holds both kinds of tool; empty sends no key at all.
        let mut offered: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        // Verbatim: the roster knows each provider's spelling, not this server.
        offered.extend(
            self.builtin_tools
                .iter()
                .map(|name| json!({ "type": name })),
        );
        if !offered.is_empty() {
            body["tools"] = offered.into();
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
        // Reopened at `input`, whose array the body fills as the network asks: the
        // transcript is never one value.
        let mut head = body.to_string();
        head.pop();
        head.push_str(",\"input\":[");
        // The note is the array's first item, so the transcript never displaces it.
        if let Some(note) = note {
            head.push_str(
                &json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": note}],
                })
                .to_string(),
            );
            head.push(',');
        }
        head.into_bytes()
    }
}

/// What the turn's model can take in, fixed when the turn starts.
#[derive(Debug, Clone)]
pub struct Vision {
    pub accepts_image: bool,
    /// Why not, in the roster's words; empty when it can.
    pub refusal: String,
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
    /// A provider-side tool item, kept for replay. See `docs/notes/server-llm.md`.
    BuiltinCall {
        item_id: String,
        item: Value,
        replayable: bool,
    },
    /// Where a sentence came from: the URL a response text was annotated with.
    Citation {
        item_id: String,
        title: String,
        url: String,
    },
    Completed {
        usage: Option<Usage>,
    },
    Failed {
        message: String,
        /// Whether resending this request could succeed at all.
        retryable: bool,
    },
    Cancelled,
    /// The previous attempt gave nothing usable; another follows after `delay`.
    Retrying {
        attempt: u32,
        max: u32,
        delay: Duration,
        /// Why that attempt gave nothing; the first failure's only record.
        reason: String,
    },
}

/// Anything the caller was handed to read, as opposed to the provider's own
/// liveness noise.
fn event_content(event: &LlmEvent) -> bool {
    matches!(
        event,
        LlmEvent::Delta { .. }
            | LlmEvent::Output { .. }
            | LlmEvent::Reasoning { .. }
            | LlmEvent::ToolCall { .. }
            | LlmEvent::BuiltinCall { .. }
    )
}

/// Any content means this attempt said something. An activity beat counts too:
/// a completed response may hold nothing else.
fn event_produced(event: &LlmEvent) -> bool {
    event_content(event)
        || matches!(
            event,
            LlmEvent::PartStart {
                part: ResponsePart::Activity,
                ..
            }
        )
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

/// Whether resending the same request could succeed: a bad minute could, a
/// refusal cannot.
///
/// # Invariant
///
/// `408` and `429` are 4xx but describe *timing*, not the request, so they stay
/// retryable. Every other 4xx is the provider saying the request itself is
/// wrong -- resending it forever only keeps the operator from seeing why.
fn status_retryable(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Both use the same `message`/`type`/`code` shape; only `response.failed` nests
/// its error under `response`.
fn event_error(d: &Value) -> &Value {
    let error = &d["response"]["error"];
    if error.is_object() {
        error
    } else {
        &d["error"]
    }
}

/// Whether a streamed `error` event describes a bad minute or a refusal.
///
/// # Invariant
///
/// An unknown kind stays retryable: providers invent their own spellings, and
/// giving up on one this code has not heard of is worse than one extra try.
fn error_retryable(d: &Value) -> bool {
    let error = event_error(d);
    let kind = error["type"]
        .as_str()
        .or_else(|| error["code"].as_str())
        .unwrap_or_default();
    !matches!(
        kind,
        "invalid_request_error"
            | "context_length_exceeded"
            | "authentication_error"
            | "permission_error"
            | "not_found_error"
            | "insufficient_quota"
    )
}

/// Adds the one instruction a size refusal leaves out, from `size_hint`.
///
/// # Invariant
///
/// The provider says "too long" and stops there; the fix is a shell command the
/// reader has to know about. Anything that is not about size keeps its words.
fn with_size_hint(prompts: &crate::prompts::Prompts, message: String) -> String {
    let lower = message.to_ascii_lowercase();
    let about_size = lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("too many tokens")
        || lower.contains("too long");
    if !about_size {
        return message;
    }
    // One line, so the hint reads as a sentence rather than as a wrapped
    // fragment of the provider's message.
    prompts.render("size_hint", &[("message", message.as_str())])
}

/// What the previous attempt did, in the words the operator needs to read.
///
/// # Invariant
///
/// Never empty: a retry that names no cause is the line this replaces, and it
/// left "why did it fail" unanswerable once the retry succeeded.
fn retry_reason(failure: &Option<(String, bool)>, completed: bool, produced: bool) -> String {
    if let Some((message, _)) = failure {
        return message.clone();
    }
    if completed {
        // Acknowledged, then said nothing.
        "the provider completed the response with no content in it".to_string()
    } else if produced {
        "the stream ended before the response completed".to_string()
    } else {
        "the stream ended with nothing in it".to_string()
    }
}

/// One attempt at a streaming request, handed back as a channel.
///
/// See `docs/notes/server-llm.md`.
async fn stream_once(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: crate::body::RequestBody,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    headers_timeout: Duration,
    idle_timeout: Duration,
    cancel: CancellationToken,
) -> mpsc::Receiver<LlmEvent> {
    let (tx, rx) = mpsc::channel(256);
    let url = url.to_string();
    let api_key = api_key.to_string();
    let http = http.clone();

    tokio::spawn(async move {
        let body = body;
        let outcome = AssertUnwindSafe(read_stream(
            &http,
            &url,
            &api_key,
            body,
            bytes,
            headers_timeout,
            idle_timeout,
            &cancel,
            &tx,
        ))
        .catch_unwind()
        .await;
        if let Err(payload) = outcome {
            let message = crate::agent::panic_message(&*payload);
            tracing::error!(error = %message, "model request: reading the stream panicked");
            let _ = tx
                .send(LlmEvent::Failed {
                    message: format!("reading the response panicked: {message}"),
                    retryable: true,
                })
                .await;
        }
    });

    rx
}

/// Reads one streamed response into events until it ends, is cancelled, or goes quiet.
async fn read_stream(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: crate::body::RequestBody,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    headers_timeout: Duration,
    idle_timeout: Duration,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<LlmEvent>,
) {
    let started = Instant::now();
    tracing::info!(%url, "model request: sending");
    // The count is known only afterwards. The type must be stated: a provider
    // answers 415 for a streamed body without it.
    let request = http
        .post(url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .bearer_auth(api_key)
        .body(reqwest::Body::wrap_stream(body));

    let response = match tokio::time::timeout(headers_timeout, request.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, elapsed = ?started.elapsed(), "model request: send failed");
            let _ = tx
                .send(LlmEvent::Failed {
                    message: format!("{e}"),
                    retryable: true,
                })
                .await;
            return;
        }
        Err(_) => {
            tracing::warn!(
                limit = ?headers_timeout,
                elapsed = ?started.elapsed(),
                "model request: no answer"
            );
            let _ = tx
                .send(LlmEvent::Failed {
                    message: format!("no response headers within {headers_timeout:?}"),
                    retryable: true,
                })
                .await;
            return;
        }
    };
    tracing::info!(
        status = %response.status(),
        bytes = bytes.load(std::sync::atomic::Ordering::Relaxed),
        elapsed = ?started.elapsed(),
        "model request: headers"
    );
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        tracing::warn!(%status, body = %shorten_body(&text), "model request: rejected");
        let _ = tx
            .send(LlmEvent::Failed {
                message: format!("HTTP {status}: {}", shorten_body(&text)),
                retryable: status_retryable(status),
            })
            .await;
        return;
    }

    let mut stream = response.bytes_stream();
    let mut buf = String::new();
    // item_id -> output item metadata, so deltas can be attributed.
    let mut items: HashMap<String, ItemMeta> = HashMap::new();
    let mut out = Outbox::new(tx);
    // Silence of content is failure: no error, no EOF, nothing else wakes the
    // turn. It is deliberately not "time since the request started", which
    // would cut a long answer in the middle of writing it.
    let idle = tokio::time::sleep(idle_timeout);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = out.send(LlmEvent::Cancelled).await;
                return;
            }
            _ = &mut idle => {
                tracing::warn!(idle = ?idle_timeout, elapsed = ?started.elapsed(), "model request: no content");
                let _ = out.send(LlmEvent::Failed {
                    message: format!("nothing said for {idle_timeout:?}"),
                    retryable: true,
                }).await;
                return;
            }
            next = stream.next() => {
                let Some(chunk) = next else { break };
                match chunk {
                    Ok(bytes) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(block) = next_sse_block(&mut buf) {
                            if let Some(event) = parse_sse_block(&block) {
                                if let Err(e) = handle(event, &mut items, &mut out).await {
                                    tracing::debug!(error = %e, "llm stream closed early");
                                    return;
                                }
                            }
                        }
                        // Only content moves the clock. A chunk that held a
                        // keep-alive said nothing, and a provider that keeps the
                        // socket warm while saying nothing is the stall this
                        // bound is for.
                        if out.take_content() {
                            idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        }
                    }
                    Err(e) => {
                        let _ = out.send(LlmEvent::Failed { message: format!("{e}"), retryable: true }).await;
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
            let _ = handle(event, &mut items, &mut out).await;
        }
    }
    tracing::info!(elapsed = ?started.elapsed(), "model request: stream ended");
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// How much of `input_tokens` the provider served from its own prefix cache.
    pub input_tokens_details: Option<InputTokens>,
}

/// The nested half of `usage`, spelled the way the Responses API spells it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputTokens {
    pub cached_tokens: Option<u64>,
}

impl Usage {
    /// Tokens billed at the cache rate, when the provider says: `None` is it
    /// declining to say, not a miss.
    pub fn cached_tokens(&self) -> Option<u64> {
        self.input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
    }
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
    /// A tool the provider executed: `web_search_call`, `web_fetch_call`, ...
    ProviderCall,
}

/// Whether an item type names a tool the provider runs on its own side.
///
/// # Invariant
///
/// The rule is the `_call` suffix, not a list of names: providers add their own
/// (`web_fetch_call`, `mcp_call`, ...), and a name this server has not heard of
/// must still be visible. `function_call` is ours and is matched first.
fn is_provider_call(item_type: &str) -> bool {
    item_type.ends_with("_call") && item_type != "function_call"
}

/// A provider activity item, made safe to keep and to replay.
///
/// # Invariant
///
/// A generated picture does not belong in the transcript, yet the item is not a
/// shape the API takes back without it: the bytes go beside the home and the row
/// keeps the id they live under, and `sendable_activity` puts them back. An item
/// whose file could not be written stays non-replayable and says so.
/// See `docs/notes/server-blobs.md`.
fn storable_activity(item: &Value) -> (Value, bool) {
    let mut item = item.clone();
    // Removed, not blanked: a `null` is still a payload field to the provider.
    let Some(payload) = item.as_object_mut().and_then(|o| o.remove("result")) else {
        return (item, true);
    };
    if payload.is_null() {
        return (item, true);
    }
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let stored = match (crate::blobs::dir(), id.is_empty()) {
        (Some(dir), false) => crate::blobs::put(dir, &id, &payload.to_string())
            .ok()
            .map(|()| {
                item["result_blob"] = json!(id);
            }),
        _ => None,
    };
    if stored.is_none() {
        tracing::warn!(
            item_type = item["type"].as_str().unwrap_or("?"),
            item_id = %id,
            "no blob directory to keep this provider payload in, so the item cannot be replayed"
        );
    }
    (item, stored.is_some())
}

/// The item as a request should carry it, payload read back from `dir`.
pub(crate) fn sendable_activity(
    item: &Value,
    replayable: bool,
    dir: Option<&std::path::Path>,
) -> Option<Value> {
    let mut item = item.clone();
    let blob = item
        .get("result_blob")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(id) = blob {
        let value = dir
            .and_then(|dir| crate::blobs::read(dir, &id))
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let Some(payload) = value else {
            tracing::warn!(
                item_id = %id,
                "this provider item's payload file is gone; leaving the item out of this request"
            );
            return None;
        };
        item["result"] = payload;
        item.as_object_mut()?.remove("result_blob");
        return Some(item);
    }
    if !replayable {
        tracing::warn!(
            item_type = item["type"].as_str().unwrap_or("?"),
            item_id = item["id"].as_str().unwrap_or("?"),
            "this provider tool item lost its binary payload when it was stored, \
             so it cannot be replayed; leaving it out of this request"
        );
        return None;
    }
    Some(item)
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

/// `response.<type>.<phase>` for a provider tool's own progress beats.
fn provider_phase(event_name: &str) -> Option<(&str, &str)> {
    let rest = event_name.strip_prefix("response.")?;
    let (item_type, phase) = rest.split_once('.')?;
    is_provider_call(item_type).then_some((item_type, phase))
}

/// The event channel, plus whether content has gone out since anyone asked.
///
/// It exists so the idle clock can be moved by what the model *said* rather
/// than by what the socket carried: see [`crate`]'s note on `idle_timeout`.
struct Outbox<'a> {
    tx: &'a mpsc::Sender<LlmEvent>,
    content: bool,
}

impl<'a> Outbox<'a> {
    fn new(tx: &'a mpsc::Sender<LlmEvent>) -> Self {
        Self { tx, content: false }
    }

    async fn send(&mut self, event: LlmEvent) -> Result<(), mpsc::error::SendError<LlmEvent>> {
        self.content |= event_content(&event);
        self.tx.send(event).await
    }

    /// Whether content went out since the last call, clearing the mark.
    fn take_content(&mut self) -> bool {
        std::mem::take(&mut self.content)
    }
}

async fn handle(
    event: SseEvent,
    items: &mut HashMap<String, ItemMeta>,
    out: &mut Outbox<'_>,
) -> Result<(), mpsc::error::SendError<LlmEvent>> {
    let d = &event.data;
    // The provider's own beats land on the line its item already opened.
    if let Some((_, phase)) = provider_phase(&event.name) {
        let text = match phase {
            "in_progress" => " (started)",
            "searching" => " (searching)",
            "generating" => " (generating)",
            "interpreting" => " (reading)",
            _ => "",
        };
        if !text.is_empty() {
            out.send(LlmEvent::Delta {
                part: ResponsePart::Activity,
                item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                text: text.to_string(),
            })
            .await?;
        }
        return Ok(());
    }
    match event.name.as_str() {
        "response.output_item.added" => {
            let item = &d["item"];
            let id = item["id"].as_str().unwrap_or_default().to_string();
            let kind = match item["type"].as_str().unwrap_or_default() {
                "reasoning" => ItemKind::Reasoning,
                "message" => ItemKind::Message,
                "function_call" => ItemKind::FunctionCall,
                other if is_provider_call(other) => ItemKind::ProviderCall,
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
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::Reasoning,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::Message => {
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::Output,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::FunctionCall => {
                    // Only the marker: the arguments arrive as deltas of their own
                    // and are appended to this part, so the call is shown once --
                    // `name(` now, the deltas as they come, `)` at `done`.
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::ToolCall,
                        item_id: id.clone(),
                    })
                    .await?;
                    out.send(LlmEvent::Delta {
                        part: ResponsePart::ToolCall,
                        item_id: id,
                        text: format!("{}(", item["name"].as_str().unwrap_or("?")),
                    })
                    .await?;
                }
                ItemKind::ProviderCall => {
                    let shown = activity_name(item["type"].as_str().unwrap_or("call"));
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::Activity,
                        item_id: id.clone(),
                    })
                    .await?;
                    out.send(LlmEvent::Delta {
                        part: ResponsePart::Activity,
                        item_id: id,
                        text: shown.to_string(),
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
            out.send(LlmEvent::Delta {
                part: ResponsePart::Reasoning,
                item_id,
                text: delta,
            })
            .await?;
        }
        "response.output_text.delta" => {
            out.send(LlmEvent::Delta {
                part: ResponsePart::Output,
                item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                text: d["delta"].as_str().unwrap_or_default().to_string(),
            })
            .await?;
        }
        "response.function_call_arguments.delta" => {
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let delta = d["delta"].as_str().unwrap_or_default().to_string();
            // Remembered so `done` knows whether the arguments were already shown.
            items
                .entry(item_id.clone())
                .or_default()
                .text
                .push_str(&delta);
            out.send(LlmEvent::Delta {
                part: ResponsePart::ToolCall,
                item_id,
                text: delta,
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
                    other if is_provider_call(other) => ItemKind::ProviderCall,
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
                    out.send(LlmEvent::Reasoning {
                        item_id: id.clone(),
                        summary,
                        encrypted: item["encrypted_content"].as_str().map(str::to_string),
                    })
                    .await?;
                    out.send(LlmEvent::PartDone {
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
                    out.send(LlmEvent::Output {
                        item_id: id.clone(),
                        text,
                    })
                    .await?;
                    out.send(LlmEvent::PartDone {
                        part: ResponsePart::Output,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::FunctionCall => {
                    // The call was streamed as `name(` + its argument deltas, so all
                    // that is left to say is the closing bracket -- and one line
                    // for a provider that streamed no deltas at all.
                    let streamed = !meta.text.is_empty();
                    let text = if streamed {
                        ")".to_string()
                    } else {
                        let args = item["arguments"].as_str().unwrap_or("{}");
                        format!("{}({args})", meta.name)
                    };
                    out.send(LlmEvent::Delta {
                        part: ResponsePart::ToolCall,
                        item_id: id.clone(),
                        text,
                    })
                    .await?;
                    out.send(LlmEvent::ToolCall {
                        item_id: id.clone(),
                        call_id: meta.call_id,
                        name: meta.name,
                        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
                    })
                    .await?;
                    out.send(LlmEvent::PartDone {
                        part: ResponsePart::ToolCall,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::ProviderCall => {
                    let (stored, replayable) = storable_activity(item);
                    out.send(LlmEvent::Delta {
                        part: ResponsePart::Activity,
                        item_id: id.clone(),
                        text: activity_detail(item),
                    })
                    .await?;
                    out.send(LlmEvent::BuiltinCall {
                        item_id: id.clone(),
                        item: stored,
                        replayable,
                    })
                    .await?;
                    out.send(LlmEvent::PartDone {
                        part: ResponsePart::Activity,
                        item_id: id,
                    })
                    .await?;
                }
                ItemKind::Unknown => {}
            }
        }
        // Where a sentence came from: a line of its own, or a search has no sources.
        "response.output_text.annotation.added" => {
            let annotation = &d["annotation"];
            if let Some(url) = annotation["url"].as_str() {
                out.send(LlmEvent::Citation {
                    item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                    title: annotation["title"].as_str().unwrap_or_default().to_string(),
                    url: url.to_string(),
                })
                .await?;
            }
        }
        "response.completed" => {
            let usage = serde_json::from_value(d["response"]["usage"].clone()).ok();
            out.send(LlmEvent::Completed { usage }).await?;
        }
        "response.failed" | "error" => {
            let error = event_error(d);
            let message = error["message"]
                .as_str()
                .or_else(|| d["message"].as_str())
                .unwrap_or("unknown error")
                .to_string();
            out.send(LlmEvent::Failed {
                message,
                retryable: error_retryable(d),
            })
            .await?;
        }
        _ => {}
    }
    Ok(())
}

/// One stored picture as an `input_image` part, read from disk now.
///
/// # Invariant
///
/// The transcript holds a path, never the bytes; this is the only place a file
/// is read. A picture that is gone degrades to a sentence naming it, because a
/// turn that cannot see one image is still a turn. See
/// `docs/notes/server-llm.md`.
/// Pulls a readable message out of an error body, tolerating plain text.
fn shorten_body(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) => v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| body.chars().take(400).collect()),
        Err(_) => body.chars().take(400).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with no roster behind it: what a test drives is the body it builds.
    fn client_with(model: &str, builtin_tools: &[&str], modalities: &[&str]) -> LlmClient {
        let cfg = crate::config::LlmConfig {
            provider: None,
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: model.to_string(),
            context: None,
            api_key_env: String::new(),
            api_key: "k".to_string(),
            temperature: None,
            max_output_tokens: None,
            store: false,
            headers_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(1),
            max_retries: 0,
            retry_base: Duration::from_millis(1),
            builtin_tools: builtin_tools.iter().map(|t| t.to_string()).collect(),
            modalities: modalities.iter().map(|m| m.to_string()).collect(),
        };
        LlmClient::new(&cfg).unwrap()
    }

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

    /// The head as an object, with the array it opens left empty: a test of the
    /// envelope needs no transcript.
    fn head(
        client: &LlmClient,
        instructions: &str,
        note: Option<&str>,
        tools: &[ToolDefinition],
        effort: Option<&str>,
    ) -> Value {
        let mut text = String::from_utf8(client.build_head(instructions, note, tools, effort))
            .expect("a head is built from text");
        // Cut at the marker: a note is written after it.
        let at = text
            .find(",\"input\":[")
            .expect("a head ends by opening its input array");
        text.truncate(at);
        text.push('}');
        serde_json::from_str(&text).expect("the head is one JSON object")
    }

    /// The tool schema is out of band, so a fold cannot drop it -- and this
    /// pins that every request still carries it.
    #[test]
    fn every_request_carries_the_tool_schema() {
        let client = client_with("m", &[], &[]);
        let params = serde_json::json!({"type": "object", "properties": {"task": {"type": "string"}}, "required": ["cmd"]});
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "run".into(),
            parameters: params,
        }];

        // A folded prefix must not change what the model is offered: the
        // schema is out of band, not part of the history.
        for handover in [None, Some("earlier work, folded out")] {
            let body = head(&client, "instructions", handover, &tools, None);
            let sent = &body["tools"][0];
            assert_eq!(sent["name"], "bash");
            assert_eq!(sent["parameters"]["required"][0], "cmd");
            assert!(sent["parameters"]["properties"].get("task").is_some());
        }

        // An empty registry means no `tools` key at all, rather than an empty
        // array that some providers reject.
        let body = head(&client, "instructions", None, &[], None);
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
            &mut Outbox::new(&tx),
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
                &mut Outbox::new(&tx),
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
            &mut Outbox::new(&tx),
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
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.reasoning_summary_text.delta",
                json!({"item_id": "rs_1", "delta": "from deltas"}),
            ),
            &mut items,
            &mut Outbox::new(&tx),
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
            &mut Outbox::new(&tx),
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
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.reasoning_summary_text.delta",
                json!({"item_id": "rs_1", "delta": "out loud"}),
            ),
            &mut items,
            &mut Outbox::new(&tx),
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
            &mut Outbox::new(&tx),
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

    /// Provider-side work is a response even when there is no assistant text.
    #[test]
    fn provider_activity_counts_as_produced() {
        let builtin = LlmEvent::BuiltinCall {
            item_id: "ws_1".into(),
            item: json!({"id": "ws_1", "type": "web_search_call"}),
            replayable: true,
        };
        let activity = LlmEvent::PartStart {
            part: ResponsePart::Activity,
            item_id: "ws_1".into(),
        };
        assert!(event_produced(&builtin));
        assert!(event_produced(&activity));
        assert!(!retryable(0, 0, event_produced(&builtin)));
        assert!(!retryable(0, 0, event_produced(&activity)));
        assert!(!event_produced(&LlmEvent::Completed { usage: None }));
    }

    /// A refusal is not a bad minute: resending it forever is what keeps the
    /// reason from ever reaching an operator.
    #[test]
    fn a_refused_request_is_not_retried() {
        for status in [400, 401, 403, 404, 413, 422] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(!status_retryable(status), "{status} is a refusal");
        }
        // Timing, not the request: worth another attempt.
        for status in [408, 429, 500, 502, 503, 504] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(status_retryable(status), "{status} is a bad minute");
        }
    }

    /// A retry always names a cause, and the first attempt's own words win.
    #[test]
    fn a_retry_names_the_previous_attempt() {
        let said = retry_reason(&Some(("HTTP 500: boom".into(), true)), false, false);
        assert_eq!(said, "HTTP 500: boom");
        // Acknowledged, then silent.
        assert!(retry_reason(&None, true, false).contains("no content"));
        // Spoke, then the stream stopped.
        assert!(retry_reason(&None, false, true).contains("before the response completed"));
        assert!(retry_reason(&None, false, false).contains("nothing in it"));
    }

    /// A streamed error decides by its kind: an unknown one keeps the try it
    /// might deserve.
    #[test]
    fn a_streamed_error_decides_by_its_kind() {
        let refusal = json!({"error": {"type": "invalid_request_error"}});
        assert!(!error_retryable(&refusal));
        let too_long = json!({"error": {"code": "context_length_exceeded"}});
        assert!(!error_retryable(&too_long));
        let bad_minute = json!({"error": {"type": "server_error"}});
        assert!(error_retryable(&bad_minute));
        let unknown = json!({"error": {"type": "invented-by-a-gateway"}});
        assert!(error_retryable(&unknown));
    }

    /// Both Responses envelopes keep the provider's message and classify a
    /// permanent failure by `type` or `code`.
    #[tokio::test]
    async fn streamed_failures_read_both_error_envelopes() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut items = HashMap::new();
        let cases = [
            (
                "response.failed",
                json!({"response": {"error": {
                    "message": "bad request",
                    "type": "invalid_request_error",
                    "code": null
                }}}),
                "bad request",
                false,
            ),
            (
                "response.failed",
                json!({"response": {"error": {
                    "message": "too many tokens",
                    "type": null,
                    "code": "context_length_exceeded"
                }}}),
                "too many tokens",
                false,
            ),
            (
                "error",
                json!({"error": {
                    "message": "gateway unavailable",
                    "type": "server_error",
                    "code": null
                }}),
                "gateway unavailable",
                true,
            ),
        ];
        for (name, data, expected_message, expected_retryable) in cases {
            handle(
                SseEvent {
                    name: name.into(),
                    data,
                },
                &mut items,
                &mut Outbox::new(&tx),
            )
            .await
            .unwrap();
            match rx.recv().await.expect("a failure event") {
                LlmEvent::Failed { message, retryable } => {
                    assert_eq!(message, expected_message);
                    assert_eq!(retryable, expected_retryable);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// The provider says "too long" and stops; the command that fixes it is the
    /// part only this side knows.
    #[test]
    fn a_size_refusal_says_how_to_fold() {
        let prompts = crate::prompts::Prompts::of(&[(
            "size_hint",
            "{{message}} -- fold it: `transcript.py collapse --session main`",
        )]);
        let said = with_size_hint(
            &prompts,
            "HTTP 400 Bad Request: This model's maximum context length is 131072 tokens"
                .to_string(),
        );
        assert!(said.contains("transcript.py collapse"), "{said}");
        assert!(said.starts_with("HTTP 400 Bad Request"), "{said}");
        // Anything else keeps the provider's words and nothing more.
        let other = with_size_hint(
            &prompts,
            "HTTP 400 Bad Request: unknown parameter".to_string(),
        );
        assert_eq!(other, "HTTP 400 Bad Request: unknown parameter");
    }

    /// Which optional keys the provider chose to send must not decide whether the
    /// cached count is read.
    #[test]
    fn usage_keeps_what_the_provider_cached() {
        let warm: Usage = serde_json::from_value(json!({
            "input_tokens": 41200,
            "input_tokens_details": {"cached_tokens": 38912},
            "output_tokens": 512,
            "total_tokens": 41712,
        }))
        .unwrap();
        assert_eq!(warm.cached_tokens(), Some(38912));
        assert_eq!(warm.input_tokens, Some(41200));

        // Providers omit the details when nothing is cached: "not said" is not a zero.
        let cold: Usage = serde_json::from_value(json!({"input_tokens": 10})).unwrap_or_default();
        assert_eq!(cold.cached_tokens(), None);
        assert_eq!(cold.input_tokens, Some(10));
    }

    /// `--thinking_effort` rides on the request as the API spells it; unset sends
    /// no `reasoning` object at all.
    #[test]
    fn a_thinking_level_rides_on_the_request() {
        let client = client_with("m", &[], &[]);
        let body = head(&client, "instructions", None, &[], Some("high"));
        assert_eq!(body["reasoning"]["effort"], "high");
        let body = head(&client, "instructions", None, &[], None);
        assert!(
            body.get("reasoning").is_none(),
            "an unset level must not become an empty reasoning object"
        );
    }

    /// What this harness can run and what the provider runs share one array, in
    /// the roster's own spelling for the latter.
    #[test]
    fn builtin_tools_share_the_array_with_functions() {
        let client = client_with("m", &["web_search", "web_fetch"], &[]);
        let tools = vec![ToolDefinition {
            name: "bash".into(),
            description: "run".into(),
            parameters: json!({"type": "object"}),
        }];
        let body = head(&client, "instructions", None, &tools, None);
        let offered = body["tools"].as_array().expect("tools");
        assert_eq!(offered.len(), 3, "{body}");
        assert_eq!(offered[0]["type"], "function");
        assert_eq!(offered[0]["name"], "bash");
        assert_eq!(offered[1], json!({"type": "web_search"}));
        assert_eq!(offered[2], json!({"type": "web_fetch"}));
    }

    /// Undeclared means unsent: offering a tool a provider does not know is a 400.
    #[test]
    fn a_model_that_declares_no_builtin_tools_sends_none() {
        let client = client_with("m", &[], &[]);
        let body = head(&client, "instructions", None, &[], None);
        assert!(
            body.get("tools").is_none(),
            "an empty tools array is not the same as no tools key: {body}"
        );
        // Only a builtin tool, no local ones, still sends the key.
        let client = client_with("m", &["web_search"], &[]);
        let body = head(&client, "instructions", None, &[], None);
        assert_eq!(body["tools"], json!([{"type": "web_search"}]));
    }

    /// A provider tool item is visible, and replayable behind the line it drew.
    #[tokio::test]
    async fn a_provider_tool_item_is_parsed_not_swallowed() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut items = HashMap::new();
        let ev = |name: &str, data: Value| SseEvent {
            name: name.to_string(),
            data,
        };
        handle(
            ev(
                "response.output_item.added",
                json!({"item": {"id": "ws_1", "type": "web_search_call", "status": "in_progress"}}),
            ),
            &mut items,
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.web_search_call.searching",
                json!({"item_id": "ws_1"}),
            ),
            &mut items,
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();
        handle(
            ev(
                "response.output_item.done",
                json!({"item": {
                    "id": "ws_1",
                    "type": "web_search_call",
                    "status": "completed",
                    "action": {"type": "search", "query": "rust async book"}
                }}),
            ),
            &mut items,
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();

        let mut line = String::new();
        let mut parts = Vec::new();
        let mut replay = None;
        while let Ok(event) = rx.try_recv() {
            match event {
                LlmEvent::PartStart { part, .. } => parts.push(part),
                LlmEvent::Delta { part, text, .. } => {
                    assert_eq!(part, ResponsePart::Activity);
                    line.push_str(&text);
                }
                LlmEvent::BuiltinCall {
                    item, replayable, ..
                } => replay = Some((item, replayable)),
                LlmEvent::PartDone { part, .. } => parts.push(part),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(parts, vec![ResponsePart::Activity, ResponsePart::Activity]);
        assert!(
            line.contains("web_search") && line.contains("rust async book"),
            "the line has to say what happened: {line:?}"
        );
        let (item, replayable) = replay.expect("the item is kept for the next request");
        assert!(replayable);
        assert_eq!(item["type"], "web_search_call");
        assert_eq!(item["action"]["query"], "rust async book");
    }

    /// A provider-named tool nobody has heard of is still an activity.
    #[test]
    fn the_suffix_not_a_list_of_names_decides_what_is_a_provider_tool() {
        assert!(is_provider_call("web_search_call"));
        assert!(is_provider_call("web_fetch_call"));
        assert!(is_provider_call("some_vendors_own_call"));
        assert!(!is_provider_call("function_call"), "that one is ours");
        assert!(!is_provider_call("message"));
        assert!(!is_provider_call("reasoning"));
        assert_eq!(activity_name("web_search_call"), "web_search");
    }

    /// A source is worth a line: a search whose citations nobody sees is unverifiable.
    #[tokio::test]
    async fn a_citation_names_the_source() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut items = HashMap::new();
        handle(
            SseEvent {
                name: "response.output_text.annotation.added".into(),
                data: json!({
                    "item_id": "msg_1",
                    "annotation": {
                        "type": "url_citation",
                        "url": "https://example.org/async",
                        "title": "Async Rust",
                        "start_index": 0,
                        "end_index": 12
                    }
                }),
            },
            &mut items,
            &mut Outbox::new(&tx),
        )
        .await
        .unwrap();
        match rx.try_recv().expect("a citation event") {
            LlmEvent::Citation { title, url, .. } => {
                assert_eq!(title, "Async Rust");
                assert_eq!(url, "https://example.org/async");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A generated picture round-trips through the file beside the home.
    #[test]
    fn a_provider_payload_round_trips_through_the_blob_directory() {
        let dir = tempfile::tempdir().unwrap();
        let raw = json!({
            "id": "ig_1",
            "type": "image_generation_call",
            "result": "iVBORw0KGgoAAAANSUhEUg",
        });
        let (stored, replayable) = storable_activity_with(dir.path(), &raw);
        assert!(replayable, "the file was written: {stored}");
        assert!(stored.get("result").is_none(), "no base64 in the row");
        assert_eq!(stored["result_blob"], json!("ig_1"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        let back = sendable_activity(&stored, replayable, Some(dir.path()))
            .expect("the item is sendable again");
        assert_eq!(back, raw, "byte for byte as the provider sent it");

        // No file, no item -- not a half one.
        assert!(
            sendable_activity(&stored, replayable, None).is_none(),
            "no directory, no payload"
        );
    }

    /// The same, with the directory named rather than the process's.
    fn storable_activity_with(dir: &std::path::Path, item: &Value) -> (Value, bool) {
        let mut item = item.clone();
        let Some(payload) = item.as_object_mut().and_then(|o| o.remove("result")) else {
            return (item, true);
        };
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        crate::blobs::put(dir, id, &payload.to_string()).unwrap();
        item["result_blob"] = json!(id);
        (item, true)
    }

    /// The item goes back exactly as it came: it was executed at the provider, so
    /// there is no local output to pair it with.
    #[test]
    fn an_activity_item_replays_exactly_as_it_came() {
        let item = json!({
            "id": "ws_1",
            "type": "web_search_call",
            "status": "completed",
            "action": {"type": "search", "query": "q"}
        });
        let replayed = sendable_activity(&item, true, None).expect("replayed");
        assert_eq!(replayed, item);
        // One that lost its payload with nowhere to read it back from is not a
        // shape the API takes back.
        assert!(sendable_activity(&item, false, None).is_none());
    }

    /// Base64 does not go into the transcript, so an item that had it is stored
    /// without it and never sent back.
    #[test]
    fn an_activity_item_with_a_binary_payload_is_kept_but_not_replayed() {
        let raw = json!({
            "id": "ig_1",
            "type": "image_generation_call",
            "status": "completed",
            "result": "iVBORw0KGgoAAAANSUhEUg=="
        });
        let (stored, replayable) = storable_activity(&raw);
        assert!(!replayable);
        assert!(
            stored.get("result").is_none(),
            "the payload must not reach sqlite: {stored}"
        );
        assert_eq!(stored["id"], "ig_1");
        // A web search carries no payload and stays replayable.
        let (_, replayable) = storable_activity(&json!({"type": "web_search_call"}));
        assert!(replayable);
    }

    /// The only encoding `image_url` takes for a data URL.

    /// The roster's `modalities` is the whole of the image question.
    #[test]
    fn a_text_only_model_reports_why_it_cannot_look() {
        let prompts = crate::prompts::Prompts::of(&[(
            "image_refusal",
            "{{model}} takes only {{modalities}}",
        )]);
        let blind = client_with("roster/text-only", &[], &["text"]);
        assert!(!blind.accepts_image());
        let vision = blind.vision(&prompts);
        assert!(!vision.accepts_image);
        assert!(vision.refusal.contains("roster/text-only"), "{vision:?}");
        assert!(vision.refusal.contains("\"text\""), "{vision:?}");

        let seeing = client_with("roster/sees", &[], &["text", "image"]);
        assert!(seeing.accepts_image());
        // Unstated is not a refusal: the roster having no opinion is not a no.
        assert!(client_with("m", &[], &[]).accepts_image());
    }
}
