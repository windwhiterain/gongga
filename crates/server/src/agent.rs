//! The agent loop: one user message in, a stream of events out.
//! See `docs/notes/server-agent.md`.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use protocol::{
    Event, LogLevel, ResponseEvent, ResponsePart, SessionStatus, ToolAnswerEvent, ToolStream,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::bus::EventBus;
use crate::config::LlmConfig;
use crate::llm::{LlmClient, LlmEvent};
use crate::providers::{self};
use crate::session::SessionHandle;
use crate::store::Store;
use crate::tools::detached::DetachedRegistry;
use crate::tools::{ToolContext, ToolRegistry};

/// Share of the window compaction fills; the rest is headroom for one step's output.
const DEFAULT_COMPACT_PERCENT: u8 = 70;

pub struct Agent {
    pub store: Store,
    /// What a session talks to unless it named its own `--llm`.
    pub llm: Arc<LlmClient>,
    /// The machine's roster, which `--llm` names resolve against. A live view:
    /// a reload reaches every later route.
    pub providers: providers::Handle,
    /// What a session's own entry inherits: timeouts and retries.
    pub llm_config: LlmConfig,
    /// Clients for sessions that named their own entry, keyed by `provider/model`
    /// and by the roster read that produced it.
    pub clients: std::sync::Mutex<std::collections::HashMap<String, (u64, Arc<LlmClient>)>>,
    /// [`Self::llm`]'s context window, for a percentage set without naming a model.
    pub default_context: Option<u64>,
    pub tools: Arc<ToolRegistry>,
    pub bus: EventBus,
    /// Compress once a request would carry more than this many input tokens.
    /// 0 disables it.
    pub compress_above_tokens: u64,
    /// Where capped tool output is spilled; without it the tail is lost.
    pub output_dir: PathBuf,
    /// Detached jobs of every session, reached through `ToolContext`.
    pub detached: DetachedRegistry,
}

impl Agent {
    /// Runs one already-claimed turn whose user messages and obligations are durable.
    pub async fn run_turn(
        &self,
        session: &SessionHandle,
        tasks: Vec<i64>,
        epoch: u64,
        gate: tokio::sync::OwnedMutexGuard<()>,
    ) {
        let outcome = AssertUnwindSafe(self.turn_inner(session, epoch, gate))
            .catch_unwind()
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(payload) => {
                let message = panic_message(&payload);
                tracing::error!(session = %session.name(), "turn panicked: {message}");
                self.bus.publish(Event::Log {
                    level: LogLevel::Error,
                    message: format!("{}: turn panicked: {message}", session.name()),
                });
                TurnOutcome::Failed
            }
        };
        let (status, detail) = match outcome {
            TurnOutcome::Done => (SessionStatus::Idle, None),
            TurnOutcome::Cancelled => (SessionStatus::Idle, Some("cancelled".to_string())),
            TurnOutcome::Failed => (SessionStatus::Error, Some("see log".to_string())),
        };
        let preserve = session.take_resumable();
        if let Err(e) = self
            .store
            .finish_turn(session.id(), session.name(), &tasks, status, preserve)
        {
            tracing::error!(session = %session.name(), error = %e, "could not finish the durable turn");
        }
        if !session.is_deleted() {
            self.bus.publish(Event::Status {
                session: session.name().to_string(),
                status,
                detail,
            });
        }
    }

    async fn turn_inner(
        &self,
        session: &SessionHandle,
        epoch: u64,
        _gate: tokio::sync::OwnedMutexGuard<()>,
    ) -> TurnOutcome {
        let name = session.name().to_string();
        let id = session.id();
        if session.cancel_epoch() != epoch {
            // Cancelled while queued: running looks like the interrupt did nothing.
            tracing::info!(session = %name, "turn dropped: cancelled while queued");
            return TurnOutcome::Cancelled;
        }
        if session.is_deleted() {
            tracing::info!(session = %name, "turn dropped: session was deleted before starting");
            return TurnOutcome::Cancelled;
        }
        let (turn, cancel) = session.new_turn();
        let started = std::time::Instant::now();
        // A call the last turn left open is closed before anything is asked:
        // providers reject the pair.
        match self.store.close_dangling_calls(session.id()) {
            Ok(0) => {}
            Ok(n) => tracing::info!(session = %name, closed = n, "calls that never got a result"),
            Err(e) => tracing::warn!(session = %name, error = %e, "could not close a dangling call"),
        }
        // Resolved once: every step of a turn sends to the model it started on.
        let (llm, budget) = match self.route(&name, &session.tuning()) {
            Ok(route) => route,
            Err(e) => {
                self.error(id, &name, "resolving the session's llm", e);
                return TurnOutcome::Failed;
            }
        };
        // Read once, like the route: every step sends the level this turn began with.
        let effort = session.tuning().thinking_effort;
        tracing::info!(session = %name, model = %llm.model(), budget, "turn start");

        self.bus.publish(Event::Status {
            session: name.clone(),
            status: SessionStatus::Running,
            detail: None,
        });
        let _ = self.store.set_status(id, SessionStatus::Running);

        // No step ceiling: it ends on a stop, a cancel or a failure, and every break sets it.
        let mut step: u32 = 0;

        let outcome = loop {
            if cancel.is_cancelled() {
                break TurnOutcome::Cancelled;
            }
            // Step boundary: a user line between a call and its result is rejected, so inject here.
            self.deliver_inbox(id, &name);
            // Compress before the request is built, so the very next request is the small one.
            self.maybe_notify_context(session, id, &name, budget);
            let cwd = session.cwd();
            tracing::info!(session = %name, step, "step start");
            let attempt = Attempt {
                number: step,
                llm: &llm,
                effort: effort.as_deref(),
                // Read again per step, like every other word of the prompt.
                cwd: &cwd,
            };
            match self.step(id, &name, turn, &cancel, &attempt).await {
                Ok(StepResult::Finished) => {
                    // Something may have landed while it finished; deliver it and go round again.
                    if self.deliver_inbox(id, &name) > 0 {
                        step = step.wrapping_add(1);
                        continue;
                    }
                    break TurnOutcome::Done;
                }
                Ok(StepResult::NeedsTools(calls)) => {
                    if self
                        .run_tools(session, id, &name, &cancel, &llm, calls)
                        .await
                    {
                        break TurnOutcome::Cancelled;
                    }
                }
                Ok(StepResult::Stopped) => break TurnOutcome::Cancelled,
                Err(e) => {
                    self.error(id, &name, "model request", e);
                    break TurnOutcome::Failed;
                }
            }
            // Wrapping, not saturating: the counter is only a log label.
            step = step.wrapping_add(1);
        };

        tracing::info!(session = %name, outcome = ?outcome, elapsed = ?started.elapsed(), "turn end");
        outcome
    }

    /// Where this turn sends, and how many input tokens it may carry before compacting.
    fn route(
        &self,
        name: &str,
        tuning: &crate::session::Tuning,
    ) -> anyhow::Result<(Arc<LlmClient>, u64)> {
        let (llm, context) = match &tuning.llm {
            None => (self.llm.clone(), self.default_context),
            Some(spec) => {
                let choice = self
                    .providers
                    .resolve(spec)
                    .map_err(|e| anyhow::anyhow!("--llm {spec}: {e}"))?;
                let client = self.client_for(&choice.entry, choice.api_key)?;
                (client, choice.entry.context)
            }
        };
        let budget = self.budget(name, context, tuning.compact_threshold);
        Ok((llm, budget))
    }

    /// The compaction threshold in input tokens; zero means off without exception.
    fn budget(&self, name: &str, context: Option<u64>, percent: Option<u8>) -> u64 {
        // A percentage has no size without a window; warn rather than guess.
        if self.compress_above_tokens != 0 && context.is_none() && percent.is_some() {
            tracing::warn!(
                session = %name,
                "no context window is known for this model; using the configured token budget"
            );
        }
        compaction_budget(self.compress_above_tokens, context, percent)
    }

    /// The client for one roster entry, built once and shared.
    fn client_for(
        &self,
        entry: &providers::Entry,
        api_key: String,
    ) -> anyhow::Result<Arc<LlmClient>> {
        let spec = entry.spec();
        let generation = self.providers.generation();
        let cached = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&spec)
            .cloned();
        if let Some((pinned, client)) = cached {
            if pinned == generation {
                return Ok(client);
            }
        }
        // Built outside the lock: a duplicate client is cheaper than serialising turns.
        let client = Arc::new(LlmClient::new(&self.llm_config.for_entry(entry, api_key))?);
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec, (generation, client.clone()));
        Ok(client)
    }

    /// Moves the session's inbox into the transcript in one durable transaction.
    fn deliver_inbox(&self, id: i64, name: &str) -> usize {
        match self.store.deliver_pending(id, name) {
            Ok(delivered) => {
                if delivered > 0 {
                    tracing::info!(session = %name, delivered, "delivered queued messages at a step boundary");
                    self.bus.publish(Event::Log {
                        level: LogLevel::Info,
                        message: format!("{name}: delivered {delivered} queued message(s)"),
                    });
                }
                delivered
            }
            Err(e) => {
                tracing::warn!(session = %name, error = %e, "could not deliver queued messages");
                0
            }
        }
    }

    /// Tells the session its context reached the threshold, and stops there.
    ///
    /// # Invariant
    ///
    /// The loop never folds the transcript itself. Folding is a judgement about
    /// work only the agent has seen, so the server measures and asks: the agent
    /// decides what the next stretch of context should contain. See the
    /// `heir` skill and `docs/notes/server-agent.md`.
    fn maybe_notify_context(&self, session: &SessionHandle, id: i64, name: &str, budget: u64) {
        if budget == 0 {
            return;
        }
        // The size the provider last measured for this session; never an estimate.
        let Ok(tokens) = self.store.context_tokens(id) else {
            return;
        };
        if tokens < budget {
            return;
        }
        let Ok((_, upto)) = self.store.summary(id) else {
            return;
        };
        // Once per state of the transcript: nagging every step teaches nothing.
        if !session.note_context_asked(upto) {
            return;
        }
        let message = context_notice(crate::prompts::shared(), name, tokens, budget, upto);
        if let Err(e) = self.store.add_pending_task(
            name,
            "inject",
            &json!({"session": name, "message": message}),
        ) {
            self.error(id, name, "queueing the context notice", e);
            return;
        }
        self.bus.publish(Event::Log {
            level: LogLevel::Info,
            message: format!(
                "{name}: context at {tokens} of {budget} tokens; asked the agent to fold"
            ),
        });
    }

    /// The prose this session sends, read from the row written when it last moved.
    ///
    /// # Invariant
    ///
    /// Templates and rule files are read at a step that *stores* something new --
    /// the turn's first step, a handover note that moved, a directory that changed --
    /// and never re-rendered for a step that does not. A stored row is what gets
    /// sent, so a session's requests share one prefix and an edit lands at a
    /// boundary instead of re-billing the whole history mid-turn.
    fn prompt_in_force(
        &self,
        id: i64,
        name: &str,
        cwd: &std::path::Path,
        note: Option<&str>,
        turn_start: bool,
    ) -> anyhow::Result<crate::store::Prompt> {
        let stored = self.store.prompt(id)?;
        let cwd = cwd.display().to_string();
        let here = crate::store::stamp(note);
        if let Some(kept) = stored.clone().filter(|p| !prompt_moved(p, &cwd, &here, turn_start)) {
            return Ok(kept);
        }
        let prompts = crate::prompts::shared();
        let instructions = prompts.text("instructions").unwrap_or_default();
        let summary = note
            .map(|s| prompts.render("summary_preamble", &[("summary", s)]));
        let prompt = crate::store::Prompt {
            instructions,
            summary,
            cwd,
            stamp: here,
        };
        // Rewritten prose is a real event: the prefix the provider cached is gone
        // from here, and the ledger will show the miss.
        if let Some(p) = &stored {
            if p.instructions != prompt.instructions || p.summary != prompt.summary {
                tracing::info!(session = %name, "the prompt in force changed: a new row is sent");
            }
        }
        self.store.append_prompt(id, &prompt)?;
        Ok(prompt)
    }

    /// One model request. Returns the tool calls it asked for, if any.
    async fn step(
        &self,
        id: i64,
        name: &str,
        turn: u64,
        cancel: &tokio_util::sync::CancellationToken,
        attempt: &Attempt<'_>,
    ) -> anyhow::Result<StepResult> {
        let Attempt {
            number: step,
            llm,
            effort,
            cwd,
        } = *attempt;
        let (summary, history) = self.store.context(id)?;
        let definitions = self.tools.definitions();
        let prompt = self.prompt_in_force(id, &name, cwd, summary.as_deref(), step == 1)?;
        let mut rx = llm.stream(
            &prompt.instructions,
            prompt.summary.as_deref(),
            &history,
            &definitions,
            effort,
            cancel.clone(),
        );

        let mut calls: Vec<ToolCall> = Vec::new();
        let mut failure: Option<String> = None;
        let mut saw_completion = false;

        while let Some(event) = rx.recv().await {
            match event {
                LlmEvent::PartStart { .. } => {}
                LlmEvent::Delta { part, text, .. } => {
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        part,
                        text,
                        done: false,
                    }));
                }
                LlmEvent::PartDone { part, .. } => {
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        part,
                        text: String::new(),
                        done: true,
                    }));
                }
                LlmEvent::Reasoning {
                    item_id,
                    summary,
                    encrypted,
                } => {
                    let mut payload = json!({"summary": summary});
                    if let Some(e) = encrypted {
                        payload["encrypted"] = json!(e);
                    }
                    payload["item_id"] = json!(item_id);
                    let _ = self.store.append_message(id, "reasoning", &payload);
                }
                // The provider executed this one; keeping the item verbatim is
                // what lets the next request carry the same transcript.
                LlmEvent::BuiltinCall {
                    item, replayable, ..
                } => {
                    let _ = self.store.append_message(
                        id,
                        "builtin_call",
                        &json!({"item": item, "replayable": replayable}),
                    );
                }
                LlmEvent::Citation { title, url, .. } => {
                    // One line per source: a search without its sources is unverifiable.
                    let text = format!("cited {title:?} — {url}");
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        part: ResponsePart::Activity,
                        text: text.clone(),
                        done: true,
                    }));
                    // Operator-only: the model keeps its text, not the annotation.
                    let _ = self
                        .store
                        .append_message(id, "notice", &json!({"text": text}));
                }
                LlmEvent::Output { text, .. } => {
                    if !text.trim().is_empty() {
                        let _ = self.store.append_message(
                            id,
                            "text",
                            &json!({"role":"assistant","text":text}),
                        );
                    }
                }
                LlmEvent::ToolCall {
                    call_id,
                    name: tool,
                    arguments,
                    ..
                } => {
                    let _ = self.store.append_message(
                        id,
                        "tool_call",
                        &json!({"call_id": call_id, "name": tool, "arguments": arguments}),
                    );
                    let _ = self.store.start_tool_call(id, &call_id, &tool, &arguments);
                    calls.push(ToolCall {
                        call_id,
                        tool,
                        arguments,
                    });
                }
                LlmEvent::Completed { usage } => {
                    saw_completion = true;
                    if let Some(u) = usage {
                        let cached = u.cached_tokens();
                        tracing::info!(
                            session = %name,
                            input = u.input_tokens.unwrap_or(0),
                            cached = cached.unwrap_or(0),
                            output = u.output_tokens.unwrap_or(0),
                            total = u.total_tokens.unwrap_or(0),
                            "provider usage"
                        );
                        // The row outlives the log line: "did the replay hold the
                        // cache?" is asked after the turn, per step.
                        let _ = self.store.append_event(
                            Some(id),
                            "usage",
                            &json!({
                                "model": llm.model(),
                                "input": u.input_tokens,
                                "cached": cached,
                                "output": u.output_tokens,
                                "total": u.total_tokens,
                            }),
                        );
                        // The provider's own count, never computed: no arithmetic matches it.
                        if let Some(input) = u.input_tokens {
                            let _ = self.store.set_context_tokens(id, input);
                        }
                    }
                }
                // Fatal by the time it reaches here: `llm::stream` retries
                // what another attempt could fix and reports the rest.
                LlmEvent::Failed { message, .. } => {
                    failure = Some(message);
                    break;
                }
                // A retry is the only news a stalled request gives; tell the operator, not the model.
                LlmEvent::Retrying {
                    attempt,
                    max,
                    delay,
                    reason,
                } => {
                    self.bus.publish(Event::Log {
                        level: LogLevel::Warn,
                        message: retry_warning(&name, attempt, max, delay, &reason),
                    });
                }
                LlmEvent::Cancelled => {
                    return Ok(StepResult::Stopped);
                }
            }
        }

        if let Some(message) = failure {
            anyhow::bail!("{message}");
        }
        if !saw_completion {
            anyhow::bail!("model stream ended without completing");
        }
        if cancel.is_cancelled() {
            return Ok(StepResult::Stopped);
        }
        if calls.is_empty() {
            return Ok(StepResult::Finished);
        }
        tracing::debug!(session = %name, step, calls = calls.len(), "step produced tool calls");
        Ok(StepResult::NeedsTools(calls))
    }

    /// Turns `@<path>` mentions in a tool's output into attachments.
    ///
    /// # Invariant
    ///
    /// A mention that names no readable picture is left as text: a command that
    /// prints `@missing.png` must not fail the turn, and a model that has to learn
    /// the file is gone should be told so by the note `image::content_part` emits.
    /// A model that cannot take images gets those references reported once, in the
    /// output, rather than silently dropped.
    async fn attach_references(
        &self,
        ctx: &ToolContext,
        output: &str,
        llm: &LlmClient,
    ) -> Vec<crate::image::ImageRef> {
        let mut out = Vec::new();
        for given in crate::image::references(output) {
            let path = crate::image::resolve(&ctx.cwd, &given);
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                // Not a picture this harness can read: leave the text alone.
                Err(_) => continue,
            };
            let Some(media) = crate::image::media_type(&path, &bytes) else {
                continue;
            };
            if !ctx.vision.accepts_image {
                self.bus.publish(Event::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "{}: {given} was referenced but not attached -- {}",
                        ctx.session, ctx.vision.refusal
                    ),
                });
                continue;
            }
            let mut image = crate::image::ImageRef::new(path.to_string_lossy().to_string(), media);
            // One id per file per session: a replay reuses it instead of the bytes.
            match llm.upload_image(&image.path, &image.media_type).await {
                Ok(id) => image.file_id = Some(id),
                Err(e) => {
                    // Attaching is an improvement, not a requirement: the turn goes
                    // on with the reference as text.
                    self.bus.publish(Event::Log {
                        level: LogLevel::Warn,
                        message: format!(
                            "{}: could not attach {given} ({e}); it stays a path in the text",
                            ctx.session
                        ),
                    });
                    continue;
                }
            }
            out.push(image);
        }
        out
    }

    /// Executes the tool calls for a step. Returns true if the turn was cancelled.
    async fn run_tools(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        cancel: &tokio_util::sync::CancellationToken,
        llm: &LlmClient,
        calls: Vec<ToolCall>,
    ) -> bool {
        for call in calls {
            if cancel.is_cancelled() {
                return true;
            }
            let arguments: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));

            // Forward streamed tool output onto the bus, tagging each chunk with
            // the tool name the context does not know.
            let (tx, mut rx) = mpsc::channel::<ToolAnswerEvent>(256);
            let bus = self.bus.clone();
            let tool_label = call.tool.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(mut e) = rx.recv().await {
                    e.tool = tool_label.clone();
                    bus.publish(Event::ToolAnswer(e));
                }
            });

            let ctx = ToolContext {
                session: name.to_string(),
                call_id: call.call_id.clone(),
                output_dir: self.output_dir.clone(),
                shells: session.shells(),
                detached: self.detached.clone(),
                events: Some(tx),
                cancel: cancel.clone(),
                store: self.store.clone(),
                cwd: session.cwd(),
                vision: llm.vision(crate::prompts::shared()),
                inbox: session.inbox_signal(),
            };
            let started = std::time::Instant::now();
            tracing::info!(session = %name, tool = %call.tool, "tool start");
            let outcome = self.tools.run(&call.tool, arguments, &ctx).await;
            let scanned = self.attach_references(&ctx, &outcome.output, llm).await;
            drop(ctx);
            let _ = forwarder.await;
            tracing::info!(
                session = %name,
                tool = %call.tool,
                ok = outcome.ok,
                elapsed = ?started.elapsed(),
                "tool end"
            );

            self.bus.publish(Event::ToolAnswer(ToolAnswerEvent {
                session: name.to_string(),
                call_id: call.call_id.clone(),
                tool: call.tool.clone(),
                stream: ToolStream::Stdout,
                text: String::new(),
                done: true,
            }));

            let status = if outcome.ok { "ok" } else { "error" };
            // A command that printed `@shot.png` names a picture: it becomes an
            // attachment, uploaded once per file.
            let images: Vec<_> = scanned
                .iter()
                .map(crate::image::ImageRef::to_json)
                .collect();
            let _ = self.store.append_message(
                id,
                "tool_result",
                &json!({"call_id": call.call_id, "output": outcome.output, "images": images}),
            );
            let _ = self
                .store
                .finish_tool_call(id, &call.call_id, &outcome.output, status);
            let _ = self.store.append_event(
                Some(id),
                "tool_result",
                &json!({"tool": call.tool, "call_id": call.call_id, "status": status}),
            );
        }
        false
    }

    /// Reports a failure, and leaves the reason somewhere it can be read later.
    ///
    /// # Invariant
    ///
    /// The line goes into the transcript as a `notice`, which `history_since`
    /// skips. An `Event::Log` alone is broadcast and forgotten: "why did that
    /// turn fail" was unanswerable a minute later, and the failure of the
    /// *first* attempt of a request exists nowhere else at all.
    fn error(&self, id: i64, session: &str, what: &str, e: anyhow::Error) {
        tracing::error!(session = %session, "failed to {what}: {e}");
        let said = format!("{session}: failed to {what}: {e}");
        let _ = self
            .store
            .append_message(id, "notice", &json!({"text": said}));
        self.bus.publish(Event::Log {
            level: LogLevel::Error,
            message: said,
        });
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub tool: String,
    pub arguments: String,
}

enum StepResult {
    /// The model answered without asking for tools.
    Finished,
    /// The model wants tools run before it continues.
    NeedsTools(Vec<ToolCall>),
    /// Cancellation or an aborted stream.
    Stopped,
}

/// One model request: which round of the turn it is, and how to make it.
#[derive(Clone, Copy)]
struct Attempt<'a> {
    number: u32,
    llm: &'a LlmClient,
    /// How hard the model should think, when the session asked for a level.
    effort: Option<&'a str>,
    /// Where the session's shell stands, which the row records so a move is seen.
    cwd: &'a std::path::Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    Done,
    Cancelled,
    Failed,
}

/// What a panicking task carries, as text.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// The operator-facing line for a request that is about to be tried again.
///
/// # Invariant
///
/// `max == 0` is "no ceiling", not "zero allowed": printing `1/0` read like the
/// opposite of what it meant.
fn retry_warning(
    session: &str,
    attempt: u32,
    max: u32,
    delay: std::time::Duration,
    reason: &str,
) -> String {
    let of = if max == 0 {
        "until cancelled".to_string()
    } else {
        format!("of {max}")
    };
    format!("{session}: model request failed ({reason}); retry {attempt} {of} in {delay:?}")
}

/// Whether the prose has to be rendered and stored again for this step.
///
/// # Invariant
///
/// A note that moved is content entering the store, so it is noticed at any step. A
/// different directory is noticed only at a turn's first step, and an edited template
/// is not noticed here at all -- it lands at the next turn. The alternative,
/// re-rendering per step, re-bills the history that sits behind the prose.
fn prompt_moved(stored: &crate::store::Prompt, cwd: &str, stamp: &str, turn_start: bool) -> bool {
    stored.stamp != stamp || (turn_start && stored.cwd != cwd)
}

/// The message that hands the folding decision to the agent, from the templates.
///
/// # Invariant
///
/// It names the numbers it is talking about and the two tools that act on them:
/// a notice that says only "you are near the limit" leaves the reader to guess
/// what is possible. See the `heir` skill.
fn context_notice(
    prompts: &crate::prompts::Prompts,
    session: &str,
    tokens: u64,
    budget: u64,
    upto: i64,
) -> String {
    let folded = if upto == 0 {
        prompts.text("folded_none").unwrap_or_default()
    } else {
        prompts.render("folded_through", &[("upto", &upto.to_string())])
    };
    prompts.render(
        "context_notice",
        &[
            ("session", session),
            ("tokens", &tokens.to_string()),
            ("budget", &budget.to_string()),
            ("folded", folded.trim()),
        ],
    )
}

/// The compaction threshold in input tokens, or 0 for "off".
fn compaction_budget(configured: u64, context: Option<u64>, percent: Option<u8>) -> u64 {
    if configured == 0 {
        return 0;
    }
    let Some(window) = context.filter(|w| *w > 0) else {
        return configured;
    };
    providers::tokens_for_percent(window, percent.unwrap_or(DEFAULT_COMPACT_PERCENT))
}

/// Where a summary may take over: up to and including the seq is compressed.
///
/// # Invariant
///
/// The boundary is followed by a `text` or `tool_call`, never by an orphaned
/// `tool_result`, which providers reject.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_config_turns_compaction_off_whatever_the_window_is() {
        assert_eq!(compaction_budget(0, Some(1_000_000), Some(50)), 0);
        assert_eq!(compaction_budget(0, Some(1_000_000), None), 0);
        assert_eq!(compaction_budget(0, None, None), 0);
    }

    /// A known window decides the threshold, so a bigger model needs no retuning.
    #[test]
    fn a_known_window_beats_the_configured_number() {
        assert_eq!(compaction_budget(40_000, Some(1_000_000), None), 700_000);
        assert_eq!(compaction_budget(40_000, Some(200_000), Some(50)), 100_000);
        // No window in the roster: the configured number is all there is.
        assert_eq!(compaction_budget(40_000, None, Some(50)), 40_000);
        assert_eq!(compaction_budget(40_000, None, None), 40_000);
    }


    /// A template edit waits for the next turn; a fold does not; a step that
    /// stores nothing re-renders nothing.
    #[test]
    fn the_prompt_is_rebuilt_only_when_something_moved() {
        let stored = crate::store::Prompt {
            instructions: "i".into(),
            summary: None,
            cwd: "C:/a".into(),
            stamp: crate::store::stamp(None),
        };
        let same = crate::store::stamp(None);
        let folded = crate::store::stamp(Some("the note"));
        assert!(!prompt_moved(&stored, "C:/a", &same, true), "nothing moved");
        assert!(!prompt_moved(&stored, "C:/b", &same, false), "a step is not a turn");
        assert!(prompt_moved(&stored, "C:/b", &same, true), "the row names the directory");
        assert!(prompt_moved(&stored, "C:/a", &folded, false), "a handover is new content");
    }

    /// Every value the template can name has to arrive: the words are the
    /// config's, the numbers are this function's.
    #[test]
    fn the_context_notice_says_what_to_do() {
        let notice = context_notice(&notice_prompts(), "main", 700_000, 700_000, 0);
        assert!(notice.contains("700000 input tokens"), "{notice}");
        assert!(notice.contains("Nothing has been folded yet."), "{notice}");
        assert!(notice.contains("--session main"), "{notice}");
        assert!(notice.contains("transcript.py read"), "{notice}");
        assert!(notice.contains("transcript.py collapse"), "{notice}");
        // And it says the messages are not lost, the fear that stops folding.
        assert!(notice.contains("stay readable"), "{notice}");
        assert!(!notice.contains("{{"), "a placeholder nobody filled: {notice}");
    }

    fn notice_prompts() -> crate::prompts::Prompts {
        crate::prompts::Prompts::of(&[
            ("context_notice", "Context notice for `{{session}}`: {{tokens}} input tokens of {{budget}}. {{folded}} Read `transcript.py read --session {{session}}`, fold `transcript.py collapse --session {{session}}`; the messages stay readable."),
            ("folded_none", "Nothing has been folded yet."),
            ("folded_through", "The summary runs through seq {{upto}}."),
        ])
    }

    /// The warning names the cause: "failed or came back empty" is what made
    /// "why did the first attempt fail" unanswerable.
    #[test]
    fn a_retry_warning_names_why() {
        let said = retry_warning(
            "main",
            2,
            0,
            std::time::Duration::from_secs(2),
            "HTTP 500: boom",
        );
        assert!(said.contains("HTTP 500: boom"), "{said}");
        assert!(said.contains("retry 2 until cancelled"), "{said}");
        assert!(said.contains("2s"), "{said}");
        // An explicit ceiling still reads as a ceiling, not as a fraction of 0.
        let said = retry_warning("main", 1, 3, std::time::Duration::from_secs(1), "empty");
        assert!(said.contains("retry 1 of 3"), "{said}");
    }

    /// A notice for a session that already folded says where the summary ends.
    #[test]
    fn the_notice_names_what_is_already_folded() {
        let notice = context_notice(&notice_prompts(), "main", 10, 20, 42);
        assert!(notice.contains("through seq 42"), "{notice}");
        assert!(!notice.contains("{{"), "{notice}");
    }
}
