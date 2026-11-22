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

/// Prefix for the per-conversation id every session-id header carries: the session's
/// stable database id, pure ASCII.
const SESSION_ID_PREFIX: &str = "gongga-";

/// A Responses endpoint that understands the harness's declarative memory view.
pub const MEMORY_MASK_API: &str = "openai-responses-memory-mask";

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
    /// Whether this endpoint accepts `gongga_memory` and per-row `gongga_seq`.
    memory_mask: bool,
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
    /// The full-tape input+output ceiling this model publishes, when stated.
    context: Option<u64>,
    /// The visible input+output ceiling a memory-mask model publishes, when stated.
    unmasked_context: Option<u64>,
    store: bool,
    /// How long the provider may take to send response headers: not about the body,
    /// which only [`Self::idle_timeout`] bounds.
    headers_timeout: Duration,
    /// Silence of *content* that counts as a broken stream: keep-alives do not move it.
    idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response.
    max_retries: u32,
    /// First retry delay; doubled per attempt, with no ceiling.
    retry_base: Duration,
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
    /// Whether to send `prompt_cache_key`, carrying the per-conversation id.
    cache_affinity: bool,
    /// Header names that carry the per-conversation id; empty sends nothing.
    session_id_headers: Vec<String>,
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
/// One original memory span hidden in a pinned view.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MemoryMaskSpan {
    pub memory_id: String,
    pub start_seq: i64,
    pub end_seq: i64,
}

/// The complete memory view a request and every retry must send.
#[derive(Clone, Debug)]
pub struct MemoryView {
    pub upto_seq: i64,
    pub view_generation: u64,
    pub masked: Vec<MemoryMaskSpan>,
}

#[derive(Clone)]
pub struct RequestSource {
    pub db_path: std::path::PathBuf,
    pub session_id: i64,
    pub upto_seq: i64,
    /// Where the pictures are kept: owned, because the body outlives the call.
    pub store: crate::store::Store,
    /// Present only in mask mode, including an empty masked snapshot at generation 0.
    pub memory: Option<MemoryView>,
}

impl RequestSource {
    /// Opens the window as a body; `uploader` is the only handle it may use.
    fn open(
        &self,
        face: Face,
        head: &[u8],
        uploader: std::sync::Arc<Uploader>,
        memory_mask: bool,
    ) -> anyhow::Result<(
        crate::body::RequestBody,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    )> {
        let rows =
            crate::transcript::Transcript::open(&self.db_path, self.session_id, self.upto_seq)?;
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let files = crate::image::files::Files::new(self.store.clone(), uploader);
        let body = crate::body::RequestBody::new(
            face,
            head.to_vec(),
            rows,
            files,
            counter.clone(),
            memory_mask,
        );
        Ok((body, counter))
    }
}

impl LlmClient {
    /// The answer length to ask for: the stated one, never more than the window has left.
    ///
    /// # Invariant
    ///
    /// The provider checks input plus this number against the window, and reserves the
    /// model's whole output when given none: 664,898 in, 384,000 reserved, 322 over a
    /// 1,048,576 window, which is a request that fits refused. The measurement is the
    /// last step's input for this session, so the sum lands inside the window by the
    /// provider's own arithmetic. See `docs/notes/server-llm.md`.
    fn wanted_output(&self, spent: u64) -> u32 {
        let stated = self.max_output_tokens;
        let window = if self.memory_mask {
            self.unmasked_context.or(self.context)
        } else {
            self.context
        };
        let Some(window) = window else {
            return stated;
        };
        // A roster that states no ceiling leaves the number to the provider: capping by
        // what is left would ask for more than the model can answer.
        if stated == 0 || spent == 0 || window <= spent {
            return stated;
        }
        (window - spent).min(u64::from(stated)) as u32
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
            memory_mask: cfg.api == MEMORY_MASK_API && face == Face::Responses,
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model(),
            model_name: cfg.model_name.clone(),
            measure: cfg.measure.clone(),
            provider: cfg.provider.clone(),
            temperature: cfg.temperature,
            max_output_tokens,
            context: cfg.context,
            unmasked_context: cfg.context,
            store: cfg.store,
            headers_timeout: cfg.headers_timeout,
            idle_timeout: cfg.idle_timeout,
            max_retries: cfg.max_retries,
            retry_base: cfg.retry_base,
            builtin_tools: cfg.builtin_tools.clone(),
            modalities: cfg.modalities.clone(),
            cache_tier: cfg
                .cache_tier
                .clone()
                .filter(|tier| cfg.cache_tiers.contains_key(tier)),
            cache_affinity: cfg.cache_affinity,
            session_id_headers: cfg.session_id_headers.clone(),
            cache_tiers: cfg.cache_tiers.keys().cloned().collect(),
        })
    }

    /// Applies the provider's separate visible window after construction.
    pub fn with_unmasked_context(mut self, context: Option<u64>) -> Self {
        self.unmasked_context = context.or(self.context);
        self
    }

    /// The model this client asks for, `provider/model_name`; logged at the start
    /// of every turn.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether this endpoint accepts a full tape plus a declarative memory mask.
    pub fn supports_memory_mask(&self) -> bool {
        self.memory_mask
    }

    /// The visible context window used by mask-mode budgets.
    pub fn unmasked_context(&self) -> Option<u64> {
        self.unmasked_context.or(self.context)
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
    /// The wait for an answer grows the same way: each attempt allows the longer of the
    /// configured wait and twice this session's own longest, and doubles again for
    /// every attempt already spent. See `docs/notes/server-llm.md`.
    pub fn stream(
        &self,
        instructions: &str,
        plan: Option<&str>,
        parameters: Option<&str>,
        tail: Option<&str>,
        source: &RequestSource,
        tools: &Value,
        effort: Option<&str>,
        cache_tier: Option<&str>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<LlmEvent> {
        // SPEC's `active_quota` is ordered by a provider's last request, so the
        // stamp belongs here.
        if let Some(provider) = self.provider() {
            crate::quota::note_request(provider);
        }
        let (tx, rx) = mpsc::channel(256);
        let build_started = Instant::now();
        // One per-conversation id -- the stable database id -- for every affinity field:
        // body key and headers alike.
        let session_id = format!("{SESSION_ID_PREFIX}{}", source.session_id);
        let spent = self
            .measure
            .as_ref()
            .and_then(|store| store.measured_input_tokens(source.session_id).ok())
            .unwrap_or(0);
        let memory_mask = self.memory_mask && source.memory.is_some();
        let head = self.build_head(
            instructions,
            plan,
            parameters,
            tail,
            &session_id,
            tools,
            effort,
            cache_tier,
            spent,
            source.memory.as_ref(),
        );
        let session_headers: Vec<(String, String)> = self
            .session_id_headers
            .iter()
            .map(|name| (name.clone(), session_id.clone()))
            .collect();
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
        // The floor an attempt's own wait is built against.
        let headers_floor = self.headers_timeout;
        let idle_timeout = self.idle_timeout;
        let max_retries = self.max_retries;
        // The one handle a body may use to reach a provider: which endpoint a picture
        // goes to, and nothing about the model.
        let uploader = self.uploader();
        let retry_base = self.retry_base;

        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            // A dead file id is answered once by forgetting it, not through `retryable`.
            let mut recovered_file = false;
            // Counted on its own: a moderation verdict is retried under a small budget.
            let mut moderation_retries: u32 = 0;
            loop {
                // Opened again for every attempt: a body is read once, and a retry
                // has to send what the attempt before it sent.
                let open_started = Instant::now();
                let opened = source.open(face, &head, uploader.clone(), memory_mask);
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
                // Twice this session's longest wait; the retry ladder doubles it again.
                let headers_timeout = protocol::retry::after(
                    headers_floor.max(
                        source
                            .store
                            .answer_headers_ms(source.session_id)
                            .map_or(Duration::ZERO, |ms| {
                                Duration::from_millis(ms.saturating_mul(2))
                            }),
                    ),
                    attempt.saturating_add(1),
                );
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
                    session_headers.clone(),
                )
                .await;
                let mut produced = false;
                let mut completed = false;
                let mut cancelled = false;
                // Held back, not forwarded: if a retry follows, this error is
                // that retry's `reason` rather than the answer.
                let mut failure: Option<(String, bool)> = None;

                // What this attempt saw of the wait.
                let mut answer_wait: Option<u64> = None;
                while let Some(event) = once.recv().await {
                    produced |= event_produced(&event);
                    if let LlmEvent::Attempt { probe } = &event {
                        answer_wait = probe.answer_wait_ms();
                    }
                    match &event {
                        LlmEvent::Completed { usage, .. } => {
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
                // Kept before anything decides what follows: the next attempt reads it.
                if let Some(ms) = answer_wait
                    && let Err(e) = source.store.note_answer_headers_ms(source.session_id, ms)
                {
                    tracing::debug!(error = %e, "could not record the wait for an answer");
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
                // A misfire worth sending again; its budget is its own. `docs/notes/server-llm.md`.
                if let Some((message, again)) = failure.as_mut()
                    && !*again
                    && moderation_retries < MODERATION_RETRIES
                    && moderation_refusal(message)
                {
                    moderation_retries += 1;
                    *again = true;
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
                let delay = protocol::retry::after(retry_base, attempt);
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
                    _ = protocol::retry::sleep(delay) => {}
                }
            }
        });

        rx
    }

    /// The tool block this client sends, rendered once for the session's record.
    ///
    /// # Invariant
    ///
    /// Called only from a stored change event, never while a request is built: the
    /// tool list is part of the context, so it is a row and not a rendering a
    /// request redoes. What it returns is exactly what `build_head` splices, which
    /// is why the two cannot disagree about a schema. See `docs/notes/server-tools.md`.
    pub fn wire_tools(&self, definitions: &[ToolDefinition]) -> Value {
        if self.face == Face::Anthropic {
            return Value::Array(
                definitions
                    .iter()
                    .map(|t| crate::anthropic::tool(&t.name, &t.description, &t.parameters))
                    .collect(),
            );
        }
        let mut offered: Vec<Value> = definitions
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
        Value::Array(offered)
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
        plan: Option<&str>,
        parameters: Option<&str>,
        card: Option<&str>,
        session_id: &str,
        tools: &Value,
        effort: Option<&str>,
        cache_tier: Option<&str>,
        spent: u64,
        memory: Option<&MemoryView>,
    ) -> Vec<u8> {
        // The plan travels as a message, never in the system text.
        // See `docs/reference/prefix-cache.md`.
        let plan = plan.filter(|plan| !plan.is_empty());
        if self.face == Face::Anthropic {
            // The stored row's bytes; a request assembles no part of its head.
            let offered: &[Value] = tools.as_array().map(Vec::as_slice).unwrap_or(&[]);
            return crate::anthropic::head(
                &self.model_name,
                self.max_output_tokens,
                instructions,
                offered,
                effort,
                self.temperature,
                (plan_framing(), plan),
                parameters,
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
        // Invariant: vendors that document the key want session isolation, so its value
        // is the headers' per-conversation id.
        if self.cache_affinity {
            body["prompt_cache_key"] = json!(session_id);
        }
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        // The stored block, verbatim; empty sends no key at all.
        if tools.as_array().is_some_and(|offered| !offered.is_empty()) {
            body["tools"] = tools.clone();
        }
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        // The provider reserves the whole output window unless told otherwise; ask for the rest.
        let wanted = self.wanted_output(spent);
        if wanted > 0 {
            body["max_output_tokens"] = json!(wanted);
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
        if let Some(memory) = memory {
            body["gongga_memory"] = json!({
                "upto_seq": memory.upto_seq,
                "view_generation": memory.view_generation,
                "masked": memory.masked,
            });
        }
        // Reopened at `input`, whose array the body fills as the network asks: the
        // transcript is never one value.
        let mut head = body.to_string();
        head.pop();
        head.push_str(",\"input\":[");
        // The opening and separator come first: the plan is never the first message.
        let framing = plan_framing();
        if !framing.opening.is_empty() {
            head.push_str(
                &json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": framing.opening}],
                })
                .to_string(),
            );
            head.push(',');
        }
        if !framing.separator.is_empty() && plan.is_some() {
            let separator = &framing.separator;
            head.push_str(
                &json!({
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": separator}],
                })
                .to_string(),
            );
            head.push(',');
        }
        if let Some(plan) = plan {
            // This endpoint measures one long `user` turn against a ceiling far below
            // the model's window. Bridging the plan with assistant turns keeps every
            // turn short while sending the same bytes in the same order, and the API
            // requires the last turn to be a user. Splitting is unconditional: it
            // depends only on the plan bytes, never on the model, tools or provider,
            // so swapping any of those cannot change this part of the wire prefix.
            let bridge: &str = if framing.separator.is_empty() {
                "."
            } else {
                &framing.separator
            };
            let turns = split_plan(plan, bridge, PLAN_CHUNK_CHARS);
            for turn in turns {
                match turn {
                    PlanTurn::User(text) => head.push_str(
                        &json!({
                            "role": "user",
                            "content": [{"type": "input_text", "text": text}],
                        })
                        .to_string(),
                    ),
                    PlanTurn::Assistant(text) => head.push_str(
                        &json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        })
                        .to_string(),
                    ),
                }
                head.push(',');
            }
        }
        // Before the card: the same words for every session, the card being what differs.
        if let Some(parameters) = parameters.filter(|parameters| !parameters.is_empty()) {
            head.push_str(
                &json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": parameters}],
                })
                .to_string(),
            );
            head.push(',');
        }
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

/// Largest plan turn, in characters, on the wire.
///
/// # Invariant
///
/// The endpoint mis-reads one long `user` turn against a small ceiling. Chunks stay
/// far below that ceiling even for dense CJK text (~2 tokens/char), and the bytes stay
/// in order: only message boundaries change, never the transcript's content. The cut
/// depends only on the plan bytes, so it is the same for every model, provider and
/// tool set. See the split experiment in `docs/notes/server-llm.md`.
const PLAN_CHUNK_CHARS: usize = 12_000;

/// One turn of a split plan, written into the request body verbatim.
enum PlanTurn<'a> {
    User(&'a str),
    Assistant(&'a str),
}

/// Splits one plan into short `user` turns bridged by `assistant` turns.
///
/// # Invariant
///
/// The concatenation of every `User` slice equals `plan` byte for byte: no text
/// is dropped, reordered or rewritten, and cuts land on UTF-8 character boundaries.
/// A short plan stays one `user` turn with no bridge. The last turn is always a
/// `user`, which the endpoint requires; an `assistant` last turn is refused before
/// the length check is ever reached.
fn split_plan<'a>(plan: &'a str, bridge: &'a str, limit: usize) -> Vec<PlanTurn<'a>> {
    let mut turns = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (byte, _) in plan.char_indices() {
        count += 1;
        if count > limit {
            turns.push(PlanTurn::User(&plan[start..byte]));
            turns.push(PlanTurn::Assistant(bridge));
            start = byte;
            count = 1;
        }
    }
    turns.push(PlanTurn::User(&plan[start..]));
    turns
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

/// When one attempt's stream began to speak, and when it stopped saying anything.
///
/// # Invariant
///
/// Both are read where the provider's events arrive, by the task that reads the
/// socket, and never by the task that consumes them: a consumer held up by a tool
/// for a minute would otherwise count that minute as time spent generating, which
/// is the rate SPEC's title would then show. See `docs/notes/server-agent.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamWindow {
    /// The first delta of any kind, in milliseconds from the attempt's request.
    /// `None` is an attempt that streamed no content at all.
    pub first_delta_ms: Option<u64>,
    /// The provider's last event, in milliseconds from the same start.
    pub end_ms: u64,
}

impl StreamWindow {
    /// Output tokens per second this attempt generated, when both numbers exist.
    ///
    /// # Invariant
    ///
    /// The window is the provider's last event minus its first delta -- what the
    /// stream itself took -- so a rate is a property of the stream and not of the
    /// step it happened during. An empty window measures nothing rather than
    /// dividing by zero.
    pub fn tps(&self, output_tokens: Option<u64>) -> Option<f64> {
        let first = self.first_delta_ms?;
        let span = self.end_ms.saturating_sub(first);
        if span == 0 {
            return None;
        }
        Some(output_tokens? as f64 / (span as f64 / 1000.0))
    }
}

/// What the model produced, one chunk at a time.
///
/// # Invariant
///
/// Every variant carries `item_id`, which is the only thing that keeps
/// interleaved output items apart. See `docs/notes/server-llm.md`.
/// Which field of an item a streamed chunk fills.
///
/// # Invariant
///
/// The names are the provider's own events: a reasoning item's `reasoning_text` is not
/// its `summary`, and only a draft that keeps them apart can be sent back as the item
/// the provider produced. See `docs/notes/server-llm.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `response.output_text.delta`: an assistant message's text.
    OutputText,
    /// `response.reasoning_text.delta`: the thought itself.
    ReasoningText,
    /// `response.reasoning_summary_text.delta`: the provider's summary of the thought.
    SummaryText,
}

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
        /// Which field the text fills, when it fills one: a draft rebuilds the item from it.
        field: Option<Field>,
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
        /// Measured by the reader, so the step's own clock cannot touch it.
        window: StreamWindow,
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

/// Whether the provider refused a transcript it wrote without thinking.
pub fn thinking_refusal(message: &str) -> bool {
    message.contains("reasoning_text") && message.contains("thinking mode")
}

/// Whether the provider's own content inspection refused what it was generating.
///
/// # Invariant
///
/// Measured on `qwen4-max-preview`: 0.27% of 1,840 requests, reproducible with the same
/// prompt, and fired once with zero generated text -- so it is the provider's misfire
/// and not a statement about this request. See `docs/notes/server-llm.md`.
pub fn moderation_refusal(message: &str) -> bool {
    let said = message.to_ascii_lowercase();
    said.contains("data_inspection")
        || said.contains("datainspectionfailed")
        || said.contains("inappropriate content")
}

/// How many times one request refused that way is sent again.
///
/// # Invariant
///
/// Its own budget, never `max_retries`: that one is 0 on this machine, which this module
/// reads as no ceiling, and a provider genuinely blocking content must not loop.
const MODERATION_RETRIES: u32 = 2;

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

/// The kinds a provider names for a request it will never accept; the `invalid*`
/// family is caught by prefix instead.
const REFUSED_KINDS: &[&str] = &[
    "context_length_exceeded",
    "authentication_error",
    "permission_error",
    "not_found_error",
    "insufficient_quota",
];

/// Whether a streamed `error` event describes a bad minute or a refusal.
///
/// # Invariant
///
/// `status_retryable`'s rule reaches the face that has no status line: a kind naming
/// the request invalid, or a status the provider states itself, is a refusal and ends
/// the turn. Any other kind stays retryable -- providers invent their own spellings,
/// and one extra try costs less than giving up on one this code has not heard of.
///
/// See `docs/notes/server-llm.md`.
fn error_retryable(d: &Value) -> bool {
    let error = event_error(d);
    let kind = error["type"]
        .as_str()
        .or_else(|| error["code"].as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind.starts_with("invalid") || REFUSED_KINDS.contains(&kind.as_str()) {
        return false;
    }
    match stated_status(error, d) {
        Some(status) => status_retryable(status),
        None => true,
    }
}

/// The status a provider stated in the failure itself, when it stated one.
///
/// # Invariant
///
/// A streamed failure has no status line -- the `200` went out before the provider
/// changed its mind -- so a status is known only where the provider writes one: in its
/// `code`, or opening its message. A number anywhere else is prose, and prose read as
/// a status ends a turn a retry could have saved.
fn stated_status(error: &Value, d: &Value) -> Option<reqwest::StatusCode> {
    if let Some(status) = error["code"]
        .as_u64()
        .and_then(|code| u16::try_from(code).ok())
        .and_then(|code| reqwest::StatusCode::from_u16(code).ok())
    {
        return Some(status);
    }
    let said = error["message"]
        .as_str()
        .or_else(|| d["message"].as_str())
        .unwrap_or_default();
    opening_status(said)
}

/// A status written at the front of a message: `<400> InternalError.Algo...`.
fn opening_status(said: &str) -> Option<reqwest::StatusCode> {
    let rest = said.trim_start().trim_start_matches(['<', '[']);
    let digits = rest.get(..3)?;
    // Four digits is a number, not a status.
    let next_is_digit = rest.as_bytes().get(3).is_some_and(u8::is_ascii_digit);
    if !digits.bytes().all(|b| b.is_ascii_digit()) || next_is_digit {
        return None;
    }
    reqwest::StatusCode::from_u16(digits.parse().ok()?).ok()
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
    crate::prompts::harness_wrap(prompts.render("size_hint", &[("message", message.as_str())]))
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
    session_headers: Vec<(String, String)>,
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
            session_headers,
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
    session_headers: Vec<(String, String)>,
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
        session_headers,
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
    session_headers: Vec<(String, String)>,
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
    // Every vendor extension the roster names, each carrying the same session id.
    for (name, value) in session_headers {
        request = request.header(name, value);
    }
    let request = request.body(reqwest::Body::wrap_stream(body));

    let response = match tokio::select! {
        // Nothing else listens to the token: a stop
        // is `interrupt_session`, not a refusal. See docs/notes/server-monitor.md.
        _ = cancel.cancelled() => {
            probe.outcome("cancelled");
            tracing::info!(elapsed = ?started.elapsed(), "model request: cancelled before headers");
            let _ = tx.send(LlmEvent::Cancelled).await;
            return;
        }
        sent = tokio::time::timeout(headers_timeout, request.send()) => sent,
    } {
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
    let mut out = Outbox::new(tx, started);
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
    /// Complete tape positions, including masked spans.
    pub input_tokens: Option<u64>,
    /// Visible positions after the provider applies the pinned memory view.
    pub gongga_unmasked_input_tokens: Option<u64>,
    /// Provider-tokenized sizes of the masked blocks in this request.
    #[serde(default)]
    pub gongga_memory_tokens: Vec<MemoryTokenUsage>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// How much of `input_tokens` the provider served from its own prefix cache.
    pub input_tokens_details: Option<InputTokens>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryTokenUsage {
    pub memory_id: String,
    pub tokens: u64,
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
    /// The thought itself, from `reasoning_text` deltas.
    text: String,
    /// The provider's own summary of the thought, when it streamed one.
    summary_text: String,
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

/// The constant opening and separator, when this home carries them.
fn plan_framing() -> crate::anthropic::Framing {
    let prompts = crate::prompts::shared();
    // Both are in `REQUIRED`: a home without them does not start.
    let text = |name: &str| prompts.text(name).unwrap_or_default().trim().to_string();
    crate::anthropic::Framing {
        opening: text(crate::prompts::PLAN_OPENING),
        separator: text(crate::prompts::PLAN_SEPARATOR),
    }
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
    /// The attempt's own start, so the window below is on the request's clock.
    started: Instant,
    /// When this attempt first said anything; `None` is an attempt that said nothing.
    first_delta_ms: Option<u64>,
}

impl<'a> Outbox<'a> {
    pub(crate) fn new(tx: &'a mpsc::Sender<LlmEvent>, started: Instant) -> Self {
        Self {
            tx,
            content: false,
            started,
            first_delta_ms: None,
        }
    }

    pub(crate) async fn send(
        &mut self,
        event: LlmEvent,
    ) -> Result<(), mpsc::error::SendError<LlmEvent>> {
        let content = event_content(&event);
        if content && self.first_delta_ms.is_none() {
            self.first_delta_ms = Some(self.ms());
        }
        self.content |= content;
        self.tx.send(event).await
    }

    /// The whole response is in: what the stream took, measured here and not by the
    /// reader's consumer.
    ///
    /// # Invariant
    ///
    /// The window is closed before the event is queued, so no wait of the task that
    /// takes it can widen it. See [`StreamWindow`].
    pub(crate) async fn complete(
        &mut self,
        usage: Option<Usage>,
    ) -> Result<(), mpsc::error::SendError<LlmEvent>> {
        let window = StreamWindow {
            first_delta_ms: self.first_delta_ms,
            end_ms: self.ms(),
        };
        self.send(LlmEvent::Completed { usage, window }).await
    }

    fn ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
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
                field: None,
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
                    summary_text: String::new(),
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
                        field: None,
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
                        field: None,
                    })
                    .await?;
                }
                ItemKind::Unknown => {}
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let delta = d["delta"].as_str().unwrap_or_default().to_string();
            let field = if event.name.contains("summary") {
                Field::SummaryText
            } else {
                Field::ReasoningText
            };
            // Deltas can precede `output_item.added`; `done` re-derives the kind.
            let meta = items.entry(item_id.clone()).or_default();
            if field == Field::SummaryText {
                meta.summary_text.push_str(&delta);
            } else {
                meta.text.push_str(&delta);
            }
            out.send(LlmEvent::Delta {
                part: ResponsePart::Reasoning,
                item_id,
                text: delta,
                field: Some(field),
            })
            .await?;
        }
        // Whole summary at once: for some providers the only copy there is.
        "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
            let item_id = d["item_id"].as_str().unwrap_or_default().to_string();
            let full = d["text"].as_str().unwrap_or_default();
            let field = if event.name.contains("summary") {
                Field::SummaryText
            } else {
                Field::ReasoningText
            };
            let meta = items.entry(item_id.clone()).or_default();
            let seen = if field == Field::SummaryText {
                meta.summary_text.as_str()
            } else {
                meta.text.as_str()
            };
            let rest = full.strip_prefix(seen).map(str::to_string);
            if let Some(rest) = rest.filter(|rest| !rest.is_empty()) {
                if field == Field::SummaryText {
                    meta.summary_text.push_str(&rest);
                } else {
                    meta.text.push_str(&rest);
                }
                out.send(LlmEvent::Delta {
                    part: ResponsePart::Reasoning,
                    item_id,
                    text: rest,
                    field: Some(field),
                })
                .await?;
            }
        }
        "response.output_text.delta" => {
            out.send(LlmEvent::Delta {
                part: ResponsePart::Output,
                item_id: d["item_id"].as_str().unwrap_or_default().to_string(),
                text: d["delta"].as_str().unwrap_or_default().to_string(),
                field: Some(Field::OutputText),
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
                field: None,
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
                        // The reader wants the thought when the provider summarised nothing.
                        summary = if meta.summary_text.trim().is_empty() {
                            meta.text.clone()
                        } else {
                            meta.summary_text.clone()
                        };
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
                            field: None,
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
                        field: None,
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
            out.complete(usage).await?;
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
    use super::{PLAN_CHUNK_CHARS, PlanTurn, split_plan};

    #[test]
    fn short_plan_stays_one_user_turn() {
        let turns = split_plan("hello world", "---", PLAN_CHUNK_CHARS);
        assert_eq!(turns.len(), 1);
        assert!(matches!(&turns[0], PlanTurn::User(s) if *s == "hello world"));
    }

    #[test]
    fn split_is_lossless_and_ends_on_user() {
        let plan = "x".repeat(PLAN_CHUNK_CHARS * 3 + 7);
        let turns = split_plan(&plan, "---", PLAN_CHUNK_CHARS);
        let rebuilt: String = turns
            .iter()
            .filter_map(|t| match t {
                PlanTurn::User(s) => Some(*s),
                PlanTurn::Assistant(_) => None,
            })
            .collect();
        assert_eq!(rebuilt, plan, "user turns must rebuild the plan verbatim");
        assert!(matches!(turns.last(), Some(PlanTurn::User(_))));
        // N+1 user turns separated by N assistant bridges for N+1 chunks.
        let chunks = 4; // 3 full + 1 remainder
        assert_eq!(turns.len(), chunks * 2 - 1);
    }

    #[test]
    fn split_never_breaks_utf8_and_keeps_each_chunk_small() {
        // Multibyte CJK: cutting by char count must not slice a codepoint.
        let plan = "上下文索引".repeat(PLAN_CHUNK_CHARS / 5);
        let bridge = "——";
        let turns = split_plan(&plan, bridge, PLAN_CHUNK_CHARS);
        let rebuilt: String = turns
            .iter()
            .filter_map(|t| match t {
                PlanTurn::User(s) => Some(*s),
                PlanTurn::Assistant(_) => None,
            })
            .collect();
        assert_eq!(rebuilt, plan);
        for turn in &turns {
            match turn {
                PlanTurn::User(s) => assert!(s.chars().count() <= PLAN_CHUNK_CHARS),
                PlanTurn::Assistant(s) => assert_eq!(*s, bridge),
            }
        }
        assert!(matches!(turns.last(), Some(PlanTurn::User(_))));
    }

    #[test]
    fn exactly_at_limit_is_one_turn() {
        let plan = "a".repeat(PLAN_CHUNK_CHARS);
        let turns = split_plan(&plan, "---", PLAN_CHUNK_CHARS);
        assert_eq!(turns.len(), 1);
        assert!(matches!(&turns[0], PlanTurn::User(s) if s.len() == PLAN_CHUNK_CHARS));
    }

    #[test]
    fn one_over_limit_is_two_turns_with_a_bridge() {
        let plan = "a".repeat(PLAN_CHUNK_CHARS + 1);
        let turns = split_plan(&plan, "---", PLAN_CHUNK_CHARS);
        assert_eq!(turns.len(), 3);
        assert!(matches!(&turns[0], PlanTurn::User(s) if s.chars().count() == PLAN_CHUNK_CHARS));
        assert!(matches!(&turns[1], PlanTurn::Assistant(_)));
        assert!(matches!(&turns[2], PlanTurn::User(s) if s.chars().count() == 1));
    }
}
