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
use crate::store::{HistoryItem, Store};
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
    /// Messages a compression leaves alone at the end of the transcript.
    pub keep_recent: usize,
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
            self.maybe_compress(id, &name, &llm, budget, &cancel).await;
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

    /// Folds the older transcript into one summary when the next request exceeds the budget.
    ///
    /// # Invariant
    ///
    /// The rows are not deleted; `summarised_upto` only marks where the summary
    /// takes over, so the full transcript stays readable.
    async fn maybe_compress(
        &self,
        id: i64,
        name: &str,
        llm: &LlmClient,
        budget: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
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
        let Ok(index) = self.store.message_index(id) else {
            return;
        };
        let Ok((previous, upto)) = self.store.summary(id) else {
            return;
        };
        // Eligible: only what a request would carry; the last summary's prefix is not resent.
        let live: Vec<&(i64, String)> = index.iter().filter(|(s, _)| *s > upto).collect();
        let Some(cut) = choose_cut(&live, self.keep_recent) else {
            return;
        };
        // Ask by sequence: rows and items are not one-to-one, so counting rows misses.
        let Ok(old) = self.store.history_between(id, upto, cut) else {
            return;
        };
        if old.is_empty() {
            return;
        }
        let covered = old.len();

        self.bus.publish(Event::Log {
            level: LogLevel::Info,
            message: format!("{name}: compressing {covered} item(s) into a summary"),
        });
        let started = std::time::Instant::now();
        let prompt = summary_prompt(previous.as_deref(), &old);
        match llm.summarise(&prompt, cancel).await {
            Ok(text) if !text.trim().is_empty() => {
                tracing::info!(session = %name, chars = text.len(), elapsed = ?started.elapsed(), "compression done");
                if let Err(e) = self.store.set_summary(id, &text, cut) {
                    self.error(name, "storing the summary", e);
                    return;
                }
                // It measured the prefix just folded; clear it so the next step cannot reuse it.
                let _ = self.store.set_context_tokens(id, 0);
                self.bus.publish(Event::Log {
                    level: LogLevel::Info,
                    message: format!("{name}: compressed up to message {cut}"),
                });
            }
            Ok(_) => {
                tracing::warn!("compression returned an empty summary; keeping the context as is")
            }
            Err(e) => {
                // Not fatal: the turn continues with the context it has. It will
                // try again on the next step.
                self.error(name, "compressing context", e);
            }
        }
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
            let images: Vec<_> = outcome
                .images
                .iter()
                .map(|i| json!({"path": i.path, "media_type": i.media_type}))
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
fn choose_cut(live: &[&(i64, String)], keep_recent: usize) -> Option<i64> {
    if live.len() <= keep_recent {
        return None;
    }
    let start = live.len() - keep_recent;
    for i in start..live.len() {
        if matches!(live[i].1.as_str(), "text" | "tool_call") {
            return Some(if i == 0 { 0 } else { live[i - 1].0 });
        }
    }
    None
}

/// The prompt that turns a stretch of transcript into a handover note.
fn summary_prompt(previous: Option<&str>, old: &[HistoryItem]) -> String {
    let mut p = String::from(
        "You are compressing the history of an agent session so another model \
         can continue the work without seeing it.\n\n\
         Write a handover note that keeps: what the user asked for, what was \
         tried, what worked, what failed and why, the current state of files \
         and processes, and any decision or constraint that must survive. \
         Drop raw command output, file contents and repetition. Plain prose or \
         tight bullets, under 800 words, no preamble.\n\n",
    );
    if let Some(prev) = previous {
        p.push_str("Summary of everything before this stretch:\n");
        p.push_str(prev);
        p.push_str("\n\nThis is what happened since:\n");
    }
    p.push_str("--- transcript ---\n");
    for item in old {
        match item {
            HistoryItem::Text { role, text } => {
                p.push_str(&format!("{role}: {}\n", clip(text, 2_000)))
            }
            HistoryItem::Reasoning { summary, .. } if !summary.is_empty() => {
                p.push_str(&format!("(thought: {})\n", clip(summary, 600)))
            }
            HistoryItem::ToolCall {
                name, arguments, ..
            } => p.push_str(&format!("called {name} {}\n", clip(arguments, 800))),
            HistoryItem::ToolResult { output, images, .. } => {
                p.push_str(&format!("=> {}\n", clip(output, 800)));
                // A summary cannot fold a picture in, so it says one was there.
                for image in images {
                    p.push_str(&format!("   (an image was shown here: {})\n", image.path));
                }
            }
            HistoryItem::BuiltinCall { item, .. } => {
                let kind = item["type"].as_str().unwrap_or("provider tool");
                p.push_str(&format!("({kind} ran at the provider)\n"));
            }
            // A reasoning item with no readable summary adds nothing here.
            HistoryItem::Reasoning { .. } => {}
        }
    }
    p.push_str("--- end ---\n");
    p
}

/// Shortens text to `max` characters, saying how much was dropped.
///
/// # Invariant
///
/// Counted and cut in characters, not bytes: a cut inside a character would
/// panic.
fn clip(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max)
        .map_or(text.len(), |(index, _)| index);
    format!("{}... [{} more characters]", &text[..end], total - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(entries: &[(i64, &str)]) -> Vec<(i64, String)> {
        entries.iter().map(|(s, k)| (*s, k.to_string())).collect()
    }

    /// A cut inside a Chinese character must not panic the turn.
    #[test]
    fn clip_cuts_between_characters_not_bytes() {
        let text = "文".repeat(500);
        let clipped = clip(&text, 3);
        assert!(clipped.starts_with("文文文"), "{clipped:?}");
        assert!(clipped.contains("497 more characters"), "{clipped:?}");
        // A cut point inside a character is the whole point of the test.
        assert_eq!(clip("中文abc", 5), "中文abc");
        assert!(clip("中文abc", 3).starts_with("中文a"));
    }

    /// The API rejects an orphaned tool result, so the cut must never leave one first.
    #[test]
    fn cut_never_leaves_an_orphaned_tool_result() {
        let rows = live(&[
            (1, "text"),
            (2, "tool_call"),
            (3, "tool_result"),
            (4, "reasoning"),
            (5, "tool_result"),
            (6, "text"),
        ]);
        let borrow: Vec<&(i64, String)> = rows.iter().collect();
        // Cutting at 4 would leave a tool_result first; 5 is as far back as is safe.
        assert_eq!(choose_cut(&borrow, 2), Some(5));
    }

    /// With nothing that can start a request on its own, leave the transcript alone.
    #[test]
    fn a_recent_window_of_only_tool_results_is_not_compressed() {
        let rows = live(&[
            (1, "text"),
            (2, "tool_call"),
            (3, "tool_result"),
            (4, "tool_result"),
            (5, "tool_result"),
            (6, "tool_result"),
        ]);
        let borrow: Vec<&(i64, String)> = rows.iter().collect();
        assert_eq!(choose_cut(&borrow, 2), None);
    }

    #[test]
    fn cut_lands_after_a_completed_step_when_possible() {
        let rows = live(&[
            (1, "text"),
            (2, "tool_call"),
            (3, "tool_result"),
            (4, "text"),
            (5, "tool_call"),
            (6, "tool_result"),
            (7, "text"),
        ]);
        let borrow: Vec<&(i64, String)> = rows.iter().collect();
        // Keeps 5..7: a complete step plus the user's line, never half of one.
        assert_eq!(choose_cut(&borrow, 3), Some(4));
    }

    #[test]
    fn nothing_is_compressed_when_the_transcript_is_short() {
        let rows = live(&[(1, "text"), (2, "tool_call")]);
        let borrow: Vec<&(i64, String)> = rows.iter().collect();
        assert_eq!(choose_cut(&borrow, 30), None);
    }

    #[test]
    fn an_earlier_summary_is_carried_into_the_next_one() {
        let items = vec![HistoryItem::Text {
            role: "user".into(),
            text: "do the thing".into(),
        }];
        let prompt = summary_prompt(Some("we already tried X"), &items);
        assert!(prompt.contains("we already tried X"));
        assert!(prompt.contains("do the thing"));
    }

    /// Off stays off: a server that asked for no compaction is not overruled by
    /// a default share of the model's window.
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

    /// The summary says a picture was there, so nothing is reasoned on silently.
    #[test]
    fn the_summary_says_a_picture_was_there() {
        let items = vec![
            HistoryItem::ToolResult {
                call_id: "c1".into(),
                output: "the model is looking at shot.png".into(),
                images: vec![crate::store::ImageRef {
                    path: "C:/w/shot.png".into(),
                    media_type: "image/png".into(),
                }],
            },
            HistoryItem::BuiltinCall {
                item: json!({"type": "web_search_call"}),
                replayable: true,
            },
        ];
        let prompt = summary_prompt(None, &items);
        assert!(
            prompt.contains("an image was shown here: C:/w/shot.png"),
            "{prompt}"
        );
        assert!(
            prompt.contains("web_search_call ran at the provider"),
            "{prompt}"
        );
    }

    fn long_output_is_clipped_in_the_summary_prompt() {
        let long = "x".repeat(5_000);
        let clipped = clip(&long, 800);
        assert!(clipped.len() < 1_000);
        assert!(clipped.contains("more characters"));
    }
}
