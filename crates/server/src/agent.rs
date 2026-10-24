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
use crate::providers::{self, Catalog};
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
    /// The machine's roster, which is what `--llm` names are resolved against.
    pub providers: &'static Catalog,
    /// What a session's own entry inherits: timeouts, retries, instructions.
    pub llm_config: LlmConfig,
    /// Clients for sessions that named their own entry, keyed by `provider/model`.
    pub clients: std::sync::Mutex<std::collections::HashMap<String, Arc<LlmClient>>>,
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
    /// Runs one turn, reporting failures on the bus rather than returning them.
    ///
    /// # Invariant
    ///
    /// `task` names a pending row whose message is already in the transcript, so
    /// the step-boundary inbox must not hand it back.
    pub async fn run_turn(&self, session: &SessionHandle, user_text: &str, task: Option<i64>) {
        self.turn(session, Some(user_text), task).await;
    }

    /// Replays a turn a shutdown interrupted; its user message is already stored.
    pub async fn resume_turn(&self, session: &SessionHandle, task: Option<i64>) {
        self.turn(session, None, task).await;
    }

    async fn turn(&self, session: &SessionHandle, user_text: Option<&str>, task: Option<i64>) {
        // A panic must surface as an error status, not a session that looks stuck.
        let outcome = AssertUnwindSafe(self.turn_inner(session, user_text, task))
            .catch_unwind()
            .await;
        if let Err(payload) = outcome {
            let message = panic_message(&payload);
            let name = session.name().to_string();
            tracing::error!(session = %name, "turn panicked: {message}");
            let _ = self.store.set_status(session.id(), SessionStatus::Error);
            self.bus.publish(Event::Log {
                level: LogLevel::Error,
                message: format!("{name}: turn panicked: {message}"),
            });
            self.bus.publish(Event::Status {
                session: name,
                status: SessionStatus::Error,
                detail: Some("panicked".to_string()),
            });
        }
    }

    async fn turn_inner(
        &self,
        session: &SessionHandle,
        user_text: Option<&str>,
        task: Option<i64>,
    ) {
        let name = session.name().to_string();
        let id = session.id();
        // Take the gate before `new_turn`: it cancels, so claiming first kills the queue.
        let epoch = session.cancel_epoch();
        let queued = session.busy();
        let _gate = session.claim().await;
        if queued {
            // A queued turn looks frozen otherwise; say so, since that can last minutes.
            tracing::info!(session = %name, "turn waited for the session to be free");
        }
        if session.cancel_epoch() != epoch {
            // Cancelled while queued: running looks like the interrupt did nothing.
            tracing::info!(session = %name, "turn dropped: cancelled while queued");
            return;
        }
        if session.is_deleted() {
            // Waited behind `del_session`: writing would resurrect deleted rows.
            tracing::info!(session = %name, "turn dropped: session was deleted while queued");
            return;
        }
        let (turn, cancel) = session.new_turn();
        let started = std::time::Instant::now();
        // Resolved once: every step of a turn sends to the model it started on.
        let (llm, budget) = match self.route(&name, &session.tuning()) {
            Ok(route) => route,
            Err(e) => {
                self.fail(&name, id, "resolving the session's llm", e);
                return;
            }
        };
        // Read once, like the route: every step sends the level this turn began with.
        let effort = session.tuning().thinking_effort;
        tracing::info!(
            session = %name,
            resumed = user_text.is_none(),
            model = %llm.model(),
            budget,
            "turn start"
        );

        if let Some(text) = user_text {
            if let Err(e) =
                self.store
                    .append_message(id, "text", &json!({"role": "user", "text": text}))
            {
                self.error(&name, "storing user message", e);
                return;
            }
        }
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
            self.deliver_inbox(id, &name, task);
            // Compress before the request is built, so the very next request is the small one.
            self.maybe_notify_context(session, id, &name, budget);
            tracing::info!(session = %name, step, "step start");
            let attempt = Attempt {
                number: step,
                llm: &llm,
                effort: effort.as_deref(),
            };
            match self.step(id, &name, turn, &cancel, &attempt).await {
                Ok(StepResult::Finished) => {
                    // Something may have landed while it finished; deliver it and go round again.
                    if self.deliver_inbox(id, &name, task) > 0 {
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
                    self.error(&name, "model request", e);
                    break TurnOutcome::Failed;
                }
            }
            // Wrapping, not saturating: the counter is only a log label.
            step = step.wrapping_add(1);
        };

        let (status, detail) = match outcome {
            TurnOutcome::Done => (SessionStatus::Idle, None),
            TurnOutcome::Cancelled => (SessionStatus::Idle, Some("cancelled".to_string())),
            TurnOutcome::Failed => (SessionStatus::Error, Some("see log".to_string())),
        };
        let _ = self.store.set_status(id, status);
        let _ = self.store.touch_session(id);
        tracing::info!(session = %name, outcome = ?outcome, elapsed = ?started.elapsed(), "turn end");
        self.bus.publish(Event::Status {
            session: name,
            status,
            detail,
        });
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
        let cached = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&spec)
            .cloned();
        if let Some(client) = cached {
            return Ok(client);
        }
        // Built outside the lock: a duplicate client is cheaper than serialising turns.
        let client = Arc::new(LlmClient::new(&self.llm_config.for_entry(entry, api_key))?);
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec, client.clone());
        Ok(client)
    }

    /// Reports a pre-turn failure and leaves the session in a state to act on.
    fn fail(&self, name: &str, id: i64, what: &str, e: anyhow::Error) {
        self.error(name, what, e);
        let _ = self.store.set_status(id, SessionStatus::Error);
        self.bus.publish(Event::Status {
            session: name.to_string(),
            status: SessionStatus::Error,
            detail: Some("see log".to_string()),
        });
    }

    /// Moves the session's inbox into the transcript, returning how many it delivered.
    ///
    /// # Invariant
    ///
    /// Called only at step boundaries, and the row is deleted with the transcript
    /// write, so a message is delivered exactly once.
    fn deliver_inbox(&self, id: i64, name: &str, own: Option<i64>) -> usize {
        let Ok(tasks) = self.store.take_pending_tasks(name, own) else {
            return 0;
        };
        let mut delivered = 0;
        for task in tasks {
            let Some(text) = task.payload.get("message").and_then(|v| v.as_str()) else {
                // No message means it cannot be delivered; dropping it beats blocking the queue.
                tracing::warn!(session = %name, kind = %task.kind, "dropping an empty pending task");
                continue;
            };
            if let Err(e) =
                self.store
                    .append_message(id, "text", &json!({"role": "user", "text": text}))
            {
                tracing::warn!(session = %name, error = %e, "could not deliver a queued message");
                continue;
            }
            delivered += 1;
            tracing::info!(session = %name, chars = text.len(), "delivered queued message at a step boundary");
            self.bus.publish(Event::Log {
                level: LogLevel::Info,
                message: format!("{name}: delivered a message that arrived while working"),
            });
        }
        delivered
    }

    /// Tells the session its context reached the threshold, and stops there.
    ///
    /// # Invariant
    ///
    /// The loop never folds the transcript itself. Folding is a judgement about
    /// work only the agent has seen, so the server measures and asks: the agent
    /// decides what the next stretch of context should contain. See
    /// `.agents/skills/compact/` and `docs/notes/server-agent.md`.
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
        let message = context_notice(name, tokens, budget, upto);
        if let Err(e) = self.store.add_pending_task(
            name,
            "inject",
            &json!({"session": name, "message": message}),
        ) {
            self.error(name, "queueing the context notice", e);
            return;
        }
        self.bus.publish(Event::Log {
            level: LogLevel::Info,
            message: format!(
                "{name}: context at {tokens} of {budget} tokens; asked the agent to fold"
            ),
        });
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
        } = *attempt;
        let (summary, history) = self.store.context(id)?;
        let definitions = self.tools.definitions();
        let mut rx = llm.stream(
            summary.as_deref(),
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
                        tracing::debug!(
                            session = %name,
                            input = u.input_tokens.unwrap_or(0),
                            output = u.output_tokens.unwrap_or(0),
                            total = u.total_tokens.unwrap_or(0),
                            "response completed"
                        );
                        // The provider's own count, never computed: no arithmetic matches it.
                        if let Some(input) = u.input_tokens {
                            let _ = self.store.set_context_tokens(id, input);
                        }
                    }
                }
                LlmEvent::Failed { message } => {
                    failure = Some(message);
                    break;
                }
                // A retry is the only news a stalled request gives; tell the operator, not the model.
                LlmEvent::Retrying {
                    attempt,
                    max,
                    delay,
                } => {
                    // `max == 0` is "no ceiling", not "zero allowed": printing
                    // `1/0` read like the opposite of what it meant.
                    let of = if max == 0 {
                        "until cancelled".to_string()
                    } else {
                        format!("of {max}")
                    };
                    self.bus.publish(Event::Log {
                        level: LogLevel::Warn,
                        message: format!(
                            "{name}: model request failed or came back empty; \
                             retry {attempt} {of} in {delay:?}"
                        ),
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
                vision: llm.vision(),
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

    fn error(&self, session: &str, what: &str, e: anyhow::Error) {
        tracing::error!(session = %session, "failed to {what}: {e}");
        self.bus.publish(Event::Log {
            level: LogLevel::Error,
            message: format!("{session}: failed to {what}: {e}"),
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
/// The message that hands the folding decision to the agent.
///
/// # Invariant
///
/// It names the numbers it is talking about and the two tools that act on them:
/// a notice that says only "you are near the limit" leaves the reader to guess
/// what is possible. See `.agents/skills/compact/`.
fn context_notice(session: &str, tokens: u64, budget: u64, upto: i64) -> String {
    let folded = if upto == 0 {
        "nothing has been folded yet".to_string()
    } else {
        format!("everything through seq {upto} is already folded into the summary")
    };
    format!(
        "Context notice for `{session}`: the last request carried {tokens} input tokens, and \
         this session's threshold is {budget}. This server does not compact for you -- the \
         shape of the context is your decision.\n\n\
         Right now {folded}.\n\n\
         To fold the older part, from a shell:\n\
         \n\
         .agents/skills/compact/transcript.py read --session {session}\n\
         \n\
         that shows the messages actually being sent. Then collapse with a handover note that \
         keeps what the work still needs:\n\
         \n\
         .agents/skills/compact/transcript.py collapse --session {session} --upto <seq> \\\n\
         \x20   --summary '<what was asked, what was tried, what worked and failed, the \
         current state, the constraints that must survive>'\n\
         \n\
         Folding removes nothing from the database: the messages stay readable, they just stop \
         being sent. If the work is nearly done, or the context is fine as it is, do nothing. \
         The `.agents/skills/compact/SKILL.md` skill has the full procedure."
    )
}

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

    /// The notice names the numbers, the session, and the two commands.
    ///
    /// # Invariant
    ///
    /// A notice that says only "you are near the limit" leaves the reader to
    /// guess what is possible; one that does not name the session cannot be acted
    /// on, because the script takes it as an argument.
    #[test]
    fn the_context_notice_says_what_to_do() {
        let notice = context_notice("main", 700_000, 700_000, 0);
        assert!(notice.contains("700000 input tokens"), "{notice}");
        assert!(notice.contains("nothing has been folded yet"), "{notice}");
        assert!(notice.contains("--session main"), "{notice}");
        assert!(notice.contains("transcript.py read"), "{notice}");
        assert!(notice.contains("transcript.py collapse"), "{notice}");
        // And it says the messages are not lost, the fear that stops folding.
        assert!(notice.contains("stay readable"), "{notice}");
    }

    /// A notice for a session that already folded says where the summary ends.
    #[test]
    fn the_notice_names_what_is_already_folded() {
        let notice = context_notice("main", 10, 20, 42);
        assert!(notice.contains("through seq 42"), "{notice}");
    }
}
