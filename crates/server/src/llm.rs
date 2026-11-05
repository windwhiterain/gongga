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

/// A connect this slow is a broken route, not a slow provider.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The name this harness gives a provider: every request's `User-Agent`. See
/// `docs/notes/server-llm.md`.
const HARNESS_AGENT: &str = concat!("gongga-harness/", env!("CARGO_PKG_VERSION"));

/// The request shape a provider is spoken in, named by the roster's endpoint.
///
/// # Invariant
///
/// One shape per request, decided before the body is built: the two disagree about
/// where the tool list goes, what a tool result looks like, and where usage is
/// reported, so a request cannot be half of each. See `docs/notes/server-anthropic.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Responses,
    Anthropic,
}

impl Face {
    /// The face an endpoint's `api` label names; anything else is the Responses API.
    pub fn of(api: &str) -> Self {
        match api {
            "anthropic-messages" => Face::Anthropic,
            _ => Face::Responses,
        }
    }

    /// The last segment of the URL this face appends to a provider's base.
    fn path(self) -> &'static str {
        match self {
            Face::Responses => "/responses",
            Face::Anthropic => crate::anthropic::PATH,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    /// Which shape this provider is spoken in.
    face: Face,
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    /// The model as a caller names it, `provider/model_name`.
    model: String,
    /// The name the API takes in its `model` field.
    model_name: String,
    /// Where a token measurement is recorded, when the caller has one.
    measure: Option<crate::store::Store>,
    /// The roster provider this client talks to: SPEC's `active_quota` row.
    provider: Option<String>,
    temperature: Option<f32>,
    /// The largest answer this model may give; this face requires the number.
    max_output_tokens: u32,
    store: bool,
    /// How long the provider may take to send response headers: not about the body,
    /// which only [`Self::idle_timeout`] bounds.
    headers_timeout: Duration,
    /// Silence of *content* that counts as a broken stream: keep-alives do not move it.
    idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response.
    max_retries: u32,
    /// First retry delay; doubled per attempt up to `retry_ceiling`.
    retry_base: Duration,
    /// The longest one retry delay may grow to.
    retry_ceiling: Duration,
    /// Provider-side tools, in the provider's own spelling; sent verbatim.
    builtin_tools: Vec<String>,
    /// What the roster says this model takes in; empty is "unstated".
    modalities: Vec<String>,
    /// The cache tier to ask for, already checked against the roster's tiers.
    ///
    /// # Invariant
    ///
    /// `None` unless the roster states this provider can be asked for that tier:
    /// a field the endpoint ignores would price the plan for the wrong tier.
    cache_tier: Option<String>,
    /// Route affinity, off unless a machine states its endpoint accepts it.
    cache_affinity: bool,
    /// Pin the inference instance with an `X-Session-ID` header.
    cache_session_header: bool,
    /// Tiers the roster names; the last check before a directive goes on the wire.
    cache_tiers: Vec<String>,
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
    /// Where the pictures are kept: owned, because the body outlives the call.
    pub store: crate::store::Store,
}

impl RequestSource {
    /// Opens the window as a body; `uploader` is the only handle it may use.
    fn open(
        &self,
        face: Face,
        head: &[u8],
        uploader: std::sync::Arc<Uploader>,
    ) -> anyhow::Result<(
        crate::body::RequestBody,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    )> {
        let rows =
            crate::transcript::Transcript::open(&self.db_path, self.session_id, self.upto_seq)?;
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let files = crate::image::files::Files::new(self.store.clone(), uploader);
        let body = crate::body::RequestBody::new(face, head.to_vec(), rows, files, counter.clone());
        Ok((body, counter))
    }
}

impl LlmClient {
    pub fn new(cfg: &crate::config::ModelConfig) -> anyhow::Result<Self> {
        if cfg.api_key.is_empty() {
            anyhow::bail!(
                "no API key: set {} (or GONGGA_API_KEY) before starting the server",
                cfg.api_key_env
            );
        }
        let face = Face::of(&cfg.api);
        // This face requires it, and a guessed answer length is nobody's budget.
        let max_output_tokens = match cfg.max_output_tokens {
            Some(tokens) => tokens,
            None if face == Face::Anthropic => anyhow::bail!(
                "the anthropic-messages face requires max_tokens: state \"max_output_tokens\" \
                 in the roster's model entry, or \"[model] max_output_tokens\" in this home"
            ),
            None => 0,
        };
        // No client-wide deadline on purpose: three bounds know what they bound.
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(HARNESS_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Ok(Self {
            face,
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model(),
            model_name: cfg.model_name.clone(),
            measure: cfg.measure.clone(),
            provider: cfg.provider.clone(),
            temperature: cfg.temperature,
            max_output_tokens,
            store: cfg.store,
            headers_timeout: cfg.headers_timeout,
            idle_timeout: cfg.idle_timeout,
            max_retries: cfg.max_retries,
            retry_base: cfg.retry_base,
            retry_ceiling: cfg.retry_ceiling,
            builtin_tools: cfg.builtin_tools.clone(),
            modalities: cfg.modalities.clone(),
            cache_tier: cfg
                .cache_tier
                .clone()
                .filter(|tier| cfg.cache_tiers.contains_key(tier)),
            cache_affinity: cfg.cache_affinity,
            cache_session_header: cfg.cache_session_header,
            cache_tiers: cfg.cache_tiers.keys().cloned().collect(),
        })
    }

    /// The model this client asks for, `provider/model_name`; logged at the start
    /// of every turn.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The roster provider behind this client, when the roster named one.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Whether the roster says this model can look at a picture.
    pub fn accepts_image(&self) -> bool {
        crate::config::accepts_image(&self.modalities)
    }

    /// The cache tier this request will ask for, when one was configured and the
    /// roster says the provider offers it.
    pub fn cache_tier(&self) -> Option<&str> {
        self.cache_tier.as_deref()
    }

    /// The Files API for this connection: what a request body uploads its pictures to.
    pub fn uploader(&self) -> std::sync::Arc<Uploader> {
        std::sync::Arc::new(Uploader {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
        })
    }

    /// What this turn's model can see, with the refusal rendered from `prompts`.
    pub fn vision(&self, prompts: &crate::prompts::Prompts) -> Vision {
        Vision {
            accepts_image: self.accepts_image(),
            refusal: crate::config::image_refusal(prompts, &self.model, &self.modalities),
        }
    }

    /// Streams one model request.
    ///
    /// # Invariant
    ///
    /// A failed, timed-out or empty attempt is retried with a doubling delay, so the
    /// caller sees one stream; only the provider saying the request is wrong ends it.
    /// See `docs/notes/server-llm.md`.
    pub fn stream(
        &self,
        instructions: &str,
        index: Option<&str>,
        tail: Option<&str>,
        source: &RequestSource,
        affinity: &str,
        tools: &[ToolDefinition],
        effort: Option<&str>,
        cache_tier: Option<&str>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<LlmEvent> {
        let (tx, rx) = mpsc::channel(256);
        let build_started = Instant::now();
        let head = self.build_head(
            instructions,
            index,
            tail,
            affinity,
            tools,
            effort,
            cache_tier,
        );
        let session_header = self.cache_session_header.then(|| affinity.to_string());
        // Owned, because the attempt below records it after this body is gone.
        let effort_sent = effort.map(str::to_string);
        let build_head_ms = build_started.elapsed().as_millis() as u64;
        let head_bytes = head.len();
        let source = source.clone();
        let url = format!("{}{}", self.base_url, self.face.path());
        let face = self.face;
        let api_key = self.api_key.clone();
        let http = self.http.clone();
        // The measurement's own record: which model, and where to learn from.
        let self_model = self.model_name.clone();
        let self_measure = self.measure.clone();
        let headers_timeout = self.headers_timeout;
        let idle_timeout = self.idle_timeout;
        let max_retries = self.max_retries;
        // The one handle a body may use to reach a provider: which endpoint a picture
        // goes to, and nothing about the model.
        let uploader = self.uploader();
        let retry_base = self.retry_base;
        let retry_ceiling = self.retry_ceiling;

        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            // A dead file id is answered once by forgetting it, not through `retryable`.
            let mut recovered_file = false;
            loop {
                // Opened again for every attempt: a body is read once, and a retry
                // has to send what the attempt before it sent.
                let open_started = Instant::now();
                let opened = source.open(face, &head, uploader.clone());
                let transcript_open_ms = open_started.elapsed().as_millis() as u64;
                let mut probe =
                    crate::probe::AttemptProbe::new(build_head_ms, transcript_open_ms, head_bytes);
                probe.effort(effort_sent.as_deref());
                let (body, bytes) = match opened {
                    Ok(pair) => pair,
                    Err(e) => {
                        probe.outcome("transcript_open_failed");
                        let _ = tx.send(LlmEvent::Attempt { probe }).await;
                        let _ = tx
                            .send(LlmEvent::Failed {
                                message: format!("could not read the transcript: {e}"),
                                retryable: false,
                            })
                            .await;
                        return;
                    }
                };
                // The same counter the body writes into, kept here to measure against.
                let sent = bytes.clone();
                let mut once = stream_once(
                    face,
                    &http,
                    &url,
                    &api_key,
                    body,
                    bytes,
                    headers_timeout,
                    idle_timeout,
                    cancel.clone(),
                    probe,
                    session_header.clone(),
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
                        LlmEvent::Completed { usage } => {
                            completed = true;
                            // What the provider measured against what was written: the
                            // only honest source for "how many tokens is an item".
                            if let (Some(tokens), Some(store)) = (
                                usage.as_ref().and_then(|u| u.input_tokens),
                                self_measure.as_ref(),
                            ) {
                                let written =
                                    sent.load(std::sync::atomic::Ordering::Relaxed) as u64;
                                if let Err(e) = store.note_token_ratio(&self_model, written, tokens)
                                {
                                    tracing::debug!(error = %e, "could not record the token ratio");
                                }
                            }
                        }
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
                // A file the provider no longer holds is forgotten and the body rebuilt.
                if !recovered_file
                    && failure
                        .as_ref()
                        .is_some_and(|(message, _)| dead_file(message))
                {
                    recovered_file = true;
                    let forgotten =
                        crate::image::uploads::forget_scope(&source.store, &uploader.scope())
                            .unwrap_or(0);
                    tracing::warn!(
                        forgotten,
                        "the provider refused a picture's file; its ids were forgotten and \
                         the request is sent again"
                    );
                    continue;
                }
                // Empty completion or cut-short stream: both are retryable, so
                // long as nothing said this attempt can never work.
                let refused = failure.as_ref().is_some_and(|(_, again)| !*again);
                if refused || !retryable(attempt, max_retries) || cancel.is_cancelled() {
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
                let delay = backoff(retry_base, retry_ceiling, attempt);
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

    /// One request's opening bytes, joined to the transcript by the body.
    ///
    /// # Invariant
    ///
    /// The head and the body are one shape: the head opens the array the body fills,
    /// so a face that opened `"messages"` must be the face that writes messages.
    fn build_head(
        &self,
        instructions: &str,
        index: Option<&str>,
        card: Option<&str>,
        affinity: &str,
        tools: &[ToolDefinition],
        effort: Option<&str>,
        cache_tier: Option<&str>,
    ) -> Vec<u8> {
        // The index joins the prose: both change only when a file does. The card is
        // stamped once, so the two lifetimes differ.
        let mut instructions = instructions.to_string();
        if let Some(index) = index.filter(|index| !index.is_empty()) {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(index);
        }
        if self.face == Face::Anthropic {
            let offered: Vec<Value> = tools
                .iter()
                .map(|t| crate::anthropic::tool(&t.name, &t.description, &t.parameters))
                .collect();
            return crate::anthropic::head(
                &self.model_name,
                self.max_output_tokens,
                &instructions,
                &offered,
                effort,
                self.temperature,
                card,
            );
        }
        let mut body = json!({
            "model": self.model_name,
            "stream": true,
            "store": self.store,
        });
        // Invariant: only a roster-declared tier may be sent; per-request wins.
        let directive = cache_tier
            .or(self.cache_tier.as_deref())
            .filter(|tier| self.cache_tiers.iter().any(|name| name.as_str() == *tier));
        if let Some(tier) = directive {
            body["prompt_cache_options"] = json!({"mode": "implicit", "ttl": tier});
        }
        // Invariant: the key names the unit that shares a prefix, not a conversation.
        if self.cache_affinity {
            body["prompt_cache_key"] = json!(affinity);
        }
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
        if self.max_output_tokens > 0 {
            body["max_output_tokens"] = json!(self.max_output_tokens);
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
        // SPEC's card: one message after the system prompt and before the first user
        // message, so it goes in first.
        if let Some(card) = card.filter(|card| !card.is_empty()) {
            head.push_str(
                &json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": card}],
                })
                .to_string(),
            );
            head.push(',');
        }
        head.into_bytes()
    }
}

/// The part of a connection that files are uploaded to, and deleted from.
///
/// # Invariant
///
/// Separate from `LlmClient` because a request body outlives the call that built it and
/// needs only this: an endpoint, a key, and a client. Handing a body the whole client
/// would hand it a model, a roster entry and a retry policy it must not be free to use.
#[derive(Debug, Clone)]
pub struct Uploader {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Uploader {
    /// Mints a provider-side id for one picture, so the request carries that instead
    /// of the bytes.
    ///
    /// # Invariant
    ///
    /// `path` only names the file the provider will see: what is sent is `bytes`, so a
    /// picture on a machine this server cannot read is uploaded exactly once. The
    /// lifetime is stated, because a provider that is not told keeps the file for ever,
    /// and permanent files are what exhaust an account. See `image::uploads`.
    pub async fn upload(
        &self,
        path: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> anyhow::Result<crate::image::Uploaded> {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes.to_vec())
            .file_name(name)
            .mime_str(media_type)?;
        let form = reqwest::multipart::Form::new()
            .text("purpose", "user_data")
            .text("expires_after[anchor]", "created_at")
            .text(
                "expires_after[seconds]",
                crate::image::uploads::EXPIRES_AFTER_SECS.to_string(),
            )
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
        let id = value["id"].as_str().map(str::to_string).ok_or_else(|| {
            anyhow::anyhow!(
                "{url} returned no file id: {}",
                body.chars().take(200).collect::<String>()
            )
        })?;
        // The provider's own statement, or an assumption counted from now.
        let expires_at = value["expires_at"]
            .as_i64()
            .map(|secs| secs * 1000)
            .unwrap_or_else(|| crate::image::uploads::expiry_assumed(crate::store::now_millis()));
        Ok(crate::image::Uploaded { id, expires_at })
    }

    /// Which provider namespace these files belong to: the endpoint, never the key.
    pub fn scope(&self) -> String {
        crate::image::uploads::scope(&self.base_url)
    }

    /// Deletes one provider file, so a copy that will never be sent again stops
    /// counting against the account.
    pub async fn delete(&self, file_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/files/{file_id}", self.base_url);
        let response = self
            .http
            .delete(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {} from {url}", response.status());
        }
        Ok(())
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
        /// The call this part streams into, when the part is a tool call.
        call_id: Option<String>,
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
        /// The provider's own item, replayed as it came; see `docs/notes/server-llm.md`.
        ///
        /// # Invariant
        ///
        /// Whole, never narrowed: a field dropped here is a field no request can send.
        item: serde_json::Value,
        /// The readable thought, for the reader only.
        summary: String,
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
    /// Aggregated measurements for one HTTP attempt, emitted after its last event.
    Attempt {
        probe: crate::probe::AttemptProbe,
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

/// Any content means this attempt said something; an activity beat counts too.
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

/// Delay before attempt `attempt` (1-based): the base doubled each time, capped
/// at `ceiling`.
///
/// # Invariant
///
/// Doubling is meant to wait out a provider's bad minute, not to add a silent one
/// of our own, so the cap arrives from the config rather than from here: a home
/// that wants to give up sooner says so, and this function has no opinion.
fn backoff(base: Duration, ceiling: Duration, attempt: u32) -> Duration {
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    base.saturating_mul(factor).min(ceiling)
}

/// Whether the provider refused a transcript it wrote without thinking.
pub fn thinking_refusal(message: &str) -> bool {
    message.contains("reasoning_text") && message.contains("thinking mode")
}

/// Whether another attempt is worth making.
///
/// # Invariant
///
/// `max_retries == 0` is no ceiling, and how far an attempt got is not part of
/// the question: a provider that dies mid-answer is the same bad minute as one
/// that dies before its first byte, and the caller drops what the failed attempt
/// wrote before the next one is sent. Only a refusal ends the stream.
///
/// See `docs/notes/server-llm.md`.
fn retryable(attempt: u32, max_retries: u32) -> bool {
    max_retries == 0 || attempt < max_retries
}

/// Whether the provider is refusing a picture's file rather than the request.
///
/// # Invariant
///
/// A file id is the one part of a request that can die without this server being wrong:
/// the provider expires it, deletes it, or another key on the same account does. Treating
/// that as a refusal would leave a transcript that can never be sent again, so it is
/// recognized here by the words a provider uses and handled by forget-and-retry.
fn dead_file(detail: &str) -> bool {
    let said = detail.to_lowercase();
    let file = said.contains("file");
    let gone = said.contains("expired")
        || said.contains("not found")
        || said.contains("does not exist")
        || said.contains("deleted")
        || said.contains("invalid");
    file && gone
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
    face: Face,
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: crate::body::RequestBody,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    headers_timeout: Duration,
    idle_timeout: Duration,
    cancel: CancellationToken,
    probe: crate::probe::AttemptProbe,
    session_header: Option<String>,
) -> mpsc::Receiver<LlmEvent> {
    let (tx, rx) = mpsc::channel(256);
    let url = url.to_string();
    let api_key = api_key.to_string();
    let http = http.clone();

    tokio::spawn(async move {
        let body = body;
        let outcome = AssertUnwindSafe(read_stream(
            face,
            &http,
            &url,
            &api_key,
            body,
            bytes,
            headers_timeout,
            idle_timeout,
            &cancel,
            &tx,
            probe,
            session_header,
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

async fn read_stream(
    face: Face,
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: crate::body::RequestBody,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    headers_timeout: Duration,
    idle_timeout: Duration,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<LlmEvent>,
    mut probe: crate::probe::AttemptProbe,
    session_header: Option<String>,
) {
    let outcome = AssertUnwindSafe(read_stream_inner(
        face,
        http,
        url,
        api_key,
        body,
        bytes,
        headers_timeout,
        idle_timeout,
        cancel,
        tx,
        &mut probe,
        session_header,
    ))
    .catch_unwind()
    .await;
    match outcome {
        Ok(()) => {
            let _ = tx.send(LlmEvent::Attempt { probe }).await;
        }
        Err(payload) => {
            probe.outcome("panicked");
            let _ = tx
                .send(LlmEvent::Attempt {
                    probe: probe.clone(),
                })
                .await;
            std::panic::resume_unwind(payload);
        }
    }
}

/// Reads the response while the outer attempt owns its final measurement event.
async fn read_stream_inner(
    face: Face,
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: crate::body::RequestBody,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    headers_timeout: Duration,
    idle_timeout: Duration,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<LlmEvent>,
    probe: &mut crate::probe::AttemptProbe,
    session_header: Option<String>,
) {
    let started = Instant::now();
    tracing::info!(%url, "model request: sending");
    // The count is known only afterwards. The type must be stated: a provider
    // answers 415 for a streamed body without it.
    let mut request = http
        .post(url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .bearer_auth(api_key);
    if let Some(session) = session_header {
        request = request.header("X-Session-ID", session);
    }
    let request = request.body(reqwest::Body::wrap_stream(body));

    let response = match tokio::time::timeout(headers_timeout, request.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            probe.outcome("send_failed");
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
            probe.outcome("headers_timeout");
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
    probe.headers(
        response.status(),
        bytes.load(std::sync::atomic::Ordering::Relaxed),
    );
    tracing::info!(
        status = %response.status(),
        bytes = bytes.load(std::sync::atomic::Ordering::Relaxed),
        elapsed = ?started.elapsed(),
        "model request: headers"
    );
    if !response.status().is_success() {
        probe.outcome("http_error");
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
    // The other face keeps its own running state; only one of the two is ever read.
    let mut face_state = crate::anthropic::Stream::default();
    let mut out = Outbox::new(tx);
    // Silence of content is failure: not "time since the request started", which would
    // cut a long answer mid-sentence.
    let idle = tokio::time::sleep(idle_timeout);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                probe.outcome("cancelled");
                let _ = out.send(LlmEvent::Cancelled).await;
                return;
            }
            _ = &mut idle => {
                probe.outcome("idle_timeout");
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
                        probe.chunk(bytes.len());
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(block) = next_sse_block(&mut buf) {
                            let process_started = Instant::now();
                            if let Some(event) = parse_sse_block(&block) {
                                probe.event(&event.name, &event.data, false);
                                if completes(face, &event.name) {
                                    probe.outcome("completed");
                                }
                                let result = match face {
                                    Face::Responses => handle(event, &mut items, &mut out).await,
                                    Face::Anthropic => face_state.handle(event, &mut out).await,
                                };
                                probe.process(process_started.elapsed());
                                if let Err(e) = result {
                                    probe.outcome("closed_early");
                                    tracing::debug!(error = %e, "llm stream closed early");
                                    return;
                                }
                            } else {
                                probe.process(process_started.elapsed());
                            }
                        }
                        // Only content moves the clock; a keep-alive said nothing.
                        if out.take_content() {
                            idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        }
                    }
                    Err(e) => {
                        probe.outcome("stream_error");
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
        let process_started = Instant::now();
        if let Some(event) = parse_sse_block(tail) {
            probe.event(&event.name, &event.data, true);
            if completes(face, &event.name) {
                probe.outcome("completed");
            }
            let result = match face {
                Face::Responses => handle(event, &mut items, &mut out).await,
                Face::Anthropic => face_state.handle(event, &mut out).await,
            };
            probe.process(process_started.elapsed());
            let _ = result;
        } else {
            probe.process(process_started.elapsed());
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
    /// Often zero even on a cold write; see `docs/notes/server-llm.md`.
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
}

impl Usage {
    /// Tokens billed at the cache rate, when the provider says: `None` is it
    /// declining to say, not a miss.
    pub fn cached_tokens(&self) -> Option<u64> {
        self.input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
    }

    /// Tokens billed at the fresh rate: the part a write is charged on.
    ///
    /// # Invariant
    ///
    /// Both counts must come from the same answer; a provider that reports one
    /// and not the other gives no measurement rather than half of one.
    pub fn uncached_tokens(&self) -> Option<u64> {
        Some(self.input_tokens?.saturating_sub(self.cached_tokens()?))
    }
}

#[derive(Debug, Clone, Default)]
struct ItemMeta {
    kind: ItemKind,
    name: String,
    call_id: String,
    /// Text accumulated from deltas, the only place a readable summary exists.
    text: String,
    /// Whether the call went out: providers differ on the event that ends it.
    called: bool,
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

/// Whether this event ends a response, in the face that is being read.
fn completes(face: Face, name: &str) -> bool {
    match face {
        Face::Responses => name == "response.completed",
        Face::Anthropic => crate::anthropic::completes(name),
    }
}

pub(crate) struct SseEvent {
    pub(crate) name: String,
    pub(crate) data: Value,
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
    if data == "[DONE]" {
        return Some(SseEvent {
            name: "[DONE]".to_string(),
            data: Value::Null,
        });
    }
    let parsed = serde_json::from_str(&data).unwrap_or(Value::Null);
    // SSE's `event:` is optional; Responses frames name themselves in `type`.
    if name.is_empty() {
        if let Some(t) = parsed.get("type").and_then(Value::as_str) {
            name = t.to_string();
        }
    }
    Some(SseEvent { name, data: parsed })
}

/// The arguments a provider sent, as JSON a later request can carry back.
///
/// # Invariant
///
/// Always parses: a payload no rule fixes becomes `{}`. Providers do emit
/// arguments that are not JSON -- bare values, a trailing comma, a tail cut off
/// mid-key -- and one that cannot read its own output rejects the whole next
/// request. Only the cheapest rule that parses wins.
pub(crate) fn repair_arguments(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "{}".to_string();
    }
    if serde_json::from_str::<Value>(raw).is_ok() {
        return raw.to_string();
    }
    let quoted = quote_bare_values(raw);
    let mut candidates = vec![
        drop_trailing_commas(raw),
        quoted.clone(),
        drop_trailing_commas(&quoted),
    ];
    candidates.extend(cut_incomplete_tail(&quoted));
    for candidate in candidates {
        if serde_json::from_str::<Value>(&candidate).is_ok() {
            return candidate;
        }
    }
    "{}".to_string()
}

/// The same text with commas before `}` or `]` removed, outside strings only.
fn drop_trailing_commas(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut in_str = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 2;
                    continue;
                }
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The same text with a bare value after `:` wrapped in quotes, outside strings.
fn quote_bare_values(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len() + 16);
    let mut in_str = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 2;
                    continue;
                }
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            ':' => {
                out.push(c);
                i += 1;
                while i < chars.len() && chars[i].is_whitespace() {
                    out.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() && !starts_json_value(&chars[i..]) {
                    let start = i;
                    while i < chars.len() && !matches!(chars[i], ',' | '}' | ']' | '\n' | '\r') {
                        i += 1;
                    }
                    let token: String = chars[start..i].iter().collect();
                    out.push('"');
                    for ch in token.trim().chars() {
                        match ch {
                            '"' => out.push_str("\\\""),
                            '\\' => out.push_str("\\\\"),
                            _ => out.push(ch),
                        }
                    }
                    out.push('"');
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Whether the text at `rest` already begins a JSON value, so no quote is added.
fn starts_json_value(rest: &[char]) -> bool {
    match rest.first() {
        Some('"' | '{' | '[' | '-' | '0'..='9') => true,
        Some(_) => ["true", "false", "null"].iter().any(|word| {
            let w: Vec<char> = word.chars().collect();
            rest.len() >= w.len() && rest[..w.len()] == w[..]
        }),
        None => true,
    }
}

/// The text cut back to its last complete pair and closed, when the tail is partial.
fn cut_incomplete_tail(raw: &str) -> Option<String> {
    if !raw.trim_start().starts_with('{') {
        return None;
    }
    let mut end = raw.len();
    while let Some(at) = raw[..end].rfind(',') {
        let candidate = format!("{}}}", raw[..at].trim_end());
        if serde_json::from_str::<Value>(&candidate).is_ok() {
            return Some(candidate);
        }
        end = at;
    }
    None
}

/// `response.<type>.<phase>` for a provider tool's own progress beats.
fn provider_phase(event_name: &str) -> Option<(&str, &str)> {
    let rest = event_name.strip_prefix("response.")?;
    let (item_type, phase) = rest.split_once('.')?;
    is_provider_call(item_type).then_some((item_type, phase))
}

/// The event channel, plus whether content went out: what the idle clock measures.
pub(crate) struct Outbox<'a> {
    tx: &'a mpsc::Sender<LlmEvent>,
    content: bool,
}

impl<'a> Outbox<'a> {
    pub(crate) fn new(tx: &'a mpsc::Sender<LlmEvent>) -> Self {
        Self { tx, content: false }
    }

    pub(crate) async fn send(
        &mut self,
        event: LlmEvent,
    ) -> Result<(), mpsc::error::SendError<LlmEvent>> {
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
                    called: false,
                },
            );
            match kind {
                ItemKind::Reasoning => {
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::Reasoning,
                        item_id: id,
                        call_id: None,
                    })
                    .await?;
                }
                ItemKind::Message => {
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::Output,
                        item_id: id,
                        call_id: None,
                    })
                    .await?;
                }
                ItemKind::FunctionCall => {
                    // The call streams as it did: `bash(` then its arguments, and
                    // the id its result will carry rides every chunk of it.
                    out.send(LlmEvent::PartStart {
                        part: ResponsePart::ToolCall,
                        item_id: id.clone(),
                        call_id: item["call_id"].as_str().map(str::to_string),
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
                        call_id: None,
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
        // Whole summary at once: for some providers the only copy there is.
        "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let full = d["text"].as_str().unwrap_or_default();
            let meta = items.entry(item_id.clone()).or_default();
            let rest = full.strip_prefix(meta.text.as_str()).map(str::to_string);
            if let Some(rest) = rest.filter(|rest| !rest.is_empty()) {
                meta.text.push_str(&rest);
                out.send(LlmEvent::Delta {
                    part: ResponsePart::Reasoning,
                    item_id,
                    text: rest,
                })
                .await?;
            }
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
            let meta = items.entry(item_id.clone()).or_default();
            // Without `output_item.added`, the deltas are where the call appears.
            let opens = meta.kind == ItemKind::Unknown;
            if opens {
                meta.kind = ItemKind::FunctionCall;
            }
            meta.text.push_str(&delta);
            let call_id = opens
                .then(|| meta.call_id.clone())
                .filter(|c| !c.is_empty());
            if opens {
                out.send(LlmEvent::PartStart {
                    part: ResponsePart::ToolCall,
                    item_id: item_id.clone(),
                    call_id,
                })
                .await?;
            }
            out.send(LlmEvent::Delta {
                part: ResponsePart::ToolCall,
                item_id,
                text: delta,
            })
            .await?;
        }
        "response.function_call_arguments.done" => {
            // LongCat ends a call here, with no `output_item.done` and no `call_id`.
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let arguments = repair_arguments(d["arguments"].as_str().unwrap_or("{}"));
            let meta = items.entry(item_id.clone()).or_default();
            if let Some(name) = d["name"].as_str().filter(|n| !n.is_empty()) {
                meta.name = name.to_string();
            }
            meta.kind = ItemKind::FunctionCall;
            let call_id = if meta.call_id.is_empty() {
                item_id.clone()
            } else {
                meta.call_id.clone()
            };
            let name = meta.name.clone();
            let already = meta.called;
            meta.called = true;
            if already {
                return Ok(());
            }
            out.send(LlmEvent::ToolCall {
                item_id: item_id.clone(),
                call_id,
                name,
                arguments,
            })
            .await?;
            out.send(LlmEvent::PartDone {
                part: ResponsePart::ToolCall,
                item_id,
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
                    // The provider's summary wins; the deltas are the fallback. Parts are
                    // paragraphs, so they join with a newline.
                    let mut summary = item["summary"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .or_else(|| item["summary"].as_str().map(str::to_string))
                        .unwrap_or_default();
                    if summary.is_empty() {
                        summary = meta.text.clone();
                    }
                    out.send(LlmEvent::Reasoning {
                        item_id: id.clone(),
                        item: item.clone(),
                        summary,
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
                    // Already closed where `function_call_arguments.done` did it.
                    if !meta.called {
                        // The closing bracket, then the call as the structure it is.
                        let streamed = !meta.text.is_empty();
                        let close = if streamed {
                            ")".to_string()
                        } else {
                            let args = repair_arguments(item["arguments"].as_str().unwrap_or("{}"));
                            format!("{}({args})", meta.name)
                        };
                        out.send(LlmEvent::Delta {
                            part: ResponsePart::ToolCall,
                            item_id: id.clone(),
                            text: close,
                        })
                        .await?;
                        out.send(LlmEvent::ToolCall {
                            item_id: id.clone(),
                            call_id: meta.call_id,
                            name: meta.name,
                            arguments: repair_arguments(item["arguments"].as_str().unwrap_or("{}")),
                        })
                        .await?;
                        out.send(LlmEvent::PartDone {
                            part: ResponsePart::ToolCall,
                            item_id: id,
                        })
                        .await?;
                    }
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
